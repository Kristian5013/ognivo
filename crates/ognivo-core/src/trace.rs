//! Traces in ATIF (docs/architecture.md §11): a session's event log as an Agent Trajectory
//! Interchange Format v1.7 document, the `trajectory.json` Harbor reads. Harbor validates
//! strictly, so everything ATIF does not declare lives in `extra` objects.
//!
//! - The system prompt of a conversation is a `system` step (again only when it changed),
//!   a compaction's note is one too, with the compaction in its `extra`, and so is every
//!   note ognivo gave the model.
//! - The user's messages are `user` steps.
//! - Each generation is an `agent` step: its text, its reasoning, the calls that ran and
//!   their results as the observation, its tokens as metrics (`prompt_tokens` counts the
//!   cached ones too, as ATIF has it). Its `extra` holds why it ran (`step`, or `summary`
//!   for a compaction), what came of it, the seed, a sha256 of what it was asked, its
//!   timing, calls it made that did not run, and the changes, questions and instruction
//!   files of its calls.
//! - The root's `extra.ognivo` holds the session: its project, profile and level, how each
//!   turn ended and the counters. On the Generic path there are no token ids.

use ognivo_models::profile;
use ognivo_store::{SessionId, Store, StoreError};
use ognivo_tools::projection;
use serde_json::{Map, Value, json};

pub const SCHEMA: &str = "ATIF-v1.7";

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("there is no session {0}")]
    NotFound(SessionId),
    #[error("session {0} has no events to make a trace of")]
    Empty(SessionId),
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// The trajectory of session `id`.
pub fn atif(store: &Store, id: SessionId) -> Result<Value, TraceError> {
    let row = store.session(id)?.ok_or(TraceError::NotFound(id))?;
    let mut b = Builder {
        level: row.level.clone(),
        ..Builder::default()
    };
    for e in store.events(id)? {
        let data = serde_json::from_str(&e.data).unwrap_or(Value::Null);
        b.event(&e.kind, &data, e.at, e.turn);
    }
    if b.steps.is_empty() {
        return Err(TraceError::Empty(id));
    }
    let mut agent = json!({
        "name": "ognivo",
        "version": env!("CARGO_PKG_VERSION"),
        "model_name": row.profile,
    });
    let family = profile::builtin()
        .ok()
        .and_then(|ps| ps.into_iter().find(|p| p.id == row.profile))
        .map(|p| p.family);
    if let Some(family) = family {
        // the session does not keep its layout yet: the standard one
        let tools: Vec<Value> = projection::declarations(family, projection::Layout::Standard)
            .into_iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t.name, "description": t.description, "parameters": t.parameters,
                }})
            })
            .collect();
        agent["tool_definitions"] = json!(tools);
    }
    let stats: Value = serde_json::from_str(&row.stats).unwrap_or(Value::Null);
    let total_steps = b.steps.len();
    Ok(json!({
        "schema_version": SCHEMA,
        "session_id": id.to_string(),
        "agent": agent,
        "steps": b.steps,
        "final_metrics": {
            "total_prompt_tokens": b.prompt,
            "total_completion_tokens": b.completion,
            "total_cached_tokens": b.cached,
            "total_steps": total_steps,
            "extra": {"generations": b.generations},
        },
        "extra": {"ognivo": {
            "session": id,
            "kind": row.kind,
            "project": row.root.display().to_string(),
            "profile": row.profile,
            "level": row.level,
            "server": row.server,
            "seed": row.seed,
            "title": row.title,
            "turns": b.turns,
            "stats": stats,
        }},
    }))
}

#[derive(Debug, Default)]
struct Builder {
    level: Option<String>,
    steps: Vec<Value>,
    /// The agent step the next results and side events belong to.
    agent: Option<usize>,
    /// The last system prompt given.
    system: Option<String>,
    /// The last project's context given.
    context: Option<String>,
    turns: Vec<Value>,
    prompt: u64,
    completion: u64,
    cached: u64,
    generations: u32,
}

impl Builder {
    fn push(&mut self, mut step: Value, at: i64) -> usize {
        step["step_id"] = json!(self.steps.len() + 1);
        step["timestamp"] = json!(iso8601(at));
        self.steps.push(step);
        self.steps.len() - 1
    }

    fn event(&mut self, kind: &str, d: &Value, at: i64, turn: u32) {
        match kind {
            "conversation" => {
                self.agent = None;
                let extra = json!({"conversation": d["n"], "reason": d["reason"]});
                if let Some(system) = d["system"].as_str()
                    && self.system.as_deref() != Some(system)
                {
                    self.system = Some(system.to_owned());
                    let step = json!({"source": "system", "message": system, "extra": extra});
                    self.push(step, at);
                }
                // the project's part, an ognivo note to the model
                if let Some(context) = d["context"].as_str()
                    && self.context.as_deref() != Some(context)
                {
                    self.context = Some(context.to_owned());
                    let step = json!({"source": "system", "message": context, "extra": extra});
                    self.push(step, at);
                }
                if let Some(note) = d["note"].as_str() {
                    self.push(
                        json!({"source": "system", "message": note, "extra": extra}),
                        at,
                    );
                }
            }
            "user" => {
                self.agent = None;
                self.push(json!({"source": "user", "message": d["text"]}), at);
            }
            "notice" => {
                self.agent = None;
                let step =
                    json!({"source": "system", "message": d["text"], "extra": {"from": "ognivo"}});
                self.push(step, at);
            }
            "generation" => {
                let step = self.generation(d, turn);
                self.agent = Some(self.push(step, at));
            }
            "tool_finished" => {
                let Some(step) = self.agent.and_then(|i| self.steps.get_mut(i)) else {
                    return;
                };
                let ran = step["tool_calls"]
                    .as_array()
                    .is_some_and(|calls| calls.iter().any(|c| c["tool_call_id"] == d["id"]));
                if ran {
                    let result = json!({"source_call_id": d["id"], "content": d["result"]});
                    match step["observation"]["results"].as_array_mut() {
                        Some(results) => results.push(result),
                        None => step["observation"] = json!({"results": [result]}),
                    }
                }
            }
            "file_changed" => {
                let count = |side: &str| {
                    d["hunks"].as_array().map_or(0, |hs| {
                        hs.iter()
                            .map(|h| h[side].as_array().map_or(0, Vec::len))
                            .sum::<usize>()
                    })
                };
                let change = json!({
                    "path": d["path"], "created": d["created"],
                    "added": count("added"), "removed": count("removed"),
                });
                self.attach("changes", change);
            }
            "approval" => {
                let mut q = d.clone();
                if let Some(m) = q.as_object_mut() {
                    m.remove("type");
                }
                self.attach("questions", q);
            }
            "instructions" => self.attach(
                "instructions",
                json!({"files": d["files"], "skipped": d["skipped"]}),
            ),
            "restarted" => self.attach("restarts", d["reason"].clone()),
            // the session a subagent of the step's delegate is kept in, with its own trace
            "subagent_session" => self.attach(
                "subagents",
                json!({"session": d["session"], "task": d["task"]}),
            ),
            "compacted" => {
                let note = self
                    .steps
                    .iter_mut()
                    .rev()
                    .find(|s| s["source"] == "system" && s["extra"]["reason"] == "compaction");
                if let Some(step) = note {
                    step["extra"]["compaction"] = json!({
                        "replaced": d["replaced"], "kept": d["kept"],
                        "summary": d["summary"].is_string(),
                    });
                }
            }
            "end" => {
                self.agent = None;
                let mut end = d.clone();
                if let Some(m) = end.as_object_mut() {
                    m.remove("type");
                    m.insert("turn".into(), json!(turn));
                }
                self.turns.push(end);
            }
            // tool_started, refused, compacting, the subagents' start and end, and streamed
            // text or reasoning: the generation and its results say it all
            _ => {}
        }
    }

    fn generation(&mut self, d: &Value, turn: u32) -> Value {
        self.generations += 1;
        let mut step = json!({
            "source": "agent",
            "message": d["content"].as_str().unwrap_or_default(),
            "llm_call_count": 1,
        });
        if let Some(r) = d["reasoning"].as_str().filter(|r| !r.is_empty()) {
            step["reasoning_content"] = json!(r);
        }
        if let Some(level) = &self.level {
            step["reasoning_effort"] = json!(level);
        }
        let calls: Vec<Value> = d["tool_calls"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|c| {
                json!({
                    "tool_call_id": c["id"],
                    "function_name": c["name"],
                    "arguments": arguments(c["arguments"].as_str().unwrap_or_default()),
                })
            })
            .collect();
        if !calls.is_empty() {
            step["tool_calls"] = json!(calls);
        }
        let mut metrics = Map::new();
        let (processed, cached) = (d["prompt_tokens"].as_u64(), d["cached_tokens"].as_u64());
        if processed.is_some() || cached.is_some() {
            let total = processed.unwrap_or(0) + cached.unwrap_or(0);
            metrics.insert("prompt_tokens".into(), json!(total));
            self.prompt += total;
        }
        if let Some(c) = cached {
            metrics.insert("cached_tokens".into(), json!(c));
            self.cached += c;
        }
        if let Some(c) = d["completion_tokens"].as_u64() {
            metrics.insert("completion_tokens".into(), json!(c));
            self.completion += c;
        }
        let timing = present(&[
            ("prompt_ms", &d["prompt_ms"]),
            ("generation_ms", &d["generation_ms"]),
        ]);
        if !timing.is_empty() {
            metrics.insert("extra".into(), Value::Object(timing));
        }
        if !metrics.is_empty() {
            step["metrics"] = Value::Object(metrics);
        }
        let mut extra = present(&[
            ("purpose", &d["purpose"]),
            ("outcome", &d["outcome"]),
            ("finish", &d["finish"]),
            ("seed", &d["seed"]),
            ("prompt_hash", &d["prompt_hash"]),
            ("ms", &d["ms"]),
        ]);
        extra.insert("turn".into(), json!(turn));
        if d["rejected_calls"]
            .as_array()
            .is_some_and(|c| !c.is_empty())
        {
            extra.insert("rejected_calls".into(), d["rejected_calls"].clone());
        }
        step["extra"] = Value::Object(extra);
        step
    }

    /// Adds an item to a list in the `extra` of the current agent step.
    fn attach(&mut self, list: &str, item: Value) {
        let Some(step) = self.agent.and_then(|i| self.steps.get_mut(i)) else {
            return;
        };
        match step["extra"][list].as_array_mut() {
            Some(items) => items.push(item),
            None => step["extra"][list] = json!([item]),
        }
    }
}

/// The fields that have a value.
fn present(fields: &[(&str, &Value)]) -> Map<String, Value> {
    fields
        .iter()
        .filter(|(_, v)| !v.is_null())
        .map(|(k, v)| ((*k).to_owned(), (*v).clone()))
        .collect()
}

/// A call's arguments as ATIF wants them, an object: the model's JSON when it is one.
fn arguments(raw: &str) -> Value {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(m)) => Value::Object(m),
        Ok(other) => json!({"value": other}),
        Err(_) => json!({"raw": raw}),
    }
}

/// Unix milliseconds as `2026-11-02T14:03:22.123Z`.
fn iso8601(ms: i64) -> String {
    let (secs, milli) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // the civil date of a day count (Howard Hinnant, "chrono-Compatible Low-Level Date
    // Algorithms")
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_utc_in_iso_8601() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(iso8601(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso8601(1_798_761_599_999), "2026-12-31T23:59:59.999Z");
    }

    #[test]
    fn arguments_are_always_an_object() {
        assert_eq!(arguments(r#"{"path":"a"}"#), json!({"path": "a"}));
        assert_eq!(arguments("[1]"), json!({"value": [1]}));
        assert_eq!(arguments("{oops"), json!({"raw": "{oops"}));
    }
}
