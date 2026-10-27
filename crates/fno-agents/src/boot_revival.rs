//! Bring the fleet back after a reboot.
//!
//! On the first daemon start of a boot, every registry worker that was live
//! at the boot comes back without a tap: `claude agents --json --all` lists
//! its job as stopped or failed, so `claude respawn <job id>` restarts it.
//! A worker whose node is done or superseded stays stopped. Leads go
//! first. A revival re-seats a row that already held a seat, so it never
//! asks the spawn gate. One receipt row per worker lands in
//! `<agents home>/boot-revival.json`, and that file's boot stamp is what
//! makes the pass run once per boot.
//!
//! The receipt is not the whole delivery: after the pass, a failed row
//! reaches its spawned_by lead as durable mail (reason, re-spawn line,
//! stale claim), and a revived worker reaches its own row with its open PR
//! and that PR's CI verdict, so nobody wakes idle.

use std::collections::HashSet;
use std::path::Path;

use serde::Serialize;
use serde_json::{json, Value};

use crate::claude_roster::ClaudeAgentsSnapshot;
use crate::paths::AgentsHome;
use crate::state::RegistryEntry;

const RECEIPT_FILE: &str = "boot-revival.json";

/// The sender stamp the pass's mail carries, so a drain attributes the rows
/// to the boot pass the way `lead_mail::SETTLE_SENDER` does for the settle
/// arm.
pub(crate) const SENDER: &str = "fno/boot-revival";

/// macOS moves `kern.boottime` when the wall clock is stepped, so two reads
/// in one boot can differ by a few seconds.
const BOOT_DRIFT_S: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReceiptRow {
    pub(crate) name: String,
    /// "revived", "skipped" or "failed".
    pub(crate) outcome: &'static str,
    pub(crate) reason: String,
}

impl ReceiptRow {
    fn new(name: &str, outcome: &'static str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            outcome,
            reason: reason.into(),
        }
    }
}

/// Plan the pass once, then revive off the caller's thread. Planning reads
/// the registry before the startup sweep rewrites a single status, so a row
/// still reads the way it did when the machine went down. A second daemon
/// start in the same boot finds the receipt and does nothing.
pub(crate) fn start(home: &AgentsHome) {
    let Some(boot) = boot_time() else {
        return;
    };
    let receipt = home.root().join(RECEIPT_FILE);
    if ran_this_boot(recorded_boot(&receipt), boot) {
        return;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    let listing = crate::claude_roster::read_all_agents_union();
    // An unread listing reads every job as unlisted. Stamping the boot then
    // would skip the whole fleet for good, so the next daemon start retries.
    if !listing.is_known() || !listing.warning_text().is_empty() {
        eprintln!(
            "boot revival: claude agents --json --all unread ({}); retrying on the next daemon start",
            listing.warning_text()
        );
        return;
    }
    let (closed, merged) = closed_and_merged_nodes();
    let deliberate = deliberate_stops(home);
    let (leads, workers, mut rows) = plan(
        &registry.entries,
        &listing,
        &closed,
        &merged,
        &deliberate,
        boot,
    );
    // Stamp the boot first: a daemon that dies mid-pass must not respawn the
    // fleet again on its next start. The tap is the fallback for a row the
    // pass never reached.
    write_receipt(&receipt, boot, &rows);
    let home = home.clone();
    std::thread::spawn(move || {
        for name in &leads {
            rows.push(revive(&home, name));
        }
        let handles: Vec<_> = workers
            .into_iter()
            .map(|name| {
                let home = home.clone();
                std::thread::spawn(move || revive(&home, &name))
            })
            .collect();
        for handle in handles {
            if let Ok(row) = handle.join() {
                rows.push(row);
            }
        }
        write_receipt(&receipt, boot, &rows);
        notify(&home, &rows);
    });
}

/// Post-pass delivery: a failed row reaches its spawned_by lead with the
/// reason, the exact re-spawn line and the stale claim to release; a revived
/// worker reaches its own row with its open PR and that PR's CI verdict. A
/// send failure prints and never unwinds the pass - the revival already
/// happened, and a mail outage must not read as one.
fn notify(home: &AgentsHome, rows: &[ReceiptRow]) {
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    for row in rows.iter().filter(|r| r.outcome == "failed") {
        let Some(worker) = registry.entries.iter().find(|e| e.name == row.name) else {
            continue;
        };
        if let Err(error) = mail_failure(home, &registry.entries, worker, &row.reason) {
            eprintln!("boot revival: lead mail for {} unsent ({error})", row.name);
        }
    }
    for row in rows.iter().filter(|r| r.outcome == "revived") {
        let Some(worker) = registry.entries.iter().find(|e| e.name == row.name) else {
            continue;
        };
        // A lead's wake rides its own loop; the idle-wake gap is a worker's.
        if worker.role_level.is_some() {
            continue;
        }
        if let Err(error) = mail_wake(worker) {
            eprintln!("boot revival: wake mail for {} unsent ({error})", row.name);
        }
    }
}

/// One durable envelope to the lead the worker's spawned_by session names,
/// falling back to the row's scope team. Neither resolves: receipt-only,
/// the mail cannot invent an address.
fn mail_failure(
    home: &AgentsHome,
    entries: &[RegistryEntry],
    worker: &RegistryEntry,
    reason: &str,
) -> Result<(), String> {
    let body = failure_body(home, worker, reason);
    match spawned_lead_name(entries, worker) {
        Some(lead) => crate::lead_mail::send_to_name(SENDER, &lead, &body),
        None => match worker.role_scope.as_deref() {
            Some(scope) => crate::lead_mail::send(SENDER, scope, &body).map(|_| ()),
            None => Err("no spawner row and no scope on the worker row".to_string()),
        },
    }
}

/// The registry row the worker's `spawned_by_session` names. The full
/// session id is the join key (the registry's keying law); a finished row
/// loses to a live one, and the first row wins among equals.
fn spawned_lead_name(entries: &[RegistryEntry], worker: &RegistryEntry) -> Option<String> {
    let sid = worker.spawned_by_session.as_deref()?;
    let mut matches: Vec<&RegistryEntry> = entries
        .iter()
        .filter(|e| e.harness_session_id.as_deref() == Some(sid))
        .collect();
    matches.sort_by_key(|e| !crate::daemon::is_non_terminal(e.status));
    matches.first().map(|e| e.name.clone())
}

/// The lead's brief: who failed and why, the exact line to bring it back,
/// and the stale claim to release so the board stops counting a ghost
/// driver.
fn failure_body(home: &AgentsHome, worker: &RegistryEntry, reason: &str) -> String {
    let node = worker.node.as_deref().unwrap_or("none");
    let mut lines = vec![
        format!(
            "boot revival could not bring back {} (node {node}).",
            worker.name
        ),
        format!("reason: {reason}"),
        format!("re-spawn: {}", respawn_line(home, worker)),
    ];
    if let Some(node) = worker.node.as_deref() {
        lines.push(format!(
            "stale claim to release: fno agents claim release node:{node} --force --reason \"boot revival failed this worker\""
        ));
    }
    lines.join("\n")
}

/// The canonical re-entry plan's argv when one resolves; otherwise the
/// row-name resume form the row's own route settings serve. Best effort by
/// design: the lead reruns or repairs it.
fn respawn_line(home: &AgentsHome, worker: &RegistryEntry) -> String {
    if let Ok(plan) = crate::reentry::resolve_reentry(
        &home.registry_json(),
        &worker.name,
        crate::reentry::ReentryTransition::Revive,
        None,
        None,
    ) {
        return plan.argv.join(" ");
    }
    match worker
        .harness_session_id
        .as_deref()
        .filter(|s| !s.is_empty())
    {
        Some(_) => format!("fno agents session resume {}", worker.name),
        None => "no session id on the row; see fno-agents reentry-plan".to_string(),
    }
}

/// One line to the revived worker: its node, the open PR on it, that PR's
/// CI verdict, and the watcher to re-arm. A node with no recorded PR sends
/// nothing - a worker between ships wakes to its node context alone.
fn mail_wake(worker: &RegistryEntry) -> Result<(), String> {
    let Some(node) = worker.node.as_deref() else {
        return Ok(());
    };
    let Some((number, url, merged)) = open_pr(node) else {
        return Ok(());
    };
    let verdict = if merged {
        "merged".to_string()
    } else {
        ci_verdict(worker, number)
    };
    let body = format!(
        "Revived after the reboot. Node {node}: {url} CI {verdict}. Re-arm your watcher, continue."
    );
    crate::lead_mail::send_to_name(SENDER, &worker.name, &body)
}

/// The node's recorded PR: number, url, and whether the primary ship already
/// merged. A read failure answers None - the wake line does not invent a PR.
fn open_pr(node: &str) -> Option<(u64, String, bool)> {
    let rows = crate::graph_store::read_rows_where(
        &crate::graph_get::default_graph_path(),
        &crate::backlog::RowQuery {
            fields: Some(
                ["id", "pr_number", "pr_url", "merge_status"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            with_blockers: false,
            ..Default::default()
        },
    )
    .ok()?;
    let row = rows
        .iter()
        .find(|r| crate::graph_store::entry_id(r) == Some(node))?;
    let number = row.get("pr_number").and_then(Value::as_u64)?;
    let url = row.get("pr_url").and_then(Value::as_str)?.to_string();
    let merged = row.get("merge_status").and_then(Value::as_str) == Some("merged");
    Some((number, url, merged))
}

/// The PR's CI verdict through the status door's cache - the same read the
/// merge guard's checks read answers through - or "unread" when the door
/// refuses. The wake line never guesses a verdict it could not read.
fn ci_verdict(worker: &RegistryEntry, pr: u64) -> String {
    let (_code, stdout, _stderr) =
        crate::pr_status::cache::run_door("status-read", &json!({ "cwd": worker.cwd, "pr": pr }));
    serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|v| v.get("verdict").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "unread".to_string())
}

/// The failed rows of THIS boot, for the lead check-in's reading. A receipt
/// from an older boot, or no receipt at all, reads empty: a stale failure is
/// not this boot's news. `None` means the receipt exists but is unreadable -
/// the caller renders that as a failed reading, never a silent zero.
pub(crate) fn failed_rows_this_boot(home: &AgentsHome) -> Option<Vec<ReceiptRow>> {
    let receipt = home.root().join(RECEIPT_FILE);
    let raw = match std::fs::read_to_string(&receipt) {
        Ok(raw) => raw,
        // No pass has run this boot: clean, not failed.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        // Exists but unreadable: unmeasured, never a silent zero.
        return None;
    };
    let boot = boot_time()?;
    if !ran_this_boot(v.get("boot").and_then(Value::as_u64), boot) {
        return Some(Vec::new());
    }
    let Some(rows) = v.get("rows").and_then(Value::as_array) else {
        return Some(Vec::new());
    };
    Some(
        rows.iter()
            .filter_map(|r| {
                let name = r.get("name")?.as_str()?.to_string();
                let reason = r.get("reason")?.as_str()?.to_string();
                (r.get("outcome")?.as_str()? == "failed")
                    .then_some(ReceiptRow::new(&name, "failed", reason))
            })
            .collect(),
    )
}

/// Split the registry into leads to revive, workers to revive, and the rows
/// skipped with their reason. A row counts only when it was live at the
/// boot: a live-ish status, or an exit stamped after the boot began.
pub(crate) fn plan(
    entries: &[RegistryEntry],
    listing: &ClaudeAgentsSnapshot,
    closed_nodes: &HashSet<String>,
    merged_nodes: &HashSet<String>,
    deliberate_stops: &HashSet<String>,
    boot: u64,
) -> (Vec<String>, Vec<String>, Vec<ReceiptRow>) {
    let mut leads = Vec::new();
    let mut workers = Vec::new();
    let mut skipped = Vec::new();
    for e in entries {
        if e.harness_name() != "claude" || e.mux.is_some() {
            continue;
        }
        let exited_after_boot = e
            .exited_at
            .as_deref()
            .and_then(crate::state::rfc3339_like_to_secs)
            .is_some_and(|at| at >= boot);
        if !crate::daemon::is_non_terminal(e.status) && !exited_after_boot {
            continue;
        }
        let Some(sid) = e.harness_session_id.as_deref().filter(|s| s.len() >= 8) else {
            skipped.push(ReceiptRow::new(
                &e.name,
                "skipped",
                "no session id recorded",
            ));
            continue;
        };
        if let Some(node) = e.node.as_deref().filter(|n| closed_nodes.contains(*n)) {
            skipped.push(ReceiptRow::new(
                &e.name,
                "skipped",
                format!("node {node} is done or superseded"),
            ));
            continue;
        }
        let job: String = sid.chars().take(8).collect();
        let state = listing.find(&job).and_then(|row| row.state.clone());
        match state.as_deref() {
            Some("stopped" | "failed") => {
                if e.role_level.is_some() {
                    leads.push(e.name.clone());
                } else if let Some(node) = e
                    .node
                    .as_deref()
                    .filter(|n| merged_nodes.contains(*n))
                    .filter(|_| deliberate_stop(e, deliberate_stops))
                {
                    skipped.push(ReceiptRow::new(
                        &e.name,
                        "skipped",
                        format!(
                            "node {node}'s PR merged; worker stopped on purpose; never revived"
                        ),
                    ));
                } else {
                    workers.push(e.name.clone());
                }
            }
            Some(other) => skipped.push(ReceiptRow::new(
                &e.name,
                "skipped",
                format!("claude agents lists job {job} as {other}"),
            )),
            None => skipped.push(ReceiptRow::new(
                &e.name,
                "skipped",
                format!("claude agents does not list job {job}"),
            )),
        }
    }
    (leads, workers, skipped)
}

/// One revival through the canonical re-entry plan: the listing lists the
/// job, so the plan is `claude respawn <job id>`.
fn revive(home: &AgentsHome, name: &str) -> ReceiptRow {
    let plan = match crate::reentry::resolve_reentry(
        &home.registry_json(),
        name,
        crate::reentry::ReentryTransition::Resume,
        None,
        None,
    ) {
        Ok(plan) => plan,
        Err(reason) => return ReceiptRow::new(name, "failed", reason),
    };
    match crate::resume_wake::run_and_confirm_respawn(&plan, name, "revive", "agent_revived", home)
    {
        0 => ReceiptRow::new(name, "revived", plan.argv.join(" ")),
        code => ReceiptRow::new(
            name,
            "failed",
            format!("{} exited {code}", plan.argv.join(" ")),
        ),
    }
}

/// The nodes a revival must leave alone: done or superseded. A merged PR
/// under a live node is a multi-ship node mid-flight; its worker comes back
/// UNLESS someone stopped it through fno (a deliberate stop, proven by a
/// `StopRecord` or an `agent_stopped` event): a worker whose work delivered
/// is never revived, whoever stopped it. The node's own status stays the
/// authority for closure - the primary PR's merge state once read a
/// multi-ship node closed and left its worker down during the 2026-10-10
/// reboot.
fn closed_row(row: &Value) -> bool {
    crate::graph_store::is_terminal_entry(row)
}

/// True when the row proves a stop someone ran through fno: a `StopRecord`
/// on the row, or an `agent_stopped` event naming it. A reboot death
/// carries neither, which is what keeps the multi-ship worker coming back.
fn deliberate_stop(e: &RegistryEntry, evidence: &HashSet<String>) -> bool {
    e.stop.is_some() || evidence.contains(&e.name)
}

/// Every name an `agent_stopped` event ever carried, ageless: the stop
/// evidence is durable-class (never auto-expires), so the whole store
/// answers. A failed read empties the set and says so - the pass proceeds
/// and only the merged-PR skip loses its proof.
fn deliberate_stops(home: &AgentsHome) -> HashSet<String> {
    let query = crate::event_store::EventQuery::of_types(&["agent_stopped"]);
    let rows = match crate::event_store::query_events(
        &crate::daemon::global_events_path(home),
        &query,
    ) {
        Ok(rows) => rows,
        Err(error) => {
            eprintln!(
                "boot revival: agent_stopped read failed ({error}); merged-PR skips lose their stop proof"
            );
            return Default::default();
        }
    };
    rows.into_iter()
        .filter_map(|row| {
            let value: Value = serde_json::from_str(&row.line).ok()?;
            value
                .get("data")
                .unwrap_or(&value)
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The closed and merged sets for this boot's pass.
fn closed_and_merged_nodes() -> (HashSet<String>, HashSet<String>) {
    let rows = crate::graph_store::read_rows_where(
        &crate::graph_get::default_graph_path(),
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "deferred_kind",
                    "superseded_by",
                    "completed_at",
                    "id",
                    "status",
                    "merge_status",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let mut closed = HashSet::new();
    let mut merged = HashSet::new();
    for row in &rows {
        let Some(id) = crate::graph_store::entry_id(row) else {
            continue;
        };
        if closed_row(row) {
            closed.insert(id.to_string());
        }
        if row.get("merge_status").and_then(Value::as_str) == Some("merged") {
            merged.insert(id.to_string());
        }
    }
    (closed, merged)
}

fn ran_this_boot(recorded: Option<u64>, boot: u64) -> bool {
    recorded.is_some_and(|at| at.abs_diff(boot) <= BOOT_DRIFT_S)
}

fn recorded_boot(receipt: &Path) -> Option<u64> {
    let raw = std::fs::read_to_string(receipt).ok()?;
    serde_json::from_str::<Value>(&raw)
        .ok()?
        .get("boot")
        .and_then(Value::as_u64)
}

fn write_receipt(receipt: &Path, boot: u64, rows: &[ReceiptRow]) {
    let body = serde_json::json!({ "boot": boot, "rows": rows });
    let tmp = receipt.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, receipt);
    }
}

/// This boot's start, in epoch seconds.
fn boot_time() -> Option<u64> {
    if cfg!(target_os = "linux") {
        let stat = std::fs::read_to_string("/proc/stat").ok()?;
        return stat
            .lines()
            .find_map(|line| line.strip_prefix("btime "))
            .and_then(|n| n.trim().parse().ok());
    }
    let out = std::process::Command::new("sysctl")
        .args(["-n", "kern.boottime"])
        .output()
        .ok()?;
    parse_boottime(&String::from_utf8_lossy(&out.stdout))
}

/// `{ sec = 1790338000, usec = 123 } Thu Sep 24 ...` -> 1790338000.
fn parse_boottime(text: &str) -> Option<u64> {
    let rest = text.split("sec = ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claude_roster::ClaudeAgentRow;
    use crate::AgentStatus;

    const BOOT: u64 = 1_790_338_000;

    fn row(name: &str, sid: &str, status: AgentStatus) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            harness: Some("claude".into()),
            harness_session_id: Some(sid.into()),
            status,
            ..Default::default()
        }
    }

    #[test]
    fn the_boot_pass_revives_stopped_and_failed_workers_leads_first() {
        let mut lead = row("quill", "99473043-aaaa", AgentStatus::Live);
        lead.role_level = Some(1);
        let failed = row("kestrel", "11112222-bbbb", AgentStatus::Orphaned);
        // Exited by this boot's own daemon: it was live when the machine
        // went down.
        let mut exited_now = row("folio", "33334444-cccc", AgentStatus::Exited);
        exited_now.exited_at = Some("2026-09-26T02:00:00Z".into());
        // Exited long before the boot: a deliberate stop stays stopped.
        let mut stopped_before = row("old", "55556666-dddd", AgentStatus::Exited);
        stopped_before.exited_at = Some("2026-01-01T00:00:00Z".into());
        let mut done_node = row("shipped", "77778888-eeee", AgentStatus::Live);
        done_node.node = Some("x-done".into());
        let running = row("candor", "9999aaaa-ffff", AgentStatus::Live);
        let unlisted = row("warden", "49a80492-0000", AgentStatus::Live);
        let listing = ClaudeAgentsSnapshot::known(vec![
            ClaudeAgentRow::new("99473043", Some("stopped")),
            ClaudeAgentRow::new("11112222", Some("failed")),
            ClaudeAgentRow::new("33334444", Some("stopped")),
            ClaudeAgentRow::new("55556666", Some("stopped")),
            ClaudeAgentRow::new("77778888", Some("stopped")),
            ClaudeAgentRow::new("9999aaaa", Some("working")),
        ]);
        let closed = HashSet::from(["x-done".to_string()]);
        let boot = crate::state::rfc3339_like_to_secs("2026-09-26T01:00:00Z").unwrap();
        let (leads, workers, skipped) = plan(
            &[
                lead,
                failed,
                exited_now,
                stopped_before,
                done_node,
                running,
                unlisted,
            ],
            &listing,
            &closed,
            &HashSet::new(),
            &HashSet::new(),
            boot,
        );
        assert_eq!(leads, vec!["quill"]);
        assert_eq!(workers, vec!["kestrel", "folio"]);
        let names: Vec<_> = skipped.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["shipped", "candor", "warden"], "{skipped:?}");
        assert!(skipped.iter().all(|r| r.outcome == "skipped"));
        assert!(skipped[0].reason.contains("x-done"), "{skipped:?}");
        assert!(skipped[1].reason.contains("working"), "{skipped:?}");
    }

    #[test]
    fn a_deliberately_stopped_worker_on_a_merged_node_stays_down() {
        let mut stopped_by_record = row("kestrel", "11112222-bbbb", AgentStatus::Live);
        stopped_by_record.node = Some("x-merged".into());
        crate::state::record_stop(&mut stopped_by_record, "lead", None);
        let mut stopped_by_event = row("quill", "9999aaaa-aaaa", AgentStatus::Live);
        stopped_by_event.node = Some("x-merged".into());
        let mut reboot_death = row("folio", "33334444-cccc", AgentStatus::Live);
        reboot_death.node = Some("x-merged".into());
        let mut lead = row("lead-quill", "aaaa1111-lead", AgentStatus::Live);
        lead.role_level = Some(1);
        lead.node = Some("x-merged".into());
        crate::state::record_stop(&mut lead, "operator", None);
        let listing = ClaudeAgentsSnapshot::known(vec![
            ClaudeAgentRow::new("11112222", Some("stopped")),
            ClaudeAgentRow::new("9999aaaa", Some("stopped")),
            ClaudeAgentRow::new("33334444", Some("stopped")),
            ClaudeAgentRow::new("aaaa1111", Some("stopped")),
        ]);
        let merged = HashSet::from(["x-merged".to_string()]);
        let evidence = HashSet::from(["quill".to_string()]);
        let (leads, workers, skipped) = plan(
            &[stopped_by_record, stopped_by_event, reboot_death, lead],
            &listing,
            &HashSet::new(),
            &merged,
            &evidence,
            BOOT,
        );
        assert_eq!(leads, vec!["lead-quill"], "a lead comes back regardless");
        assert_eq!(
            workers,
            vec!["folio"],
            "no stop proof reads as a reboot death: the worker comes back"
        );
        let names: Vec<_> = skipped.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["kestrel", "quill"], "{skipped:?}");
        assert!(
            skipped
                .iter()
                .all(|r| r.reason.contains("merged") && r.reason.contains("never revived")),
            "{skipped:?}"
        );
    }

    #[test]
    fn the_done_check_reads_node_status() {
        let in_progress_merged =
            json!({"id": "x-a", "status": "in_progress", "merge_status": "merged"});
        let done = json!({"id": "x-b", "status": "done"});
        let superseded = json!({"id": "x-c", "superseded_by": "x-d"});
        assert!(!closed_row(&in_progress_merged));
        assert!(closed_row(&done));
        assert!(closed_row(&superseded));
    }

    #[test]
    fn a_failed_row_names_its_spawner_and_the_respawn() {
        let mut spawner = row("lead-quill", "aaaa1111-lead", AgentStatus::Live);
        spawner.role_level = Some(1);
        let mut dead_lead = row("lead-old", "aaaa1111-lead", AgentStatus::Exited);
        dead_lead.exited_at = Some("2026-01-01T00:00:00Z".into());
        let mut worker = row("kestrel", "22223333-worker", AgentStatus::Orphaned);
        worker.spawned_by_session = Some("aaaa1111-lead".into());
        worker.node = Some("x-1".into());
        worker.harness_session_id = Some("22223333-worker".into());
        worker.route_provider_id = Some("zai".into());
        worker.model_name = Some("glm-5.3-flash[1m]".into());
        let entries = vec![spawner, dead_lead, worker.clone()];
        // The live spawner row wins over the finished one for the same sid.
        assert_eq!(
            spawned_lead_name(&entries, &worker).unwrap(),
            "lead-quill".to_string()
        );
        let mut worker_no_lead = worker.clone();
        worker_no_lead.spawned_by_session = Some("ffff0000-gone".into());
        assert!(spawned_lead_name(&entries, &worker_no_lead).is_none());
        let home = AgentsHome::at(tempfile::tempdir().unwrap().keep());
        let body = failure_body(&home, &worker, "launch account zai rides an api-key lane");
        assert!(body.contains("kestrel"), "{body}");
        assert!(body.contains("x-1"), "{body}");
        assert!(body.contains("reason: launch account zai"), "{body}");
        assert!(body.contains("fno agents session resume kestrel"), "{body}");
        assert!(body.contains("claim release node:x-1 --force"), "{body}");
    }

    #[test]
    fn failed_rows_this_boot_reads_this_boot_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(home.root()).unwrap();
        let receipt = dir.path().join(RECEIPT_FILE);
        let this_boot = boot_time().unwrap();
        let rows = serde_json::json!([
            {"name": "kestrel", "outcome": "failed", "reason": "zai lane cannot ride a re-entry plan"},
            {"name": "folio", "outcome": "revived", "reason": "claude respawn 33334444"}
        ]);
        std::fs::write(
            &receipt,
            serde_json::json!({"boot": this_boot, "rows": rows}).to_string(),
        )
        .unwrap();
        let failed = failed_rows_this_boot(&home).unwrap();
        assert_eq!(failed.len(), 1, "{failed:?}");
        assert_eq!(failed[0].name, "kestrel");
        assert!(failed[0].reason.contains("zai lane"));
        // A previous boot's receipt is not this boot's news.
        std::fs::write(
            &receipt,
            serde_json::json!({"boot": this_boot - 86_400, "rows": rows}).to_string(),
        )
        .unwrap();
        assert!(failed_rows_this_boot(&home).unwrap().is_empty());
        // A corrupted receipt reads unmeasured, never a silent zero.
        std::fs::write(&receipt, "{not json").unwrap();
        assert!(failed_rows_this_boot(&home).is_none());
        // No receipt at all is a clean none.
        std::fs::remove_file(&receipt).unwrap();
        assert!(failed_rows_this_boot(&home).unwrap().is_empty());
    }

    #[test]
    fn the_receipt_stamp_makes_the_pass_run_once_per_boot() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join(RECEIPT_FILE);
        assert!(!ran_this_boot(recorded_boot(&receipt), BOOT));
        write_receipt(&receipt, BOOT, &[]);
        assert!(ran_this_boot(recorded_boot(&receipt), BOOT));
        // A clock step nudges kern.boottime; it is still the same boot.
        assert!(ran_this_boot(recorded_boot(&receipt), BOOT + 3));
        assert!(!ran_this_boot(recorded_boot(&receipt), BOOT + 86_400));
    }
}
