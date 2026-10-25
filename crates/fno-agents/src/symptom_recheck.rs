//! `fno-agents probe-run recheck`: re-run a closed node's symptom probe.
//!
//! A node closes on "PR merged, CI green". That proves the fix against the
//! inventory at merge, not that it still holds. A plan that names a lasting
//! symptom declares `recheck_probes:`, and this walk re-runs that list for
//! every done node on the daemon's cadence. A list that fails twice in one
//! pass files a bug node with `caused_by` naming the original.
//!
//! Not `close_probes`: a close probe is free to read a window (`--since 24h`)
//! or a surface that later retires. Measured 2026-10-25, 13 of 21 done nodes'
//! close probes failed for those reasons alone.
//!
//! Only a FAIL verdict counts. BLOCKED (the gh budget refused), SKIP and an
//! unreadable plan read `undeterminable` and file nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::paths::AgentsHome;

pub const RECHECK_KEY: &str = "recheck_probes";
pub const RECHECK_INTERVAL_S: u64 = 21_600;
/// Each probe can spend the runner's 60 s timeout twice, so a pass is bounded.
const NODE_CAP: usize = 40;

type Runner<'a> = &'a dyn Fn(&[String]) -> (i32, String);

pub struct Candidate {
    pub id: String,
    pub title: String,
    pub plan: String,
    pub cwd: String,
    /// The idea cap counts per epic, so a regression filed under the
    /// original's nearest epic gets that epic's room.
    pub epic: Option<String>,
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn nearest_epic(row: &Value, rows: &[Value]) -> Option<String> {
    let mut parent = text(row, "parent");
    for _ in 0..64 {
        let up = rows
            .iter()
            .find(|r| !parent.is_empty() && text(r, "id") == parent)?;
        if text(up, "type") == "epic" {
            return Some(parent.to_string());
        }
        parent = text(up, "parent");
    }
    None
}

/// Done rows whose plan file exists and declares the key.
pub fn candidates(rows: &[Value]) -> Vec<Candidate> {
    let marker = format!("\n{RECHECK_KEY}:");
    rows.iter()
        .filter(|r| text(r, "status") == "done")
        .filter_map(|r| {
            let plan = text(r, "plan_path").split('#').next().unwrap_or("");
            let body = std::fs::read_to_string(plan).ok()?;
            body.contains(&marker).then(|| Candidate {
                id: text(r, "id").to_string(),
                title: text(r, "title").to_string(),
                plan: plan.to_string(),
                cwd: text(r, "cwd").to_string(),
                epic: nearest_epic(r, rows),
            })
        })
        .collect()
}

fn one_run(c: &Candidate, run: Runner) -> (i32, bool, String) {
    let mut args: Vec<String> = ["--plan", &c.plan, "--key", RECHECK_KEY, "--json"]
        .map(String::from)
        .into();
    if !c.cwd.is_empty() {
        args.extend(["--cwd".to_string(), c.cwd.clone()]);
    }
    let (code, out) = run(&args);
    let body: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let fail = body["results"]
        .as_array()
        .is_some_and(|r| r.iter().any(|row| row["verdict"] == "FAIL"));
    let reason = body["reason"].as_str().unwrap_or("no reason").to_string();
    (code, code == 1 && fail, reason)
}

/// `pass`, `flaky` (failed, then passed on the retry), `regressed` or
/// `undeterminable`, with the runner's reason.
pub fn judge(c: &Candidate, run: Runner) -> (&'static str, String) {
    match one_run(c, run) {
        (0, _, _) => return ("pass", String::new()),
        (_, false, why) => return ("undeterminable", why),
        _ => {}
    }
    match one_run(c, run) {
        (0, _, why) => ("flaky", why),
        (_, true, why) => ("regressed", why),
        (_, false, why) => ("undeterminable", why),
    }
}

fn fno(args: &[&str]) -> Result<std::process::Output, String> {
    std::process::Command::new(crate::scrape::fno_bin())
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("fno {}: {e}", args[..2].join(" ")))
}

/// The filing door. An unattended `idea` needs `--separate`, or a fold
/// candidate answers `choice_required` and mints nothing.
pub fn file_regression(c: &Candidate, reason: &str) -> Result<String, String> {
    let title = format!("regression: {}", c.title);
    let details = format!(
        "The recheck_probes of done node {} failed twice in one pass.\n\nplan: {}\nprobe: {reason}",
        c.id, c.plan
    );
    let fixed = "--separate --difficulty medium --type bug --source-kind from_observation --json";
    let mut args = vec![
        "backlog",
        "idea",
        &title,
        "--related",
        &c.id,
        "--source-node",
        &c.id,
    ];
    args.extend(["--origin-evidence", &c.plan, "--details", &details]);
    args.extend(fixed.split(' '));
    if !c.cwd.is_empty() {
        args.extend(["--cwd", &c.cwd]);
    }
    if let Some(epic) = &c.epic {
        args.extend(["--parent", epic]);
    }
    let out = fno(&args)?;
    let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let Some(id) = body["id"].as_str() else {
        return Err(format!(
            "fno backlog idea minted nothing (exit {}): {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    };
    if !fno(&["backlog", "update", id, &format!("caused_by={}", c.id)])?
        .status
        .success()
    {
        return Err(format!("filed {id} but caused_by did not land"));
    }
    Ok(id.to_string())
}

/// One pass. Probes and filing are parameters so tests need no shell and no
/// live graph. An open node whose `caused_by` names the original stops a
/// second filing. A refused filing (the idea cap, most often) stays on the
/// row as `file_error` and counts as `unfiled`.
pub fn walk(
    rows: &[Value],
    apply: bool,
    run: Runner,
    file: &mut dyn FnMut(&Candidate, &str) -> Result<String, String>,
) -> Value {
    let open: std::collections::HashMap<&str, &str> = rows
        .iter()
        .filter(|r| !matches!(text(r, "status"), "done" | "superseded"))
        .filter_map(|r| {
            // source_node_id lands at birth, caused_by one call later: a
            // filing whose second call failed must still stop a refile.
            let orig = match text(r, "caused_by") {
                "" if text(r, "title").starts_with("regression: ") => text(r, "source_node_id"),
                linked => linked,
            };
            (!orig.is_empty()).then(|| (orig, text(r, "id")))
        })
        .collect();
    let all = candidates(rows);
    let mut results = Vec::new();
    for c in all.iter().take(NODE_CAP) {
        let (verdict, reason) = judge(c, run);
        let mut row = json!({"node": c.id, "verdict": verdict, "reason": reason});
        if verdict == "regressed" {
            match open.get(c.id.as_str()) {
                Some(existing) => row["regression"] = json!(existing),
                None if apply => match file(c, &reason) {
                    Ok(id) => row["filed"] = json!(id),
                    Err(e) => row["file_error"] = json!(e),
                },
                None => {}
            }
        }
        results.push(row);
    }
    let count = |k: &str, v: &str| results.iter().filter(|r| r[k] == v).count();
    let has = |k: &str| results.iter().filter(|r| !r[k].is_null()).count();
    json!({
        "key": RECHECK_KEY,
        "apply": apply,
        "checked": results.len(),
        "passed": count("verdict", "pass") + count("verdict", "flaky"),
        "failed": count("verdict", "regressed"),
        "undeterminable": count("verdict", "undeterminable"),
        "filed": has("filed"),
        "unfiled": has("file_error"),
        "over_cap": all.len().saturating_sub(NODE_CAP),
        "results": results,
    })
}

fn pass(apply: bool) -> (i32, Value) {
    let graph = crate::backlog::settings::graph_path();
    let rows = match crate::graph_store::read_rows(&graph) {
        Ok(rows) => rows,
        Err(e) => return (2, json!({"error": format!("graph unreadable: {e}")})),
    };
    let decide = crate::acceptance_evidence::decide_probe_run;
    let body = walk(&rows, apply, &decide, &mut file_regression);
    (i32::from(body["failed"] != 0), body)
}

/// `fno-agents probe-run recheck [--apply] [--json]`. Without `--apply` it
/// files nothing. Exit 0 when nothing regressed, 1 when something did, 2 when
/// the graph is unreadable. stdout is always one JSON object.
pub fn run(args: &[String]) -> i32 {
    let (code, body) = pass(args.iter().any(|a| a == "--apply"));
    println!("{body}");
    code
}

/// The daemon arm: a cadence stamp plus a one-in-flight gate.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(RECHECK_INTERVAL_S))
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let (_, body) = pass(true);
        let n = |k: &str| body[k].as_u64().unwrap_or(0);
        let detail = body["error"]
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| {
                format!(
                    "checked {} regressed {} undeterminable {} filed {} unfiled {} over_cap {}",
                    n("checked"),
                    n("failed"),
                    n("undeterminable"),
                    n("filed"),
                    n("unfiled"),
                    n("over_cap")
                )
            });
        let skip = body["error"].is_string().then_some("error");
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        crate::tick_ledger::emit_tick(
            &journal,
            "symptom_recheck",
            crate::tick_ledger::SCHED_DAEMON,
            n("filed"),
            skip,
            Some(&detail),
            RECHECK_INTERVAL_S,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acceptance_evidence::decide_probe_run;

    fn done(dir: &std::path::Path, id: &str, probes: &str) -> Value {
        let plan = dir.join(format!("{id}.md"));
        std::fs::write(&plan, format!("---\ntitle: t\n{probes}\n---\n")).unwrap();
        json!({"id": id, "title": id, "status": "done", "plan_path": plan, "cwd": ""})
    }

    #[test]
    fn symptom_recheck_files_one_regression_and_reads_only_its_key() {
        let tmp = tempfile::tempdir().unwrap();
        let mut open = done(tmp.path(), "x-o", "recheck_probes:\n  - \"exit 1\"");
        open["status"] = json!("in_review");
        let mut rows = vec![
            done(tmp.path(), "x-p", "recheck_probes:\n  - \"echo ok\""),
            done(tmp.path(), "x-c", "close_probes:\n  - \"exit 1\""),
            done(tmp.path(), "x-f", "recheck_probes:\n  - \"exit 3\""),
            open,
        ];
        let mut filed = Vec::new();
        let mut file = |c: &Candidate, _: &str| {
            filed.push(c.id.clone());
            Ok("x-new".to_string())
        };
        let body = walk(&rows, true, &decide_probe_run, &mut file);
        assert_eq!(
            (body["checked"].as_u64(), body["passed"].as_u64()),
            (Some(2), Some(1))
        );
        assert_eq!(body["results"][1]["verdict"], "regressed");
        assert_eq!(filed, vec!["x-f".to_string()]);

        // The filed node is open and names the original: the next pass skips it.
        rows.push(json!({"id": "x-new", "status": "idea", "caused_by": "x-f"}));
        let mut never = |_: &Candidate, _: &str| -> Result<String, String> { panic!("refiled") };
        let body = walk(&rows, true, &decide_probe_run, &mut never);
        assert_eq!(body["results"][1]["regression"], "x-new");
        assert_eq!(body["filed"], 0);

        // A filing whose caused_by write failed still stops a refile.
        rows.pop();
        rows.push(json!({"id": "x-half", "title": "regression: x-f", "status": "idea", "source_node_id": "x-f"}));
        let body = walk(&rows, true, &decide_probe_run, &mut never);
        assert_eq!(body["results"][1]["regression"], "x-half");
    }

    #[test]
    fn symptom_recheck_never_files_on_flaky_or_undeterminable() {
        let c = Candidate {
            id: "x-a".into(),
            title: String::new(),
            plan: String::new(),
            cwd: String::new(),
            epic: None,
        };
        let fail = r#"{"reason":"exited 1","results":[{"verdict":"FAIL"}]}"#;
        let calls = std::cell::Cell::new(0);
        let flaky = |_: &[String]| {
            calls.set(calls.get() + 1);
            (if calls.get() == 1 { 1 } else { 0 }, fail.to_string())
        };
        assert_eq!(judge(&c, &flaky).0, "flaky");
        let blocked = |_: &[String]| (1, r#"{"results":[{"verdict":"BLOCKED"}]}"#.to_string());
        assert_eq!(judge(&c, &blocked).0, "undeterminable");
        assert_eq!(
            judge(&c, &|_: &[String]| (2, "{}".to_string())).0,
            "undeterminable"
        );
    }
}
