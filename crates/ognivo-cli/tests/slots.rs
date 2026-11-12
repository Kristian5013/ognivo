//! Slots and llama-server's router, end to end: requests are pinned to a slot only when the
//! user says, and behind a router every request names the model. No GPU, no network.

mod common;

use std::io::Write as _;
use std::path::Path;
use std::process::{Child, Command, Stdio};

use common::{answer_stream, fake_router, fake_server};
use serde_json::json;

/// `ognivo run` with `args` in a project under `dir`, its data in `dir/data`.
fn run(dir: &Path, url: &str, args: &[&str]) -> Child {
    let root = dir.join("proj");
    std::fs::create_dir_all(&root).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ognivo"))
        .args(["run", "--url", url, "--seed", "1", "--data-dir"])
        .arg(dir.join("data"))
        .arg("--cwd")
        .arg(&root)
        .args(args)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
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
        .write_all(b"Say hello.")
        .unwrap();
    child
}

async fn finish(child: Child) -> std::process::Output {
    tokio::task::spawn_blocking(move || child.wait_with_output().unwrap())
        .await
        .unwrap()
}

#[tokio::test]
async fn requests_go_where_the_server_puts_them_unless_a_slot_is_given() {
    let dir = tempfile::tempdir().unwrap();
    let (url, bodies) = fake_server(vec![answer_stream("Hello."), answer_stream("Hello.")]).await;
    for args in [
        &["--profile", "gpt-oss-20b"][..],
        &["--profile", "gpt-oss-20b", "--slot", "1"],
    ] {
        let out = finish(run(dir.path(), &url, args)).await;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let bodies = bodies.lock().unwrap();
    // the server's prompt cache keeps the conversation's KV in whichever slot it runs
    assert_eq!(bodies[0].get("id_slot"), None);
    assert_eq!(bodies[1]["id_slot"], json!(1));
    assert!(bodies.iter().all(|b| b.get("model").is_none()));
}

#[tokio::test]
async fn behind_a_router_every_request_names_the_model_of_the_profile() {
    let dir = tempfile::tempdir().unwrap();
    let models = json!([
        {"id": "qwen3.8-27b-q4", "aliases": [], "status": {"value": "loaded", "args": []}},
        {"id": "gpt-oss-20b-F16", "aliases": [], "status": {"value": "loaded", "args": []}}
    ]);
    let (url, bodies, gets) = fake_router(models.clone(), vec![answer_stream("Hello.")]).await;
    let out = finish(run(dir.path(), &url, &["--profile", "gpt-oss-20b"])).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(bodies.lock().unwrap()[0]["model"], json!("gpt-oss-20b-F16"));
    {
        let gets = gets.lock().unwrap();
        assert!(
            gets.iter()
                .any(|g| g.starts_with("GET /props?model=gpt-oss-20b-F16 ")),
            "{gets:?}"
        );
    }

    // two models loaded and nothing to choose by: the user says which
    let (url, _, _) = fake_router(models, vec![]).await;
    let out = finish(run(dir.path(), &url, &[])).await;
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains(
            "is llama-server's router with qwen3.8-27b-q4 (loaded), gpt-oss-20b-F16 (loaded): pass --model"
        ),
        "{stderr}"
    );
}
