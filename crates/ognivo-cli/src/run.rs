//! `ognivo run`: one task, no user (docs/architecture.md §10). The prompt comes on stdin,
//! as the M0 harness passes it; the final answer goes to stdout, followed by ognivo's own
//! verification status (what the turn's tool calls show, not what the answer claims).
//!
//! The run is kept in the session store like a chat (`ognivo sessions` lists it), the end
//! event names its session, and `--atif FILE` writes its trajectory in ATIF for Harbor.
//!
//! Exit codes: 0 the model answered; 1 it hit the step limit or got stuck; 2 bad usage;
//! 3 the server failed; 124 the time limit ran out (as with `timeout`); 128 + N stopped by
//! signal N (TERM from `timeout`, INT from ^C): the turn is dropped, which kills the process
//! group of a running command, and the end event is still written. Without an answer the
//! end event carries the verification status of what was done.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, ValueEnum};
use ognivo_core::agent::{AgentEvent, AllowAll, Approver, NoUser, TurnEnd};
use ognivo_core::verify;
use tokio::signal::unix::{SignalKind, signal};

use crate::session::{Failure, SessionArgs};
use crate::view::{compacted, given, kilo, not_read};

#[derive(Debug, Args)]
pub struct RunArgs {
    #[command(flatten)]
    session: SessionArgs,
    /// Actions that would ask: deny them and tell the model why, or approve them all
    /// (only inside a container: an eval sandbox, never the host)
    #[arg(long, value_enum, default_value_t = Approve::Deny)]
    approve: Approve,
    /// Wall-clock limit in seconds: the model is told it, commands never run past it, and
    /// the run ends itself with exit code 124 when it runs out
    #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..))]
    time_limit: Option<u64>,
    /// Events as JSON lines on stdout instead of text
    #[arg(long)]
    jsonl: bool,
    /// Write the run's trajectory in ATIF (Harbor's trajectory.json) to this file at the end
    #[arg(long, value_name = "FILE")]
    atif: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Approve {
    Deny,
    Auto,
}

fn fail(f: Failure) -> anyhow::Result<ExitCode> {
    eprintln!("ognivo run: {}", f.message);
    Ok(ExitCode::from(f.code))
}

pub async fn run(a: RunArgs) -> anyhow::Result<ExitCode> {
    let root = match a.session.root() {
        Ok(root) => root,
        Err(f) => return fail(f),
    };
    let approver: Box<dyn Approver> = match a.approve {
        Approve::Deny => Box::new(NoUser),
        Approve::Auto if in_container() => Box::new(AllowAll),
        Approve::Auto => {
            return fail(Failure::usage(
                "--approve auto works only inside a container",
            ));
        }
    };
    let mut prompt = String::new();
    std::io::stdin().read_to_string(&mut prompt)?;
    if prompt.trim().is_empty() {
        return fail(Failure::usage(
            "the prompt comes on stdin, and stdin was empty",
        ));
    }
    let store = match a.session.store() {
        Ok(store) => store,
        Err(f) => return fail(f),
    };
    let mut s = match a.session.connect(root.clone(), None).await {
        Ok(s) => s,
        Err(f) => return fail(f),
    };
    for p in &s.rule_problems {
        eprintln!("· not a rule: {p}");
    }
    // the project's allow rules count only as the user answered in chat
    if let Some(pending) = s.pending.take() {
        match store.trusted(&root, &pending.key()) {
            Ok(Some(true)) => s.opts.rules.trust(pending),
            _ => eprintln!(
                "· the project's allow rules are not used: trust them once in ognivo chat"
            ),
        }
    }
    let seed = s.opts.seed;
    s.opts.time_limit = a.time_limit.map(Duration::from_secs);
    let mut agent = match s.new_agent("run", &a.session.url, store, approver) {
        Ok(agent) => agent,
        Err(f) => return fail(f),
    };
    let session = agent.session_id();
    let jsonl = a.jsonl;
    let start = agent.instructions();
    if !start.files.is_empty() || !start.skipped.is_empty() {
        let e = AgentEvent::Instructions {
            files: start.files.clone(),
            skipped: start.skipped.clone(),
        };
        show(&e, jsonl);
    }
    let mut on = move |e: AgentEvent| show(&e, jsonl);
    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    let end = {
        let turn = agent.turn(&prompt, &mut on);
        tokio::pin!(turn);
        tokio::select! {
            end = &mut turn => Ok(end),
            _ = term.recv() => Err(("SIGTERM", 143)),
            _ = int.recv() => Err(("SIGINT", 130)),
        }
    };
    if let Err((name, _)) = &end {
        agent.abandon_turn(&format!("ognivo was stopped by {name}."));
    }
    if let Some(e) = agent.store_failure() {
        eprintln!("ognivo run: the session store failed: {e}; the session is not saved in full");
    }
    if let (Some(file), Some(id)) = (&a.atif, session) {
        write_trajectory(&a.session, id, file);
    }
    let stats = serde_json::to_value(&agent.stats)?;
    let verification = serde_json::to_value(agent.verification())?;
    let end = match end {
        Ok(end) => end,
        Err((name, code)) => {
            eprintln!("ognivo run: stopped by {name}");
            if jsonl {
                let end = serde_json::json!({"type": "end", "exit": code, "answer": null, "stopped": name, "verification": verification, "session": session, "seed": seed, "stats": stats});
                println!("{end}");
            }
            return Ok(ExitCode::from(code));
        }
    };
    let (code, answer, stopped) = match end {
        Ok(TurnEnd::Answer(a)) => (0, Some(a), None),
        Ok(TurnEnd::StepLimit) => {
            eprintln!("ognivo run: step limit ({}) reached", a.session.max_steps);
            (1, None, Some("step_limit"))
        }
        Ok(TurnEnd::Stuck(why)) => {
            eprintln!("ognivo run: stopped: {why}");
            (1, None, Some("stuck"))
        }
        Ok(TurnEnd::TimeLimit) => {
            eprintln!(
                "ognivo run: time limit ({} s) reached",
                a.time_limit.unwrap_or_default()
            );
            (124, None, Some("time_limit"))
        }
        Err(e) => {
            eprintln!("ognivo run: {e}");
            (3, None, Some("server_error"))
        }
    };
    if jsonl {
        let mut end = serde_json::json!({"type": "end", "exit": code, "answer": answer, "session": session, "seed": seed, "stats": stats});
        if let Some(why) = stopped {
            end["stopped"] = why.into();
            end["verification"] = verification;
        }
        println!("{end}");
    } else {
        if let Some(a) = &answer {
            println!();
            if let Some(line) = verify::summary(&a.verification) {
                println!("\n[ognivo] {line}");
            }
            if a.unsupported_claim {
                println!("[ognivo] the answer above claims a check that did not happen");
            }
        }
        eprintln!(
            "ognivo run: session {}; seed {seed}; {stats}",
            session.unwrap_or_default()
        );
    }
    Ok(ExitCode::from(code))
}

/// Writes the trajectory of session `id`; a failure is reported and changes no exit code.
fn write_trajectory(args: &SessionArgs, id: ognivo_store::SessionId, file: &Path) {
    let written = args
        .store()
        .map_err(|f| f.message)
        .and_then(|store| ognivo_core::trace::atif(&store, id).map_err(|e| e.to_string()))
        .and_then(|t| {
            let text = serde_json::to_string_pretty(&t).map_err(|e| e.to_string())? + "\n";
            std::fs::write(file, text).map_err(|e| format!("{}: {e}", file.display()))
        });
    if let Err(e) = written {
        eprintln!("ognivo run: the trajectory is not written: {e}");
    }
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
        AgentEvent::ToolStarted {
            tool, arguments, ..
        } => {
            let mut args: String = arguments.chars().take(160).collect();
            if args.len() < arguments.len() {
                args.push('…');
            }
            eprintln!("\n→ {tool} {args}");
        }
        AgentEvent::ToolFinished { result, .. } => {
            eprintln!("← {}", result.lines().next().unwrap_or_default());
        }
        AgentEvent::FileChanged { path, hunks, .. } => {
            let removed: usize = hunks.iter().map(|h| h.removed.len()).sum();
            let added: usize = hunks.iter().map(|h| h.added.len()).sum();
            eprintln!("· {path}: +{added} -{removed}");
        }
        AgentEvent::Instructions { files, skipped } => {
            if !files.is_empty() {
                eprintln!("· instructions: {}", given(files));
            }
            for s in skipped {
                eprintln!("{}", not_read(s));
            }
        }
        AgentEvent::Compacting { tokens, limit } => eprintln!(
            "· compacting the conversation: about {} tokens, over the {} budget",
            kilo(*tokens),
            kilo(*limit)
        ),
        AgentEvent::Compacted {
            replaced,
            kept,
            summary,
        } => eprintln!("{}", compacted(*replaced, *kept, summary.as_deref()).0),
        AgentEvent::Refused { tool, reason } => eprintln!("✗ {tool}: {reason}"),
        AgentEvent::Notice { text } => eprintln!("· {text}"),
    }
}

/// Podman and Docker mark their containers with these files.
fn in_container() -> bool {
    Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists()
}
