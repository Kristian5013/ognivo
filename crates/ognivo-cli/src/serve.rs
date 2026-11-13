//! `ognivo core`: the core the GUI talks to (docs/architecture.md §3), one per user and data
//! directory. It listens where only its user can talk to it ([`ognivo_os::ipc`]): a Unix
//! socket, `$XDG_RUNTIME_DIR/ognivo/core.sock`, or on Windows a named pipe, which
//! `core.pipe` in the data directory names; and speaks JSON-RPC 2.0 there, one message a
//! line (`ognivo_protocol::rpc`). There is no TCP listener.
//!
//! - Each thread is a session of the store, run as `ognivo chat` runs one: its agent, its
//!   edit journal, its questions, which go to the clients as requests of the core.
//! - Every event of every thread goes to every client that called `initialize`. The store
//!   keeps them all but the text deltas (`wire`), so a client that comes back or opens a
//!   thread later reads them there.
//! - The core quits when no client has been there and no turn has run for `--idle`
//!   seconds, or on SIGTERM (on Windows when its console closes). Its threads are kept in
//!   the store and can be picked up again.
//!
//! Exit codes: 0 idle or SIGTERM; 1 another core runs; 2 bad usage.

mod hub;
mod mcp;
mod thread;

use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use ognivo_models::image::{self, Image};
use ognivo_os::ipc::{self, Address, Listen};
use ognivo_protocol::{self as p, rpc};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;

use crate::session;
use hub::Hub;

/// A message longer than this ends its connection: requests are short, but for the images
/// of a message, which may have 32 MB together (43 MB in base64).
const MAX_MESSAGE: usize = 64 << 20;

#[derive(Debug, Args)]
pub struct CoreArgs {
    /// The socket to listen on; $XDG_RUNTIME_DIR/ognivo/core.sock by default. On Windows a
    /// named pipe (\\.\pipe\NAME); by default a new one each start, which core.pipe in the
    /// data directory names
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Where sessions and logs go; ~/.ognivo/data by default
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Quit after this many seconds without a client and without a running turn
    #[arg(long, default_value_t = 60)]
    idle: u64,
}

/// A JSON-RPC error.
#[derive(Debug)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

pub type Reply = Result<Value, RpcError>;

fn fail(message: impl std::fmt::Display, code: u8) -> anyhow::Result<ExitCode> {
    eprintln!("ognivo core: {message}");
    Ok(ExitCode::from(code))
}

pub async fn serve(a: CoreArgs) -> anyhow::Result<ExitCode> {
    let data_dir = match session::data_dir(a.data_dir.as_deref()) {
        Ok(d) => d,
        Err(f) => return fail(f.message, f.code),
    };
    let socket = match a
        .socket
        .clone()
        .map(Address::at)
        .map_or_else(|| Address::default_for(&data_dir), Ok)
    {
        Ok(s) => s,
        Err(e) => return fail(e, 2),
    };
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        return fail(format!("{}: {e}", data_dir.display()), 2);
    }
    // one core per data directory: it numbers the clients' stream in the store
    let lock_path = data_dir.join("core.lock");
    let lock = match ognivo_os::lock_file(&lock_path) {
        Ok(f) => f,
        Err(e) => return fail(format!("{}: {e}", lock_path.display()), 2),
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => {
            return fail(format!("another core runs on {}", data_dir.display()), 1);
        }
        Err(std::fs::TryLockError::Error(e)) => {
            return fail(format!("{}: {e}", lock_path.display()), 2);
        }
    }
    let mut listener = match ipc::listen(&socket) {
        Ok(l) => l,
        Err(Listen::Taken) => {
            return fail(
                format!("another core listens at {}", socket.path().display()),
                1,
            );
        }
        Err(Listen::Failed(e)) => {
            return fail(format!("{}: {e}", socket.path().display()), 2);
        }
    };
    let hub = match Hub::new(data_dir) {
        Ok(hub) => Arc::new(hub),
        Err(f) => return fail(f.message, f.code),
    };
    eprintln!("ognivo core: listening at {}", socket.path().display());
    let mut term = ognivo_os::signal::terminate()?;
    let mut int = ognivo_os::signal::interrupt()?;
    let idle = idle(hub.clone(), Duration::from_secs(a.idle));
    tokio::pin!(idle);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok(stream) = accepted else { continue };
                tokio::spawn(connection(hub.clone(), stream));
            }
            _ = &mut idle => {
                eprintln!("ognivo core: idle, quitting");
                break;
            }
            _ = hub.quitting() => {
                eprintln!("ognivo core: a client asked it to quit");
                break;
            }
            _ = term.recv() => break,
            _ = int.recv() => break,
        }
    }
    hub.shutdown().await;
    listener.close();
    Ok(ExitCode::SUCCESS)
}

/// Ends once the core has had no client and no running turn for `idle`.
async fn idle(hub: Arc<Hub>, idle: Duration) {
    let mut activity = hub.activity();
    loop {
        let quiet = *activity.borrow_and_update() == (0, 0);
        if quiet {
            match tokio::time::timeout(idle, activity.changed()).await {
                Err(_) | Ok(Err(_)) => return,
                Ok(Ok(())) => {}
            }
        } else if activity.changed().await.is_err() {
            return;
        }
    }
}

/// One client: its requests, each answered when it is done, and its answers to the core's.
async fn connection(hub: Arc<Hub>, stream: ipc::Connection) {
    let (read, mut write) = tokio::io::split(stream);
    let (out, mut lines) = mpsc::unbounded_channel::<String>();
    let client = hub.connect(out);
    let writer = tokio::spawn(async move {
        while let Some(mut line) = lines.recv().await {
            line.push('\n');
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let mut read = BufReader::new(read);
    let mut buf = Vec::new();
    while let Ok(true) = read_message(&mut read, &mut buf).await {
        let message = match serde_json::from_slice::<Value>(&buf) {
            Ok(m) => m,
            Err(e) => {
                hub.send(
                    client,
                    error(Value::Null, -32700, &format!("not JSON: {e}")),
                );
                continue;
            }
        };
        let id = message.get("id").cloned();
        match (message.get("method").and_then(Value::as_str), id) {
            (Some(method), Some(id)) => {
                let (hub, method) = (hub.clone(), method.to_owned());
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                tokio::spawn(async move {
                    let line = match request(&hub, client, &method, params).await {
                        Ok(result) => {
                            json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
                        }
                        Err(e) => error(id, e.code, &e.message),
                    };
                    hub.send(client, line);
                });
            }
            // a client sends no notifications the core acts on
            (Some(_), None) => {}
            // an answer to a question of the core's; an error is no answer
            (None, Some(id)) => {
                if let Some(id) = id.as_u64() {
                    hub.answer(id, message.get("result").cloned());
                }
            }
            (None, None) => hub.send(client, error(Value::Null, -32600, "not a request")),
        }
    }
    hub.disconnect(client);
    writer.abort();
}

/// The next line into `buf`, without its end; `false` at the end of the stream.
async fn read_message(
    read: &mut (impl AsyncBufRead + Unpin),
    buf: &mut Vec<u8>,
) -> std::io::Result<bool> {
    buf.clear();
    loop {
        let chunk = read.fill_buf().await?;
        if chunk.is_empty() {
            return Ok(!buf.is_empty());
        }
        match chunk.iter().position(|&b| b == b'\n') {
            Some(at) => {
                buf.extend_from_slice(&chunk[..at]);
                read.consume(at + 1);
                return Ok(true);
            }
            None => {
                let n = chunk.len();
                buf.extend_from_slice(chunk);
                read.consume(n);
            }
        }
        if buf.len() > MAX_MESSAGE {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "message too long",
            ));
        }
    }
}

fn error(id: Value, code: i64, message: &str) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}).to_string()
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params).map_err(|e| RpcError::new(-32602, e.to_string()))
}

async fn request(hub: &Arc<Hub>, client: u64, method: &str, params: Value) -> Reply {
    match method {
        rpc::INITIALIZE => {
            let _: rpc::Initialize = parse(params)?;
            let seq = hub.ready(client);
            reply(rpc::Initialized {
                protocol: p::VERSION,
                core: env!("CARGO_PKG_VERSION").to_owned(),
                exe: std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
                seq,
            })
        }
        rpc::CORE_QUIT => {
            let _: rpc::Quit = parse(params)?;
            if hub.quit() {
                reply(rpc::Done {})
            } else {
                Err(RpcError::new(
                    rpc::error::BUSY,
                    "a turn is running: the core quits once no turn runs",
                ))
            }
        }
        rpc::THREAD_LIST => thread::list(hub, parse(params)?),
        rpc::SERVER_DETECT => detect(parse(params)?).await,
        rpc::MCP_SERVERS => mcp::servers(hub, parse(params)?),
        rpc::MCP_CONFIG => mcp::config(parse(params)?),
        rpc::MCP_CONFIG_SET => mcp::set(parse(params)?),
        rpc::MCP_LOGIN => mcp::login(parse(params)?).await,
        rpc::MCP_LOGOUT => mcp::logout(parse(params)?),
        rpc::THREAD_START => thread::start(hub, parse(params)?).await,
        rpc::THREAD_RESUME => thread::resume(hub, parse(params)?).await,
        rpc::THREAD_READ => thread::read(hub, parse(params)?),
        rpc::THREAD_TRAJECTORY => thread::trajectory(hub, parse(params)?),
        rpc::TURN_START => {
            let p: rpc::StartTurn = parse(params)?;
            let images = images(&p.images)?;
            let command = |reply| thread::Command::Turn(p.text, images, reply);
            thread::command(hub, p.thread, command).await
        }
        rpc::TURN_EDIT => {
            let p: rpc::EditTurn = parse(params)?;
            let images = images(&p.images)?;
            let command = |reply| thread::Command::Edit(p.turn.0, p.text, images, reply);
            thread::command(hub, p.thread, command).await
        }
        rpc::THREAD_DELETE => thread::delete(hub, parse(params)?).await,
        rpc::THREAD_MODE => thread::set_mode(hub, parse(params)?),
        rpc::CHANGES_ACCEPT => {
            let p: rpc::Accept = parse(params)?;
            thread::command(hub, p.thread, |reply| {
                thread::Command::Accept(p.path, reply)
            })
            .await
        }
        rpc::CHANGES_REVERT => {
            let p: rpc::Revert = parse(params)?;
            let command = |reply| thread::Command::Revert(p.path, p.force, reply);
            thread::command(hub, p.thread, command).await
        }
        rpc::CHANGES_BASE => {
            let p: rpc::BaseOf = parse(params)?;
            thread::command(hub, p.thread, |reply| thread::Command::Base(p.path, reply)).await
        }
        rpc::THREAD_CLEAR
        | rpc::THREAD_CLOSE
        | rpc::TURN_INTERRUPT
        | rpc::TURN_ANSWER_NOW
        | rpc::CHANGES_LIST => {
            let p: rpc::ThreadRef = parse(params)?;
            let command = match method {
                rpc::THREAD_CLEAR => thread::Command::Clear,
                rpc::THREAD_CLOSE => thread::Command::Close,
                rpc::TURN_INTERRUPT => thread::Command::Interrupt,
                rpc::TURN_ANSWER_NOW => thread::Command::AnswerNow,
                _ => thread::Command::Changes,
            };
            thread::command(hub, p.thread, command).await
        }
        _ => Err(RpcError::new(-32601, format!("no method {method}"))),
    }
}

/// The images of a new message as the model gets them, or why one cannot go (invalid params).
/// The most it may hold fit in [`MAX_MESSAGE`].
fn images(given: &[rpc::NewImage]) -> Result<Vec<Image>, RpcError> {
    if given.len() > image::MAX_PER_MESSAGE {
        return Err(RpcError::new(
            -32602,
            format!(
                "{} images: a message may hold at most {}",
                given.len(),
                image::MAX_PER_MESSAGE
            ),
        ));
    }
    let images: Vec<Image> = given
        .iter()
        .enumerate()
        .map(|(i, g)| {
            Image::from_base64(&g.data)
                .map_err(|e| RpcError::new(-32602, format!("image {}: {e}", i + 1)))
        })
        .collect::<Result<_, _>>()?;
    match image::too_many(&images) {
        Some(why) => Err(RpcError::new(-32602, why)),
        None => Ok(images),
    }
}

const _: () = assert!(image::MAX_MESSAGE_BYTES.div_ceil(3) * 4 + (1 << 20) < MAX_MESSAGE);

/// How long `server/detect` waits for the server.
const DETECT_TIMEOUT: Duration = Duration::from_secs(5);

/// The profile of the model `s` runs, found as a thread's start finds it, with its levels.
/// Behind llama-server's router: its models, and the profile of the one `s` names, or else
/// of the one loaded. One not loaded is known by its name, and has no window yet: detecting
/// does not have the router start it.
async fn detect(s: rpc::Server) -> Reply {
    use ognivo_models::{doctor, llama::LlamaServer, llama::RouterModel, profile};
    let base =
        LlamaServer::new(&s.url, s.api_key).map_err(|e| RpcError::new(-32602, e.to_string()))?;
    let profiles = profile::builtin().map_err(|e| RpcError::new(-32603, e.to_string()))?;
    let found = tokio::time::timeout(DETECT_TIMEOUT, async {
        let props = base.props().await?;
        if props.role.as_deref() != Some("router") {
            let found = doctor::detect_profile(&base, &props, &profiles).await?;
            return Ok((
                found.map(|(p, _)| p),
                props.n_ctx,
                props.vision,
                Vec::new(),
                None,
            ));
        }
        let models = base.models().await?;
        let named = |m: &RouterModel| doctor::profile_by_name(&m.names(), &profiles);
        let listed = models
            .iter()
            .map(|m| rpc::RoutedModel {
                id: m.id.clone(),
                status: m.status.clone(),
                profile: named(m).map(|p| p.id.clone()),
            })
            .collect();
        let asked = s.model.as_ref().and_then(|name| {
            models
                .iter()
                .find(|m| &m.id == name || m.aliases.contains(name))
        });
        let loaded: Vec<&RouterModel> = models.iter().filter(|m| m.status == "loaded").collect();
        let chosen = asked.or(match loaded.as_slice() {
            [one] => Some(*one),
            _ => None,
        });
        let (found, window, vision) = match chosen {
            Some(m) if m.status == "loaded" => {
                let server = base.for_model(&m.id);
                let props = server.props().await?;
                let found = doctor::detect_profile(&server, &props, &profiles).await?;
                (found.map(|(p, _)| p), props.n_ctx, props.vision)
            }
            Some(m) => (named(m), None, None),
            None => (None, None, None),
        };
        let chosen = chosen.map(|m| m.id.clone());
        Ok::<_, ognivo_models::EngineError>((found, window, vision, listed, chosen))
    })
    .await
    .map_err(|_| {
        RpcError::new(
            rpc::error::NOT_STARTED,
            format!(
                "{} did not answer within {} s",
                s.url,
                DETECT_TIMEOUT.as_secs()
            ),
        )
    })?
    .map_err(|e| RpcError::new(rpc::error::NOT_STARTED, format!("{}: {e}", s.url)))?;
    let (p, window, vision, models, model) = found;
    reply(rpc::Detected {
        profile: p.map(|p| p.id.clone()),
        levels: p.map(|p| p.reasoning.levels.clone()).unwrap_or_default(),
        default_level: p.map(|p| p.reasoning.default.clone()),
        native: p.is_some_and(|p| ognivo_models::native::reads(p.family)),
        window,
        models,
        model,
        vision,
    })
}

pub fn reply(value: impl serde::Serialize) -> Reply {
    serde_json::to_value(value).map_err(|e| RpcError::new(-32603, e.to_string()))
}
