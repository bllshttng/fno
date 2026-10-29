//! PostToolUse task boundary: a commit whose message carries a `Task <id>:`
//! line for a task the bound plan lists records that task's task_done at the
//! commit time, plus a task_started at the run's previous boundary when the
//! run has none (`derived: commit`). The flat path used to ask for these in
//! prose, and the prose was skipped. The shim runs this entry in the
//! background after the budget line, so a slow store never costs the model
//! that line. A failed write is dropped, and run_summary shows the gap.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The background entry: the commit payload arrives on stdin.
pub fn run(_args: &[String]) -> i32 {
    let payload = serde_json::from_str(super::read_stdin().trim()).unwrap_or(Value::Null);
    let _ = record(&payload);
    0
}

/// Record the boundary for this Bash call, if it committed a plan task.
fn record(payload: &Value) -> Option<()> {
    super::diff_budget::committed(payload).then_some(())?;
    let cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let manifest = super::diff_budget::manifest_path(&cwd)?;
    let text = std::fs::read_to_string(&manifest).ok()?;
    let field = |key: &str| manifest_field(&text, key);
    let (run, node) = (field("fno_id")?, field("graph_node_id")?);
    let plan = super::diff_budget::plan_path(&manifest, &cwd)?;
    let git = |args: &[&str]| super::diff_budget::git_out(&cwd, args);
    let task = commit_task(&git(&["log", "-1", "--format=%B"]), &plan)?;
    let events = crate::paths::events_path(&cwd);
    let commit = (
        git(&["rev-parse", "HEAD"]),
        git(&["log", "-1", "--format=%cI"]),
    );
    let ids = [run.as_str(), node.as_str(), task.as_str()];
    let _ = emit(&events, ids, &commit, field("created_at").as_deref());
    settle(&node, &task);
    Some(())
}

/// Settle the task row as the prose emit did.
fn settle(node: &str, task: &str) {
    let _ = Command::new(crate::loopcheck::loopcheck_fno_bin())
        .args(["backlog", "task", "update", node, task, "--status", "done"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn manifest_field(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix(':')?;
        let value = value.trim().trim_matches('"').trim();
        (!value.is_empty() && value != "null").then(|| value.to_string())
    })
}

/// The plan task a commit message names: the first `Task <id>:` line whose
/// id the plan lists.
fn commit_task(message: &str, plan: &Path) -> Option<String> {
    let tasks = crate::wave::plan_tasks(plan).ok()?;
    message.lines().find_map(|line| {
        let (id, _) = line.trim().strip_prefix("Task ")?.split_once(':')?;
        tasks
            .iter()
            .find(|t| t.id == id.trim())
            .map(|t| t.id.clone())
    })
}

/// Write the boundary rows for `[run, node, task]` and the commit's
/// `(sha, committed_at)`. A task this run already closed is left alone, so
/// an amend or a second commit for one task writes nothing new.
fn emit(
    events: &Path,
    [run, node, task]: [&str; 3],
    (sha, committed_at): &(String, String),
    run_started_at: Option<&str>,
) -> Result<(), String> {
    crate::event_store::import_all(events)?;
    let query = crate::event_store::EventQuery {
        types: vec!["task_started".into(), "task_done".into()],
        node_id: Some(node.to_string()),
        ..Default::default()
    };
    let mine: Vec<Value> = crate::event_store::query_events(events, &query)?
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .filter(|v| v.get("run").and_then(Value::as_str) == Some(run))
        .collect();
    let of = |kind: &'static str| {
        mine.iter()
            .filter(move |v| v.get("type").and_then(Value::as_str) == Some(kind))
    };
    let mine_task = |v: &&Value| v.get("task").and_then(Value::as_str) == Some(task);
    if of("task_done").any(|v| mine_task(&v)) {
        return Ok(());
    }
    let ts = normalize(committed_at).ok_or("commit time unreadable")?;
    let envelope = |kind: &str, ts: &str, data: Value| {
        json!({"ts": ts, "v": 1, "type": kind, "source": "target",
               "run": run, "node": node, "task": task, "data": data})
    };
    let append = |env: &Value| {
        crate::claims::append_event_line(events, env, std::time::Duration::from_secs(2))
    };
    if !of("task_started").any(|v| mine_task(&v)) {
        let previous = of("task_done")
            .filter_map(|v| v.get("ts").and_then(Value::as_str))
            .max()
            .map(str::to_string)
            .or_else(|| run_started_at.and_then(normalize))
            .unwrap_or_else(|| ts.clone());
        append(&envelope(
            "task_started",
            &previous,
            json!({"derived": "commit"}),
        ))?;
    }
    let mut done = envelope(
        "task_done",
        &ts,
        json!({"commit": sha, "derived": "commit"}),
    );
    done["outcome"] = json!("SUCCESS");
    append(&done)
}

/// An RFC 3339 time as the UTC `Z` stamp the journal writes.
fn normalize(ts: &str) -> Option<String> {
    let t = chrono::DateTime::parse_from_rfc3339(ts.trim()).ok()?;
    Some(
        t.with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_task_commits_land_two_starts_and_two_dones() {
        let dir = tempfile::tempdir().unwrap();
        let plan = dir.path().join("plan.md");
        std::fs::write(&plan, "## Changes\n\n### 1. First\n\n### 2. Second\n").unwrap();
        assert_eq!(commit_task("Task 2: Second", &plan).as_deref(), Some("2"));
        assert_eq!(commit_task("Task 9: absent", &plan), None);

        let (events, start) = (
            dir.path().join("events.jsonl"),
            Some("2026-10-28T09:00:00Z"),
        );
        for (task, at) in [("1", "2026-10-28T10:00:00Z"), ("2", "2026-10-28T11:00:00Z")] {
            let commit = ("abc".to_string(), at.to_string());
            for _ in 0..2 {
                emit(&events, ["r1", "x-1", task], &commit, start).unwrap();
            }
        }
        let query = crate::event_store::EventQuery {
            node_id: Some("x-1".into()),
            ..Default::default()
        };
        let rows: Vec<Value> = crate::event_store::query_events(&events, &query)
            .unwrap()
            .iter()
            .map(|r| serde_json::from_str(&r.line).unwrap())
            .collect();
        let of = |t: &str| rows.iter().filter(|v| v["type"] == t).collect::<Vec<_>>();
        assert_eq!((of("task_started").len(), of("task_done").len()), (2, 2));
        assert_eq!(of("task_started")[1]["ts"], "2026-10-28T10:00:00Z");
        // The waves lead reports task 2 again: run_summary still counts two.
        let lead = json!({"ts": "2026-10-28T11:01:00Z", "v": 1, "type": "task_done",
            "source": "target", "run": "r1", "node": "x-1", "task": "2",
            "outcome": "DONE_WITH_CONCERNS", "data": {}});
        crate::claims::append_event_line(&events, &lead, std::time::Duration::from_secs(2))
            .unwrap();
        let counts = crate::finalize_run_summary::count_run_tasks(&events, "r1");
        assert_eq!(counts, (2, 2, 0));
    }
}
