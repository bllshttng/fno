//! Active backlog dispatcher: the mission drain-tick core + circuit breaker.
//!
//! This module is the engine for the always-on backlog drain. Since (K2)
//! the drain is MISSION-SCOPED: the daemon's resident supervisor
//! ([`run_supervisor`]) drives one independent drain loop PER ACTIVE MISSION -
//! an epic with `mission_active=true`, K1's activation record - not per project.
//! The legacy per-project interval drain is deleted (epic Locked Decision 4);
//! merge-triggered `fno backlog advance` is the same-project coverage.
//!
//! ## Mission tick (dispatch + reconcile)
//!
//! One *tick* first RECONCILES any dispatches fired on a prior tick from events
//! (feeding [`map_outcome`] -> the auto-defer breaker), then DISPATCHES by
//! shelling K1's converge core, `fno backlog advance --epic <id> --json`. That
//! core fans out the epic's ready LEAF children across ALL projects, doing its
//! own per-dependent-root `walker:<root>` respect, per-project `max_lanes` cap,
//! and `node:`/`dispatch:` claim dedup - so the mission drain reuses the exact
//! dispatch logic the merge-advance path uses and never forks it. See
//! [`dispatch_mission`] / [`mission_drain_tick`] / [`mission_drain_loop`].
//!
//! ## Fire-and-forget reconcile (preserved)
//!
//! The tick does NOT own the worker child. `advance --epic` self-mints each
//! worker session and re-anchors the `node:<id>` claim to `target-session:<sid>`.
//! A later tick RECONCILES each dispatched node by reading its session id back
//! from the claim holder and polling its termination event
//! (`Journal::find_termination`), then feeding the outcome through
//! [`map_outcome`] - so the auto-defer streak is identical to the supervised
//! path. A worker that dies without a termination event is caught by the crash
//! floor (claim gone past the boot window). See [`reconcile_pending`].
//!
//! ## Circuit-breaker park (recoverable, per mission)
//!
//! When a child fails `failure_limit` consecutive drains the breaker trips and
//! the daemon `fno backlog defer`s the node (graph state), then resets the
//! in-memory streak. Independent branches keep dispatching while one branch is
//! parked. Deferring (not an endlessly-refreshed claim) is what makes the park
//! recoverable: `fno backlog undefer` returns the node with a fresh
//! `failure_limit` attempts. The breaker is per mission loop.
//!
//! ## Mission liveness
//!
//! Each tick re-checks the mission: `advance --epic` reporting `deactivated` or
//! `all_done`, or the epic dropping out of the resolved target set (its
//! `mission_active` cleared), RETIRES the loop - no zombie ticks.
//!
//! ## Events (Journal contract)
//!
//! Every transition emits through [`Journal::append`] (project journal fatal,
//! global mirror best-effort): `active_backlog_dispatched` / `_parked` /
//! `_skip` / `_mission_retired`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::claims::{self, ClaimState};
use crate::events::EventEmitter;
use crate::loop_dispatch::{fno_cmd, retry_etxtbsy};
use crate::loop_runtime::{
    CloseOutcome, Evidence, GlobalJournalPath, Journal, ProjectJournalPath, UnitResult,
};
use crate::loopcheck::TerminationReason;
use crate::run_outcome::classify;
use crate::territory;

/// Cross-tick per-node consecutive-failure counter (the circuit breaker).
///
/// Hermes semantics: increment on a failed drain, reset to zero on a successful
/// close. When the streak reaches `failure_limit` the caller trips: it
/// `fno backlog defer`s the node and then [`reset`](Self::reset)s the streak, so
/// the graph (not an in-memory set) owns the exclusion and `fno backlog undefer`
/// recovers the node with a fresh `failure_limit` attempts. This struct is the
/// pure counting policy; the defer IO is the caller's step.
#[derive(Debug, Default)]
pub struct CircuitBreaker {
    failure_limit: u32,
    failures: HashMap<String, u32>,
}

impl CircuitBreaker {
    /// `failure_limit` is clamped to at least 1 (a zero limit would trip every
    /// node on its first failure, which is never the intent).
    pub fn new(failure_limit: u32) -> Self {
        Self {
            failure_limit: failure_limit.max(1),
            failures: HashMap::new(),
        }
    }

    /// Record a failed drain for `node`. Returns `true` iff this failure trips
    /// the breaker (the streak just reached `failure_limit`).
    pub fn record_failure(&mut self, node: &str) -> bool {
        let n = self.failures.entry(node.to_string()).or_insert(0);
        *n += 1;
        *n >= self.failure_limit
    }

    /// Record a successful close for `node`: clear the streak.
    pub fn record_success(&mut self, node: &str) {
        self.failures.remove(node);
    }

    /// Clear the streak for `node` (called after a trip+defer so a later
    /// `undefer` gives the node a fresh `failure_limit` attempts).
    pub fn reset(&mut self, node: &str) {
        self.failures.remove(node);
    }

    /// The current consecutive-failure count for `node` (0 if none).
    pub fn consecutive_failures(&self, node: &str) -> u32 {
        self.failures.get(node).copied().unwrap_or(0)
    }
}

/// Everything one [`mission_drain_tick`] needs, resolved by the daemon per tick.
///
/// The dispatch logic lives in `advance --epic` (K1's converge core), so the
/// daemon carries only what reconcile + the breaker need: the epic id to
/// converge, the epic's own cwd (roots the journal + node-global `done`/`defer`
/// reads), the `fno` binary, and the failure limit.
#[derive(Debug, Clone)]
pub struct DrainConfig {
    /// The mission's epic project cwd - roots the journal and the node-global
    /// `backlog done`/`defer` reads (a mission fans out across projects at
    /// dispatch time via `advance --epic`, not here).
    pub cwd: PathBuf,
    /// The `fno` binary name/path (FNO_BIN override honored by the caller).
    pub fno_bin: String,
    /// The active mission's epic id - the `advance --epic <mission>` argument.
    pub mission: String,
    /// The mission epic's own project: qualifies the owner carried
    /// on every dispatch this loop makes, so an autonomous row never says
    /// nothing about who answers for it.
    pub project: String,
    /// The territory key: the canonical team scope this loop drains.
    /// Empty on a legacy receipt; the loop then keys by `mission`.
    pub scope: String,
    /// No live team holds this territory; the readout names it leadless while
    /// the drain continues (machinery does not need a lead to dispatch).
    pub leadless: bool,
    /// What one tick converges: epic members shell `advance --epic`, project
    /// members shell `advance --loose --project`. Empty falls back to a single
    /// epic member = `mission` (the legacy single-mission receipt).
    pub members: Vec<DrainMember>,
    /// Cross-tick consecutive-failure limit (the circuit breaker).
    pub failure_limit: u32,
    /// The mission's poll interval, for the control-plane tick row's staleness.
    pub interval_seconds: u64,
    /// The wall clock on one member's `backlog advance` child, in seconds.
    /// The lease the child's own single-flight lock carries is
    /// `flight_gate::FLIGHT_TTL_MS`; a child outliving its lease is the hung
    /// shape the stuck-work read pages about, so the drain kills it here.
    pub advance_timeout_s: u64,
    /// 1-based position and population of this mission in the drain rotation
    /// (epic-id order), `None` when it is the only drainable mission. Readout-only:
    /// the arms-table detail prints `mission=x (1 of 4 draining)` so one row sampled
    /// from a many-mission drain cannot read as the whole population. The base is
    /// labeled because the mux band counts all graph-active missions - a mission
    /// with no workspace path drains nowhere but still renders - and two bare `of
    /// N` suffixes with different N read as a contradiction.
    pub rotation: Option<(usize, usize)>,
}

/// One member a territory tick converges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrainMember {
    /// The epic id (rung 2) or project name (rungs 0/1) to converge.
    pub id: String,
    /// True: shell `advance --epic <id> --continuation`; false: shell
    /// `advance --loose --project <id>`.
    pub epic: bool,
}

/// What one [`mission_drain_tick`]'s reconcile did, for tests. Dispatch itself
/// returns [`MissionDispatch`]; these are the outcomes [`map_outcome`] produces
/// as it feeds the breaker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrainOutcome {
    /// A node was reconciled and closed successfully.
    Dispatched { node: String },
    /// A node tripped the circuit breaker and was deferred (parked).
    Parked { node: String, failures: u32 },
    /// No node to reconcile / dispatch this tick.
    NoWork,
    /// The tick could not reconcile a node to a close (a node that failed
    /// without yet tripping the breaker).
    Skipped { reason: String },
}

/// Whether the mission is still live after a dispatch, or should retire its loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionDispatch {
    /// The mission dispatched (or found nothing new); keep ticking.
    Continue,
    /// `advance --epic` reported the mission deactivated / all children done.
    Retire,
}

/// Best-effort `fno backlog defer <node>` for the circuit-breaker park. Graph
/// state, recoverable via `fno backlog undefer`. Node ids are global, so the
/// epic's cwd is a valid working dir for the child-node defer.
/// `retry_etxtbsy` like every other shellout here - this was the one that
/// skipped it. A transient busy binary (a concurrent `fno doctor update`, or under
/// `cargo test` a sibling thread's freshly-written stub) makes the spawn fail,
/// and `let _ =` swallows it, so a tripped breaker silently never defers its
/// node and re-dispatches it into the same crash loop.
/// Returns whether the defer actually landed, so the caller's `parked` journal
/// row states what happened rather than asserting it. Retrying the spawn is only
/// half the fix: exhausted retries, any other spawn error, and a NON-ZERO exit
/// from `fno backlog defer` (unresolvable id, graph lock contention) all still
/// produce the silent re-dispatch loop described above, and `breaker.reset` runs
/// either way.
fn defer_node(fno_bin: &str, cwd: &Path, node: &str, reason: &str) -> bool {
    match retry_etxtbsy(|| {
        fno_cmd(fno_bin)
            .current_dir(cwd)
            .args(["backlog", "defer", node, "--reason", reason])
            .output()
    }) {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            eprintln!(
                "active-backlog: defer of {node} failed (exit {:?}): {}",
                out.status.code(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
            false
        }
        Err(e) => {
            eprintln!("active-backlog: defer of {node} could not run: {e}");
            false
        }
    }
}

/// Does the node carry a PR reference in graph state? Read through `fno backlog
/// get` so node resolution stays the CLI's job. FAIL-OPEN: an unreadable or
/// unparseable answer reports `true`, so a flaky read can never auto-defer a
/// healthy node.
fn node_has_pr_ref(cfg: &DrainConfig, node_id: &str) -> bool {
    // retry_etxtbsy like every other shellout here: a transient busy binary
    // (a concurrent `fno doctor update`) must not read as "healthy, has PR" and quietly
    // disable the guard.
    let Ok(out) = retry_etxtbsy(|| {
        fno_cmd(&cfg.fno_bin)
            .args(["backlog", "get", node_id])
            .current_dir(&cfg.cwd)
            .output()
    }) else {
        return true;
    };
    if !out.status.success() {
        return true;
    }
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return true;
    };
    crate::backlog::done_evidence::has_pr_ref(&v)
}

/// Reconcile passes a ref-less `DonePRGreen` must persist across before it counts
/// as a dead dispatch. `finalize` stamps `pr_number` AFTER loop-check emits the
/// termination event, and its tail (plan stamp, handoff, verifier) has no bounded
/// duration - so a single poll landing in that window would read a healthy ship
/// as ref-less. Re-checking on a later tick costs nothing and never blocks the
/// drain thread, which a sleep here would.
const PR_STAMP_GRACE_TICKS: u32 = 3;

/// Map a dispatched node's termination outcome to a [`DrainOutcome`], updating
/// the breaker and emitting the decision event. Fed by [`reconcile_pending`]
/// with the evidence polled from the worker's own termination event, so the
/// success/park policy is identical to the old supervised path without spawning
/// a real worker.
fn map_outcome(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    journal: &Journal,
    reason: &TerminationReason,
    last_unit: Option<&crate::loop_runtime::UnitResult>,
) -> DrainOutcome {
    let Some(last) = last_unit else {
        // No unit reached close.
        return match reason {
            TerminationReason::NoWork => DrainOutcome::NoWork,
            other => {
                let _ = journal.append(
                    "active_backlog_skip",
                    json!({"reason": "no-close", "mission": cfg.mission, "termination": format!("{other:?}")}),
                );
                DrainOutcome::Skipped {
                    reason: format!("{other:?}"),
                }
            }
        };
    };

    let node = last.unit_id.clone();
    let predicates = classify(last.evidence.reason.clone()).projection();

    // Batch-lane: a member that terminated DoneBatched succeeded - its commits
    // are on the shared batch branch and it ships via the batch PR, so the node
    // closes at merge by `fno backlog reconcile`, not here. For the daemon that
    // is a SUCCESSFUL dispatch, not a failure. Recognize it in the keep-set so a
    // batched member never trips the cross-tick circuit breaker.
    if predicates.batch_member {
        breaker.record_success(&node);
        let _ = journal.append(
            "active_backlog_dispatched",
            json!({"node_id": node, "termination": "DoneBatched", "batched": true}),
        );
        return DrainOutcome::Dispatched { node };
    }

    // DoneAwaitingMerge / DoneAwaitingReview: the node built successfully (PR
    // up, green) but could not auto-complete - DoneAwaitingMerge is blocked by a
    // proven pre-existing main-red; DoneAwaitingReview by a rate-limited required
    // bot that posted a usage-limit comment instead of a review. Both
    // are SUCCESSFUL dispatches for the daemon, not failures - the node is closed
    // at the human merge by `fno backlog reconcile`, exactly like DoneBatched.
    // Keep them out of the cross-tick circuit breaker (mirror the DoneBatched
    // keep-set).
    if predicates.circuit_breaker_success {
        breaker.record_success(&node);
        let _ = journal.append(
            "active_backlog_dispatched",
            json!({"node_id": node, "termination": format!("{:?}", last.evidence.reason), "awaiting_merge": true}),
        );
        return DrainOutcome::Dispatched { node };
    }

    // DoneUnreviewed: green but nothing reviewed. A SUCCESSFUL dispatch
    // for the daemon - the node closes later at a human merge or the reconcile
    // path, exactly like DoneAwaitingMerge. Keep it out of the cross-tick
    // circuit breaker so an unreviewed-but-green terminal does not auto-defer
    // the node as if it had failed.
    if predicates.awaiting_review_notify {
        breaker.record_success(&node);
        let _ = journal.append(
            "active_backlog_dispatched",
            json!({"node_id": node, "termination": "DoneUnreviewed", "awaiting_review": true}),
        );
        return DrainOutcome::Dispatched { node };
    }

    match &last.close {
        CloseOutcome::Closed => {
            breaker.record_success(&node);
            let _ = journal.append(
                "active_backlog_dispatched",
                json!({"node_id": node, "termination": format!("{:?}", last.evidence.reason)}),
            );
            DrainOutcome::Dispatched { node }
        }
        // an exit-5 (PR OPEN, not merged) close arrives here as
        // AwaitingMerge with a DonePRGreen reason (the DoneAwaitingMerge-reason
        // early return above handles the other producer). It is a SUCCESSFUL
        // dispatch - closed later at the human merge by reconcile - so it must
        // never trip the cross-tick circuit breaker (mirror the DoneBatched /
        // DoneAwaitingMerge-reason keep-set). Without this, every healthy
        // ship-green close would count as a failed drain and auto-defer the node.
        CloseOutcome::AwaitingMerge => {
            breaker.record_success(&node);
            let _ = journal.append(
                "active_backlog_dispatched",
                json!({"node_id": node, "awaiting_merge": true, "close": "awaiting-merge"}),
            );
            DrainOutcome::Dispatched { node }
        }
        CloseOutcome::Parked(detail) | CloseOutcome::Refused(detail) => {
            let tripped = breaker.record_failure(&node);
            if tripped {
                // Park by deferring the node in graph state (recoverable via
                // `fno backlog undefer`), then reset the streak so a later
                // undefer gives it a fresh failure_limit attempts. Name what
                // broke beside the count, so a capacity casualty does not read
                // like five broken nodes. The `auto-failure:` sentinel prefix
                // is load-bearing: graph/failure.py AUTO_FAILURE_SENTINEL
                // matches by startswith.
                let reason_str = format!(
                    "auto-failure: {} consecutive failed drains (last: {})",
                    cfg.failure_limit,
                    detail.chars().take(120).collect::<String>()
                );
                // Recorded, not asserted: `breaker.reset` below hands the node a
                // fresh streak allowance either way, so a `parked` row claiming
                // a defer that never landed is what an operator debugging a
                // re-dispatch loop would be misled by.
                let deferred = defer_node(&cfg.fno_bin, &cfg.cwd, &node, &reason_str);
                breaker.reset(&node);
                let _ = journal.append(
                    "active_backlog_parked",
                    json!({"node_id": node, "consecutive_failures": cfg.failure_limit, "detail": detail, "deferred": deferred}),
                );
                DrainOutcome::Parked {
                    node,
                    failures: cfg.failure_limit,
                }
            } else {
                let _ = journal.append(
                    "active_backlog_skip",
                    json!({
                        "reason": "node-not-closed",
                        "mission": cfg.mission,
                        "node_id": node,
                        "close": detail,
                        "consecutive_failures": breaker.consecutive_failures(&node),
                    }),
                );
                DrainOutcome::Skipped {
                    reason: format!("node {node} not closed: {detail}"),
                }
            }
        }
    }
}

// ── fire-and-forget reconcile ───────────────────────────────────────
//
// A tick DISPATCHES the mission's ready children fire-and-forget via K1's
// converge core (`fno backlog advance --epic`, which routes through `fno agents
// spawn`, self-mints each worker session, and re-anchors the `node:<id>` claim
// to `target-session:<sid>`), then RECONCILES prior dispatches from events across
// later ticks - never owning the worker child.
//
// Failure accounting is reconstructed from the worker's own termination event
// (find_termination on the session id read back from the claim holder) fed
// through the `map_outcome` policy, so the auto-defer streak is identical by
// construction. A worker that dies without emitting any termination event is
// caught by the crash floor (claim gone past the boot window), replacing the
// awaited-exit-code `node_failed` watchdog the fire-and-forget model can no
// longer read.

/// A ready node dispatched fire-and-forget in a prior tick, polled to completion
/// from events.
#[derive(Debug, Clone)]
pub struct PendingDispatch {
    node_id: String,
    /// The worker's session id, read back from the `node:<id>` claim holder
    /// (`target-session:<sid>`) once the worker inits and re-anchors the claim.
    /// `None` until first observed; find_termination cannot be polled before it.
    session_id: Option<String>,
    /// Reconcile passes since dispatch. Guards the boot window: a worker that has
    /// not yet taken the node claim holds none, which must not read as a death
    /// until `BOOT_GRACE_TICKS` have elapsed.
    ticks: u32,
    /// Passes this entry has read a ref-less `DonePRGreen`. Lets the PR stamp land
    /// before a zero-artifact verdict sticks (see `PR_STAMP_GRACE_TICKS`).
    stamp_waits: u32,
}

/// Reconcile passes to wait for a dispatched worker to take its `node:<id>`
/// claim before a claim-absent verdict counts as a boot crash.
const BOOT_GRACE_TICKS: u32 = 3;

/// True for the terminal reasons that mark a node done (a successful code or
/// doc delivery). `DoneBatched`/`DoneAwaitingMerge` are NOT here - they close at
/// merge and are recognized as success by `map_outcome`'s keep-set instead.
fn is_done_reason(r: &TerminationReason) -> bool {
    classify(r.clone()).projection().node_closable
}

/// Poll each in-flight dispatch and retire the ones that finished, updating the
/// breaker through `map_outcome` (identical policy to the supervised path).
/// Resolved entries are removed from `pending`.
fn reconcile_pending(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    pending: &mut Vec<PendingDispatch>,
    journal: &Journal,
) {
    pending.retain_mut(|p| {
        p.ticks += 1;
        // `node:<id>` is a GLOBAL-id claim: it routes to $FNO_CLAIMS_ROOT (else
        // $HOME) by prefix, NOT under the project cwd, so the worker (which
        // acquires it via `fno agents claim` with no explicit root) and this read must
        // resolve the SAME dir. Passing Some(cfg.cwd) would look in the wrong
        // place and never find the worker's claim (claim_status root mismatch).
        let (state, rec) = claims::status(&format!("node:{}", p.node_id), None);
        if let Some(sid) = rec
            .as_ref()
            .and_then(|r| r.holder.strip_prefix("target-session:"))
        {
            p.session_id = Some(sid.to_string());
        }
        // Live/Suspect: the worker (or its TTL) still holds the node claim.
        // Suspect is a respawned-supervisor worker, never a death (claims.rs).
        let worker_live = matches!(state, ClaimState::Live | ClaimState::Suspect);

        // A termination event is authoritative whenever we can poll for it,
        // held claim or not (a worker can terminate a tick before release).
        if let Some(sid) = p.session_id.clone() {
            match journal.find_termination(&sid) {
                Ok(Some(ev)) => {
                    // A ref-less DonePRGreen may just be racing finalize's stamp;
                    // keep the entry and re-read on a later tick before deciding.
                    if classify(ev.reason.clone()).projection().merge_armable
                        && !node_has_pr_ref(cfg, &p.node_id)
                        && p.stamp_waits < PR_STAMP_GRACE_TICKS
                    {
                        p.stamp_waits += 1;
                        return true;
                    }
                    resolve_dispatch(cfg, breaker, journal, &p.node_id, ev);
                    return false;
                }
                Ok(None) if !worker_live => {
                    // Claim gone, session known, no event: the worker died
                    // mid-flight without terminating. Crash floor -> failure.
                    resolve_crash(cfg, breaker, journal, &p.node_id);
                    return false;
                }
                _ => {} // still running, or an unreadable journal this pass: keep
            }
        } else if !worker_live && p.ticks >= BOOT_GRACE_TICKS {
            // Never observed the worker take the node claim within the boot
            // window: the dispatch failed to start. Crash floor -> failure.
            resolve_crash(cfg, breaker, journal, &p.node_id);
            return false;
        }
        true
    });
}

/// The close note the drain records when it closes a PR-less node: which
/// terminal closed it and, when the event carries one, its first message
/// line. Capped at 200 chars so a chatty message cannot bloat the graph
/// row's completion_note.
fn drain_close_note(reason: &TerminationReason, message: &str) -> String {
    let mut note = format!("closed by the backlog drain on {reason:?}");
    let first_line = message.lines().next().unwrap_or("").trim();
    if !first_line.is_empty() {
        note.push_str(": ");
        note.push_str(first_line);
    }
    if note.len() > 200 {
        let mut cut = 200;
        while !note.is_char_boundary(cut) {
            cut -= 1;
        }
        note.truncate(cut);
    }
    note
}

/// Apply a polled termination event to the breaker via the shared `map_outcome`
/// policy, mirroring the supervised path's `queue.close` side effects.
fn resolve_dispatch(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    journal: &Journal,
    node_id: &str,
    ev: Evidence,
) {
    // A successful delivery close runs `fno backlog done` (retry_etxtbsy for a
    // transient busy binary) and Closes only on success - a failed `done` Parks
    // with the error, so the breaker counts it as a failure (never a false
    // success). Exit 5 (PR OPEN, not merged) is AwaitingMerge, not a failure:
    // a no-merge dispatch lands its PR open, so `done` exits 5 and the node
    // closes at the human merge via reconcile - map_outcome's keep-set counts
    // it as a successful dispatch. DoneBatched/DoneAwaitingMerge close at merge
    // via reconcile and are NOT marked here - map_outcome recognizes them too.
    // Park a dead dispatch BEFORE `fno backlog done`: its merged-PR cross-check
    // only runs when refs already exist, so a ref-less node would otherwise
    // close exit 0 and score the dead dispatch as a win.
    let merge_armable = classify(ev.reason.clone()).projection().merge_armable;
    // The park guard and the close note ask the same question, so the node
    // is read once per event and the answer is shared. Non-close terminals
    // (a crash, NoProgress) spawn no read at all.
    let close_eligible = merge_armable || is_done_reason(&ev.reason);
    let has_pr_ref = if close_eligible {
        node_has_pr_ref(cfg, node_id)
    } else {
        false
    };
    let close = if merge_armable && !has_pr_ref {
        CloseOutcome::Parked(
            "DonePRGreen terminal with no PR ref on the node (zero-artifact dispatch)".to_string(),
        )
    } else if is_done_reason(&ev.reason) {
        // The store refuses an evidence-less close, so a PR-less node
        // (DoneAdvisory, DoneDelivery) closes with a note carrying the
        // terminal and the event's first line. A node with a PR ref keeps
        // the bare argv: the canonical close and its gates stay as they are.
        let mut args = vec![
            "backlog".to_string(),
            "done".to_string(),
            node_id.to_string(),
        ];
        if !has_pr_ref {
            let note = drain_close_note(&ev.reason, &ev.message);
            args.push("--note".to_string());
            args.push(note);
        }
        match retry_etxtbsy(|| {
            fno_cmd(&cfg.fno_bin)
                .args(&args)
                .current_dir(&cfg.cwd)
                .output()
        }) {
            Ok(o) if o.status.success() => CloseOutcome::Closed,
            Ok(o) if o.status.code() == Some(5) => CloseOutcome::AwaitingMerge,
            // Exit 4 is the OUTAGE code: the gh cross-check could not be read,
            // or the promise gate could not confirm a declared ship count. In
            // both the node is healthy and the read is retryable, so this is
            // success-shaped like exit 5 - reconcile closes it once GitHub
            // answers. Without this arm the catch-all below parks it, and
            // Parked is `breaker.record_failure`: a long enough gh incident
            // auto-defers the very node the promise gate held open for safety.
            Ok(o) if o.status.code() == Some(4) => CloseOutcome::AwaitingMerge,
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                CloseOutcome::Parked(if stderr.is_empty() {
                    format!("fno backlog done {node_id} failed (exit {})", o.status)
                } else {
                    stderr
                })
            }
            Err(e) => CloseOutcome::Parked(format!("fno backlog done {node_id} spawn failed: {e}")),
        }
    } else {
        CloseOutcome::Parked(format!("session terminated: {:?}", ev.reason))
    };
    let reason = ev.reason.clone();
    let ur = UnitResult {
        unit_id: node_id.to_string(),
        evidence: ev,
        close,
    };
    map_outcome(cfg, breaker, journal, &reason, Some(&ur));
}

/// Crash floor: a dispatched worker died with no termination event. Synthesize
/// NoProgress evidence and feed the SAME `map_outcome` path, so the failure
/// counts toward the auto-defer streak exactly as the supervised `node_failed`
/// watchdog did.
fn resolve_crash(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    journal: &Journal,
    node_id: &str,
) {
    let message = "worker exited with no termination event (fire-and-forget crash floor)";
    let ur = UnitResult {
        unit_id: node_id.to_string(),
        evidence: Evidence {
            reason: TerminationReason::NoProgress,
            message: message.to_string(),
        },
        close: CloseOutcome::Parked(message.to_string()),
    };
    map_outcome(
        cfg,
        breaker,
        journal,
        &TerminationReason::NoProgress,
        Some(&ur),
    );
}

/// Did a SYNCHRONOUS (headless) child already reach a terminal state? The
/// one-shot worker ran to completion before its dispatch returned, so graph
/// state is the only evidence left. FAIL-OPEN: an unreadable or unparseable
/// read reports closed, never a fabricated failure (same discipline as
/// `node_has_pr_ref`).
fn sync_child_completed(cfg: &DrainConfig, node_id: &str) -> bool {
    let read = || -> Option<serde_json::Value> {
        let out = retry_etxtbsy(|| {
            fno_cmd(&cfg.fno_bin)
                .args(["backlog", "get", node_id])
                .current_dir(&cfg.cwd)
                .output()
        })
        .ok()?;
        if !out.status.success() {
            return None;
        }
        serde_json::from_slice(&out.stdout).ok()
    };
    let Some(v) = read() else {
        return true;
    };
    let stamped = |key: &str| {
        v.get(key)
            .and_then(|t| t.as_str())
            .is_some_and(|t| !t.trim().is_empty())
    };
    stamped("completed_at")
        || stamped("deferred_at")
        || crate::backlog::done_evidence::has_pr_ref(&v)
}

/// Resolve a synchronous (headless) child on the spot instead of holding it
/// open for crash reconcile. A completed/deferred/PR'd node records a
/// success; a node with none of those goes through `map_outcome` as
/// `CloseOutcome::Parked`, so the streak, the defer and the journal row stay
/// one implementation with the detached path.
fn resolve_sync_child(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    journal: &Journal,
    node_id: &str,
) {
    if sync_child_completed(cfg, node_id) {
        breaker.record_success(node_id);
        let _ = journal.append(
            "active_backlog_sync_resolved",
            json!({"node_id": node_id, "resolution": "success"}),
        );
        return;
    }
    // Journal the sync branch under its own reason BEFORE map_outcome's rows,
    // so the event stream can tell a synchronous resolve from a detached one.
    let _ = journal.append(
        "active_backlog_sync_resolved",
        json!({"node_id": node_id, "resolution": "not-closed"}),
    );
    let message = "synchronous child finished with no completion, defer or PR ref in graph state";
    let ur = UnitResult {
        unit_id: node_id.to_string(),
        evidence: Evidence {
            reason: TerminationReason::NoProgress,
            message: message.to_string(),
        },
        close: CloseOutcome::Parked(message.to_string()),
    };
    map_outcome(
        cfg,
        breaker,
        journal,
        &TerminationReason::NoProgress,
        Some(&ur),
    );
}

/// `#[serde(default)]` covers a MISSING key, never an explicit `null`, and the
/// Python writer emits `"reason": null` on every dispatched child
/// (`AdvanceResult.reason` is Optional; null is the honest value there). Read
/// a null as the field default so the writer's truthful receipt parses.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// One child row from the `advance --epic --json` receipt's `children[]`, the
/// shared vocabulary with the per-child journal events at advance.py:3638.
/// Every field defaulted so an evolving receipt never fails the parse; a
/// null-valued string field reads as its default (`null_as_default`).
#[derive(Debug, Default, Deserialize)]
struct AdvanceChild {
    #[serde(default, deserialize_with = "null_as_default")]
    node_id: String,
    #[serde(default, deserialize_with = "null_as_default")]
    decision: String,
    #[serde(default, deserialize_with = "null_as_default")]
    reason: String,
    /// The failure text behind a `failed` row (the exception string), empty on
    /// skipped rows and on an older CLI's receipt, which serialized only the
    /// generic `reason`. The breaker's defer reason carries it so an operator
    /// reading `auto-failure: N (last: ...)` sees what actually broke.
    #[serde(default, deserialize_with = "null_as_default")]
    detail: String,
    /// Resolved launch substrate. `Some("headless")` is SYNCHRONOUS:
    /// `subprocess.run` returned only after the one-shot worker finished, so
    /// the child must be resolved from graph state now, never held open for
    /// crash reconcile. None on an older CLI's receipt (enqueue as detached,
    /// the old behavior) - and None is what the CLI writes for skipped and
    /// failed rows, so this must stay Option: a plain String rejects the
    /// explicit null and voids the whole receipt parse.
    #[serde(default)]
    substrate: Option<String>,
}

/// The `fno backlog advance --epic <id> --json` receipt, the only fields the
/// mission drain reads. `#[serde(default)]` on every field so a partial or
/// evolving receipt never fails the parse (a missing field defaults benignly).
#[derive(Debug, Default, Deserialize)]
struct AdvanceEpicReceipt {
    #[serde(default)]
    deactivated: bool,
    #[serde(default)]
    all_done: bool,
    /// Every child the pass considered this tick, dispatched or not - the
    /// honest signal `mission_drain_tick` was previously discarding. THE
    /// enqueue authority: `dispatched` (the receipt's human-facing id list)
    /// is substrate-blind and can no longer be trusted to say what is held
    /// open.
    #[serde(default)]
    children: Vec<AdvanceChild>,
    /// A receipt-level gate error (e.g. `"disabled"`, `"walker-live"`) reported
    /// with no children, distinct from a truly exhausted mission.
    #[serde(default)]
    error: Option<String>,
    /// A held receipt: another advance for this scope was still in flight and
    /// the CLI stood this one down instead of stacking a second copy (the
    /// one-in-flight gate). Held is a skip, never a retirement.
    #[serde(default)]
    held: bool,
    /// Held requests the gate has counted for this scope.
    #[serde(default)]
    requests: u64,
}

/// Facts about one `dispatch_mission` pass beyond the fire-and-forget dispatch
/// ids, used only by `mission_drain_tick` to name an honest skip reason when
/// nothing new dispatched. `ready` is how many children the pass considered;
/// `reason` is the shared per-child skip reason when every child in the pass
/// agrees, or `"skipped-mixed"` when they don't; `error` is a receipt-level
/// gate error reported with no children.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct DispatchFacts {
    ready: usize,
    reason: Option<String>,
    error: Option<String>,
    /// The CLI reported the scope held: no children were considered because a
    /// previous advance for the same scope is still running.
    held: bool,
    /// Held requests the gate has counted for this scope; readout only.
    requests: u64,
    /// Children resolved synchronously this pass (dispatched headless rows):
    /// real work the tick did even though nothing entered `pending`.
    sync_resolved: usize,
    /// A dispatch gate (the fleet-incident defense in depth) refused
    /// this pass before the advance shell-out. Names the tick's skip token;
    /// nothing else may read as this gate, so a stopped verdict can never
    /// masquerade as `no_work` and trigger the stranded observer.
    gate: Option<String>,
    /// The gate's human detail: the stop's generation, or the unreadable
    /// record's error.
    gate_detail: Option<String>,
}

fn undispatched_count(cfg: &DrainConfig) -> Result<usize, String> {
    let out = retry_etxtbsy(|| {
        fno_cmd(&cfg.fno_bin)
            .args(["backlog", "undispatched", "--json"])
            .current_dir(&cfg.cwd)
            .output()
    })
    .map_err(|error| error.to_string())?;
    if !out.status.success() {
        return Err(format!("exit {:?}", out.status.code()));
    }
    let receipt: Value = serde_json::from_slice(&out.stdout).map_err(|error| error.to_string())?;
    if receipt.get("status").and_then(Value::as_str) != Some("ok") {
        return Err("observer status was not ok".to_string());
    }
    receipt
        .get("rows")
        .and_then(Value::as_array)
        .filter(|rows| rows.iter().all(Value::is_object))
        .map(Vec::len)
        .ok_or_else(|| "missing valid rows array".to_string())
}

fn facts_from_receipt(receipt: &AdvanceEpicReceipt) -> DispatchFacts {
    let ready = receipt.children.len();
    let reason = if ready == 0 {
        None
    } else {
        let mut reasons = receipt.children.iter().map(|c| c.reason.as_str());
        let first = reasons.next().unwrap_or("");
        if !first.is_empty() && reasons.all(|r| r == first) {
            Some(first.to_string())
        } else {
            Some("skipped-mixed".to_string())
        }
    };
    DispatchFacts {
        ready,
        reason,
        error: receipt.error.clone(),
        held: receipt.held,
        requests: receipt.requests,
        // Set later, by the dispatch loop that actually resolves the rows.
        sync_resolved: 0,
        gate: None,
        gate_detail: None,
    }
}

/// One drain member's `backlog advance` arguments. Epic members converge as
/// `--epic <id> --continuation`; project members drain as
/// `--loose --project <id>` (never retire - a loose territory has no mission
/// lifecycle, and the CLI takes no positional beside those flags).
fn advance_member_args(member: &DrainMember) -> Vec<String> {
    if member.epic {
        vec![
            "--epic".to_string(),
            member.id.clone(),
            "--continuation".to_string(),
        ]
    } else {
        vec![
            "--loose".to_string(),
            "--project".to_string(),
            member.id.clone(),
        ]
    }
}

/// Dispatch ONE territory member by shelling K1's converge core, recording each
/// dispatched child in `pending` for later reconcile. Epic members run
/// `advance --epic <id> --continuation` (Retire on deactivated/all-done);
/// project members run `advance --loose --project <id>` (never retire - a
/// loose territory has no mission lifecycle).
///
/// The converge core owns ALL dispatch policy (cross-project fan-out, per-root
/// `walker:` respect, `max_lanes` cap, claim dedup), so this never forks it. A
/// non-zero exit or unparseable receipt is a transient skip (Continue) - a truly
/// gone mission is caught by the loop's re-resolve, not guessed at here.
// rustfmt::skip keeps the `backlog advance` seam literal multi-line; the
// autonomous-dispatch census greps that exact shape (test_autonomous_dispatch_census).
#[rustfmt::skip]
fn dispatch_member(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    member: &DrainMember,
    pending: &mut Vec<PendingDispatch>,
    journal: &Journal,
) -> (MissionDispatch, DispatchFacts) {
    // the durable fleet incident stop gates BEFORE the advance
    // subprocess, and the file is re-read EVERY tick - a daemon that starts
    // mid-incident takes this branch on its first tick, proving the stop is
    // durable state rather than a missed announcement. Reconciliation and tick
    // reporting continue; only new dispatch is refused. An unreadable state
    // fails closed with its own reason, never as clear. The gate asks the
    // spawns question: dispatch is automatic spawning, so a stop that holds
    // only tests or merges keeps dispatching.
    let incident = crate::fleet_incident::verdict_for("spawns");
    if !matches!(incident, crate::fleet_incident::Verdict::Clear(_)) {
        let (token, generation, detail) = match &incident {
            crate::fleet_incident::Verdict::Stopped(r) => (
                "fleet_stop",
                Some(r.generation),
                format!("generation {}", r.generation),
            ),
            crate::fleet_incident::Verdict::Unavailable(d) => {
                ("fleet_stop_unavailable", None, d.clone())
            }
            crate::fleet_incident::Verdict::Clear(_) => unreachable!(),
        };
        let _ = journal.append(
            "active_backlog_skip",
            json!({"reason": token, "mission": cfg.mission, "generation": generation, "detail": detail}),
       );
        return (
            MissionDispatch::Continue,
            DispatchFacts {
                gate: Some(token.to_string()),
                gate_detail: Some(detail),
                ..Default::default()
            },
        );
    }
    // Epic members converge as `advance --epic <id> --continuation`; loose
    // members drain as `advance --loose --project <project>` (the CLI takes no
    // positional beside those flags). --continuation never reactivates the
    // mission, so an operator `--stop` between drain ticks is not undone.
    let target_args = advance_member_args(member);
    let out = match retry_etxtbsy(|| {
        // A fresh Command per attempt: retry_etxtbsy re-spawns on ETXTBSY,
        // and the bounded call consumes what it is handed.
        let mut cmd = fno_cmd(&cfg.fno_bin);
        cmd.args([
                // The `backlog advance` argv literal at this indentation is the
                // seam marker the autonomous-dispatch census greps. Keep the
                // elements multi-line; `--json` stays last.
                "backlog",
                "advance",
            ])
        .args(target_args.clone())
        .args(["--source", "ab"])
        .arg("--json")
        .current_dir(&cfg.cwd)
        .envs(spawn_provenance_env(cfg, "active-backlog"));
        crate::bounded_cmd::output_with_timeout_result(cmd, cfg.advance_timeout_s)
    }) {
        Ok(o) if o.status.success() => o,
        // A signal death is the wall clock's kill: the child outlived the
        // lease its own lock carries, so the skip names the bound.
        Ok(o) if o.status.code().is_none() => {
            let _ = journal.append(
                "active_backlog_skip",
                json!({"reason": "advance-timeout", "mission": cfg.mission,
                       "detail": format!("killed after {}s wall clock", cfg.advance_timeout_s)}),
            );
            return (MissionDispatch::Continue, DispatchFacts::default());
        }
        Ok(o) => {
            let detail = String::from_utf8_lossy(&o.stderr).trim().to_string();
            let _ = journal.append(
                "active_backlog_skip",
                json!({"reason": "advance-epic-failed", "mission": cfg.mission, "detail": detail}),
            );
            return (MissionDispatch::Continue, DispatchFacts::default());
        }
        Err(e) => {
            let _ = journal.append(
                "active_backlog_skip",
                json!({"reason": "advance-epic-failed", "mission": cfg.mission, "detail": format!("{e}")}),
            );
            return (MissionDispatch::Continue, DispatchFacts::default());
        }
    };
    let receipt: AdvanceEpicReceipt = match serde_json::from_slice(&out.stdout) {
        Ok(r) => r,
        Err(e) => {
            let _ = journal.append(
                "active_backlog_skip",
                json!({"reason": "advance-epic-unparseable", "mission": cfg.mission, "detail": format!("{e}")}),
            );
            // Not no_work: `DispatchFacts::default()` made mission_drain_tick
            // read a refused receipt as an exhausted mission (ready=0 +
            // stranded=N) - exactly the reading that hid real dispatches.
            // The gate:{err} arm names the tick honestly instead.
            return (
                MissionDispatch::Continue,
                DispatchFacts {
                    error: Some("receipt-unparseable".to_string()),
                    ..DispatchFacts::default()
                },
            );
        }
    };
    let mut facts = facts_from_receipt(&receipt);
    if member.epic && (receipt.deactivated || receipt.all_done) {
        return (MissionDispatch::Retire, facts);
    }
    let mut new_ids = Vec::new();
    for child in &receipt.children {
        match child.decision.as_str() {
            "dispatched" => {}
            "failed" => {
                // A spawn failure is a real failure: before a failed
                // child never entered pending, so failure_limit never tripped
                // and the drain retried the node forever. Feed the SAME
                // map_outcome policy with the row's reason; it never enters
                // pending. A skipped row means "not now" and must never touch
                // the breaker (walker-live, lane-cap, already-claimed, ...).
                // Prefer `detail` (the actual error text); `reason` is the
                // generic category ("spawn-failed") and names nothing.
                let cause = if child.detail.is_empty() {
                    child.reason.as_str()
                } else {
                    child.detail.as_str()
                };
                let message = if cause.is_empty() {
                    "advance reported the spawn failed".to_string()
                } else {
                    format!("advance reported the spawn failed: {cause}")
                };
                let ur = UnitResult {
                    unit_id: child.node_id.clone(),
                    evidence: Evidence {
                        reason: TerminationReason::NoProgress,
                        message: message.clone(),
                    },
                    close: CloseOutcome::Parked(message),
                };
                map_outcome(
                    cfg,
                    breaker,
                    journal,
                    &TerminationReason::NoProgress,
                    Some(&ur),
                );
                continue;
            }
            _ => continue,
        }
        // Synchronous (headless) dispatch: subprocess.run returned only after
        // the one-shot worker finished and released its claim, so there is
        // nothing to reconcile later - the crash floor would fabricate a crash
        // on tick 3, every time. Resolve from graph state now.
        if child.substrate.as_deref() == Some("headless") {
            resolve_sync_child(cfg, breaker, journal, &child.node_id);
            facts.sync_resolved += 1;
            continue;
        }
        // Guard against re-recording a still-pending node (a prior tick's
        // dispatch whose worker has not yet closed): advance already dedups by
        // live claim, but a boot-window respawn could echo the id. An older
        // CLI's receipt carries no substrate key and lands here: enqueued as
        // detached, today's behavior.
        if pending.iter().any(|p| p.node_id == child.node_id) {
            continue;
        }
        pending.push(PendingDispatch {
            node_id: child.node_id.clone(),
            session_id: None,
            ticks: 0,
            stamp_waits: 0,
        });
        new_ids.push(child.node_id.clone());
    }
    if !new_ids.is_empty() {
        let _ = journal.append(
            "active_backlog_dispatched",
            json!({"mission": cfg.mission, "dispatched": new_ids, "fire_and_forget": true}),
        );
    }
    (MissionDispatch::Continue, facts)
}

/// Dispatch the territory by converging EVERY member, recording each dispatched
/// child in `pending` for later reconcile. Retires only when the territory has
/// epic members and EVERY one reports deactivated / all children done - a
/// project member never retires its territory (: a loose territory has
/// no mission lifecycle; it drains while the workspace exists).
fn dispatch_mission(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    pending: &mut Vec<PendingDispatch>,
    journal: &Journal,
) -> (MissionDispatch, DispatchFacts) {
    let members: Vec<DrainMember> = if cfg.members.is_empty() {
        // Legacy single-mission receipt: one epic member = the mission itself.
        vec![DrainMember {
            id: cfg.mission.clone(),
            epic: true,
        }]
    } else {
        cfg.members.clone()
    };
    let mut merged = DispatchFacts::default();
    let mut epic_members = 0usize;
    let mut retired = 0usize;
    for member in &members {
        let (outcome, facts) = dispatch_member(cfg, breaker, member, pending, journal);
        merged.ready += facts.ready;
        merged.sync_resolved += facts.sync_resolved;
        if merged.error.is_none() {
            merged.error = facts.error.clone();
        }
        if merged.reason.is_none() {
            merged.reason = facts.reason.clone();
        }
        // The gate is per member but the tick reads it once: any member the
        // incident stopped must survive the fold, or a stopped verdict folds
        // back into `no_work`.
        if merged.gate.is_none() {
            merged.gate = facts.gate.clone();
            merged.gate_detail = facts.gate_detail.clone();
        }
        if member.epic {
            epic_members += 1;
            if outcome == MissionDispatch::Retire {
                retired += 1;
            }
        }
    }
    let outcome = if epic_members > 0 && retired == epic_members {
        MissionDispatch::Retire
    } else {
        MissionDispatch::Continue
    };
    (outcome, merged)
}

/// One mission drain tick: reconcile prior dispatches (feeding the breaker), then
/// dispatch the mission's currently-ready children. Reconcile runs FIRST so a
/// child that just auto-deferred is excluded from this tick's `advance --epic`
/// selection. Synchronous (the loop offloads it to a blocking task).
///
/// Every tick also appends one `control_plane_tick` row (arm `active_backlog`),
/// so the arms readout can tell a draining mission from a dead supervisor.
pub fn mission_drain_tick(
    cfg: &DrainConfig,
    breaker: &mut CircuitBreaker,
    pending: &mut Vec<PendingDispatch>,
    journal: &Journal,
) -> MissionDispatch {
    let pending_before = pending.len();
    reconcile_pending(cfg, breaker, pending, journal);
    let closed = pending_before.saturating_sub(pending.len()) as u64;
    let pre_dispatch = pending.len();
    let (outcome, facts) = dispatch_mission(cfg, breaker, pending, journal);
    let newly_dispatched = (pending.len() - pre_dispatch) as u64;
    let sync_closed = facts.sync_resolved as u64;
    let skip_reason: Option<String> = match outcome {
        MissionDispatch::Retire => Some("mission_retired".to_string()),
        // The dispatch gate refused before any child: name the gate, never
        // `no_work` - a stopped verdict must not read as an exhausted
        // mission, and `no_work` is what triggers the stranded observer
        //.
        MissionDispatch::Continue if facts.gate.is_some() => facts.gate.clone(),
        MissionDispatch::Continue if closed + newly_dispatched + sync_closed > 0 => None,
        // The gate held this tick's converge: name HELD, never no_work - an
        // empty child set from a held receipt is the amplifier reporting
        // itself, not an exhausted mission.
        MissionDispatch::Continue if facts.held => Some("held".to_string()),
        // Something dispatched by an earlier tick is still running: a full
        // spawn lane on THIS pass does not make that stale.
        MissionDispatch::Continue if !pending.is_empty() => Some("in_flight".to_string()),
        MissionDispatch::Continue => {
            if let Some(err) = &facts.error {
                // The `disabled` / `walker-live` early returns stop
                // masquerading as a genuinely exhausted mission.
                Some(format!("gate:{err}"))
            } else if facts.ready == 0 {
                // True exhaustion: the pass found nothing at all.
                Some("no_work".to_string())
            } else {
                // Children found, none dispatched: name the real skip reason
                // (e.g. `lane-cap`) instead of collapsing it into no_work.
                facts.reason.clone()
            }
        }
    };
    let rotation = cfg
        .rotation
        .map(|(pos, total)| format!(" ({pos} of {total} draining)"))
        .unwrap_or_default();
    let (label, leadless_mark) = if cfg.scope.is_empty() {
        (format!("mission={}", cfg.mission), String::new())
    } else {
        (
            format!("territory={}", cfg.scope),
            if cfg.leadless {
                " leadless".to_string()
            } else {
                String::new()
            },
        )
    };
    let held_requests = if facts.held {
        format!(" requests={}", facts.requests)
    } else {
        String::new()
    };
    let gate_note = facts
        .gate_detail
        .as_deref()
        .map(|d| format!(" gate={d}"))
        .unwrap_or_default();
    let detail = format!(
        "{}{}{} ready={} closed={} dispatched={} sync={} pending={}{}{}{}",
        label,
        leadless_mark,
        rotation,
        facts.ready,
        closed,
        newly_dispatched,
        sync_closed,
        pending.len(),
        held_requests,
        gate_note,
        match skip_reason.as_deref() {
            Some("no_work") => match undispatched_count(cfg) {
                Ok(count) => format!(" stranded={count}"),
                Err(error) => {
                    eprintln!("active-backlog: board-wide stranded count unavailable: {error}");
                    " stranded=unknown".to_string()
                }
            },
            _ => String::new(),
        }
    );
    crate::tick_ledger::emit_tick(
        journal,
        "active_backlog",
        "daemon",
        closed + newly_dispatched + sync_closed,
        skip_reason.as_deref(),
        Some(&detail),
        cfg.interval_seconds.max(1),
    );
    outcome
}

// ── target resolution + resident supervisor ─────────────────────────────────────

/// One mission drain target as resolved by the Python `fno config
/// active-backlog --json` helper (an active mission + the epic's workspace path).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct ResolvedTarget {
    /// The mission epic's own project (for keying + cwd resolution).
    pub project: String,
    /// The epic project's cwd - roots the loop's journal + node-global reads.
    pub cwd: String,
    pub interval_seconds: u64,
    pub failure_limit: u32,
    /// The active mission's epic id (the drain's `advance --epic` argument).
    /// Optional only so a malformed receipt deserializes; a target with no
    /// mission is skipped by the supervisor.
    #[serde(default)]
    pub mission: Option<String>,
    /// The territory key: the canonical team scope, empty on a
    /// legacy receipt (the loop then keys by `mission`).
    #[serde(default)]
    pub scope: String,
    /// The team rung of the scope; 0 on a legacy receipt.
    #[serde(default)]
    pub rung: u8,
    /// No live team holds the scope; the drain continues regardless.
    #[serde(default)]
    pub leadless: bool,
    /// What one tick converges: epic ids at rung 2, project names at rungs 0/1.
    #[serde(default)]
    pub members: Vec<String>,
    /// GLOBAL ceiling on concurrent converge runs across every mission, from
    /// `config.active_backlog.max_concurrent`. Every target carries the same
    /// value; the target list is just the daemon's one config channel. An
    /// older receipt that omits it parses to the serial default of 1.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: u32,
}

fn default_max_concurrent() -> u32 {
    1
}

/// The drain loop's key: the territory scope when the receipt carries one,
/// else the legacy mission id (an older Python resolver still in the field).
fn territory_key(target: &ResolvedTarget) -> String {
    if target.scope.is_empty() {
        target.mission.clone().unwrap_or_default()
    } else {
        target.scope.clone()
    }
}

/// The drain's ONE converge gate: `max_concurrent` permits shared by every
/// mission loop.
///
/// The cap was declared and read by nothing for a release, so a config saying
/// 1 ran five concurrent `advance --epic` children (one per active mission) at
/// 27-38% CPU each and held the machine's load above the spawn gate's refusal
/// trigger for hours. Holding it here, where the converge runs are launched, is
/// what makes the declared number bind.
#[derive(Debug)]
pub struct ConvergeGate {
    sem: Semaphore,
    /// Permits currently issued. `Semaphore` exposes only what is *available*,
    /// which says nothing about capacity while converges hold permits, so the
    /// total is tracked here to compute a resize delta.
    total: AtomicU32,
}

impl ConvergeGate {
    pub fn new(cap: u32) -> Self {
        let cap = cap.max(1);
        Self {
            sem: Semaphore::new(cap as usize),
            total: AtomicU32::new(cap),
        }
    }

    /// Re-sync the gate to `cap` so a config change lands without a daemon
    /// restart. Growing adds permits at once. Shrinking can only forget permits
    /// that are FREE, so the total records what actually went and the next
    /// resync retries the remainder as in-flight converges hand theirs back.
    /// ponytail: a shrink while every permit stays continuously busy can lag
    /// the declared cap until a mission goes idle; each mission's own poll
    /// floor (minutes, not this 60s resync) makes that idle gap routine in
    /// practice. Upgrade path if it ever bites: a permit wrapper that forgets
    /// itself on return while a shrink is still pending, instead of this
    /// snapshot-based forget.
    pub fn resize(&self, cap: u32) {
        let cap = cap.max(1);
        let old = self.total.load(Ordering::SeqCst);
        if cap > old {
            self.sem.add_permits((cap - old) as usize);
            self.total.store(cap, Ordering::SeqCst);
        } else if cap < old {
            let forgotten = self.sem.forget_permits((old - cap) as usize) as u32;
            self.total.store(old - forgotten, Ordering::SeqCst);
        }
    }

    /// The cap the gate is currently enforcing.
    pub fn capacity(&self) -> u32 {
        self.total.load(Ordering::SeqCst)
    }
}

/// [`resolve_targets`] plus what the supervisor's tick row needs to say WHY:
/// the receipt's own zero-path token and mission count (a disabled drain is
/// `drain_disabled`, not `no_missions` -), beside the shell-level
/// failure (`env_broken`, the missing-click class) that predates the receipt.
#[derive(Debug, Clone)]
pub struct DrainResolve {
    pub targets: Vec<ResolvedTarget>,
    /// Active missions counted by the CLI whatever the config says; a legacy
    /// receipt reports none.
    pub missions: u64,
    /// The receipt's zero-path token (`drain_disabled`, `bad_interval`, ...);
    /// absent from a legacy receipt and whenever targets resolved.
    pub skip_reason: Option<String>,
    /// Why the resolver never produced a reading (missing fno, non-zero exit,
    /// unparseable output); `None` when it ran clean.
    pub failure: Option<String>,
}

/// Shell `fno config active-backlog --json` to discover enabled drain targets.
/// Best-effort: any failure (missing fno, non-zero exit, unparseable output)
/// yields an empty list, so the feature simply stays dormant.
pub fn resolve_targets(config_cwd: &Path, registry_path: &Path) -> Vec<ResolvedTarget> {
    resolve_targets_report(config_cwd, registry_path).targets
}

/// The drain-target receipt, computed natively from the territory fact set
/// (`territory::resolve_territories` + the `active_backlog` config block) -
/// the same JSON shape the Python `fno config active-backlog --json` command
/// prints, so the supervisor and the passthroughs read one contract. An
/// unreadable source is the Err naming it; never an empty list read as
/// "nothing enabled".
pub fn native_receipt(config_cwd: &Path, registry_path: &Path) -> Result<Vec<Value>, String> {
    let facts = territory::active_backlog_facts(config_cwd);
    if !facts.any_enabled() {
        return Ok(Vec::new());
    }
    let held = held_for(config_cwd, registry_path);
    let territories = territory::resolve_territories(config_cwd, registry_path).map_err(|e| e.0)?;
    Ok(drain_targets_json(&facts, &territories, &held).0)
}

/// The held map for one config cwd: one fold over the question journals,
/// failing open to an empty map (a missing journal holds nothing).
fn held_for(cwd: &Path, registry_path: &Path) -> std::collections::BTreeMap<String, String> {
    match registry_path.parent().and_then(Path::parent) {
        Some(fno_dir) => crate::needs::held_map(fno_dir, cwd),
        None => Default::default(),
    }
}

/// Target rows for one resolved territory set, plus the drop counts the
/// zero-path reason needs: `(rows, project_disabled_drops, missing_path_drops)`.
/// The config gate runs BEFORE the rootability check so a drop counts exactly
/// once, in the order the reason rule reads them.
fn drain_targets_json(
    facts: &territory::ActiveBacklogFacts,
    territories: &[territory::Territory],
    held: &std::collections::BTreeMap<String, String>,
) -> (Vec<Value>, usize, usize, Vec<(String, String)>) {
    let Some(interval) = facts.interval_seconds else {
        return (Vec::new(), 0, 0, Vec::new());
    };
    let mut targets = Vec::new();
    let mut disabled = 0;
    let mut missing_path = 0;
    let mut held_drops: Vec<(String, String)> = Vec::new();
    for territory in territories {
        let root_project = territory.project.clone();
        if !facts.is_enabled_for(Some(&root_project)) {
            disabled += 1;
            continue;
        }
        // `Territory::project` already carries the right root for both cases
        // (the first member epic's own project at rung 2, the project itself
        // at rungs 0/1 - see `resolve_territories`), so there is one path.
        if root_project.is_empty() || territory.cwd.is_empty() {
            missing_path += 1;
            continue; // unrootable: skipped, never guessed
        }
        let mission = if territory.rung == 2 {
            territory.members.first().cloned()
        } else {
            None
        };
        // A held mission stops the whole territory: the drain must not spawn
        // into an epic a question is blocking. Other held members
        // drop from the member list; the receipt names the question.
        if mission.as_deref().is_some_and(|m| held.contains_key(m)) {
            if let Some(qid) = mission.as_deref().and_then(|m| held.get(m)) {
                held_drops.push((territory.key.clone(), qid.clone()));
            }
            continue;
        }
        let members: Vec<String> = territory
            .members
            .iter()
            .filter(|m| !held.contains_key(m.as_str()))
            .cloned()
            .collect();
        targets.push(json!({
            "project": territory.project,
            "cwd": territory.cwd,
            "interval_seconds": interval as u64,
            "failure_limit": facts.failure_limit,
            "mission": mission,
            "scope": territory.key,
            "rung": territory.rung,
            "leadless": territory.leadless,
            "members": members,
            "max_concurrent": facts.max_concurrent,
        }));
    }
    // The drain iterates in canonical scope order (the Python resolver's
    // `sorted(_territories(), key=scope)`).
    targets.sort_by(|a, b| {
        a["scope"]
            .as_str()
            .unwrap_or("")
            .cmp(b["scope"].as_str().unwrap_or(""))
    });
    (targets, disabled, missing_path, held_drops)
}

/// [`resolve_targets`] plus what the supervisor's tick row needs to say WHY
///, computed on one native pass: the receipt's own zero-path token
/// (`drain_disabled`, `bad_interval`, `no_missions`, `project_disabled`,
/// `no_workspace_path`) and the territory count, taken BEFORE the config
/// gates so the count is the truth even when the drain is off. An unreadable
/// source is `failure` (`env_broken`, the missing-click class), never an
/// empty list read as "nothing enabled".
pub fn resolve_targets_report(config_cwd: &Path, registry_path: &Path) -> DrainResolve {
    let facts = territory::active_backlog_facts(config_cwd);
    let held = held_for(config_cwd, registry_path);
    let territories = territory::resolve_territories(config_cwd, registry_path);
    let missions = territories.as_ref().map_or(0, |t| t.len() as u64);
    let mut skip_reason: Option<String> = None;
    if !facts.any_enabled() {
        skip_reason = Some("drain_disabled".to_string());
    } else if facts.interval_seconds.is_none() {
        skip_reason = Some("bad_interval".to_string());
    }
    match territories {
        Err(e) => DrainResolve {
            targets: Vec::new(),
            missions,
            skip_reason,
            failure: Some(e.0.chars().take(200).collect::<String>()),
        },
        Ok(territories) => {
            if skip_reason.is_none() && territories.is_empty() {
                skip_reason = Some("no_missions".to_string());
            }
            let (rows, disabled, missing_path, held_drops) =
                drain_targets_json(&facts, &territories, &held);
            match rows
                .into_iter()
                .map(|t| serde_json::from_value::<ResolvedTarget>(t))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(targets) => {
                    if targets.is_empty() && skip_reason.is_none() {
                        // Every live target held: name the question, not a
                        // generic empty receipt.
                        if let Some((_, qid)) = held_drops.first() {
                            skip_reason = Some(format!("held:{qid}"));
                        }
                    }
                    if targets.is_empty() && skip_reason.is_none() {
                        skip_reason = Some(if disabled >= missing_path {
                            "project_disabled".to_string()
                        } else {
                            "no_workspace_path".to_string()
                        });
                    }
                    DrainResolve {
                        targets,
                        missions,
                        skip_reason,
                        failure: None,
                    }
                }
                Err(e) => DrainResolve {
                    targets: Vec::new(),
                    missions,
                    skip_reason,
                    failure: Some(format!("active-backlog receipt unrepresentable: {e}")),
                },
            }
        }
    }
}

/// A project the status-fanout supervisor should tick. Enablement is
/// "has >=1 enabled status sink", INDEPENDENT of the drain's active_backlog set -
/// a project can fan status out without opting into the backlog drain.
#[derive(Debug, Clone, serde::Deserialize)]
struct FanoutTarget {
    pub project: String,
    pub cwd: String,
    pub interval_seconds: u64,
}

/// Shell `fno config status-sinks --json` to discover fanout targets. Best-effort:
/// any failure (missing fno, non-zero exit, unparseable output) yields an empty
/// list, so a broken config never crashes the daemon - it just runs no fanout.
fn resolve_fanout_targets(fno_bin: &str) -> Vec<FanoutTarget> {
    match fno_cmd(fno_bin)
        .args(["config", "status-sinks", "--json"])
        .output()
    {
        Ok(o) if o.status.success() => serde_json::from_slice(&o.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// One project's status-fanout loop: shell `fno doctor event fanout tick` in the
/// project cwd on the configured cadence, best-effort. Independent of the drain
/// loops; a tick failure is swallowed and the next tick retries. The supervisor
/// resolves enablement once per pass and owns it: a new `interval_secs` lands in
/// the loop's `interval` cell, and removing the project's sinks aborts the loop.
/// The loop itself never shells the config read. Exits on shutdown.
/// Cap on a single `fno doctor event fanout tick` child. A legitimately slow tick
/// (several stalled sinks x (retries+1) x http_timeout + backoff) can reach
/// minutes; 300s bounds the pathological hang, not normal work.
const TICK_CHILD_CAP: Duration = Duration::from_secs(300);

/// Await `cmd`'s completion bounded by `cap`, killing the child on timeout.
/// Returns `true` if the child exceeded the cap and was killed. `kill_on_drop`
/// is load-bearing: on timeout the `output()` future is dropped, which SIGKILLs
/// the child - without it a wedged tick parks the loop's shutdown response and
/// leaks one subprocess per tick. Extracted for unit-testability.
async fn output_with_cap(mut cmd: tokio::process::Command, cap: Duration) -> bool {
    cmd.kill_on_drop(true);
    match tokio::time::timeout(cap, cmd.output()).await {
        Ok(Ok(_)) => false,
        // Spawn/exec failure (binary missing, cwd gone, ...) is best-effort like
        // the tick itself, but log it - a swallowed missing-`fno` is undiagnosable.
        Ok(Err(e)) => {
            eprintln!("fanout tick failed to execute: {e}");
            false
        }
        Err(_) => true,
    }
}

async fn per_project_fanout_loop(
    target: FanoutTarget,
    fno_bin: String,
    interval: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
) {
    let project = target.project.clone();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let interval = Duration::from_secs(interval.load(Ordering::SeqCst).max(1));
        let mut cmd = tokio::process::Command::new(&fno_bin);
        cmd.args(["doctor", "event", "fanout", "tick"])
            .current_dir(&target.cwd);
        // Failure otherwise swallowed (next tick retries; at-least-once cursor
        // semantics). The kill must NOT be silent - the one line below is required.
        if output_with_cap(cmd, TICK_CHILD_CAP).await {
            eprintln!(
                "fanout tick for {project} exceeded {TICK_CHILD_CAP:?}; killed, retrying next tick"
            );
        }
        sleep_interruptible(interval, &shutdown).await;
    }
}

/// One live fanout loop and the interval cell the supervisor updates.
type FanoutTasks = HashMap<String, (tokio::task::JoinHandle<()>, Arc<AtomicU64>)>;

/// Reconcile the fanout loops with this pass's targets: stop a loop whose
/// project left the set, update a live loop's interval, spawn a missing one.
/// Shared by the normal pass and the paused pass, which differ only in what
/// they resolve.
fn spawn_fanout_loops(
    fanout_targets: Vec<FanoutTarget>,
    fno_bin: &str,
    fanout_tasks: &mut FanoutTasks,
    shutdown: Arc<AtomicBool>,
) {
    fanout_tasks.retain(|project, (handle, _)| {
        let keep = fanout_targets.iter().any(|t| &t.project == project);
        if !keep {
            handle.abort();
        }
        keep
    });
    for ft in fanout_targets {
        match fanout_tasks.entry(ft.project.clone()) {
            std::collections::hash_map::Entry::Occupied(slot) => {
                slot.get().1.store(ft.interval_seconds, Ordering::SeqCst);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                let interval = Arc::new(AtomicU64::new(ft.interval_seconds));
                let handle = tokio::spawn(per_project_fanout_loop(
                    ft,
                    fno_bin.to_string(),
                    Arc::clone(&interval),
                    Arc::clone(&shutdown),
                ));
                slot.insert((handle, interval));
            }
        }
    }
}

/// Build the per-project loop journal (project events.jsonl fatal, global mirror
/// best-effort) for a drain target's cwd.
fn journal_for(cwd: &Path) -> Journal {
    let project_events = ProjectJournalPath::from_caller_root(cwd);
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    let global_events = home.join(".fno").join("events.jsonl");
    Journal::new(project_events, GlobalJournalPath(global_events))
}

/// The dispatch-provenance env carrier: the origin names THIS
/// daemon arm (never a session - the daemon env is scrubbed by design), and
/// the owner names the durable mission or team scope the work answers to,
/// leadless or not. `fno agents spawn` consumes these on the request and the
/// row carries them; a session running `fno backlog advance` by hand exports
/// neither, so its workers keep their ambient session parent.
fn spawn_provenance_env(cfg: &DrainConfig, arm: &str) -> Vec<(String, String)> {
    // A legacy receipt with neither project nor scope cannot honestly name an
    // owner; exporting a blank one would refuse at the door, so the carrier
    // is omitted and those dispatches keep today's shape.
    if cfg.project.is_empty() && cfg.scope.is_empty() {
        return Vec::new();
    }
    let origin = serde_json::json!({
        "kind": "non_session",
        "source": {
            "kind": "daemon",
            "exe": "fno-agents-daemon",
            "arm": arm,
            "cause": "ab",
        },
    });
    let owner = if cfg.scope.is_empty() {
        serde_json::json!({
            "kind": "mission",
            "project": cfg.project,
            "mission": cfg.mission,
        })
    } else {
        serde_json::json!({
            "kind": "role",
            "project": cfg.project,
            "scope": cfg.scope,
        })
    };
    vec![
        ("FNO_SPAWN_ORIGIN".to_string(), origin.to_string()),
        ("FNO_SPAWN_OWNER".to_string(), owner.to_string()),
    ]
}

/// Resolve a [`DrainConfig`] for a mission target, or `None` if the target
/// carries no mission id (a malformed receipt). No driver-lib preflight: the
/// worker drivers are resolved per CHILD project inside `advance --epic`, not at
/// the epic's cwd, so the epic project need not itself be drivable.
fn drain_config_for(
    target: &ResolvedTarget,
    fno_bin: &str,
    rotation: Option<(usize, usize)>,
) -> Option<DrainConfig> {
    let key = territory_key(target);
    if key.is_empty() {
        // A malformed receipt (no scope and no mission) keys an unnamed loop.
        return None;
    }
    let members: Vec<DrainMember> = target
        .members
        .iter()
        .map(|id| DrainMember {
            id: id.clone(),
            epic: target.rung == 2,
        })
        .collect();
    Some(DrainConfig {
        cwd: PathBuf::from(&target.cwd),
        fno_bin: fno_bin.to_string(),
        mission: target.mission.clone().unwrap_or_else(|| key.clone()),
        project: target.project.clone(),
        scope: target.scope.clone(),
        leadless: target.leadless,
        members,
        failure_limit: target.failure_limit,
        interval_seconds: target.interval_seconds,
        rotation,
        advance_timeout_s: crate::flight_gate::FLIGHT_TTL_MS as u64 / 1000,
    })
}

/// The wake nudge sentinel path ($HOME/.fno/state/active-backlog-nudge by
/// default). Mirrors the Python writer it replaces under the default state
/// dir; a non-default state_dir only loses the latency optimization, never
/// correctness (the poll floor is the guarantee).
pub(crate) fn nudge_sentinel_path() -> PathBuf {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    home.join(".fno").join("state").join("active-backlog-nudge")
}

/// The sentinel's mtime, or `None` if it does not exist / cannot be stat'd.
/// The blocking `stat` is offloaded to the blocking pool so polling it every
/// 500ms never blocks the async executor (gemini finding). `tokio::fs` is not
/// used to avoid adding the `fs` feature to the tokio dependency.
async fn nudge_mtime() -> Option<std::time::SystemTime> {
    tokio::task::spawn_blocking(|| {
        std::fs::metadata(nudge_sentinel_path())
            .and_then(|m| m.modified())
            .ok()
    })
    .await
    .ok()
    .flatten()
}

/// Wait up to `total` for the next poll tick, waking EARLY if the nudge sentinel
/// changes (an event nudge) or `shutdown` flips. `last` carries the mtime across
/// calls; a burst of touches during a tick coalesces to a single wake because
/// `last` advances to the newest mtime once, here. The poll floor (`total`) is
/// the backstop, so a missed nudge just delays a drain by at most one interval.
async fn wait_for_wake(
    total: Duration,
    shutdown: &Arc<AtomicBool>,
    last: &mut Option<std::time::SystemTime>,
) {
    let step = Duration::from_millis(500);
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let current = nudge_mtime().await;
        if current != *last {
            *last = current;
            return; // event nudge: wake early (coalesced)
        }
        let chunk = step.min(total - elapsed);
        tokio::time::sleep(chunk).await;
        elapsed += chunk;
    }
}

/// The resident drain supervisor (node).
///
/// Spawns ONE independent drain loop per enabled project so a long-running drain
/// in one project never blocks or starves another (gemini finding). It sets
/// `live` true whenever there is >=1 enabled target so the daemon's idle-exit
/// stays out (OQ1 Option A: an enabled but drained board keeps the daemon
/// resident and polling). Runs until `shutdown` is set, then aborts the
/// per-project loops; an in-flight `spawn_blocking` tick is not abortable, but
/// that is safe by design - the dispatched worker owns its `node:<id>` claim
/// independently and the live-claims filter excludes it on the next start.
/// Daemon entry: spawn the drain supervisor exactly as the daemon's inline
/// block used to, so daemon.rs (over the line budget) shrinks. A sandbox
/// home gets a no-op handle: fleet work must not act on real state from a
/// tempdir. The handle returns to the caller for shutdown abort.
pub fn spawn_for_daemon(
    home: &crate::paths::AgentsHome,
    sandbox: bool,
    live: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) -> tokio::task::JoinHandle<()> {
    if sandbox {
        return tokio::spawn(std::future::ready(()));
    }
    let fno_bin = crate::scrape::fno_bin().to_string_lossy().into_owned();
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "active-backlog");
    tokio::spawn(run_supervisor(fno_bin, emitter, live, shutdown))
}

pub async fn run_supervisor(
    fno_bin: String,
    emitter: EventEmitter,
    live: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
) {
    // Mission drain loops, keyed by epic id (K2): one per active mission.
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    // Sibling loop family: status-fanout ticks, keyed by project. A
    // separate enablement set (projects with >=1 status sink) from the drain
    // above, so a sinks-only project fans out without opting into the drain.
    let mut fanout_tasks: FanoutTasks = HashMap::new();
    let recheck = Duration::from_secs(60);
    // ONE gate for the whole drain: `max_concurrent` bounds concurrent converge
    // runs across every mission, never per mission.
    let gate = Arc::new(ConvergeGate::new(1));

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        // Drop handles for loops that have exited (a mission retired / deactivated).
        tasks.retain(|_, h| !h.is_finished());
        fanout_tasks.retain(|_, (h, _)| !h.is_finished());

        // while dispatch is effectively paused, the supervisor must
        // NOT resolve drain targets. Resident mission handles are retained
        // (each loop gates itself before any child); status-fanout keeps
        // resolving so the fanout family stays live. One positive tick row
        // names the pause at this loop's real 60s recheck cadence.
        let pause = crate::loops_pause::dispatch_pause();
        if pause.is_paused() {
            let fanout_targets = resolve_fanout_targets(&fno_bin);
            live.store(
                !tasks.is_empty() || !fanout_targets.is_empty(),
                Ordering::SeqCst,
            );
            spawn_fanout_loops(
                fanout_targets,
                &fno_bin,
                &mut fanout_tasks,
                Arc::clone(&shutdown),
            );
            let _ = emitter.emit(
                crate::tick_ledger::EVENT_TYPE,
                &serde_json::json!({
                    "arm": "active_backlog",
                    "scheduler": "daemon",
                    "acted": 0,
                    "skip_reason": pause.skip_reason(),
                    "detail": format!("dispatch paused: {}", pause.detail()),
                    "interval_s": 60,
                }),
            );
            sleep_interruptible(recheck, &shutdown).await;
            continue;
        }

        let DrainResolve {
            targets,
            missions: receipt_missions,
            skip_reason: receipt_reason,
            failure: resolve_failure,
        } = resolve_targets_report(
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            &crate::paths::AgentsHome::from_env().registry_json(),
        );
        // Re-sync the cap every recheck so `fno config set` lands without a
        // daemon restart. With no targets there is nothing to gate.
        if let Some(cap) = targets.iter().map(|t| t.max_concurrent).max() {
            gate.resize(cap);
        }
        let fanout_targets = resolve_fanout_targets(&fno_bin);
        // `live` keeps the daemon out of idle-exit while ANY supervised work
        // exists - drain OR fanout. A sink-only project (no active_backlog) must
        // keep the daemon alive, else the daemon idle-exits and kills its fanout
        // loop (codex P1).
        live.store(
            !targets.is_empty() || !fanout_targets.is_empty(),
            Ordering::SeqCst,
        );

        // The arm's supervisor-level tick row, ONLY while no mission loop is
        // live to write its own (fresher) rows: it says why the drain has
        // nothing to do - a broken resolver (env_broken, the class that ticked
        // silently for hours because its Python env lacked click), the
        // receipt's own zero-path (drain_disabled names the config switch;
        //), or genuinely no missions. ab_live covers the fanout family.
        if targets.is_empty() {
            let skip = if resolve_failure.is_some() {
                "env_broken".to_string()
            } else {
                receipt_reason.unwrap_or_else(|| "no_missions".to_string())
            };
            let _ = emitter.emit(
                crate::tick_ledger::EVENT_TYPE,
                &serde_json::json!({
                    "arm": "active_backlog",
                    "scheduler": "daemon",
                    "acted": 0,
                    "skip_reason": skip,
                    "detail": format!(
                        "missions={} targets=0 ab_live={} fanouts={}{}",
                        receipt_missions,
                        !fanout_targets.is_empty(),
                        fanout_targets.len(),
                        resolve_failure.as_deref().map(|f| format!(" resolve={f}")).unwrap_or_default()
                    ),
                    // The recheck cadence this row actually rides, not the
                    // configured drain interval: staleness must bound the
                    // supervisor's own 60s loop.
                    "interval_s": 60,
                }),
            );
        }

        for target in targets {
            // Key by territory: the canonical team scope; a legacy
            // receipt without one still keys by mission. An empty key is a
            // malformed receipt; skip it rather than key an unnamed loop.
            let key = territory_key(&target);
            if key.is_empty() {
                continue;
            }
            // Entry API (single lookup): only spawn when this territory has no
            // live loop yet, mirroring the fanout family below.
            if let std::collections::hash_map::Entry::Vacant(slot) = tasks.entry(key) {
                slot.insert(tokio::spawn(mission_drain_loop(
                    target,
                    fno_bin.clone(),
                    emitter.clone(),
                    Arc::clone(&shutdown),
                    Arc::clone(&gate),
                )));
            }
        }

        spawn_fanout_loops(
            fanout_targets,
            &fno_bin,
            &mut fanout_tasks,
            Arc::clone(&shutdown),
        );

        sleep_interruptible(recheck, &shutdown).await;
    }

    for (_, h) in tasks {
        h.abort();
    }
    for (_, (h, _)) in fanout_tasks {
        h.abort();
    }
    live.store(false, Ordering::SeqCst);
}

/// Sleep `total`, waking early if `shutdown` flips. Checked in small steps so a
/// long poll interval still tears down promptly at daemon shutdown.
async fn sleep_interruptible(total: Duration, shutdown: &Arc<AtomicBool>) {
    let step = Duration::from_millis(500);
    let mut elapsed = Duration::ZERO;
    while elapsed < total {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let chunk = step.min(total - elapsed);
        tokio::time::sleep(chunk).await;
        elapsed += chunk;
    }
}

/// One mission's independent drain loop: reconcile + dispatch the mission's ready
/// children, wait the poll floor (or an event nudge), repeat. Owns its own
/// [`CircuitBreaker`] so failure streaks are per mission. Exits when `shutdown`
/// flips, the mission drops out of the resolved target set (its `mission_active`
/// was cleared), or `advance --epic` reports the mission deactivated / all done.
async fn mission_drain_loop(
    target: ResolvedTarget,
    fno_bin: String,
    emitter: EventEmitter,
    shutdown: Arc<AtomicBool>,
    gate: Arc<ConvergeGate>,
) {
    // A malformed target with no key is filtered by the supervisor before
    // spawn; default to empty so this never panics if one slips through (the
    // re-resolve below then finds no match and exits).
    let key = territory_key(&target);
    let mut breaker = CircuitBreaker::new(target.failure_limit);
    // In-flight fire-and-forget dispatches, reconciled from events across ticks
    //. Resident like the breaker so a worker dispatched one tick is
    // polled to completion on the next.
    let mut pending: Vec<PendingDispatch> = Vec::new();
    let mut last_nudge = nudge_mtime().await;
    let mut backoff = Duration::from_secs(1);

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }

        // The effective dispatch pause (manual sentinel, machine breaker,
        // or this territory's targeted breaker record) is checked before the
        // permit, so a stopped incident runs no dispatch-oriented child. The
        // target this loop was spawned with names the blocked tick (cwd,
        // interval) without re-resolving; pending and the breaker are
        // untouched, and the same loop re-resolves and resumes on clear.
        let subject = crate::fleet_incident::Subject {
            session_ids: Vec::new(),
            node: target.mission.clone().or_else(|| Some(key.clone())),
            territory: (!target.scope.is_empty()).then(|| target.scope.clone()),
            cwd: Path::new(&target.cwd),
        };
        let pause = crate::loops_pause::dispatch_pause_for_territory(&subject);
        if pause.is_paused() {
            let journal = journal_for(Path::new(&target.cwd));
            crate::tick_ledger::emit_tick(
                &journal,
                "active_backlog",
                "daemon",
                0,
                Some(pause.skip_reason()),
                Some(&format!(
                    "mission={} dispatch paused: {}",
                    key,
                    pause.detail()
                )),
                target.interval_seconds.max(1),
            );
            wait_for_wake(
                Duration::from_secs(target.interval_seconds.max(1)),
                &shutdown,
                &mut last_nudge,
            )
            .await;
            continue;
        }

        // Re-resolve this territory's liveness. If its scope dropped out of the
        // target set (team revoked / workspace gone), exit the loop (the
        // supervisor will not respawn it). The position in this list (already
        // scope-ordered) names the rotation in the tick's detail row; a lone
        // territory prints no `(1 of 1)` - that reads as a fault, not a count.
        let all = resolve_targets(
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            &crate::paths::AgentsHome::from_env().registry_json(),
        );
        let Some(pos) = all.iter().position(|t| territory_key(t) == key) else {
            break;
        };
        let t = &all[pos];
        let interval = Duration::from_secs(t.interval_seconds.max(1));
        let rotation = (all.len() >= 2).then_some((pos + 1, all.len()));

        let Some(cfg) = drain_config_for(t, &fno_bin, rotation) else {
            // Malformed target (no mission id); back off and re-check.
            sleep_interruptible(interval, &shutdown).await;
            continue;
        };
        let journal = journal_for(&cfg.cwd);

        // Take a converge slot before the tick shells `advance --epic`. A
        // mission that must wait SAYS so first and then waits its turn: a
        // skipped mission starves silently, and an unlogged wait reads as one.
        let permit = match gate.sem.try_acquire() {
            Ok(p) => p,
            Err(_) => {
                crate::tick_ledger::emit_tick(
                    &journal,
                    "active_backlog",
                    "daemon",
                    0,
                    Some("converge_cap"),
                    Some(&format!(
                        "mission={} queued for 1 of {} converge slot(s)",
                        cfg.mission,
                        gate.capacity()
                    )),
                    cfg.interval_seconds.max(1),
                );
                match gate.sem.acquire().await {
                    Ok(p) => p,
                    Err(_) => break,
                }
            }
        };

        // The tick is synchronous; offload so the async runtime is never stalled.
        // Move the breaker AND pending set in and hand them back so the streak
        // and in-flight tracking survive the tick.
        let taken_b = std::mem::take(&mut breaker);
        let taken_p = std::mem::take(&mut pending);
        let handle = tokio::task::spawn_blocking(move || {
            let mut b = taken_b;
            let mut p = taken_p;
            let outcome = mission_drain_tick(&cfg, &mut b, &mut p, &journal);
            (outcome, b, p)
        });
        let tick_result = handle.await;
        // Hand the slot back the moment the converge is over, before any wait.
        drop(permit);
        match tick_result {
            Ok((outcome, b, p)) => {
                breaker = b;
                pending = p;
                backoff = Duration::from_secs(1);
                if outcome == MissionDispatch::Retire {
                    let _ =
                        emitter.emit("active_backlog_mission_retired", &json!({"mission": key}));
                    break;
                }
            }
            Err(join_err) => {
                let _ = emitter.emit(
                    "active_backlog_task_crashed",
                    &json!({"mission": key, "error": join_err.to_string()}),
                );
                // The panicked breaker's streak is lost (rare); a fresh one is
                // safe (a crash-looping node re-accrues failures and re-defers).
                // Pending tracking is also lost, but the in-flight workers still
                // run and their nodes close at merge via `fno backlog reconcile`.
                breaker = CircuitBreaker::new(t.failure_limit);
                pending = Vec::new();
                sleep_interruptible(backoff, &shutdown).await;
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        }

        wait_for_wake(interval, &shutdown, &mut last_nudge).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::territory::Territory;

    fn held_facts() -> territory::ActiveBacklogFacts {
        territory::ActiveBacklogFacts {
            enabled: Ok(true),
            interval_seconds: Some(60),
            failure_limit: 3,
            max_concurrent: 2,
        }
    }

    fn held_territory(members: Vec<String>) -> Territory {
        Territory {
            key: "x-e".to_string(),
            rung: 2,
            leadless: true,
            members,
            project: "alpha".to_string(),
            cwd: "/tmp/alpha".to_string(),
        }
    }

    #[test]
    fn drain_drops_a_held_mission_and_names_the_question() {
        let facts = held_facts();
        let territories = vec![held_territory(vec!["x-e".to_string(), "x-e2".to_string()])];
        let held: std::collections::BTreeMap<String, String> =
            [("x-e".to_string(), "q-1".to_string())]
                .into_iter()
                .collect();
        let (rows, disabled, missing_path, held_drops) =
            drain_targets_json(&facts, &territories, &held);
        assert_eq!(disabled, 0);
        assert_eq!(missing_path, 0);
        assert!(
            rows.is_empty(),
            "held mission: territory absent from targets"
        );
        assert_eq!(held_drops, vec![("x-e".to_string(), "q-1".to_string())]);
    }

    #[test]
    fn loose_member_argv_names_the_project_door() {
        // The CLI takes no positional beside --loose/--project; the daemon's
        // loose dispatch must name the project through --project or the
        // command is a usage error and the rung-1 territory never drains.
        let member = DrainMember {
            id: "readyrule".to_string(),
            epic: false,
        };
        assert_eq!(
            advance_member_args(&member),
            vec![
                "--loose".to_string(),
                "--project".to_string(),
                "readyrule".to_string()
            ]
        );
        let epic = DrainMember {
            id: "x-e".to_string(),
            epic: true,
        };
        assert_eq!(
            advance_member_args(&epic),
            vec![
                "--epic".to_string(),
                "x-e".to_string(),
                "--continuation".to_string()
            ]
        );
    }

    /// The supervisor owns fanout enablement: a pass updates a live loop's
    /// interval, and a project whose sinks were removed has its loop stopped.
    #[tokio::test]
    async fn the_supervisor_pass_updates_and_stops_fanout_loops() {
        let parse = |json: &[u8]| -> Vec<FanoutTarget> { serde_json::from_slice(json).unwrap() };
        let shutdown = Arc::new(AtomicBool::new(false));
        let mut tasks: FanoutTasks = HashMap::new();
        // A missing binary: each tick fails to execute and the loop sleeps on.
        let bin = "/nonexistent/fno";
        let first = parse(br#"[{"project":"fno","cwd":"/","interval_seconds":5}]"#);
        assert_eq!(first[0].cwd, "/");
        spawn_fanout_loops(first, bin, &mut tasks, Arc::clone(&shutdown));
        let second = parse(br#"[{"project":"fno","cwd":"/","interval_seconds":30}]"#);
        spawn_fanout_loops(second, bin, &mut tasks, Arc::clone(&shutdown));
        assert_eq!(
            tasks["fno"].1.load(Ordering::SeqCst),
            30,
            "one loop, new interval"
        );
        let handle = tasks["fno"].0.abort_handle();
        spawn_fanout_loops(Vec::new(), bin, &mut tasks, Arc::clone(&shutdown));
        assert!(tasks.is_empty(), "a removed project leaves the set");
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(handle.is_finished(), "its loop was stopped");
        shutdown.store(true, Ordering::SeqCst);
    }

    #[test]
    fn advance_epic_receipt_parses_children_and_substrate() {
        // children[] is THE enqueue authority; substrate decides detached vs
        // synchronous.
        let r: AdvanceEpicReceipt = serde_json::from_slice(
            br#"{"epic_id":"x-e","error":null,"activated":true,"deactivated":false,
                 "all_done":false,"dispatched":["x-a"],
                 "children":[{"node_id":"x-a","decision":"dispatched","reason":null,"detail":null,
                              "short_id":"84a8d946","substrate":"thread","notes":[]},
                             {"node_id":"x-b","decision":"dispatched","reason":null,"detail":null,
                              "short_id":"84a8d947","substrate":"headless","notes":[]}]}"#,
        )
        .unwrap();
        assert_eq!(r.children.len(), 2);
        assert_eq!(r.children[0].substrate.as_deref(), Some("thread"));
        assert_eq!(r.children[1].substrate.as_deref(), Some("headless"));
        assert!(!r.deactivated);
        assert!(!r.all_done);
    }

    #[test]
    fn a_held_receipt_parses_and_reads_as_held_not_no_work() {
        // the CLI's one-in-flight gate answers `held` with an empty
        // child set. Without the field that receipt read as no_work - a lie
        // that hides exactly the stacking this gate exists to delete. Held is
        // never a retirement, so the mission flags must stay false.
        let r: AdvanceEpicReceipt =
            serde_json::from_slice(br#"{"epic_id":"x-e","held":true,"requests":4}"#).unwrap();
        assert!(r.held);
        assert_eq!(r.requests, 4);
        assert!(!r.deactivated && !r.all_done);
        let facts = facts_from_receipt(&r);
        assert!(facts.held);
        assert_eq!(facts.ready, 0);
        assert_eq!(facts.error, None);
        assert_eq!(facts.requests, 4);
    }

    // ── reconcile policy ────────────────────────────────────────────
    //
    // These drive the private reconcile helpers directly with a stub `fno` (for
    // the defer/done side effects) + a temp Journal, so the failure-streak policy
    // is covered without env-mutating claim setup. The crash-floor boot-grace
    // path uses a unique fake node id that is naturally `Free` at the real global
    // claims root, so it reads real state for a key that never exists (and never
    // writes there).

    /// Hold this for the whole body of any test that shells `fno_cmd`.
    ///
    /// `fno_cmd` resolves its binary from the process-global `$FNO_BIN` IN
    /// PREFERENCE to the path passed in, and cargo runs a crate's tests as
    /// threads in ONE process. So while a sibling test has `FNO_BIN` set to its
    /// own stub (`scrape.rs` does exactly this), every stub built here is
    /// silently bypassed and that sibling's stub runs instead - which answers
    /// nothing this test asked, leaving an empty receipt that
    /// `dispatch_mission` correctly treats as a benign skip. The failure
    /// therefore surfaces as a plain empty-vec assertion far from its cause,
    /// and only under enough parallelism to overlap the two.
    ///
    /// Reading `$FNO_BIN` needs the lock exactly as much as writing it: the
    /// race is reader-vs-writer, so a lock only the writer takes excludes
    /// nobody.
    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// A stub `fno` that appends its argv to `record` and exits 0, so a test can
    /// assert which `backlog done`/`defer` side effects the reconcile fired.
    fn stub_fno(dir: &std::path::Path, record: &std::path::Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\necho \"$@\" >> \"{}\"\nexit 0\n",
                record.display()
            ),
        );
        p.display().to_string()
    }

    /// Like [`stub_fno`], but `backlog get` answers with `node_json` on stdout so
    /// a test can control whether the node carries a PR ref. Every other command
    /// records its argv and exits 0.
    fn stub_fno_get(dir: &std::path::Path, record: &std::path::Path, node_json: &str) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\n\
                 if [ \"$2\" = \"get\" ]; then printf '%s' '{}'; exit 0; fi\n\
                 echo \"$@\" >> \"{}\"\nexit 0\n",
                node_json,
                record.display()
            ),
        );
        p.display().to_string()
    }

    fn test_cfg(tmp: &std::path::Path, fno_bin: String, failure_limit: u32) -> DrainConfig {
        DrainConfig {
            cwd: tmp.to_path_buf(),
            fno_bin,
            mission: "x-epic".to_string(),
            project: "fno".to_string(),
            scope: String::new(),
            leadless: false,
            members: Vec::new(),
            failure_limit,
            interval_seconds: 300,
            rotation: None,
            advance_timeout_s: 30,
        }
    }

    // AC7-HP: an advance child sleeping past its bound is killed and the
    // skip names the wall clock it outlived.
    #[test]
    fn a_hung_advance_child_is_killed_at_the_wall_clock() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("bin").join("fno");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        // exec: the kill hits the sleeper itself, not a shell wrapper.
        let p = crate::write_exec_stub(p.parent().unwrap(), "fno", "#!/bin/bash\nexec sleep 10\n");
        let mut cfg = test_cfg(tmp.path(), p.display().to_string(), 3);
        cfg.advance_timeout_s = 1;
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();
        let member = DrainMember {
            id: "proj".to_string(),
            epic: false,
        };
        let (dispatch, _facts) =
            dispatch_member(&cfg, &mut breaker, &member, &mut pending, &journal);
        assert!(matches!(dispatch, MissionDispatch::Continue));
        let lines = journal_lines(&project_journal);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("advance-timeout") && l.contains("killed after 1s wall clock")),
            "lines: {lines:?}"
        );
    }

    fn test_journal(tmp: &std::path::Path) -> (Journal, PathBuf) {
        let project = tmp.join(".fno").join("events.jsonl");
        let global = tmp.join("global-events.jsonl");
        std::fs::create_dir_all(project.parent().unwrap()).unwrap();
        (Journal::new_raw(project.clone(), global), project)
    }

    fn journal_lines(p: &std::path::Path) -> Vec<String> {
        // Committed rows, not journal bytes: the store cutover stopped journal
        // appends, so emitted events live only in the store beside the journal.
        let _ = crate::event_store::import_all(p);
        crate::event_store::query_events(p, &crate::event_store::EventQuery::default())
            .unwrap_or_default()
            .iter()
            .map(|r| r.line.clone())
            .collect()
    }

    #[test]
    fn resolve_dispatch_done_records_success_and_marks_done() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let record = tmp.path().join("fno-calls.txt");
        let fno = stub_fno(&tmp.path().join("bin"), &record);
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        breaker.record_failure("x-suc0001"); // pre-existing streak to prove reset

        resolve_dispatch(
            &cfg,
            &mut breaker,
            &journal,
            "x-suc0001",
            Evidence {
                reason: TerminationReason::DonePRGreen,
                message: "done".to_string(),
            },
        );

        assert_eq!(
            breaker.consecutive_failures("x-suc0001"),
            0,
            "success resets the streak"
        );
        // is_done_reason -> the reconcile marks the node done (mirrors queue.close).
        let calls = std::fs::read_to_string(&record).unwrap_or_default();
        assert!(calls.contains("backlog done x-suc0001"), "calls: {calls}");
        assert!(journal_lines(&project_journal)
            .iter()
            .any(|l| l.contains("active_backlog_dispatched") && l.contains("x-suc0001")));
    }

    #[test]
    fn resolve_dispatch_done_pr_green_without_pr_ref_is_a_failure() {
        let _env = env_guard();
        // The dead-dispatch signature. A DonePRGreen terminal asserts a
        // PR; a node carrying none means the worker died leaving nothing. It must
        // count toward the streak AND must not `backlog done` (whose merged-PR
        // cross-check is skipped entirely for a ref-less node).
        let tmp = tempfile::TempDir::new().unwrap();
        let record = tmp.path().join("fno-calls.txt");
        let fno = stub_fno_get(
            &tmp.path().join("bin"),
            &record,
            r#"{"id":"x-dead0001","status":"in_review"}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);

        resolve_dispatch(
            &cfg,
            &mut breaker,
            &journal,
            "x-dead0001",
            Evidence {
                reason: TerminationReason::DonePRGreen,
                message: "promised".to_string(),
            },
        );

        assert_eq!(
            breaker.consecutive_failures("x-dead0001"),
            1,
            "a zero-artifact DonePRGreen counts toward the streak"
        );
        let calls = std::fs::read_to_string(&record).unwrap_or_default();
        assert!(
            !calls.contains("backlog done"),
            "must not close a node whose terminal lied: {calls}"
        );
        assert!(journal_lines(&project_journal)
            .iter()
            .any(|l| l.contains("active_backlog_skip") && l.contains("x-dead0001")));
    }

    #[test]
    fn resolve_dispatch_failed_done_records_failure_not_false_success() {
        // If `fno backlog done` FAILS, the node was not actually closed, so the
        // dispatch must Park (a failure toward the streak), never a false success.
        // Regression guard for the review finding.
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fno = crate::write_exec_stub(
            &bin,
            "fno",
            "#!/usr/bin/env bash\nif [[ \"$1\" == backlog && \"$2\" == done ]]; then echo 'node has open blockers' >&2; exit 1; fi\nexit 0\n",
        );
        let cfg = test_cfg(tmp.path(), fno.display().to_string(), 3);
        let (journal, _pj) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);

        resolve_dispatch(
            &cfg,
            &mut breaker,
            &journal,
            "x-donefail",
            Evidence {
                reason: TerminationReason::DonePRGreen,
                message: "done".to_string(),
            },
        );

        assert_eq!(
            breaker.consecutive_failures("x-donefail"),
            1,
            "a failed `backlog done` must count as a failure, not a false success"
        );
    }

    #[test]
    fn resolve_dispatch_done_exit6_is_parked_with_refusal_intact() {
        // the promise gate refuses with exit 6. The loop closer must NOT
        // need a new arm - the catch-all (non-zero, non-5) maps it to Parked with
        // the stderr intact, so an unmet promise parks the node with its refusal
        // as the recorded reason rather than closing it.
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let fno = crate::write_exec_stub(
            &bin,
            "fno",
            "#!/usr/bin/env bash\nif [[ \"$1\" == backlog && \"$2\" == done ]]; then\n  echo 'Refused: promised 2 waves and asserts none of them.' >&2\n  exit 6\nfi\nexit 0\n",
        );
        // failure_limit 1 so a single exit-6 trips and emits the parked event.
        let cfg = test_cfg(tmp.path(), fno.display().to_string(), 1);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(1);

        resolve_dispatch(
            &cfg,
            &mut breaker,
            &journal,
            "x-prom6001",
            Evidence {
                reason: TerminationReason::DonePRGreen,
                message: "done".to_string(),
            },
        );

        // Exit 6 is a failure (unlike exit 5's awaiting-merge success): the
        // streak tripped at limit 1, so the node is parked, not closed.
        let parked = journal_lines(&project_journal)
            .into_iter()
            .find(|l| l.contains("active_backlog_parked") && l.contains("x-prom6001"))
            .expect("a promise-gate refusal (exit 6) must park, not close");
        // The refusal text rode through the catch-all arm intact - an operator
        // reading the journal sees WHY the close was refused, not a bare code.
        assert!(
            parked.contains("promised 2 waves"),
            "refusal text must survive into the parked detail: {parked}"
        );
    }

    #[test]
    fn resolve_crash_at_limit_defers_and_parks() {
        let _env = env_guard();
        // AC1-FR: a worker death (no termination event) counts as a failure; the
        // Nth consecutive death trips the breaker -> defer + parked event.
        let tmp = tempfile::TempDir::new().unwrap();
        let record = tmp.path().join("fno-calls.txt");
        let fno = stub_fno(&tmp.path().join("bin"), &record);
        let cfg = test_cfg(tmp.path(), fno, 2);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(2);

        resolve_crash(&cfg, &mut breaker, &journal, "x-cra0001"); // failure 1/2
        assert_eq!(breaker.consecutive_failures("x-cra0001"), 1);
        resolve_crash(&cfg, &mut breaker, &journal, "x-cra0001"); // failure 2/2 -> trip

        // Trip defers the node (graph exclusion) and resets the streak.
        assert_eq!(breaker.consecutive_failures("x-cra0001"), 0);
        let calls = std::fs::read_to_string(&record).unwrap_or_default();
        assert!(calls.contains("backlog defer x-cra0001"), "calls: {calls}");
        let parked = journal_lines(&project_journal)
            .into_iter()
            .find(|l| l.contains("active_backlog_parked") && l.contains("x-cra0001"))
            .expect("parked event");
        // A defer that landed is recorded as such, not assumed.
        assert!(parked.contains("\"deferred\":true"), "parked: {parked}");
    }

    #[test]
    fn reconcile_boot_grace_then_crash_floor() {
        let _env = env_guard();
        // A dispatched worker that never takes its `node:<id>` claim (never
        // booted) is kept for BOOT_GRACE_TICKS reconcile passes, then counted as
        // a crash. Uses a unique fake node id (naturally Free at the global root).
        let tmp = tempfile::TempDir::new().unwrap();
        crate::paths::pin_test_claims_root(tmp.path());
        let record = tmp.path().join("fno-calls.txt");
        let fno = stub_fno(&tmp.path().join("bin"), &record);
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, _pj) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = vec![PendingDispatch {
            node_id: "x-bootgrace-never-real".to_string(),
            session_id: None,
            ticks: 0,
            stamp_waits: 0,
        }];

        // Passes before the grace expires keep the dispatch and record nothing.
        for _ in 1..BOOT_GRACE_TICKS {
            reconcile_pending(&cfg, &mut breaker, &mut pending, &journal);
            assert_eq!(
                pending.len(),
                1,
                "must keep the dispatch during the boot window"
            );
            assert_eq!(breaker.consecutive_failures("x-bootgrace-never-real"), 0);
        }
        // The pass that reaches the grace counts a crash-floor failure and drops it.
        reconcile_pending(&cfg, &mut breaker, &mut pending, &journal);
        assert!(
            pending.is_empty(),
            "the never-booted dispatch is retired as a crash"
        );
        assert_eq!(breaker.consecutive_failures("x-bootgrace-never-real"), 1);
    }

    #[test]
    fn resolved_target_parses_mission_target() {
        // The Python emitter's mission-target shape round-trips; a receipt with
        // no mission deserializes (mission=None) so the supervisor can skip it.
        let t: ResolvedTarget = serde_json::from_str(
            r#"{"project":"fno","cwd":"/x","interval_seconds":60,"failure_limit":3,"mission":"x-epic"}"#,
        )
        .unwrap();
        assert_eq!(t.mission.as_deref(), Some("x-epic"));
        let no_mission: ResolvedTarget = serde_json::from_str(
            r#"{"project":"p","cwd":"/x","interval_seconds":60,"failure_limit":3}"#,
        )
        .unwrap();
        assert_eq!(no_mission.mission, None);
    }

    #[test]
    fn breaker_trips_at_limit() {
        let mut b = CircuitBreaker::new(3);
        assert!(!b.record_failure("n1"));
        assert_eq!(b.consecutive_failures("n1"), 1);
        assert!(!b.record_failure("n1"));
        assert_eq!(b.consecutive_failures("n1"), 2);
        // third failure trips
        assert!(b.record_failure("n1"));
        assert_eq!(b.consecutive_failures("n1"), 3);
    }

    /// A stub `fno` whose `backlog advance --epic` prints a fixed JSON receipt on
    /// stdout (exit 0). Any other subcommand is a no-op exit 0.
    fn stub_fno_advance(dir: &std::path::Path, receipt_json: &str) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\nif [[ \"$1\" == backlog && \"$2\" == advance ]]; then \
                 cat <<'JSON'\n{receipt_json}\nJSON\nfi\nexit 0\n"
            ),
        );
        p.display().to_string()
    }

    /// `backlog advance --epic` answers `advance_json`; `backlog get` answers
    /// `node_json`; every other command appends its argv to `record`. For the
    /// synchronous-child tests, which read graph state AND fire defers.
    fn stub_fno_advance_and_get(
        dir: &std::path::Path,
        record: &std::path::Path,
        advance_json: &str,
        node_json: &str,
    ) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\n\
                 if [[ \"$1\" == backlog && \"$2\" == advance ]]; then \
                 cat <<'JSON'\n{advance_json}\nJSON\nexit 0; fi\n\
                 if [ \"$2\" = \"get\" ]; then printf '%s' '{node_json}'; exit 0; fi\n\
                 echo \"$@\" >> \"{}\"\nexit 0\n",
                record.display()
            ),
        );
        p.display().to_string()
    }

    fn stub_fno_advance_with_observer(
        dir: &std::path::Path,
        advance_json: &str,
        observer_json: &str,
        observer_marker: Option<&std::path::Path>,
    ) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let observer = observer_marker
            .map(|path| format!("printf 'called' > '{}'\n", path.display()))
            .unwrap_or_default();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\nif [[ \"$1\" == backlog && \"$2\" == advance ]]; then \\
                 cat <<'JSON'\n{advance_json}\nJSON\nelif [[ \"$1\" == backlog && \"$2\" == undispatched ]]; then \\
                 {observer}printf '%s' '{observer_json}'\nfi\nexit 0\n"
            ),
        );
        p.display().to_string()
    }

    #[test]
    fn dispatch_mission_records_dispatched_and_continues() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","deactivated":false,"all_done":false,"dispatched":["x-a","x-b"],
                "children":[{"node_id":"x-a","decision":"dispatched","reason":null,"detail":null,
                             "short_id":"84a8d946","substrate":"thread","notes":[]},
                            {"node_id":"x-b","decision":"dispatched","reason":null,"detail":null,
                             "short_id":"84a8d947","substrate":"thread","notes":[]}]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        let (outcome, _facts) = dispatch_mission(&cfg, &mut breaker, &mut pending, &journal);
        assert_eq!(outcome, MissionDispatch::Continue);
        assert_eq!(
            pending
                .iter()
                .map(|p| p.node_id.clone())
                .collect::<Vec<_>>(),
            vec!["x-a", "x-b"]
        );
        assert!(journal_lines(&project_journal)
            .iter()
            .any(|l| l.contains("active_backlog_dispatched") && l.contains("x-a")));
    }

    #[test]
    fn incident_unavailable_reads_as_fleet_stop_unavailable_never_no_work() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let record = tmp.path().join("fno-calls.txt");
        let fno = stub_fno_drain_recorder(&tmp.path().join("bin"), &record, tmp.path());
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending: Vec<PendingDispatch> = Vec::new();

        // A corrupt fleet record is Unavailable, never clear: the tick must
        // name fleet_stop_unavailable and spawn no poll child - and never
        // read as no_work.
        let saved_agents = std::env::var_os("FNO_AGENTS_HOME");
        let agents = tmp.path().join("agents-home");
        std::fs::create_dir_all(&agents).unwrap();
        std::env::set_var("FNO_AGENTS_HOME", &agents);
        std::fs::write(
            crate::fleet_incident::fleet_stop_path(&crate::paths::AgentsHome::at(&agents)),
            b"{not json",
        )
        .unwrap();
        mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);
        match saved_agents {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }

        let rows: Vec<serde_json::Value> = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "control_plane_tick")
            .collect();
        assert_eq!(rows.len(), 1, "exactly one tick row: {rows:?}");
        let data = &rows[0]["data"];
        assert_eq!(data["skip_reason"], "fleet_stop_unavailable");
        let calls = std::fs::read_to_string(&record).unwrap_or_default();
        assert!(
            !calls.contains("advance")
                && !calls.contains("undispatched")
                && !calls.contains("active-backlog"),
            "no poll child while the record is unreadable: {calls}"
        );
    }

    /// A stub `fno` that answers the drain's three commands AND records EVERY
    /// argv, so a paused cycle is proven silent against a recorder the clear
    /// cycle proves live (the positive control).
    fn stub_fno_drain_recorder(
        dir: &std::path::Path,
        record: &std::path::Path,
        cwd: &std::path::Path,
    ) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\n\
                 echo \"$@\" >> \"{}\"\n\
                 if [[ \"$1\" == config && \"$2\" == active-backlog ]]; then \
                 cat <<'JSON'\n{{\"targets\":[{{\"project\":\"fno\",\"cwd\":\"{}\",\"interval_seconds\":1,\"failure_limit\":3,\"mission\":\"x-epic\",\"max_concurrent\":1}}],\"missions\":1}}\nJSON\nexit 0; fi\n\
                 if [[ \"$1\" == backlog && \"$2\" == advance ]]; then \
                 cat <<'JSON'\n{{\"epic_id\":\"x-epic\",\"deactivated\":false,\"all_done\":false,\"children\":[{{\"node_id\":\"x-a\",\"decision\":\"dispatched\",\"substrate\":\"thread\"}}]}}\nJSON\nexit 0; fi\n\
                 if [[ \"$1\" == backlog && \"$2\" == undispatched ]]; then \
                 printf '%s' '{{\"status\":\"ok\",\"rows\":[]}}'; fi\nexit 0\n",
                record.display(),
                cwd.display()
            ),
        );
        p.display().to_string()
    }

    #[test]
    fn dispatch_mission_refuses_to_mint_work_while_a_fleet_stop_is_active() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        // The stub's observer marker is the positive control: if the gate
        // fails and advance RUNS, this file exists and the test fails on it.
        let marker = tmp.path().join("advance-ran");
        let fno = stub_fno_advance_with_observer(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","deactivated":false,"all_done":false,
                "children":[{"node_id":"x-a","decision":"dispatched","substrate":"thread"}]}"#,
            r#"{"status":"ok","rows":[{}]}"#,
            Some(&marker),
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending: Vec<PendingDispatch> = Vec::new();

        // The stop lives in the agents home the verdict reader resolves; a
        // stopped record, not a missing one.
        let saved_home = std::env::var_os("FNO_AGENTS_HOME");
        let home = tmp.path().join("agents-home");
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("FNO_AGENTS_HOME", &home);
        let record = crate::fleet_incident::IncidentRecord {
            version: crate::fleet_incident::STATE_VERSION,
            state: "stopped".into(),
            generation: 5,
            changed_at: "2026-09-11T00:00:00Z".into(),
            changed_by: "op".into(),
            reason: "wedged lock".into(),
            holds: Vec::new(),
            source: Some("file".into()),
            target: None,
            expires_at: None,
            origin: None,
            mail: None,
            mail_session_id: None,
        };
        std::fs::write(
            crate::fleet_incident::fleet_stop_path(&crate::paths::AgentsHome::at(&home)),
            serde_json::to_string(&record).unwrap(),
        )
        .unwrap();

        let (outcome, facts) = dispatch_mission(&cfg, &mut breaker, &mut pending, &journal);

        // Restore before asserts so a panic does not leak the pin.
        match saved_home {
            Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
            None => std::env::remove_var("FNO_AGENTS_HOME"),
        }

        // AC3-DAEMON: the tick continues (reconciliation/tick reporting alive),
        // advance was NEVER invoked, and the skip row names fleet-stop + the
        // generation the stop was written at.
        assert_eq!(outcome, MissionDispatch::Continue);
        assert!(pending.is_empty(), "no child may enter pending");
        assert!(facts.ready == 0);
        assert!(!marker.exists(), "advance must not run while stopped");
        let skips: Vec<serde_json::Value> = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["data"]["reason"] == "fleet_stop")
            .collect();
        assert!(
            skips
                .iter()
                .any(|v| v["data"]["generation"] == 5 && v["data"]["mission"] == cfg.mission),
            "skip row must carry state generation and mission: {skips:?}"
        );
    }

    #[test]
    fn mission_drain_tick_appends_one_control_plane_tick_row() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","deactivated":false,"all_done":false,
                "children":[{"node_id":"x-a","decision":"dispatched","substrate":"thread"}]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        let outcome = mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);

        assert_eq!(outcome, MissionDispatch::Continue);
        let rows: Vec<serde_json::Value> = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "control_plane_tick")
            .collect();
        assert_eq!(rows.len(), 1, "exactly one arm row per tick");
        let data = &rows[0]["data"];
        assert_eq!(data["arm"], "active_backlog");
        assert_eq!(data["scheduler"], "daemon");
        assert_eq!(data["acted"], 1);
        assert_eq!(data["interval_s"], 300);
        assert!(data["skip_reason"].is_null());
        assert!(data["detail"].as_str().unwrap().contains("dispatched=1"));
    }

    #[test]
    fn dispatch_mission_retires_on_deactivated() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","deactivated":true,"all_done":false,"dispatched":[]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, _pj) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();
        assert_eq!(
            dispatch_mission(&cfg, &mut breaker, &mut pending, &journal).0,
            MissionDispatch::Retire
        );
    }

    #[test]
    fn dispatch_mission_dedups_already_pending() {
        let _env = env_guard();
        // A boot-window re-echo of a still-pending node must not double-record it.
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","children":[{"node_id":"x-a","decision":"dispatched","substrate":"thread"}]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, _pj) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = vec![PendingDispatch {
            node_id: "x-a".to_string(),
            session_id: None,
            ticks: 2,
            stamp_waits: 0,
        }];
        dispatch_mission(&cfg, &mut breaker, &mut pending, &journal);
        assert_eq!(pending.len(), 1, "x-a already pending must not be re-added");
    }

    #[test]
    fn mission_drain_tick_names_lane_cap_instead_of_no_work() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"children":[{"node_id":"x-a","decision":"skipped","reason":"lane-cap"}],"dispatched":[]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        let outcome = mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);
        assert_eq!(outcome, MissionDispatch::Continue);

        let rows: Vec<serde_json::Value> = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["type"] == "control_plane_tick")
            .collect();
        let data = &rows[0]["data"];
        assert_eq!(data["skip_reason"], "lane-cap");
        let detail = data["detail"].as_str().unwrap();
        assert!(detail.contains("ready=1"), "detail was {detail}");
        assert!(detail.contains("pending=0"), "detail was {detail}");
    }

    #[test]
    fn mission_drain_tick_reports_stranded_board_rows_on_no_work() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let observer = r#"{"status":"ok","rows":[{}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {}]}"#;
        let fno = stub_fno_advance_with_observer(
            &tmp.path().join("bin"),
            r#"{"children":[],"dispatched":[]}"#,
            observer,
            None,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);

        let row = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["type"] == "control_plane_tick")
            .expect("one tick row");
        assert_eq!(row["data"]["skip_reason"], "no_work");
        let detail = row["data"]["detail"].as_str().unwrap();
        assert!(detail.contains("mission=x-epic"), "detail was {detail}");
        assert!(detail.contains("stranded=25"), "detail was {detail}");
    }

    #[test]
    fn mission_drain_tick_dispatches_the_real_null_bearing_receipt() {
        // The receipt the CLI actually prints on a dispatch carries
        // "reason": null on the child. The tick must read acted=1 with no
        // skip reason and the child in pending - never no_work.
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let fno = stub_fno_advance(
            &tmp.path().join("bin"),
            r#"{"epic_id":"x-epic","error":null,"activated":false,"deactivated":false,
                "all_done":false,"dispatched":["x-a"],
                "children":[{"node_id":"x-a","decision":"dispatched","reason":null,"detail":null,
                             "short_id":"84a8d946","substrate":"thread","notes":[]}]}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);

        assert_eq!(pending.len(), 1, "dispatched child must enter pending");
        let row = journal_lines(&project_journal)
            .iter()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .find(|v| v["type"] == "control_plane_tick")
            .expect("one tick row");
        assert_eq!(row["data"]["acted"], 1);
        assert!(row["data"]["skip_reason"].is_null());
        assert!(
            row["data"]["detail"]
                .as_str()
                .unwrap()
                .contains("dispatched=1"),
            "detail was {:?}",
            row["data"]["detail"]
        );
        assert!(
            journal_lines(&project_journal)
                .iter()
                .any(|l| l.contains("active_backlog_dispatched")),
            "a dispatching tick must journal its dispatch"
        );
    }

    // ── synchronous children ────────────────────────────────────────

    #[test]
    fn synchronous_child_not_closed_trips_breaker_and_defers_once() {
        // A synchronous child with no completion, no defer and no PR ref scores
        // as a failure per tick through the SAME map_outcome policy, and trips
        // the breaker exactly once at failure_limit - the detached path's
        // behaviour.
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let record = tmp.path().join("record");
        let fno = stub_fno_advance_and_get(
            &tmp.path().join("bin"),
            &record,
            r#"{"epic_id":"x-epic","children":[{"node_id":"x-h","decision":"dispatched","substrate":"headless"}]}"#,
            r#"{"id":"x-h"}"#,
        );
        let cfg = test_cfg(tmp.path(), fno, 3);
        let (journal, project_journal) = test_journal(tmp.path());
        let mut breaker = CircuitBreaker::new(3);
        let mut pending = Vec::new();

        for _ in 0..3 {
            mission_drain_tick(&cfg, &mut breaker, &mut pending, &journal);
        }
        let defers = std::fs::read_to_string(&record)
            .unwrap()
            .lines()
            .filter(|l| l.contains("backlog defer x-h"))
            .count();
        assert_eq!(defers, 1, "exactly one defer at failure_limit=3");
        assert!(journal_lines(&project_journal)
            .iter()
            .any(|l| l.contains("active_backlog_parked")));
    }

    /// A stub `fno` for the converge-cap tests: every `backlog advance`
    /// (epic or loose) writes `S`, holds the slot for 300ms, writes `E`, then
    /// prints a benign receipt. The S/E log is the positive marker: overlap in
    /// it is concurrent converge runs measured directly, never a config read.
    /// The receipt itself is native now, so the fixture files carry it.
    fn stub_fno_converge(dir: &std::path::Path, log: &std::path::Path) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let p = crate::write_exec_stub(
            dir,
            "fno",
            &format!(
                "#!/usr/bin/env bash\n\
                 if [[ \"$1\" == backlog && \"$2\" == advance ]]; then \
                 echo S >> \"{log}\"\nsleep 0.3\necho E >> \"{log}\"\n\
                 printf '%s' '{{\"epic_id\":\"x-e\",\"deactivated\":false,\"all_done\":false,\"children\":[]}}'\nexit 0; fi\n\
                 exit 0\n",
                log = log.display()
            ),
        );
        p.display().to_string()
    }

    /// The greatest number of converge runs that overlapped, replayed from the
    /// stub's S/E log, plus how many ran at all. A cap enforced as a permanent
    /// block would show a low overlap AND a low run count, so both are read.
    fn peak_and_total(log: &std::path::Path) -> (usize, usize) {
        let mut running = 0usize;
        let mut peak = 0usize;
        let mut total = 0usize;
        // Raw marker log, not committed events: the stub's S/E lines are not
        // envelopes and never enter the store.
        let text = std::fs::read_to_string(log).unwrap_or_default();
        for line in text.lines() {
            match line.trim() {
                "S" => {
                    running += 1;
                    total += 1;
                    peak = peak.max(running);
                }
                "E" => running = running.saturating_sub(1),
                _ => {}
            }
        }
        (peak, total)
    }

    /// Run `missions` real [`mission_drain_loop`]s through one shared gate of
    /// `cap` permits for `run_ms`, then hand back the temp dir so a caller can
    /// read the journal it wrote.
    async fn drive_missions(
        tmp: &std::path::Path,
        cap: u32,
        missions: usize,
        run_ms: u64,
    ) -> String {
        std::env::set_var("HOME", tmp);
        std::env::set_var("FNO_HOME", tmp);
        std::env::set_var("FNO_AGENTS_HOME", tmp.join("agents-home"));
        std::env::set_var("FNO_CONFIG", tmp.join("config.toml"));
        let log = tmp.join("converges.log");
        // The receipt is native: N workspace projects with no teams resolve
        // to N leadless rung-1 territories rooted at the fixture dir.
        let mut projects = String::new();
        for i in 0..missions {
            projects.push_str(&format!(
                "[[work.workspaces.main.projects]]\nname = \"p{i}\"\npath = \"{}\"\n",
                tmp.display()
            ));
        }
        std::fs::write(
            tmp.join("config.toml"),
            format!(
                "[active_backlog]\nenabled = true\ninterval = \"1s\"\nfailure_limit = 3\nmax_concurrent = {cap}\n[work.workspaces.main]\n{projects}"
            ),
        )
        .unwrap();
        crate::graph_store::seed_rows(&tmp.join("graph.json"), &[]).unwrap();
        let registry = crate::paths::AgentsHome::from_env().registry_json();
        if let Some(parent) = registry.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(
            &registry,
            format!(
                "{{\"schema_version\": {}, \"agents\": []}}",
                crate::state::REGISTRY_SCHEMA_VERSION
            ),
        )
        .unwrap();
        let fno = stub_fno_converge(&tmp.join("bin"), &log);

        let resolved = resolve_targets(&std::env::current_dir().unwrap(), &registry);
        assert_eq!(
            resolved.len(),
            missions,
            "the fixture must resolve every mission"
        );
        let gate = Arc::new(ConvergeGate::new(cap));
        let shutdown = Arc::new(AtomicBool::new(false));
        let emitter = EventEmitter::new(tmp.join("emitter.jsonl"), "test");
        let mut handles = Vec::new();
        for target in resolved {
            handles.push(tokio::spawn(mission_drain_loop(
                target,
                fno.clone(),
                emitter.clone(),
                Arc::clone(&shutdown),
                Arc::clone(&gate),
            )));
        }
        tokio::time::sleep(Duration::from_millis(run_ms)).await;
        shutdown.store(true, Ordering::SeqCst);
        for h in handles {
            let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
        }
        log.display().to_string()
    }

    /// The cap is GLOBAL: three missions, one slot, one converge at a time.
    /// Asserted on overlapping converge runs across consecutive ticks, which is
    /// what five concurrent `advance --epic` children looked like in the field.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn one_slot_serializes_every_mission() {
        let _env = env_guard();
        let tmp = tempfile::TempDir::new().unwrap();
        let log = drive_missions(tmp.path(), 1, 3, 2500).await;
        let (peak, total) = peak_and_total(std::path::Path::new(&log));
        assert!(
            total >= 3,
            "every mission must get a turn, saw {total} converge(s)"
        );
        assert_eq!(
            peak, 1,
            "max_concurrent=1 must admit one converge at a time"
        );
    }
}
