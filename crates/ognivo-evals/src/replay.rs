//! The parse corpus of T0: the chat completions `llm_tap.py` recorded in the M0 and M1
//! runs (`runs/**/trajectory/llm.jsonl`), each read back by the Generic driver's own code
//! ([`read_recorded`]) and named by what it allows: which calls, an answer, a cut, a loop,
//! what made it malformed, a server error. The verdicts are kept in git
//! (`evals/t0/replay.json`); the recordings stay on the machine that made them, since they
//! hold private code.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ognivo_models::driver::{Finished, read_recorded};
use ognivo_models::generic::{is_context_exceeded, is_format_error};
use ognivo_models::{Channel, EngineError, Guard, ModelProfile, Outcome};
use serde::Deserialize;

/// Verdicts by recording (its directory relative to the runs, without `trajectory/`), one
/// per chat completion in the order they ran.
pub type Verdicts = BTreeMap<String, Vec<String>>;

/// The smallest body `llm_tap.py` stops recording at (M0's 8 MiB; 64 MiB since): a longer
/// stream may have gone on after what the recording holds.
const TAP_LIMIT: usize = 8 << 20;

/// The fields of a record the replay reads; the request is skipped unparsed.
#[derive(Deserialize)]
struct Record {
    path: String,
    status: u16,
    #[serde(default)]
    response_raw: String,
}

/// The verdicts of every recording under `runs`, read on all cores. Recordings of a model
/// without a built-in profile are left out.
pub fn replay(runs: &Path, profiles: &[ModelProfile]) -> std::io::Result<Verdicts> {
    let mut work: Vec<(String, PathBuf, Guard)> = Vec::new();
    let mut files = recordings(runs)?;
    // the largest go first, so no core ends up with a big one alone at the end
    files.sort_by_key(|f| std::fs::metadata(f).map_or(0, |m| m.len()));
    for file in files {
        let name = file
            .parent()
            .and_then(Path::parent)
            .and_then(|d| d.strip_prefix(runs).ok())
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        if let Some(p) = profiles.iter().find(|p| name.contains(p.id.as_str())) {
            work.push((name, file, Guard::new(&p.tokens)));
        }
    }
    let work = Mutex::new(work);
    let out = Mutex::new(Ok(Verdicts::new()));
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get());
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                loop {
                    let next = work.lock().unwrap().pop();
                    let Some((name, file, guard)) = next else {
                        break;
                    };
                    let verdicts = std::fs::read_to_string(&file).map(|t| recording(&t, &guard));
                    let mut out = out.lock().unwrap();
                    match (out.as_mut(), verdicts) {
                        (Ok(all), Ok(v)) => {
                            all.insert(name, v);
                        }
                        (Ok(_), Err(e)) => *out = Err(e),
                        (Err(_), _) => {}
                    }
                }
            });
        }
    });
    out.into_inner().unwrap()
}

fn recordings(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_owned()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "llm.jsonl") {
                out.push(path);
            }
        }
    }
    Ok(out)
}

/// The verdicts of one `llm.jsonl`: its chat completions in order, other requests skipped.
fn recording(text: &str, guard: &Guard) -> Vec<String> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<Record>(line).ok())
        .filter(|r| r.path == "/v1/chat/completions")
        .map(|r| verdict(r.status, &r.response_raw, guard))
        .collect()
}

/// What one recorded response allows, as the Generic driver reads it.
pub fn verdict(status: u16, raw: &str, guard: &Guard) -> String {
    match read_recorded(status, raw.as_bytes(), guard, 0) {
        Ok(f) => outcome(&f),
        Err(EngineError::Protocol(m)) if m.starts_with("stream ended") => if raw.len() >= TAP_LIMIT
        {
            "tap-cut"
        } else {
            "incomplete"
        }
        .into(),
        Err(e) if is_format_error(&e) => "error:format".into(),
        Err(e) if is_context_exceeded(&e) => "error:context".into(),
        Err(EngineError::Status { status, .. }) => format!("error:{status}"),
        Err(EngineError::Protocol(_)) => "error:protocol".into(),
        Err(_) => "error".into(),
    }
}

fn outcome(f: &Finished) -> String {
    match &f.outcome {
        Outcome::Calls(calls) => {
            let names: Vec<&str> = calls.calls().iter().map(|c| c.name.as_str()).collect();
            format!("calls:{}", names.join(","))
        }
        Outcome::Answer if f.content.trim().is_empty() => "answer:empty".into(),
        Outcome::Answer => "answer".into(),
        Outcome::Truncated { partial } if partial.is_empty() => "truncated".into(),
        Outcome::Truncated { .. } => "truncated:call".into(),
        Outcome::Repeating { channel, .. } => match channel {
            Channel::Reasoning => "repeating:reasoning",
            Channel::Text => "repeating:text",
            Channel::Arguments { .. } => "repeating:arguments",
        }
        .into(),
        Outcome::Malformed { reason, .. } => format!("malformed:{}", malformed(reason)),
    }
}

/// The class of a malformed generation, from the reason the parser gives.
fn malformed(reason: &str) -> String {
    let kind = if reason.contains("a control token of your own format") {
        "framing"
    } else if reason.contains("arguments are not JSON:") {
        "not-json"
    } else if reason.contains("arguments are not a JSON object") {
        "not-object"
    } else if reason.contains("has no id or name") {
        "no-id-or-name"
    } else if reason.starts_with("call id ") {
        "repeated-id"
    } else if reason == "tool calls with finish_reason stop" {
        "calls-with-stop"
    } else if reason == "finish_reason tool_calls without calls" {
        "no-calls"
    } else if reason == "no finish_reason" {
        "no-finish"
    } else if let Some(r) = reason.strip_prefix("finish_reason ") {
        return format!("finish-{r}");
    } else {
        "other"
    };
    kind.to_owned()
}

/// How `now` differs from `kept`, one line each.
pub fn differences(kept: &Verdicts, now: &Verdicts) -> Vec<String> {
    let mut out = Vec::new();
    for (name, verdicts) in now {
        match kept.get(name) {
            None => out.push(format!("{name}: new, {} completions", verdicts.len())),
            Some(old) if old.len() != verdicts.len() => out.push(format!(
                "{name}: {} completions, kept {}",
                verdicts.len(),
                old.len()
            )),
            Some(old) => {
                for (i, (a, b)) in old.iter().zip(verdicts).enumerate() {
                    if a != b {
                        out.push(format!("{name} #{}: {a} -> {b}", i + 1));
                    }
                }
            }
        }
    }
    for name in kept.keys().filter(|k| !now.contains_key(*k)) {
        out.push(format!("{name}: kept, but not found now"));
    }
    out
}

/// How many completions got each verdict, all calls counted as `calls`.
pub fn tally(verdicts: &Verdicts) -> BTreeMap<&str, usize> {
    let mut out = BTreeMap::new();
    for v in verdicts.values().flatten() {
        let class = if v.starts_with("calls:") {
            "calls"
        } else {
            v.as_str()
        };
        *out.entry(class).or_insert(0) += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: &str, finish: &str) -> String {
        format!(
            "data: {{\"choices\":[{{\"index\":0,\"delta\":{delta},\"finish_reason\":{finish}}}]}}\n\n"
        )
    }

    #[test]
    fn verdicts_name_what_a_response_allows() {
        let g = Guard::default();
        let call = |args: &str| {
            let delta = format!(
                r#"{{"tool_calls":[{{"index":0,"id":"a","type":"function","function":{{"name":"read","arguments":{}}}}}]}}"#,
                serde_json::to_string(args).unwrap()
            );
            chunk(&delta, "null")
        };
        let done = "data: [DONE]\n\n";
        let ok = call(r#"{"path":"a.rs"}"#) + &chunk("{}", r#""tool_calls""#) + done;
        assert_eq!(verdict(200, &ok, &g), "calls:read");
        let bad = call(r#"{"path":"#) + &chunk("{}", r#""tool_calls""#) + done;
        assert_eq!(verdict(200, &bad, &g), "malformed:not-json");
        let cut = call(r#"{"path":"a.rs"}"#);
        assert_eq!(verdict(200, &cut, &g), "incomplete");
        let empty = chunk(r#"{"content":" "}"#, r#""stop""#) + done;
        assert_eq!(verdict(200, &empty, &g), "answer:empty");
        let long = chunk("{}", r#""length""#) + done;
        assert_eq!(verdict(200, &long, &g), "truncated");
        assert_eq!(verdict(503, "loading", &g), "error:503");
    }

    #[test]
    fn differences_point_at_each_changed_completion() {
        let kept = Verdicts::from([("a".into(), vec!["answer".into(), "calls:read".into()])]);
        let mut now = kept.clone();
        now.get_mut("a").unwrap()[1] = "incomplete".into();
        now.insert("b".into(), vec!["answer".into()]);
        assert_eq!(
            differences(&kept, &now),
            ["a #2: calls:read -> incomplete", "b: new, 1 completions"]
        );
        assert_eq!(
            tally(&now),
            BTreeMap::from([("answer", 2), ("incomplete", 1)])
        );
    }
}
