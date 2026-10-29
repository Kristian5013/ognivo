//! Shell command classification for the approval policy (docs/architecture.md §8).
//!
//! A conservative placeholder until the tree-sitter-bash classifier lands in M1: a
//! command is `ReadOnly` or `ExecutesProjectCode` only when every word is positively
//! recognised; everything else is `SideEffects` and asks. Recognised means:
//! - plain words only, no shell syntax (pipes, chains, redirects, substitutions, globs,
//!   quoting, assignments, `~` expansion);
//! - the command runs with its working directory inside the project root;
//! - each option is on the tool's allowlist, and each path argument resolves, symlinks
//!   included, to a location inside the project root;
//! - for `cat`/`head`/`tail`, each path is an existing regular file (no FIFOs, devices or
//!   links to `/dev/zero`).
//!
//! The classifier only decides whether to ask. The executor (M1) must also run commands
//! in a hermetic form: git with `-c core.fsmonitor=false --no-pager` (plus `--no-ext-diff
//! --no-textconv` for diff/log/show), rg with `--no-config` and no `RIPGREP_CONFIG_PATH`,
//! no `*_API_KEY`/`*_TOKEN` in the child environment, and a hard time and output cap for
//! `ReadOnly` commands. Checks here are not atomic with execution (the filesystem can
//! change in between); the agent's own Read/Grep/Glob tools use `openat2` confinement.

use std::path::{Component, Path, PathBuf};

use crate::Effect;

/// Where a command would run.
#[derive(Debug, Clone)]
pub struct ExecContext {
    /// Project root, already canonical.
    pub root: PathBuf,
    /// Current working directory of the agent's shell; persists between commands.
    pub cwd: PathBuf,
}

impl ExecContext {
    /// Resolves `arg` against the cwd; through symlinks when the path exists.
    fn resolve(&self, arg: &str) -> PathBuf {
        let joined = self.cwd.join(arg);
        std::fs::canonicalize(&joined).unwrap_or_else(|_| normalize(&joined))
    }

    fn inside(&self, path: &Path) -> bool {
        path.starts_with(&self.root)
    }
}

/// Lexical normalisation for paths that do not exist yet.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

const SHELL_SYNTAX: &[char] = &[
    ';', '&', '|', '<', '>', '$', '`', '(', ')', '{', '}', '*', '?', '[', ']', '\n', '\r', '\\',
    '"', '\'', '#', '=', '!',
];

/// Allowed options of one read-only or project-code tool.
struct Spec {
    /// Options that take no value.
    flags: &'static [&'static str],
    /// Options whose value is the next word and is not a path (counts, patterns, types).
    with_value: &'static [&'static str],
    /// Short options that may carry a number glued on (`-n5`, `-U3`, `-5`).
    numeric_prefixes: &'static [&'static str],
    /// Positional arguments must be existing regular files.
    files_only: bool,
}

const NONE: &[&str] = &[];

fn s(
    flags: &'static [&'static str],
    with_value: &'static [&'static str],
    numeric_prefixes: &'static [&'static str],
    files_only: bool,
) -> Spec {
    Spec {
        flags,
        with_value,
        numeric_prefixes,
        files_only,
    }
}

/// The allowlist for a command and how many leading words name it.
fn spec_for(words: &[&str]) -> Option<(Spec, usize)> {
    Some(match words {
        ["git", "status", ..] => (
            s(
                &[
                    "-s",
                    "--short",
                    "-b",
                    "--branch",
                    "--porcelain",
                    "-v",
                    "--ignored",
                    "-u",
                    "--no-renames",
                ],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["git", "diff", ..] => (
            s(
                &[
                    "--stat",
                    "--cached",
                    "--staged",
                    "--name-only",
                    "--name-status",
                    "--shortstat",
                    "--numstat",
                    "-w",
                    "--ignore-all-space",
                    "--color",
                    "--no-color",
                    "--word-diff",
                    "-M",
                    "--summary",
                    "--",
                ],
                NONE,
                &["-U"],
                false,
            ),
            2,
        ),
        ["git", "log", ..] => (
            s(
                &[
                    "--oneline",
                    "--stat",
                    "--graph",
                    "--decorate",
                    "-p",
                    "--name-only",
                    "--name-status",
                    "--reverse",
                    "--first-parent",
                    "--",
                ],
                &["-n", "--max-count"],
                &["-n", "-"],
                false,
            ),
            2,
        ),
        ["git", "show", ..] => (
            s(
                &[
                    "--stat",
                    "--name-only",
                    "--name-status",
                    "-p",
                    "--oneline",
                    "--",
                ],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["git", "branch", ..] => (
            s(
                &["-a", "-r", "-v", "-vv", "--list", "--show-current"],
                NONE,
                NONE,
                false,
            ),
            2,
        ),
        ["ls", ..] => (
            s(
                &[
                    "-l", "-a", "-la", "-al", "-lh", "-lah", "-h", "-1", "-R", "-t", "-S", "-r",
                    "-d",
                ],
                NONE,
                NONE,
                false,
            ),
            1,
        ),
        ["pwd"] => (s(NONE, NONE, NONE, false), 1),
        ["cat", ..] => (s(&["-n"], NONE, NONE, true), 1),
        ["head" | "tail", ..] => (s(NONE, &["-n", "-c"], &["-n", "-c", "-"], true), 1),
        ["wc", ..] => (s(&["-l", "-w", "-c", "-m"], NONE, NONE, true), 1),
        ["rg", ..] => (
            s(
                &[
                    "-n",
                    "-i",
                    "-S",
                    "-s",
                    "-w",
                    "-l",
                    "-c",
                    "-F",
                    "-x",
                    "-v",
                    "--files",
                    "--hidden",
                    "--no-ignore",
                    "-u",
                    "-uu",
                    "--count",
                    "--line-number",
                    "--ignore-case",
                    "--smart-case",
                    "--word-regexp",
                    "--fixed-strings",
                    "--files-with-matches",
                    "--no-heading",
                    "--heading",
                    "--",
                ],
                &[
                    "-e",
                    "-t",
                    "-T",
                    "--type",
                    "-m",
                    "--max-count",
                    "-A",
                    "-B",
                    "-C",
                ],
                &["-A", "-B", "-C", "-m"],
                false,
            ),
            1,
        ),
        // -r does not follow symlinks; -R does and is not listed
        ["grep", ..] => (
            s(
                &[
                    "-n", "-i", "-r", "-l", "-c", "-w", "-v", "-E", "-F", "-x", "-H", "-h", "-o",
                    "-s", "-q", "--",
                ],
                &["-e", "-m", "-A", "-B", "-C"],
                &["-A", "-B", "-C", "-m"],
                false,
            ),
            1,
        ),
        ["find", ..] => (
            s(
                &["-print", "-empty", "-not", "-o", "-a"],
                &[
                    "-name",
                    "-iname",
                    "-type",
                    "-maxdepth",
                    "-mindepth",
                    "-path",
                    "-ipath",
                    "-size",
                    "-mtime",
                ],
                NONE,
                false,
            ),
            1,
        ),
        [
            "cargo",
            "test" | "build" | "check" | "run" | "clippy" | "bench",
            ..,
        ] => (
            s(
                &[
                    "--release",
                    "--all-targets",
                    "--workspace",
                    "--lib",
                    "--bins",
                    "--tests",
                    "--no-run",
                    "--quiet",
                    "-q",
                    "--offline",
                    "--locked",
                    "--",
                ],
                &[
                    "-p",
                    "--package",
                    "--bin",
                    "--test",
                    "--example",
                    "--bench",
                    "--features",
                    "-F",
                    "-j",
                    "--jobs",
                    "--profile",
                ],
                NONE,
                false,
            ),
            2,
        ),
        ["npm" | "pnpm" | "yarn", "test" | "run" | "start", ..] => {
            (s(&["--silent", "--"], NONE, NONE, false), 2)
        }
        ["pytest", ..] => (
            s(
                &["-q", "-x", "-v", "-vv", "-s", "--lf", "--ff", "--no-header"],
                &["-k", "-m", "--maxfail"],
                NONE,
                false,
            ),
            1,
        ),
        ["make" | "just", ..] => (s(&["-n", "-s", "-k"], &["-j"], &["-j"], false), 1),
        ["ctest", ..] => (
            s(
                &["--output-on-failure", "-V", "-N"],
                &["-R", "-E", "-j"],
                &["-j"],
                false,
            ),
            1,
        ),
        ["go", "test" | "build" | "vet", ..] => (
            s(
                &["-v", "-race", "-short"],
                &["-run", "-count", "-timeout"],
                NONE,
                false,
            ),
            2,
        ),
        _ => return None,
    })
}

fn effect_of(words: &[&str]) -> Effect {
    match words {
        [
            "cargo" | "npm" | "pnpm" | "yarn" | "pytest" | "make" | "just" | "ctest" | "go",
            ..,
        ] => Effect::ExecutesProjectCode,
        _ => Effect::ReadOnly,
    }
}

/// Classifies `command` as it would run in `ctx`.
pub fn classify_command(command: &str, ctx: &ExecContext) -> Effect {
    if command.contains(SHELL_SYNTAX) {
        return Effect::SideEffects;
    }
    let words: Vec<&str> = command.split_whitespace().collect();
    if words.iter().any(|w| w.starts_with('~')) || !ctx.inside(&ctx.resolve(".")) {
        return Effect::SideEffects;
    }
    let Some((spec, skip)) = spec_for(&words) else {
        return Effect::SideEffects;
    };
    let mut args = words[skip..].iter();
    let mut after_dashdash = false;
    while let Some(&arg) = args.next() {
        if !after_dashdash && arg.starts_with('-') && arg.len() > 1 {
            if arg == "--" {
                after_dashdash = true;
            } else if spec.with_value.contains(&arg) {
                if args.next().is_none() {
                    return Effect::SideEffects;
                }
            } else if !spec.flags.contains(&arg) && !numeric_flag(arg, spec.numeric_prefixes) {
                return Effect::SideEffects;
            }
            continue;
        }
        // positional: a path, a ref, a package or a pattern; anything resolving outside is refused
        if arg.contains('@') {
            return Effect::SideEffects; // `go run mod@latest`, scp-like remotes
        }
        let path = ctx.resolve(arg);
        if !ctx.inside(&path) {
            return Effect::SideEffects;
        }
        if spec.files_only && !std::fs::metadata(&path).is_ok_and(|m| m.is_file()) {
            return Effect::SideEffects;
        }
    }
    effect_of(&words)
}

/// `-n5`, `-U3`, `-20`: an allowed short option with only digits glued on.
fn numeric_flag(arg: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|p| {
        arg.strip_prefix(p)
            .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        dir: std::path::PathBuf,
        ctx: ExecContext,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// A throwaway project: src/lib.rs, a symlink to /etc and a FIFO-like non-file.
    fn project(name: &str) -> Fixture {
        let base =
            std::env::temp_dir().join(format!("ognivo-cmd-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        std::fs::write(base.join("src/lib.rs"), "fn main() {}\n").unwrap();
        std::os::unix::fs::symlink("/etc", base.join("etc-link")).unwrap();
        std::os::unix::fs::symlink("/dev/zero", base.join("zero")).unwrap();
        let root = std::fs::canonicalize(&base).unwrap();
        Fixture {
            dir: base,
            ctx: ExecContext {
                cwd: root.clone(),
                root,
            },
        }
    }

    #[test]
    fn reads_inside_the_project() {
        let f = project("reads");
        let c = |cmd| classify_command(cmd, &f.ctx);
        assert_eq!(c("git status"), Effect::ReadOnly);
        assert_eq!(c("git diff --stat"), Effect::ReadOnly);
        assert_eq!(c("git log --oneline -n 5"), Effect::ReadOnly);
        assert_eq!(c("git diff HEAD~1 -- src"), Effect::ReadOnly);
        assert_eq!(c("cat src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("head -n 20 src/lib.rs"), Effect::ReadOnly);
        assert_eq!(c("rg -n TODO src"), Effect::ReadOnly);
        assert_eq!(c("git branch"), Effect::ReadOnly);
        assert_eq!(c("cargo test -p ognivo-core"), Effect::ExecutesProjectCode);
    }

    #[test]
    fn refuses_what_it_cannot_vouch_for() {
        let f = project("refuse");
        let c = |cmd| classify_command(cmd, &f.ctx);
        for cmd in [
            "git status && rm -rf target",
            "cat secrets > /tmp/x",
            "echo $(id)",
            "rm -rf build",
            "FOO=1 cargo test",
            "cat /etc/shadow",
            "cat ../other/secret",
            "cat etc-link/passwd", // symlink out of the project
            "cat zero",            // not a regular file
            "cat ~/.ssh/id_ed25519",
            "tail -f build.log",
            "git branch -D main",
            "git diff --output patch.txt",
            "git diff --ext-diff",
            "git diff --textconv",
            "git log --show-signature",
            "git diff --no-index a b",
            "rg --pre sh pattern",
            "rg -L secret",
            "rg --hostname-bin x y",
            "rg -z x",
            "grep -R key .",
            "grep -f/abs/patterns src",
            "find . -delete",
            "find . -fprint out.txt",
            "find -L . -name x",
            "wc --files0-from list",
            "go run example.com/tool@latest",
            "cargo test --manifest-path /tmp/x/Cargo.toml",
            "cargo clippy --fix",
            "make -C /",
            "npm run --prefix ../other x",
            "pytest --rootdir ..",
            "pytest /tmp/elsewhere",
        ] {
            assert_eq!(c(cmd), Effect::SideEffects, "{cmd}");
        }
    }

    #[test]
    fn cwd_outside_the_project_is_never_silent() {
        let f = project("cwd");
        let outside = ExecContext {
            root: f.ctx.root.clone(),
            cwd: f.ctx.root.parent().unwrap().to_path_buf(),
        };
        assert_eq!(
            classify_command("git status", &outside),
            Effect::SideEffects
        );
        assert_eq!(
            classify_command("cargo test", &outside),
            Effect::SideEffects
        );
    }
}
