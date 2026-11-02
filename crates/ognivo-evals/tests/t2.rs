//! T2-mini without a GPU: the scenarios load and each judges something, and the runner,
//! driven by a scripted model, tells a passing run from a failing one and counts the exit
//! metrics from what the session logged.

mod common;

use common::{Gen, Reply, Server, profile};
use ognivo_evals::t2::{self, Target};
use ognivo_models::profile as profiles;
use serde_json::json;

#[test]
fn every_scenario_loads_and_judges_something() {
    let all = t2::load(&t2::scenarios_dir()).unwrap();
    assert!(all.len() >= 30, "{}", all.len());
    let raw: Vec<String> = std::fs::read_dir(t2::scenarios_dir())
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    for p in profiles::builtin().unwrap() {
        for text in &raw {
            assert!(
                p.tokens.guarded().all(|t| !text.contains(t)),
                "a control token in one piece (§5.3)"
            );
        }
        for s in &all {
            let s = s.for_profile(&p);
            let texts = [s.prompt.clone()]
                .into_iter()
                .chain(s.files.values().cloned())
                .chain(s.expect.files.values().cloned());
            for t in texts {
                assert!(!t.contains('⟦'), "{}: a stand-in left for {}", s.id, p.id);
            }
        }
    }
}

fn scenario(id: &str) -> t2::Scenario {
    t2::load(&t2::scenarios_dir())
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
}

#[tokio::test]
async fn a_scripted_model_passes_and_fails_and_is_counted() {
    let p = profile("gpt-oss-20b");
    let s = scenario("recovery-stale-description");
    let out = tempfile::tempdir().unwrap();
    let replies = [
        // reads, edits what is there, answers
        Gen::calls(
            "Read it first.",
            &[("c1", "read", json!({"path": "src/conf.rs"}))],
        ),
        Gen::calls(
            "",
            &[(
                "c2",
                "edit",
                json!({"path": "src/conf.rs", "old_string": "RETRIES: u32 = 4", "new_string": "RETRIES: u32 = 5"}),
            )],
        ),
        Gen::answer("", "RETRIES is 5 now; it was 4, not 3."),
        // edits from the prompt alone, and gives up
        Gen::calls(
            "",
            &[(
                "c1",
                "edit",
                json!({"path": "src/conf.rs", "old_string": "RETRIES: u32 = 3", "new_string": "RETRIES: u32 = 5"}),
            )],
        ),
        Gen::answer("", "I could not find RETRIES set to 3 there."),
    ];
    let server = Server::start(replies.into_iter().map(Reply::Stream).collect()).await;
    let target = Target {
        url: server.url.clone(),
        profile: p.clone(),
        n_ctx: None,
    };
    let good = t2::run(&s, &target, 1, out.path()).await.unwrap();
    assert!(good.passed, "{:?}", good.failures);
    assert_eq!(good.tools, ["Read", "Edit"]);
    assert_eq!((good.counts.attempts, good.counts.usable), (3, 3));
    assert!(
        !out.path()
            .join("work/recovery-stale-description.1")
            .exists()
    );
    assert!(
        out.path()
            .join("traces/recovery-stale-description.1.json")
            .exists()
    );

    let bad = t2::run(&s, &target, 2, out.path()).await.unwrap();
    assert!(!bad.passed);
    assert_eq!(
        bad.failures,
        [
            r#"src/conf.rs: line 1: "pub const RETRIES: u32 = 4;", not "pub const RETRIES: u32 = 5;""#
        ]
    );
    assert_eq!(bad.stats.edits_failed, 1);
    // a failed run keeps its project to look at
    assert!(
        out.path()
            .join("work/recovery-stale-description.2/proj/src/conf.rs")
            .exists()
    );

    let sum = t2::summarize(&p.id, json!({}), &[1, 2], &[good, bad]);
    assert_eq!((sum.runs, sum.passed), (2, 1));
    assert_eq!((sum.parse_ok.ok, sum.parse_ok.of), (5, 5));
    assert_eq!((sum.edit_first_try.ok, sum.edit_first_try.of), (1, 2));
    assert_eq!((sum.schema_ok.ok, sum.schema_ok.of), (3, 3));
    assert_eq!(sum.categories["recovery"], [1, 2]);
    assert_eq!(sum.failed, ["recovery-stale-description.2"]);
}

#[tokio::test]
async fn a_cut_generation_counts_against_parse_ok_and_runs_nothing() {
    let p = profile("gpt-oss-20b");
    let s = scenario("irrelevance-arithmetic");
    let out = tempfile::tempdir().unwrap();
    let mut cut = Gen::calls(
        "Let me check the files first.",
        &[("c9", "read", json!({"path": "READ"}))],
    );
    cut.cut = true;
    let replies = [cut, Gen::answer("", "391")];
    let server = Server::start(replies.into_iter().map(Reply::Stream).collect()).await;
    let target = Target {
        url: server.url.clone(),
        profile: p,
        n_ctx: None,
    };
    let r = t2::run(&s, &target, 1, out.path()).await.unwrap();
    assert!(r.passed, "{:?}", r.failures);
    let c = r.counts;
    assert_eq!(
        (c.attempts, c.usable, c.truncated, c.truncated_executed),
        (2, 1, 1, 0)
    );
}

#[tokio::test]
async fn a_refused_call_is_not_a_tool_that_ran() {
    let p = profile("gpt-oss-20b");
    let s = scenario("shell-run-script");
    let out = tempfile::tempdir().unwrap();
    let replies = [
        // the project's script runs headless on a clean project
        Gen::calls(
            "",
            &[("c1", "shell", json!({"command": "bash scripts/info.sh"}))],
        ),
        Gen::answer("", "It prints version 3.14.15."),
        // a command that may change things is refused, and the answer comes from reading
        Gen::calls(
            "",
            &[("c1", "shell", json!({"command": "rm -rf scripts/cache"}))],
        ),
        Gen::answer("", "The script would print version 3.14.15."),
    ];
    let server = Server::start(replies.into_iter().map(Reply::Stream).collect()).await;
    let target = Target {
        url: server.url.clone(),
        profile: p,
        n_ctx: None,
    };
    let ran = t2::run(&s, &target, 1, out.path()).await.unwrap();
    assert!(ran.passed, "{:?}", ran.failures);
    assert_eq!(ran.tools, ["Bash"]);
    assert!(ran.refused.is_empty());

    let refused = t2::run(&s, &target, 2, out.path()).await.unwrap();
    assert!(!refused.passed);
    assert_eq!(refused.refused, ["Bash"]);
    assert_eq!(refused.failures, [r#"used none of ["Bash"] (used [])"#]);
}
