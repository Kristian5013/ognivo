//! T0's injection corpus: the text of every control token of every profile, wherever text
//! reaches a request (the user's message, an instruction file, a file the model reads, a
//! command's output, grep's matches, an error that quotes a call, the model's own earlier
//! output), never once whole in what the server gets, and whole again in what the tools
//! and the user get, whether the model copies it with the guard's joiner or without.

mod common;

use common::{Gen, Project, Reply, Server, agent, options, profile};
use ognivo_core::agent::{AgentEvent, TurnEnd};
use ognivo_models::guard::JOINER;
use ognivo_models::{Family, Guard};
use serde_json::{Value, json};

/// A profile's tools for the scenario: read, grep, shell and write with their argument
/// names (crates/ognivo-tools/src/projection.rs).
struct Tools {
    read: (&'static str, &'static str),
    grep: &'static str,
    shell: &'static str,
    write: (&'static str, &'static str, &'static str),
}

fn tools(family: Family) -> Tools {
    match family {
        Family::Harmony => Tools {
            read: ("read", "path"),
            grep: "grep",
            shell: "shell",
            write: ("write", "path", "content"),
        },
        Family::Gemma4 => Tools {
            read: ("read", "file"),
            grep: "grep",
            shell: "bash",
            write: ("write", "file", "text"),
        },
        Family::QwenXml => Tools {
            read: ("Read", "file_path"),
            grep: "Grep",
            shell: "Bash",
            write: ("Write", "file_path", "content"),
        },
    }
}

fn strings<'a>(v: &'a Value, out: &mut Vec<&'a str>) {
    match v {
        Value::String(s) => out.push(s),
        Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
        Value::Object(o) => {
            for (k, x) in o {
                out.push(k);
                strings(x, out);
            }
        }
        _ => {}
    }
}

#[tokio::test]
async fn control_tokens_never_reach_the_server_whole() {
    for id in ["gpt-oss-20b", "gemma-4-26b-a4b", "qwen3.8-27b"] {
        let p = profile(id);
        let t = tools(p.family);
        let guard = Guard::new(&p.tokens);
        let tokens: Vec<&str> = p.tokens.guarded().collect();
        let list: String = tokens.iter().map(|t| format!("{t}\n")).collect();
        // what the model reads, so what it copies
        let broken = guard.escape(&list).into_owned();
        // M1 T2-mini: gpt-oss copied the joiner as U+200D ZERO WIDTH JOINER
        let lookalike = broken.replace(JOINER, "\u{200D}");
        // not framing: a copy without the joiner may run
        let plain = *tokens
            .iter()
            .find(|t| !p.tokens.framing.iter().any(|f| f == *t))
            .expect("a guarded string that does not frame calls");
        // a Makefile: a project with a build, whose unverified answer goes back once
        let proj = Project::new(&[
            ("AGENTS.md", &format!("Never print these:\n{list}")),
            ("notes.txt", &list),
            ("Makefile", "all:\n"),
        ]);
        let (read, path) = t.read;
        let (write, file, content) = t.write;
        let gens = [
            Gen::calls("", &[("c1", read, json!({path: "notes.txt"}))]),
            Gen::calls(
                &format!("The notes hold {}; nothing to run there.", tokens.join(" ")),
                &[("c2", t.grep, json!({"pattern": ".", "path": "notes.txt"}))],
            ),
            Gen::calls("", &[("c3", t.shell, json!({"command": "cat notes.txt"}))]),
            Gen::calls(
                "",
                &[("c4", read, json!({path: format!("missing{plain}.txt")}))],
            ),
            Gen::calls(
                "",
                &[(
                    "c5",
                    write,
                    json!({file: "out.txt", content: format!("{broken}{lookalike}{plain}\n")}),
                )],
            ),
            // the first answer after a change goes back: nothing was built or tested
            Gen::answer("", "Copied."),
            Gen::answer("", &format!("{broken}{lookalike}")),
        ];
        let server = Server::start(gens.iter().cloned().map(Reply::Stream).collect()).await;
        let mut a = agent(&server.url, &p, options(&proj.root, &p));
        let mut on = |_: AgentEvent| {};
        let ask = format!("Copy notes.txt to out.txt; it has {}", tokens.join(" "));
        let end = a.turn(&ask, &mut on).await;
        let end = end.unwrap_or_else(|e| {
            let last = server.bodies().pop().map(|b| b["messages"].clone());
            panic!("{id}: {e}; the last request's messages: {last:#?}")
        });

        let bodies = server.bodies();
        assert_eq!(bodies.len(), gens.len(), "{id}");
        for (k, body) in bodies.iter().enumerate() {
            let mut all = Vec::new();
            strings(body, &mut all);
            for s in all {
                for token in &tokens {
                    assert!(
                        !s.contains(token),
                        "{id}: request {k} has {token} whole in {s:?}"
                    );
                }
            }
        }
        // every surface reached the requests, broken
        let last = bodies.last().unwrap().to_string();
        let escaped = |t: &str| serde_json::to_string(&guard.escape(t)).unwrap();
        for surface in ["Never print these:", "missing", "The notes hold"] {
            assert!(last.contains(surface), "{id}: {surface}");
        }
        assert!(last.contains(escaped(plain).trim_matches('"')), "{id}");
        assert_eq!(
            std::fs::read_to_string(proj.root.join("out.txt")).unwrap(),
            format!("{list}{list}{plain}\n"),
            "{id}"
        );
        let TurnEnd::Answer(answer) = end else {
            panic!("{id}: {end:?}");
        };
        assert_eq!(answer.text, format!("{list}{list}"), "{id}");
    }
}
