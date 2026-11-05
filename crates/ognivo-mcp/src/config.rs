//! Where MCP servers are configured, in the `mcpServers` format of Claude Code, Cursor and
//! llama.cpp (`servers` too, as VS Code writes it):
//!
//! - `~/.claude.json`: the servers Claude Code keeps for this project (its local scope),
//!   then its user's;
//! - `.mcp.json` in the project's root: the project's, which start only once the user trusts
//!   exactly them ([`Loaded::pending`]): a cloned repository must not run commands;
//! - `~/.ognivo/mcp.json`: the user's own.
//!
//! A name configured in two places is the first one's, in that order: local, project, the
//! user's own, Claude Code's user's. `${VAR}` and `${VAR:-default}` in a command, its
//! arguments, its environment, a URL, its headers and its `oauth` texts take the variable's
//! value.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// One server as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub name: String,
    pub transport: Transport,
    /// Where it is configured, as the user names it: `.mcp.json`, `~/.ognivo/mcp.json`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
    Http {
        url: String,
        headers: BTreeMap<String, String>,
        oauth: OAuth,
    },
}

/// What signing in to a remote server needs from its config, `oauth` as Claude Code writes
/// it; without it the client registers itself with the server's authorization server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OAuth {
    /// A client registered beforehand (`clientId`), and its secret (`clientSecret`).
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    /// The scopes to ask for (`scopes`, a list or a text of them); none: the server's.
    pub scopes: Vec<String>,
    /// The loopback port the browser comes back to (`callbackPort`); none: any free one.
    pub callback_port: Option<u16>,
}

impl ServerConfig {
    /// What starts it, for the user to see before it does: the command line or the URL.
    pub fn shown(&self) -> String {
        match &self.transport {
            Transport::Stdio { command, args, .. } => {
                let mut line = command.clone();
                for a in args {
                    line.push(' ');
                    line.push_str(a);
                }
                line
            }
            Transport::Http { url, .. } => url.clone(),
        }
    }
}

/// What [`load`] found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loaded {
    /// The servers that may start: the user's, and the project's once trusted.
    pub servers: Vec<ServerConfig>,
    /// The project's servers (`.mcp.json`), which wait for the user's trust.
    pub pending: Vec<ServerConfig>,
    /// Servers and files that could not be used, with why.
    pub problems: Vec<String>,
}

impl Loaded {
    /// The project's servers as one text, the key under which the user's answer is kept:
    /// another command is another question.
    pub fn pending_key(&self) -> String {
        let mut lines: Vec<String> = self
            .pending
            .iter()
            .map(|s| format!("mcp {}: {:?}", s.name, s.transport))
            .collect();
        lines.sort();
        lines.join("\n")
    }

    /// The project's servers may start too.
    pub fn trust(&mut self) {
        self.servers.append(&mut self.pending);
    }
}

/// Reads the configs of the project at `root` and of the user whose home is `home`;
/// `claude_dir` is Claude Code's `CLAUDE_CONFIG_DIR` when set, whose `.claude.json` then
/// counts instead of the home's. `var` gives the environment's variables.
pub fn load(
    root: &Path,
    home: Option<&Path>,
    claude_dir: Option<&Path>,
    var: impl Fn(&str) -> Option<String>,
) -> Loaded {
    let mut loaded = Loaded::default();
    let mut taken: Vec<String> = Vec::new();
    let claude_json = claude_dir
        .map(|d| d.join(".claude.json"))
        .or_else(|| home.map(|h| h.join(".claude.json")));
    let claude = claude_json
        .as_deref()
        .and_then(|p| read(p, "~/.claude.json", &mut loaded.problems));
    // Claude Code's local scope: its servers for this project, kept outside it
    if let Some(local) = claude
        .as_ref()
        .and_then(|c| c.get("projects"))
        .and_then(|p| p.get(root.to_string_lossy().as_ref()))
    {
        add(
            local,
            "~/.claude.json",
            false,
            &var,
            &mut taken,
            &mut loaded,
        );
    }
    let project = root.join(".mcp.json");
    if let Some(v) = read(&project, ".mcp.json", &mut loaded.problems) {
        add(&v, ".mcp.json", true, &var, &mut taken, &mut loaded);
    }
    if let Some(home) = home {
        let own = home.join(".ognivo/mcp.json");
        if let Some(v) = read(&own, "~/.ognivo/mcp.json", &mut loaded.problems) {
            add(
                &v,
                "~/.ognivo/mcp.json",
                false,
                &var,
                &mut taken,
                &mut loaded,
            );
        }
    }
    if let Some(c) = &claude {
        add(c, "~/.claude.json", false, &var, &mut taken, &mut loaded);
    }
    loaded
}

/// A config file's JSON; nothing when there is no file.
fn read(path: &Path, shown: &str, problems: &mut Vec<String>) -> Option<Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            problems.push(format!("{shown}: {e}"));
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            problems.push(format!("{shown}: {e}"));
            None
        }
    }
}

/// The servers of one config object.
fn add(
    config: &Value,
    source: &str,
    of_project: bool,
    var: &impl Fn(&str) -> Option<String>,
    taken: &mut Vec<String>,
    loaded: &mut Loaded,
) {
    let Some(servers) = config
        .get("mcpServers")
        .or_else(|| config.get("servers"))
        .and_then(Value::as_object)
    else {
        return;
    };
    for (name, entry) in servers {
        if taken.contains(name) {
            continue;
        }
        match server(name, entry, source, var) {
            Ok(Some(s)) => {
                taken.push(name.clone());
                if of_project {
                    loaded.pending.push(s);
                } else {
                    loaded.servers.push(s);
                }
            }
            Ok(None) => taken.push(name.clone()),
            Err(why) => loaded.problems.push(format!("{name} in {source}: {why}")),
        }
    }
}

/// One server's entry; `None` for one that is turned off.
fn server(
    name: &str,
    entry: &Value,
    source: &str,
    var: &impl Fn(&str) -> Option<String>,
) -> Result<Option<ServerConfig>, String> {
    let Some(e) = entry.as_object() else {
        return Err("not an object".into());
    };
    if e.get("disabled").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    let text = |key: &str| -> Result<Option<String>, String> {
        match e.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => expand(s, var).map(Some),
            Some(_) => Err(format!("`{key}` is not a string")),
        }
    };
    let kind = text("type")?;
    let transport = match (kind.as_deref(), text("command")?, text("url")?) {
        (None | Some("stdio"), Some(command), _) => Transport::Stdio {
            command,
            args: match e.get("args") {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => a
                    .iter()
                    .map(|v| match v {
                        Value::String(s) => expand(s, var),
                        other => Ok(other.to_string()),
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("`args` is not a list".into()),
            },
            env: strings(e.get("env"), "env", var)?,
        },
        (None | Some("http" | "streamable-http" | "streamableHttp"), None, Some(url)) => {
            Transport::Http {
                url,
                headers: strings(e.get("headers"), "headers", var)?,
                oauth: oauth(e.get("oauth"), var)?,
            }
        }
        (Some("sse"), _, _) => {
            return Err(
                "the HTTP+SSE transport is gone from MCP; ognivo speaks stdio and Streamable HTTP"
                    .into(),
            );
        }
        (Some(other), _, _) => return Err(format!("no transport {other:?}")),
        (None, None, None) => return Err("neither `command` nor `url`".into()),
    };
    Ok(Some(ServerConfig {
        name: name.to_owned(),
        transport,
        source: source.to_owned(),
    }))
}

/// An entry's `oauth`.
fn oauth(value: Option<&Value>, var: &impl Fn(&str) -> Option<String>) -> Result<OAuth, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(OAuth::default());
    };
    let Some(o) = value.as_object() else {
        return Err("`oauth` is not an object".into());
    };
    let text = |key: &str| -> Result<Option<String>, String> {
        match o.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => expand(s, var).map(Some),
            Some(_) => Err(format!("`oauth.{key}` is not a string")),
        }
    };
    let scopes = match o.get("scopes").or_else(|| o.get("scope")) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(s)) => expand(s, var)?
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| match v {
                Value::String(s) => expand(s, var),
                _ => Err("`oauth.scopes` holds something not a string".into()),
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err("`oauth.scopes` is not a list".into()),
    };
    let callback_port = match o.get("callbackPort") {
        None | Some(Value::Null) => None,
        Some(Value::Number(n)) => Some(
            n.as_u64()
                .and_then(|n| u16::try_from(n).ok())
                .ok_or("`oauth.callbackPort` is not a port")?,
        ),
        Some(Value::String(s)) => Some(
            expand(s, var)?
                .parse::<u16>()
                .map_err(|_| "`oauth.callbackPort` is not a port")?,
        ),
        Some(_) => return Err("`oauth.callbackPort` is not a port".into()),
    };
    Ok(OAuth {
        client_id: text("clientId")?,
        client_secret: text("clientSecret")?,
        scopes,
        callback_port,
    })
}

/// An object of strings, each expanded.
fn strings(
    value: Option<&Value>,
    key: &str,
    var: &impl Fn(&str) -> Option<String>,
) -> Result<BTreeMap<String, String>, String> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(BTreeMap::new());
    };
    let Some(obj) = value.as_object() else {
        return Err(format!("`{key}` is not an object"));
    };
    obj.iter()
        .map(|(k, v)| match v {
            Value::String(s) => expand(s, var).map(|s| (k.clone(), s)),
            other => Ok((k.clone(), other.to_string())),
        })
        .collect()
}

/// `text` with `${VAR}` and `${VAR:-default}` replaced, as Claude Code expands `.mcp.json`.
fn expand(text: &str, var: &impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("${") {
        out.push_str(&rest[..at]);
        let Some(end) = rest[at..].find('}') else {
            return Err(format!("`${{` without `}}` in {text:?}"));
        };
        let inner = &rest[at + 2..at + end];
        let (name, default) = match inner.split_once(":-") {
            Some((n, d)) => (n, Some(d)),
            None => (inner, None),
        };
        match (var(name), default) {
            (Some(v), _) => out.push_str(&v),
            (None, Some(d)) => out.push_str(d),
            (None, None) => return Err(format!("${{{name}}} is not set")),
        }
        rest = &rest[at + end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `~/.ognivo/mcp.json` of the home `home`.
pub fn user_file(home: &Path) -> PathBuf {
    home.join(".ognivo/mcp.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Map, json};

    /// The config objects of `servers`, as the files hold them.
    fn json(servers: &[(&str, Value)]) -> Value {
        let map: Map<String, Value> = servers
            .iter()
            .map(|(n, v)| ((*n).to_owned(), v.clone()))
            .collect();
        json!({ "mcpServers": map })
    }

    fn env(name: &str) -> Option<String> {
        match name {
            "TOKEN" => Some("t0k".into()),
            "HOME" => Some("/home/u".into()),
            _ => None,
        }
    }

    #[test]
    fn servers_come_from_every_place_and_the_first_name_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (root, home) = (dir.path().join("proj"), dir.path().join("home"));
        std::fs::create_dir_all(home.join(".ognivo")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let write = |p: PathBuf, v: Value| std::fs::write(p, v.to_string()).unwrap();
        write(
            root.join(".mcp.json"),
            json(&[
                (
                    "repo",
                    json!({"command": "npx", "args": ["-y", "repo-mcp"], "env": {"KEY": "${TOKEN}"}}),
                ),
                (
                    "docs",
                    json!({"type": "http", "url": "https://docs.example/mcp", "headers": {"Authorization": "Bearer ${TOKEN}"}}),
                ),
                (
                    "old",
                    json!({"type": "sse", "url": "https://old.example/sse"}),
                ),
                (
                    "signed",
                    json!({"type": "http", "url": "https://signed.example/mcp", "oauth": {"clientId": "ognivo-${TOKEN}", "scopes": "read write", "callbackPort": 33418}}),
                ),
            ]),
        );
        write(
            user_file(&home),
            json(&[
                ("docs", json!({"command": "never"})),
                (
                    "notes",
                    json!({"command": "${HOME}/bin/notes", "args": ["--dir", "${NOTES:-/tmp/notes}"]}),
                ),
                ("off", json!({"command": "x", "disabled": true})),
            ]),
        );
        write(
            home.join(".claude.json"),
            json!({
                "mcpServers": {"gh": {"type": "stdio", "command": "gh-mcp"}, "notes": {"command": "shadowed"}},
                "projects": {root.to_string_lossy(): {"mcpServers": {"local": {"command": "mine", "args": []}}}},
            }),
        );
        let l = load(&root, Some(&home), None, env);
        let names = |v: &[ServerConfig]| v.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&l.servers), ["local", "notes", "gh"]);
        // in the order of their file
        assert_eq!(names(&l.pending), ["repo", "docs", "signed"]);
        assert_eq!(
            l.servers[1].transport,
            Transport::Stdio {
                command: "/home/u/bin/notes".into(),
                args: vec!["--dir".into(), "/tmp/notes".into()],
                env: BTreeMap::new(),
            }
        );
        assert_eq!(l.pending[0].shown(), "npx -y repo-mcp");
        assert!(
            matches!(&l.pending[1].transport, Transport::Http { headers, oauth, .. } if headers["Authorization"] == "Bearer t0k" && *oauth == OAuth::default())
        );
        assert!(matches!(
            &l.pending[2].transport,
            Transport::Http { oauth, .. } if *oauth == OAuth {
                client_id: Some("ognivo-t0k".into()),
                client_secret: None,
                scopes: vec!["read".into(), "write".into()],
                callback_port: Some(33418),
            }
        ));
        assert_eq!(l.problems.len(), 1);
        assert!(
            l.problems[0].starts_with("old in .mcp.json: the HTTP+SSE transport is gone"),
            "{:?}",
            l.problems
        );
        // trusting is for exactly these servers
        let key = l.pending_key();
        assert!(key.contains("mcp repo:") && key.contains("t0k"), "{key}");
        let mut trusted = l.clone();
        trusted.trust();
        assert_eq!(trusted.servers.len(), 6);
        assert!(trusted.pending.is_empty());
    }

    #[test]
    fn a_variable_without_a_value_or_a_default_is_a_problem() {
        assert_eq!(expand("a${TOKEN}b", &env), Ok("at0kb".into()));
        assert_eq!(expand("${NOPE:-x}", &env), Ok("x".into()));
        assert!(expand("${NOPE}", &env).is_err());
        assert!(expand("${OPEN", &env).is_err());
    }
}
