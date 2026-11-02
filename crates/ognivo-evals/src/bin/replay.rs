//! T0's parse corpus against its snapshot:
//!
//!   replay RUNS              print how many completions got each verdict
//!   replay RUNS --check FILE exit 1 if the verdicts differ from FILE
//!   replay RUNS --write FILE keep the verdicts in FILE
//!
//! RUNS is the M0 harness's `runs` directory (`~/.local/share/ognivo/m0-baseline/runs`).

use std::path::PathBuf;
use std::process::ExitCode;

use ognivo_evals::replay::{self, Verdicts};
use ognivo_models::profile;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (runs, action) = match args.as_slice() {
        [runs] => (PathBuf::from(runs), None),
        [runs, flag, file] if flag == "--check" || flag == "--write" => (
            PathBuf::from(runs),
            Some((flag.as_str(), PathBuf::from(file))),
        ),
        _ => {
            eprintln!("usage: replay RUNS [--check FILE | --write FILE]");
            return ExitCode::from(2);
        }
    };
    let profiles = match profile::builtin() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("replay: {e}");
            return ExitCode::from(2);
        }
    };
    let now = match replay::replay(&runs, &profiles) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("replay: {}: {e}", runs.display());
            return ExitCode::from(2);
        }
    };
    let total: usize = now.values().map(Vec::len).sum();
    println!("{} recordings, {total} completions", now.len());
    for (class, n) in replay::tally(&now) {
        println!("  {n:>5}  {class}");
    }
    match action {
        None => ExitCode::SUCCESS,
        Some(("--write", file)) => {
            let text = serde_json::to_string_pretty(&now).unwrap_or_default() + "\n";
            match std::fs::write(&file, text) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("replay: {}: {e}", file.display());
                    ExitCode::from(2)
                }
            }
        }
        Some((_, file)) => {
            let kept: Verdicts = match std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
            {
                Ok(k) => k,
                Err(e) => {
                    eprintln!("replay: {}: {e}", file.display());
                    return ExitCode::from(2);
                }
            };
            let diff = replay::differences(&kept, &now);
            if diff.is_empty() {
                println!("as kept in {}", file.display());
                ExitCode::SUCCESS
            } else {
                for line in &diff {
                    println!("{line}");
                }
                println!("{} differences from {}", diff.len(), file.display());
                ExitCode::FAILURE
            }
        }
    }
}
