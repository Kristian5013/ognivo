//! T0's KV-fidelity simulation: llama-server reuses the cached prompt as far as the next
//! request repeats it token for token, so within a prompt every request must repeat the
//! one before and go on with the generation it got, byte for byte. Each scenario runs the
//! agent on the Generic driver against a fake server and names the requests where the
//! prompt starts over and those that leave a generation out; any other break is a bug.

mod common;

use std::time::Duration;

use common::{Gen, Project, Reply, Server, agent, cache, driver, options, profile};
use ognivo_core::agent::{AgentEvent, AllowAll};
use ognivo_core::session::Session;
use ognivo_models::Family;
use ognivo_models::profile::ContextBudget;
use ognivo_store::{NewSession, Store};
use serde_json::json;

const LIB: &str = "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";

fn replies(gens: &[Option<Gen>]) -> Vec<Reply> {
    gens.iter()
        .map(|g| g.clone().map_or(Reply::Hang, Reply::Stream))
        .collect()
}

async fn turn(a: &mut ognivo_core::agent::Agent, text: &str) {
    let mut on = |_: AgentEvent| {};
    a.turn(text, &mut on).await.unwrap();
}

/// Starts a turn and drops it after `after`, as ^C does.
async fn stopped_turn(a: &mut ognivo_core::agent::Agent, text: &str, after: Duration) {
    let mut on = |_: AgentEvent| {};
    let t = a.turn(text, &mut on);
    assert!(tokio::time::timeout(after, t).await.is_err());
}

#[tokio::test]
async fn gpt_oss_steps_and_turns_extend_one_prompt() {
    let p = profile("gpt-oss-20b");
    // a crate: the unverified answer goes back once
    let proj = Project::new(&[("src/lib.rs", LIB), ("Cargo.toml", "[package]\n")]);
    let gens = [
        Gen::calls("Read it first.", &[("c1", "read", json!({"path": "src/lib.rs"}))]),
        Gen::calls(
            "Saturate the sum.",
            &[("c2", "edit", json!({"path": "src/lib.rs", "old_string": "    a + b", "new_string": "    a.saturating_add(b)"}))],
        ),
        // sent back to verify: the draft is left out on purpose (agent.rs, Outcome::Answer)
        Gen::answer("Done.", "add saturates now."),
        Gen::answer("No build file, so nothing to run.", "add saturates now; nothing was built or tested."),
        Gen::calls("Look again.", &[("c3", "grep", json!({"pattern": "saturating", "path": "src"}))]),
        Gen::answer("", "Yes, in src/lib.rs."),
    ]
    .map(Some);
    let server = Server::start(replies(&gens)).await;
    let mut a = agent(&server.url, &p, options(&proj.root, &p));
    turn(&mut a, "make add saturate").await;
    turn(&mut a, "is it there?").await;
    let bodies = server.bodies();
    assert_eq!(bodies.len(), gens.len());
    let c = cache(&bodies, &gens, Family::Harmony);
    assert_eq!((c.restarts, c.dropped), (vec![], vec![3]));
}

#[tokio::test]
async fn gemma_and_qwen_histories_extend_one_prompt() {
    let gemma = profile("gemma-4-26b-a4b");
    let proj = Project::new(&[("src/lib.rs", LIB)]);
    let mut talk = Gen::calls("", &[("g1", "read", json!({"file": "src/lib.rs"}))]);
    // Gemma's history rule: an assistant message with calls carries no text
    talk.content = "Let me look.".into();
    let gens = [talk, Gen::answer("It adds.", "add returns a + b.")].map(Some);
    let server = Server::start(replies(&gens)).await;
    let mut a = agent(&server.url, &gemma, options(&proj.root, &gemma));
    turn(&mut a, "what does add do?").await;
    let c = cache(&server.bodies(), &gens, Family::Gemma4);
    assert_eq!((c.restarts, c.dropped), (vec![], vec![]));

    let qwen = profile("qwen3.8-27b");
    let gens = [
        Gen::calls(
            "Both at once.",
            &[
                ("q1", "Read", json!({"file_path": "src/lib.rs"})),
                ("q2", "Grep", json!({"pattern": "add", "path": "src"})),
            ],
        ),
        Gen::answer("", "add is defined once, in src/lib.rs."),
    ]
    .map(Some);
    let server = Server::start(replies(&gens)).await;
    let mut a = agent(&server.url, &qwen, options(&proj.root, &qwen));
    turn(&mut a, "where is add?").await;
    let c = cache(&server.bodies(), &gens, Family::QwenXml);
    assert_eq!((c.restarts, c.dropped), (vec![], vec![]));
}

#[tokio::test]
async fn a_stopped_turn_leaves_what_was_sent_as_it_was() {
    let p = profile("gpt-oss-20b");
    let proj = Project::new(&[("src/lib.rs", LIB)]);
    let gens = [
        // stopped in the generation
        None,
        // stopped while the command runs
        Some(Gen::calls(
            "Wait for it.",
            &[("c1", "shell", json!({"command": "sleep 5"}))],
        )),
        Some(Gen::answer("", "Stopped twice; nothing changed.")),
    ];
    let server = Server::start(replies(&gens)).await;
    let mut a = agent(&server.url, &p, options(&proj.root, &p));
    stopped_turn(&mut a, "first", Duration::from_millis(300)).await;
    a.abandon_turn("The user stopped the last turn.");
    stopped_turn(&mut a, "second", Duration::from_millis(500)).await;
    a.abandon_turn("The user stopped the last turn.");
    turn(&mut a, "third").await;
    let bodies = server.bodies();
    let c = cache(&bodies, &gens, Family::Harmony);
    assert_eq!((c.restarts, c.dropped), (vec![], vec![]));
    // the model learns of each stop with the next message
    let last = bodies[2]["messages"].as_array().unwrap();
    // the user's messages, after the project's context
    let users: Vec<&str> = last
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].as_str().unwrap())
        .skip(1)
        .collect();
    assert_eq!(
        users,
        [
            "first",
            "second\n\n[ognivo] The user stopped the last turn.",
            "third\n\n[ognivo] The user stopped the last turn."
        ]
    );
}

#[tokio::test]
async fn a_compaction_starts_the_one_new_prompt() {
    let p = profile("gpt-oss-20b");
    let big = "x".repeat(70) + "\n";
    let proj = Project::new(&[("big.txt", &big.repeat(100)), ("small.txt", "small\n")]);
    let gens = [
        Gen::calls("Read it.", &[("c1", "read", json!({"path": "big.txt"}))]),
        Gen::answer("", "big.txt is 100 lines of x."),
        // the summary, in a request that goes on from the last one
        Gen::answer(
            "",
            "Task: look at the files. Done: read big.txt. Next: read small.txt.",
        ),
        Gen::calls(
            "Now the small one.",
            &[("c2", "read", json!({"path": "small.txt"}))],
        ),
        Gen::answer("", "small.txt says small."),
    ]
    .map(Some);
    let server = Server::start(replies(&gens)).await;
    let mut opts = options(&proj.root, &p);
    opts.context = ContextBudget {
        working_max: 6000,
        compact_at: 0.5,
        max_tokens: 1000,
        ceiling: 6000,
    };
    let mut a = agent(&server.url, &p, opts);
    turn(&mut a, "look at big.txt").await;
    turn(&mut a, "and small.txt").await;
    let bodies = server.bodies();
    assert_eq!(bodies.len(), gens.len());
    let summary = bodies[2]["messages"].as_array().unwrap();
    assert!(
        summary.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .contains("compacted")
    );
    let c = cache(&bodies, &gens, Family::Harmony);
    // the summary request goes on from the last step; the step after it starts over
    assert_eq!((c.restarts, c.dropped), (vec![3], vec![]));
}

#[tokio::test]
async fn a_resumed_session_goes_on_with_the_same_prompt() {
    let p = profile("gpt-oss-20b");
    let proj = Project::new(&[("src/lib.rs", LIB)]);
    let gens = [
        Some(Gen::calls(
            "Read it.",
            &[("c1", "read", json!({"path": "src/lib.rs"}))],
        )),
        Some(Gen::answer("", "add returns a + b.")),
        // ognivo dies in this generation
        None,
        Some(Gen::answer("", "It still returns a + b.")),
    ];
    let server = Server::start(replies(&gens)).await;
    let db = proj.root.parent().unwrap().join("ognivo.db");
    let opts = options(&proj.root, &p);
    let mut a = agent(&server.url, &p, opts.clone());
    let new = NewSession {
        kind: "chat".into(),
        root: proj.root.clone(),
        profile: p.id.clone(),
        level: None,
        server: server.url.clone(),
        seed: 1,
    };
    a.keep_in(Session::create(Store::open(&db).unwrap(), &new).unwrap());
    let id = a.session_id().unwrap();
    turn(&mut a, "what does add do?").await;
    stopped_turn(&mut a, "and now?", Duration::from_millis(300)).await;
    // no abandon_turn and no close: the process is gone
    drop(a);

    let (session, resumed) = Session::open(Store::open(&db).unwrap(), id).unwrap();
    let mut a = ognivo_core::agent::Agent::resume(
        Box::new(driver(&server.url, &p)),
        p.family,
        Box::new(AllowAll),
        opts,
        session,
        resumed,
    );
    turn(&mut a, "still there?").await;
    let bodies = server.bodies();
    let c = cache(&bodies, &gens, Family::Harmony);
    assert_eq!((c.restarts, c.dropped), (vec![], vec![]));
    let last = bodies[3]["messages"].as_array().unwrap();
    assert_eq!(
        last.last().unwrap()["content"],
        "still there?\n\n[ognivo] ognivo stopped before the last turn ended; the session goes on now."
    );
}
