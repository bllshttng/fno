//! Bring the fleet back after a reboot.
//!
//! On the first daemon start of a boot, every registry worker that was live
//! at the boot comes back without a retire-arm tap. A claude row answers the
//! `claude agents --json --all` listing: it names the job stopped or failed,
//! and `claude respawn <job id>` restarts it. A keeper-lane row of a harness
//! whose `[harness.<h>.keeper]` carries `resume_session_id` (agy,
//! cursor-agent) never rides that listing - it never lists their jobs - and
//! comes back through its own keeper revival on the contract's resume form.
//! A worker whose node is done or superseded stays stopped.
//! Leads go first. A revival re-seats a row that already held a seat, so it
//! never asks the spawn gate. One receipt row per worker lands in
//! `<agents home>/boot-revival.json`; that file's boot stamp makes the pass
//! run once per boot, and its `done` marker tells the daemon's retire arm
//! whether the pass has finished - the arm holds its vacates and row reaps
//! until it has, so a power loss cannot end a lead before the revival that
//! would have re-seated it.
//!
//! The receipt is not the whole delivery: after the pass, a failed row
//! reaches its spawned_by lead as durable mail (reason, re-spawn line,
//! stale claim), and a revived worker reaches its own row with its open PR
//! and that PR's CI verdict, so nobody wakes idle.

use std::collections::HashSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::claude_roster::ClaudeAgentsSnapshot;
use crate::paths::AgentsHome;
use crate::state::RegistryEntry;

const RECEIPT_FILE: &str = "boot-revival.json";

/// The batch's in-process in-flight guard: a second `start` while the batch
/// thread is alive returns instead of re-planning under it (the listing
/// would still read the pre-respawn world and double-respawn).
static BATCH_RUNNING: AtomicBool = AtomicBool::new(false);

/// The hold's bound in seconds past boot: revival protection is not a veto.
/// A listing that never reads (a broken harness CLI) must not hold the
/// retire arm forever, so 30 minutes past boot the sweeps resume and the
/// failure stays in the events.
const REVIVAL_HOLD_CEILING_SECS: u64 = 30 * 60;

/// Why the retire arm holds its vacates and row reaps: `Some` while this
/// boot's revival pass has not finished and the boot is recent enough for
/// the hold to bind, `None` when the pass is done, or when the host cannot
/// even name its boot (a gate nobody can evaluate is no gate).
pub(crate) fn hold_reason(home: &AgentsHome) -> Option<String> {
    let Some(boot) = boot_time() else {
        return None;
    };
    if boot_age_past_ceiling(boot) {
        return None;
    }
    let receipt = home.root().join(RECEIPT_FILE);
    let parsed = std::fs::read_to_string(&receipt)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    match parsed {
        None => Some("no receipt for this boot yet".to_string()),
        Some(v) => {
            let matches_boot = ran_this_boot(v.get("boot").and_then(Value::as_u64), boot);
            let done = v.get("done").and_then(Value::as_bool).unwrap_or(false);
            match (matches_boot, done) {
                (true, true) => None,
                (true, false) => Some("this boot's revival pass has not finished".to_string()),
                (false, _) => Some("the receipt is from an earlier boot".to_string()),
            }
        }
    }
}

/// Seconds since the boot exceeded the hold ceiling: wall-clock bound.
fn boot_age_past_ceiling(boot_epoch_s: u64) -> bool {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(u64::MAX);
    boot_age_past_ceiling_at(boot_epoch_s, now)
}

/// The pure ceiling read, now injected so the test pins a young and an old
/// boot without depending on the runner's own uptime.
fn boot_age_past_ceiling_at(boot_epoch_s: u64, now_s: u64) -> bool {
    now_s.saturating_sub(boot_epoch_s) > REVIVAL_HOLD_CEILING_SECS
}

/// The sender stamp the pass's mail carries, so a drain attributes the rows
/// to the boot pass the way `lead_mail::SETTLE_SENDER` does for the settle
/// arm.
pub(crate) const SENDER: &str = "fno/boot-revival";

/// macOS moves `kern.boottime` when the wall clock is stepped, so two reads
/// in one boot can differ by a few seconds.
const BOOT_DRIFT_S: u64 = 120;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceiptRow {
    pub(crate) name: String,
    /// "revived", "skipped" or "failed".
    pub(crate) outcome: String,
    pub(crate) reason: String,
}

impl ReceiptRow {
    fn new(name: &str, outcome: &'static str, reason: impl Into<String>) -> Self {
        Self {
            name: name.to_string(),
            outcome: outcome.to_string(),
            reason: reason.into(),
        }
    }
}

/// The worker-revive concurrency bound. The 2026-10-29 boot measured twelve
/// parallel respawns racing one supervisor birth and losing (exit 16); the
/// batch runs this many at a time behind one upfront birth.
pub(crate) const REVIVE_CONCURRENCY: usize = 3;

/// Plan the pass, stamp the receipt, and revive off the caller's thread.
/// Planning reads the registry before the startup sweep rewrites a single
/// status, so a row still reads the way it did when the machine went down.
/// A receipt from this boot that reads unfinished (a daemon that died
/// mid-batch) re-runs the batch: the listing re-reads live truth, so a row
/// the earlier pass already revived lists working and is skipped, making a
/// re-run complete the pass instead of double-respawning it.
pub(crate) fn start(home: &AgentsHome) {
    let Some(boot) = boot_time() else {
        return;
    };
    let receipt = home.root().join(RECEIPT_FILE);
    if receipt_done_for_boot(&receipt, boot) {
        return;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    let listing = crate::claude_roster::read_all_agents_union();
    // An unread listing reads every claude job as unlisted. The claude rows
    // wait as named skip rows; keeper rows need no listing and revive
    // regardless. Stamping done then would skip the claude fleet for good,
    // so the receipt stays unfinished and the retire arm's interval kick
    // retries until a listing reads, bounded by the hold ceiling.
    let listing_readable = listing.is_known() && listing.warning_text().is_empty();
    if !listing_readable {
        eprintln!(
            "boot revival: claude agents --json --all unread ({}); claude rows wait for the retire arm's interval kick",
            listing.warning_text()
        );
    }
    if BATCH_RUNNING.swap(true, Ordering::SeqCst) {
        return;
    }
    let (closed, merged) = closed_and_merged_nodes();
    let (leads, workers, keepers, mut rows) =
        plan(&registry.entries, &listing, &closed, &merged, boot);
    // Stamp before the batch. The in-process guard holds the same-daemon
    // case; a re-run after death is safe because the listing re-reads live
    // truth, and the tap is the fallback for a row the pass never reached.
    write_receipt(&receipt, boot, &rows, false);
    let pass_home = home.clone();
    std::thread::spawn(move || {
        rows.extend(revive_batch(
            &pass_home,
            &leads,
            &workers,
            &keepers,
            |plan| crate::claude_supervisor::guard_birth_for_plan(&plan.env),
            revive_unattended,
            |home, name| {
                crate::reentry::resolve_reentry(
                    &home.registry_json(),
                    name,
                    crate::reentry::ReentryTransition::Resume,
                    None,
                    None,
                )
            },
        ));
        let passed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            // Re-seat before the gate opens: the first post-revival sweep
            // must read the world after the role re-seat, not before it.
            reseat_vacated_roles(&pass_home, &revived_names(&rows));
            write_receipt(&receipt, boot, &rows, listing_readable);
            notify(&pass_home, &rows);
        }));
        // The guard releases on every path: a panicked batch leaves the
        // receipt unfinished on purpose, so a later kick re-runs it.
        BATCH_RUNNING.store(false, Ordering::SeqCst);
        if passed.is_err() {
            eprintln!("boot revival: the batch panicked; the receipt stays unfinished");
        }
    });
}

/// The revived rows' names, the re-seat's join-key input.
fn revived_names(rows: &[ReceiptRow]) -> Vec<String> {
    rows.iter()
        .filter(|r| r.outcome == "revived")
        .map(|r| r.name.clone())
        .collect()
}

/// Whether the receipt names this boot AND reads finished: the one shape
/// that lets `start` return without work and the retire arm run its sweeps.
/// An older receipt predates the `done` field and reads unfinished.
fn receipt_done_for_boot(receipt: &Path, boot: u64) -> bool {
    std::fs::read_to_string(receipt)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .map(|v| {
            ran_this_boot(v.get("boot").and_then(Value::as_u64), boot)
                && v.get("done").and_then(Value::as_bool).unwrap_or(false)
        })
        .unwrap_or(false)
}

/// Re-seat the roles of revived holders: for each revived row whose session
/// a `.md.vacated` lead manifest names, restore the manifest and re-stamp
/// the row, refusing over a live rival or a manifest a successor armed
/// meanwhile. A reboot vacate keeps the manifest aside precisely so this can
/// run; without it a revived lead would come back roleless. Best effort: a
/// refusal prints and the revive outcome stands.
fn reseat_vacated_roles(home: &AgentsHome, revived: &[String]) {
    if revived.is_empty() {
        return;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    for name in revived {
        if let Err(reason) = reseat_one_name(home, &registry.entries, name, "daemon") {
            eprintln!("boot revival: {reason}");
        }
    }
}

/// The manual resume's share of the re-seat: a revival by session id must
/// come back holding the seat its vacate set aside, not wait for the next
/// boot or a fresh grant. Best effort like the boot pass: a refusal prints
/// on the resume's stderr and the resume stands.
pub(crate) fn reseat_resumed_row(home: &AgentsHome, row_name: &str) {
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return;
    };
    if let Err(reason) = reseat_one_name(home, &registry.entries, row_name, "agents") {
        eprintln!("fno agents resume: {reason}");
    }
}

/// One row's re-seat scan: every `.md.vacated` lead manifest in the row's
/// space whose `harness_session_id` names the row's session gets restored
/// onto the row, with one `agent_team_reseated` event per restore stamped
/// `source` (the boot pass and the resume door stay distinguishable in the
/// journal). A row with no session, no leads dir, or no matching manifest
/// answers `Ok` - the common no-op for a worker resume.
fn reseat_one_name(
    home: &AgentsHome,
    entries: &[RegistryEntry],
    name: &str,
    source: &str,
) -> Result<(), String> {
    let Some(row) = entries.iter().find(|e| e.name == name) else {
        return Ok(());
    };
    let Some(sid) = row.harness_session_id.as_deref().filter(|s| !s.is_empty()) else {
        return Ok(());
    };
    let leads = crate::paths::space_dir(std::path::Path::new(&row.cwd)).join("leads");
    let Ok(read) = std::fs::read_dir(&leads) else {
        return Ok(());
    };
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), source);
    let mut first: Option<String> = None;
    let mut vacated: Vec<std::path::PathBuf> = read.flatten().map(|e| e.path()).collect();
    vacated.sort();
    for path in vacated {
        let is_vacated = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".md.vacated"));
        if !is_vacated {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if crate::claude_adopt::manifest_field(&content, "harness_session_id").as_deref()
            != Some(sid)
        {
            continue;
        }
        let Some(scope) = crate::claude_adopt::manifest_field(&content, "role_scope")
            .filter(|s| !s.trim().is_empty())
        else {
            continue;
        };
        let Some(level) = crate::claude_adopt::manifest_field(&content, "role_level")
            .and_then(|l| l.parse::<u32>().ok())
        else {
            continue;
        };
        let grantor = crate::claude_adopt::manifest_field(&content, "role_grantor")
            .filter(|g| !g.trim().is_empty())
            .unwrap_or_else(|| "spawn".to_string());
        match reseat_one(
            &home.registry_json(),
            &row.name,
            &scope,
            level,
            &grantor,
            &path,
        ) {
            Ok(()) => {
                let _ = emitter.emit(
                    "agent_team_reseated",
                    &serde_json::json!({
                        "scope": scope, "level": level, "holder_session": sid,
                        "grantor": grantor, "row": row.name,
                    }),
                );
            }
            Err(reason) => {
                // One refused manifest never stops the scan: the boot loop
                // printed and kept going, so the first refusal is held and
                // the remaining manifests still get their pass.
                if first.is_none() {
                    first = Some(format!(
                        "re-seat for {} over {scope} refused ({reason})",
                        row.name
                    ));
                }
            }
        }
    }
    match first {
        Some(reason) => Err(reason),
        None => Ok(()),
    }
}

/// One re-seat: re-stamp the row under the live-rival check (registry
/// first, the vacate's own order), then restore the manifest under the
/// destination's flock. A destination that already exists refuses - a
/// successor armed while this holder was down keeps the seat - and the
/// vacated file stays standing for a later pass.
fn reseat_one(
    registry_path: &Path,
    row_name: &str,
    scope: &str,
    level: u32,
    grantor: &str,
    vacated_path: &Path,
) -> Result<(), String> {
    let key = crate::lead_state::territory_key(scope);
    let refused = crate::state::update_registry(registry_path, |doc| {
        let rival = doc.entries.iter().any(|r| {
            r.name != row_name
                && !crate::lead_state::is_terminal(r)
                && r.role_scope
                    .as_deref()
                    .is_some_and(|s| crate::lead_state::territory_key(s) == key)
        });
        if rival {
            return None;
        }
        let row = doc.entries.iter_mut().find(|r| r.name == row_name)?;
        row.role_scope = Some(scope.to_string());
        row.role_level = Some(level);
        row.role_grantor = Some(grantor.to_string());
        Some(())
    })
    .map_err(|e| format!("registry write refused: {e}"))?;
    if refused.is_none() {
        return Err(format!(
            "a live rival holds {scope:?}; the territory stays theirs"
        ));
    }
    let Some(file_name) = vacated_path.file_name().and_then(|n| n.to_str()) else {
        return Err(format!(
            "cannot restore {}: no file name",
            vacated_path.display()
        ));
    };
    let live_path = vacated_path.with_file_name(file_name.trim_end_matches(".vacated"));
    let lock_path = live_path.with_extension("md.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {parent:?}: {e}"))?;
    }
    let lock = crate::chokepoint::take(&crate::chokepoint::TEAM_REAP, &lock_path)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        if live_path.exists() {
            return Err(format!(
                "{} exists; a successor armed while this holder was down",
                live_path.display()
            ));
        }
        std::fs::rename(vacated_path, &live_path)
            .map_err(|e| format!("cannot restore {}: {e}", live_path.display()))
    })();
    drop(lock);
    result
}

/// The config-dir overlay a plan's guard addresses, the dedupe key for the
/// upfront birth: two plans in one config dir share one supervisor.
fn plan_env_key(plan: &crate::reentry::ReentryPlan) -> String {
    plan.env
        .get("CLAUDE_CONFIG_DIR")
        .cloned()
        .unwrap_or_default()
}

/// The pass body: one supervisor birth per distinct plan env before any
/// respawn, leads sequentially, then the workers through bounded chunks,
/// then one sequential retry over every failed row. `resolve_fn`, `guard_fn`
/// and `revive_fn` are injected so tests observe the ordering without
/// spawning claude. Keeper rows chunk through the same bound without the
/// claude resolve: that resolve exists to feed the supervisor birth guard,
/// and a keeper launch takes no claude birth.
fn revive_batch<F, G, R>(
    home: &AgentsHome,
    leads: &[String],
    workers: &[String],
    keepers: &[String],
    guard_fn: G,
    revive_fn: F,
    resolve_fn: R,
) -> Vec<ReceiptRow>
where
    G: Fn(&crate::reentry::ReentryPlan),
    F: Fn(&AgentsHome, &str) -> ReceiptRow + Sync,
    R: Fn(&AgentsHome, &str) -> Result<crate::reentry::ReentryPlan, String>,
{
    let mut rows: Vec<ReceiptRow> = Vec::new();
    // Resolve each worker once for the upfront birth: the guard is
    // idempotent, so one call per distinct config dir turns twelve racing
    // births into one. The batch threads re-resolve inside revive(); the
    // plans here feed the guard only.
    let (planned, resolve_failed) = resolve_via(home, workers, &resolve_fn);
    let mut seen = HashSet::new();
    for (_, plan) in &planned {
        if seen.insert(plan_env_key(plan)) {
            guard_fn(plan);
        }
    }
    for name in leads {
        rows.push(revive_fn(home, name));
    }
    // Chunks iterate the RESOLVED workers only: a worker whose resolution
    // failed already owns its failed row from resolve_failed, and a second
    // attempt through the batch would record the name twice and retry it
    // twice.
    for chunk in planned.chunks(REVIVE_CONCURRENCY) {
        let names: Vec<String> = chunk.iter().map(|(name, _)| name.clone()).collect();
        revive_chunk(home, &names, &revive_fn, &mut rows);
    }
    // Keeper rows skip the resolve phase entirely, for the reason in the
    // doc comment above.
    for chunk in keepers.chunks(REVIVE_CONCURRENCY) {
        revive_chunk(home, chunk, &revive_fn, &mut rows);
    }
    rows.extend(resolve_failed);
    let failed: Vec<String> = rows
        .iter()
        .filter(|r| r.outcome == "failed")
        .map(|r| r.name.clone())
        .collect();
    for name in &failed {
        let retried = revive_fn(home, name);
        if let Some(slot) = rows.iter_mut().find(|r| &r.name == name) {
            *slot = retried;
        }
    }
    rows
}

/// One bounded chunk of revivals on scoped threads. A panicked thread's row
/// still lands, so the receipt keeps one row per name and the retry loop
/// sees it.
fn revive_chunk<F>(home: &AgentsHome, names: &[String], revive_fn: &F, rows: &mut Vec<ReceiptRow>)
where
    F: Fn(&AgentsHome, &str) -> ReceiptRow + Sync,
{
    std::thread::scope(|s| {
        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let home = home.clone();
                let value = name.clone();
                let handle = s.spawn(move || revive_fn(&home, &value));
                (name.clone(), handle)
            })
            .collect();
        for (name, handle) in handles {
            match handle.join() {
                Ok(row) => rows.push(row),
                // A panicked thread's row still lands, so the receipt
                // keeps one row per worker and the retry loop sees it.
                Err(_) => rows.push(ReceiptRow::new(&name, "failed", "revive thread panicked")),
            }
        }
    });
}

/// resolve_targets over an injected resolver, the tests' seam for plans.
fn resolve_via<R>(
    home: &AgentsHome,
    names: &[String],
    resolve_fn: &R,
) -> (Vec<(String, crate::reentry::ReentryPlan)>, Vec<ReceiptRow>)
where
    R: Fn(&AgentsHome, &str) -> Result<crate::reentry::ReentryPlan, String>,
{
    let mut planned = Vec::new();
    let mut failed = Vec::new();
    for name in names {
        match resolve_fn(home, name) {
            Ok(plan) => planned.push((name.clone(), plan)),
            Err(reason) => failed.push(ReceiptRow::new(name, "failed", reason)),
        }
    }
    (planned, failed)
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

/// The registry row the worker's current owner session names (the birth
/// edge where no succession reown narrows it). The full session id is the
/// join key (the registry's keying law); a finished row loses to a live
/// one, and the first row wins among equals.
fn spawned_lead_name(entries: &[RegistryEntry], worker: &RegistryEntry) -> Option<String> {
    let sid = worker.current_owner_session()?;
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

/// Split the registry into leads, claude workers, keeper-lane rows, and the
/// rows skipped with their reason. A row counts only when it was live at the
/// boot: a live-ish status, or an exit stamped after the boot began. Claude
/// rows answer the `claude agents` listing; keeper rows skip it (it never
/// lists their jobs) and ride their own list, because the batch resolves
/// claude workers only to feed the supervisor birth guard a keeper launch
/// does not take.
pub(crate) fn plan(
    entries: &[RegistryEntry],
    listing: &ClaudeAgentsSnapshot,
    closed_nodes: &HashSet<String>,
    merged_nodes: &HashSet<String>,
    boot: u64,
) -> (Vec<String>, Vec<String>, Vec<String>, Vec<ReceiptRow>) {
    let mut leads = Vec::new();
    let mut workers = Vec::new();
    let mut keepers = Vec::new();
    let mut skipped = Vec::new();
    let listing_readable = listing.is_known() && listing.warning_text().is_empty();
    let contract = crate::harness_capabilities::HarnessContract::packaged();
    for e in entries {
        if e.mux.is_some() {
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
        if e.harness_name() == "claude" {
            // An unread listing reads every claude job as unlisted, so the
            // claude half waits as a named skip row and the retire arm's
            // interval kick retries it; the receipt stamps done only when
            // the listing read, which is what keeps that kick coming.
            if !listing_readable {
                skipped.push(ReceiptRow::new(
                    &e.name,
                    "skipped",
                    format!(
                        "claude agents --json --all unread ({}); the retire arm's interval kick retries",
                        listing.warning_text()
                    ),
                ));
                continue;
            }
        } else {
            // A keeper-lane row: the contract decides. A harness whose
            // keeper carries no same-id revival skips with that named,
            // never silence, and the listing match below is claude's
            // answer, never the keeper row's.
            let carries = contract
                .as_ref()
                .ok()
                .map(|c| crate::resume_route::keeper_carries_revival(c, e.harness_name()))
                .unwrap_or(false);
            if !carries {
                skipped.push(ReceiptRow::new(
                    &e.name,
                    "skipped",
                    format!(
                        "[harness.{}.keeper] does not carry resume_session_id",
                        e.harness_name()
                    ),
                ));
                continue;
            }
            if e.harness_session_id
                .as_deref()
                .filter(|s| s.len() >= 8)
                .is_none()
            {
                skipped.push(ReceiptRow::new(
                    &e.name,
                    "skipped",
                    "no session id recorded",
                ));
                continue;
            }
            if let Some(node) = e.node.as_deref().filter(|n| closed_nodes.contains(*n)) {
                skipped.push(ReceiptRow::new(
                    &e.name,
                    "skipped",
                    format!("node {node} is done or superseded"),
                ));
                continue;
            }
            if e.role_level.is_some() {
                leads.push(e.name.clone());
            } else if let Some(node) = e
                .node
                .as_deref()
                .filter(|n| merged_nodes.contains(*n))
                .filter(|_| deliberate_stop(e))
            {
                skipped.push(ReceiptRow::new(
                    &e.name,
                    "skipped",
                    format!("node {node}'s PR merged; worker stopped on purpose; never revived"),
                ));
            } else {
                keepers.push(e.name.clone());
            }
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
                    .filter(|_| deliberate_stop(e))
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
    (leads, workers, keepers, skipped)
}

/// One revival through the dispatcher: a claude row takes the canonical
/// re-entry plan (`claude respawn <job id>`), a keeper-lane row takes its
/// own keeper revival on the contract's resume form. A revive no user asked
/// for: `autonomy.mode` may hold either. The row's own cwd picks the project
/// config. A hold reads `failed`, so the lead hears of it and the explicit
/// rerun can reach it.
pub(crate) fn revive_unattended(home: &AgentsHome, name: &str) -> ReceiptRow {
    let entry = crate::state::load_registry(&home.registry_json())
        .ok()
        .and_then(|r| r.entries.into_iter().find(|e| e.name == name));
    let Some(entry) = entry else {
        return ReceiptRow::new(name, "failed", "no registry row left to revive");
    };
    if !crate::autonomy_policy::restart_allowed(std::path::Path::new(&entry.cwd)) {
        return ReceiptRow::new(name, "failed", "autonomy.mode holds unattended restart");
    }
    if entry.harness_name() == "claude" {
        return revive(home, name);
    }
    revive_keeper(home, &entry)
}

/// The keeper half of the dispatcher: the same gate, reservation and launch
/// the manual `fno agents session resume` door runs for a KeeperRevive row,
/// sized to a receipt row instead of an exit code. The contract decides: a
/// harness whose keeper lane carries no same-id revival is a failed row
/// naming the missing word, never the claude resolver's refusal.
fn revive_keeper(home: &AgentsHome, entry: &RegistryEntry) -> ReceiptRow {
    let name = &entry.name;
    let harness = entry.harness_name();
    let contract = match crate::harness_capabilities::HarnessContract::packaged() {
        Ok(contract) => contract,
        Err(_) => {
            return ReceiptRow::new(name, "failed", "harness contract unread");
        }
    };
    if !crate::resume_route::keeper_carries_revival(&contract, harness) {
        return ReceiptRow::new(
            name,
            "failed",
            format!("[harness.{harness}.keeper] does not carry resume_session_id"),
        );
    }
    let Some(sid) = entry.harness_session_id.as_deref().filter(|s| s.len() >= 8) else {
        return ReceiptRow::new(name, "failed", "no session id recorded");
    };
    let argv = match contract.render_session_argv_raw(harness, "interactive_resume", Some(sid)) {
        Ok(argv) => argv,
        Err(_) => {
            return ReceiptRow::new(name, "failed", "the resume form did not render");
        }
    };
    // A keeper already answering this session is the session itself: skip
    // with that named, the same short-circuit revive_with takes, so a
    // reboot survivor never reads revived and a kick re-run never re-mails.
    let sock = crate::convert::keeper_rebind::thread_socket_path(
        home.root().parent().unwrap_or(home.root()),
        name,
    );
    if let crate::daemon::KeeperProbe::Answered(reply) =
        crate::daemon::probe_keeper_socket(&sock, std::time::Duration::from_secs(2))
    {
        if reply.get("session_id").and_then(Value::as_str) == Some(sid) {
            return ReceiptRow::new(
                name,
                "skipped",
                "already live on its keeper; nothing launched",
            );
        }
    }
    // The manual door reserves before anything launches; the reservation is
    // what revive_with's failure releases read against.
    if let Some(code) = crate::resume_gate::gate_and_reserve(home, name, sid) {
        return ReceiptRow::new(name, "failed", format!("resume gate refused (exit {code})"));
    }
    let code = crate::keeper_revival::revive(home, name, harness, sid, &entry.cwd, &argv, None);
    match code {
        0 => ReceiptRow::new(name, "revived", argv.join(" ")),
        code => ReceiptRow::new(
            name,
            "failed",
            format!("{harness} keeper revival exited {code}"),
        ),
    }
}

/// The rerun's dispatcher: an explicit operator command, so the autonomy
/// hold does not bind (the same bypass the claude rerun always had), and a
/// keeper row still takes its own launch.
fn rerun_revive(home: &AgentsHome, name: &str) -> ReceiptRow {
    let entry = crate::state::load_registry(&home.registry_json())
        .ok()
        .and_then(|r| r.entries.into_iter().find(|e| e.name == name));
    match entry {
        Some(e) if e.harness_name() != "claude" => revive_keeper(home, &e),
        _ => revive(home, name),
    }
}

pub(crate) fn revive(home: &AgentsHome, name: &str) -> ReceiptRow {
    match crate::reentry::resolve_reentry(
        &home.registry_json(),
        name,
        crate::reentry::ReentryTransition::Resume,
        None,
        None,
    ) {
        Ok(plan) => match crate::resume_wake::run_and_confirm_respawn(
            &plan,
            name,
            "revive",
            "agent_revived",
            home,
        ) {
            0 => ReceiptRow::new(name, "revived", plan.argv.join(" ")),
            code => ReceiptRow::new(
                name,
                "failed",
                format!("{} exited {code}", plan.argv.join(" ")),
            ),
        },
        Err(reason) => ReceiptRow::new(name, "failed", reason),
    }
}

/// The nodes a revival must leave alone: done or superseded. A merged PR
/// under a live node is a multi-ship node mid-flight; its worker comes back
/// UNLESS a stop someone ran through fno still stands on the row (an Exited
/// row carrying its `StopRecord`): a worker whose work delivered and whose
/// stop was never resumed past is never revived. The node's own status
/// stays the authority for closure - the primary PR's merge state once read
/// a multi-ship node closed and left its worker down during the 2026-10-10
/// reboot.
fn closed_row(row: &Value) -> bool {
    crate::graph_store::is_terminal_entry(row)
}

/// True when a stop someone ran through fno STILL STANDS on this row: the
/// row reads Exited and carries the `StopRecord` that stop stamped. A row
/// that reads live resumed after any older stop, so its record is stale and
/// says nothing about the death this pass is deciding on - which is what
/// keeps a rebooted multi-ship worker coming back.
fn deliberate_stop(e: &RegistryEntry) -> bool {
    matches!(e.status, crate::AgentStatus::Exited) && e.stop.is_some()
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

pub(crate) fn write_receipt(receipt: &Path, boot: u64, rows: &[ReceiptRow], done: bool) {
    let body = serde_json::json!({ "boot": boot, "rows": rows, "done": done });
    let tmp = receipt.with_extension("json.tmp");
    if std::fs::write(&tmp, body.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, receipt);
    }
}

struct ReceiptBody {
    boot: u64,
    rows: Vec<ReceiptRow>,
}

fn read_receipt(receipt: &Path) -> Result<ReceiptBody, String> {
    let raw = std::fs::read_to_string(receipt)
        .map_err(|e| format!("receipt {} unreadable ({e})", receipt.display()))?;
    let body: Value = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "receipt {} does not parse ({e}); a corrupted receipt is a failed reading, never a silent zero",
            receipt.display()
        )
    })?;
    let boot = body
        .get("boot")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("receipt {} carries no boot stamp", receipt.display()))?;
    let rows: Vec<ReceiptRow> =
        serde_json::from_value(body.get("rows").cloned().unwrap_or(json!([])))
            .map_err(|e| format!("receipt {} rows unreadable ({e})", receipt.display()))?;
    Ok(ReceiptBody { boot, rows })
}

/// Merge a rerun into the receipt rows: every rerun-set row takes its fresh
/// reading (a revive outcome, else the re-judged skip); rows outside the set
/// keep the old line. Returns the merged rows and whether any rerun row
/// still reads failed.
fn merge_rerun_rows(
    old: Vec<ReceiptRow>,
    rerun: &HashSet<String>,
    revived: Vec<ReceiptRow>,
    fresh_skips: Vec<ReceiptRow>,
) -> (Vec<ReceiptRow>, bool) {
    let fresh: std::collections::HashMap<String, ReceiptRow> = fresh_skips
        .into_iter()
        .map(|r| (r.name.clone(), r))
        .collect();
    let revived: std::collections::HashMap<String, ReceiptRow> =
        revived.into_iter().map(|r| (r.name.clone(), r)).collect();
    let mut any_failed = false;
    let mut rows = old;
    for row in rows.iter_mut() {
        if !rerun.contains(&row.name) {
            continue;
        }
        if let Some(new) = revived.get(&row.name) {
            *row = new.clone();
        } else if let Some(skip) = fresh.get(&row.name) {
            *row = skip.clone();
        }
        if row.outcome == "failed" {
            any_failed = true;
        }
    }
    (rows, any_failed)
}

/// `fno agents session recover --boot-failed`: re-judge this boot's failed
/// and skipped rows through the same pass rules, revive the survivors
/// through the same bounded batch, and rewrite the receipt in place. Exit 0
/// when no rerun row reads failed at the end, 1 otherwise.
pub(crate) fn rerun_failed(home: &AgentsHome) -> i32 {
    let receipt = home.root().join(RECEIPT_FILE);
    let body = match read_receipt(&receipt) {
        Ok(body) => body,
        Err(reason) => {
            eprintln!("fno agents session recover: {reason}");
            return 1;
        }
    };
    let boot = body.boot;
    // A receipt from an earlier boot selects rows against that boot's world:
    // a worker stopped during the current boot would read eligible against
    // the old stamp. Refuse rather than rerun the wrong boot.
    match boot_time() {
        Some(now) if ran_this_boot(Some(boot), now) => {}
        Some(now) => {
            eprintln!(
                "fno agents session recover: receipt {} is from an earlier boot (stamp {boot}, now {now}); this boot has not produced its own receipt yet",
                receipt.display()
            );
            return 1;
        }
        None => {
            eprintln!("fno agents session recover: boot time unreadable; refusing to rerun against an unverifiable receipt");
            return 1;
        }
    }
    let old_rows = body.rows;
    let rerun: HashSet<String> = old_rows
        .iter()
        .filter(|r| r.outcome != "revived")
        .map(|r| r.name.clone())
        .collect();
    if rerun.is_empty() {
        println!("boot revival rerun: no failed or skipped rows this boot");
        return 0;
    }
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        eprintln!(
            "fno agents session recover: registry {} unreadable",
            home.registry_json().display()
        );
        return 1;
    };
    let listing = crate::claude_roster::read_all_agents_union();
    // The unread-listing refusal scoped to the claude half: claude rows
    // re-judge to named skip rows while keeper rows still re-judge and
    // revive.
    let (closed, merged) = closed_and_merged_nodes();
    let (mut leads, mut workers, mut keepers, fresh_skips) =
        plan(&registry.entries, &listing, &closed, &merged, boot);
    leads.retain(|n| rerun.contains(n));
    workers.retain(|n| rerun.contains(n));
    keepers.retain(|n| rerun.contains(n));
    let fresh_skips: Vec<ReceiptRow> = fresh_skips
        .into_iter()
        .filter(|r| rerun.contains(&r.name))
        .collect();
    let run_rows = revive_batch(
        home,
        &leads,
        &workers,
        &keepers,
        |plan| crate::claude_supervisor::guard_birth_for_plan(&plan.env),
        rerun_revive,
        |home, name| {
            crate::reentry::resolve_reentry(
                &home.registry_json(),
                name,
                crate::reentry::ReentryTransition::Resume,
                None,
                None,
            )
        },
    );
    let (merged_rows, any_failed) =
        merge_rerun_rows(old_rows, &rerun, run_rows.clone(), fresh_skips);
    write_receipt(&receipt, boot, &merged_rows, true);
    for row in merged_rows.iter().filter(|r| rerun.contains(&r.name)) {
        println!("{}: {} ({})", row.name, row.outcome, row.reason);
    }
    notify(home, &run_rows);
    if any_failed {
        1
    } else {
        0
    }
}

/// This boot's start, in epoch seconds.
pub(crate) fn boot_time() -> Option<u64> {
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
        let (leads, workers, _keepers, skipped) = plan(
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
        let mut standing_stop = row("kestrel", "11112222-bbbb", AgentStatus::Exited);
        standing_stop.node = Some("x-merged".into());
        crate::state::record_stop(&mut standing_stop, "lead", None);
        // Exited inside this boot's window, so the candidate filter lets the
        // row through and the stop is this boot's news.
        standing_stop.exited_at = Some("2026-09-26T01:01:00Z".into());
        let mut stale_stop = row("quill", "9999aaaa-aaaa", AgentStatus::Live);
        stale_stop.node = Some("x-merged".into());
        crate::state::record_stop(&mut stale_stop, "lead", None);
        let mut reboot_death = row("folio", "33334444-cccc", AgentStatus::Live);
        reboot_death.node = Some("x-merged".into());
        let listing = ClaudeAgentsSnapshot::known(vec![
            ClaudeAgentRow::new("11112222", Some("stopped")),
            ClaudeAgentRow::new("9999aaaa", Some("stopped")),
            ClaudeAgentRow::new("33334444", Some("stopped")),
        ]);
        let merged = HashSet::from(["x-merged".to_string()]);
        let (leads, workers, _keepers, skipped) = plan(
            &[standing_stop, stale_stop, reboot_death],
            &listing,
            &HashSet::new(),
            &merged,
            BOOT,
        );
        assert!(
            leads.is_empty(),
            "a standing-stop worker is not a lead; nothing revives here"
        );
        assert_eq!(
            workers,
            vec!["quill", "folio"],
            "a resumed-after stop and a bare reboot death both come back"
        );
        let names: Vec<_> = skipped.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["kestrel"], "{skipped:?}");
        assert!(
            skipped[0].reason.contains("merged") && skipped[0].reason.contains("never revived"),
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
    fn the_receipts_done_marker_decides_the_pass_and_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join(RECEIPT_FILE);
        assert!(!receipt_done_for_boot(&receipt, BOOT));
        write_receipt(&receipt, BOOT, &[], false);
        assert!(
            !receipt_done_for_boot(&receipt, BOOT),
            "stamped but unfinished holds"
        );
        write_receipt(&receipt, BOOT, &[], true);
        assert!(receipt_done_for_boot(&receipt, BOOT));
        // A clock step nudges kern.boottime; it is still the same boot.
        write_receipt(&receipt, BOOT + 3, &[], true);
        assert!(receipt_done_for_boot(&receipt, BOOT + 3));
        assert!(!receipt_done_for_boot(&receipt, BOOT + 86_400));
    }
    /// The retire arm's hold table: no receipt holds, an earlier boot's
    /// receipt holds, this boot unfinished holds, this boot finished runs,
    /// and a receipt beyond the hold ceiling does not bind.
    #[test]
    fn the_retire_arm_holds_until_this_boots_pass_finishes_then_runs() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(home.root()).unwrap();
        let receipt = dir.path().join(RECEIPT_FILE);
        assert!(hold_reason(&home).is_some(), "no receipt must hold");
        let boot = boot_time().unwrap();
        write_receipt(&receipt, boot - 86_400, &[], true);
        assert!(
            hold_reason(&home).is_some(),
            "an earlier boot's receipt must hold"
        );
        write_receipt(&receipt, boot, &[], false);
        assert!(hold_reason(&home).is_some(), "unfinished must hold");
        write_receipt(&receipt, boot, &[], true);
        assert!(hold_reason(&home).is_none(), "finished must run");
        // The ceiling is a pure bound on the boot's age, tested with now
        // injected: a receipt older than the window reads as an earlier
        // boot, so the wall-clock ceiling is what ends the hold.
        let young = boot_age_past_ceiling_at(boot, boot + 60);
        assert!(!young, "a fresh boot is inside the ceiling");
        let old = boot_age_past_ceiling_at(boot - 1, boot + REVIVAL_HOLD_CEILING_SECS);
        assert!(old, "past the ceiling the hold does not bind");
    }
    /// The re-seat: a revived holder gets its vacated manifest restored and
    /// its row re-stamped; a live rival on the territory refuses the stamp
    /// and the manifest stays set aside.
    #[test]
    fn a_reseat_restores_the_vacated_manifest_and_restamps_the_row() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_SPACES_DIR");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::env::set_var("FNO_SPACES_DIR", dir.path().join("spaces"));
        let leads = crate::paths::space_dir(&cwd).join("leads");
        std::fs::create_dir_all(&leads).unwrap();
        let sid = "11111111-2222-4333-8444-555555555555";
        let manifest = "---\nscope: zed\nharness: claude\n".to_string()
            + "harness_session_id: "
            + sid
            + "\ncreated_at: 2026-11-05T00:00:00Z\nrole_scope: zed\nrole_level: 2\n"
            + "role_grantor: operator\n---\n";
        std::fs::write(leads.join("zed.md.vacated"), manifest.clone()).unwrap();
        let agents = dir.path().join("agents");
        let home = AgentsHome::at(&agents);
        std::fs::create_dir_all(home.root()).unwrap();
        let registry = home.registry_json();
        crate::registry_store::seed_raw(
            &registry,
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [{
                    "name": "l2",
                    "cwd": cwd,
                    "status": "exited",
                    "harness": "claude",
                    "harness_session_id": sid,
                }],
            })
            .to_string(),
        );
        reseat_vacated_roles(&home, &["l2".to_string()]);
        let live = leads.join("zed.md");
        assert!(live.exists(), "the manifest is restored");
        assert!(!leads.join("zed.md.vacated").exists());
        let row = crate::state::load_registry(&registry).unwrap().entries[0].clone();
        assert_eq!(row.role_scope.as_deref(), Some("zed"));
        assert_eq!(row.role_level, Some(2));
        assert_eq!(row.role_grantor.as_deref(), Some("operator"));
        // A live rival on the territory refuses the stamp: the manifest
        // stays set aside and the row keeps no role.
        // The re-vacate renames the restored manifest aside; the fixture
        // replays that before the rival leg.
        std::fs::remove_file(&live).unwrap();
        std::fs::write(leads.join("zed.md.vacated"), manifest).unwrap();
        crate::registry_store::seed_raw(
            &registry,
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [
                    {"name": "l2", "cwd": cwd, "status": "exited",
                     "harness": "claude", "harness_session_id": sid},
                    {"name": "rival", "cwd": cwd, "status": "live",
                     "harness": "claude",
                     "harness_session_id": "22222222-3333-4444-8555-666666666666",
                     "role_scope": "zed", "role_level": 2},
                ],
            })
            .to_string(),
        );
        reseat_vacated_roles(&home, &["l2".to_string()]);
        assert!(!live.exists(), "a rival keeps the manifest set aside");
        let rows = crate::state::load_registry(&registry).unwrap().entries;
        let l2 = rows.iter().find(|r| r.name == "l2").unwrap();
        assert!(l2.role_scope.is_none(), "{:?}", l2.role_scope);
        std::fs::remove_dir_all(dir.path()).ok();
        match saved {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
    }

    /// A revival by session id re-seats the role its vacate set aside, the
    /// same treatment the boot pass gives a revived holder: the resumed
    /// lead's own check-in must find its team live, not wait for a reboot
    /// or a fresh grant.
    #[test]
    fn a_resume_revived_lead_reseats_its_role_and_the_checkin_passes() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let saved = std::env::var_os("FNO_SPACES_DIR");
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        std::env::set_var("FNO_SPACES_DIR", dir.path().join("spaces"));
        let space = crate::paths::space_dir(&cwd);
        let leads = space.join("leads");
        std::fs::create_dir_all(&leads).unwrap();
        let sid = "33333333-4444-4555-8666-777777777777";
        let manifest = "---\nscope: fno-web\nharness: claude\n".to_string()
            + "harness_session_id: "
            + sid
            + "\ncreated_at: 2026-11-06T00:00:00Z\nrole_scope: fno-web\nrole_level: 1\n"
            + "role_grantor: operator\n---\n";
        // The vacate's own product: the manifest set aside, the row's team
        // fields cleared. The resume leaves the row liveish but roleless.
        std::fs::write(leads.join("rosa.md.vacated"), manifest).unwrap();
        let agents = dir.path().join("agents");
        let home = AgentsHome::at(&agents);
        std::fs::create_dir_all(home.root()).unwrap();
        let registry = home.registry_json();
        crate::registry_store::seed_raw(
            &registry,
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [{
                    "name": "rosa",
                    "cwd": cwd,
                    "status": "busy",
                    "harness": "claude",
                    "harness_session_id": sid,
                }],
            })
            .to_string(),
        );
        let state = || {
            crate::lead_state::lead_state(space.as_path(), Some("fno-web"), None, None, &registry)
        };
        assert_eq!(
            state().teamed,
            Some(false),
            "the vacated lead reads unteamed before the re-seat"
        );

        crate::boot_revival::reseat_resumed_row(&home, "rosa");

        let live = leads.join("rosa.md");
        assert!(live.exists(), "the manifest is restored");
        assert!(!leads.join("rosa.md.vacated").exists());
        let row = crate::state::load_registry(&registry).unwrap().entries[0].clone();
        assert_eq!(row.role_scope.as_deref(), Some("fno-web"));
        assert_eq!(row.role_level, Some(1));
        assert_eq!(row.role_grantor.as_deref(), Some("operator"));
        // The check-in passes: the team reads live, held by this session.
        let state = state();
        assert_eq!(state.teamed, Some(true), "{:?}", state.unknown_reason);
        assert_eq!(state.live, Some(true));
        let team = crate::team_names::live_index(&registry)
            .unwrap()
            .remove("fno-web");
        assert_eq!(team.map(|t| t.holder_session), Some(Some(sid.to_string())));
        std::fs::remove_dir_all(dir.path()).ok();
        match saved {
            Some(v) => std::env::set_var("FNO_SPACES_DIR", v),
            None => std::env::remove_var("FNO_SPACES_DIR"),
        }
    }

    #[test]
    fn boot_failed_refuses_a_name() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        let code = crate::client_commands::run_recover(
            &["--boot-failed".to_string(), "somename".to_string()],
            &home,
        );
        assert_eq!(code, 2);
    }

    #[test]
    fn rerun_refuses_a_receipt_from_an_earlier_boot() {
        let dir = tempfile::tempdir().unwrap();
        let home = AgentsHome::at(dir.path());
        std::fs::create_dir_all(home.root()).unwrap();
        let receipt = dir.path().join(RECEIPT_FILE);
        let old_boot = boot_time().unwrap() - 86_400;
        write_receipt(
            &receipt,
            old_boot,
            &[ReceiptRow::new(
                "folio",
                "failed",
                "claude respawn 33334444 exited 16",
            )],
            true,
        );
        let code = rerun_failed(&home);
        assert_eq!(code, 1, "a previous boot's receipt is not this boot's news");
    }

    fn keeper_row(name: &str, sid: &str, harness: &str, status: AgentStatus) -> RegistryEntry {
        RegistryEntry {
            name: name.into(),
            harness: Some(harness.into()),
            harness_session_id: Some(sid.into()),
            status,
            ..Default::default()
        }
    }

    /// A carrying harness's live-at-boot rows land in the keeper list (a
    /// post-boot exit stamp, the sweep's write, included) and a lead rides
    /// the leads list; a pre-boot exit stays out; the claude plan is
    /// unchanged beside them.
    #[test]
    fn plan_sends_live_keeper_rows_of_a_carrying_harness_to_the_revive_list() {
        let claude_worker = row("kestrel", "11112222-bbbb", AgentStatus::Orphaned);
        let mut agy_post_boot = keeper_row("agy-a", "agy11111-session", "agy", AgentStatus::Exited);
        agy_post_boot.exited_at = Some("2026-09-26T01:01:00Z".into());
        let cursor = keeper_row(
            "cursor-a",
            "cur11111-session",
            "cursor-agent",
            AgentStatus::Live,
        );
        let mut agy_lead = keeper_row("agy-lead", "agy44444-session", "agy", AgentStatus::Live);
        agy_lead.role_level = Some(1);
        let mut agy_pre_boot =
            keeper_row("agy-old", "agy22222-session", "agy", AgentStatus::Exited);
        agy_pre_boot.exited_at = Some("2026-01-01T00:00:00Z".into());
        let listing =
            ClaudeAgentsSnapshot::known(vec![ClaudeAgentRow::new("11112222", Some("failed"))]);
        let (leads, workers, keepers, skipped) = plan(
            &[claude_worker, agy_post_boot, cursor, agy_lead, agy_pre_boot],
            &listing,
            &HashSet::new(),
            &HashSet::new(),
            BOOT,
        );
        assert!(leads == vec!["agy-lead".to_string()], "{leads:?}");
        assert_eq!(workers, vec!["kestrel"]);
        assert_eq!(keepers, vec!["agy-a", "cursor-a"]);
        assert!(skipped.is_empty(), "{skipped:?}");
    }

    /// A live pi, grok or gemini row is a skipped receipt row whose reason
    /// names the harness's keeper stanza and the missing revival word.
    #[test]
    #[allow(clippy::indexing_slicing)] // order-pinning asserts on the receipt rows
    fn plan_names_the_missing_keeper_revival() {
        let pi = keeper_row("pi-a", "pi11111-session", "pi", AgentStatus::Live);
        let grok = keeper_row("grok-a", "grok1111-session", "grok", AgentStatus::Live);
        let gemini = keeper_row("gem-a", "gem11111-session", "gemini", AgentStatus::Live);
        let listing = ClaudeAgentsSnapshot::known(vec![]);
        let (leads, workers, keepers, skipped) = plan(
            &[pi, grok, gemini],
            &listing,
            &HashSet::new(),
            &HashSet::new(),
            BOOT,
        );
        assert!(leads.is_empty() && workers.is_empty() && keepers.is_empty());
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        for r in &skipped {
            assert!(
                r.reason.contains("does not carry resume_session_id"),
                "{skipped:?}"
            );
        }
        assert!(
            skipped[0].reason.contains("[harness.pi.keeper]"),
            "{skipped:?}"
        );
        assert!(
            skipped[1].reason.contains("[harness.grok.keeper]"),
            "{skipped:?}"
        );
        assert!(
            skipped[2].reason.contains("[harness.gemini.keeper]"),
            "{skipped:?}"
        );
    }

    /// The unread listing is claude's answer: the claude row waits as a
    /// named skip row while the keeper row still plans.
    #[test]
    #[allow(clippy::indexing_slicing)] // order-pinning asserts on the receipt rows
    fn an_unread_listing_skips_only_claude_rows() {
        let claude = row("kestrel", "11112222-bbbb", AgentStatus::Orphaned);
        let agy = keeper_row("agy-a", "agy11111-session", "agy", AgentStatus::Live);
        let listing = ClaudeAgentsSnapshot::unknown("no roster file");
        let (leads, workers, keepers, skipped) = plan(
            &[claude, agy],
            &listing,
            &HashSet::new(),
            &HashSet::new(),
            BOOT,
        );
        assert!(leads.is_empty() && workers.is_empty());
        assert_eq!(keepers, vec!["agy-a"]);
        assert_eq!(skipped.len(), 1, "{skipped:?}");
        assert!(skipped[0].reason.contains("unread"), "{skipped:?}");
        assert!(skipped[0].reason.contains("interval kick"), "{skipped:?}");
    }

    /// Keeper rows chunk through the batch without the claude resolve: that
    /// resolve exists to feed the supervisor birth guard, and its refusal is
    /// never a keeper row's receipt.
    #[test]
    fn the_batch_chunks_keeper_rows_without_the_claude_resolve() {
        let home = AgentsHome::at(tempfile::tempdir().unwrap().keep());
        let revived: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let resolve_calls: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let rows = revive_batch(
            &home,
            &[],
            &["c1".to_string()],
            &["k1".to_string()],
            |_| {},
            |_, name| {
                revived.lock().unwrap().push(name.to_string());
                ReceiptRow::new(name, "revived", "x")
            },
            |_, name| {
                resolve_calls.lock().unwrap().push(name.to_string());
                Err("the re-entry resolver is claude-only".to_string())
            },
        );
        let revived = revived.into_inner().unwrap();
        assert_eq!(
            revived,
            vec!["k1", "c1"],
            "the keeper chunk runs first; the resolve-failed claude row never chunks and retries once"
        );
        let resolve_calls = resolve_calls.into_inner().unwrap();
        assert_eq!(resolve_calls, vec!["c1"]);
        assert!(rows.iter().all(|r| r.outcome == "revived"), "{rows:?}");
    }

    /// The dispatcher routes a non-claude row whose harness carries no
    /// revival to a named refusal, never the claude-only resolver's error.
    #[test]
    fn the_dispatcher_names_a_missing_keeper_revival_instead_of_the_claude_resolver() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        std::fs::create_dir_all(&cwd).unwrap();
        let home = AgentsHome::at(dir.path().join("agents"));
        std::fs::create_dir_all(home.root()).unwrap();
        let registry = home.registry_json();
        crate::registry_store::seed_raw(
            &registry,
            serde_json::json!({
                "schema_version": crate::state::REGISTRY_SCHEMA_VERSION,
                "agents": [
                    {"name": "gem-a", "cwd": cwd.display().to_string(), "status": "live",
                     "harness": "gemini", "harness_session_id": "gem11111-session"},
                    {"name": "pi-a", "cwd": cwd.display().to_string(), "status": "live",
                     "harness": "pi", "harness_session_id": "pi11111-session"},
                ],
            })
            .to_string(),
        );
        for name in ["gem-a", "pi-a"] {
            let r = revive_unattended(&home, name);
            assert_eq!(r.outcome, "failed", "{r:?}");
            assert!(r.reason.contains("[harness."), "{:?}", r.reason);
            assert!(
                r.reason.contains("does not carry resume_session_id"),
                "{:?}",
                r.reason
            );
        }
        std::fs::remove_dir_all(dir.path()).ok();
    }

    /// The keeper mirror of the claude standing-stop rule: a row stopped on
    /// purpose on a merged node stays down; a stop a resume already passed
    /// (live row) and a bare reboot death still come back.
    #[test]
    #[allow(clippy::indexing_slicing)] // order-pinning asserts on the receipt rows
    fn a_deliberately_stopped_keeper_row_on_a_merged_node_stays_down() {
        let mut standing = keeper_row("agy-s", "agy11111-session", "agy", AgentStatus::Exited);
        standing.node = Some("x-merged".into());
        standing.exited_at = Some("2026-09-26T01:01:00Z".into());
        crate::state::record_stop(&mut standing, "lead", None);
        let mut stale = keeper_row("agy-b", "agy22222-session", "agy", AgentStatus::Live);
        stale.node = Some("x-merged".into());
        crate::state::record_stop(&mut stale, "lead", None);
        let mut closed = keeper_row("agy-c", "agy33333-session", "agy", AgentStatus::Live);
        closed.node = Some("x-done".into());
        let closed_set = HashSet::from(["x-done".to_string()]);
        let merged = HashSet::from(["x-merged".to_string()]);
        let listing = ClaudeAgentsSnapshot::known(vec![]);
        let (leads, workers, keepers, skipped) = plan(
            &[standing, stale, closed],
            &listing,
            &closed_set,
            &merged,
            BOOT,
        );
        assert!(leads.is_empty() && workers.is_empty());
        assert_eq!(keepers, vec!["agy-b"]);
        assert_eq!(skipped.len(), 2, "{skipped:?}");
        assert!(skipped[0].reason.contains("never revived"), "{skipped:?}");
        assert!(
            skipped[1].reason.contains("done or superseded"),
            "{skipped:?}"
        );
    }
}
