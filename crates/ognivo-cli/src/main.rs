//! The `ognivo` binary. Only `doctor` works in M0; the rest follow the milestones in
//! docs/architecture.md §13.

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use ognivo_models::doctor::{self, DoctorOptions, DoctorReport, Level};
use ognivo_models::{ServerClass, ServerKind, StreamIds, profile};

/// Upper bound for one server's checks, so a stuck server cannot hang the CLI.
const DOCTOR_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug, Parser)]
#[command(
    name = "ognivo",
    version,
    about = "Coding agent for local models served by llama.cpp"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check running llama-server / ik_llama.cpp servers
    Doctor {
        /// Server base URL; repeat the flag to check several servers
        #[arg(long = "url", default_value = "http://127.0.0.1:8080")]
        urls: Vec<String>,
        /// Bearer token if the server was started with --api-key
        #[arg(long, env = "OGNIVO_API_KEY", hide_env_values = true)]
        api_key: Option<String>,
        /// Also run a short generation to verify that every token id is streamed (uses the GPU)
        #[arg(long)]
        probe: bool,
        /// Print the reports as JSON
        #[arg(long)]
        json: bool,
    },
    /// Run one task without the GUI (M1)
    Run { task: String },
    /// Line-mode chat in the terminal (M1)
    Chat,
}

#[tokio::main]
async fn main() -> anyhow::Result<ExitCode> {
    match Cli::parse().command {
        Command::Doctor {
            urls,
            api_key,
            probe,
            json,
        } => {
            let profiles = profile::builtin()?;
            let mut failed = false;
            let mut reports = Vec::new();
            for url in &urls {
                let check = doctor::diagnose(
                    url,
                    api_key.clone(),
                    &profiles,
                    DoctorOptions {
                        probe_stream: probe,
                    },
                );
                let report = tokio::time::timeout(DOCTOR_TIMEOUT, check)
                    .await
                    .unwrap_or_else(|_| {
                        let mut r = DoctorReport::new(url);
                        r.note(
                            Level::Error,
                            format!("no answer within {} s", DOCTOR_TIMEOUT.as_secs()),
                        );
                        r
                    });
                failed |= report.has_errors();
                if !json {
                    print_report(&report);
                }
                reports.push(report);
            }
            if json {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            }
            Ok(if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            })
        }
        Command::Run { .. } | Command::Chat => {
            eprintln!("not implemented yet: arrives in M1 (docs/architecture.md §13)");
            Ok(ExitCode::from(2))
        }
    }
}

fn build_label(build: &Option<u32>, commit: &Option<String>) -> String {
    format!(
        "{}{}",
        build.map(|b| format!("b{b}")).unwrap_or_else(|| "?".into()),
        commit
            .as_deref()
            .map(|c| format!(" ({c})"))
            .unwrap_or_default()
    )
}

fn print_report(r: &DoctorReport) {
    println!("ognivo doctor — {}", r.url);
    if r.reachable {
        let server = match &r.kind {
            Some(ServerKind::LlamaCpp { build, commit }) => {
                format!("llama.cpp {}", build_label(build, commit))
            }
            Some(ServerKind::Router { build, commit }) => {
                format!("llama.cpp router {}", build_label(build, commit))
            }
            Some(ServerKind::IkLlama) => "ik_llama.cpp".into(),
            Some(ServerKind::Unknown) | None => "unknown server".into(),
        };
        let class = match r.class {
            Some(ServerClass::A) => "class A",
            Some(ServerClass::B) => "class B",
            Some(ServerClass::C) => "class C",
            None => "class ?",
        };
        println!(
            "  server    {server} · {class} · n_ctx {} · slots {}",
            r.n_ctx.map_or("?".into(), |n| n.to_string()),
            r.total_slots.map_or("?".into(), |n| n.to_string())
        );
        if r.profile.is_some() || r.model_path.is_some() {
            println!(
                "  model     {} ({}) · {}",
                r.profile.as_deref().unwrap_or("unknown"),
                r.family.map_or("generic".into(), |f| f.to_string()),
                r.model_path.as_deref().unwrap_or("?")
            );
        }
        if let Some(sha) = &r.chat_template_sha256 {
            println!("  template  sha256 {}…", &sha[..12.min(sha.len())]);
        }
        match r.stream_ids {
            StreamIds::Complete => println!("  stream    all token ids streamed"),
            StreamIds::Lossy {
                received,
                predicted,
            } => println!("  stream    {received} of {predicted} token ids streamed"),
            StreamIds::Missing => println!("  stream    no token ids streamed"),
            StreamIds::Inconclusive => println!("  stream    probe inconclusive"),
            StreamIds::NotProbed => {}
        }
    }
    for f in &r.findings {
        let mark = match f.level {
            Level::Ok => "✓",
            Level::Info => "·",
            Level::Warn => "!",
            Level::Error => "✗",
        };
        println!("  {mark} {}", f.message);
    }
    if let (Some(flags), Some(profile)) = (&r.recommended_flags, &r.profile) {
        println!("  recommended llama-server flags for {profile}:\n    {flags}");
    }
    println!();
}
