//! Does the plan's fidelity gate pass?
//!
//! Ported from `cli/src/fno/plan/fidelity.py` (deleted in the same change,
//! port protocol steps 2-4). The gate assembly reads its inputs in process:
//! the tolerant ledger read, the graph rows, the plan's frontmatter, the
//! carveout ledger, the plan-fidelity join (one fold, two readers:
//! `scoreboard::plan_join`), and the PR body leg through the one body
//! parser (`scripts/ci/check-oos-tracked.sh`, still bash). The old shell hop
//! to `fno do plan fidelity` is gone, so the hang-the-child failure class
//! the bounded runner covered is unrepresentable here.

use super::*;
use crate::backlog::api::{self as backlog_api, Store as GraphStore};
use crate::merge_gates::breadcrumb;
use crate::plan_doc::codec as fm_codec;
use serde_json::json;

/// Plan-fidelity stop gate. The stop-gate half of AC5; the merge gate
/// (`merge_gates::plan_fidelity_blocker`, which computes the same decision
/// in process) is the other. Blocks DonePRGreen when a planned deliverable
/// is unjoined and uncovered by a carveout - the agent must file a carveout
/// before it may stop. Mirrors `ProbeGate`'s shape deliberately.
#[derive(Debug)]
pub(crate) enum FidelityGate {
    /// No plan bound, or the probe degraded. Fail open: a missing plan must
    /// NOT wedge the stop gate - the merge gate is the backstop. Fail open
    /// here is what keeps a broken read from wedging every run.
    Absent,
    Pass,
    Refused {
        reason: String,
    },
}

/// Classify one decision object (the gate's JSON shape) into the stop-gate
/// verdict. The two readers consume the same shape, so one classifier
/// serves both.
pub(crate) fn classify_plan_fidelity(stdout: &[u8]) -> FidelityGate {
    let v: Value = match serde_json::from_slice(stdout) {
        Ok(v) => v,
        Err(_) => return FidelityGate::Absent,
    };
    match v.get("refused").and_then(|r| r.as_bool()) {
        Some(true) => FidelityGate::Refused {
            reason: v
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("plan has unjoined deliverables with no covering carveout")
                .to_string(),
        },
        _ => FidelityGate::Pass,
    }
}

/// Run the native gate for the bound plan and classify the decision.
pub(crate) fn evaluate_plan_fidelity(plan_path: Option<&str>, cwd: &Path) -> FidelityGate {
    let plan = match plan_path {
        Some(p) if !p.is_empty() => p,
        _ => return FidelityGate::Absent,
    };
    let decision = compute_plan_fidelity(plan, cwd, 180);
    classify_plan_fidelity(decision.to_string().as_bytes())
}

/// The green-conjunct stop read for one fire: the plan-fidelity gate, or the
/// delegated-merge park that skips it.
///
/// A delegated merge (a per-run no-merge manifest, or a valid team ruling
/// hold on this node) skips the stop-time read: someone else merges, the
/// merge gate re-runs the same fidelity join at merge time, and DonePRGreen
/// is a shipped terminal, so the later join sees a delivered row rather than
/// stranding a row on a non-shipped terminal no later merge ever restamps.
/// The park names who merges so a human (or the merge queue) owns the next
/// act.
pub(crate) fn green_conjunct_read(
    manifest_no_merge: bool,
    manifest_source: Option<&str>,
    node_id: Option<&str>,
    plan_path: Option<&str>,
    cwd: &Path,
    pr_number: i64,
    head_oid: &str,
) -> GreenRead {
    let ruling = node_id.and_then(super::awaiting_merge::ruling_hold);
    if manifest_no_merge || ruling.is_some() {
        let owner = if manifest_no_merge {
            let source = manifest_source.unwrap_or("unknown");
            let repo_slug = crate::finalize::slug_from_git_remote(cwd).unwrap_or_default();
            format!(
                "the operator (per-run no-merge, source {source}); attended grant: {}",
                crate::merge_grant::attended_grant_command(&repo_slug, pr_number, head_oid)
            )
        } else {
            format!("the ruling {}", ruling.unwrap_or_default())
        };
        return GreenRead {
            block: None,
            merge_owner: Some(owner),
        };
    }
    let block = match evaluate_plan_fidelity(plan_path, cwd) {
        FidelityGate::Refused { reason } => Some(reason),
        _ => None,
    };
    GreenRead {
        block,
        merge_owner: None,
    }
}

pub(crate) struct GreenRead {
    /// The fidelity refusal that blocks DonePRGreen, if any.
    pub block: Option<String>,
    /// Set only when the merge is delegated: who lands this PR now.
    pub merge_owner: Option<String>,
}

// ── the gate assembly (the fidelity.py port) ─────────────────────────────────

/// One body parser: the oos-tracked guard the body leg runs.
const OOS_SCRIPT_REL: &str = "scripts/ci/check-oos-tracked.sh";

/// Wall-clock ceiling for the body-guard child. The Python leg bounded the
/// same script at 10 s.
const BODY_GUARD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// The gate's inputs, resolved once so the decision is computable from
/// fixtures (the characterization drives this shape directly).
pub(crate) struct GateInputs<'a> {
    pub plan_path: &'a str,
    /// The gated checkout: the slug read, the carveout root and the resolve
    /// base all key on it.
    pub cwd: &'a Path,
    pub ledger_path: &'a Path,
    pub graph_path: &'a Path,
    pub since_days: i64,
    pub now: chrono::DateTime<chrono::Local>,
    /// The PR body read, injected so the characterization never opens a
    /// network connection. The production reader is the REST door.
    pub read_body: &'a dyn Fn(u64) -> Option<String>,
    /// The oos script to run. None resolves the plugin ladder at run time.
    pub oos_script: Option<&'a Path>,
}

/// `paths._remote_url_to_slug`: the LAST `/`-or-`:` segment of the remote
/// url, one trailing `.git` stripped. Not the owner/repo pair the merge
/// gate's pr_url matcher builds - the gate's scoping slug is the bare repo
/// name, and the ledger's `project` column carries the same shape.
pub(crate) fn remote_url_to_slug(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    if url.is_empty() {
        return None;
    }
    let mut tail = url.rsplit(['/', ':']).next().unwrap_or("");
    if let Some(stripped) = tail.strip_suffix(".git") {
        tail = stripped;
    }
    if tail.is_empty() || tail.contains('/') || tail.contains('\\') {
        return None;
    }
    Some(tail.to_string())
}

/// `fidelity._plan_key`: the plan path scoped to a project - the last two
/// path segments (parent dir + file), so a recurring folder-plan filename
/// does not collapse unrelated plans, prefixed `project::`. Prefix-free
/// otherwise, so the planning row and its shipped row match regardless of
/// the worktree each was stamped under.
pub(crate) fn gate_plan_key(plan_path: &str, project: Option<&str>) -> Option<String> {
    if plan_path.is_empty() {
        return None;
    }
    let p = Path::new(plan_path.split('#').next().unwrap_or(plan_path));
    let file = p.file_name()?.to_string_lossy().into_owned();
    let parent = p
        .parent()
        .and_then(|par| par.file_name())
        .map(|par| par.to_string_lossy().into_owned());
    let tail = match parent {
        Some(par) if !par.is_empty() => format!("{par}/{file}"),
        _ => file,
    };
    if tail.is_empty() || tail == "/" {
        return None;
    }
    Some(format!("{}::{tail}", project.unwrap_or_default()))
}

/// `Path.resolve()`'s non-strict shape: absolute, symlinks resolved when the
/// path exists, lexical absolutization when it does not.
fn resolve_loose(path: &Path, base: &Path) -> PathBuf {
    if let Ok(resolved) = path.canonicalize() {
        return resolved;
    }
    if path.is_absolute() {
        return path.to_path_buf();
    }
    base.join(path)
}

pub(crate) fn compute_plan_fidelity(plan_path: &str, cwd: &Path, since_days: i64) -> Value {
    // Read-only: the ledger is global, so an undeclared test process answers
    // the passthrough (the same fence `ledger_plan_path` carries) instead of
    // reading the operator's real history.
    if cfg!(test) && !crate::paths::test_root_declared() {
        return json!({
            "refused": false,
            "reason": Value::Null,
            "planned": 0,
            "delivered": 0,
            "shortfall": 0,
            "carveouts": 0,
            "covered": true,
            "unjoined": [],
        });
    }
    let ledger = crate::paths_cli::ledger_json(cwd);
    let graph = crate::org_board::scope::graph_json_path(cwd);
    let read_body = |pr: u64| read_pr_body_rest(pr, cwd);
    compute_plan_fidelity_from(&GateInputs {
        plan_path,
        cwd,
        ledger_path: &ledger,
        graph_path: &graph,
        since_days,
        now: chrono::Local::now(),
        read_body: &read_body,
        oos_script: None,
    })
}

pub(crate) fn compute_plan_fidelity_from(inputs: &GateInputs) -> Value {
    let plan_file_txt = inputs
        .plan_path
        .split('#')
        .next()
        .unwrap_or(inputs.plan_path);
    let plan_file = Path::new(plan_file_txt);
    if !plan_file.is_file() {
        return refused_decision(&format!(
            "plan unreadable ({}); a gate degrades to refuse, not n/a",
            inputs.plan_path
        ));
    }

    // The tolerant ledger read (the one owner, `scoreboard::readers`); a
    // broken ledger is no planned rows, not a crash.
    let rows: Vec<Value> =
        crate::scoreboard::readers::load_ledger_rows(inputs.ledger_path).unwrap_or_default();
    let graph_nodes: Vec<Value> = graph_rows(inputs.graph_path).unwrap_or_default();

    let origin = git_origin_url(inputs.cwd).unwrap_or_default();
    let project = remote_url_to_slug(&origin);
    let target = gate_plan_key(inputs.plan_path, project.as_deref());
    let target_file = resolve_loose(Path::new(plan_file_txt), inputs.cwd);
    let node_plan_paths: std::collections::HashMap<String, PathBuf> = graph_nodes
        .iter()
        .filter_map(|node| {
            let id = node.get("id")?.as_str()?;
            let pp = node.get("plan_path")?.as_str()?;
            Some((
                id.to_string(),
                resolve_loose(Path::new(pp.split('#').next().unwrap_or(pp)), inputs.cwd),
            ))
        })
        .collect();

    // Scope rows to this plan BEFORE the fold: it joins per plan, so an
    // unscoped global ledger measures someone else's deliverables.
    let filtered: Vec<Value> = rows
        .into_iter()
        .filter(|row| {
            if let (Some(key), Some(t)) = (
                gate_plan_key(
                    row.get("plan_path").and_then(Value::as_str).unwrap_or(""),
                    row.get("project").and_then(Value::as_str),
                ),
                target.as_deref(),
            ) {
                if key == t {
                    return true;
                }
            }
            let has_plan_path = row
                .get("plan_path")
                .map(|v| v.as_str().map(|s| !s.is_empty()).unwrap_or(false))
                .unwrap_or(false);
            if has_plan_path {
                return false;
            }
            match row.get("graph_node_id").and_then(Value::as_str) {
                Some(nid) => node_plan_paths
                    .get(nid)
                    .map(|p| *p == target_file)
                    .unwrap_or(false),
                None => false,
            }
        })
        .collect();

    // Frontmatter declares the carveouts vocabulary and the node id; a
    // parse failure declares nothing.
    let fm = read_plan_text(plan_file)
        .and_then(|text| crate::plan_doc::codec::parse_frontmatter(&text).ok())
        .map(|parsed| parsed.fields)
        .unwrap_or_default();
    // Any declared value but `allowed` is forbidden, so a typo fails closed.
    let forbidden = fm
        .get("carveouts")
        .map(|v| match v {
            fm_codec::Value::Scalar(s) => s.trim().to_lowercase() != "allowed",
            // A list or raw block is a declared value that is not the word
            // `allowed`, so it forbids, the same str()-coercion answer the
            // Python fold gave.
            _ => true,
        })
        .unwrap_or(false);
    let scalar_word = |key: &str| -> Option<String> {
        match fm.get(key) {
            Some(fm_codec::Value::Scalar(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
            _ => None,
        }
    };
    let node_id = scalar_word("node").or_else(|| scalar_word("claims"));

    let now_iso = inputs.now.to_rfc3339();
    let join = crate::scoreboard::plan_join::plan_fidelity_op(&json!({
        "rows": filtered,
        "entries": graph_nodes,
        "since_days": inputs.since_days,
        "now": now_iso,
        "unmeasurable": "refuse",
    }));
    let mut decision = match join {
        Ok(pf) if pf.get("state").and_then(Value::as_str) == Some("ok") => {
            let plan_rows = pf
                .get("results")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let unjoined: Vec<Value> = plan_rows
                .iter()
                .filter(|r| r.get("status").and_then(Value::as_str) == Some("unjoined"))
                .cloned()
                .collect();
            let planned = plan_rows.len();
            let session_ids: Vec<String> = plan_rows
                .iter()
                .filter_map(|r| r.get("session_id").and_then(Value::as_str))
                .map(String::from)
                .collect();
            let carveouts = read_covering_carveouts(inputs.cwd, &session_ids);
            let shortfall = unjoined.len();
            let covering = carveouts.len();
            let covered = shortfall > 0 && covering >= shortfall && !forbidden;
            let refused = (shortfall > 0 && !covered) || (forbidden && covering > 0);
            let reason = if forbidden && covering > 0 {
                Some(format!(
                    "plan forbids carve-outs; {covering} carveout(s) filed by this plan's sessions"
                ))
            } else if refused {
                Some(format!(
                    "{shortfall} planned deliverable(s) unjoined with only \
                     {covering} covering carveout(s); a PR-body sentence is not a carveout"
                ))
            } else {
                None
            };
            json!({
                "refused": refused,
                "shortfall": shortfall,
                "carveouts": covering,
                "covered": covered,
                "reason": reason,
                "unjoined": unjoined,
                "planned": planned,
                "delivered": planned - shortfall,
                "plan_path": inputs.plan_path,
            })
        }
        Ok(_) => {
            // No planned rows in window: nothing to measure, nothing to refuse.
            json!({
                "refused": false,
                "reason": Value::Null,
                "planned": 0,
                "delivered": 0,
                "shortfall": 0,
                "carveouts": 0,
                "covered": true,
                "unjoined": [],
            })
        }
        Err(_) => {
            // The fold refused to run. The Python leg crashed the command on
            // this path; the native gate degrades to the passthrough instead
            // of turning an internal error into a refusal. Recorded delta.
            breadcrumb("plan fidelity join errored; degrading to passthrough");
            json!({
                "refused": false,
                "reason": Value::Null,
                "planned": 0,
                "delivered": 0,
                "shortfall": 0,
                "carveouts": 0,
                "covered": true,
                "unjoined": [],
            })
        }
    };

    // A plan with `carveouts: forbidden` also refuses a PR body that declares
    // an exclusion section. check-oos-tracked.sh stays the one body parser.
    if forbidden {
        decision["carveouts_policy"] = json!("forbidden");
        if decision
            .get("refused")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return decision;
        }
        let pr_number = node_id.as_ref().and_then(|nid| {
            graph_nodes
                .iter()
                .find(|n| n.get("id").and_then(Value::as_str) == Some(nid.as_str()))
                .and_then(|n| n.get("pr_number"))
                .and_then(Value::as_i64)
        });
        let Some(pr_number) = pr_number else {
            return decision; // no PR yet; the stop gate runs only after one is open
        };
        decision["carveouts_pr"] = json!(pr_number);
        let reason = match (inputs.read_body)(pr_number as u64) {
            Some(body) => run_body_guard(body, inputs.oos_script, inputs.cwd),
            None => Some("PR body unreadable; a gate degrades to refuse".to_string()),
        };
        if let Some(reason) = reason {
            decision["refused"] = json!(true);
            decision["reason"] = json!(reason);
        }
    }
    decision
}

fn refused_decision(reason: &str) -> Value {
    json!({
        "refused": true,
        "reason": reason,
        "planned": 0,
        "delivered": 0,
        "shortfall": 0,
        "carveouts": 0,
        "covered": false,
        "unjoined": [],
    })
}

fn read_plan_text(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn graph_rows(graph_path: &Path) -> Option<Vec<Value>> {
    let store = GraphStore::new(graph_path);
    backlog_api::rows(&store).ok()
}

fn git_origin_url(cwd: &Path) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        None
    } else {
        Some(url)
    }
}

/// Carveouts filed by the plan's sessions. An unreadable read fails OPEN on
/// coverage: the unreadable-plan case already refuses above, and a carveout
/// read failure must not be the hole that ships an uncovered shortfall.
fn read_covering_carveouts(root: &Path, session_ids: &[String]) -> Vec<Value> {
    if session_ids.is_empty() {
        return Vec::new();
    }
    match crate::retro::ledger::read_carveouts(root, None) {
        Ok(rows) => crate::backlog::carveout::scope_rows(rows, Some(session_ids), false),
        Err(_) => Vec::new(),
    }
}

/// The REST body read through the conditional door, `fidelity.py`'s shape.
fn read_pr_body_rest(pr_number: u64, cwd: &Path) -> Option<String> {
    let origin = git_origin_url(cwd)?;
    let slug = crate::merge_gates::repo_slug_from_origin(&origin)?;
    let payload = json!({
        "op": "status-rest-get",
        "cwd": cwd.to_string_lossy(),
        "path": format!("repos/{slug}/pulls/{pr_number}"),
    });
    let answer: Value =
        serde_json::from_str(&crate::pr_status_facts::run_op("status-rest-get", &payload)).ok()?;
    if !answer.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let stdout = answer.get("stdout").and_then(Value::as_str)?;
    let row: Value = serde_json::from_str(stdout).ok()?;
    row.get("body").and_then(Value::as_str).map(String::from)
}

/// One completed body-guard child run.
pub(crate) struct GuardOutput {
    /// `None` = killed at the bound (a signal exit has no code).
    pub(crate) status: Option<i32>,
    pub(crate) stderr_tail: String,
}

/// Resolve the oos script through the plugin ladder, then run it with the
/// gate's env contract. Exit 0 passes; exit 1 refuses with the guard's own
/// first stderr line; anything else is an unreadable check, which refuses.
pub(crate) fn run_body_guard(body: String, script: Option<&Path>, cwd: &Path) -> Option<String> {
    let resolved = script
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| crate::target_init::plugin_rel_path(OOS_SCRIPT_REL, cwd));
    let mut cmd = std::process::Command::new("bash");
    cmd.arg(&resolved).current_dir(cwd);
    // Child-only env: the process env is shared with the loop's other reads,
    // so a stamped PR_BODY must never leak onto it.
    cmd.env("PR_BODY", body).env("PLAN_CARVEOUTS", "forbidden");
    let out = bounded_spawn(cmd, BODY_GUARD_TIMEOUT);
    match out {
        Some(out) => body_guard_verdict(&out),
        None => Some("carve-out check unreadable (timeout); a gate degrades to refuse".to_string()),
    }
}

/// Bounded child run with child-only env stamping: spawn through the
/// crate's one bounded spawner (retry and process-group rules included),
/// drain pipes on reader threads, poll to the deadline, kill at the bound.
pub(crate) fn bounded_spawn(
    cmd: std::process::Command,
    timeout: std::time::Duration,
) -> Option<GuardOutput> {
    use std::io::Read;
    let mut child = crate::bounded_spawn::spawn_bounded_command(cmd).ok()?;
    let Some(mut stdout) = child.stdout.take() else {
        unreachable!("stdout was just piped")
    };
    let Some(mut stderr) = child.stderr.take() else {
        unreachable!("stderr was just piped")
    };
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + timeout;
    let status: Option<i32> = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => break None,
        }
    };
    let stderr_tail = err_handle
        .join()
        .map(|buf| truncate_tail(&buf))
        .unwrap_or_default();
    let _ = out_handle.join();
    Some(GuardOutput {
        status,
        stderr_tail,
    })
}

fn truncate_tail(buf: &[u8]) -> String {
    const CAP: usize = 8 * 1024;
    let start = buf.len().saturating_sub(CAP);
    String::from_utf8_lossy(&buf[start..]).into_owned()
}

pub(crate) fn body_guard_verdict(out: &GuardOutput) -> Option<String> {
    match out.status {
        Some(0) => None,
        Some(1) => {
            let first = out.stderr_tail.trim().lines().next().unwrap_or("").trim();
            if !first.is_empty() {
                Some(first.to_string())
            } else {
                Some("carve-out check unreadable (exit 1); a gate degrades to refuse".to_string())
            }
        }
        Some(code) => Some(format!(
            "carve-out check unreadable (exit {code}); a gate degrades to refuse"
        )),
        None => Some("carve-out check unreadable (timeout); a gate degrades to refuse".to_string()),
    }
}

#[cfg(test)]
mod gate_tests {
    use super::*;

    #[test]
    fn remote_slug_takes_the_last_segment_and_strips_git() {
        assert_eq!(
            remote_url_to_slug("https://github.com/acme/widget.git"),
            Some("widget".to_string())
        );
        assert_eq!(
            remote_url_to_slug("git@github.com:acme/widget.git"),
            Some("widget".to_string())
        );
        assert_eq!(remote_url_to_slug(""), None);
    }

    #[test]
    fn plan_key_scopes_by_last_two_segments() {
        assert_eq!(
            gate_plan_key("/a/plans/p.md", Some("widget")),
            Some("widget::plans/p.md".to_string())
        );
        assert_eq!(
            gate_plan_key("/a/plans/p.md#group-g2", Some("widget")),
            Some("widget::plans/p.md".to_string())
        );
        assert_eq!(gate_plan_key("p.md", None), Some("::p.md".to_string()));
        assert_eq!(gate_plan_key("", Some("w")), None);
    }
}
