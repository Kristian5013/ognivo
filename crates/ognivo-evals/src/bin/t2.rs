//! T2-mini against a running llama-server:
//!
//!   t2 --url URL --profile ID --out DIR [--seeds 1,2,3] [--only TEXT]
//!
//! Runs every scenario of `evals/t2/scenarios` (or those whose id or category holds TEXT)
//! with each seed, one line per run, then the summary. DIR keeps the results
//! (`results.jsonl`, one run per line), the summary (`summary.json`), the sessions
//! (`sessions.db`), their trajectories (`traces/`) and the projects of failed runs
//! (`work/`). Runs already in `results.jsonl` are not run again, so an interrupted suite
//! goes on where it stopped.

use std::collections::HashSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use ognivo_evals::t2::{self, Rate, RunResult, Summary, Target};
use ognivo_models::llama::LlamaServer;
use ognivo_models::{profile, template};
use serde_json::json;

struct Args {
    url: String,
    profile: String,
    out: PathBuf,
    seeds: Vec<u32>,
    only: Option<String>,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args().skip(1);
    let (mut url, mut profile, mut out, mut seeds, mut only) = (None, None, None, vec![1], None);
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or(format!("{flag} needs a value"));
        match flag.as_str() {
            "--url" => url = Some(value()?),
            "--profile" => profile = Some(value()?),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--only" => only = Some(value()?),
            "--seeds" => {
                seeds = value()?
                    .split(',')
                    .map(|s| {
                        s.trim()
                            .parse::<u32>()
                            .map_err(|e| format!("--seeds: {s}: {e}"))
                    })
                    .collect::<Result<_, _>>()?;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let need = |v: Option<String>, name: &str| v.ok_or(format!("{name} is required"));
    Ok(Args {
        url: need(url, "--url")?,
        profile: need(profile, "--profile")?,
        out: out.ok_or("--out is required")?,
        seeds,
        only,
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    let a = match args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!(
                "t2: {e}\nusage: t2 --url URL --profile ID --out DIR [--seeds 1,2,3] [--only TEXT]"
            );
            return ExitCode::from(2);
        }
    };
    match suite(&a).await {
        Ok(s) if s.failed.is_empty() => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("t2: {e}");
            ExitCode::from(2)
        }
    }
}

async fn suite(a: &Args) -> Result<Summary, String> {
    let p = profile::builtin()
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|p| p.id == a.profile)
        .ok_or(format!("no built-in profile {}", a.profile))?;
    let server = LlamaServer::new(&a.url, None).map_err(|e| e.to_string())?;
    let props = server
        .props()
        .await
        .map_err(|e| format!("the server at {} does not answer: {e}", a.url))?;
    // the model's file name only: its directory is the owner's
    let model = props
        .model_path
        .as_deref()
        .and_then(|m| Path::new(m).file_name())
        .map(|n| n.to_string_lossy().into_owned());
    let template = props.chat_template.as_deref().map(|t| {
        let sha = template::sha256_hex(t);
        p.template.lookup(&sha).map_or_else(
            || format!("unknown {}", &sha[..12]),
            |k| format!("{:?}", k.verdict).to_lowercase(),
        )
    });
    let info = json!({"build": props.build_info, "model": model, "template": template, "n_ctx": props.n_ctx});
    println!(
        "{} · {} · {} · template {} · n_ctx {}",
        p.id,
        props.build_info.as_deref().unwrap_or("?"),
        model.as_deref().unwrap_or("?"),
        template.as_deref().unwrap_or("?"),
        props.n_ctx.map_or("?".into(), |n| n.to_string())
    );

    let scenarios: Vec<_> = t2::load(&t2::scenarios_dir())?
        .into_iter()
        .filter(|s| {
            a.only
                .as_deref()
                .is_none_or(|o| s.id.contains(o) || s.category.name() == o)
        })
        .collect();
    std::fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let log = a.out.join("results.jsonl");
    let mut results: Vec<RunResult> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let done: HashSet<(String, u32)> = results
        .iter()
        .map(|r| (r.scenario.clone(), r.seed))
        .collect();
    let target = Target {
        url: a.url.clone(),
        profile: p.clone(),
        n_ctx: props.n_ctx,
    };
    for s in &scenarios {
        for &seed in &a.seeds {
            if done.contains(&(s.id.clone(), seed)) {
                continue;
            }
            let r = t2::run(s, &target, seed, &a.out).await?;
            let why = r.failures.first().cloned().unwrap_or_default();
            println!(
                "  {}  {:<32} {:>6.1} s  {}",
                if r.passed { "pass" } else { "FAIL" },
                format!("{}.{seed}", s.id),
                r.ms as f64 / 1000.0,
                if r.passed { r.tools.join(" ") } else { why }
            );
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log)
                .map_err(|e| format!("{}: {e}", log.display()))?;
            writeln!(f, "{}", serde_json::to_string(&r).unwrap_or_default())
                .map_err(|e| e.to_string())?;
            results.push(r);
        }
    }
    let wanted: HashSet<&str> = scenarios.iter().map(|s| s.id.as_str()).collect();
    results.retain(|r| wanted.contains(r.scenario.as_str()) && a.seeds.contains(&r.seed));
    let s = t2::summarize(&p.id, info, &a.seeds, &results);
    let text = serde_json::to_string_pretty(&s).unwrap_or_default() + "\n";
    std::fs::write(a.out.join("summary.json"), text).map_err(|e| e.to_string())?;
    print_summary(&s);
    Ok(s)
}

fn print_summary(s: &Summary) {
    let pct = |r: &Rate| match (r.value, r.low, r.high) {
        (Some(v), Some(l), Some(h)) => format!(
            "{:5.1}%  ({}/{}, 95% {:.1}–{:.1})",
            v * 100.0,
            r.ok,
            r.of,
            l * 100.0,
            h * 100.0
        ),
        _ => "  —".into(),
    };
    println!("{} runs, {} passed, {:.1} min", s.runs, s.passed, s.minutes);
    println!("  parse_ok        {}", pct(&s.parse_ok));
    println!("  schema_ok       {}", pct(&s.schema_ok));
    println!("  edit first try  {}", pct(&s.edit_first_try));
    println!("  cache hit       {}", pct(&s.cache_hit));
    let c = &s.counts;
    println!(
        "  truncated calls executed: {} · truncated {} · malformed {} · loops stopped {} · empty answers {} · format errors {} · repeated calls {}",
        c.truncated_executed,
        c.truncated,
        c.malformed,
        c.repeating,
        c.empty_answers,
        c.format_errors,
        s.repeated
    );
    let by: Vec<String> = s
        .categories
        .iter()
        .map(|(k, [p, n])| format!("{k} {p}/{n}"))
        .collect();
    println!("  by category: {}", by.join(" · "));
    if !s.failed.is_empty() {
        println!("  failed: {}", s.failed.join(" "));
    }
}
