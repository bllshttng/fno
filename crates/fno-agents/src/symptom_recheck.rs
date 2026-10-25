//! `fno-agents probe-run recheck`: re-run a closed node's symptom probe.
//!
//! A node closes on "PR merged, CI green". That proves the fix against the
//! inventory at merge, not that it still holds. A plan that names a lasting
//! symptom declares `recheck_probes:`, and this walk re-runs that list for
//! every done node on the daemon's cadence. A list that fails twice in one
//! pass files a bug node whose `source_node_id` names the original.
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

/// The rows that carry a plan. The caller passes only done rows. The plan
/// file is never read here: the probe runner answers `declared: false` for a
/// plan with no `recheck_probes`, and the walk skips it.
pub fn candidates(rows: &[&Value], all: &[Value]) -> Vec<Candidate> {
    rows.iter()
        .filter(|r| !text(r, "plan_path").is_empty())
        .map(|r| {
            // A repo-relative plan_path reads against the node's cwd, as
            // decide_probe_run does, never the daemon's.
            let raw = std::path::Path::new(text(r, "plan_path").split('#').next().unwrap_or(""));
            let cwd = text(r, "cwd");
            let plan = if raw.is_relative() && !cwd.is_empty() {
                std::path::Path::new(cwd).join(raw)
            } else {
                raw.to_path_buf()
            };
            Candidate {
                id: text(r, "id").to_string(),
                title: text(r, "title").to_string(),
                plan: plan.to_string_lossy().into_owned(),
                cwd: text(r, "cwd").to_string(),
                epic: nearest_epic(r, all),
            }
        })
        .collect()
}

fn one_run(c: &Candidate, run: Runner) -> (i32, bool, String, bool) {
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
    (code, code == 1 && fail, reason, body["declared"] == false)
}

/// `absent` (the plan declares no recheck_probes), `pass`, `flaky` (failed,
/// then passed on the retry), `regressed` or `undeterminable`, with the
/// runner's reason.
pub fn judge(c: &Candidate, run: Runner) -> (&'static str, String) {
    match one_run(c, run) {
        (_, _, _, true) => return ("absent", String::new()),
        (0, _, _, _) => return ("pass", String::new()),
        (_, false, why, _) => return ("undeterminable", why),
        _ => {}
    }
    match one_run(c, run) {
        (0, _, why, _) => ("flaky", why),
        (_, true, why, _) => ("regressed", why),
        (_, false, why, _) => ("undeterminable", why),
    }
}

/// The filing door, in process: the same parse and birth path `fno backlog
/// idea` runs, so the title gate and the idea cap still hold. Calling
/// `create_node` directly skips the fold offer, as `--separate` does. The
/// node's `source_node_id` names the original at birth, and that link is
/// what the dedupe reads.
pub fn file_regression(c: &Candidate, reason: &str) -> Result<String, String> {
    let title = format!("regression: {}", c.title);
    let details = format!(
        "The recheck_probes of done node {} failed twice in one pass.\n\nplan: {}\nprobe: {reason}",
        c.id, c.plan
    );
    let mut args: Vec<String> = [&title, "--related", &c.id, "--source-node", &c.id]
        .into_iter()
        .map(String::from)
        .collect();
    args.extend(["--origin-evidence", &c.plan, "--details", &details].map(String::from));
    let fixed = "--difficulty medium --type bug --source-kind from_observation";
    args.extend(fixed.split(' ').map(String::from));
    if !c.cwd.is_empty() {
        args.extend(["--cwd".to_string(), c.cwd.clone()]);
    }
    if let Some(epic) = &c.epic {
        args.extend(["--parent".to_string(), epic.clone()]);
    }
    use crate::backlog::create_cli::{create_node, parse_idea, ParsedIdea};
    let ParsedIdea::Args(idea) = parse_idea(&args) else {
        return Err("the regression filing did not parse".to_string());
    };
    create_node(&idea.add, &|_| None)
        .map(|born| born.id)
        .map_err(|r| format!("no node was filed: {}", r.message))
}

/// One pass. Probes and filing are parameters so tests need no shell and no
/// live graph. An open regression or `caused_by` link to the original stops a
/// second filing. A refused filing (the idea cap, most often) stays on the
/// row as `file_error` and counts as `unfiled`.
/// The first candidate of pass `window`. Over the cap, each pass starts 40
/// further on, so every candidate is checked within ceil(len / 40) passes.
pub fn batch_start(len: usize, window: usize) -> usize {
    if len > NODE_CAP {
        window.wrapping_mul(NODE_CAP) % len
    } else {
        0
    }
}

pub fn walk(
    rows: &[Value],
    window: usize,
    apply: bool,
    run: Runner,
    file: &mut dyn FnMut(&Candidate, &str) -> Result<String, String>,
) -> Value {
    let open: std::collections::HashMap<&str, &str> = rows
        .iter()
        .filter(|r| !matches!(text(r, "status"), "done" | "superseded"))
        .filter_map(|r| {
            // A recheck filing names its original in source_node_id; a node a
            // person linked by caused_by tracks it just as well.
            let orig = match text(r, "caused_by") {
                "" if text(r, "title").starts_with("regression: ") => text(r, "source_node_id"),
                linked => linked,
            };
            (!orig.is_empty()).then(|| (orig, text(r, "id")))
        })
        .collect();
    let done: Vec<&Value> = rows
        .iter()
        .filter(|r| text(r, "status") == "done")
        .collect();
    let all = candidates(&done, rows);
    let mut results = Vec::new();
    let start = batch_start(all.len(), window);
    let mut scanned = 0;
    for c in all.iter().cycle().skip(start).take(all.len()) {
        if results.len() == NODE_CAP {
            break;
        }
        scanned += 1;
        let (verdict, reason) = judge(c, run);
        if verdict == "absent" {
            continue;
        }
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
        "over_cap": all.len() - scanned,
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
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let window = (now / RECHECK_INTERVAL_S) as usize;
    let body = walk(&rows, window, apply, &decide, &mut file_regression);
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
        let body = walk(&rows, 0, true, &decide_probe_run, &mut file);
        assert_eq!(
            (body["checked"].as_u64(), body["passed"].as_u64()),
            (Some(2), Some(1))
        );
        assert_eq!(body["results"][1]["verdict"], "regressed");
        assert_eq!(filed, vec!["x-f".to_string()]);

        // The filed node is open and names the original: the next pass skips it.
        rows.push(json!({"id": "x-new", "status": "idea", "caused_by": "x-f"}));
        let mut never = |_: &Candidate, _: &str| -> Result<String, String> { panic!("refiled") };
        let body = walk(&rows, 0, true, &decide_probe_run, &mut never);
        assert_eq!(body["results"][1]["regression"], "x-new");
        assert_eq!(body["filed"], 0);

        // A recheck filing links by source_node_id alone.
        rows.pop();
        rows.push(json!({"id": "x-half", "title": "regression: x-f", "status": "idea", "source_node_id": "x-f"}));
        let body = walk(&rows, 0, true, &decide_probe_run, &mut never);
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

        // Over the cap, consecutive passes start 40 further on and wrap.
        let starts = (batch_start(100, 1), batch_start(100, 3), batch_start(5, 7));
        assert_eq!(starts, (40, 20, 0));
    }
}
