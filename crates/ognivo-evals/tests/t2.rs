//! T2-mini without a GPU: the scenarios load and each judges something, and the runner,
//! driven by a scripted model, tells a passing run from a failing one and counts the exit
//! metrics from what the session logged.

mod common;

use std::collections::BTreeSet;

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
        native: false,
        layout: Default::default(),
        subagents: false,
        slots: 1,
        mcp_server: t2::mcp_server(),
        mcp_listing: Default::default(),
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

    let results = [good, bad];
    let sum = t2::summarize(&p.id, json!({}), &[1, 2], &results, &BTreeSet::new());
    assert_eq!((sum.runs, sum.passed), (2, 1));
    assert_eq!((sum.parse_ok.ok, sum.parse_ok.of), (5, 5));
    assert_eq!((sum.edit_first_try.ok, sum.edit_first_try.of), (1, 2));
    assert_eq!(sum.edit_first_try_ambiguous.of, 0);
    assert_eq!((sum.schema_ok.ok, sum.schema_ok.of), (3, 3));
    assert_eq!(sum.categories["recovery"], [1, 2]);
    assert_eq!(sum.failed, ["recovery-stale-description.2"]);

    // the edits of an ambiguous scenario count apart from the exit gate
    let apart = BTreeSet::from(["recovery-stale-description".to_owned()]);
    let sum = t2::summarize(&p.id, json!({}), &[1, 2], &results, &apart);
    assert_eq!(sum.edit_first_try.of, 0);
    let a = &sum.edit_first_try_ambiguous;
    assert_eq!((a.ok, a.of), (1, 2));
}

#[test]
fn the_scenarios_with_a_duplicate_on_purpose_are_ambiguous() {
    let all = t2::load(&t2::scenarios_dir()).unwrap();
    assert_eq!(
        t2::ambiguous(&all),
        BTreeSet::from(
            [
                "edit-fix-operator",
                "edit-python-indent",
                "recovery-ambiguous-line"
            ]
            .map(str::to_owned)
        )
    );
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
        native: false,
        layout: Default::default(),
        subagents: false,
        slots: 1,
        mcp_server: t2::mcp_server(),
        mcp_listing: Default::default(),
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
        native: false,
        layout: Default::default(),
        subagents: false,
        slots: 1,
        mcp_server: t2::mcp_server(),
        mcp_listing: Default::default(),
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

#[tokio::test]
async fn an_mcp_scenario_runs_at_each_size_with_its_tools_declared_or_searched() {
    let p = profile("gpt-oss-20b");
    let ids: BTreeSet<String> = t2::load(&t2::scenarios_dir())
        .unwrap()
        .into_iter()
        .filter(|s| s.id.starts_with("mcp-current-weather"))
        .map(|s| s.id)
        .collect();
    assert_eq!(
        ids.into_iter().collect::<Vec<_>>(),
        [
            "mcp-current-weather@15",
            "mcp-current-weather@40",
            "mcp-current-weather@5"
        ]
    );
    let out = tempfile::tempdir().unwrap();
    let weather = json!({"city": "Oslo"});
    let replies = [
        // five tools, declared: the model calls the one it needs by name
        Gen::calls(
            "",
            &[("c1", "mcp__workspace__get_weather", weather.clone())],
        ),
        Gen::answer("", "It is -3 °C in Oslo, with light snow."),
        // forty, found: it searches, then calls through mcp_call
        Gen::calls(
            "",
            &[("c1", "tool_search", json!({"query": "weather now"}))],
        ),
        Gen::calls(
            "",
            &[(
                "c2",
                "mcp_call",
                json!({"tool": "mcp__workspace__get_weather", "arguments": weather}),
            )],
        ),
        Gen::answer("", "Oslo has -3 °C now."),
        // forty again: a forecast is not the weather now
        Gen::calls("", &[("c1", "tool_search", json!({"query": "weather"}))]),
        Gen::calls(
            "",
            &[(
                "c2",
                "mcp_call",
                json!({"tool": "mcp__workspace__get_forecast", "arguments": {"city": "Oslo", "days": 1}}),
            )],
        ),
        Gen::answer("", "Probably about -3 °C."),
    ];
    let server = Server::start(replies.into_iter().map(Reply::Stream).collect()).await;
    let target = Target {
        url: server.url.clone(),
        profile: p,
        n_ctx: None,
        native: false,
        layout: Default::default(),
        subagents: false,
        slots: 1,
        mcp_server: t2::mcp_server(),
        mcp_listing: Default::default(),
    };
    let few = t2::run(&scenario("mcp-current-weather@5"), &target, 1, out.path())
        .await
        .unwrap();
    assert!(few.passed, "{:?}", few.failures);
    let m = few.mcp.as_ref().unwrap();
    assert_eq!((m.tools, m.searched, m.tool_correct), (5, false, true));
    assert_eq!(m.calls, ["get_weather"]);
    let names = |body: &serde_json::Value| -> Vec<String> {
        body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
            .collect()
    };
    let bodies = server.bodies();
    assert!(names(&bodies[0]).contains(&"mcp__workspace__get_weather".to_owned()));
    // the project's message says what the servers are for
    let project = bodies[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        project.contains("MCP servers give tools that reach beyond the project: workspace (5 tools). A question about what they cover"),
        "{project}"
    );

    // forty, as many as a session declares: found with tool_search when it is asked for
    let searching = Target {
        mcp_listing: ognivo_mcp::Listing::Searched,
        ..target.clone()
    };
    let many = t2::run(
        &scenario("mcp-current-weather@40"),
        &searching,
        1,
        out.path(),
    )
    .await
    .unwrap();
    assert!(many.passed, "{:?}", many.failures);
    let m = many.mcp.as_ref().unwrap();
    assert_eq!((m.tools, m.searched, m.tool_correct), (40, true, true));
    assert_eq!(m.calls, ["get_weather"]);
    assert_eq!(many.tools, ["ToolSearch", "mcp__workspace__get_weather"]);
    // the tools stay the same whatever the search finds: the prompt's prefix holds
    let bodies = server.bodies();
    let (searched, called) = (&bodies[2], &bodies[3]);
    assert_eq!(names(searched), names(called));
    let declared = names(searched);
    assert!(
        declared.contains(&"tool_search".to_owned()) && declared.contains(&"mcp_call".to_owned())
    );
    let project = searched["messages"][1]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        project.contains("workspace (40 tools)")
            && project.contains("call the tool, found with tool_search, rather"),
        "{project}"
    );
    assert!(
        !declared.iter().any(|n| n.starts_with("mcp__")),
        "{declared:?}"
    );
    // the search's result declares the tool as gpt-oss reads declarations
    let found = called["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .unwrap()["content"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        found.starts_with("[tool_search: 1 tool for \"weather now\"]"),
        "{found}"
    );
    assert!(
        found.contains("type mcp__workspace__get_weather = (_: {"),
        "{found}"
    );
    assert!(
        found.ends_with("Call it with mcp_call: `tool` is its name, `arguments` its parameters."),
        "{found}"
    );

    let wrong = t2::run(
        &scenario("mcp-current-weather@40"),
        &searching,
        2,
        out.path(),
    )
    .await
    .unwrap();
    assert!(!wrong.passed);
    let m = wrong.mcp.as_ref().unwrap();
    assert!(!m.tool_correct);
    assert_eq!(
        wrong.failures,
        [r#"did not call the MCP tool get_weather (called ["get_forecast"])"#]
    );
}
