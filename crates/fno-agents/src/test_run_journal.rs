//! Durable `test_run` rows for the `fno doctor test` door, and the reader
//! that turns them into a median and p90 per normalized command.
//!
//! `run_test_run` is the one place that knows a run's exit code and
//! duration, so it is the one writer: a `started` row when the child spawns
//! and a `finished` row when it ends, joined by `run_id`. A refused run
//! spawns nothing and writes nothing. A journal write that fails never
//! changes the run's exit code.

use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use crate::events::EventEmitter;

const COMMAND_CAP: usize = 400;

pub(crate) struct Run<'a> {
    pub(crate) run_id: &'a str,
    pub(crate) argv: &'a [String],
    pub(crate) whole: bool,
}

fn events_path() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::state_path::resolve("events", &cwd).unwrap_or_else(|| crate::paths::events_path(&cwd))
}

fn basename(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// The grouping key for a command: the tool, its crate or selector when it
/// has one, and `(whole)` when no target narrowed the run.
pub(crate) fn command_key(argv: &[String], whole: bool) -> String {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let base = if words.iter().any(|w| basename(w).starts_with("pytest")) {
        "pytest".to_string()
    } else if words.first().map(|w| basename(w)) == Some("cargo") && words.contains(&"test") {
        let crate_name = words
            .iter()
            .position(|w| *w == "--manifest-path")
            .and_then(|i| words.get(i + 1))
            .and_then(|p| p.rsplit('/').nth(1));
        let selector = ["--lib", "--test", "--bin", "--doc"]
            .into_iter()
            .find(|s| words.contains(s));
        let mut parts = vec!["cargo test".to_string()];
        parts.extend(crate_name.map(str::to_string));
        parts.extend(selector.map(str::to_string));
        parts.join(" ")
    } else {
        words
            .first()
            .map(|w| basename(w).to_string())
            .unwrap_or_default()
    };
    if whole {
        format!("{base} (whole)")
    } else {
        base
    }
}

fn append(run: &Run<'_>, stage: &str, extra: Value) {
    let mut data = json!({
        "run_id": run.run_id,
        "stage": stage,
        "command": run.argv.join(" ").chars().take(COMMAND_CAP).collect::<String>(),
        "command_key": command_key(run.argv, run.whole),
    });
    let present = [
        ("node_id", std::env::var("FNO_NODE").ok()),
        ("session_id", std::env::var("FNO_SESSION_ID").ok()),
        (
            "worktree",
            std::env::current_dir()
                .ok()
                .map(|c| c.display().to_string()),
        ),
    ];
    for (field, value) in present {
        if let Some(v) = value.filter(|v| !v.is_empty()) {
            data[field] = json!(v);
        }
    }
    if let (Some(fields), Some(extra)) = (data.as_object_mut(), extra.as_object()) {
        fields.extend(extra.clone());
    }
    let _ = EventEmitter::new(events_path(), "agents").emit("test_run", &data);
}

pub(crate) fn record_started(run: &Run<'_>, pid: u32) {
    append(run, "started", json!({ "pid": pid }));
}

pub(crate) fn record_finished(run: &Run<'_>, exit_code: i32, elapsed: Duration) {
    let ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    append(
        run,
        "finished",
        json!({ "exit_code": exit_code, "duration_ms": ms }),
    );
}

/// Linear-interpolated percentile of an ascending list.
fn percentile(sorted: &[u64], p: f64) -> f64 {
    let rank = p * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    sorted[lo] as f64 + (sorted[hi] as f64 - sorted[lo] as f64) * (rank - lo as f64)
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Summary {
    pub(crate) durations_ms: BTreeMap<String, Vec<u64>>,
    pub(crate) unfinished: usize,
}

/// Fold `test_run` journal lines into per-key durations. A started row with
/// no finished row counts as unfinished and never as a duration.
pub(crate) fn summarize(text: &str) -> Summary {
    let mut started: HashSet<String> = HashSet::new();
    let mut finished: HashMap<String, (String, u64)> = HashMap::new();
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("test_run") {
            continue;
        }
        let data = &event["data"];
        let Some(run_id) = data.get("run_id").and_then(Value::as_str) else {
            continue;
        };
        match data.get("stage").and_then(Value::as_str) {
            Some("started") => {
                started.insert(run_id.to_string());
            }
            Some("finished") => {
                let key = data.get("command_key").and_then(Value::as_str);
                let ms = data.get("duration_ms").and_then(Value::as_u64);
                if let (Some(key), Some(ms)) = (key, ms) {
                    finished.insert(run_id.to_string(), (key.to_string(), ms));
                }
            }
            _ => {}
        }
    }
    let mut summary = Summary {
        unfinished: started
            .iter()
            .filter(|id| !finished.contains_key(*id))
            .count(),
        ..Summary::default()
    };
    for (key, ms) in finished.into_values() {
        summary.durations_ms.entry(key).or_default().push(ms);
    }
    summary
}

pub(crate) fn render(summary: &Summary) -> String {
    let mut out = String::new();
    for (key, ms) in &summary.durations_ms {
        let mut sorted = ms.clone();
        sorted.sort_unstable();
        out.push_str(&format!(
            "{key}  n={}  median={:.1}s  p90={:.1}s\n",
            sorted.len(),
            percentile(&sorted, 0.5) / 1000.0,
            percentile(&sorted, 0.9) / 1000.0,
        ));
    }
    out.push_str(&format!("unfinished: {}\n", summary.unfinished));
    out
}

/// `test-run stats`: median and p90 per command key.
pub(crate) fn run_stats(args: &[String]) -> i32 {
    if let Some(extra) = args.first() {
        eprintln!("fno-agents test-run stats: unknown argument {extra}");
        return 2;
    }
    let text = crate::event_store::journal_text(&events_path(), &["test_run"]);
    print!("{}", render(&summarize(&text)));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn line(run_id: &str, stage: &str, key: &str, ms: Option<u64>) -> String {
        let mut data = json!({"run_id": run_id, "stage": stage, "command_key": key});
        if let Some(ms) = ms {
            data["duration_ms"] = json!(ms);
        }
        json!({"ts": "2026-10-22T10:00:00Z", "type": "test_run", "source": "agents", "data": data})
            .to_string()
    }

    #[test]
    fn command_key_groups_by_tool_crate_and_selector() {
        assert_eq!(
            command_key(&argv(&["python3", "-m", "pytest", "a.py"]), false),
            "pytest"
        );
        assert_eq!(
            command_key(
                &argv(&[
                    "cargo",
                    "test",
                    "--manifest-path",
                    "crates/fno-agents/Cargo.toml",
                    "--lib",
                    "x::"
                ]),
                false
            ),
            "cargo test fno-agents --lib"
        );
        assert_eq!(
            command_key(
                &argv(&["cargo", "test", "--manifest-path", "crates/fno/Cargo.toml"]),
                true
            ),
            "cargo test fno (whole)"
        );
    }

    #[test]
    fn stats_print_median_and_p90_for_ten_runs() {
        let text: String = (1..=10)
            .map(|n| line(&format!("r{n}"), "finished", "pytest", Some(n * 1000)))
            .collect::<Vec<_>>()
            .join("\n");
        let out = render(&summarize(&text));
        assert!(out.contains("pytest  n=10  median=5.5s  p90=9.1s"), "{out}");
    }

    #[test]
    fn started_row_with_no_finished_row_is_unfinished_not_a_duration() {
        let text = [
            line("a", "started", "pytest", None),
            line("b", "started", "pytest", None),
            line("b", "finished", "pytest", Some(2000)),
        ]
        .join("\n");
        let summary = summarize(&text);
        assert_eq!(summary.unfinished, 1);
        assert_eq!(summary.durations_ms["pytest"], vec![2000]);
    }
}
