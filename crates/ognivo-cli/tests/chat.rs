//! `ognivo chat` end to end against the fake llama-server. Off a terminal the line editor
//! reads plain lines, so stdin carries both the messages and the answers to ognivo's
//! questions.

mod common;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{HANG, answer_stream, call_stream, chunk, fake_server, sse};
use serde_json::json;

fn spawn(url: &str, root: &Path, data: &Path, stdin: &str) -> Child {
    spawn_with(url, root, data, stdin, &["--seed", "3"])
}

fn spawn_with(url: &str, root: &Path, data: &Path, stdin: &str, args: &[&str]) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["chat", "--url", url, "--profile", "gpt-oss-20b"])
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .arg("--cwd")
        .arg(root)
        // the user's own instruction files are looked for there
        .env("HOME", root.parent().unwrap())
        .env_remove("CLAUDE_CONFIG_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child
}

fn project() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("proj");
    std::fs::create_dir(&root).unwrap();
    (dir, root)
}

#[tokio::test]
async fn asks_runs_and_remembers_always_for_the_session() {
    // each turn: the call, an answer sent back once to verify the side effect, the answer
    let turn = |id: &str, text: &str| {
        [
            call_stream(id, "shell", r#"{"command":"touch made.txt"}"#),
            answer_stream(text),
            answer_stream(text),
        ]
    };
    let mut responses = Vec::new();
    for (id, text) in [("c1", "Touched."), ("c2", "Again."), ("c3", "Third.")] {
        responses.extend(turn(id, text));
    }
    let (url, bodies) = fake_server(responses).await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    let stdin = "make a file\ny\n/clear\nonce more\na\nand again\n/exit\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(root.join("made.txt").exists());
    assert!(
        stdout.starts_with("ognivo chat · gpt-oss-20b · http://127.0.0.1:"),
        "{stdout}"
    );
    assert!(stdout.contains("n_ctx 4096"), "{stdout}");
    // asked twice, the second time answered "a"; the third run went without a question
    assert_eq!(
        stdout.matches("? shell touch made.txt").count(),
        2,
        "{stdout}"
    );
    assert_eq!(
        stdout
            .matches("· allowed for this session: touch made.txt")
            .count(),
        1,
        "{stdout}"
    );
    // the command's line; each run came after a question or a note, so its result has a
    // line of its own
    assert_eq!(
        stdout.matches("→ $ touch made.txt\n").count(),
        3,
        "{stdout}"
    );
    assert_eq!(stdout.matches("\n← exit 0, ").count(), 3, "{stdout}");
    for text in ["Touched.", "· new conversation", "Again.", "Third."] {
        assert!(stdout.contains(text), "{text}: {stdout}");
    }
    assert!(
        stdout.contains("[ognivo] NOT verified: `touch made.txt`"),
        "{stdout}"
    );
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 9);
    // after /clear the conversation starts over: system and the new message
    assert_eq!(
        bodies[3]["messages"][1],
        json!({"role": "user", "content": "once more"})
    );
    assert_eq!(bodies[3]["messages"].as_array().unwrap().len(), 2);
    let history = data.join("chat_history");
    let text = std::fs::read_to_string(&history).unwrap();
    assert!(
        text.contains("make a file") && text.contains("and again"),
        "{text}"
    );
    assert!(!text.contains("/exit"), "{text}");
    let mode = std::fs::metadata(&history).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[tokio::test]
async fn reads_show_as_one_line_edits_as_a_diff_failures_with_their_end() {
    let (url, _) = fake_server(vec![
        call_stream("c1", "read", r#"{"path":"notes.txt"}"#),
        call_stream(
            "c2",
            "edit",
            r#"{"path":"notes.txt","old_string":"two","new_string":"TWO"}"#,
        ),
        call_stream(
            "c3",
            "shell",
            r#"{"command":"touch x; echo first; echo boom; exit 3"}"#,
        ),
        answer_stream("Edited."),
        answer_stream("Edited."),
    ])
    .await;
    let (dir, root) = project();
    std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let data = dir.path().join("data");
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn(&url, &r, &d, "fix the notes\ny\n/exit\n").wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );
    assert!(
        stdout.contains("\n→ read notes.txt · lines 1-3 of 3 lines, 14 B, ~4 tokens\n"),
        "{stdout}"
    );
    assert!(
        stdout.contains("\n→ edit notes.txt · +1 -1\n  2 - two\n  2 + TWO\n"),
        "{stdout}"
    );
    // asked first, so the result has a line of its own, and the end of the output under it
    assert!(
        stdout.contains("\n← exit 3, ") && stdout.contains(" s\n    first\n    boom\n"),
        "{stdout}"
    );
}

#[tokio::test]
async fn changes_are_shown_reverted_and_accepted_between_turns() {
    let (url, bodies) = fake_server(vec![
        call_stream(
            "c1",
            "edit",
            r#"{"path":"notes.txt","old_string":"two","new_string":"TWO"}"#,
        ),
        call_stream("c2", "write", r#"{"path":"new.txt","content":"fresh\n"}"#),
        answer_stream("Done."),
        answer_stream("Done."),
        answer_stream("Noted."),
    ])
    .await;
    let (dir, root) = project();
    std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let data = dir.path().join("data");
    let stdin = "fix\n/changes\n/revert new.txt\nnext\n/accept\n/changes\n/exit\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    // not a git tree: what it had before the session waits for an accept too
    assert!(
        stdout.contains(" · not a clean git tree: builds and tests ask until /accept\n"),
        "{stdout}"
    );
    let expected = [
        "→ write new.txt · new, 1 line\n  1 + fresh\n",
        "· unreviewed: new.txt (new, 1 line), notes.txt (+1 -1) · /changes /accept /revert\n",
        // /changes
        "→ new.txt · new, 1 line\n  1 + fresh\n→ notes.txt · +1 -1\n  2 - two\n  2 + TWO\n",
        "· and what the project had before this session (git status shows it)\n",
        "· removed new.txt: ognivo had created it\n",
        "Noted.",
        "· accepted: notes.txt, what the project had before this session\n",
        "· nothing unreviewed: builds and tests run without asking\n· nothing unreviewed\n",
    ];
    let mut at = 0;
    for text in expected {
        let found = stdout[at..].find(text);
        assert!(found.is_some(), "{text:?} after {at}:\n{stdout}");
        at += found.unwrap() + text.len();
    }
    assert!(!root.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "one\nTWO\nthree\n"
    );
    // the model hears of the revert with the next message
    let bodies = bodies.lock().unwrap();
    let last = bodies.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(
        last["content"],
        json!(
            "next\n\n[ognivo] Since your last turn the user reverted your changes to new.txt (removed: you had created it): they are as they were before your edits. Read them again before you edit them."
        )
    );
}

#[tokio::test]
async fn a_revert_asks_when_something_else_changed_the_file_too() {
    let (url, _) = fake_server(vec![
        call_stream(
            "c1",
            "edit",
            r#"{"path":"notes.txt","old_string":"two","new_string":"TWO"}"#,
        ),
        call_stream("c2", "shell", r#"{"command":"cargo test"}"#),
        call_stream("c3", "shell", r#"{"command":"echo extra >> notes.txt"}"#),
        answer_stream("Done."),
        answer_stream("Done."),
    ])
    .await;
    let (dir, root) = project();
    std::fs::write(root.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
    let data = dir.path().join("data");
    // all of it: the file, and the command that stays done
    let stdin = "fix\nn\ny\n/revert\ny\n/exit\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    // the question before running project code says what is unreviewed
    assert!(
        stdout.contains(
            "? shell cargo test\n  it runs project code while there are unreviewed changes\n  unreviewed: notes.txt, changes from before this session\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "? notes.txt also changed by a command or outside ognivo since its edits; a revert loses that\n· reverted notes.txt\n"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains("· commands with side effects are not undone"),
        "{stdout}"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("notes.txt")).unwrap(),
        "one\ntwo\nthree\n"
    );
}

#[tokio::test]
async fn a_declined_command_tells_the_model_why() {
    let (url, bodies) = fake_server(vec![
        call_stream("c1", "shell", r#"{"command":"touch nope.txt"}"#),
        answer_stream("OK, not touching it."),
    ])
    .await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    // a block message, then the answer to the question; stdin ends after it
    let stdin = "\"\"\"\nfirst line\nsecond line\n\"\"\"\nn use echo instead\n";
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&url, &r, &d, stdin).wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert!(!root.join("nope.txt").exists());
    assert!(
        stdout.contains("✗ shell: The user declined `touch nope.txt` and says: use echo instead"),
        "{stdout}"
    );
    assert!(stdout.contains("OK, not touching it."), "{stdout}");
    let bodies = bodies.lock().unwrap();
    assert_eq!(
        bodies[0]["messages"][1],
        json!({"role": "user", "content": "first line\nsecond line"})
    );
    let last = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["role"], json!("tool"));
    assert_eq!(
        last["content"],
        json!("[refused] The user declined `touch nope.txt` and says: use echo instead")
    );
}

#[tokio::test]
async fn ctrl_c_stops_the_turn_and_the_chat_goes_on() {
    let (url, bodies) = fake_server(vec![HANG.to_owned(), answer_stream("Going on.")]).await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    let watched = bodies.clone();
    let out = tokio::task::spawn_blocking(move || {
        let child = spawn(&url, &root, &data, "hello\ngo on\n/exit\n");
        // the first request is at the server: the turn runs
        for _ in 0..200 {
            if !watched.lock().unwrap().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let pid = rustix::process::Pid::from_raw(child.id() as i32).unwrap();
        rustix::process::kill_process(pid, rustix::process::Signal::INT).unwrap();
        child.wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert!(stdout.contains("· stopped"), "{stdout}");
    assert!(stdout.contains("Going on."), "{stdout}");
    // the model learns that the first turn was stopped
    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 2);
    let m = &bodies[1]["messages"];
    assert_eq!(
        m[1],
        json!({"role": "user", "content": "hello\n\n[ognivo] The user stopped this turn."})
    );
    assert_eq!(m[2], json!({"role": "user", "content": "go on"}));
}

#[tokio::test]
async fn instruction_files_are_named_and_reach_the_model() {
    let (url, bodies) = fake_server(vec![
        call_stream("c1", "read", r#"{"path":"sub/x.rs"}"#),
        answer_stream("Read it."),
    ])
    .await;
    let (dir, root) = project();
    let home = dir.path();
    std::fs::create_dir(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/CLAUDE.md"), "Be brief.\n").unwrap();
    std::fs::write(root.join("AGENTS.md"), "Answer in one word.\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/AGENTS.md"), "Tests live in tests/.\n").unwrap();
    std::fs::write(root.join("sub/x.rs"), "x\n").unwrap();
    // a link out of the project is named, not read
    std::fs::write(home.join("outside.md"), "the secret").unwrap();
    std::os::unix::fs::symlink(home.join("outside.md"), root.join("CLAUDE.md")).unwrap();
    let data = home.join("data");
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn(&url, &r, &d, "look\n/clear\n/exit\n").wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    let banner = "\ninstructions ~/.claude/CLAUDE.md, AGENTS.md\n· not read: CLAUDE.md (a link to outside the project)\n";
    assert!(stdout.contains(&format!("{banner}/help")), "{stdout}");
    // a new conversation reads them again
    assert!(
        stdout.contains(&format!("· new conversation{banner}")),
        "{stdout}"
    );
    let at = |s: &str| stdout.find(s).unwrap_or_else(|| panic!("{s}: {stdout}"));
    assert!(at("→ read sub/x.rs") < at("\n· instructions: sub/AGENTS.md\n"));
    assert!(at("\n· instructions: sub/AGENTS.md\n") < at("Read it."));

    let bodies = bodies.lock().unwrap();
    let system = bodies[0]["messages"][0]["content"].as_str().unwrap();
    assert!(
        system.contains("<instructions file=\"~/.claude/CLAUDE.md\">\nBe brief.\n</instructions>\n\n<instructions file=\"AGENTS.md\">\nAnswer in one word.\n</instructions>"),
        "{system}"
    );
    assert!(!system.contains("the secret"), "{system}");
    let read = bodies[1]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "tool")
        .unwrap();
    assert!(
        read["content"].as_str().unwrap().ends_with(
            "<instructions file=\"sub/AGENTS.md\">\nTests live in tests/.\n</instructions>"
        ),
        "{read}"
    );
}

/// `ognivo sessions` with these arguments: its stdout, or its stderr when it failed.
fn sessions(data: &Path, root: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .arg("sessions")
        .args(args)
        .arg("--data-dir")
        .arg(data)
        .arg("--cwd")
        .arg(root)
        .env("HOME", root.parent().unwrap())
        .output()
        .unwrap();
    let text = if out.status.success() {
        out.stdout
    } else {
        out.stderr
    };
    (
        out.status.success(),
        String::from_utf8_lossy(&text).into_owned(),
    )
}

#[tokio::test]
async fn a_session_is_kept_and_goes_on_with_its_changes() {
    let (url, _) = fake_server(vec![
        call_stream(
            "c1",
            "edit",
            r#"{"path":"notes.txt","old_string":"two","new_string":"TWO"}"#,
        ),
        answer_stream("Changed the second line."),
        answer_stream("Changed the second line."),
    ])
    .await;
    let (dir, root) = project();
    std::fs::write(root.join("notes.txt"), "one\ntwo\n").unwrap();
    // a git tree with nothing to review when the session begins
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&root)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {out:?}");
    };
    git(&["init", "-q"]);
    git(&["add", "notes.txt"]);
    git(&["commit", "-qm", "start"]);
    let data = dir.path().join("data");
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn(&url, &r, &d, "make two loud\n/exit\n").wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    assert!(stdout.contains(" · session 1"), "{stdout}");
    assert!(
        stdout.ends_with("· session 1 is kept: ognivo chat --resume 1\n"),
        "{stdout}"
    );

    // a later chat goes on with it: the conversation, the journal, the seed
    let (url, bodies) = fake_server(vec![answer_stream("Still here.")]).await;
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn_with(
            &url,
            &r,
            &d,
            "/changes\nare you there?\n/exit\n",
            &["--continue"],
        )
        .wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{:?}\n{stdout}\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let at = |s: &str| stdout.find(s).unwrap_or_else(|| panic!("{s}: {stdout}"));
    assert!(at(" · session 1, resumed after 1 turn\n") < at("· last message: make two loud\n"));
    assert!(at("· unreviewed: notes.txt (+1 -1) · /changes /accept /revert\n") < at("/help"));
    // what git shows is the journal's own change, nothing else
    assert!(!stdout.contains("ognivo did not make"), "{stdout}");
    assert!(at("→ notes.txt · +1 -1\n  2 - two\n  2 + TWO\n") < at("Still here."));
    {
        let bodies = bodies.lock().unwrap();
        let messages = bodies[0]["messages"].as_array().unwrap();
        assert_eq!(
            messages[1],
            json!({"role": "user", "content": "make two loud"})
        );
        assert_eq!(
            messages.last().unwrap(),
            &json!({"role": "user", "content": "are you there?"})
        );
        assert_eq!(bodies[0]["seed"], json!(3 + 3 + 1));
    }

    let (ok, list) = sessions(&data, &root, &[]);
    assert!(ok, "{list}");
    assert!(
        list.starts_with("   1  ")
            && list.contains("  chat  gpt-oss-20b  2 turns  make two loud · 1 file unreviewed\n"),
        "{list}"
    );
    let (ok, found) = sessions(&data, &root, &["--search", "still"]);
    assert!(ok, "{found}");
    assert!(
        found.contains("make two loud") && found.contains("[Still] here."),
        "{found}"
    );
    let (_, none) = sessions(&data, &root, &["--search", "nowhere"]);
    assert_eq!(none, "nothing found\n");

    // a change ognivo did not make keeps builds and tests asking
    std::fs::write(root.join("mine.txt"), "by hand\n").unwrap();
    let (url, _) = fake_server(vec![]).await;
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn_with(&url, &r, &d, "/exit\n", &["--resume", "1"]).wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(
            "· the project may hold changes ognivo did not make: builds and tests ask until /accept\n"
        ),
        "{stdout}"
    );
    // the session had turns, so it stays
    assert!(stdout.contains("· session 1 is kept"), "{stdout}");
}

#[tokio::test]
async fn a_session_without_a_turn_leaves_nothing_and_a_missing_one_is_named() {
    let (url, _) = fake_server(vec![]).await;
    let (dir, root) = project();
    let data = dir.path().join("data");
    let (r, d, u) = (root.clone(), data.clone(), url.clone());
    let out = tokio::task::spawn_blocking(move || spawn(&u, &r, &d, "/exit\n").wait_with_output())
        .await
        .unwrap()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(!stdout.contains("is kept"), "{stdout}");
    assert_eq!(
        sessions(&data, &root, &[]),
        (true, "no sessions in this project yet\n".to_owned())
    );
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn_with(&url, &r, &d, "", &["--resume", "7"]).wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("there is no session 7; see ognivo sessions"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A call whose step leaves the server holding `held` tokens.
fn call_holding(id: &str, name: &str, args: &str, held: u64) -> String {
    let events = [
        chunk(json!({"role": "assistant", "content": null}), None),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": id, "type": "function", "function": {"name": name, "arguments": args}}]}),
            None,
        ),
        chunk(json!({}), Some("tool_calls")),
        json!({"choices": [], "timings": {"cache_n": 0, "prompt_n": held, "predicted_n": 20}}),
    ];
    sse(&events)
}

#[tokio::test]
async fn a_full_context_is_compacted_and_the_turn_goes_on() {
    let read = |id: &str, path: &str| call_stream(id, "read", &format!(r#"{{"path":"{path}"}}"#));
    let (url, bodies) = fake_server(vec![
        read("c1", "big1.txt"),
        read("c2", "big2.txt"),
        read("c3", "big3.txt"),
        call_holding("c4", "read", r#"{"path":"small.txt"}"#, 70_000),
        answer_stream("Task: read the files. Next: answer."),
        answer_stream("Read them."),
    ])
    .await;
    let (dir, root) = project();
    let line = format!("{}\n", "x".repeat(59));
    for name in ["big1.txt", "big2.txt", "big3.txt"] {
        std::fs::write(root.join(name), line.repeat(1000)).unwrap();
    }
    std::fs::write(root.join("small.txt"), "small\n").unwrap();
    let data = dir.path().join("data");
    let (r, d) = (root.clone(), data.clone());
    let out = tokio::task::spawn_blocking(move || {
        spawn(&url, &r, &d, "read all the files\n/exit\n").wait_with_output()
    })
    .await
    .unwrap()
    .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{:?}\n{stdout}", out.status);
    let at = |s: &str| stdout.find(s).unwrap_or_else(|| panic!("{s}: {stdout}"));
    assert!(at("→ read small.txt") < at("\n· compacting the conversation: about 70."));
    assert!(
        stdout.contains("K tokens, over the 68.8K budget\n"),
        "{stdout}"
    );
    assert!(at("· compacting the conversation") < at("\n· compacted: "));
    assert!(at(" became a summary of 6 words; ") < at("Read them."));

    let bodies = bodies.lock().unwrap();
    assert_eq!(bodies.len(), 6);
    // the summary is asked for with no calls, over the whole history
    assert_eq!(bodies[4]["tool_choice"], json!("none"));
    let ask = bodies[4]["messages"].as_array().unwrap();
    assert!(
        ask.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .starts_with("[ognivo] The context is nearly full")
    );
    assert_eq!(
        ask.len(),
        bodies[3]["messages"].as_array().unwrap().len() + 3
    );
    // the next step: the system message, the note, the kept steps ending with the small read
    let next = bodies[5]["messages"].as_array().unwrap();
    assert_eq!(next[0], ask[0]);
    let note = next[1]["content"].as_str().unwrap();
    assert!(
        note.starts_with("[ognivo] The conversation was compacted to fit the context")
            && note.contains("\n1. read all the files\n")
            && note.contains("Task: read the files. Next: answer."),
        "{note}"
    );
    let last = next.last().unwrap();
    assert_eq!(last["role"], json!("tool"));
    assert!(
        last["content"]
            .as_str()
            .unwrap()
            .starts_with("[read: small.txt"),
        "{last}"
    );
    assert!(next.len() < ask.len() - 2, "{}", next.len());
}
