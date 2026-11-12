//! Running shell commands for the model (docs/architecture.md §9, docs/m0/baseline.md
//! requirement 10).
//!
//! - No TTY: stdin is `/dev/null`, stdout and stderr are one stream in order.
//! - `bash -o pipefail`: the exit code is the pipeline's, so `cargo test | tail` fails
//!   when the tests fail (M0: 49 of Qwen's 59 build/test calls went through tail/head).
//! - Its own process tree, killed as a whole ([`ognivo_os::Tree`]): on timeout SIGINT,
//!   then SIGTERM, then SIGKILL (on Windows the job's end at once), and the result says so
//!   with exit code 124; dropping the future kills it too.
//! - On Windows the bash is Git for Windows' ([`ognivo_os::shell::bash`]).
//! - The environment loses credentials (`*_API_KEY`, `*_TOKEN`) and the variables that
//!   make bash source files (`BASH_ENV`, `ENV`), and gains pager-free settings.
//! - The model sees the head and tail of the output within a byte budget; the full log
//!   goes to a file whose path the result names.
//! - `cd` persists: the shell writes its final directory to a side file.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use ognivo_os::shell::{self, NoBash};
use ognivo_os::{Stop, Tree};
use tokio::io::AsyncReadExt;

/// Default and maximum time a command may run.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_TIMEOUT: Duration = Duration::from_secs(600);
/// Bytes of output the model sees: 30% from the head, 70% from the tail.
pub const DEFAULT_OUTPUT_BUDGET: usize = 24 * 1024;
/// The full log keeps at most this much.
const LOG_MAX_BYTES: usize = 64 * 1024 * 1024;
/// Grace between the signals of a kill.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// How long output is still read after the shell itself exited.
const DRAIN: Duration = Duration::from_secs(2);

#[derive(Debug, Clone)]
pub struct ShellOptions {
    pub cwd: PathBuf,
    pub timeout: Duration,
    pub output_budget: usize,
    /// Directory for full logs (under `~/.ognivo/data/logs`; never `/tmp`, which is RAM
    /// on the owner's machine).
    pub log_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellResult {
    /// `None` when a signal ended the shell.
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    /// What the model reads: a status line, then the (possibly cut) output.
    pub text: String,
    /// The directory the shell ended in; the next command starts there.
    pub cwd: PathBuf,
    pub log: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("cannot start bash: {0}")]
    Spawn(std::io::Error),
    #[error(transparent)]
    NoBash(#[from] NoBash),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

pub async fn run(command: &str, opts: &ShellOptions) -> Result<ShellResult, ExecError> {
    std::fs::create_dir_all(&opts.log_dir).map_err(|source| ExecError::Io {
        path: opts.log_dir.clone(),
        source,
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let log = opts.log_dir.join(format!("shell-{stamp}.log"));
    let cwd_file = opts.log_dir.join(format!("shell-{stamp}.cwd"));
    // exec 2>&1 keeps stdout and stderr in one ordered stream; the trailer records the
    // final directory without changing the command's exit code
    let script = format!(
        "exec 2>&1\n{command}\n__ognivo_rc=$?\n{} > \"$__OGNIVO_CWD_FILE\" 2>/dev/null\nexit $__ognivo_rc\n",
        shell::PWD
    );
    let mut cmd = ognivo_os::async_command(shell::bash()?);
    cmd.arg("--noprofile")
        .arg("--norc")
        .arg("-o")
        .arg("pipefail")
        .arg("-c")
        .arg(&script)
        .current_dir(&opts.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, _) in std::env::vars_os() {
        let k = key.to_string_lossy();
        if k.ends_with("_API_KEY") || k.ends_with("_TOKEN") || k == "BASH_ENV" || k == "ENV" {
            cmd.env_remove(&key);
        }
    }
    cmd.env("PAGER", "cat")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C.UTF-8")
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("AI_AGENT", "ognivo")
        .env("__OGNIVO_CWD_FILE", shell::bash_path(&cwd_file));
    let started = Instant::now();
    let mut tree = Tree::spawn(cmd).map_err(ExecError::Spawn)?;
    if let Some(id) = tree.id() {
        // a build that eats the RAM must not take llama-server with it
        ognivo_os::oom_first(id);
    }
    let mut stdout = tree.stdout().expect("stdout is piped");
    let mut out = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    let deadline = tokio::time::Instant::now() + opts.timeout;
    let mut status = None;
    let mut eof = false;
    let mut timed_out = false;
    let mut drain_until = None;
    // read until the shell exits and its output is drained; a background job that keeps
    // the pipe open gets DRAIN after the exit, not the whole timeout
    while !(eof && status.is_some()) {
        tokio::select! {
            r = stdout.read(&mut buf), if !eof => match r {
                Ok(0) | Err(_) => eof = true,
                Ok(n) => {
                    let room = LOG_MAX_BYTES.saturating_sub(out.len());
                    out.extend_from_slice(&buf[..n.min(room)]);
                }
            },
            s = tree.wait(), if status.is_none() => {
                status = Some(s.ok());
                drain_until = Some(tokio::time::Instant::now() + DRAIN);
            }
            _ = tokio::time::sleep_until(drain_until.map_or(deadline, |d: tokio::time::Instant| d.min(deadline))) => {
                timed_out = drain_until.is_none();
                break;
            }
        }
    }
    if timed_out {
        for how in [Stop::Interrupt, Stop::Terminate, Stop::Kill] {
            tree.stop(how);
            if let Ok(s) = tokio::time::timeout(KILL_GRACE, tree.wait()).await {
                status = Some(s.ok());
                break;
            }
        }
    }
    // whatever the command left running in its tree dies with it
    tree.stop(Stop::Kill);
    let status = status.flatten();
    let elapsed = started.elapsed();
    std::fs::write(&log, &out).map_err(|source| ExecError::Io {
        path: log.clone(),
        source,
    })?;
    let cwd = std::fs::read_to_string(&cwd_file)
        .ok()
        .map(|s| shell::native(s.trim_end_matches(['\n', '\r'])))
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| opts.cwd.clone());
    let _ = std::fs::remove_file(&cwd_file);
    let exit_code = if timed_out {
        Some(124)
    } else {
        status.and_then(|s| s.code())
    };
    let mut head = match (timed_out, exit_code) {
        (true, _) => format!(
            "[shell: timed out after {} s and was killed; exit 124]",
            opts.timeout.as_secs()
        ),
        (false, Some(c)) => format!("[shell: exit {c}, {:.1} s]", elapsed.as_secs_f64()),
        (false, None) => "[shell: killed by a signal]".to_owned(),
    };
    if cwd != opts.cwd {
        head.push_str(&format!("\n[cwd: {}]", ognivo_os::slashed(&cwd)));
    }
    let text = format!("{head}\n{}", budgeted(&out, opts.output_budget, &log));
    Ok(ShellResult {
        exit_code,
        timed_out,
        text,
        cwd,
        log,
    })
}

/// The output as the model sees it: everything when it fits, otherwise 30% of the budget
/// from the head and 70% from the tail, cut at line starts, with the log path between.
fn budgeted(out: &[u8], budget: usize, log: &Path) -> String {
    let text = String::from_utf8_lossy(out);
    if text.len() <= budget {
        return text.into_owned();
    }
    let head_end = floor_line(&text, budget * 3 / 10);
    let tail_start = ceil_line(&text, text.len() - budget * 7 / 10);
    let cut_lines = text[head_end..tail_start].lines().count();
    format!(
        "{}… [{cut_lines} lines, {} bytes cut; full log: {}] …\n{}",
        &text[..head_end],
        tail_start - head_end,
        ognivo_os::slashed(log),
        &text[tail_start..]
    )
}

/// The end of the last whole line before `at` (or a char boundary if there is none).
fn floor_line(s: &str, at: usize) -> usize {
    let at = floor_char(s, at);
    s[..at].rfind('\n').map_or(at, |i| i + 1)
}

/// The start of the first whole line at or after `at`.
fn ceil_line(s: &str, at: usize) -> usize {
    let at = floor_char(s, at);
    s[at..].find('\n').map_or(at, |i| at + i + 1)
}

fn floor_char(s: &str, mut at: usize) -> usize {
    while !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(dir: &Path, timeout: Duration, budget: usize) -> ShellOptions {
        ShellOptions {
            cwd: ognivo_os::canonicalize(dir).unwrap(),
            timeout,
            output_budget: budget,
            log_dir: dir.join("logs"),
        }
    }

    #[tokio::test]
    async fn pipeline_exit_code_is_honest() {
        let d = tempfile::tempdir().unwrap();
        let o = opts(d.path(), DEFAULT_TIMEOUT, DEFAULT_OUTPUT_BUDGET);
        let r = run("echo building; false | tail -n 1", &o).await.unwrap();
        assert_eq!(r.exit_code, Some(1), "{}", r.text);
        assert!(
            r.text.starts_with("[shell: exit 1,") && r.text.contains("building"),
            "{}",
            r.text
        );
        let r = run("echo err >&2; exit 3", &o).await.unwrap();
        assert_eq!(r.exit_code, Some(3));
        assert!(r.text.contains("err"), "stderr merged: {}", r.text);
    }

    #[tokio::test]
    async fn cd_persists_and_env_is_scrubbed() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        let o = opts(d.path(), DEFAULT_TIMEOUT, DEFAULT_OUTPUT_BUDGET);
        let r = run(
            "cd sub && echo $PAGER $GIT_TERMINAL_PROMPT ${OPENAI_API_KEY:-none}",
            &o,
        )
        .await
        .unwrap();
        assert_eq!(r.cwd, o.cwd.join("sub"));
        assert!(
            r.text.contains("[cwd: ") && r.text.contains("cat 0 none"),
            "{}",
            r.text
        );
    }

    #[tokio::test]
    async fn timeout_kills_the_whole_group() {
        let d = tempfile::tempdir().unwrap();
        let o = opts(d.path(), Duration::from_millis(500), DEFAULT_OUTPUT_BUDGET);
        let marker = d.path().join("survived");
        let cmd = format!(
            "(sleep 3; touch '{}') & sleep 30",
            shell::bash_path(&marker).to_string_lossy()
        );
        let r = run(&cmd, &o).await.unwrap();
        assert!(r.timed_out && r.exit_code == Some(124), "{}", r.text);
        assert!(r.text.contains("timed out after 0 s"), "{}", r.text);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "a background child outlived the kill");
    }

    #[tokio::test]
    async fn background_job_does_not_hold_the_result() {
        let d = tempfile::tempdir().unwrap();
        let o = opts(d.path(), Duration::from_secs(60), DEFAULT_OUTPUT_BUDGET);
        let t = Instant::now();
        let r = run(
            "(while true; do echo tick; sleep 0.2; done) & echo started",
            &o,
        )
        .await
        .unwrap();
        assert_eq!(r.exit_code, Some(0), "{}", r.text);
        assert!(r.text.contains("started"));
        assert!(t.elapsed() < Duration::from_secs(10), "{:?}", t.elapsed());
    }

    #[tokio::test]
    async fn long_output_keeps_head_and_tail_and_logs_all() {
        let d = tempfile::tempdir().unwrap();
        let o = opts(d.path(), DEFAULT_TIMEOUT, 1000);
        let r = run("seq 1 2000", &o).await.unwrap();
        assert!(
            r.text.contains("\n1\n2\n") && r.text.ends_with("1999\n2000\n"),
            "{}",
            r.text
        );
        assert!(r.text.contains("bytes cut; full log: "), "{}", r.text);
        assert!(r.text.len() < 1300, "{}", r.text.len());
        let full = std::fs::read_to_string(&r.log).unwrap();
        assert_eq!(full.lines().count(), 2000);
    }
}
