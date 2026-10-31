//! The real seams, the whole-fleet sweep, and the sweep file both cadences share.

use super::tail::{facts_from_entries, sweep_listing, tail_entries, TAIL_RECORDS};
use super::{
    branch_commit_count, unknown_provider_report, verdicts, ClaimView, Seams, TailFacts, Verdict,
    LEAVE, SILENCE, TERMINAL_STATES,
};
use crate::backlog::worked::{self, FleetRow};
use crate::paths::AgentsHome;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// A sweep over zero rows is an unreadable instrument, never an empty fleet: it
/// would write empty counts and a fresh mtime, which reads as a healthy quiet
/// fleet.
pub(crate) const ROSTER_REFUSAL: &str = "roster unreadable: 0 rows (an empty fleet and a broken instrument read the same; refusing so staleness reads loud)";

pub(crate) fn utc_stamp(now_s: f64) -> String {
    chrono::DateTime::from_timestamp(now_s as i64, 0)
        .map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

pub(crate) fn now_epoch_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

pub(crate) fn state_root() -> PathBuf {
    crate::agents_config::state_dir(&cwd()).unwrap_or_else(|| PathBuf::from(".fno"))
}

/// Best-effort event on the global journal. The source is `daemon` for both
/// cadences, the schema's only allowed word for these types. Loud on a miss: a
/// lane that writes nothing must not look like silence.
pub(crate) fn emit_event(kind: &str, data: Value) {
    let path = state_root().join("events.jsonl");
    let fields: Map<String, Value> = match data {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    let emitter = crate::events::EventEmitter::new(path, "daemon");
    if let Err(exc) = emitter.emit_fields(kind, fields) {
        eprintln!("watchdog: event {kind} not written: {exc}");
    }
}

/// The node claim view; a failed store read is `Err`, never an empty answer.
pub(crate) fn claim_view(node: &str) -> Result<ClaimView, String> {
    let key = format!("node:{node}");
    match crate::claims::status_checked(&key, None) {
        Ok((state, record)) => Ok(ClaimView {
            state: state.as_str().to_string(),
            holder: record.map(|r| r.holder),
        }),
        Err(exc) => Err(format!("claims root unreadable ({exc})")),
    }
}

fn graph_index() -> Result<BTreeMap<String, Value>, String> {
    let home = AgentsHome::from_env();
    let rows = crate::gc_sweep::read_graph_rows(&home)
        .ok_or_else(|| "graph unreadable (no rows answered)".to_string())?;
    let mut index = BTreeMap::new();
    for row in rows {
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            if !id.is_empty() {
                index.insert(id.to_string(), row.clone());
            }
        }
    }
    Ok(index)
}

/// Current PR state through the routed reader in the row's own checkout; None
/// is unknown and the polling lane stays silent on it.
fn production_pr_state(cwd: &str, pr_number: i64) -> Option<String> {
    let mut command = std::process::Command::new(crate::scrape::fno_bin());
    command.args(["do", "pr", "info", &pr_number.to_string()]);
    if !cwd.is_empty() {
        command.current_dir(cwd);
    }
    let out = command.output().ok()?;
    if !out.status.success() {
        return None;
    }
    let payload: Value = serde_json::from_slice(&out.stdout).ok()?;
    payload
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Every fleet row with machine-written identity, promoted leads included.
pub(crate) fn fleet_rows() -> (Vec<FleetRow>, Vec<String>) {
    let raw = crate::claude_roster::read_all_agents_raw();
    let registry = crate::state::load_registry(&AgentsHome::from_env().registry_json())
        .ok()
        .map(|r| r.entries);
    worked::fleet_rows_from(raw, registry.as_deref(), true)
}

/// The sweep's parsed transcripts, one read per row per tick.
pub(crate) fn read_entries(rows: &[FleetRow]) -> BTreeMap<String, Option<Vec<Value>>> {
    let listing = sweep_listing();
    let mut out = BTreeMap::new();
    for row in rows {
        out.insert(
            row.row_id.clone(),
            tail_entries(&row.row_id, &row.cwd, &row.agent, &listing),
        );
    }
    out
}

fn classify(
    rows: &[FleetRow],
    entries: &BTreeMap<String, Option<Vec<Value>>>,
    now_s: f64,
    provider_outages: Option<&Value>,
    silence_after_s: Option<f64>,
    warnings: &mut Vec<String>,
) -> Vec<Verdict> {
    let graph = graph_index();
    if let Err(detail) = &graph {
        warnings.push(format!("graph unreadable for every row: {detail}"));
    }
    let transcript_for = |sid: &str| -> Option<TailFacts> {
        match entries.get(sid) {
            Some(parsed) => facts_from_entries(parsed.as_deref(), TAIL_RECORDS),
            None => None,
        }
    };
    let claim_for = |node: &str| claim_view(node);
    let node_state_for = |node: &str| -> Result<Option<Value>, String> {
        match &graph {
            Ok(index) => Ok(index.get(node).cloned()),
            Err(detail) => Err(detail.clone()),
        }
    };
    let worktree_check = |cwd: &str| worked::is_linked_worktree(cwd);
    let seen: std::cell::RefCell<BTreeMap<(String, i64), Option<String>>> =
        std::cell::RefCell::new(BTreeMap::new());
    let pr_state_for = |cwd: &str, n: i64| -> Option<String> {
        let key = (cwd.to_string(), n);
        if let Some(known) = seen.borrow().get(&key) {
            return known.clone();
        }
        let state = production_pr_state(cwd, n);
        seen.borrow_mut().insert(key, state.clone());
        state
    };
    let commit_count = |cwd: &str| branch_commit_count(cwd);
    let seams = Seams {
        transcript_for: &transcript_for,
        claim_for: &claim_for,
        node_state_for: &node_state_for,
        worktree_check: &worktree_check,
        pr_state_for: &pr_state_for,
        commit_count: &commit_count,
    };
    verdicts(rows, &seams, now_s, provider_outages, silence_after_s)
}

/// Build the real seams and classify the whole fleet once. Rows stay index-aligned
/// with the verdicts. Read-only. A zero-row enumeration sets `refused` and
/// classifies nothing: callers must not write the sweep file or advance the mail
/// gate on it. `provider_outages` reuses a tick's measurement.
pub(crate) fn run_sweep(now_s: f64, provider_outages: Option<Value>) -> (Value, Vec<FleetRow>) {
    let (rows, mut warnings) = fleet_rows();
    let entries = read_entries(&rows);
    let provider_outages = match provider_outages {
        Some(report) => report,
        None => super::outage::measure_provider_outages(&rows, now_s, &entries),
    };
    if rows.is_empty() {
        warnings.push(ROSTER_REFUSAL.to_string());
        return (
            json!({
                "generated_at": utc_stamp(now_s),
                "verdicts": [],
                "counts": {},
                "warnings": warnings,
                "refused": ROSTER_REFUSAL,
                "provider_outages": provider_outages,
            }),
            rows,
        );
    }
    let vs = classify(
        &rows,
        &entries,
        now_s,
        Some(&provider_outages),
        None,
        &mut warnings,
    );
    let mut counts = Map::new();
    for v in &vs {
        let next = counts.get(&v.verdict).and_then(Value::as_i64).unwrap_or(0) + 1;
        counts.insert(v.verdict.clone(), json!(next));
    }
    let terminal = rows
        .iter()
        .filter(|r| TERMINAL_STATES.contains(&r.state.as_str()))
        .count();
    (
        json!({
            "generated_at": utc_stamp(now_s),
            "verdicts": serde_json::to_value(&vs).unwrap_or_else(|_| json!([])),
            "counts": counts,
            "terminal_harness_rows": terminal,
            "warnings": warnings,
            "provider_outages": provider_outages,
        }),
        rows,
    )
}

/// Decode the payload's verdict rows.
pub(crate) fn payload_verdicts(payload: &Value) -> Vec<Verdict> {
    let mut out = Vec::new();
    for v in payload
        .get("verdicts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let field = |key: &str| v.get(key).and_then(Value::as_str).unwrap_or("").to_string();
        out.push(Verdict {
            row_id: field("row_id"),
            name: field("name"),
            state: field("state"),
            verdict: field("verdict"),
            basis: field("basis"),
            action: field("action"),
            agent: field("agent"),
        });
    }
    out
}

/// The silence lane's row set: the registry, not `claude agents --json`.
fn silence_rows(roots: &[PathBuf]) -> Vec<FleetRow> {
    let Ok(registry) = crate::state::load_registry(&AgentsHome::from_env().registry_json()) else {
        return Vec::new();
    };
    let root_strs: Vec<String> = roots
        .iter()
        .map(|r| {
            std::fs::canonicalize(r)
                .unwrap_or_else(|_| r.clone())
                .display()
                .to_string()
        })
        .collect();
    let mut out = Vec::new();
    for e in &registry.entries {
        let live = worked::is_live_status(e.status) && e.harness.as_deref() != Some("codex");
        let spawn = e.origin.as_deref() == Some("spawn") && e.role_level.is_none();
        let node = e.node.clone().filter(|n| !n.is_empty());
        let scope = if e.project_root.is_empty() {
            e.cwd.clone()
        } else {
            e.project_root.clone()
        };
        let (true, true, Some(node), false) = (live, spawn, node, scope.is_empty()) else {
            continue;
        };
        let Ok(resolved) = std::fs::canonicalize(&scope) else {
            continue;
        };
        let resolved = resolved.display().to_string();
        let in_scope = root_strs
            .iter()
            .any(|r| resolved == *r || resolved.starts_with(&format!("{r}/")));
        if !in_scope {
            continue;
        }
        let row_id = e
            .harness_session_id
            .clone()
            .filter(|s| !s.is_empty())
            .or_else(|| (!e.short_id.is_empty()).then(|| e.short_id.clone()))
            .unwrap_or_else(|| e.name.clone());
        out.push(FleetRow {
            row_id,
            name: e.name.clone(),
            state: worked::status_word(e.status).to_string(),
            node: Some(node),
            cwd: e.cwd.clone(),
            agent: e
                .harness
                .clone()
                .filter(|h| !h.is_empty())
                .unwrap_or_else(|| "claude".to_string()),
            stopped_at: None,
            pid: None,
            pid_start_time: None,
            mux: None,
            substrate: None,
        });
    }
    out
}

/// The silence lane's classify step, with the age gate armed.
pub(crate) fn silence_verdicts(roots: &[PathBuf], now_s: f64) -> Vec<(Verdict, FleetRow)> {
    let after =
        crate::agents_config::config_lookup(&cwd(), &["recovery", "idle_threshold_seconds"])
            .and_then(|v| v.as_integer().map(|i| i as f64).or_else(|| v.as_float()))
            .unwrap_or(900.0);
    let rows = silence_rows(roots);
    if rows.is_empty() {
        return Vec::new();
    }
    let registry = crate::state::load_registry(&AgentsHome::from_env().registry_json())
        .ok()
        .map(|r| r.entries);
    let stores = crate::session_truth::Stores::ambient();
    let transcript_for = |row_id: &str| -> Option<TailFacts> {
        let row = rows.iter().find(|r| r.row_id == row_id)?;
        let payload = crate::session_truth::resolve_payload(
            registry.as_deref(),
            &row.name,
            now_s,
            &stores,
            crate::session_truth::cursor::global(),
        );
        let epoch = payload
            .get("last_activity_age_s")
            .and_then(Value::as_f64)
            .map(|age| now_s - age);
        Some(TailFacts {
            last_event_epoch: epoch,
            ..TailFacts::default()
        })
    };
    let claim_for = |node: &str| claim_view(node);
    let graph = graph_index();
    let node_state_for = |node: &str| -> Result<Option<Value>, String> {
        match &graph {
            Ok(index) => Ok(index.get(node).cloned()),
            Err(detail) => Err(detail.clone()),
        }
    };
    let worktree_check = |cwd: &str| worked::is_linked_worktree(cwd);
    let pr_state_for = |_cwd: &str, _n: i64| -> Option<String> { None };
    let commit_count = |cwd: &str| branch_commit_count(cwd);
    let seams = Seams {
        transcript_for: &transcript_for,
        claim_for: &claim_for,
        node_state_for: &node_state_for,
        worktree_check: &worktree_check,
        pr_state_for: &pr_state_for,
        commit_count: &commit_count,
    };
    verdicts(&rows, &seams, now_s, None, Some(after))
        .into_iter()
        .zip(rows.iter().cloned())
        .filter(|(v, _)| v.verdict == SILENCE)
        .collect()
}

pub(crate) fn sweep_path() -> PathBuf {
    crate::state_layout::place(&state_root(), "watchdog-sweep.json")
}

fn read_sweep(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

fn sweep_field(key: &str) -> String {
    read_sweep(&sweep_path())
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

pub(crate) fn last_signature() -> String {
    sweep_field("signature")
}
pub(crate) fn last_events_signature() -> String {
    sweep_field("events_signature")
}
pub(crate) fn last_unfinished_signature() -> String {
    sweep_field("unfinished_signature")
}
pub(crate) fn last_unfinished_events_signature() -> String {
    sweep_field("unfinished_events_signature")
}
pub(crate) fn last_recovery_events_signature() -> String {
    sweep_field("recovery_events_signature")
}

/// What one writer stamps; `None` carries the previous value through, because a
/// write from another lane must not speak for this one.
#[derive(Default)]
pub(crate) struct SweepWrite {
    pub(crate) counts: Option<Value>,
    pub(crate) signature: Option<String>,
    pub(crate) events_signature: Option<String>,
    pub(crate) terminal_harness_rows: Option<i64>,
    pub(crate) recoverable_count: Option<i64>,
    pub(crate) unfinished: Option<Value>,
    pub(crate) recovery_events_signature: Option<String>,
    pub(crate) provider_outages: Option<Value>,
}

/// Freshness evidence for the done probe, best-effort. The cadence stamp survives
/// a hand-run: one file serves both cadences, so a manual write must not erase the
/// only evidence the tick ran.
pub(crate) fn write_sweep_file(source: &str, now_s: f64, write: SweepWrite) {
    let path = sweep_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let previous = read_sweep(&path);
    let prev_str = |key: &str| -> String {
        previous
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    let last_tick: Option<f64> = if source == "tick" {
        Some(now_s)
    } else {
        previous.get("last_tick_epoch").and_then(Value::as_f64)
    };
    let mut payload = Map::new();
    payload.insert("source".into(), json!(source));
    payload.insert("at".into(), json!(utc_stamp(now_s)));
    payload.insert(
        "counts".into(),
        write
            .counts
            .or_else(|| previous.get("counts").cloned().filter(Value::is_object))
            .unwrap_or_else(|| json!({})),
    );
    payload.insert(
        "terminal_harness_rows".into(),
        json!(write.terminal_harness_rows.unwrap_or_else(|| {
            previous
                .get("terminal_harness_rows")
                .and_then(Value::as_i64)
                .unwrap_or(0)
        })),
    );
    payload.insert(
        "signature".into(),
        json!(write.signature.unwrap_or_else(|| prev_str("signature"))),
    );
    payload.insert(
        "events_signature".into(),
        json!(write
            .events_signature
            .unwrap_or_else(|| prev_str("events_signature"))),
    );
    payload.insert(
        "provider_outages".into(),
        write
            .provider_outages
            .or_else(|| previous.get("provider_outages").cloned())
            .unwrap_or_else(|| unknown_provider_report("provider_outage_report_missing")),
    );
    if let Some(count) = write
        .recoverable_count
        .or_else(|| previous.get("recoverable_count").and_then(Value::as_i64))
    {
        payload.insert("recoverable_count".into(), json!(count));
    }
    match write.unfinished {
        Some(unfinished) => {
            payload.insert(
                "unfinished_counts".into(),
                unfinished
                    .get("counts")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
            );
            payload.insert(
                "unfinished_work_complete".into(),
                json!(unfinished
                    .get("complete")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)),
            );
            for key in ["signature", "events_signature"] {
                payload.insert(
                    format!("unfinished_{key}"),
                    json!(unfinished.get(key).and_then(Value::as_str).unwrap_or("")),
                );
            }
        }
        None => {
            for key in [
                "unfinished_counts",
                "unfinished_work_complete",
                "unfinished_signature",
                "unfinished_events_signature",
            ] {
                if let Some(value) = previous.get(key) {
                    payload.insert(key.to_string(), value.clone());
                }
            }
        }
    }
    let recovery = write.recovery_events_signature.or_else(|| {
        previous
            .get("recovery_events_signature")
            .and_then(Value::as_str)
            .map(str::to_string)
    });
    if let Some(signature) = recovery {
        payload.insert("recovery_events_signature".into(), json!(signature));
    }
    if let Some(epoch) = last_tick {
        payload.insert("last_tick_epoch".into(), json!(epoch));
    }
    let _ = std::fs::write(&path, Value::Object(payload).to_string());
}

/// Row ids whose non-leave verdict the event lane has not already published.
pub(crate) fn fresh_non_leave(payload: &Value, prev_signature: &str) -> BTreeSet<String> {
    let prev: BTreeSet<&str> = prev_signature
        .split(';')
        .filter(|p| !p.is_empty())
        .collect();
    payload_verdicts(payload)
        .into_iter()
        .filter(|v| {
            v.verdict != LEAVE && !prev.contains(format!("{}:{}", v.row_id, v.verdict).as_str())
        })
        .map(|v| v.row_id)
        .collect()
}

/// Stable identity of the non-leave verdict set, so the mail lane speaks only on
/// change: a row stuck for a day reads once.
pub(crate) fn verdict_signature(payload: &Value) -> String {
    let mut parts: Vec<String> = payload_verdicts(payload)
        .into_iter()
        .filter(|v| v.verdict != LEAVE)
        .map(|v| format!("{}:{}", v.row_id, v.verdict))
        .collect();
    parts.sort();
    let terminal = payload
        .get("terminal_harness_rows")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if terminal != 0 {
        parts.push(format!("terminal-harness-rows:{terminal}"));
    }
    for breaker in payload
        .pointer("/provider_outages/breakers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        parts.push(breaker_part(breaker));
    }
    parts.join(";")
}

fn json_word(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// One breaker's signature part. A missing key renders `None`, as the published
/// signature has always spelled it, so a stamp survives the port unchanged.
pub(crate) fn breaker_part(breaker: &Value) -> String {
    format!(
        "provider-breaker:{}:{}:{}",
        json_word(breaker.get("provider")),
        json_word(breaker.get("account")),
        json_word(breaker.get("outage_epoch"))
    )
}

/// Merge signatures, keeping the sorted-parts shape. A filtered hand-run publishes
/// a subset, so its stamp must say "these, and whatever was already said".
pub(crate) fn union_signature(signatures: &[&str]) -> String {
    let parts: BTreeSet<&str> = signatures
        .iter()
        .flat_map(|s| s.split(';'))
        .filter(|p| !p.is_empty())
        .collect();
    parts.into_iter().collect::<Vec<_>>().join(";")
}

/// One-screen digest. Verdict rows are list items: the mail style gate reads a
/// bare line under a paragraph as an illegal mid-paragraph wrap.
pub(crate) fn digest_text(payload: &Value, limit: usize) -> String {
    let vs = payload_verdicts(payload);
    let mut counts: Vec<String> = payload
        .get("counts")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(k, v)| format!("{k}={}", json_word(Some(v))))
                .collect()
        })
        .unwrap_or_default();
    counts.sort();
    let terminal = payload
        .get("terminal_harness_rows")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let mut lines = vec![
        format!(
            "fleet watchdog swept {} rows. {} terminal harness rows: {terminal}",
            vs.len(),
            counts.join(" ")
        ),
        String::new(),
    ];
    let non_leave: Vec<&Verdict> = vs.iter().filter(|v| v.verdict != LEAVE).collect();
    for v in non_leave.iter().take(limit) {
        lines.push(format!("- {} {}: {}", v.verdict, v.name, v.basis));
    }
    if non_leave.len() > limit {
        lines.push(format!(
            "- {} more row(s) not shown",
            non_leave.len() - limit
        ));
    }
    lines.join("\n")
}

/// Send a generated report through the native mail send. A report's measured rows
/// can exceed the authored-prose budget, so it declares the reasoned exception.
pub(crate) fn send_machine_report(to: &str, body: &str) -> (bool, String) {
    let mut command = std::process::Command::new(crate::scrape::fno_bin());
    command.args([
        "agents",
        "mail",
        "send",
        "--origin",
        "recovery",
        "--style-exception",
        "generated watchdog report",
    ]);
    // A watchdog daemon can run outside any checkout; the send picks its neutral
    // sender rather than dropping the alert at resolution.
    if let Ok(project) = crate::law_match::resolve_project(
        Some(cwd().as_path()),
        &crate::law_match::settings_sources(),
    ) {
        command.args(["--from-name", &project]);
    }
    match to.strip_prefix("project:") {
        Some(project) => {
            command.args(["--to-project", project]);
        }
        None => {
            command.arg(to);
        }
    }
    command.args(["--body", body]);
    match command.output() {
        Err(exc) => (false, format!("mail send failed: {exc}")),
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let receipt = if stdout.is_empty() { stderr } else { stdout };
            (out.status.success(), receipt)
        }
    }
}

/// `(ok, receipt, signature_to_stamp)`: a caller cannot stamp a digest it never
/// delivered. The stamp is the current signature only after a settled-ok mail; a
/// failed send keeps the previous one so the next sweep retries.
pub(crate) fn mail_gate(payload: &Value, to: &str) -> (bool, String, String) {
    if to.is_empty() {
        return (true, "no recipient".to_string(), last_signature());
    }
    if let Some(refused) = payload.get("refused").and_then(Value::as_str) {
        return (false, refused.to_string(), last_signature());
    }
    let signature = verdict_signature(payload);
    let non_leave = payload_verdicts(payload)
        .iter()
        .filter(|v| v.verdict != LEAVE)
        .count();
    let terminal = payload
        .get("terminal_harness_rows")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if non_leave == 0 && terminal == 0 {
        return (
            true,
            "all rows leave, nothing to say".to_string(),
            signature,
        );
    }
    if signature == last_signature() {
        return (
            true,
            "unchanged since the last sweep, not mailed".to_string(),
            signature,
        );
    }
    let (ok, receipt) = send_machine_report(to, &digest_text(payload, 8));
    let stamp = if ok { signature } else { last_signature() };
    (ok, receipt, stamp)
}

/// Human lines for one outage report. A clean zero means measured and clear.
pub(crate) fn provider_outage_lines(report: Option<&Value>) -> Vec<String> {
    let mut lines = Vec::new();
    let Some(report) = report else {
        return lines;
    };
    for breaker in report
        .get("breakers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        lines.push(format!(
            "provider outage open: {}/{} kind={} reset_at={}",
            json_word(breaker.get("provider")),
            json_word(breaker.get("account")),
            json_word(breaker.get("kind")),
            json_word(breaker.get("reset_at")),
        ));
    }
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    for item in report
        .get("refusals")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        *counts.entry(json_word(item.get("reason"))).or_insert(0) += 1;
    }
    if !counts.is_empty() {
        let named: Vec<String> = counts.iter().map(|(k, v)| format!("{k}={v}")).collect();
        lines.push(format!("provider-outage refusals ({})", named.join(" ")));
    }
    lines
}
