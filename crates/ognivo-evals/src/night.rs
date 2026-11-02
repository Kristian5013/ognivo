//! The morning report of a night (`scripts/night.sh`): one T2-mini run per model, each in
//! its own directory of the night (`<night>/<model>/summary.json` and `results.jsonl`),
//! next to `night.json`, what the script ran and how it went. The report puts the models
//! side by side, says how each moved since the night before, and lists the scenarios that
//! failed at least once.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::Deserialize;

use crate::t2::{Rate, RunResult, Summary};

/// What `scripts/night.sh` writes about the night.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Night {
    pub date: String,
    /// The commit the night ran.
    pub commit: String,
    pub window: String,
    pub seeds: String,
    pub models: Vec<String>,
    /// What went wrong outside the runs: a model skipped, a server that did not start.
    pub notes: Vec<String>,
}

/// One model of a night.
struct Model {
    summary: Summary,
    results: Vec<RunResult>,
}

fn load(dir: &Path) -> Result<(Night, BTreeMap<String, Model>), String> {
    let night: Night = match std::fs::read_to_string(dir.join("night.json")) {
        Ok(t) => {
            serde_json::from_str(&t).map_err(|e| format!("{}/night.json: {e}", dir.display()))?
        }
        Err(_) => Night::default(),
    };
    let mut models = BTreeMap::new();
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| e.to_string())?.path();
        let Ok(text) = std::fs::read_to_string(path.join("summary.json")) else {
            continue;
        };
        let summary: Summary = serde_json::from_str(&text)
            .map_err(|e| format!("{}/summary.json: {e}", path.display()))?;
        let results = std::fs::read_to_string(path.join("results.jsonl"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        models.insert(summary.profile.clone(), Model { summary, results });
    }
    Ok((night, models))
}

fn pct(r: &Rate) -> String {
    match (r.value, r.low, r.high) {
        (Some(v), Some(l), Some(h)) => {
            format!("{:.1}% [{:.1}–{:.1}]", v * 100.0, l * 100.0, h * 100.0)
        }
        _ => "—".into(),
    }
}

/// A share in percent, or nothing when nothing was counted.
fn share(r: &Rate) -> Option<f64> {
    r.value.map(|v| v * 100.0)
}

fn passed(s: &Summary) -> Option<f64> {
    (s.runs > 0).then(|| s.passed as f64 * 100.0 / s.runs as f64)
}

fn moved(before: Option<f64>, now: Option<f64>) -> String {
    match (before, now) {
        (Some(b), Some(n)) => format!("{b:.1}% → {n:.1}% ({:+.1})", n - b),
        (None, Some(n)) => format!("{n:.1}%"),
        _ => "—".into(),
    }
}

/// The report of the night in `dir` as Markdown, compared with the night in `previous`.
pub fn report(dir: &Path, previous: Option<&Path>) -> Result<String, String> {
    let (night, models) = load(dir)?;
    let mut out = String::new();
    let date = if night.date.is_empty() {
        dir.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        night.date.clone()
    };
    let _ = writeln!(out, "# ognivo night {date}\n");
    let build = models
        .values()
        .find_map(|m| m.summary.server["build"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "?".into());
    let _ = writeln!(
        out,
        "ognivo {} · window {} · seeds {} · llama.cpp {build}\n",
        if night.commit.is_empty() {
            "?"
        } else {
            &night.commit[..night.commit.len().min(12)]
        },
        if night.window.is_empty() {
            "?"
        } else {
            &night.window
        },
        if night.seeds.is_empty() {
            "?"
        } else {
            &night.seeds
        },
    );
    for note in &night.notes {
        let _ = writeln!(out, "- {note}");
    }
    if !night.notes.is_empty() {
        out.push('\n');
    }
    let missing: Vec<&String> = night
        .models
        .iter()
        .filter(|m| !models.contains_key(*m))
        .collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|m| m.as_str()).collect();
        let _ = writeln!(out, "No results for {}.\n", names.join(", "));
    }
    if models.is_empty() {
        out.push_str("Nothing ran.\n");
        return Ok(out);
    }
    out.push_str("| model | runs | passed | parse_ok | schema_ok | edit first try | truncated calls run | cache hit | min |\n");
    out.push_str("|---|---|---|---|---|---|---|---|---|\n");
    for (id, m) in &models {
        let s = &m.summary;
        let _ = writeln!(
            out,
            "| {id} | {} | {} ({}) | {} | {} | {} | {} | {} | {:.0} |",
            s.runs,
            s.passed,
            passed(s).map_or("—".into(), |p| format!("{p:.0}%")),
            pct(&s.parse_ok),
            pct(&s.schema_ok),
            pct(&s.edit_first_try),
            s.counts.truncated_executed,
            pct(&s.cache_hit),
            s.minutes
        );
    }

    if let Some(prev) = previous
        && let Ok((before_night, before)) = load(prev)
        && !before.is_empty()
    {
        let when = if before_night.date.is_empty() {
            prev.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            before_night.date
        };
        let _ = writeln!(out, "\nSince {when}:\n");
        out.push_str(
            "| model | passed | parse_ok | edit first try | cache hit |\n|---|---|---|---|---|\n",
        );
        for (id, m) in &models {
            let Some(b) = before.get(id) else {
                let _ = writeln!(out, "| {id} | new | | | |");
                continue;
            };
            let (b, n) = (&b.summary, &m.summary);
            let _ = writeln!(
                out,
                "| {id} | {} | {} | {} | {} |",
                moved(passed(b), passed(n)),
                moved(share(&b.parse_ok), share(&n.parse_ok)),
                moved(share(&b.edit_first_try), share(&n.edit_first_try)),
                moved(share(&b.cache_hit), share(&n.cache_hit)),
            );
        }
    }

    // scenario → model → (passed, runs)
    let mut by: BTreeMap<&str, BTreeMap<&str, [usize; 2]>> = BTreeMap::new();
    for (id, m) in &models {
        for r in &m.results {
            let c = by.entry(&r.scenario).or_default().entry(id).or_default();
            c[0] += usize::from(r.passed);
            c[1] += 1;
        }
    }
    by.retain(|_, per| per.values().any(|[p, n]| p < n));
    if !by.is_empty() {
        let ids: Vec<&str> = models.keys().map(String::as_str).collect();
        let _ = writeln!(
            out,
            "\nScenarios that failed at least once (passed/runs):\n"
        );
        let _ = writeln!(out, "| scenario | {} |", ids.join(" | "));
        let _ = writeln!(out, "|---|{}", "---|".repeat(ids.len()));
        for (scenario, per) in &by {
            let cells: Vec<String> = ids
                .iter()
                .map(|id| per.get(id).map_or("—".into(), |[p, n]| format!("{p}/{n}")))
                .collect();
            let _ = writeln!(out, "| {scenario} | {} |", cells.join(" | "));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::{Category, Counts, summarize};
    use ognivo_core::agent::Stats;
    use serde_json::json;

    fn result(scenario: &str, seed: u32, passed: bool, edits: (u32, u32)) -> RunResult {
        RunResult {
            scenario: scenario.into(),
            category: Category::Edit,
            seed,
            passed,
            failures: if passed { vec![] } else { vec!["x".into()] },
            end: "answer".into(),
            answer: None,
            tools: vec![],
            refused: vec![],
            counts: Counts {
                attempts: 4,
                usable: 4,
                ..Counts::default()
            },
            stats: Stats {
                calls: 3,
                edits_applied: edits.0,
                edits_failed: edits.1,
                prompt_tokens_cached: 900,
                prompt_tokens_processed: 100,
                ..Stats::default()
            },
            ms: 60_000,
            session: None,
        }
    }

    fn write(dir: &Path, model: &str, results: &[RunResult]) {
        let d = dir.join(model);
        std::fs::create_dir_all(&d).unwrap();
        let s = summarize(
            model,
            json!({"build": "b11924-12cc59ba4"}),
            &[1, 2],
            results,
        );
        std::fs::write(d.join("summary.json"), serde_json::to_string(&s).unwrap()).unwrap();
        let lines: String = results
            .iter()
            .map(|r| serde_json::to_string(r).unwrap() + "\n")
            .collect();
        std::fs::write(d.join("results.jsonl"), lines).unwrap();
    }

    #[test]
    fn the_report_compares_models_and_nights() {
        let root = tempfile::tempdir().unwrap();
        let (before, now) = (
            root.path().join("2026-11-03"),
            root.path().join("2026-11-04"),
        );
        write(
            &before,
            "gpt-oss-20b",
            &[result("a", 1, true, (1, 1)), result("b", 1, false, (0, 0))],
        );
        write(
            &now,
            "gpt-oss-20b",
            &[result("a", 1, true, (1, 0)), result("b", 1, true, (0, 0))],
        );
        write(
            &now,
            "qwen3.8-27b",
            &[result("a", 1, false, (0, 1)), result("b", 1, true, (0, 0))],
        );
        let night = json!({"date": "2026-11-04", "commit": "66b1de5aabbccdd", "window": "03:00-06:00",
            "seeds": "1,2", "models": ["gpt-oss-20b", "gemma-4-26b-a4b", "qwen3.8-27b"],
            "notes": ["gemma-4-26b-a4b: llama-server did not start"]});
        std::fs::write(now.join("night.json"), night.to_string()).unwrap();

        let r = report(&now, Some(&before)).unwrap();
        assert!(r.starts_with("# ognivo night 2026-11-04\n\nognivo 66b1de5aabbc · window 03:00-06:00 · seeds 1,2 · llama.cpp b11924-12cc59ba4\n"), "{r}");
        assert!(
            r.contains("- gemma-4-26b-a4b: llama-server did not start\n"),
            "{r}"
        );
        assert!(r.contains("No results for gemma-4-26b-a4b."), "{r}");
        assert!(
            r.contains("| gpt-oss-20b | 2 | 2 (100%) | 100.0% [67.6–100.0] |"),
            "{r}"
        );
        assert!(r.contains("| gpt-oss-20b | 50.0% → 100.0% (+50.0) | 100.0% → 100.0% (+0.0) | 50.0% → 100.0% (+50.0) |"), "{r}");
        assert!(r.contains("| qwen3.8-27b | new |"), "{r}");
        assert!(r.contains("| a | 1/1 | 0/1 |"), "{r}");
        assert!(!r.contains("| b |"), "{r}");
    }
}
