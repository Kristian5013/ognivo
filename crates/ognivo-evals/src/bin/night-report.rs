//! The morning report of a night of `scripts/night.sh`:
//!
//!   night-report NIGHT_DIR [--previous NIGHT_DIR]
//!
//! prints it as Markdown.

use std::path::PathBuf;
use std::process::ExitCode;

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
    match ognivo_evals::night::report(&night, previous.as_deref()) {
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
