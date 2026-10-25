//! `fno-agents probe-run recheck`: re-run a closed node's symptom probe.
//!
//! A node closes on "PR merged, CI green". That proves the fix held against
//! the inventory that existed at merge, not that it still holds. A plan that
//! names a lasting symptom declares it under `recheck_probes:`, and this walk
//! re-runs that list for every done node on the daemon's cadence. A probe
//! that fails twice in one pass files a bug node with `caused_by` naming the
//! original, so the regression is open work instead of a later surprise.
//!
//! Why a key of its own and not `close_probes`: a close probe proves the fix
//! at close time and is free to read a window (`--since 24h`) or a surface
//! that later retires. Measured 2026-10-25, 13 of 21 done nodes' close probes
//! failed for those reasons alone. `recheck_probes` is the plan's promise
//! that the probe holds forever.
//!
//! The probes run through `decide_probe_run`, the one runner both gates use.
//! Only a FAIL verdict counts. BLOCKED, SKIP and an unreadable plan read as
//! undeterminable and file nothing.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::paths::AgentsHome;

pub const RECHECK_KEY: &str = "recheck_probes";
pub const RECHECK_INTERVAL_S: u64 = 21_600;
/// Each probe can spend the runner's 60 s timeout twice, so a pass is bounded.
const NODE_CAP: usize = 40;

pub struct Candidate {
    pub id: String,
    pub title: String,
    pub plan: String,
    pub cwd: String,
    /// The nearest epic above the node: the idea cap counts per epic, so a
    /// regression filed there gets that epic's room.
    pub epic: Option<String>,
}

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Pass,
    /// Failed once, passed on the retry: reported, never filed.
    Flaky(String),
    Regressed(String),
    Undeterminable(String),
}

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Done rows whose plan file exists and declares the key.
fn nearest_epic(row: &Value, rows: &[Value]) -> Option<String> {
    let mut parent = text(row, "parent").to_string();
    for _ in 0..64 {
        let up = rows
            .iter()
            .find(|r| !parent.is_empty() && text(r, "id") == parent)?;
        if text(up, "type") == "epic" {
            return Some(parent);
        }
        parent = text(up, "parent").to_string();
    }
    None
}

pub fn candidates(rows: &[Value]) -> Vec<Candidate> {
    let marker = format!("\n{RECHECK_KEY}:");
    rows.iter()
        .filter(|r| text(r, "status") == "done")
        .filter_map(|r| {
            let plan = text(r, "plan_path").split('#').next().unwrap_or("");
            if plan.is_empty() {
                return None;
            }
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

/// The open regression already filed for each original, keyed by `caused_by`.
fn open_regressions(rows: &[Value]) -> std::collections::HashMap<String, String> {
    rows.iter()
        .filter(|r| !matches!(text(r, "status"), "done" | "superseded"))
        .filter(|r| !text(r, "caused_by").is_empty())
        .map(|r| (text(r, "caused_by").to_string(), text(r, "id").to_string()))
        .collect()
}

fn one_run(c: &Candidate, run: &dyn Fn(&[String]) -> (i32, String)) -> (i32, Value) {
    let mut args = vec![
        "--plan".to_string(),
        c.plan.clone(),
        "--key".to_string(),
        RECHECK_KEY.to_string(),
        "--json".to_string(),
    ];
    if !c.cwd.is_empty() {
        args.push("--cwd".to_string());
        args.push(c.cwd.clone());
    }
    let (code, out) = run(&args);
    (code, serde_json::from_str(&out).unwrap_or(Value::Null))
}

fn has_fail(body: &Value) -> bool {
    body["results"]
        .as_array()
        .is_some_and(|r| r.iter().any(|row| row["verdict"] == "FAIL"))
}

pub fn judge(c: &Candidate, run: &dyn Fn(&[String]) -> (i32, String)) -> Verdict {
    let (code, body) = one_run(c, run);
    let reason = body["reason"].as_str().unwrap_or("no reason").to_string();
    match code {
        0 => return Verdict::Pass,
        1 if has_fail(&body) => {}
        _ => return Verdict::Undeterminable(reason),
    }
    let (code, again) = one_run(c, run);
    match code {
        0 => Verdict::Flaky(reason),
        1 if has_fail(&again) => {
            Verdict::Regressed(again["reason"].as_str().unwrap_or(&reason).to_string())
        }
        _ => Verdict::Undeterminable(again["reason"].as_str().unwrap_or(&reason).to_string()),
    }
}

/// The filing door: an unattended `idea` needs `--separate`, or a fold
/// candidate answers `choice_required` and mints nothing.
pub fn file_regression(c: &Candidate, reason: &str) -> Result<String, String> {
    let fno = crate::scrape::fno_bin();
    let title = format!("regression: {}", c.title);
    let details = format!(
        "The recheck_probes of done node {} failed twice in one pass.\n\nplan: {}\nprobe: {reason}",
        c.id, c.plan
    );
    let mut idea = std::process::Command::new(&fno);
    idea.args([
        "backlog",
        "idea",
        &title,
        "--separate",
        "--difficulty",
        "medium",
    ])
    .args(["--type", "bug", "--related", &c.id, "--source-node", &c.id])
    .args([
        "--source-kind",
        "from_observation",
        "--origin-evidence",
        &c.plan,
    ])
    .args(["--details", &details, "--json"])
    .stdin(std::process::Stdio::null());
    if !c.cwd.is_empty() {
        idea.args(["--cwd", &c.cwd]);
    }
    if let Some(epic) = &c.epic {
        idea.args(["--parent", epic]);
    }
    let out = idea
        .output()
        .map_err(|e| format!("fno backlog idea: {e}"))?;
    let body: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    let Some(id) = body["id"].as_str() else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "fno backlog idea minted nothing (exit {}): {stderr}",
            out.status.code().unwrap_or(-1),
        ));
    };
    let link = std::process::Command::new(&fno)
        .args(["backlog", "update", id, &format!("caused_by={}", c.id)])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("fno backlog update: {e}"))?;
    if !link.status.success() {
        return Err(format!("filed {id} but caused_by did not land"));
    }
    Ok(id.to_string())
}

/// One pass over the rows. Probes and filing are parameters so tests drive
/// both without a shell or the live graph.
pub fn walk(
    rows: &[Value],
    apply: bool,
    run: &dyn Fn(&[String]) -> (i32, String),
    file: &mut dyn FnMut(&Candidate, &str) -> Result<String, String>,
) -> Value {
    let open = open_regressions(rows);
    let all = candidates(rows);
    let skipped = all.len().saturating_sub(NODE_CAP);
    let mut results = Vec::new();
    let (mut passed, mut failed, mut undeterminable, mut filed) = (0, 0, 0, 0);
    // A refused filing (the idea cap, most often) is still a regression: it
    // stays on the result row and the tick detail, never silent.
    let mut unfiled = 0;
    for c in all.iter().take(NODE_CAP) {
        let (verdict, reason) = match judge(c, run) {
            Verdict::Pass => {
                passed += 1;
                ("pass", String::new())
            }
            Verdict::Flaky(r) => {
                passed += 1;
                ("flaky", r)
            }
            Verdict::Undeterminable(r) => {
                undeterminable += 1;
                ("undeterminable", r)
            }
            Verdict::Regressed(r) => {
                failed += 1;
                ("regressed", r)
            }
        };
        let mut row = json!({"node": c.id, "verdict": verdict, "reason": reason});
        if verdict == "regressed" {
            if let Some(existing) = open.get(&c.id) {
                row["regression"] = json!(existing);
            } else if apply {
                match file(c, &reason) {
                    Ok(id) => {
                        filed += 1;
                        row["regression"] = json!(id);
                    }
                    Err(e) => {
                        unfiled += 1;
                        row["file_error"] = json!(e);
                    }
                }
            }
        }
        results.push(row);
    }
    json!({
        "key": RECHECK_KEY,
        "apply": apply,
        "checked": results.len(),
        "passed": passed,
        "failed": failed,
        "undeterminable": undeterminable,
        "filed": filed,
        "unfiled": unfiled,
        "over_cap": skipped,
        "results": results,
    })
}

/// `fno-agents probe-run recheck [--apply] [--json]`. Without `--apply` it
/// files nothing. Exit 0 when nothing regressed, 1 when something did, 2
/// when the graph is unreadable. stdout is always one JSON object.
pub fn run(args: &[String]) -> i32 {
    let apply = args.iter().any(|a| a == "--apply");
    let (code, body) = pass(apply);
    println!("{body}");
    code
}

fn pass(apply: bool) -> (i32, Value) {
    let graph = crate::backlog::settings::graph_path();
    let rows = match crate::graph_store::read_rows(&graph) {
        Ok(rows) => rows,
        Err(e) => {
            return (
                2,
                json!({"key": RECHECK_KEY, "error": format!("graph unreadable: {e}")}),
            )
        }
    };
    let body = walk(
        &rows,
        apply,
        &crate::acceptance_evidence::decide_probe_run,
        &mut file_regression,
    );
    (i32::from(body["failed"].as_u64().unwrap_or(0) > 0), body)
}

/// The daemon arm: a cadence stamp plus a one-in-flight gate.
#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

fn tick_row(body: &Value) -> (u64, Option<String>, String) {
    if let Some(e) = body["error"].as_str() {
        return (0, Some("error".to_string()), e.to_string());
    }
    let n = |k: &str| body[k].as_u64().unwrap_or(0);
    (
        n("filed"),
        None,
        format!(
            "checked {} passed {} regressed {} undeterminable {} filed {} unfiled {}",
            n("checked"),
            n("passed"),
            n("failed"),
            n("undeterminable"),
            n("filed"),
            n("unfiled")
        ),
    )
}

pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    let interval = Duration::from_secs(RECHECK_INTERVAL_S);
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        // Probes may read GitHub; inside the backoff they only spend refusals.
        let backoff_s = if crate::gh_budget::admission_armed() {
            crate::gh_budget::snapshot(&crate::gh_budget::ledger_path(), crate::gh_budget::now_ms())
                .backoff_remaining_s
        } else {
            0
        };
        let (acted, skip, detail) = if backoff_s > 0 {
            (
                0,
                Some("rate_limited".to_string()),
                format!("GitHub rate limit backoff, {backoff_s}s left; the recheck waits"),
            )
        } else {
            tick_row(&pass(true).1)
        };
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        crate::tick_ledger::emit_tick(
            &journal,
            "symptom_recheck",
            crate::tick_ledger::SCHED_DAEMON,
            acted,
            skip.as_deref(),
            Some(&detail),
            RECHECK_INTERVAL_S,
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn plan(dir: &std::path::Path, name: &str, probes: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, format!("---\ntitle: t\n{probes}\n---\n\n# doc\n")).unwrap();
        p.to_string_lossy().into_owned()
    }

    fn done(id: &str, plan: &str) -> Value {
        json!({"id": id, "title": format!("{id} title"), "status": "done", "plan_path": plan, "cwd": ""})
    }

    fn no_file(_: &Candidate, _: &str) -> Result<String, String> {
        panic!("nothing may be filed")
    }

    #[test]
    fn symptom_recheck_reads_only_the_recheck_key_on_done_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        let declared = plan(tmp.path(), "a.md", "recheck_probes:\n  - \"echo ok\"");
        let close_only = plan(tmp.path(), "b.md", "close_probes:\n  - \"exit 1\"");
        let mut open = done("x-c", &declared);
        open["status"] = json!("in_review");
        let rows = vec![done("x-a", &declared), done("x-b", &close_only), open];
        let body = walk(
            &rows,
            true,
            &crate::acceptance_evidence::decide_probe_run,
            &mut no_file,
        );
        assert_eq!(body["checked"], 1);
        assert_eq!(body["results"][0]["node"], "x-a");
        assert_eq!(body["results"][0]["verdict"], "pass");
    }

    #[test]
    fn symptom_recheck_files_once_on_a_double_fail_and_dedupes_on_caused_by() {
        let tmp = tempfile::tempdir().unwrap();
        let failing = plan(tmp.path(), "f.md", "recheck_probes:\n  - \"exit 3\"");
        let mut rows = vec![done("x-f", &failing)];
        let mut filed = Vec::new();
        let mut file = |c: &Candidate, _: &str| {
            filed.push(c.id.clone());
            Ok("x-new".to_string())
        };
        let body = walk(
            &rows,
            true,
            &crate::acceptance_evidence::decide_probe_run,
            &mut file,
        );
        assert_eq!(body["results"][0]["verdict"], "regressed");
        assert_eq!(body["filed"], 1);
        assert_eq!(filed, vec!["x-f".to_string()]);

        // The filed node is open and names the original: the next pass skips.
        rows.push(json!({"id": "x-new", "status": "idea", "caused_by": "x-f"}));
        let body = walk(
            &rows,
            true,
            &crate::acceptance_evidence::decide_probe_run,
            &mut no_file,
        );
        assert_eq!(body["filed"], 0);
        assert_eq!(body["results"][0]["regression"], "x-new");

        // A dry run reports the regression and files nothing.
        let body = walk(
            &rows[..1],
            false,
            &crate::acceptance_evidence::decide_probe_run,
            &mut no_file,
        );
        assert_eq!(body["failed"], 1);
        assert_eq!(body["filed"], 0);
    }

    #[test]
    fn symptom_recheck_never_files_on_flaky_or_undeterminable() {
        let tmp = tempfile::tempdir().unwrap();
        let declared = plan(tmp.path(), "a.md", "recheck_probes:\n  - \"echo ok\"");
        let c = Candidate {
            epic: None,
            id: "x-a".into(),
            title: String::new(),
            plan: declared,
            cwd: String::new(),
        };
        let fail = r#"{"reason":"`p` exited 1","results":[{"verdict":"FAIL"}]}"#;
        let calls = Cell::new(0);
        let flaky = |_: &[String]| {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                (1, fail.to_string())
            } else {
                (0, "{}".to_string())
            }
        };
        assert!(matches!(judge(&c, &flaky), Verdict::Flaky(_)));
        let blocked = |_: &[String]| {
            (
                1,
                r#"{"reason":"BLOCKED","results":[{"verdict":"BLOCKED"}]}"#.to_string(),
            )
        };
        assert!(matches!(judge(&c, &blocked), Verdict::Undeterminable(_)));
        let unreadable = |_: &[String]| (2, r#"{"reason":"plan unreadable"}"#.to_string());
        assert!(matches!(judge(&c, &unreadable), Verdict::Undeterminable(_)));
    }
}
