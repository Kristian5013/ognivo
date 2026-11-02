//! `ognivo chat` end to end against the fake llama-server. Off a terminal the line editor
//! reads plain lines, so stdin carries both the messages and the answers to ognivo's
//! questions.

mod common;

use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use common::{HANG, answer_stream, call_stream, fake_server};
use serde_json::json;

fn spawn(url: &str, root: &Path, data: &Path, stdin: &str) -> Child {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args([
            "chat",
            "--url",
            url,
            "--profile",
            "gpt-oss-20b",
            "--seed",
            "3",
            "--data-dir",
        ])
        .arg(data)
        .arg("--cwd")
        .arg(root)
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
