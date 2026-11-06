//! `ognivo mcp`: the MCP servers a session in this project gets, started as a session starts
//! them, with their tools under the names the model calls them by. A project's server that
//! waits for the user's trust is named and left alone: `ognivo chat` asks about it.
//!
//! `ognivo mcp login NAME` signs in to a remote server in the browser (OAuth), and
//! `ognivo mcp logout NAME` forgets that; the sign-ins are kept in `~/.ognivo/mcp-auth`.
//! `ognivo mcp call NAME TOOL [JSON]` starts the one server and calls a tool of it as a
//! session would, to see whether it answers and how soon.
//!
//! Exit codes: 0 every server that may start did (signed in, signed out); 1 one did not;
//! 2 bad usage.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand};
use ognivo_mcp::{Gateway, Loaded, ServerConfig};

use crate::session::{self, open_in_browser, user_dirs};

/// How long a server may take to start: `npx` may fetch it first.
const START: Duration = Duration::from_secs(60);

#[derive(Debug, Args)]
pub struct McpArgs {
    #[command(subcommand)]
    action: Option<Action>,
    /// Project root; the current directory by default
    #[arg(long, global = true)]
    cwd: Option<PathBuf>,
    /// Where sessions and logs go; ~/.ognivo/data by default
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Action {
    /// Sign in to a remote server in the browser (OAuth)
    Login {
        /// The server, as its config names it
        name: String,
    },
    /// Forget the sign-in to a remote server
    Logout {
        /// The server, as its config names it
        name: String,
    },
    /// Start one server and call a tool of it, as a session would
    Call {
        /// The server, as its config names it
        name: String,
        /// The tool, as the server names it
        tool: String,
        /// Its arguments, a JSON object
        #[arg(default_value = "{}")]
        arguments: String,
        /// How long the call may take, in seconds
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
}

pub async fn mcp(a: McpArgs) -> anyhow::Result<ExitCode> {
    let (root, store) = match session::root(a.cwd.as_deref())
        .and_then(|r| session::data_dir(a.data_dir.as_deref()).map(|d| (r, d)))
        .and_then(|(r, d)| session::open_store(&d).map(|s| (r, s)))
    {
        Ok(found) => found,
        Err(f) => {
            eprintln!("ognivo mcp: {}", f.message);
            return Ok(ExitCode::from(f.code));
        }
    };
    let (home, claude_dir) = user_dirs();
    let signed = home.as_deref().map(ognivo_mcp::oauth::credentials_dir);
    let mut loaded = ognivo_mcp::config::load(&root, home.as_deref(), claude_dir.as_deref(), |v| {
        std::env::var(v).ok()
    });
    let mut untrusted = None;
    if !loaded.pending.is_empty() {
        let key = format!("mcp\n{}", loaded.pending_key());
        match store.trusted(&root, &key) {
            Ok(Some(true)) => loaded.trust(),
            Ok(Some(false)) => untrusted = Some("you did not trust them"),
            _ => untrusted = Some("they wait for your trust: ognivo chat asks"),
        }
    }
    match a.action {
        None => list(&root, &loaded, untrusted, signed.as_deref()).await,
        Some(Action::Login { name }) => login(&loaded, &name, signed.as_deref()).await,
        Some(Action::Logout { name }) => logout(&loaded, &name, signed.as_deref()),
        Some(Action::Call {
            name,
            tool,
            arguments,
            timeout,
        }) => {
            call(
                &loaded,
                &name,
                &tool,
                &arguments,
                Duration::from_secs(timeout),
                signed.as_deref(),
            )
            .await
        }
    }
}

/// The servers, started, with their tools.
async fn list(
    root: &Path,
    loaded: &Loaded,
    untrusted: Option<&str>,
    signed: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    for p in &loaded.problems {
        println!("! {p}");
    }
    if let Some(why) = untrusted {
        for s in &loaded.pending {
            println!(
                "· {} ({}): {} — does not start: {why}",
                s.name,
                s.source,
                s.shown()
            );
        }
    }
    if loaded.servers.is_empty() {
        println!("no MCP server starts for {}", root.display());
        return Ok(ExitCode::SUCCESS);
    }
    let gateway = Gateway::start(&loaded.servers, START, signed).await;
    let mut failed = false;
    for (config, started) in loaded.servers.iter().zip(gateway.started()) {
        match &started.outcome {
            Ok(n) => {
                let how = match signed {
                    Some(dir) if ognivo_mcp::oauth::signed_in(dir, config) => ", signed in",
                    _ => "",
                };
                println!(
                    "✓ {} ({}{how}): {}",
                    config.name,
                    config.source,
                    config.shown()
                );
                println!("  {n} tool{}", if *n == 1 { "" } else { "s" });
                if let Some(text) = &started.instructions {
                    let first: String = text
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(90)
                        .collect();
                    println!("  says: {first}");
                }
                for t in gateway.tools().iter().filter(|t| t.server == config.name) {
                    let about: String = t
                        .description
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(90)
                        .collect();
                    println!("    {}  {about}", t.name);
                }
                let prompts: Vec<&ognivo_mcp::Prompt> = gateway
                    .prompts()
                    .iter()
                    .filter(|p| p.server == config.name)
                    .collect();
                if !prompts.is_empty() {
                    println!(
                        "  {} prompt{}, run as /name or loaded by the model:",
                        prompts.len(),
                        if prompts.len() == 1 { "" } else { "s" }
                    );
                }
                for p in prompts {
                    let args: Vec<String> = p
                        .arguments
                        .iter()
                        .map(|a| {
                            if a.required {
                                format!("<{}>", a.name)
                            } else {
                                format!("[{}]", a.name)
                            }
                        })
                        .collect();
                    let about: String = p.description.chars().take(90).collect();
                    println!("    /{} {}  {about}", p.name, args.join(" "));
                }
            }
            Err(why) => {
                failed = true;
                println!(
                    "✗ {} ({}): {} — {why}",
                    config.name,
                    config.source,
                    config.shown()
                );
            }
        }
    }
    gateway.shutdown().await;
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// The server `name` of those that may start; the message why not, when it is not one.
fn server<'a>(loaded: &'a Loaded, name: &str) -> Result<&'a ServerConfig, String> {
    if let Some(s) = loaded.servers.iter().find(|s| s.name == name) {
        return Ok(s);
    }
    if loaded.pending.iter().any(|s| s.name == name) {
        return Err(format!(
            "{name} is the project's server and waits for your trust: ognivo chat asks"
        ));
    }
    let names: Vec<&str> = loaded.servers.iter().map(|s| s.name.as_str()).collect();
    Err(if names.is_empty() {
        format!("there is no MCP server {name}")
    } else {
        format!(
            "there is no MCP server {name}; the servers: {}",
            names.join(", ")
        )
    })
}

/// Signs in to `name` and starts it with the sign-in.
async fn login(loaded: &Loaded, name: &str, signed: Option<&Path>) -> anyhow::Result<ExitCode> {
    let usage = |why: String| {
        eprintln!("ognivo mcp login: {why}");
        Ok(ExitCode::from(2))
    };
    let s = match server(loaded, name) {
        Ok(s) => s,
        Err(why) => return usage(why),
    };
    let Some(dir) = signed else {
        return usage("there is no home directory to keep the sign-in in".into());
    };
    // what it says without a sign-in: its challenge, or that it needs none
    let first = Gateway::start(std::slice::from_ref(s), START, None).await;
    let challenge = match first.started().first() {
        Some(st) if st.sign_in.is_some() => st.sign_in.clone(),
        Some(st) => match &st.outcome {
            Ok(n) => {
                println!("{name} started without a sign-in: {n} tools");
                first.shutdown().await;
                return Ok(ExitCode::SUCCESS);
            }
            Err(why) if matches!(s.transport, ognivo_mcp::Transport::Stdio { .. }) => {
                return usage(format!(
                    "{name} is a local server, with nothing to sign in to ({why})"
                ));
            }
            // a server down or of another mind may still sign in: its metadata says
            Err(_) => None,
        },
        None => None,
    };
    first.shutdown().await;
    let opened = |url: &str| {
        println!("Sign in to {name} in your browser:\n  {url}");
        open_in_browser(url);
        println!(
            "Waiting for the browser to come back ({} min at most)…",
            ognivo_mcp::oauth::SIGN_IN_WAIT.as_secs() / 60
        );
    };
    if let Err(why) = ognivo_mcp::oauth::sign_in(dir, s, challenge.as_deref(), opened).await {
        eprintln!("ognivo mcp login: {name}: {why}");
        return Ok(ExitCode::FAILURE);
    }
    let again = Gateway::start(std::slice::from_ref(s), START, Some(dir)).await;
    let outcome = again.started().first().map(|st| st.outcome.clone());
    again.shutdown().await;
    match outcome {
        Some(Ok(n)) => {
            println!(
                "✓ signed in to {name}: {n} tool{}",
                if n == 1 { "" } else { "s" }
            );
            Ok(ExitCode::SUCCESS)
        }
        Some(Err(why)) => {
            println!("signed in to {name}, but it did not start: {why}");
            Ok(ExitCode::FAILURE)
        }
        None => Ok(ExitCode::FAILURE),
    }
}

/// Starts `name` alone and calls its tool `tool` with `arguments`.
async fn call(
    loaded: &Loaded,
    name: &str,
    tool: &str,
    arguments: &str,
    timeout: Duration,
    signed: Option<&Path>,
) -> anyhow::Result<ExitCode> {
    let s = match server(loaded, name) {
        Ok(s) => s,
        Err(why) => {
            eprintln!("ognivo mcp call: {why}");
            return Ok(ExitCode::from(2));
        }
    };
    let started = std::time::Instant::now();
    let gateway = Gateway::start(std::slice::from_ref(s), START, signed).await;
    let up = started.elapsed();
    if let Some(Err(why)) = gateway.started().first().map(|st| st.outcome.clone()) {
        eprintln!("ognivo mcp call: {name} did not start: {why}");
        gateway.shutdown().await;
        return Ok(ExitCode::FAILURE);
    }
    let Some(t) = gateway
        .tools()
        .iter()
        .find(|t| t.server == s.name && t.tool == tool)
        .map(|t| t.name.clone())
    else {
        let tools: Vec<&str> = gateway.tools().iter().map(|t| t.tool.as_str()).collect();
        eprintln!(
            "ognivo mcp call: {name} has no tool {tool}; its tools: {}",
            tools.join(", ")
        );
        gateway.shutdown().await;
        return Ok(ExitCode::from(2));
    };
    let called = std::time::Instant::now();
    let result = gateway.call(&t, arguments, timeout).await;
    let took = called.elapsed();
    gateway.shutdown().await;
    eprintln!(
        "· {name} started in {:.1} s; {tool} answered in {:.1} s",
        up.as_secs_f64(),
        took.as_secs_f64()
    );
    match result {
        Ok(text) => {
            println!("{text}");
            Ok(ExitCode::SUCCESS)
        }
        Err(why) => {
            eprintln!("ognivo mcp call: {why}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// Forgets the sign-in to `name`.
fn logout(loaded: &Loaded, name: &str, signed: Option<&Path>) -> anyhow::Result<ExitCode> {
    let s = loaded
        .servers
        .iter()
        .chain(&loaded.pending)
        .find(|s| s.name == name);
    let (Some(s), Some(dir)) = (s, signed) else {
        eprintln!("ognivo mcp logout: there is no MCP server {name} with a sign-in");
        return Ok(ExitCode::from(2));
    };
    match ognivo_mcp::oauth::sign_out(dir, s) {
        Ok(true) => println!("signed out of {name}"),
        Ok(false) => println!("{name} had no sign-in"),
        Err(why) => {
            eprintln!("ognivo mcp logout: {why}");
            return Ok(ExitCode::FAILURE);
        }
    }
    Ok(ExitCode::SUCCESS)
}
