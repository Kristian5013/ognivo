//! The core's MCP methods for a client's panel (docs/architecture.md §10): a thread's servers
//! as it started them, with their tools and prompts; the files that configure a project's
//! servers, which the panel writes; and the sign-ins to remote servers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ognivo_mcp::{Gateway, ServerConfig, Transport, config, oauth};
use ognivo_protocol::rpc;

use super::hub::{Hub, lock};
use crate::serve::{Reply, RpcError, reply};
use crate::session;

/// What a thread started of MCP, kept with it, so that a panel reads it during a turn too.
#[derive(Debug, Clone, Default)]
pub struct McpView {
    pub gateway: Option<Arc<Gateway>>,
    /// The servers that could start: the user's, and the project's once trusted.
    pub servers: Vec<ServerConfig>,
    /// The project's servers, which wait for the user's trust.
    pub untrusted: Vec<ServerConfig>,
    pub problems: Vec<String>,
    /// Where the sign-ins are kept.
    pub signed: Option<PathBuf>,
}

/// The first line of a text, for a list.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

/// `mcp/servers`: the thread's servers.
pub fn servers(hub: &Hub, r: rpc::ThreadRef) -> Reply {
    let view = lock(&hub.threads)
        .get(&r.thread.0)
        .map(|h| h.mcp.clone())
        .ok_or_else(|| {
            RpcError::new(
                rpc::error::NO_THREAD,
                format!("thread {} is not open", r.thread.0),
            )
        })?;
    let g = view.gateway.as_deref();
    let mut servers = Vec::new();
    for c in &view.servers {
        let started = g.and_then(|g| g.started().iter().find(|s| s.name == c.name));
        let (state, why) = match started {
            Some(s) => match (&s.outcome, &s.sign_in) {
                (Ok(_), _) => ("running", None),
                (Err(_), Some(_)) => ("sign_in", Some("it wants you to sign in".to_owned())),
                (Err(why), None) => ("failed", Some(why.clone())),
            },
            None => ("failed", Some("it did not start".to_owned())),
        };
        servers.push(rpc::McpServerState {
            name: c.name.clone(),
            source: c.source.clone(),
            command: c.shown(),
            remote: matches!(c.transport, Transport::Http { .. }),
            state: state.into(),
            why,
            signed_in: view
                .signed
                .as_deref()
                .is_some_and(|d| oauth::signed_in(d, c)),
            instructions: started.and_then(|s| s.instructions.clone()),
            tools: g
                .map(|g| {
                    g.tools()
                        .iter()
                        .filter(|t| t.server == c.name)
                        .map(|t| rpc::McpToolInfo {
                            name: t.name.clone(),
                            description: first_line(&t.description),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            prompts: g
                .map(|g| {
                    g.prompts()
                        .iter()
                        .filter(|p| p.server == c.name)
                        .map(|p| rpc::McpPromptInfo {
                            name: p.name.clone(),
                            description: first_line(&p.description),
                            arguments: p
                                .arguments
                                .iter()
                                .map(|a| rpc::McpPromptArgument {
                                    name: a.name.clone(),
                                    required: a.required,
                                })
                                .collect(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        });
    }
    for c in &view.untrusted {
        servers.push(rpc::McpServerState {
            name: c.name.clone(),
            source: c.source.clone(),
            command: c.shown(),
            remote: matches!(c.transport, Transport::Http { .. }),
            state: "untrusted".into(),
            why: Some(
                "the project's server: it starts once you trust the project's servers".into(),
            ),
            signed_in: false,
            instructions: None,
            tools: Vec::new(),
            prompts: Vec::new(),
        });
    }
    reply(rpc::McpServers {
        servers,
        problems: view.problems.clone(),
    })
}

fn bad(message: impl Into<String>) -> RpcError {
    RpcError::new(-32602, message)
}

/// The project's root, canonical, and the user's home and Claude Code's directory.
fn places(project: &str) -> Result<(PathBuf, Option<PathBuf>, Option<PathBuf>), RpcError> {
    let root = session::root(Some(Path::new(project))).map_err(|f| bad(f.message))?;
    let (home, claude_dir) = session::user_dirs();
    Ok((root, home, claude_dir))
}

/// A path as the user names it: `~/…` under the home.
fn shown(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

/// `mcp/config`: the user's file, the project's and Claude Code's.
pub fn config(r: rpc::ProjectRef) -> Reply {
    let (root, home, claude_dir) = places(&r.project)?;
    let file =
        |scope: &str, path: PathBuf, got: Result<Vec<(String, serde_json::Value)>, String>| {
            let (servers, problem) = match got {
                Ok(entries) => (
                    entries
                        .into_iter()
                        .map(|(name, value)| rpc::McpEntry { name, value })
                        .collect(),
                    None,
                ),
                Err(why) => (Vec::new(), Some(why)),
            };
            rpc::McpConfigFile {
                scope: scope.into(),
                path: shown(&path, home.as_deref()),
                servers,
                problem,
            }
        };
    let mut files = Vec::new();
    if let Some(h) = &home {
        let path = config::user_file(h);
        files.push(file("user", path.clone(), config::entries(&path)));
    }
    let project = root.join(".mcp.json");
    files.push(file("project", project.clone(), config::entries(&project)));
    let claude = claude_dir
        .map(|d| d.join(".claude.json"))
        .or_else(|| home.as_ref().map(|h| h.join(".claude.json")));
    if let Some(path) = claude {
        let got = config::claude_entries(&path, &root);
        if !matches!(&got, Ok(e) if e.is_empty()) {
            files.push(file("claude", path, got));
        }
    }
    reply(rpc::McpConfig { files })
}

/// `mcp/config/set`: a server of the user's or the project's file, set or removed.
pub fn set(r: rpc::McpConfigSet) -> Reply {
    let (root, home, _) = places(&r.project)?;
    let path = match r.scope.as_str() {
        "user" => config::user_file(
            home.as_deref()
                .ok_or_else(|| bad("there is no home directory"))?,
        ),
        "project" => root.join(".mcp.json"),
        other => return Err(bad(format!("the scope is user or project, not {other}"))),
    };
    config::set(&path, &r.name, r.value.as_ref()).map_err(bad)?;
    reply(rpc::Done {})
}

/// The remote server `name` of the project's config, and where its sign-in is kept.
fn remote(project: &str, name: &str) -> Result<(ServerConfig, PathBuf), RpcError> {
    let (root, home, claude_dir) = places(project)?;
    let home = home.ok_or_else(|| bad("there is no home directory to keep the sign-in in"))?;
    let loaded = config::load(&root, Some(&home), claude_dir.as_deref(), |v| {
        std::env::var(v).ok()
    });
    let server = loaded
        .servers
        .iter()
        .chain(&loaded.pending)
        .find(|s| s.name == name)
        .cloned()
        .ok_or_else(|| bad(format!("there is no MCP server {name}")))?;
    if !matches!(server.transport, Transport::Http { .. }) {
        return Err(bad(format!(
            "{name} is a local server, with nothing to sign in to"
        )));
    }
    Ok((server, oauth::credentials_dir(&home)))
}

/// `mcp/login`: a sign-in in the user's browser.
pub async fn login(r: rpc::McpServerRef) -> Reply {
    let (server, dir) = remote(&r.project, &r.server)?;
    oauth::sign_in(&dir, &server, None, session::open_in_browser)
        .await
        .map_err(|why| RpcError::new(-32000, format!("{}: {why}", r.server)))?;
    reply(rpc::Done {})
}

/// `mcp/logout`: the sign-in forgotten.
pub fn logout(r: rpc::McpServerRef) -> Reply {
    let (server, dir) = remote(&r.project, &r.server)?;
    oauth::sign_out(&dir, &server).map_err(|why| RpcError::new(-32000, why))?;
    reply(rpc::Done {})
}
