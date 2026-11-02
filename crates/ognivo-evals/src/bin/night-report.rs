//! The morning report of a night of `scripts/night.sh`:
//!
//!   night-report NIGHT_DIR [--previous NIGHT_DIR]
//!
//! prints it as Markdown. Which scenarios are ambiguous comes from the scenarios of this
//! checkout.

use std::path::PathBuf;
use std::process::ExitCode;

use ognivo_evals::t2;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (night, previous) = match args.as_slice() {
        [night] => (PathBuf::from(night), None),
        [night, flag, prev] if flag == "--previous" => {
            (PathBuf::from(night), Some(PathBuf::from(prev)))
        }
        _ => {
            eprintln!("usage: night-report NIGHT_DIR [--previous NIGHT_DIR]");
            return ExitCode::from(2);
        }
    };
    let ambiguous = match t2::load(&t2::scenarios_dir()) {
        Ok(scenarios) => t2::ambiguous(&scenarios),
        Err(e) => {
            eprintln!("night-report: {e}");
            return ExitCode::from(2);
        }
    };
    match ognivo_evals::night::report(&night, previous.as_deref(), &ambiguous) {
        Ok(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("night-report: {e}");
            ExitCode::from(2)
        }
    }
}
