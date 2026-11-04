//! What the agent actually checked (docs/m0/baseline.md, requirement 1).
//!
//! The final answer is the model's text; whether the result was verified is a fact the
//! loop records from tool calls: which files changed, which build or test commands ran on
//! which state and how they ended, which were refused. "Passed" means a build or test
//! command exited 0 after the last change. The status goes to the user next to the answer,
//! whatever the answer claims (M1 live run: gpt-oss reported a passing test whose run had
//! been refused).

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Run {
    pub command: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
}

impl Run {
    fn passed(&self) -> bool {
        self.exit_code == Some(0) && !self.timed_out
    }

    /// `exited 101`, `timed out` or `was killed`.
    pub fn outcome(&self) -> String {
        if self.timed_out {
            "timed out".to_owned()
        } else {
            match self.exit_code {
                Some(c) => format!("exited {c}"),
                None => "was killed".to_owned(),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Verification {
    /// Nothing was changed in this turn.
    NoChanges,
    /// A build or test command passed after the last change.
    Passed { command: String },
    /// Something changed after the last passing build or test, or nothing passed at all.
    Unverified {
        /// Changed files, relative to the project root when inside it.
        changed: Vec<String>,
        /// Commands with side effects that may have changed more than `changed` lists.
        side_effects: Vec<String>,
        /// The last build or test run after the last change, if any (it did not pass).
        last_run: Option<Run>,
        /// Build or test commands refused after the last change.
        refused: Vec<String>,
    },
}

#[derive(Debug, Clone)]
enum Entry {
    Changed(PathBuf),
    SideEffect(String),
    Ran(Run),
    Refused(String),
}

/// The ledger of one turn, in the order things happened.
#[derive(Debug, Clone, Default)]
pub struct Ledger {
    entries: Vec<Entry>,
}

impl Ledger {
    pub fn changed(&mut self, path: PathBuf) {
        self.entries.push(Entry::Changed(path));
    }

    pub fn side_effect(&mut self, command: &str) {
        self.entries.push(Entry::SideEffect(command.to_owned()));
    }

    pub fn ran(&mut self, run: Run) {
        self.entries.push(Entry::Ran(run));
    }

    pub fn refused(&mut self, command: &str) {
        self.entries.push(Entry::Refused(command.to_owned()));
    }

    /// What happened in another ledger since, a subagent's turn.
    pub fn extend(&mut self, other: Ledger) {
        self.entries.extend(other.entries);
    }

    /// Only its builds and tests, run or refused: what a read task did, which was to change
    /// nothing. Its commands with side effects stay in the edit journal, for review; the
    /// answer is not held back for a build after them (2026-11-03: an analysis in four read
    /// tasks sent the user's agent to look for tests after `python - <<'PY'`).
    pub fn runs_only(self) -> Ledger {
        Ledger {
            entries: self
                .entries
                .into_iter()
                .filter(|e| matches!(e, Entry::Ran(_) | Entry::Refused(_)))
                .collect(),
        }
    }

    /// The builds and tests of the turn, in order.
    pub fn runs(&self) -> impl Iterator<Item = &Run> {
        self.entries.iter().filter_map(|e| match e {
            Entry::Ran(r) => Some(r),
            _ => None,
        })
    }

    pub fn status(&self, root: &Path) -> Verification {
        let changes = |e: &Entry| matches!(e, Entry::Changed(_) | Entry::SideEffect(_));
        let Some(last_change) = self.entries.iter().rposition(changes) else {
            return Verification::NoChanges;
        };
        let after = &self.entries[last_change + 1..];
        if let Some(Entry::Ran(r)) = after
            .iter()
            .rev()
            .find(|e| matches!(e, Entry::Ran(r) if r.passed()))
        {
            return Verification::Passed {
                command: r.command.clone(),
            };
        }
        let mut changed = Vec::new();
        let mut side_effects = Vec::new();
        for e in &self.entries {
            match e {
                Entry::Changed(p) => {
                    let shown = p.strip_prefix(root).unwrap_or(p).display().to_string();
                    if !changed.contains(&shown) {
                        changed.push(shown);
                    }
                }
                Entry::SideEffect(c) => side_effects.push(c.clone()),
                Entry::Ran(_) | Entry::Refused(_) => {}
            }
        }
        Verification::Unverified {
            changed,
            side_effects,
            last_run: after.iter().rev().find_map(|e| match e {
                Entry::Ran(r) => Some(r.clone()),
                _ => None,
            }),
            refused: after
                .iter()
                .filter_map(|e| match e {
                    Entry::Refused(c) => Some(c.clone()),
                    _ => None,
                })
                .collect(),
        }
    }
}

/// Commands that build or test the project. The classifier decides whether a command may
/// run; this only decides whether its outcome says anything about the result. Compound
/// commands count too: `cargo test 2>&1 | tail -n 20` under `pipefail` is a test run.
pub fn is_build_or_test(command: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?x)(^|[\s;&|(])(
                cargo\s+(\+\S+\s+)?(build|b|test|t|check|c|clippy|nextest|run|r|bench)\b
              | ctest\b | cmake\s+--build\b | make\b | ninja\b | meson\s+(test|compile)\b
              | pytest\b | python3?\s+-m\s+(pytest|unittest)\b | tox\b
              | go\s+(test|build|vet)\b
              | (npm|pnpm|yarn|bun)\s+(run\s+)?(test|build|check|lint)\b | npx\s+tsc\b | tsc\b
              | mvn\b | gradle\b | \./gradlew\b
            )",
        )
        .expect("valid regex")
    });
    RE.is_match(command)
}

/// Files at a project's root that say how it builds or tests.
const BUILD_FILES: &[&str] = &[
    "Cargo.toml",
    "package.json",
    "deno.json",
    "deno.jsonc",
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "tox.ini",
    "pytest.ini",
    "noxfile.py",
    "go.mod",
    "Makefile",
    "makefile",
    "GNUmakefile",
    "CMakeLists.txt",
    "meson.build",
    "build.gradle",
    "build.gradle.kts",
    "pom.xml",
    "build.xml",
    "build.sbt",
    "composer.json",
    "Gemfile",
    "Rakefile",
    "mix.exs",
    "build.zig",
    "dune-project",
    "stack.yaml",
    "Package.swift",
    "justfile",
    "Justfile",
    "Taskfile.yml",
    "MODULE.bazel",
    "WORKSPACE",
];

/// Extensions of build files whose names vary: `app.csproj`, `pkg.cabal`.
const BUILD_EXTENSIONS: &[&str] = &["sln", "csproj", "fsproj", "vcxproj", "cabal", "nimble"];

/// Directories at a project's root that hold its tests.
const TEST_DIRS: &[&str] = &["tests", "test", "spec", "__tests__"];

/// What a project has to check a change with, as paths from its root, sorted: its build
/// files, its test directories, and the scripts named build*, test* or check* at its root or
/// in `scripts/`. Empty for a project with nothing to build or run: the model is told so,
/// and is not sent back to run a build (T2, 2026-11-03: in projects of a few files Gemma
/// wrote programs of its own to test an edit until the step limit came).
pub fn project_checks(root: &Path) -> Vec<String> {
    // a script, not `test.rs` or `checklist.md`
    let script = |name: &str| {
        let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
        let stem = stem.to_ascii_lowercase();
        ["build", "test", "check"]
            .iter()
            .any(|p| stem.starts_with(p))
            && [
                "", "sh", "bash", "py", "js", "mjs", "ts", "rb", "pl", "ps1", "bat", "cmd",
            ]
            .contains(&ext)
    };
    let mut found = Vec::new();
    for (dir, prefix) in [(root.to_owned(), ""), (root.join("scripts"), "scripts/")] {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            let Some(name) = e.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let hit = if prefix.is_empty() && is_dir {
                TEST_DIRS.contains(&name.as_str())
            } else if is_dir {
                false
            } else {
                (prefix.is_empty()
                    && (BUILD_FILES.contains(&name.as_str())
                        || Path::new(&name)
                            .extension()
                            .and_then(|x| x.to_str())
                            .is_some_and(|x| BUILD_EXTENSIONS.contains(&x))))
                    || script(&name)
            };
            if hit {
                found.push(format!("{prefix}{name}{}", if is_dir { "/" } else { "" }));
            }
        }
    }
    found.sort();
    found
}

/// The answer says a build or test ran or passed.
pub fn claims_a_passing_run(answer: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)
              \b(tests?|build|builds|compil\w*|checks?|suite)\b[^.\n]{0,60}\b(pass(es|ed)?|succeed\w*|succeeded|green)\b
            | \b(pass(es|ed)|passing)\b[^.\n]{0,40}\b(tests?|build|checks?)\b
            | \b(I\s+)?(ran|have\s+run|executed)\b[^.\n]{0,60}\b(tests?|build|cargo|ctest|pytest|make)\b
            | \.\.\.\s+ok\b | test\s+result:\s+ok
            ",
        )
        .expect("valid regex")
    });
    RE.is_match(answer)
}

/// The answer says, in the first person, that the agent changed something.
pub fn claims_changes(answer: &str) -> bool {
    static RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)\b(I|I've|I\s+have|we|we've)\s+(added|changed|fixed|updated|modified|created|edited|implemented|replaced|removed|renamed|wrote|rewrote)\b
              | \b(was|were|has\s+been|have\s+been)\s+(added|changed|fixed|updated|modified|created|edited|implemented|replaced|removed|renamed)\b",
        )
        .expect("valid regex")
    });
    RE.is_match(answer)
}

/// Items a summary lists; the rest are counted.
const LISTED: usize = 6;
/// Characters of a command a summary shows.
const COMMAND_CHARS: usize = 80;

/// One line for the user under the answer, also part of ognivo's notes to the model;
/// `None` when nothing changed. Commands are shortened: in M1 the side effects of a turn
/// included a heredoc that made the note 10 KB.
pub fn summary(v: &Verification) -> Option<String> {
    match v {
        Verification::NoChanges => None,
        Verification::Passed { command } => Some(format!(
            "verified: {} passed after the last change",
            quoted(command)
        )),
        Verification::Unverified {
            changed,
            side_effects,
            last_run,
            refused,
        } => {
            let mut what: Vec<String> = changed.clone();
            what.extend(side_effects.iter().map(|c| quoted(c)));
            let mut s = format!(
                "NOT verified: {} changed after the last passing build or test",
                list(&what)
            );
            if let Some(r) = last_run {
                s.push_str(&format!("; {} {}", quoted(&r.command), r.outcome()));
            }
            if !refused.is_empty() {
                let refused: Vec<String> = refused.iter().map(|c| quoted(c)).collect();
                s.push_str(&format!("; refused: {}", list(&refused)));
            }
            if last_run.is_none() && refused.is_empty() {
                s.push_str("; no build or test ran after it");
            }
            Some(s)
        }
    }
}

/// `items` joined, those after the first [`LISTED`] counted.
fn list(items: &[String]) -> String {
    let mut s = items[..items.len().min(LISTED)].join(", ");
    if items.len() > LISTED {
        s.push_str(&format!(" and {} more", items.len() - LISTED));
    }
    s
}

/// A command in backquotes on one line: its first line, cut to [`COMMAND_CHARS`].
pub fn quoted(command: &str) -> String {
    let command = command.trim();
    let line: String = command
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(COMMAND_CHARS)
        .collect();
    let cut = if line.len() < command.len() {
        "…"
    } else {
        ""
    };
    format!("`{line}{cut}`")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &str, code: i32) -> Run {
        Run {
            command: cmd.into(),
            exit_code: Some(code),
            timed_out: false,
        }
    }

    #[test]
    fn passing_only_counts_after_the_last_change() {
        let root = Path::new("/p");
        let mut l = Ledger::default();
        assert_eq!(l.status(root), Verification::NoChanges);
        l.ran(run("cargo test", 0));
        assert_eq!(l.status(root), Verification::NoChanges);
        l.changed("/p/src/a.rs".into());
        assert!(matches!(
            l.status(root),
            Verification::Unverified { last_run: None, .. }
        ));
        l.ran(run("cargo test", 101));
        match l.status(root) {
            Verification::Unverified {
                changed,
                last_run: Some(r),
                ..
            } => {
                assert_eq!(changed, ["src/a.rs"]);
                assert_eq!(r.exit_code, Some(101));
            }
            other => panic!("{other:?}"),
        }
        l.ran(run("cargo test", 0));
        assert_eq!(
            l.status(root),
            Verification::Passed {
                command: "cargo test".into()
            }
        );
        // a later change voids the earlier pass
        l.side_effect("sed -i s/a/b/ x");
        l.refused("cargo test");
        let s = summary(&l.status(root)).unwrap();
        assert!(
            s.starts_with("NOT verified: src/a.rs, `sed -i s/a/b/ x` changed"),
            "{s}"
        );
        assert!(s.ends_with("refused: `cargo test`"), "{s}");
    }

    #[test]
    fn a_read_task_brings_its_builds_and_tests_and_nothing_it_changed() {
        let root = Path::new("/p");
        let mut main = Ledger::default();
        main.changed("/p/src/a.rs".into());
        let mut read = Ledger::default();
        read.side_effect("python -m compileall .");
        read.ran(run("cargo test", 0));
        read.refused("make");
        main.extend(read.clone().runs_only());
        // its passing test counts for the change before it
        assert_eq!(
            main.status(root),
            Verification::Passed {
                command: "cargo test".into()
            }
        );
        let mut analysis = Ledger::default();
        let mut read = Ledger::default();
        read.side_effect("python - <<'PY'");
        analysis.extend(read.runs_only());
        assert_eq!(analysis.status(root), Verification::NoChanges);
    }

    #[test]
    fn a_summary_stays_one_short_line() {
        // M1, Qwen on misc-ai-chat-multiturn: a heredoc among the side effects made the
        // note to the model 10 KB long
        let root = Path::new("/p");
        let mut l = Ledger::default();
        l.changed("/p/model/byte_tokenizer.py".into());
        let heredoc = format!(
            "cd /tmp && cat > test_bt.py <<'EOF'\n{}EOF\npython3 test_bt.py",
            "assert encode(x) == y\n".repeat(400)
        );
        l.side_effect(&heredoc);
        for i in 0..7 {
            l.side_effect(&format!("touch f{i}"));
        }
        l.ran(run(&format!("pytest {}", "tests/a.py ".repeat(20)), 1));
        let s = summary(&l.status(root)).unwrap();
        assert!(!s.contains('\n') && s.len() < 400, "{} bytes: {s}", s.len());
        assert!(
            s.starts_with(
                "NOT verified: model/byte_tokenizer.py, `cd /tmp && cat > test_bt.py <<'EOF'…`, `touch f0`"
            ),
            "{s}"
        );
        assert!(
            s.contains("`touch f3` and 3 more changed after the last passing build or test; `pytest tests/a.py"),
            "{s}"
        );
        assert!(s.ends_with("…` exited 1"), "{s}");
    }

    #[test]
    fn build_and_test_commands_are_recognised() {
        for c in [
            "cargo test -p x",
            "cd crates && cargo build 2>&1 | tail -5",
            "ctest --preset dev",
            "cmake --build build -j",
            "make -j8",
            "python -m pytest -q",
            "npm test",
            "go test ./...",
        ] {
            assert!(is_build_or_test(c), "{c}");
        }
        for c in ["cat Makefile", "git diff", "grep -r test src", "ls target"] {
            assert!(!is_build_or_test(c), "{c}");
        }
    }

    #[test]
    fn claims_are_noticed() {
        // the M1 live-run answer of gpt-oss
        assert!(claims_a_passing_run(
            "I have run the test with the following command: cargo test -p ognivo-tools"
        ));
        assert!(claims_a_passing_run(
            "The output shows the test launching and passing:\ntest tests::x ... ok"
        ));
        assert!(claims_a_passing_run("All tests pass."));
        assert!(!claims_a_passing_run(
            "I could not run the tests: the command was refused."
        ));
        assert!(claims_changes("I added a unit test to files.rs."));
        assert!(claims_changes(
            "The function was updated to return a Result."
        ));
        assert!(!claims_changes(
            "The tolerance cascade is in crates/ognivo-tools/src/edit.rs."
        ));
    }

    #[test]
    fn a_project_shows_what_checks_it_and_a_few_files_show_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/math.rs"), "").unwrap();
        // a program written to test an edit is not the project's test
        std::fs::write(root.join("test.rs"), "").unwrap();
        std::fs::write(root.join("checklist.md"), "").unwrap();
        assert!(project_checks(root).is_empty());
        std::fs::write(root.join("Cargo.toml"), "").unwrap();
        std::fs::write(root.join("app.csproj"), "").unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/check.sh"), "").unwrap();
        std::fs::write(root.join("scripts/deploy.sh"), "").unwrap();
        std::fs::write(root.join("build"), "").unwrap();
        assert_eq!(
            project_checks(root),
            [
                "Cargo.toml",
                "app.csproj",
                "build",
                "scripts/check.sh",
                "tests/"
            ]
        );
    }
}
