//! Row retirement, keyed by the reverse join through `node.sessions[]`
//!.
//!
//! A worker's registry row leaves when its WORK is done and its transcript
//! is quiet. WORK-done is the graph's `status == "done"` read through the
//! reverse join ([`crate::graph_store::work_state`]) over every node the
//! session is named on; quiet is the served transcript mtime past the retire
//! grace. Nothing waits for a session to end, because a session never ends
//! (d-10a72d88): the exit-stamp machinery this module used to carry
//! (`exited_at`, `StampExit`, corroboration gates, the backstop, the dormant
//! probe) asked a question with no answer and is deleted.
//!
//! The row question and the worktree question are DIFFERENT questions with
//! different keys. A row retires on work-done plus quiet regardless of its
//! worktree; the tree is then governed by its own bucket (dirty never,
//! clean-and-unmerged never, clean-and-merged loses the tree and keeps the
//! branch), so a dirty tree never pins a finished row and a row retirement
//! never destroys an unmerged branch's only checkout.
//!
//! This module holds the pure decision plus the sweep shells. The sweep body
//! lives in `gc_sweep.rs`. All I/O - the graph read, the transcript stat, the
//! stop, the worktree probes, the clock - is injected, so the policy is
//! unit-testable in isolation.

use crate::graph_store::WorkState;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// The row verdict: retire now, or keep with a named reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcAction {
    /// The row leaves the registry. The receipt records the resumable handle
    /// first, so the removal is reversible.
    Retire,
    /// The row stays, and [`KeepReason`] names the positive fact holding it.
    Keep,
}

/// The probed facts about one registry row the retirement policy needs.
#[derive(Debug, Clone)]
pub struct GcRow {
    /// The row's `origin` (state.rs). Only `"spawn"` changes the verdict: an
    /// fno-started row takes the one idle hour (d-e3defbb7).
    pub origin: Option<String>,
    /// An orchestrator team rides this row (US9), or it holds a lead role.
    pub teamed: bool,
    /// WORK through the reverse join. It labels the receipt and names the
    /// node an open PR rides on; it never keeps a row by itself.
    pub work: WorkState,
    /// Seconds since the row's transcript was last written, from the served
    /// harness store. `None` when no transcript resolved.
    pub transcript_age_s: Option<i64>,
    /// Seconds since the row's last session report, else since it was
    /// created: the clock a row with no transcript ages on.
    pub report_age_s: Option<i64>,
    /// Does this row own a REMOVABLE worktree? False for a one-shot ask, a
    /// row in the canonical checkout, or a cwd that is not a linked worktree.
    pub owns_worktree: bool,
    /// Worktree cleanliness: `Some(true)` clean, `Some(false)` dirty,
    /// `None` the probe could not answer (fail closed -> tree kept).
    pub worktree_clean: Option<bool>,
    /// Is the worktree's branch merged into the main line? Asked only after
    /// cleanliness answered `true`.
    pub branch_merged: Option<bool>,
    /// The row is a planner (blueprint/think phase, or a `bp-` label). A
    /// spawned planner's idle window is 20 minutes (d-aecc0fdc).
    pub planner: bool,
    /// The head of the transcript: `Some(true)` the session ran an fno skill
    /// (target, fix, blueprint, review), `Some(false)` a transcript with no
    /// skill call (a brainstorm, kept by law d-defa43c7), `None` no
    /// transcript at all (a failed start: it ages on the report clock).
    pub fno_skill: Option<bool>,
    /// The tail of the transcript ends the work: a `<promise>` with no newer
    /// user turn after it. Such a row waits only the grace, never the window.
    pub tail_done: bool,
    /// A live or suspect work-claim whose holder pid answered names this
    /// session as its holder.
    pub live_claim: Option<String>,
    /// The `(node, pr)` THIS session drives and the PR still reads open.
    pub open_pr: Option<(String, u64)>,
    /// A live newer peer drives the same PR, so this row's open-PR keep does
    /// not apply.
    pub peer_drives_pr: bool,
    /// The row is an eval worker whose attempt the evals history reads
    /// terminal (`graded` or `unavailable`): finished work by definition.
    pub eval_finished: bool,
}

/// The statuses that complete a PLANNING assignment: the plan was written
/// and the node moved on. `idea` is the loud exception - an idea node never
/// received the plan.
pub const PLANNING_COMPLETE_STATUSES: [&str; 5] =
    ["done", "ready", "in_progress", "in_review", "shipped"];

/// Statuses that finish a planning assignment with NO marker: a node that
/// moved to `deferred` or `superseded` has nothing left to plan.
pub const PLANNING_MOVED_ON_STATUSES: [&str; 2] = ["deferred", "superseded"];

/// The idle window for a PLANNER row (law d-aecc0fdc): 20 quiet minutes.
pub const PLANNING_IDLE_RETIRE_SECS: i64 = 1200;

/// Node statuses that are NOT active work. A parked or never-started node is
/// not evidence that a session is alive.
pub const INACTIVE_NODE_STATUSES: [&str; 2] = ["deferred", "idea"];

/// The 2026-10-21 origin ruling (d-e3defbb7): an fno-started row retires
/// after one idle hour.
pub const SPAWN_IDLE_RETIRE_SECS: i64 = 3_600;
pub const SPAWN_IDLE_GRACE_SECS: i64 = 3_300;

/// Law d-defa43c7: any other session that ran an fno skill retires after one
/// idle day, and so does a row with no transcript at all.
pub const SKILL_IDLE_RETIRE_SECS: i64 = 86_400;

/// Law d-b01ffede: a row whose node is done retires after 30 idle minutes,
/// whatever its origin.
pub const DONE_NODE_IDLE_RETIRE_SECS: i64 = 1_800;

/// WHICH positive fact holds a [`GcAction::Keep`] row. Removal is the
/// default, so an unknown fact never keeps a row: there is no variant for
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeepReason {
    /// K1: the newest transcript entry, else the last report, is inside the
    /// row's idle window.
    Active { age_s: i64 },
    /// K1: neither clock parses. Nothing measures the row, so nothing ages it.
    NoClock,
    /// K2: a live work-claim whose holder pid answered names this session.
    LiveClaim { detail: String },
    /// K3: the session drives an open PR (law d-aecc0fdc: keep and nudge).
    OpenPr { node: String, pr: u64 },
    /// K4: a teamed orchestrator row or a lead role.
    Teamed,
    /// Law d-defa43c7: the transcript shows no fno skill call. A brainstorm
    /// between the user and an agent is kept whatever its age.
    Brainstorm,
}

impl KeepReason {
    /// Stable, human-readable tag for CLI/JSON output.
    pub fn as_str(&self) -> &'static str {
        match self {
            KeepReason::Active { .. } => "active",
            KeepReason::NoClock => "no clock: neither transcript nor report age",
            KeepReason::LiveClaim { .. } => "live claim held",
            KeepReason::OpenPr { .. } => "open pr",
            KeepReason::Teamed => "teamed",
            KeepReason::Brainstorm => "brainstorm: no fno skill in the transcript",
        }
    }
}

/// The tree verdict for a RETIRED row's worktree. Asked only after the row
/// verdict is `Retire`; a keep-shaped tree never blocks the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeAction {
    /// Remove the tree, keep the branch: clean and merged.
    Prune,
    /// Dirty: uncommitted or untracked content. Tree kept, named.
    KeepDirty,
    /// Clean but the branch never merged: abandoned-but-real work a human
    /// judges. Tree kept, named.
    KeepUnmerged,
    /// The cleanliness probe could not answer. Tree kept, named.
    KeepUnprobed,
    /// The row owns no removable worktree.
    None,
}

/// The one row decision. Removal is reversible (the receipt holds the resume
/// command), so the default is retire. A row keeps only on a positive fact:
/// a team, a live claim, an open PR it drives, a brainstorm transcript, or a
/// clock inside its idle window.
pub fn gc_decide(row: &GcRow, grace_secs: i64) -> (GcAction, Option<KeepReason>) {
    if row.teamed {
        return (GcAction::Keep, Some(KeepReason::Teamed));
    }
    if let Some(detail) = &row.live_claim {
        let detail = detail.clone();
        return (GcAction::Keep, Some(KeepReason::LiveClaim { detail }));
    }
    if let Some((node, pr)) = &row.open_pr {
        if !row.peer_drives_pr {
            let (node, pr) = (node.clone(), *pr);
            return (GcAction::Keep, Some(KeepReason::OpenPr { node, pr }));
        }
    }
    if row.fno_skill == Some(false) && !row.eval_finished && row.origin.as_deref() != Some("spawn")
    {
        return (GcAction::Keep, Some(KeepReason::Brainstorm));
    }
    let window = idle_window(row, grace_secs);
    match row.transcript_age_s.or(row.report_age_s) {
        None => (GcAction::Keep, Some(KeepReason::NoClock)),
        Some(age_s) if age_s <= window => (GcAction::Keep, Some(KeepReason::Active { age_s })),
        Some(_) => (GcAction::Retire, None),
    }
}

/// How long a row may sit quiet before it retires. Finished work (a promise
/// tail, a terminal eval) waits only the grace. A spawned planner waits 20
/// minutes, any other spawned row one hour, and every other row one day: a
/// planning session the user started by hand is not a dispatched planner.
/// A row whose every node is done waits at most 30 minutes (law d-b01ffede).
pub fn idle_window(row: &GcRow, grace_secs: i64) -> i64 {
    let window = origin_idle_window(row, grace_secs);
    if matches!(row.work, WorkState::AllDone { .. }) {
        window.min(DONE_NODE_IDLE_RETIRE_SECS)
    } else {
        window
    }
}

fn origin_idle_window(row: &GcRow, grace_secs: i64) -> i64 {
    let spawned = row.origin.as_deref() == Some("spawn");
    if row.tail_done || row.eval_finished {
        grace_secs
    } else if spawned && row.planner {
        PLANNING_IDLE_RETIRE_SECS
    } else if spawned {
        SPAWN_IDLE_RETIRE_SECS
    } else {
        SKILL_IDLE_RETIRE_SECS
    }
}

/// The tree verdict for a row the policy just retired. Runs ONLY on Retire:
/// row retirement makes a tree eligible and never bypasses the bucket.
pub fn tree_action(row: &GcRow) -> TreeAction {
    if !row.owns_worktree {
        return TreeAction::None;
    }
    match row.worktree_clean {
        None => TreeAction::KeepUnprobed,
        Some(false) => TreeAction::KeepDirty,
        Some(true) => match row.branch_merged {
            Some(true) => TreeAction::Prune,
            _ => TreeAction::KeepUnmerged,
        },
    }
}

/// The one handle a row is both PROBED and REPORTED under. `fno agents truth`
/// resolves a row by short_id or by name, so the fallback is a real handle,
/// not a display string. Written once so the sweep never probes under one
/// name and reports under another.
pub fn row_handle(e: &crate::state::RegistryEntry) -> String {
    // The session id resolves for BOTH populations (`fno agents truth`
    // answers a full harness session id for registry and roster rows alike);
    // an fno short id resolves for registry rows only, so a roster-only
    // row probed under its short id always answered not-found and the
    // sweep built for it could never age one.
    if let Some(sid) = e.harness_session_id.as_deref().filter(|s| !s.is_empty()) {
        return sid.to_string();
    }
    if e.short_id.is_empty() {
        e.name.clone()
    } else {
        e.short_id.clone()
    }
}

/// The label a sweep REPORTS a row under: the short id, falling back to the
/// name - the operator-facing identity every summary bucket, hold, and
/// release ruling keys on. change 1 split this from [`row_handle`],
/// the PROBE handle: the probe must ask the harness session id, while the
/// report keeps the handle an operator (and `reap --release`) already holds.
pub fn row_label(e: &crate::state::RegistryEntry) -> String {
    if e.short_id.is_empty() {
        e.name.clone()
    } else {
        e.short_id.clone()
    }
}

/// The production transcript-age seam: the NEWEST TIMESTAMPED
/// transcript entry, read through the shared truth probe (one batched,
/// single-flighted child per sweep) - not a file stat, whose untimestamped
/// trailing records keep a dead file reading fresh (measured median +20 min,
/// max +240 h). A handle the probe cannot resolve is absent from the map, and
/// the sweep reads absence as `None`: an unresolved transcript is never a
/// quiet one.
pub fn probe_entry_ages(
    entries: &[&crate::state::RegistryEntry],
) -> std::collections::HashMap<String, Option<i64>> {
    let handles: Vec<String> = entries.iter().map(|e| row_handle(e)).collect();
    if handles.is_empty() {
        return std::collections::HashMap::new();
    }
    crate::truth_probe::family1_truth_probe_many(&handles)
        .into_iter()
        .map(|(handle, probe)| (handle, probe.last_activity_age_s.map(|a| a as i64)))
        .collect()
}

// --- the sweep shells -------------------------------------------------------
// The two triggers behind the pure decision above: the daemon's idle tick and
// the manual `fno agents reap` command both shell these. The sweep body lives in
// `gc_sweep.rs`; these resolve the production seams (graph read, store index,
// stop lane) and call it.

use crate::events::EventEmitter;
use crate::gc_sweep;
use crate::paths::AgentsHome;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// The next guard window's interval, resolved off-loop by the sweep body and
/// handed back for later ticks to read: the idle-probe verdict pattern, so
/// the select arm never blocks on config reads. Empty until the first sweep
/// lands, which reads as the default.
pub type RetireIntervalCell = Mutex<Option<Duration>>;

/// The interval the next guard window compares against: the last value the
/// sweep body handed back, or [`crate::agents_config::DEFAULT_RETIRE_INTERVAL_SECS`]
/// before the first handoff.
pub fn retire_interval_snapshot(cell: &Arc<RetireIntervalCell>) -> Duration {
    Duration::from_secs(
        cell.lock()
            .ok()
            .and_then(|c| *c)
            .map(|d| d.as_secs())
            .unwrap_or(crate::agents_config::DEFAULT_RETIRE_INTERVAL_SECS),
    )
}

/// Boot-time seed for the interval cell: one config read at daemon start so
/// the first guard window already honors the configured cadence instead of
/// waiting out the default; the sweep body keeps the cell fresh off-loop
/// afterward.
pub fn seed_retire_interval_cell(grace_cwd: &std::path::Path) -> Arc<RetireIntervalCell> {
    let grace = crate::agents_config::retire_grace_secs(grace_cwd);
    Arc::new(Mutex::new(Some(Duration::from_secs(
        crate::agents_config::retire_interval_s(grace_cwd, grace),
    ))))
}

/// The daemon idle tick's retirement sweep: classify every row, retire the
/// work-done-and-quiet ones (stop the held process first), prune their
/// clean-and-merged worktrees, and write the receipt every removal needs to
/// stay reversible.
pub fn gc_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    retain_days: u64,
) -> gc_sweep::GcSummary {
    // The settle writes the graph FIRST: the row pass then reads the file it
    // wrote, so a filled row reads closed and its session falls through to
    // the ordinary quiet and receipt gates. A refused settle leaves the row
    // kept under its existing reason.
    let (settled, mut refused) = gc_sweep::settle_stale_do_rows(home);
    refused.extend(crate::phase_close::settle_ship_rows(home));
    let store = std::cell::RefCell::new(gc_sweep::HarnessStoreIndex::default());
    let mut summary = gc_sweep::run(
        home,
        emitter,
        grace_secs,
        false,
        retain_days,
        &gc_sweep::read_graph_entries,
        &|e| store.borrow_mut().matches(e),
        &probe_entry_ages,
        &|e| gc_sweep::stop_row_process(home, e),
        &crate::gc_native::apply_active_surface_removal,
        &crate::gc_native::apply_mux_member_retirement,
        &crate::claude_roster::read_all_agents,
        &gc_sweep::production_tree_probe,
        &crate::daemon::rm_take_worktree,
    );
    summary.settled_do_rows = settled
        .into_iter()
        .map(|row| (row.node, row.harness, row.session_id))
        .collect();
    summary.settle_refused = refused;
    // A reap leaves nothing behind: a retired worker's dead cargo still
    // holds the build lock and a run slot under its own pid.
    let freed = crate::claim_commands::reap_dead_cargo_claims();
    if freed > 0 {
        eprintln!("reap: freed {freed} build or run-slot claim(s) held by a dead cargo");
    }
    summary
}

/// `fno agents reap --release <row>`: the same production seams as
/// [`gc_sweep`], with one release ruling riding the pass. The
/// settle runs first exactly as the real sweep runs it, so a do row the
/// batch can fill is already filled before the row pass reads the graph.
pub fn gc_sweep_release(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    retain_days: u64,
    release: &gc_sweep::Release,
) -> gc_sweep::GcSummary {
    let (settled, refused) = gc_sweep::settle_stale_do_rows(home);
    let store = std::cell::RefCell::new(gc_sweep::HarnessStoreIndex::default());
    let mut summary = gc_sweep::run_with_release(
        home,
        emitter,
        grace_secs,
        false,
        retain_days,
        &gc_sweep::read_graph_entries,
        &|e| store.borrow_mut().matches(e),
        &probe_entry_ages,
        &|e| gc_sweep::stop_row_process(home, e),
        &crate::gc_native::apply_active_surface_removal,
        &crate::gc_native::apply_mux_member_retirement,
        &crate::claude_roster::read_all_agents,
        &gc_sweep::production_tree_probe,
        &crate::daemon::rm_take_worktree,
        false,
        Some(release),
        None,
    );
    summary.settled_do_rows = settled
        .into_iter()
        .map(|row| (row.node, row.harness, row.session_id))
        .collect();
    summary.settle_refused = refused;
    summary
}

/// `fno agents reap --dry-run`: classify exactly as [`gc_sweep`] does, name
/// every row under exactly one bucket, mutate nothing - a reaper an operator
/// cannot rehearse is one they will not run.
pub fn gc_sweep_dry_run(home: &AgentsHome, grace_secs: i64) -> gc_sweep::GcSummary {
    // The settle plan is read-only, and the rehearsal subtracts it from the
    // graph read so the report shows the outcome the real pass would produce.
    let mut pr_reader = crate::additional_prs::gh_pr_state_reader();
    let (planned, stamps) = crate::additional_prs::plan_settle(home, &mut pr_reader);
    let read = |h: &AgentsHome| {
        gc_sweep::read_graph_entries(h).map(|g| gc_sweep::without_settled(g, &planned, &stamps))
    };
    // Never emitted to in dry-run mode (the whole write+emit tail is skipped),
    // so an unused placeholder path satisfies the shared signature.
    let emitter = EventEmitter::new(std::path::PathBuf::new(), "daemon");
    let store = std::cell::RefCell::new(gc_sweep::HarnessStoreIndex::default());
    // The rehearsal reads the same PR states the real arm would read, so its
    // prediction holds when applied. Read-only; cached per PR per pass.
    let preview = dry_surface_preview();
    let mut summary = gc_sweep::run_with_release(
        home,
        &emitter,
        grace_secs,
        true,
        0, // dry-run never expires: a rehearsal that pruned would not be one
        &read,
        &|e| store.borrow_mut().matches(e),
        &probe_entry_ages,
        &|e| gc_sweep::stop_row_process(home, e),
        &crate::gc_native::apply_active_surface_removal,
        &crate::gc_native::apply_mux_member_retirement,
        &crate::claude_roster::read_all_agents,
        &gc_sweep::production_tree_probe,
        &crate::daemon::rm_take_worktree,
        false,
        None,
        Some(&preview),
    );
    summary.settled_do_rows = planned
        .into_iter()
        .map(|row| (row.node, row.harness, row.session_id))
        .collect();
    // The ladder's DRY-RUN plan: decisions only, no effect, no state write.
    let ladder_rows = summary.open_pr_rows.clone();
    summary.open_pr_nudge = crate::pr_nudge::plan(home, &ladder_rows, grace_secs);
    summary
}

/// `fno agents registry reap --done`: the bulk all-clear pass. The same
/// seams and gates as [`gc_sweep`], scoped to rows whose work is done or
/// whose terminal eval attempt marks the row finished: the merge the user
/// already did is the all-clear, so the merge's rows leave the roster in
/// one pass, and every other row is out of scope for that pass -
/// untouched, unjudged, rendered nowhere.
pub fn gc_sweep_done_only(
    home: &AgentsHome,
    emitter: &EventEmitter,
    grace_secs: i64,
    retain_days: u64,
) -> gc_sweep::GcSummary {
    let (settled, mut refused) = gc_sweep::settle_stale_do_rows(home);
    refused.extend(crate::phase_close::settle_ship_rows(home));
    let store = std::cell::RefCell::new(gc_sweep::HarnessStoreIndex::default());
    let mut summary = gc_sweep::run_done_only(
        home,
        emitter,
        grace_secs,
        false,
        retain_days,
        &gc_sweep::read_graph_entries,
        &|e| store.borrow_mut().matches(e),
        &probe_entry_ages,
        &|e| gc_sweep::stop_row_process(home, e),
        &crate::gc_native::apply_active_surface_removal,
        &crate::gc_native::apply_mux_member_retirement,
        &crate::claude_roster::read_all_agents,
        &gc_sweep::production_tree_probe,
        &crate::daemon::rm_take_worktree,
    );
    summary.settled_do_rows = settled
        .into_iter()
        .map(|row| (row.node, row.harness, row.session_id))
        .collect();
    summary.settle_refused = refused;
    summary
}

/// `fno agents registry reap --done` without `--apply`: the same scoped
/// pass as [`gc_sweep_done_only`], rehearsed - classify, name every row
/// under exactly one bucket, mutate nothing.
pub fn gc_sweep_done_only_dry_run(home: &AgentsHome, grace_secs: i64) -> gc_sweep::GcSummary {
    let emitter = EventEmitter::new(std::path::PathBuf::new(), "daemon");
    let store = std::cell::RefCell::new(gc_sweep::HarnessStoreIndex::default());
    let preview = dry_surface_preview();
    gc_sweep::run_with_release(
        home,
        &emitter,
        grace_secs,
        true,
        0,
        &gc_sweep::read_graph_entries,
        &|e| store.borrow_mut().matches(e),
        &probe_entry_ages,
        &|e| gc_sweep::stop_row_process(home, e),
        &crate::gc_native::apply_active_surface_removal,
        &crate::gc_native::apply_mux_member_retirement,
        &crate::claude_roster::read_all_agents,
        &gc_sweep::production_tree_probe,
        &crate::daemon::rm_take_worktree,
        true,
        None,
        Some(&preview),
    )
}

/// The dry runs' read-only removal preview: the same roster union and squad
/// store the live effects read, each read at most once per pass and only
/// when a row reaches the gate.
fn dry_surface_preview() -> impl Fn(&crate::state::RegistryEntry) -> bool {
    let union = std::cell::OnceCell::new();
    let members = std::cell::OnceCell::new();
    move |e| {
        crate::gc_native::surface_provably_clear(
            e,
            union.get_or_init(crate::claude_roster::read_all_agents_union),
            members.get_or_init(crate::gc_inventory::read_mux_members),
        )
    }
}

// --- the orphan process sweep -----------------------------------------------
//
// `gc_decide` above reaps registry ROWS. Nothing reaped PROCESSES, and one was
// measured at 44.7% CPU with its parent gone: `agents stale-escalate --json`,
// reparented to init, with nothing on the machine that would ever notice it.
// Every other `libc::kill` in this crate is a targeted teardown of a pid the
// caller already owns.

/// The event the sweep emits on EVERY run, including the ones that reap
/// nothing. A reaper that speaks only when it kills cannot be told apart from a
/// reaper that never ran, and the difference is the whole question an operator
/// is asking when they go looking.
pub const ORPHAN_SWEEP_EVENT: &str = "orphan_reap_sweep";

/// How long a SIGTERMed process is given to leave before SIGKILL. Mirrors the
/// grace `cursor_agent::reap_detached_worker_servers` already uses.
const REAP_GRACE: Duration = Duration::from_secs(2);

/// One row of the process table, reduced to what the reap gate reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcRow {
    pub pid: u32,
    pub ppid: u32,
    pub age_secs: u64,
    pub args: String,
}

/// Whether this row is a child that init inherited and nobody is waiting on.
///
/// The conjunction IS the gate, and every clause is load-bearing, because the
/// cost of a false positive is killing a process a person is using:
///
/// - **argv names a reparable family.** Either the `fno-py` entrypoint, a
///   stdio MCP server in the codegraph family (measured 2026-10-02: every
///   `claude bg-spare` held one `codegraph serve --mcp` server plus a
///   `--liftoff-only` worker, 24 servers at 5.6 GB, and after their session
///   died they sat on init unreaped), or Claude Code pool machinery whose
///   daemon died: a `claude bg-pty-host` / `claude bg-spare` pair orphaned to
///   init (2026-10-07: 28 pty hosts, most 0.0% CPU for hours, three 12-33h
///   orphans of dead daemons still resident; the 2026-10-03 load
///   investigation named the idle baseline). A live daemon's pool is never
///   matched: its hosts have a real parent. Matched on whitespace-separated
///   TOKENS, never substrings: `--mcp-config` contains the string and is not
///   this.
/// - **parent pid 1.** A live foreground `fno` has a real parent. Without this
///   clause the gate matches every ordinary command an operator is running.
/// - **older than the threshold.** Without it the sweep races a child whose
///   parent is mid-exit and has simply not been reaped yet.
/// - **no registry row names the pid live.** A worker the fleet is tracking is
///   not an orphan even when it looks like one from out here.
pub fn is_reapable_orphan(row: &ProcRow, older_than: Duration, live_pids: &[u32]) -> bool {
    row.ppid == 1
        && row.age_secs >= older_than.as_secs()
        && !live_pids.contains(&row.pid)
        && row.pid != std::process::id()
        && (names_fno_py(&row.args)
            || names_stdio_mcp_server(&row.args)
            || is_claude_spare(&row.args))
}

fn names_fno_py(args: &str) -> bool {
    args.split_whitespace()
        .any(|token| token.rsplit('/').next() == Some("fno-py"))
}

/// True when argv names a stdio MCP server in the codegraph family: the
/// `serve --mcp` token pair that convention carries, or the codegraph
/// launcher's `--liftoff-only` V8 flag beside a `codegraph` path token (the
/// flag alone is generic V8, so only the pairing names this family). A server
/// whose argv carries neither marker is invisible to the reap gate; a bare
/// `--mcp` flag without the `serve` pair is not a match.
fn names_stdio_mcp_server(args: &str) -> bool {
    let tokens: Vec<&str> = args.split_whitespace().collect();
    if tokens
        .windows(2)
        .any(|w| w[0] == "serve" && w[1] == "--mcp")
    {
        return true;
    }
    tokens.iter().any(|t| *t == "--liftoff-only")
        && tokens
            .iter()
            .any(|t| t.rsplit('/').next().unwrap_or(t).contains("codegraph"))
}

/// True when argv names Claude Code pool machinery: the daemon's spare pool
/// (`claude bg-spare ...`) or its pty host (`claude bg-pty-host ...`). Same
/// shapes `spawn_context::is_claude_pool_machinery` matches; mirrored here
/// because the sweep reads its own flat [`ProcRow`] table, not the census.
fn is_claude_spare(args: &str) -> bool {
    args.split_whitespace()
        .any(|a| a == "--bg-spare" || a == "--bg-pty-host")
}

/// How many rows of `table` are stdio MCP servers whose PARENT is a live
/// spare. Pure count, no signal: an MCP child of a live spare may be the
/// server its session adopts, so it is measured, never reaped here. The
/// 2026-10-02 pile-up (21 spares, one server each) was invisible because
/// nothing counted it; this number in the sweep event is the count.
fn count_mcp_children_of_spares(table: &[ProcRow]) -> usize {
    let spare_pids: std::collections::HashSet<u32> = table
        .iter()
        .filter(|row| is_claude_spare(&row.args))
        .map(|row| row.pid)
        .collect();
    table
        .iter()
        .filter(|row| spare_pids.contains(&row.ppid) && names_stdio_mcp_server(&row.args))
        .count()
}

/// Parse `ps -o etime=`: `[[DD-]HH:]MM:SS`.
///
/// `etimes` (elapsed SECONDS, no parsing) exists on Linux and NOT on macOS, so
/// the portable column is the formatted one. `None` on anything unparseable,
/// which reads as "unknown age" and therefore never reaps: an age the gate
/// cannot establish is not evidence the process is old.
pub fn parse_etime(raw: &str) -> Option<u64> {
    let raw = raw.trim();
    let (days, rest) = match raw.split_once('-') {
        Some((d, rest)) => (d.parse::<u64>().ok()?, rest),
        None => (0, raw),
    };
    let mut parts = rest.split(':').rev();
    let secs = parts.next()?.parse::<u64>().ok()?;
    let mins = parts.next()?.parse::<u64>().ok()?;
    let hours = match parts.next() {
        Some(h) => h.parse::<u64>().ok()?,
        None => 0,
    };
    if parts.next().is_some() {
        return None;
    }
    Some(days * 86_400 + hours * 3_600 + mins * 60 + secs)
}

/// Read the process table once per sweep.
///
/// One `ps` for the whole machine, not one probe per candidate: a sweep whose
/// job is to reduce the process count must not be a fan-out of its own.
fn read_proc_table() -> Vec<ProcRow> {
    let output = match std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,etime=,args="])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return Vec::new(),
    };
    String::from_utf8_lossy(&output)
        .lines()
        .filter_map(parse_proc_line)
        .collect()
}

fn parse_proc_line(line: &str) -> Option<ProcRow> {
    let mut fields = line.split_whitespace();
    let pid = fields.next()?.parse().ok()?;
    let ppid = fields.next()?.parse().ok()?;
    let age_secs = parse_etime(fields.next()?)?;
    let args = fields.collect::<Vec<_>>().join(" ");
    if args.is_empty() {
        return None;
    }
    Some(ProcRow {
        pid,
        ppid,
        age_secs,
        args,
    })
}

/// Whether this pid is STILL the process the table said it was.
///
/// The gap between reading the table and signalling is small, and a pid that
/// exits inside it can be reused by something unrelated. This is a process
/// killer, so the window gets closed rather than reasoned about: re-read the
/// one row and require the same argv and the same inherited parent. An
/// unreadable answer is not a match, because absence is not evidence.
fn still_the_same(row: &ProcRow) -> bool {
    let output = match std::process::Command::new("ps")
        .args(["-p", &row.pid.to_string(), "-o", "ppid=,args="])
        .output()
    {
        Ok(o) if o.status.success() => o.stdout,
        _ => return false,
    };
    let text = String::from_utf8_lossy(&output);
    let Some(line) = text.lines().next() else {
        return false;
    };
    let mut fields = line.split_whitespace();
    let ppid: u32 = match fields.next().and_then(|f| f.parse().ok()) {
        Some(p) => p,
        None => return false,
    };
    ppid == row.ppid && fields.collect::<Vec<_>>().join(" ") == row.args
}

/// SIGTERM, then SIGKILL after the grace window. Returns false when the process
/// was already gone, changed identity, or refused the signal, so the summary
/// counts what it actually ended rather than what it aimed at.
fn terminate(row: &ProcRow) -> bool {
    let pid = row.pid;
    let alive = |p: u32| unsafe { libc::kill(p as libc::pid_t, 0) } == 0;
    if !still_the_same(row) {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return false;
    }
    let deadline = std::time::Instant::now() + REAP_GRACE;
    while alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    if alive(pid) {
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
    true
}

/// Reap every `fno-py` child that init inherited and nobody is waiting on.
///
/// `live_pids` are the pids the registry vouches for. `None` means the registry
/// could not be read, and the sweep then reaps NOTHING: an unreadable registry
/// is not evidence that no worker is live, and reading it that way would turn
/// one bad file into a fleet-wide kill. It still emits, naming the refusal, so
/// a skipped run is distinguishable from a quiet one.
///
/// Emits [`ORPHAN_SWEEP_EVENT`] on every run, naming the count it reaped even
/// when that count is zero.
pub fn orphan_sweep(
    emitter: &EventEmitter,
    older_than: Duration,
    live_pids: Option<&[u32]>,
) -> usize {
    let table = read_proc_table();
    let candidates: Vec<&ProcRow> = match live_pids {
        Some(live) => table
            .iter()
            .filter(|row| is_reapable_orphan(row, older_than, live))
            .collect(),
        None => Vec::new(),
    };
    let reaped = candidates.iter().filter(|row| terminate(row)).count();
    let _ = emitter.emit(
        ORPHAN_SWEEP_EVENT,
        &serde_json::json!({
            "scanned": table.len(),
            "candidates": candidates.len(),
            "reaped": reaped,
            "older_than_secs": older_than.as_secs(),
            "skipped": live_pids.is_none(),
            "mcp_children_of_spares": count_mcp_children_of_spares(&table),
        }),
    );
    reaped
}

/// The pids the registry vouches for, so the sweep never ends a tracked worker.
///
/// `None` on an unreadable registry. Degrading that to an empty list would say
/// "no worker is live", which is the strongest possible licence to kill and the
/// exact opposite of what a failed read establishes.
pub fn registry_live_pids(home: &AgentsHome) -> Option<Vec<u32>> {
    crate::state::load_registry(&home.registry_json())
        .ok()
        .map(|registry| {
            registry
                .entries
                .iter()
                .flat_map(|e| [e.pid, e.keeper_child_pid])
                .flatten()
                .collect()
        })
}

const STATE_REAP_FIELDS: [&str; 5] = ["deleted", "would_delete", "kept", "bytes", "oldest_age_s"];

fn state_reap_family_tuple(family: &gc_sweep::StateReapFamilySummary) -> serde_json::Value {
    serde_json::json!([
        family.deleted,
        family.would_delete,
        family.kept.len(),
        family.bytes,
        family.oldest_age_s,
    ])
}

fn state_reap_event_payload(summary: &gc_sweep::StateFilesReapSummary) -> serde_json::Value {
    serde_json::json!({
        "fields": STATE_REAP_FIELDS,
        "families": {
            "expired_claims": state_reap_family_tuple(&summary.expired_claims),
            "plan_locks": state_reap_family_tuple(&summary.plan_locks),
            "agent_locks": state_reap_family_tuple(&summary.agent_locks),
            "pr_status_cache": state_reap_family_tuple(&summary.pr_status_cache),
            "claim_tmp": state_reap_family_tuple(&summary.claim_tmp),
        },
        "totals": [
            summary.totals.deleted,
            summary.totals.would_delete,
            summary.totals.kept,
            summary.totals.bytes,
            summary.totals.oldest_age_s,
        ],
        "skip_reason": summary.skip_reason,
    })
}

/// Apply the configured expendable-state retention policy and record one
/// bounded outcome event, including quiet and disabled passes.
pub fn state_file_sweep(
    home: &AgentsHome,
    emitter: &EventEmitter,
    cwd: &std::path::Path,
) -> gc_sweep::StateFilesReapSummary {
    let summary = gc_sweep::reap_state_files_for_cwd(
        home,
        cwd,
        crate::agents_config::state_reap_config(cwd),
        true,
    );
    let _ = emitter.emit("state_reap", &state_reap_event_payload(&summary));
    summary
}

/// The idle tick's two sweeps no registry row accounts for. `gc_sweep` retires
/// ROWS; a child whose parent died is reparented to init and nothing owned it
/// at all, and the latch's record dir is keyed by argv, so a roster whose
/// handle set changes with every worker leaves answers to questions nobody
/// asks any more. One `ps` for the whole machine, so a sweep meant to reduce
/// the process count never fans out.
pub fn unowned_sweeps(home: &AgentsHome, emitter: &EventEmitter, cwd: &std::path::Path) {
    let _ = orphan_sweep(
        emitter,
        crate::agents_config::orphan_reap_after(cwd),
        registry_live_pids(home).as_deref(),
    );
    let _ = crate::single_flight::prune_records(
        None,
        crate::agents_config::single_flight_ttl(cwd),
        crate::agents_config::single_flight_join_budget(cwd),
    );
}

/// The daemon idle tick's retirement-sweep arm, throttled to `interval`
///. Before this guard the sweep was requested every 5s tick against
/// a 900s grace, its settle pass writing the graph before it knew whether
/// there was any work: a continuous graph consumer wearing a cadence label,
/// with the one-in-flight gate ensuring the copies never stacked but never
/// slowed either. Shaped like [`crate::orphan_reap::maybe_sweep`]: elapsed
/// check, one-in-flight swap, stamp, off-loop body. The emitted `retire`
/// tick row carries the same `interval` the guard compared, so the arms
/// readout and the loop read one number.
/// The argv `fno mux workspace prune` is built from: one seam so the daemon
/// (default flags) and the manual reap command (`--include-used-shells`) cannot
/// drift. Default flags already close an orphaned worker's tab; closing a
/// human's spent shells stays opt-in.
pub fn mux_prune_args(dry_run: bool, include_used_shells: bool) -> Vec<&'static str> {
    let mut args = vec!["mux", "workspace", "prune", "--tabs-only"];
    if include_used_shells {
        args.push("--include-used-shells");
    }
    args.push("--json");
    if dry_run {
        args.push("--dry-run");
    }
    args
}

/// (moved from the manual command) Shell out to the existing prune command
/// - one sweep body, reused, not reimplemented. Fail-closed: a spawn
/// failure, a non-zero exit, or an unparsable receipt is `Unread`, never a
/// measured zero.
///
/// The spawn rides the single-flight latch keyed on the prune argv, so a
/// prune never starts while an earlier one still runs: the joiner waits out
/// the leader's answer and reports that same receipt. The flight `ttl` is
/// ZERO - a finished prune is never answered from cache, only from a run
/// this call made or joined. The 2026-10-02 incident (~9,000 recursive
/// `fno-py mux workspace prune` processes, 89 GB resident) was the Python
/// hand-off loop; the `FNO_PY_HANDOFF` marker in the Python front closes
/// that end and this latch closes the overlap end.
///
/// `root` pins the claims root (tests); `None` uses the global one.
///
/// The daemon arm passes the DEFAULT prune flags only: an orphaned worker
/// tab closes on the retire cadence (Locked Decision 6); a human's spent
/// shells stay opt-in via the manual command.
pub fn mux_tab_sweep(
    root: Option<&Path>,
    cwd: &Path,
    dry_run: bool,
    include_used_shells: bool,
) -> crate::reap_render::MuxSweep {
    let args = mux_prune_args(dry_run, include_used_shells);
    // The spawn failure detail (exit code + first stderr line) travels out
    // of the latch closure here: run_or_join hands back only the stdout.
    let failure: std::sync::Arc<std::sync::Mutex<Option<(Option<i32>, String)>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let failure_in_flight = std::sync::Arc::clone(&failure);
    let flight = crate::single_flight::run_or_join_at(
        root,
        &crate::single_flight::flight_key(&args),
        Duration::ZERO,
        crate::agents_config::single_flight_join_budget(cwd),
        move |_spent| {
            let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
            cmd.args(&args);
            match cmd.output() {
                Ok(out) if out.status.code() == Some(0) => Some(out.stdout),
                Ok(out) => {
                    let stderr = String::from_utf8_lossy(&out.stderr);
                    *failure_in_flight.lock().unwrap() = Some((
                        out.status.code(),
                        stderr.lines().next().unwrap_or("").to_string(),
                    ));
                    None
                }
                Err(e) => {
                    *failure_in_flight.lock().unwrap() = Some((None, e.to_string()));
                    None
                }
            }
        },
    );
    match flight.stdout {
        Some(stdout) => {
            let stdout = String::from_utf8_lossy(&stdout);
            match crate::reap_render::parse_prune_receipt(&stdout) {
                Some(receipt) => crate::reap_render::MuxSweep::Ran { receipt },
                None => crate::reap_render::MuxSweep::Unread {
                    exit_code: Some(0),
                    stderr_first: String::new(),
                },
            }
        }
        None => {
            let held = failure.lock().unwrap().take();
            match held {
                Some((exit_code, stderr_first)) => crate::reap_render::MuxSweep::Unread {
                    exit_code,
                    stderr_first,
                },
                None => crate::reap_render::MuxSweep::Unread {
                    exit_code: None,
                    stderr_first: "joined an in-flight prune whose answer never arrived"
                        .to_string(),
                },
            }
        }
    }
}

/// The production roster sweep the retire arm runs: the real enumeration
/// and removal, `dry_run` false, at the scope the caller resolved.
/// The production dead-team sweep the retire arm runs: apply on. The
/// manual command calls `team_reap::sweep` itself so a dry run can report
/// without applying.
pub fn production_team_sweep(home: &AgentsHome, cwd: &Path) -> crate::team_reap::TeamReap {
    crate::team_reap::production_sweep(home, cwd, true)
}

pub fn production_roster_sweep(
    home: &AgentsHome,
    cwd: &Path,
    grace_secs: i64,
    scope: crate::agents_config::RosterScope,
) -> crate::roster_reap::RosterReapSummary {
    crate::roster_reap::roster_reap(home, cwd, grace_secs, scope, false)
}

/// The state-file sweep's own cadence. Its retention is counted in days
/// (`StateReapConfig`), so the retire pass's minutes buy nothing, and each
/// pass stats every lock, pr-status and state file.
pub const STATE_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// The daemon tick's state-file sweep: throttled to `STATE_SWEEP_INTERVAL`,
/// one in flight, off the loop. Same shape as `maybe_retirement_sweep`.
pub fn maybe_state_sweep(
    last_sweep: &mut Instant,
    in_flight: &Arc<AtomicBool>,
    home: AgentsHome,
    cwd: PathBuf,
    events: PathBuf,
) {
    if last_sweep.elapsed() < STATE_SWEEP_INTERVAL || in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    *last_sweep = Instant::now();
    let flag = Arc::clone(in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let emitter = EventEmitter::new(events, "daemon");
        let _ = state_file_sweep(&home, &emitter, &cwd);
    });
}

pub fn maybe_retirement_sweep(
    last_sweep: &mut Instant,
    in_flight: &Arc<AtomicBool>,
    next_interval: &Arc<RetireIntervalCell>,
    home: AgentsHome,
    grace_cwd: PathBuf,
    events: PathBuf,
    interval: Duration,
    tab_sweep: impl FnOnce() -> crate::reap_render::MuxSweep + Send + 'static,
    roster_sweep: fn(
        &AgentsHome,
        &Path,
        i64,
        crate::agents_config::RosterScope,
    ) -> crate::roster_reap::RosterReapSummary,
    team_sweep: fn(&AgentsHome, &Path) -> crate::team_reap::TeamReap,
) {
    if last_sweep.elapsed() < interval || in_flight.swap(true, Ordering::SeqCst) {
        return;
    }
    *last_sweep = Instant::now();
    let flag = Arc::clone(in_flight);
    let next_interval = Arc::clone(next_interval);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        let emitter = EventEmitter::new(events, "daemon");
        // Revival precedes destruction on the first sweeps after a boot:
        // a reboot empties the roster and ages every transcript past every
        // window, so a sweep that runs before the boot pass finishes would
        // vacate and reap the fleet the revival exists to bring back. The
        // hold is bounded: past the ceiling, or on a host that cannot name
        // its boot, the gate is unevaluable and the sweeps run as before.
        if let Some(reason) = crate::boot_revival::hold_reason(&home) {
            let _ = emitter.emit(
                "retire_held_for_boot_revival",
                &serde_json::json!({"reason": reason}),
            );
            crate::boot_revival::start(&home);
            return;
        }

        let grace_secs = crate::agents_config::retire_grace_secs(&grace_cwd) as i64;
        let retain_days = crate::agents_config::reap_receipt_retain_days(&grace_cwd);
        // The dead-team sweep runs BEFORE the registry sweep: a vacated
        // team frees the territory this tick, so the registry pass reads a
        // world that already answers for it.
        let teams = team_sweep(&home, &grace_cwd);
        let summary = gc_sweep(&home, &emitter, grace_secs, retain_days);
        // Locked Decision 5: the nudge ladder rides the daemon's retire arm
        // only, after the sweep that classified the open-PR rows. A manual
        // command run never nudges; its dry run prints the plan. The ladder's
        // input is the concatenation: open-PR rows first, then the
        // dead-worker rows, in one slice so `cleanup_state_files` sees both.
        let ladder_rows = summary.open_pr_rows.clone();
        crate::pr_nudge::run_ladder(&home, &emitter, &ladder_rows, grace_secs);
        unowned_sweeps(&home, &emitter, &grace_cwd);
        // The roster sweep runs AFTER the registry sweep: a row the registry
        // sweep retires this pass is already gone from the registry the
        // roster sweep loads. A session the roster sweep removes becomes a
        // corpse for the NEXT registry pass, through `origin_corpse`.
        let scope = crate::agents_config::roster_scope(&grace_cwd);
        let roster = roster_sweep(&home, &grace_cwd, grace_secs, scope);
        let scope_off = scope == crate::agents_config::RosterScope::Off;
        let mut builds_reclaimed = 0usize;
        let mut builds_bytes = 0u64;
        let mut builds_unread = false;
        for root in crate::daemon::worktree_sweep::registry_repo_roots(&home) {
            let root = Path::new(&root);
            if crate::cargo_build_dirs::workspace_manifests(root).is_empty() {
                continue;
            }
            let report = crate::cargo_build_dirs::reclaim_idle_trees(root, true, SystemTime::now());
            builds_reclaimed += report.reclaimed;
            builds_bytes += report.reclaimed_bytes;
            builds_unread |= report.unread.is_some();
        }
        let builds_detail = if builds_unread {
            "builds=unread".to_string()
        } else {
            format!("builds=reclaimed {builds_reclaimed} bytes {builds_bytes}")
        };
        // The mux surface is one of the stores a reap must clear: the
        // default-flag prune closes an orphaned worker's tab on the retire
        // cadence, so the operator never runs the manual
        // reap command just to clear tabs.
        let mux = tab_sweep();
        let mux_detail = match &mux {
            crate::reap_render::MuxSweep::Ran { receipt } => {
                format!("mux=ran closed={}", receipt.closed)
            }
            other => format!("mux={}", other.state()),
        };
        let roster_detail = if scope_off {
            "off".to_string()
        } else if roster.instrument_unread {
            "unreadable".to_string()
        } else {
            format!(
                "enumerated {} retired {} kept {} refused {}",
                roster.enumerated,
                roster.retired.len(),
                roster.kept.len(),
                roster.refused.len()
            )
        };
        let teams_detail = if teams.unread.is_some() {
            "teams=unreadable".to_string()
        } else {
            format!(
                "teams=vacated {} kept {}",
                teams.vacated.len(),
                teams.kept.len()
            )
        };
        // The running build rides every tick: a daemon that outlived a
        // binary install keeps sweeping with the old code, and only the
        // tick can show it.
        let detail = format!(
            "roster={roster_detail} {mux_detail} {teams_detail} {builds_detail} held={} build={}",
            summary.holds.len(),
            crate::gc_verify::current_rev()
        );
        // `acted` counts BOTH sweeps' retirements: the registry sweep's and
        // the roster sweep's. A tick that retired only roster sessions reads
        // acted>0, never a held zero.
        let acted = summary.retired.len() + roster.retired.len() + builds_reclaimed;
        // A zero-acted tick says which zero it was: a sweep that could not
        // read its registry, nothing classified, or work judged and held.
        let skip_reason = if acted == 0 {
            Some(if summary.registry_unreadable {
                "registry_unreadable"
            } else if summary.kept_total() == 0 {
                "no_rows"
            } else {
                "held"
            })
        } else {
            None
        };
        // Hand back the NEXT window's interval, resolved off-loop: the tick
        // that reads it never touches config.
        let next = crate::agents_config::retire_interval_s(&grace_cwd, grace_secs.max(0) as u64);
        if let Ok(mut slot) = next_interval.lock() {
            *slot = Some(Duration::from_secs(next));
        }
        let journal = crate::loop_runtime::Journal::new_raw(
            home.events_jsonl(),
            crate::daemon::global_events_path(&home),
        );
        crate::tick_ledger::emit_tick(
            &journal,
            "retire",
            "daemon",
            acted as u64,
            skip_reason,
            Some(&detail),
            interval.as_secs(),
        );
        // AC5: every held row lands in the journal once per tick -
        // one `retire_holds` row beside the tick, so a fleet question reads
        // the event stream instead of parsing bucket counts. Zero holds
        // writes nothing.
        let holds: Vec<&gc_sweep::Hold> = summary.holds.iter().chain(roster.holds.iter()).collect();
        if !holds.is_empty() {
            let _ = journal.append(
                "retire_holds",
                serde_json::json!({
                    "scheduler": "daemon",
                    "holds": holds
                        .iter()
                        .map(|h| {
                            serde_json::json!({
                                "id": h.id, "reason": h.reason, "detail": h.detail,
                                "age_s": h.age_s, "escalated": h.escalated,
                            })
                        })
                        .collect::<Vec<_>>(),
                }),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    fn no_agents() -> crate::claude_roster::ClaudeAgentsSnapshot {
        crate::claude_roster::ClaudeAgentsSnapshot::unknown("test: no snapshot staged")
    }

    /// The roster-seam stub every retire-arm test passes: the arm wiring is
    /// under test, never the sweep body, and no unit test shells out to the
    /// live claude roster.
    fn noop_roster_sweep(
        _home: &AgentsHome,
        _cwd: &Path,
        _grace_secs: i64,
        _scope: crate::agents_config::RosterScope,
    ) -> crate::roster_reap::RosterReapSummary {
        crate::roster_reap::RosterReapSummary::default()
    }

    /// Same stub for the team seam: the arm wiring is under test, never
    /// the dead-team sweep body.
    fn noop_team_sweep(_home: &AgentsHome, _cwd: &Path) -> crate::team_reap::TeamReap {
        crate::team_reap::TeamReap::default()
    }

    use super::*;

    // --- the retirement sweep arm ---

    fn retirement_sweep_tmp_home(tag: &str) -> (std::path::PathBuf, AgentsHome) {
        let dir = std::env::temp_dir().join(format!(
            "fno-retire-arm-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&dir);
        home.ensure_root().unwrap();
        crate::paths::pin_test_claims_root(&dir.join("claims-root"));
        // The retire arm reads the boot-revival receipt before it may sweep;
        // a fixture with no receipt holds every pass. Stamp one finished so
        // these tests exercise the arm itself.
        crate::boot_revival::write_receipt(
            &home.root().join("boot-revival.json"),
            crate::boot_revival::boot_time().unwrap_or(0),
            &[],
            true,
        );
        (dir, home)
    }

    fn count_retire_rows(path: &std::path::Path) -> usize {
        // Committed rows, not journal bytes: the store cutover stopped journal
        // appends, so emitted ticks live only in the store beside the journal.
        let _ = crate::event_store::import_all(path);
        crate::event_store::query_events(
            path,
            &crate::event_store::EventQuery {
                types: vec!["control_plane_tick".to_string()],
                ..Default::default()
            },
        )
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            serde_json::from_str::<serde_json::Value>(&r.line)
                .ok()
                .and_then(|row| {
                    row.get("data")
                        .and_then(|d| d.get("arm"))
                        .and_then(serde_json::Value::as_str)
                        .map(|arm| arm == "retire")
                })
                .unwrap_or(false)
        })
        .count()
    }

    fn wait_for_retire_row(path: &std::path::Path) -> usize {
        wait_for_retire_row_within(path, 5)
    }

    /// Polls the committed journal for one event kind, a 10s bound.
    fn wait_for_event(path: &std::path::Path, kind: &str) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let found = crate::events::committed_journal_text(path)
                .lines()
                .any(|l| l.contains(&format!("\"type\":\"{kind}\"")));
            if found {
                return true;
            }
            assert!(
                Instant::now() < deadline,
                "{kind} never landed in {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The longer bound for registries whose rows the age seam must probe
    /// through a real subprocess: an adopted claude row in a sandbox has no
    /// transcript store, so the probe runs out its whole timeout before the
    /// sweep classifies and the tick lands.
    fn wait_for_retire_row_within(path: &std::path::Path, secs: u64) -> usize {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let n = count_retire_rows(path);
            if n >= 1 {
                return n;
            }
            assert!(
                Instant::now() < deadline,
                "retire tick row never landed in {}",
                path.display()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn retirement_sweep_guard_admits_one_run_per_window() {
        let (dir, home) = retirement_sweep_tmp_home("one-run");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let in_flight = Arc::new(AtomicBool::new(false));
            let cell: Arc<RetireIntervalCell> = Arc::new(Mutex::new(None));
            // Backdate the stamp: a fresh stamp means the guard correctly
            // waits out the window, so the test starts due like the daemon
            // is one interval after boot.
            let mut last = Instant::now() - Duration::from_secs(301);
            let grace_cwd = dir.clone();
            // The interval is passed straight through (no resolution), so
            // this test is independent of machine config.
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                grace_cwd.clone(),
                home.events_jsonl(),
                Duration::from_secs(300),
                || crate::reap_render::MuxSweep::Skipped,
                noop_roster_sweep,
                noop_team_sweep,
            );
            // A second tick inside the window is refused by the elapsed
            // check: no second run can start until the window closes.
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                grace_cwd,
                home.events_jsonl(),
                Duration::from_secs(300),
                || crate::reap_render::MuxSweep::Skipped,
                noop_roster_sweep,
                noop_team_sweep,
            );
            let rows = wait_for_retire_row(&home.events_jsonl());
            assert_eq!(rows, 1, "two ticks in one window must yield one sweep");
            // Give a still-running first sweep a moment, then confirm no
            // second row appeared behind it.
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert_eq!(
                count_retire_rows(&home.events_jsonl()),
                1,
                "no second run may land inside one guard window"
            );
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The gate: while this boot's revival pass has not finished, the retire
    /// arm emits the hold event and runs nothing; once the receipt reads
    /// done, the next pass runs its sweeps.
    #[test]
    fn the_retire_arm_holds_its_sweeps_until_this_boots_revival_finished() {
        let (dir, home) = retirement_sweep_tmp_home("revival-gate");
        // This test owns the gate's own legs: clear the helper's finished
        // receipt so the first pass reads the no-receipt hold.
        std::fs::remove_file(home.root().join("boot-revival.json")).ok();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let in_flight = Arc::new(AtomicBool::new(false));
            let cell: Arc<RetireIntervalCell> = Arc::new(Mutex::new(None));
            let mut last = Instant::now() - Duration::from_secs(301);
            // No receipt: the pass holds, names the reason, runs no sweep.
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                dir.clone(),
                home.events_jsonl(),
                Duration::from_secs(300),
                || crate::reap_render::MuxSweep::Skipped,
                noop_roster_sweep,
                noop_team_sweep,
            );
            let held = wait_for_event(&home.events_jsonl(), "retire_held_for_boot_revival");
            assert!(held, "the held pass must name itself");
            assert_eq!(
                count_retire_rows(&home.events_jsonl()),
                0,
                "a held pass lands no retire tick"
            );
            // The pass done: the next tick runs the sweeps.
            crate::boot_revival::write_receipt(
                &home.root().join("boot-revival.json"),
                crate::boot_revival::boot_time().unwrap(),
                &[],
                true,
            );
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                dir.clone(),
                home.events_jsonl(),
                Duration::from_secs(300),
                || crate::reap_render::MuxSweep::Skipped,
                noop_roster_sweep,
                noop_team_sweep,
            );
            wait_for_retire_row(&home.events_jsonl());
            assert_eq!(count_retire_rows(&home.events_jsonl()), 1);
        });
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// One retirement pass against a temp home, through the real spawn
    /// path, returning the tick row it landed. The tab and roster sweeps
    /// arrive as seams, so no test reaches the shared mux or the live
    /// claude roster.
    fn run_retire_pass_and_read_tick(
        dir: &std::path::Path,
        home: &AgentsHome,
        tab_sweep: impl FnOnce() -> crate::reap_render::MuxSweep + Send + 'static,
        roster_sweep: fn(
            &AgentsHome,
            &std::path::Path,
            i64,
            crate::agents_config::RosterScope,
        ) -> crate::roster_reap::RosterReapSummary,
    ) -> serde_json::Value {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let in_flight = Arc::new(AtomicBool::new(false));
            let cell: Arc<RetireIntervalCell> = Arc::new(Mutex::new(None));
            let mut last = Instant::now() - Duration::from_secs(301);
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                dir.to_path_buf(),
                home.events_jsonl(),
                Duration::from_secs(300),
                tab_sweep,
                roster_sweep,
                noop_team_sweep,
            );
            // The production seams probe staged rows through real
            // subprocesses; in a sandbox without a transcript store those
            // probes run out their whole timeout before the tick lands.
            wait_for_retire_row_within(&home.events_jsonl(), 30);
            crate::events::committed_journal_text(&home.events_jsonl())
                .lines()
                .filter(|l| {
                    l.contains("\"type\":\"control_plane_tick\"")
                        && l.contains("\"arm\":\"retire\"")
                })
                .last()
                .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
                .expect("row read back")
        })
    }

    #[test]
    fn retire_tick_names_the_tabs_the_sweep_closed() {
        // AC5-HP: the tab sweep rides the retire pass and the tick's detail
        // carries its receipt; the default argv carries no used-shells flag.
        let (dir, home) = retirement_sweep_tmp_home("mux-ran");
        let row = run_retire_pass_and_read_tick(
            &dir,
            &home,
            || crate::reap_render::MuxSweep::Ran {
                receipt: crate::reap_render::PruneReceipt {
                    closed: 2,
                    would_close: 0,
                    close_named: vec!["target-x-1-a".to_string(), "target-x-1-b".to_string()],
                    sessions_unreachable: Vec::new(),
                    notice: None,
                },
            },
            noop_roster_sweep,
        );
        assert_eq!(row["data"]["acted"], 0);
        assert_eq!(row["data"]["skip_reason"], "no_rows");
        assert!(
            row["data"]["detail"]
                .as_str()
                .unwrap()
                .contains("mux=ran closed=2"),
            "the tick must name the tab sweep's receipt: {:?}",
            row["data"]["detail"]
        );
        assert_eq!(
            mux_prune_args(false, false),
            vec!["mux", "workspace", "prune", "--tabs-only", "--json"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mux_tab_sweep_never_answers_a_finished_prune_from_cache() {
        // The 2026-10-02 overlap guard: the sweep rides the single-flight
        // latch with flight ttl ZERO. A finished prune's record must never
        // answer a later call (that would report a sweep that did not run),
        // so two back-to-back calls with a counting stub spawn twice and
        // both parse. Takes the crate-wide env lock (FNO_BIN mutation).
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let (dir, _home) = retirement_sweep_tmp_home("mux-no-cache");
        let count = dir.join("invocations");
        let receipt = dir.join("receipt.json");
        std::fs::write(
            &receipt,
            r#"{"tabs_closed": 1, "tabs_would_close": 0, "tabs_close_named": ["p1"], "sessions_unreachable": []}"#,
        )
        .unwrap();
        let stub = crate::write_exec_stub(
            &dir,
            "fno-stub.sh",
            &format!(
                "#!/bin/sh\necho x >> {}\ncat {}\n",
                count.display(),
                receipt.display()
            ),
        );
        let prior = std::env::var_os("FNO_BIN");
        std::env::set_var("FNO_BIN", &stub);
        let cwd = dir.clone();
        let first = crate::gc::mux_tab_sweep(None, &cwd, false, false);
        let second = crate::gc::mux_tab_sweep(None, &cwd, false, false);
        match prior {
            Some(v) => std::env::set_var("FNO_BIN", v),
            None => std::env::remove_var("FNO_BIN"),
        }
        let spawns = std::fs::read_to_string(&count).unwrap().lines().count();
        assert_eq!(
            spawns, 2,
            "flight ttl ZERO must never answer a finished prune from cache"
        );
        match (&first, &second) {
            (
                crate::reap_render::MuxSweep::Ran { receipt: r1 },
                crate::reap_render::MuxSweep::Ran { receipt: r2 },
            ) => {
                assert_eq!(r1.closed, 1);
                assert_eq!(r2.closed, 1);
            }
            other => panic!("both sweeps must parse as Ran: {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manual_reap_command_argv_includes_used_shells() {
        // AC5-CLI: the manual command opts in to closing a human's spent
        // shells; the daemon default never does.
        assert_eq!(
            mux_prune_args(false, true),
            vec![
                "mux",
                "workspace",
                "prune",
                "--tabs-only",
                "--include-used-shells",
                "--json"
            ]
        );
        assert_eq!(
            mux_prune_args(true, true),
            vec![
                "mux",
                "workspace",
                "prune",
                "--tabs-only",
                "--include-used-shells",
                "--json",
                "--dry-run"
            ]
        );
    }

    /// AC5-HP: a tick whose sweep held rows writes ONE
    /// `retire_holds` journal row naming each held id, beside the tick row.
    #[test]
    fn a_tick_with_held_rows_writes_one_retire_holds_event() {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let dir = tempfile::tempdir().unwrap();
        // The agents home sits UNDER dir so the production graph read
        // (home.root().parent()/graph.json) answers dir/graph.json.
        let home = AgentsHome::at(dir.path().join("agents"));
        home.ensure_root().unwrap();
        crate::paths::pin_test_claims_root(dir.path());
        // The retire arm reads the boot-revival receipt before it may sweep;
        // stamp one finished so this tick exercises the held-rows event.
        crate::boot_revival::write_receipt(
            &home.root().join("boot-revival.json"),
            crate::boot_revival::boot_time().unwrap_or(0),
            &[],
            true,
        );
        crate::graph_store::seed_rows(
            &dir.path().join("graph.json"),
            &[serde_json::json!({
                "id": "x-h1",
                "status": "idea",
                "project": "p",
                "sessions": [{
                    "phase": "blueprint",
                    "harness": "codex",
                    "session_id": "s-h1",
                    "started_at": "2026-09-01T00:00:00Z",
                }],
            })],
        )
        .unwrap();
        crate::state::update_registry(&home.registry_json(), |r| {
            let mut e = crate::state::RegistryEntry::default();
            e.name = "bp-x-h1-a".into();
            e.short_id = "bp-x-h1-a".into();
            e.origin = Some("spawn".into());
            e.harness = Some("codex".into());
            e.harness_session_id = Some("s-h1".into());
            // The age probe reads no transcript for this fixture, and the
            // file on disk was just written, so the planner holds as probe
            // unread.
            let transcript = dir.path().join("s-h1.jsonl");
            std::fs::write(&transcript, "{}\n").unwrap();
            e.transcript_path = Some(transcript.to_string_lossy().into_owned());
            e.created_at = (chrono::Utc::now() - chrono::Duration::seconds(1800))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            e.status = crate::AgentStatus::Exited;
            r.entries.push(e);
        })
        .unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let in_flight = Arc::new(AtomicBool::new(false));
            let cell: Arc<RetireIntervalCell> = Arc::new(Mutex::new(None));
            let mut last = Instant::now() - Duration::from_secs(301);
            crate::gc::maybe_retirement_sweep(
                &mut last,
                &in_flight,
                &cell,
                home.clone(),
                dir.path().to_path_buf(),
                home.events_jsonl(),
                Duration::from_secs(300),
                || crate::reap_render::MuxSweep::Skipped,
                noop_roster_sweep,
                noop_team_sweep,
            );
            // The production age probe pays a real subprocess on this
            // fixture (two probes, seconds apiece under load), so the tick
            // can land long past the 5 s deadline the empty-home tests use.
            wait_for_line(&home.events_jsonl(), "\"arm\":\"retire\"", 90);
            wait_for_line(&home.events_jsonl(), "\"type\":\"retire_holds\"", 90);
        });
        let holds_row = crate::events::committed_journal_text(&home.events_jsonl())
            .lines()
            .find(|l| l.contains("\"type\":\"retire_holds\""))
            .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
            .expect("one retire_holds row for the held planner");
        let holds = holds_row["data"]["holds"].as_array().expect("holds list");
        assert_eq!(holds.len(), 1, "{holds_row}");
        assert_eq!(holds[0]["id"], "bp-x-h1-a");
        assert_eq!(holds[0]["reason"], "probe unread");
        assert_eq!(holds_row["data"]["scheduler"], "daemon");
        let _ = std::fs::remove_dir_all(dir.path());
    }

    /// Poll for a journal line until `secs` elapse (the retire_holds row is
    /// appended right after the tick row; the append is a plain write).
    fn wait_for_line(path: &std::path::Path, needle: &str, secs: u64) {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if crate::events::committed_journal_text(path).contains(needle) {
                return;
            }
            if Instant::now() >= deadline {
                panic!("line never landed in {path:?}: {needle}");
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn state_reap_event_reports_counts() {
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        let prior_claims = std::env::var_os("FNO_CLAIMS_ROOT");
        let prior_home = std::env::var_os("HOME");
        let prior_pr_cache = std::env::var_os("FNO_PR_STATUS_CACHE_DIR");
        std::env::set_var("FNO_CLAIMS_ROOT", root.path());
        std::env::set_var("HOME", root.path());
        std::env::set_var(
            "FNO_PR_STATUS_CACHE_DIR",
            root.path().join(".fno/cache/pr-status"),
        );
        let home = AgentsHome::at(root.path().join("agents"));
        home.ensure_root().unwrap();
        let cwd = root.path().join("repo");
        std::fs::create_dir_all(cwd.join(".fno")).unwrap();
        std::fs::write(
            cwd.join(".fno/config.toml"),
            "[agents.state_reap]\n\
             enabled = true\n\
             locks_retain_days = 1\n\
             expired_claims_retain_days = 1\n\
             pr_status_cache_retain_days = 1\n",
        )
        .unwrap();
        let claim = root.path().join(".fno/claims/.expired/old-claim");
        std::fs::create_dir_all(claim.parent().unwrap()).unwrap();
        std::fs::write(&claim, b"claim").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        std::fs::File::options()
            .write(true)
            .open(&claim)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();

        let emitter = EventEmitter::new(home.events_jsonl(), "test");
        let summary = state_file_sweep(&home, &emitter, &cwd);

        assert_eq!(summary.expired_claims.deleted, 1);
        assert!(!claim.exists());
        let quiet = state_file_sweep(&home, &emitter, &cwd);
        assert_eq!(quiet.totals.scanned, 0);
        let lines: Vec<serde_json::Value> =
            crate::events::committed_journal_text(&home.events_jsonl())
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        assert_eq!(lines.len(), 2, "each periodic pass must emit one event");
        let event = &lines[0];
        assert_eq!(event["type"], "state_reap");
        assert_ne!(event["type"], "event_payload_too_large");
        assert_eq!(event["source"], "test");
        let data = &event["data"];
        assert_eq!(
            data["fields"],
            serde_json::json!(["deleted", "would_delete", "kept", "bytes", "oldest_age_s"])
        );
        for family in [
            "expired_claims",
            "plan_locks",
            "agent_locks",
            "pr_status_cache",
        ] {
            assert_eq!(
                data["families"][family].as_array().map(Vec::len),
                Some(5),
                "missing compact {family} tuple: {data}"
            );
        }
        assert_eq!(data["families"]["expired_claims"][0], 1);
        assert_eq!(data["families"]["expired_claims"][1], 0);
        assert_eq!(data["families"]["expired_claims"][2], 0);
        assert_eq!(data["families"]["expired_claims"][3], 5);
        assert!(data["families"]["expired_claims"][4].is_number());
        assert_eq!(data["totals"][0], 1);
        assert_eq!(data["totals"][1], 0);
        assert_eq!(data["totals"][2], 0);
        assert_eq!(data["totals"][3], 5);
        assert!(data["totals"][4].is_number());
        assert!(data["skip_reason"].is_null());
        assert_eq!(lines[1]["type"], "state_reap");
        assert_eq!(
            lines[1]["data"]["totals"],
            serde_json::json!([0, 0, 0, 0, null])
        );
        assert!(lines[1]["data"]["skip_reason"].is_null());

        let mut live = gc_sweep::StateFilesReapSummary::default();
        live.expired_claims.deleted = 6_594;
        live.expired_claims.would_delete = 2_393;
        live.expired_claims.kept = vec![
            gc_sweep::StateReapKept {
                path: String::new(),
                reason: String::new(),
            };
            27
        ];
        live.expired_claims.bytes = 9_880_000;
        live.expired_claims.oldest_age_s = Some(8_631_360);
        live.plan_locks.deleted = 2_393;
        live.agent_locks.deleted = 4_344;
        live.pr_status_cache.deleted = 123;
        live.totals.deleted = 13_454;
        live.totals.would_delete = 2_393;
        live.totals.kept = 27;
        live.totals.bytes = 9_880_000;
        live.totals.oldest_age_s = Some(8_631_360);
        let payload = state_reap_event_payload(&live);
        let payload_len = serde_json::to_vec(&payload).unwrap().len();
        assert!(
            payload_len <= crate::events_limits::max_data_bytes(),
            "live-sized state_reap payload is {payload_len}B: {payload}"
        );
        match prior_claims {
            Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
            None => std::env::remove_var("FNO_CLAIMS_ROOT"),
        }
        match prior_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match prior_pr_cache {
            Some(value) => std::env::set_var("FNO_PR_STATUS_CACHE_DIR", value),
            None => std::env::remove_var("FNO_PR_STATUS_CACHE_DIR"),
        }
    }

    // --- the orphan process sweep ---

    fn orphan(args: &str, ppid: u32, age_secs: u64) -> ProcRow {
        ProcRow {
            pid: 4242,
            ppid,
            age_secs,
            args: args.to_string(),
        }
    }

    const FNO_PY: &str = "/Users/x/.local/share/uv/tools/fno/bin/python3 \
/Users/x/.local/share/uv/tools/fno/bin/fno-py agents stale-escalate --json";

    /// AC11: every clause satisfied is the only shape that reaps.
    #[test]
    fn an_inherited_past_threshold_fno_child_is_reapable() {
        let row = orphan(FNO_PY, 1, 7200);
        assert!(is_reapable_orphan(&row, Duration::from_secs(5400), &[]));
    }

    /// The argv clause matches a TOKEN, not a substring, in both reparable
    /// families: the fno-py entrypoint by basename, and stdio MCP servers by
    /// the `serve --mcp` pair or the codegraph `--liftoff-only` launcher flag
    /// paired with a codegraph path token. A flag that merely contains a
    /// marker string is not the family, and killing on it would end
    /// somebody's foreground command.
    #[test]
    fn the_argv_clause_matches_its_families_by_token() {
        assert!(!is_reapable_orphan(
            &orphan("/usr/bin/grep --include=fno-py-notes .", 1, 86_400),
            Duration::from_secs(5400),
            &[]
        ));
        assert!(!is_reapable_orphan(
            &orphan("/usr/bin/vim notes.txt", 1, 86_400),
            Duration::from_secs(5400),
            &[]
        ));
        assert!(is_reapable_orphan(
            &orphan("fno-py backlog advance --json", 1, 86_400),
            Duration::from_secs(5400),
            &[]
        ));
        // An orphaned stdio MCP server (the 2026-10-02 pile-up left 24 of
        // these on init at 5.6 GB) reads as reapable under the same clauses.
        assert!(is_reapable_orphan(
            &orphan("node /x/npm-shim.js serve --mcp", 1, 86_400),
            Duration::from_secs(5400),
            &[]
        ));
        assert!(is_reapable_orphan(
            &orphan(
                "node --liftoff-only /x/lib/dist/bin/codegraph.js init -y",
                1,
                86_400
            ),
            Duration::from_secs(5400),
            &[]
        ));
        // Token-exact: `--mcp-config` and a bare `--mcp` without the `serve`
        // pair name nothing this gate may end, and `--liftoff-only` without a
        // codegraph path is generic V8, somebody else's daemonized node.
        assert!(!names_stdio_mcp_server("claude --mcp-config '{}' -p ok"));
        assert!(!names_stdio_mcp_server("node server.js --mcp"));
        assert!(!names_stdio_mcp_server(
            "node --liftoff-only /x/wasm-test/run.js"
        ));
        // An MCP child of a live spare is MEASURED, never reaped: it may be
        // the server the spare's next adopted session uses.
        let table = vec![
            ProcRow {
                pid: 100,
                ppid: 1,
                age_secs: 86_400,
                args: "claude bg-spare --bg-spare /tmp/cc-daemon-501/ab/spare/s.claim.sock".into(),
            },
            orphan("node /x/npm-shim.js serve --mcp", 100, 3_600),
        ];
        assert!(!is_reapable_orphan(
            &table[1],
            Duration::from_secs(5400),
            &[]
        ));
        assert_eq!(count_mcp_children_of_spares(&table), 1);
    }

    /// An unreadable registry is not evidence that no worker is live. Reading
    /// it that way would turn one bad file into a fleet-wide kill.
    #[test]
    fn an_unreadable_registry_reaps_nothing_and_says_so() {
        let dir = std::env::temp_dir().join(format!("fno-orphan-skip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.jsonl");
        let _ = std::fs::remove_file(&path);
        let emitter = EventEmitter::new(path.clone(), "daemon");
        // A threshold of zero would reap every orphan on the machine; None must
        // hold it back anyway.
        let reaped = orphan_sweep(&emitter, Duration::from_secs(0), None);
        assert_eq!(reaped, 0);
        // orphan_reap_sweep is ephemeral-class: the store keeps the row in the
        // same journal with retention_class ephemeral; the sibling is never
        // created.
        let rows =
            crate::event_store::query_events(&path, &crate::event_store::EventQuery::default())
                .unwrap();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(rows[0].retention_class, "ephemeral");
        assert!(
            rows[0].line.contains("\"skipped\":true"),
            "{}",
            rows[0].line
        );
        assert!(
            rows[0].line.contains("\"candidates\":0"),
            "{}",
            rows[0].line
        );
        assert!(!PathBuf::from(format!(
            "{}{}",
            path.display(),
            crate::event_store::EPHEMERAL_SUFFIX
        ))
        .exists());
    }

    /// A pid that changed identity between the table read and the signal is
    /// somebody else's process now.
    #[test]
    fn a_pid_that_changed_identity_is_not_signalled() {
        // This process is alive and is not what the row claims, so the
        // re-verification must refuse it.
        let row = ProcRow {
            pid: std::process::id(),
            ppid: 1,
            age_secs: 86_400,
            args: "/tools/fno/bin/fno-py agents truth --handles a".to_string(),
        };
        assert!(!still_the_same(&row));
        assert!(!terminate(&row));
    }

    const GRACE: i64 = 900;

    /// A spawn-origin skill session whose work is done and whose transcript
    /// has been quiet past the one idle hour: the AC3-HP base case.
    fn retiring() -> GcRow {
        GcRow {
            origin: Some("spawn".into()),
            teamed: false,
            work: WorkState::AllDone {
                nodes: vec!["N1".into()],
            },
            transcript_age_s: Some(SPAWN_IDLE_RETIRE_SECS + 1),
            report_age_s: None,
            owns_worktree: true,
            worktree_clean: Some(true),
            branch_merged: Some(true),
            planner: false,
            fno_skill: Some(true),
            tail_done: false,
            live_claim: None,
            open_pr: None,
            peer_drives_pr: false,
            eval_finished: false,
        }
    }

    /// A session the user started by hand that ran `/fno:target`.
    fn hand_started() -> GcRow {
        GcRow {
            origin: None,
            work: WorkState::Open {
                node: "N1".into(),
                status: "in_progress".into(),
            },
            ..retiring()
        }
    }

    #[test]
    fn removal_is_the_default_and_each_window_is_its_own() {
        // Open work, an operator origin, no node: none of them keeps a row.
        for row in [
            retiring(),
            GcRow {
                origin: Some("operator".into()),
                transcript_age_s: Some(SKILL_IDLE_RETIRE_SECS + 1),
                ..hand_started()
            },
            GcRow {
                work: WorkState::NoProvenance,
                ..retiring()
            },
        ] {
            assert_eq!(gc_decide(&row, GRACE), (GcAction::Retire, None), "{row:?}");
        }
        // Law d-defa43c7: a hand-started skill session waits one idle day.
        let day = SKILL_IDLE_RETIRE_SECS;
        let inside = GcRow {
            transcript_age_s: Some(day),
            ..hand_started()
        };
        assert_eq!(
            gc_decide(&inside, GRACE),
            (GcAction::Keep, Some(KeepReason::Active { age_s: day }))
        );
        // A promise tail waits only the grace; a planner 20 minutes.
        let promised = GcRow {
            tail_done: true,
            transcript_age_s: Some(GRACE + 1),
            ..hand_started()
        };
        assert_eq!(gc_decide(&promised, GRACE), (GcAction::Retire, None));
        let planner = GcRow {
            planner: true,
            transcript_age_s: Some(PLANNING_IDLE_RETIRE_SECS + 1),
            ..retiring()
        };
        assert_eq!(gc_decide(&planner, GRACE), (GcAction::Retire, None));
        // A planning session the user started by hand keeps its day.
        let hand_planner = GcRow {
            origin: None,
            ..planner
        };
        assert_eq!(gc_decide(&hand_planner, GRACE).0, GcAction::Keep);
        // Law d-b01ffede: a done node caps any origin's window at 30 minutes.
        let hand_done = GcRow {
            origin: None,
            transcript_age_s: Some(DONE_NODE_IDLE_RETIRE_SECS + 1),
            ..retiring()
        };
        assert_eq!(gc_decide(&hand_done, GRACE), (GcAction::Retire, None));
        // A row with no transcript ages on the report clock for a day.
        let failed_start = GcRow {
            fno_skill: None,
            transcript_age_s: None,
            report_age_s: Some(day + 1),
            ..hand_started()
        };
        assert_eq!(gc_decide(&failed_start, GRACE), (GcAction::Retire, None));
        let unclocked = GcRow {
            report_age_s: None,
            ..failed_start
        };
        assert_eq!(
            gc_decide(&unclocked, GRACE),
            (GcAction::Keep, Some(KeepReason::NoClock))
        );
    }

    #[test]
    fn a_row_keeps_only_on_a_positive_fact() {
        let old = GcRow {
            transcript_age_s: Some(30 * SKILL_IDLE_RETIRE_SECS),
            ..retiring()
        };
        let claim = "node:N1 (holder s-1)".to_string();
        let keeps = [
            (
                GcRow {
                    teamed: true,
                    ..old.clone()
                },
                KeepReason::Teamed,
            ),
            (
                GcRow {
                    live_claim: Some(claim.clone()),
                    ..old.clone()
                },
                KeepReason::LiveClaim { detail: claim },
            ),
            (
                GcRow {
                    open_pr: Some(("N1".into(), 1943)),
                    ..old.clone()
                },
                KeepReason::OpenPr {
                    node: "N1".into(),
                    pr: 1943,
                },
            ),
            // Law d-defa43c7: a brainstorm keeps whatever its age.
            (
                GcRow {
                    origin: None,
                    fno_skill: Some(false),
                    ..old.clone()
                },
                KeepReason::Brainstorm,
            ),
        ];
        for (row, reason) in keeps {
            assert_eq!(gc_decide(&row, GRACE), (GcAction::Keep, Some(reason)));
        }
        // A peer that drives the PR lifts K3; a spawn row is never a
        // brainstorm; a finished eval is finished work.
        for row in [
            GcRow {
                open_pr: Some(("N1".into(), 1943)),
                peer_drives_pr: true,
                ..old.clone()
            },
            GcRow {
                fno_skill: Some(false),
                ..old.clone()
            },
            GcRow {
                origin: None,
                fno_skill: Some(false),
                eval_finished: true,
                ..old
            },
        ] {
            assert_eq!(gc_decide(&row, GRACE), (GcAction::Retire, None), "{row:?}");
        }
    }

    #[test]
    fn ac3_hp_work_done_and_quiet_retires_and_prunes() {
        let row = retiring();
        assert_eq!(gc_decide(&row, GRACE), (GcAction::Retire, None));
        assert_eq!(tree_action(&row), TreeAction::Prune);
    }

    #[test]
    fn ac3_edge_claude_row_without_any_session_id_lands_in_stop_refused() {
        // A claude row owns no worker socket, so the old socket probe read
        // "down" instantly and the row dropped while the claude daemon still
        // held the session (the adopt-then-rm recovery, 50 times). Now a
        // claude row the stop cannot REACH - no short id, no session id -
        // answers false and lands in stop_refused: the sweep must not drop a
        // row whose sideline entry it cannot settle.
        let dir = std::env::temp_dir().join(format!(
            "fno-gc-claude-stop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&dir);
        home.ensure_root().unwrap();
        let entry = crate::state::RegistryEntry {
            name: "target-x-ffff-worker".into(),
            cwd: dir.to_string_lossy().to_string(),
            harness: Some("claude".into()),
            ..Default::default()
        };
        assert!(!gc_sweep::stop_row_process(&home, &entry));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dry_run_never_stops_a_retiring_row() {
        // A rehearsal that killed the worker it rehearsed retiring would be
        // the destructive run wearing a dry flag. The stop seam must never
        // fire in dry-run, even for a row that classifies Retire end to end.
        use std::collections::HashMap;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = std::env::temp_dir().join(format!(
            "fno-gc-dry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&dir);
        home.ensure_root().unwrap();
        // A transcript quiet past grace: written, then aged 2000s.
        let transcript = dir.join("rollout.jsonl");
        std::fs::write(&transcript, b"{}\n").unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2000);
        std::fs::File::options()
            .write(true)
            .open(&transcript)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        crate::state::update_registry(&home.registry_json(), |r| {
            let mut e = crate::state::RegistryEntry::default();
            e.name = "dryw".into();
            e.short_id = "dryw".into();
            e.origin = Some("spawn".into());
            e.harness = Some("codex".into());
            e.harness_session_id = Some("S-dry".into());
            r.entries.push(e);
        })
        .unwrap();
        let mut index = HashMap::new();
        index.insert(
            "s-dry".to_string(),
            vec![("N1".to_string(), "done".to_string())],
        );
        let graph = std::cell::RefCell::new(Some(gc_sweep::GraphRead {
            work_index: index.clone(),
            index,
            open_do: HashMap::new(),
            phases: HashMap::new(),
            closed_planning: HashMap::new(),
            plan_written: HashMap::new(),
            statuses: HashMap::from([("N1".to_string(), "done".to_string())]),
            pr_state: HashMap::from([("N1".to_string(), (None, 0, 0))]),
            pr_number: HashMap::new(),
            plan_paths: HashMap::new(),
            delivery_merged_refs: HashMap::new(),
            do_nodes: HashMap::new(),
            pr_reads: HashMap::new(),
        }));
        let emitter = crate::events::EventEmitter::new(std::path::PathBuf::new(), "daemon");
        let stopped = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stopped);
        let summary = gc_sweep::run(
            &home,
            &emitter,
            900,
            true,
            7,
            &|_h| graph.borrow_mut().take(),
            &|_e| Some(vec![transcript.clone()]),
            &|entries| {
                entries
                    .iter()
                    .map(|e| (row_handle(e), Some(2 * 3600)))
                    .collect::<HashMap<_, _>>()
            },
            &move |_e| {
                flag.store(true, Ordering::SeqCst);
                true
            },
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_e| (None, None),
            &|_e| None,
        );
        assert!(
            !stopped.load(Ordering::SeqCst),
            "the stop seam fired in dry-run"
        );
        // the row still classifies would-retire, but a dry run with
        // no positive stop evidence holds it under needs_live_stop instead
        // of promising the retirement.
        assert_eq!(
            summary.needs_live_stop.len(),
            1,
            "the row still classifies would-retire: {summary:?}"
        );
        assert!(
            summary.retired.is_empty() && summary.dry_run_unverified.is_empty(),
            "a dry run promises nothing: {:?}",
            (summary.retired, summary.dry_run_unverified)
        );
        assert!(
            crate::state::load_registry(&home.registry_json())
                .unwrap()
                .entries
                .iter()
                .any(|e| e.name == "dryw"),
            "dry-run mutated the registry"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ac3_err_a_kept_tree_never_blocks_a_retired_row() {
        // The tree buckets, on rows that DID retire: a dirty or unmerged or
        // unprobed tree never blocks the row, it only keeps the tree.
        let dirty = GcRow {
            worktree_clean: Some(false),
            ..retiring()
        };
        assert_eq!(gc_decide(&dirty, GRACE), (GcAction::Retire, None));
        assert_eq!(tree_action(&dirty), TreeAction::KeepDirty);

        let unmerged = GcRow {
            branch_merged: Some(false),
            ..retiring()
        };
        assert_eq!(gc_decide(&unmerged, GRACE), (GcAction::Retire, None));
        assert_eq!(tree_action(&unmerged), TreeAction::KeepUnmerged);

        let unprobed = GcRow {
            worktree_clean: None,
            ..retiring()
        };
        assert_eq!(gc_decide(&unprobed, GRACE), (GcAction::Retire, None));
        assert_eq!(tree_action(&unprobed), TreeAction::KeepUnprobed);

        let detached = GcRow {
            branch_merged: None,
            ..retiring()
        };
        assert_eq!(tree_action(&detached), TreeAction::KeepUnmerged);

        // A row owning nothing removable: no tree question at all.
        let bare = GcRow {
            owns_worktree: false,
            ..retiring()
        };
        assert_eq!(tree_action(&bare), TreeAction::None);
    }

    /// The live registry's shapes on 2026-11-04, run through the whole sweep:
    /// kept equals exactly the rows with a positive keep fact. The two open-PR
    /// rows are the ones the old idle-hour arm retired with their PRs open.
    #[test]
    fn a_registry_snapshot_keeps_exactly_the_rows_with_a_keep_fact() {
        use std::collections::{HashMap, HashSet};
        let dir = std::env::temp_dir().join(format!(
            "fno-gc-snapshot-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = AgentsHome::at(&dir);
        home.ensure_root().unwrap();
        let line = |role: &str, text: &str| {
            serde_json::json!({"type": "response_item", "payload": {"type": "message",
                "role": role, "content": [{"type": "text", "text": text}]}})
            .to_string()
        };
        const DAY: i64 = 86_400;
        // (name, origin, transcript lines or None for no transcript, age,
        // node, pr, expected verdict)
        let rows: Vec<(
            &str,
            Option<&str>,
            Option<Vec<String>>,
            Option<i64>,
            Option<(&str, &str)>,
            Option<u64>,
            &str,
        )> = vec![
            (
                "t-command-center-is",
                Some("spawn"),
                Some(vec![line("user", "$fno:target node-hq")]),
                Some(2 * DAY),
                Some(("node-hq", "in_review")),
                Some(4420),
                "open pr",
            ),
            (
                "t-decisions-backlog-nodes",
                None,
                Some(vec![line("user", "$fno:target node-numbers")]),
                Some(2 * DAY),
                Some(("node-numbers", "in_review")),
                Some(4434),
                "open pr",
            ),
            (
                "brainstorm",
                None,
                Some(vec![line("user", "what should the reaper keep?")]),
                Some(30 * DAY),
                None,
                None,
                "brainstorm",
            ),
            (
                "skill-idle",
                None,
                Some(vec![line("user", "$fno:fix the red test")]),
                Some(2 * DAY),
                Some(("node-fix", "done")),
                None,
                "retire",
            ),
            (
                "skill-recent",
                None,
                Some(vec![line("user", "$fno:review high")]),
                Some(2 * 3600),
                None,
                None,
                "active",
            ),
            (
                "promised",
                None,
                Some(vec![
                    line("user", "$fno:target node-ship"),
                    line("assistant", "<promise>MISSION COMPLETE: shipped</promise>"),
                ]),
                Some(GRACE + 60),
                None,
                None,
                "retire",
            ),
            ("failed-start", None, None, None, None, None, "retire"),
        ];
        let created =
            chrono::DateTime::from_timestamp(crate::daemon::now_epoch_secs() - 3 * DAY, 0)
                .unwrap()
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string();
        let mut graph = gc_sweep::GraphRead::default();
        let mut transcripts: HashMap<String, Vec<std::path::PathBuf>> = HashMap::new();
        let mut ages: HashMap<String, Option<i64>> = HashMap::new();
        crate::state::update_registry(&home.registry_json(), |r| {
            for (name, origin, lines, age, node, pr, _) in &rows {
                let sid = format!("sid-{name}");
                let mut e = crate::state::RegistryEntry::default();
                e.name = name.to_string();
                e.short_id = name.to_string();
                e.origin = origin.map(str::to_string);
                e.harness = Some("codex".into());
                e.harness_session_id = Some(sid.clone());
                e.cwd = dir.to_string_lossy().to_string();
                e.created_at = created.clone();
                let mut paths = Vec::new();
                if let Some(lines) = lines {
                    let path = dir.join(format!("{name}.jsonl"));
                    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
                    paths.push(path);
                }
                transcripts.insert(name.to_string(), paths);
                ages.insert(row_handle(&e), *age);
                if let Some((node, status)) = node {
                    let key = crate::graph_store::work_state_key(&sid);
                    let named = vec![(node.to_string(), status.to_string())];
                    graph.work_index.insert(key.clone(), named.clone());
                    graph.index.insert(key, named);
                    graph.statuses.insert(node.to_string(), status.to_string());
                    graph.pr_state.insert(node.to_string(), (None, 0, 0));
                    graph.pr_number.insert(node.to_string(), *pr);
                    let mut driven = HashSet::new();
                    driven.insert(node.to_string());
                    graph.do_nodes.insert(sid.to_ascii_lowercase(), driven);
                    if let Some(pr) = pr {
                        graph.pr_reads.insert((e.cwd.clone(), *pr), Some(true));
                    }
                }
                r.entries.push(e);
            }
        })
        .unwrap();
        let graph = std::cell::RefCell::new(Some(graph));
        let emitter = crate::events::EventEmitter::new(std::path::PathBuf::new(), "daemon");
        let summary = gc_sweep::run(
            &home,
            &emitter,
            GRACE,
            true,
            7,
            &|_h| graph.borrow().clone(),
            &|e| transcripts.get(&e.name).cloned(),
            &|entries| {
                entries
                    .iter()
                    .map(|e| (row_handle(e), ages.get(&row_handle(e)).copied().flatten()))
                    .collect::<HashMap<_, _>>()
            },
            &|_e| true,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &|_e| crate::daemon::CascadeOutcome::NotApplicable,
            &no_agents,
            &|_e| (None, None),
            &|_e| None,
        );
        let mut verdict: HashMap<String, String> = HashMap::new();
        for (id, _) in summary
            .needs_live_stop
            .iter()
            .chain(&summary.dry_run_unverified)
            .chain(&summary.retired)
        {
            verdict.insert(id.clone(), "retire".into());
        }
        for (id, _) in &summary.kept_open_pr {
            verdict.insert(id.clone(), "open pr".into());
        }
        for (id, _) in &summary.kept_active {
            verdict.insert(id.clone(), "active".into());
        }
        for (id, reason) in &summary.kept_other {
            if reason == KeepReason::Brainstorm.as_str() {
                verdict.insert(id.clone(), "brainstorm".into());
            }
        }
        for (name, .., want) in &rows {
            let e = crate::state::RegistryEntry {
                name: name.to_string(),
                short_id: name.to_string(),
                ..Default::default()
            };
            assert_eq!(
                verdict.get(&row_label(&e)).map(String::as_str),
                Some(*want),
                "{name}: {summary:?}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
