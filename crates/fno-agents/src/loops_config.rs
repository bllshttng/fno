//! `fno-agents loops ls` and `fno-agents loops level <name>`: the loop-level
//! config reads the deleted `cli/src/fno/loops.py` answered. The
//! pause verbs live in [crate::loops_pause]; these two reads need the
//! settings stack and the project journal, so they live beside it in a
//! module named for the question they answer.
//!
//! Substrate only: no standing loop ships here. Every later loop is born
//! with a pause button by reading `loops paused` at tick start (paused = log
//! one line, exit 0) and its configured autonomy via `loops level`.

use std::path::PathBuf;

use serde_json::Value;

use crate::backlog::advance_settings::load_merged;
use crate::paths::space_dir;

/// The configured level for loop `name`. Never fails: an unconfigured or
/// unrecognized name always falls back to "report" (observe only), the
/// safest default for a loop that has not graduated yet.
fn loop_level(settings: &Value, name: &str) -> String {
    settings
        .get("loops")
        .and_then(|l| l.get(name))
        .and_then(|e| e.get("level"))
        .and_then(Value::as_str)
        .unwrap_or("report")
        .to_string()
}

/// Best-effort: the most recent `loop_tick` event timestamp for `name`.
/// A read/parse failure degrades to None rather than raising (ls is a
/// read-only status view, never a gate).
fn last_tick(events_path: &std::path::Path, name: &str) -> Option<String> {
    let text = std::fs::read_to_string(events_path).ok()?;
    let mut last: Option<String> = None;
    for line in text.lines() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("type").and_then(Value::as_str) != Some("loop_tick") {
            continue;
        }
        let matches = event
            .get("data")
            .and_then(|d| d.get("name"))
            .and_then(Value::as_str)
            == Some(name);
        if !matches {
            continue;
        }
        if let Some(ts) = event.get("ts").and_then(Value::as_str) {
            if last.as_ref().is_none_or(|cur| ts > cur.as_str()) {
                last = Some(ts.to_string());
            }
        }
    }
    last
}

/// The project journal the Python `paths.project_log("events.jsonl")`
/// resolved: the space dir keyed on the canonical root, so any subdirectory
/// or worktree lands on the same file.
fn events_path_from_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    Some(space_dir(&cwd).join("events.jsonl"))
}

/// `fno-agents loops ls [names...]`: configured loops, their level, and
/// last-tick timestamp; restricted to the named loops when any are given.
pub fn run(args: &[String]) -> i32 {
    let settings = load_merged(None);
    let all: Vec<String> = settings
        .get("loops")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    if args.is_empty() && all.is_empty() {
        println!("no loops configured");
        return 0;
    }
    let mut names: Vec<String> = if args.is_empty() { all } else { args.to_vec() };
    names.sort();
    let events_path = events_path_from_cwd();
    for name in &names {
        let level = loop_level(&settings, name);
        let tick = events_path
            .as_deref()
            .and_then(|p| last_tick(p, name))
            .unwrap_or_else(|| "never".to_string());
        println!("{name}\t{level}\t{tick}");
    }
    0
}

/// `fno-agents loops level <name>`: one level, one line, JSON-encoded - the
/// observer harness crosses through `command_call`, which JSON-decodes the
/// stdout, so a bare `assisted` would raise and fall back to "report".
pub fn run_level(args: &[String]) -> i32 {
    let Some(name) = args.first() else {
        eprintln!("usage: fno-agents loops level <name>");
        return 2;
    };
    let settings = load_merged(None);
    println!("{}", serde_json::json!(loop_level(&settings, name)));
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn level_defaults_to_report_on_absent_and_unrecognized() {
        let settings = json!({"loops": {"mine": {"level": "assisted"}, "weird": {}}});
        assert_eq!(loop_level(&settings, "mine"), "assisted");
        assert_eq!(loop_level(&settings, "weird"), "report");
        assert_eq!(loop_level(&settings, "absent"), "report");
    }

    #[test]
    fn last_tick_reads_the_latest_matching_event_only() {
        let dir = std::env::temp_dir().join(format!("fno-loops-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        std::fs::write(
            &path,
            "not json\n{\"type\":\"loop_tick\",\"ts\":\"2026-01-01T00:00:00Z\",\"data\":{\"name\":\"a\"}}\n\
             {\"type\":\"loop_tick\",\"ts\":\"2026-01-02T00:00:00Z\",\"data\":{\"name\":\"a\"}}\n\
             {\"type\":\"loop_tick\",\"ts\":\"2026-01-03T00:00:00Z\",\"data\":{\"name\":\"b\"}}\n",
        )
        .unwrap();
        assert_eq!(
            last_tick(&path, "a").as_deref(),
            Some("2026-01-02T00:00:00Z")
        );
        assert_eq!(last_tick(&path, "absent"), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
