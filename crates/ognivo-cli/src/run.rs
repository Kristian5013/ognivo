//! `ognivo run`: one task, no user (docs/architecture.md §10). The prompt comes on stdin,
//! as the M0 harness passes it; the final answer goes to stdout.
//!
//! Exit codes: 0 the model answered; 1 it hit the step limit or got stuck; 2 bad usage;
//! 3 the server failed.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use ognivo_core::agent::{Agent, AgentEvent, AgentOptions, AllowAll, Approver, NoUser, TurnEnd};
use ognivo_core::approval::WorkspaceState;
use ognivo_models::llama::LlamaServer;
use ognivo_models::{ChatDriver, doctor, profile};

#[derive(Debug, Args)]
pub struct RunArgs {
    /// Server base URL, e.g. http://127.0.0.1:8090
    #[arg(long)]
    url: String,
    /// Bearer token if the server was started with --api-key
    #[arg(long, env = "OGNIVO_API_KEY", hide_env_values = true)]
    api_key: Option<String>,
    /// Profile id (gpt-oss-20b, gemma-4-26b-a4b, qwen3.8-27b); detected from the server when omitted
    #[arg(long)]
    profile: Option<String>,
    /// Project root; the current directory by default
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Actions that would ask: deny them and tell the model why, or approve them all
    /// (only inside a container: an eval sandbox, never the host)
    #[arg(long, value_enum, default_value_t = Approve::Deny)]
    approve: Approve,
    #[arg(long, default_value_t = 200)]
    max_steps: u32,
    /// Seed of the first step (random when omitted; printed at the end)
    #[arg(long)]
    seed: Option<u32>,
    /// Reasoning level from the profile (e.g. low/medium/high for gpt-oss)
    #[arg(long)]
    level: Option<String>,
    /// Server slot to pin, so the session keeps its KV cache
    #[arg(long)]
    slot: Option<u32>,
    /// Events as JSON lines on stdout instead of text
    #[arg(long)]
    jsonl: bool,
    /// Where logs go; ~/.ognivo/data by default
    #[arg(long)]
    data_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Approve {
    Deny,
    Auto,
}

fn usage(msg: impl std::fmt::Display) -> anyhow::Result<ExitCode> {
    eprintln!("ognivo run: {msg}");
    Ok(ExitCode::from(2))
}

pub async fn run(a: RunArgs) -> anyhow::Result<ExitCode> {
    let dir = match &a.cwd {
        Some(d) => d.clone(),
        None => std::env::current_dir()?,
    };
    let Ok(root) = std::fs::canonicalize(&dir) else {
        return usage(format!("{}: no such directory", dir.display()));
    };
    let approver: Box<dyn Approver> = match a.approve {
        Approve::Deny => Box::new(NoUser),
        Approve::Auto if in_container() => Box::new(AllowAll),
        Approve::Auto => return usage("--approve auto works only inside a container"),
    };
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() {
        return usage("the prompt comes on stdin, and stdin was empty");
    }
    let data_dir = match a.data_dir {
        Some(d) => d,
        None => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".ognivo/data"),
            None => return usage("HOME is not set; pass --data-dir"),
        },
    };

    let server = LlamaServer::new(&a.url, a.api_key)?;
    let profiles = profile::builtin()?;
    let chosen = match &a.profile {
        Some(id) => profiles.iter().find(|p| &p.id == id),
        None => match doctor::identify(&server, &profiles).await {
            Ok(found) => found.map(|(p, _)| p),
            Err(e) => {
                eprintln!("ognivo run: {}: {e}", a.url);
                return Ok(ExitCode::from(3));
            }
        },
    };
    let Some(profile) = chosen.cloned() else {
        return usage("no profile matches this server; pass --profile (see ognivo doctor)");
    };
    let family = profile.family;
    let max_tokens = profile.context.max_tokens;
    let driver = match ChatDriver::new(server, profile, a.level.as_deref(), a.slot) {
        Ok(d) => d,
        Err(e) => return usage(e),
    };
    let seed = a.seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.subsec_nanos())
    });
    let opts = AgentOptions {
        workspace: start_state(&root),
        root,
        data_dir,
        max_steps: a.max_steps,
        max_tokens,
        seed,
    };
    let mut agent = Agent::new(Box::new(driver), family, approver, opts);
    let jsonl = a.jsonl;
    let mut on = move |e: AgentEvent| show(&e, jsonl);
    let end = agent.turn(&prompt, &mut on).await;
    let stats = serde_json::to_value(&agent.stats)?;
    let (code, answer) = match end {
        Ok(TurnEnd::Answer(text)) => (0, Some(text)),
        Ok(TurnEnd::StepLimit) => {
            eprintln!("ognivo run: step limit ({}) reached", a.max_steps);
            (1, None)
        }
        Ok(TurnEnd::Stuck(why)) => {
            eprintln!("ognivo run: stopped: {why}");
            (1, None)
        }
        Err(e) => {
            eprintln!("ognivo run: {e}");
            (3, None)
        }
    };
    if jsonl {
        let end = serde_json::json!({"type": "end", "exit": code, "answer": answer, "seed": seed, "stats": stats});
        println!("{end}");
    } else {
        if answer.is_some() {
            println!();
        }
        eprintln!("ognivo run: seed {seed}; {stats}");
    }
    Ok(ExitCode::from(code))
}

/// Text mode: the model's words on stdout as they stream, tool activity on stderr.
fn show(e: &AgentEvent, jsonl: bool) {
    if jsonl {
        if let Ok(line) = serde_json::to_string(e) {
            println!("{line}");
        }
        return;
    }
    match e {
        AgentEvent::Text { text } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        AgentEvent::Reasoning { .. } => {}
        AgentEvent::Restarted { reason } => eprintln!("\n· restarted: {reason}"),
        AgentEvent::ToolStarted { tool, arguments } => {
            let mut args: String = arguments.chars().take(160).collect();
            if args.len() < arguments.len() {
                args.push('…');
            }
            eprintln!("\n→ {tool} {args}");
        }
        AgentEvent::ToolFinished { result, .. } => {
            eprintln!("← {}", result.lines().next().unwrap_or_default());
        }
        AgentEvent::Refused { tool, reason } => eprintln!("✗ {tool}: {reason}"),
        AgentEvent::Notice { text } => eprintln!("· {text}"),
    }
}

/// Podman and Docker mark their containers with these files.
fn in_container() -> bool {
    Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists()
}

/// Clean only when git says nothing is changed or untracked; otherwise, or outside git,
/// assume unreviewed changes exist (§8: never assume clean).
fn start_state(root: &Path) -> WorkspaceState {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.fsmonitor=false",
            "--no-pager",
            "status",
            "--porcelain",
        ])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(o) if o.status.success() && o.stdout.is_empty() => WorkspaceState::CLEAN,
        _ => WorkspaceState::DIRTY,
    }
}
