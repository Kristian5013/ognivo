//! `ognivo sessions`: the sessions in the store, the most recent first: those of this
//! project, of every project with `--all`, or those whose messages and answers hold some
//! words with `--search`. `ognivo chat --resume ID` goes on with one, and `--atif ID`
//! prints one's trajectory in ATIF.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use ognivo_store::SessionRow;

use crate::session::{self, Failure};
use crate::view::printable;

/// Characters of a title or a snippet on a line.
const TEXT_CHARS: usize = 72;

#[derive(Debug, Args)]
pub struct SessionsArgs {
    /// Words to find in the user's messages and the answers
    #[arg(long)]
    search: Option<String>,
    /// Sessions of every project, not only this one
    #[arg(long)]
    all: bool,
    /// Project root; the current directory by default
    #[arg(long)]
    cwd: Option<PathBuf>,
    /// Where the store is; ~/.ognivo/data by default
    #[arg(long)]
    data_dir: Option<PathBuf>,
    /// Sessions to show at most
    #[arg(long, default_value_t = 20)]
    limit: u32,
    /// Print this session's trajectory in ATIF (Harbor's trajectory.json) instead
    #[arg(long, value_name = "ID")]
    atif: Option<i64>,
}

pub fn sessions(a: SessionsArgs) -> anyhow::Result<ExitCode> {
    let out = match a.atif {
        Some(id) => trajectory(&a, id),
        None => list(&a),
    };
    match out {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(f) => {
            eprintln!("ognivo sessions: {}", f.message);
            Ok(ExitCode::from(f.code))
        }
    }
}

fn trajectory(a: &SessionsArgs, id: i64) -> Result<Vec<String>, Failure> {
    let store = session::open_store(&session::data_dir(a.data_dir.as_deref())?)?;
    let t = ognivo_core::trace::atif(&store, id).map_err(Failure::usage)?;
    Ok(vec![
        serde_json::to_string_pretty(&t).map_err(Failure::usage)?,
    ])
}

fn list(a: &SessionsArgs) -> Result<Vec<String>, Failure> {
    let store = session::open_store(&session::data_dir(a.data_dir.as_deref())?)?;
    let root = if a.all {
        None
    } else {
        Some(session::root(a.cwd.as_deref())?)
    };
    let home = ognivo_os::home();
    let mut lines = Vec::new();
    match &a.search {
        Some(words) => {
            for (id, snippet) in store.search(words, u32::MAX).map_err(Failure::usage)? {
                let Some(row) = store.session(id).map_err(Failure::usage)? else {
                    continue;
                };
                if root.as_ref().is_some_and(|r| *r != row.root) {
                    continue;
                }
                lines.push(line(&row, a.all, home.as_deref()));
                lines.push(format!("      {}", short(&snippet)));
                if lines.len() >= 2 * a.limit as usize {
                    break;
                }
            }
            if lines.is_empty() {
                lines.push("nothing found".to_owned());
            }
        }
        None => {
            let rows = store
                .sessions(root.as_deref(), a.limit)
                .map_err(Failure::usage)?;
            lines.extend(rows.iter().map(|r| line(r, a.all, home.as_deref())));
            if lines.is_empty() {
                lines.push(
                    if a.all {
                        "no sessions yet"
                    } else {
                        "no sessions in this project yet"
                    }
                    .to_owned(),
                );
            }
        }
    }
    Ok(lines)
}

/// `  12  2026-11-02 14:03  chat  gpt-oss-20b  3 turns  fix the bug · 2 files unreviewed`,
/// with the project before the title when sessions of every project are listed.
fn line(r: &SessionRow, with_root: bool, home: Option<&Path>) -> String {
    let mut line = format!(
        "{:>4}  {}  {:<4}  {}  {} turn{}",
        r.id,
        r.updated_local,
        r.kind,
        r.profile,
        r.turns,
        if r.turns == 1 { "" } else { "s" }
    );
    if with_root {
        let root = match home.and_then(|h| r.root.strip_prefix(h).ok()) {
            Some(rel) => format!("~/{}", ognivo_os::slashed(rel)),
            None => ognivo_os::slashed(&r.root),
        };
        line.push_str(&format!("  {}", printable(&root)));
    }
    line.push_str(&format!("  {}", short(&r.title)));
    if r.unreviewed_files > 0 {
        line.push_str(&format!(
            " · {} file{} unreviewed",
            r.unreviewed_files,
            if r.unreviewed_files == 1 { "" } else { "s" }
        ));
    }
    line
}

/// One line of at most [`TEXT_CHARS`] characters, control characters shown as signs.
fn short(text: &str) -> String {
    let flat: String = printable(text)
        .chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .collect();
    let mut out: String = flat.chars().take(TEXT_CHARS).collect();
    if flat.chars().count() > TEXT_CHARS {
        out.push('…');
    }
    out
}
