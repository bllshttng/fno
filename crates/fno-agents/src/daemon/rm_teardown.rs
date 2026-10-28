//! The process-end legs `fno agents rm` owns since (law d-81c6da7e:
//! remove needs no prior stop).
//!
//! Two legs moved out of daemon.rs so the lifecycle commands share them: the
//! codex thread's interrupt-settle-shutdown-actor-drop (shared with the stop
//! command, which stamps and emits around it) and the claude background
//! thread's bounded stop, which rm now runs itself before it re-reads the
//! roster.

use super::{off_executor, Ctx, InterruptOutcome, Request, Response};
use crate::codex_thread::stop_settle_bound;
use crate::state::RegistryEntry;
use crate::AgentStatus;

/// End a codex thread CONFIRMABLY: when the daemon map holds no handle, the
/// thread itself still lives in the shared app-server (a restart, or a
/// recovery stamp, emptied the map, never the app-server), so ending it
/// without a re-attach answers "no-turn" against nothing and the turn keeps
/// running there. The re-attach rides the row's durable identity (resume by
/// session id + cwd); a refused re-attach is the caller's signal to keep the
/// row addressable rather than report a teardown that never touched the
/// process.
pub(crate) async fn end_codex_thread_confirmed(
    ctx: &Ctx,
    entry: &RegistryEntry,
) -> Result<String, String> {
    if !ctx.codex_threads.lock().await.contains_key(&entry.name) {
        super::codex_thread_resume::ensure_codex_thread_handle(ctx, entry)
            .await
            .map_err(|reason| {
                format!("thread not hosted here and the re-attach refused: {reason}")
            })?;
    }
    end_codex_thread(ctx, &entry.name).await
}

/// Interrupt the codex thread's in-flight turn, shut the actor down, and
/// drop it from the map. Shared by the stop command and rm: the caller that
/// keeps the row stamps and emits around the answer.
///
/// `Ok(report)` names the settled outcome (`no-turn`, `actor-gone`, an
/// interrupted-turn receipt status) and leaves a torn-down thread behind:
/// the caller may proceed to drop the row. `Err(report)` says the turn is
/// still running: the handle stays in the map, and the caller keeps the
/// row non-terminal, or refuses, for rm.
pub(crate) async fn end_codex_thread(ctx: &Ctx, name: &str) -> Result<String, String> {
    let handle = ctx.codex_threads.lock().await.get(name).cloned();
    let mut interrupt_report = "no-turn".to_string();
    let mut settled = true;
    if let Some(handle) = handle.as_ref() {
        let outcome = match tokio::time::timeout(stop_settle_bound(), handle.interrupt()).await {
            Ok(Ok(InterruptOutcome::NoTurnInFlight)) => "no-turn".to_string(),
            Ok(Ok(InterruptOutcome::Interrupted(receipt))) => receipt.status,
            Ok(Ok(InterruptOutcome::Timeout)) => {
                settled = false;
                "timeout-turn-still-running".to_string()
            }
            Ok(Err(_)) => {
                // The only Err interrupt() produces is a dead actor task
                // (send fails or the ack sender was dropped): no turn can
                // still be running and the interrupt handle died with it,
                // so teardown is already done.
                "actor-gone".to_string()
            }
            Err(_) => {
                settled = false;
                "interrupt-failed-turn-still-running: stop exchange timed out".to_string()
            }
        };
        interrupt_report = outcome;
        if settled {
            let _ = handle.shutdown().await;
        }
    }
    if !settled {
        return Err(interrupt_report);
    }
    ctx.codex_threads.lock().await.remove(name);
    Ok(interrupt_report)
}

/// rm's codex-thread arm: end the thread, then refuse unless `--force` was
/// passed. `Some` carries the Busy refusal text that keeps the row; `None`
/// means rm proceeds to drop the row (teardown done, or the force override).
/// The entry is the teardown's identity source: an unmapped thread re-attaches
/// through its row before the interrupt, so a live thread cannot be rm'd
/// while its turn keeps running in the shared app-server.
pub(crate) async fn codex_rm_refusal(
    ctx: &Ctx,
    entry: &RegistryEntry,
    force: bool,
) -> Option<String> {
    let name = entry.name.clone();
    if let Err(interrupt_report) = end_codex_thread_confirmed(ctx, entry).await {
        if !force {
            return Some(format!(
                "agent {name}: the codex thread did not confirm teardown \
                 ({interrupt_report}); the registry row and the codex index \
                 entry are kept"
            ));
        }
        // The row drops without a settled teardown, so the map entry goes
        // with it: ensure_codex_thread_handle hands a respawned row the
        // stale actor when the name matches.
        ctx.codex_threads.lock().await.remove(&name);
    }
    None
}

/// What rm's claude stop leg learned. `Asked` is the old receipt: the
/// bounded ask ran and its exit decided nothing beyond itself. `Confirmed`
/// means fno proved the process set from the daemon roster, asked, and
/// ended every member: the death is fno's own measurement, so the verdict
/// below needs no vendor word.
pub(crate) enum StopLeg {
    Asked,
    Confirmed,
}

/// The production stop leg behind `handle_rm_with`'s seam. The process set
/// is captured BEFORE the ask (the stop command's own ordering, so the
/// start times are the pre-ask incarnations and a pid respawned inside the
/// window takes no signal), the bounded ask runs, and whatever survived it
/// is ended here. The escalation exists because the ask alone cannot be
/// trusted to end the worker: on a glm (zai-route) thread `claude stop`
/// returned without ending the session and the listing row never flipped
/// terminal, so every rm refused and the seat stayed charged (three
/// refusals on 2026-10-02). No proof (an unreadable roster, or no
/// roster worker names the session - an isolated-account root the default
/// roster never carries) degrades to the ask-only receipt. Blocking; the
/// caller hops through `off_executor`, whose blocking-permitted context a
/// fresh current-thread runtime is legal inside.
pub(crate) fn confirmed_claude_stop(short: &str, session_id: Option<&str>) -> StopLeg {
    // The seam runs under `off_executor`, so the proof takes the blocking
    // variant: re-entering `off_executor` from blocking context is the
    // nesting this leg must not do.
    let members = super::claude_stop::prove_target_blocking(short, session_id);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map(|rt| {
            rt.block_on(async {
                let _ = crate::lifecycle_child::bounded_claude_stop(
                    short,
                    std::time::Duration::from_secs(15),
                )
                .await;
                match members {
                    Ok(members) => {
                        let (_, survivors) = super::claude_stop::end_survivors(&members).await;
                        if survivors.is_empty() {
                            StopLeg::Confirmed
                        } else {
                            StopLeg::Asked
                        }
                    }
                    Err(_) => StopLeg::Asked,
                }
            })
        })
        .unwrap_or(StopLeg::Asked)
}

/// The stop verdict `run_stop_leg` hands back: whether the row is now
/// provably gone, and the post-stop listing snapshot the caller's live gate
/// recomputed on.
pub(crate) struct StopLegResult {
    pub(crate) provably_gone: bool,
    pub(crate) snapshot: crate::claude_roster::ClaudeAgentsSnapshot,
}

/// The stop leg `fno agents rm` runs for a stored-live claude background
/// thread (law d-81c6da7e: rm runs the one exception's stop itself), moved
/// out of daemon.rs under the file budget with the glm escalation, gate
/// and short-id resolution included. `Ok(None)` is the gate answering no.
/// A confirmed end marks the registry row stopped - the row's memory the
/// reachability gate reads - and flips the caller's entry, so the recomputed
/// fno-first death verdict reads Finished without waiting for a vendor word
/// the zai provider never writes. `Err` is a failed registry write surfaced
/// to the caller, never a clean stop (silent-failure review).
pub(crate) async fn run_stop_leg(
    ctx: &Ctx,
    req: &Request,
    entry: &mut RegistryEntry,
    force: bool,
    provably_gone: bool,
    claude_stop: &(dyn Fn(&str, Option<&str>) -> StopLeg + Sync),
    read_claude_agents: &(dyn Fn() -> crate::claude_roster::ClaudeAgentsSnapshot + Sync),
    harness_row_id: Option<&str>,
) -> Result<Option<StopLegResult>, Response> {
    if entry.status != AgentStatus::Live
        || force
        || provably_gone
        || !crate::gc_native::stop_precedes_removal(entry)
    {
        return Ok(None);
    }
    let short = entry.transport_short().map(str::to_string).or_else(|| {
        entry
            .harness_session_id
            .clone()
            .filter(|session_id| !session_id.trim().is_empty())
    })?;
    let name = entry.name.clone();
    let session_id = entry.harness_session_id.clone();
    let leg = off_executor(move || claude_stop(&short, session_id.as_deref()));
    if let StopLeg::Confirmed = leg {
        if let Err(response) = super::claude_stop::mark_claude_stopped(ctx, req, &name).await {
            return Err(response);
        }
        entry.status = AgentStatus::Exited;
    }
    let snapshot = off_executor(read_claude_agents);
    let provably_gone = super::roster_death::row_death_reason(entry, &snapshot).is_some()
        || super::roster_death::claude_row_provably_absent(Some(&snapshot), harness_row_id);
    Ok(Some(StopLegResult {
        provably_gone,
        snapshot,
    }))
}

/// Stamp the tombstone BEFORE the registry row is published absent, so a
/// resolver racing this rm can never observe row-gone-but-no-tombstone and
/// heal the session back under a fresh short-id name (the adopted duplicate
/// that then blocked resume). Any harness: the store fallback adopts claude
/// transcripts by the same door. A failed write rides the rm receipt
/// (`tombstone_reason`), never refuses the removal. `None` when nothing was
/// stamped (no session id) or the write landed; `Some` carries the error.
pub(crate) fn stamp_removed_session_tombstone(
    home: &crate::paths::AgentsHome,
    entry: &RegistryEntry,
) -> Option<String> {
    let session_id = entry
        .harness_session_id
        .as_deref()
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())?;
    crate::rm_tombstone::record(
        home,
        entry.harness_name(),
        session_id,
        entry.short_id.as_str(),
        &entry.name,
        &entry.cwd,
        entry.host_mode.as_deref().unwrap_or_default(),
    )
    .err()?;
    // The wake-identity tombstone rides the same stamp: a removed row can
    // be woken later, and its identity must survive the drop.
    crate::wake_name::stamp_from_row(home, entry);
    None
}
