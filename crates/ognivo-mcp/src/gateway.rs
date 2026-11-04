//! The servers of a session and their tools, as the model sees and calls them.
//!
//! - A tool is named as Claude Code names it, `mcp__<server>__<tool>`, with every
//!   character that is not a letter, a digit or `_` made `_` (a `.` would break gpt-oss's
//!   `to=functions.<name>`, a `-` a TypeScript name) and cut to 64; a name taken twice gets
//!   a number. Rules and hooks see the same name.
//! - The tools come in a fixed order, by name, so the prompt's prefix stays the server's
//!   cache across sessions.
//! - A result is text for the model: the content's text, what is not text named; the
//!   structured content when there is nothing else; an error marked as one; and no more than
//!   [`RESULT_CHARS`] of it.

use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, ClientCapabilities, ClientConfig, Implementation, ProtocolVersion,
};
use rmcp::service::{ClientLifecycleMode, ClientServiceExt as _, RoleClient, RunningService};
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde_json::{Map, Value};

use crate::config::{ServerConfig, Transport};

/// Characters of a result the model gets; the rest is cut with a note.
pub const RESULT_CHARS: usize = 24_000;
/// Characters of a tool's name, as OpenAI's API allows them.
const NAME_CHARS: usize = 64;

/// The client ognivo is to a server.
#[derive(Debug, Clone, Copy)]
struct Client;

impl rmcp::ClientHandler for Client {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("ognivo", env!("CARGO_PKG_VERSION")),
        )
    }
}

/// A tool of a server, under the name the model calls it by.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    /// `mcp__<server>__<tool>`, made a name.
    pub name: String,
    pub server: String,
    /// As the server names it.
    pub tool: String,
    pub description: String,
    /// Its arguments' JSON Schema.
    pub schema: Value,
    /// The server says it changes nothing; a hint only, from a server not checked.
    pub read_only: bool,
}

/// A server that started, or why it did not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Started {
    pub name: String,
    /// Its tools' count, or why it did not start.
    pub outcome: Result<usize, String>,
}

/// The session's servers that started, and their tools.
#[derive(Debug, Default)]
pub struct Gateway {
    clients: HashMap<String, RunningService<RoleClient, Client>>,
    tools: Vec<Tool>,
    started: Vec<Started>,
    /// The process groups of the stdio servers, which end with the gateway.
    groups: Vec<u32>,
}

impl Drop for Gateway {
    /// Ends the servers at once, as ognivo may be on its way out: `npx` and its node too.
    fn drop(&mut self) {
        for pid in &self.groups {
            if let Some(pid) = i32::try_from(*pid)
                .ok()
                .and_then(rustix::process::Pid::from_raw)
            {
                let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
            }
        }
    }
}

impl Gateway {
    /// Starts `servers` at once, each within `timeout`, and lists their tools; one that
    /// fails is left out, with why.
    pub async fn start(servers: &[ServerConfig], timeout: Duration) -> Gateway {
        let connecting = servers.iter().map(|s| async move {
            let r = tokio::time::timeout(timeout, connect(s)).await;
            let r = match r {
                Ok(r) => r,
                Err(_) => Err(format!("it did not answer within {} s", timeout.as_secs())),
            };
            (s.name.clone(), r)
        });
        let mut gateway = Gateway::default();
        let mut found: Vec<(String, rmcp::model::Tool)> = Vec::new();
        for (name, r) in futures::future::join_all(connecting).await {
            match r {
                Ok((client, tools, group)) => {
                    gateway.groups.extend(group);
                    gateway.started.push(Started {
                        name: name.clone(),
                        outcome: Ok(tools.len()),
                    });
                    found.extend(tools.into_iter().map(|t| (name.clone(), t)));
                    gateway.clients.insert(name, client);
                }
                Err(why) => gateway.started.push(Started {
                    name,
                    outcome: Err(why),
                }),
            }
        }
        gateway.tools = named(found);
        gateway
    }

    /// The tools, by name.
    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// The servers in the order they were configured, each started or not.
    pub fn started(&self) -> &[Started] {
        &self.started
    }

    /// Calls the tool the model knows as `name` with `arguments` (a JSON object), and gives
    /// the result as the model reads it; `Err` when no answer came: the text says why.
    pub async fn call(
        &self,
        name: &str,
        arguments: &str,
        timeout: Duration,
    ) -> Result<String, String> {
        let Some(tool) = self.tool(name) else {
            return Err(format!("there is no tool {name}"));
        };
        let Some(client) = self.clients.get(&tool.server) else {
            return Err(format!("the server {} is not running", tool.server));
        };
        let args: Map<String, Value> = match serde_json::from_str(arguments) {
            Ok(Value::Object(m)) => m,
            Ok(Value::Null) => Map::new(),
            _ => return Err(format!("{name}: the arguments are not a JSON object")),
        };
        let params = CallToolRequestParams::new(tool.tool.clone()).with_arguments(args);
        let called = tokio::time::timeout(timeout, client.call_tool(params)).await;
        let result = match called {
            Err(_) => {
                return Err(format!(
                    "{} gave no answer within {} s",
                    tool.server,
                    timeout.as_secs()
                ));
            }
            Ok(Err(e)) => return Err(format!("{}: {e}", tool.server)),
            Ok(Ok(r)) => r,
        };
        let value = serde_json::to_value(&result).map_err(|e| e.to_string())?;
        Ok(text_of(&tool.server, &tool.tool, &value))
    }

    /// Ends the servers: each is told, then its process group goes.
    pub async fn shutdown(mut self) {
        for (_, client) in std::mem::take(&mut self.clients) {
            let _ = client.cancel().await;
        }
    }
}

/// A started server and its tools.
#[allow(clippy::type_complexity)]
async fn connect(
    s: &ServerConfig,
) -> Result<
    (
        RunningService<RoleClient, Client>,
        Vec<rmcp::model::Tool>,
        Option<u32>,
    ),
    String,
> {
    let lifecycle = ClientLifecycleMode::Auto {
        preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        legacy_version: Some(ProtocolVersion::V_2025_06_18),
    };
    let (client, group) = match &s.transport {
        Transport::Stdio { command, args, env } => {
            let mut cmd = tokio::process::Command::new(command);
            cmd.args(args).envs(env);
            let mut wrap = process_wrap::tokio::CommandWrap::from(cmd);
            // `npx` and its node go together
            wrap.wrap(process_wrap::tokio::ProcessGroup::leader());
            let (process, _) = TokioChildProcess::builder(wrap)
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("{command}: {e}"))?;
            let group = process.id();
            let client = Client
                .serve_with_lifecycle(process, lifecycle)
                .await
                .map_err(|e| e.to_string())?;
            (client, group)
        }
        Transport::Http { url, headers } => {
            let mut custom = HashMap::new();
            for (k, v) in headers {
                let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|e| format!("header {k}: {e}"))?;
                let value = reqwest::header::HeaderValue::from_str(v)
                    .map_err(|e| format!("header {k}: {e}"))?;
                custom.insert(name, value);
            }
            let config =
                StreamableHttpClientTransportConfig::with_uri(url.as_str()).custom_headers(custom);
            let transport = StreamableHttpClientTransport::from_config(config);
            let client = Client
                .serve_with_lifecycle(transport, lifecycle)
                .await
                .map_err(|e| e.to_string())?;
            (client, None)
        }
    };
    let tools = client.list_all_tools().await.map_err(|e| e.to_string())?;
    Ok((client, tools, group))
}

/// The tools under the names the model calls them by, in order of those names.
fn named(found: Vec<(String, rmcp::model::Tool)>) -> Vec<Tool> {
    let mut tools: Vec<Tool> = Vec::new();
    for (server, t) in found {
        let v = serde_json::to_value(&t).unwrap_or(Value::Null);
        let tool = v["name"].as_str().unwrap_or_default().to_owned();
        let mut name = name_of(&server, &tool);
        let mut n = 2;
        while tools.iter().any(|t| t.name == name) {
            let suffix = format!("_{n}");
            name = format!(
                "{}{suffix}",
                cut(&name_of(&server, &tool), NAME_CHARS - suffix.len())
            );
            n += 1;
        }
        let mut schema = v["inputSchema"].clone();
        if !schema.is_object() {
            schema = serde_json::json!({"type": "object"});
        }
        tools.push(Tool {
            name,
            server: server.clone(),
            description: v["description"]
                .as_str()
                .unwrap_or_default()
                .trim()
                .to_owned(),
            schema,
            read_only: v["annotations"]["readOnlyHint"].as_bool() == Some(true),
            tool,
        });
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    tools
}

/// `mcp__<server>__<tool>` with what a name cannot hold made `_`, cut to 64.
pub fn name_of(server: &str, tool: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    cut(
        &format!("mcp__{}__{}", clean(server), clean(tool)),
        NAME_CHARS,
    )
    .to_owned()
}

fn cut(s: &str, n: usize) -> &str {
    &s[..s.len().min(n)]
}

/// A `tools/call` result as the model reads it, by its JSON: the first line names the call,
/// or says it failed.
pub fn text_of(server: &str, tool: &str, result: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for block in result["content"].as_array().into_iter().flatten() {
        let s = |k: &str| block[k].as_str().unwrap_or_default();
        parts.push(match s("type") {
            "text" => s("text").to_owned(),
            "image" | "audio" => format!(
                "[{} {}, {} bytes: not shown]",
                s("type"),
                s("mimeType"),
                s("data").len() / 4 * 3
            ),
            "resource_link" => format!("[resource {} <{}>]", s("name"), s("uri")),
            "resource" => {
                let r = &block["resource"];
                match r["text"].as_str() {
                    Some(text) => format!(
                        "[resource <{}>]\n{text}",
                        r["uri"].as_str().unwrap_or_default()
                    ),
                    None => format!(
                        "[resource <{}>: not text]",
                        r["uri"].as_str().unwrap_or_default()
                    ),
                }
            }
            other => format!("[{other}: not shown]"),
        });
    }
    if parts.is_empty()
        && let Some(s) = result.get("structuredContent").filter(|v| !v.is_null())
    {
        parts.push(serde_json::to_string_pretty(s).unwrap_or_default());
    }
    let body = parts.join("\n");
    let head = if result["isError"].as_bool() == Some(true) {
        format!("[error] {server} · {tool} failed:")
    } else {
        format!("[mcp: {server} · {tool}]")
    };
    let mut text = if body.is_empty() {
        format!("{head} (no content)")
    } else {
        format!("{head}\n{body}")
    };
    if text.len() > RESULT_CHARS {
        let mut at = RESULT_CHARS;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        let rest = text.len() - at;
        text.truncate(at);
        text.push_str(&format!(
            "\n[ognivo] cut here: {rest} more bytes of the result"
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_claude_codes_and_names_a_model_can_write() {
        assert_eq!(
            name_of("github", "create_issue"),
            "mcp__github__create_issue"
        );
        assert_eq!(
            name_of("my-docs.v2", "get.page"),
            "mcp__my_docs_v2__get_page"
        );
        assert_eq!(name_of("s", &"x".repeat(100)).len(), 64);
        let tool = |n: &str| -> rmcp::model::Tool {
            serde_json::from_value(json!({"name": n, "inputSchema": {"type": "object"}})).unwrap()
        };
        let tools = named(vec![
            ("s".into(), tool("b")),
            ("s".into(), tool("a-x")),
            ("s".into(), tool("a_x")),
        ]);
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["mcp__s__a_x", "mcp__s__a_x_2", "mcp__s__b"]);
        assert_eq!(tools[0].tool, "a-x");
    }

    #[test]
    fn a_result_is_text_the_model_can_read() {
        let r = json!({"content": [
            {"type": "text", "text": "two issues"},
            {"type": "image", "mimeType": "image/png", "data": "AAAA"},
            {"type": "resource_link", "name": "README", "uri": "file:///r/README.md"},
            {"type": "resource", "resource": {"uri": "mem://a", "text": "alpha"}},
        ]});
        assert_eq!(
            text_of("gh", "list", &r),
            "[mcp: gh · list]\ntwo issues\n[image image/png, 3 bytes: not shown]\n[resource README <file:///r/README.md>]\n[resource <mem://a>]\nalpha"
        );
        let failed = json!({"isError": true, "content": [{"type": "text", "text": "no token"}]});
        assert_eq!(
            text_of("gh", "list", &failed),
            "[error] gh · list failed:\nno token"
        );
        let structured = json!({"content": [], "structuredContent": {"n": 2}});
        assert_eq!(
            text_of("gh", "count", &structured),
            "[mcp: gh · count]\n{\n  \"n\": 2\n}"
        );
        let big = json!({"content": [{"type": "text", "text": "é".repeat(RESULT_CHARS)}]});
        let t = text_of("gh", "big", &big);
        assert!(
            t.len() < RESULT_CHARS + 100 && t.ends_with("more bytes of the result"),
            "{}",
            t.len()
        );
    }
}
