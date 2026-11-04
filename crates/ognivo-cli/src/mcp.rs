//! `ognivo mcp`: the MCP servers a session in this project gets, started as a session starts
//! them, with their tools under the names the model calls them by. A project's server that
//! waits for the user's trust is named and left alone: `ognivo chat` asks about it.
//!
//! Exit codes: 0 every server that may start did; 1 one did not; 2 bad usage.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Args;

use crate::session::{self, user_dirs};

/// How long a server may take to start: `npx` may fetch it first.
const START: Duration = Duration::from_secs(60);

#[derive(Debug, Args)]
pub struct McpArgs {
    /// Project root; the current directory by default
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Where sessions and logs go; ~/.ognivo/data by default
    #[arg(long)]
    data_dir: Option<PathBuf>,
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
    let mut loaded = ognivo_mcp::config::load(&root, home.as_deref(), claude_dir.as_deref(), |v| {
        std::env::var(v).ok()
    });
    for p in &loaded.problems {
        println!("! {p}");
    }
    if !loaded.pending.is_empty() {
        let key = format!("mcp\n{}", loaded.pending_key());
        match store.trusted(&root, &key) {
            Ok(Some(true)) => loaded.trust(),
            answer => {
                let why = match answer {
                    Ok(Some(false)) => "you did not trust them",
                    _ => "they wait for your trust: ognivo chat asks",
                };
                for s in &loaded.pending {
                    println!(
                        "· {} ({}): {} — does not start: {why}",
                        s.name,
                        s.source,
                        s.shown()
                    );
                }
            }
        }
    }
    if loaded.servers.is_empty() {
        println!("no MCP server starts for {}", root.display());
        return Ok(ExitCode::SUCCESS);
    }
    let gateway = ognivo_mcp::Gateway::start(&loaded.servers, START).await;
    let mut failed = false;
    for (config, started) in loaded.servers.iter().zip(gateway.started()) {
        match &started.outcome {
            Ok(n) => {
                println!("✓ {} ({}): {}", config.name, config.source, config.shown());
                println!("  {n} tool{}", if *n == 1 { "" } else { "s" });
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
