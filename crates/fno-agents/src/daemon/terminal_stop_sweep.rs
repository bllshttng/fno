//! The terminal-stop sweep: every tick, spend the terminal-stop markers the
//! client left for bg sessions that reached end-of-turn. Extracted verbatim
//! from daemon.rs (shrink-only) beside its claude_stop arms; the wake-name
//! tombstone stamp rides the confirmed-stop write.

use super::claude_stop;
use super::claude_stop::end_survivors;
use super::update_registry_offloaded;
use crate::events::EventEmitter;
use crate::paths::AgentsHome;
use crate::state;
use serde_json::json;
use std::time::Duration;

pub(crate) async fn terminal_stop_sweep(home: &AgentsHome, emitter: &EventEmitter) {
    // read_markers (dir list + N file reads) and the roster load/parse are
    // blocking fs; run them off the async runtime so a slow disk or a large
    // marker dir never stalls a tokio worker thread. Returns the markers plus
    // the roster load result (an ERROR is kept distinct from a MISSING roster).
    let home_read = home.clone();
    let loaded = tokio::task::spawn_blocking(move || {
        let markers = crate::terminal_stop::read_markers(&home_read);
        if markers.is_empty() {
            return (markers, None);
        }
        let roster = crate::claude_roster::ClaudeRoster::load_default();
        (markers, Some(roster))
    })
    .await;
    let (markers, roster) = match loaded {
        Ok(v) => v,
        Err(e) => {
            eprintln!("daemon: terminal-stop sweep: read task failed: {e}");
            return;
        }
    };
    if markers.is_empty() {
        return;
    }
    // A load ERROR (e.g. a torn read while Claude rewrites roster.json, or a
    // future roster-format drift) must NOT be read as "session absent" — that
    // would delete every marker as stale and permanently leak the parked
    // workers this sweep exists to stop. Skip the tick and retry next time;
    // markers persist. A MISSING roster is a benign empty (Ok), correctly
    // yielding RemoveStale for a genuinely untracked session.
    let roster = match roster {
        Some(Ok(r)) => r,
        Some(Err(e)) => {
            eprintln!("daemon: terminal-stop sweep: roster load failed: {e} (retry next tick)");
            return;
        }
        None => return,
    };
    for marker in markers {
        // A pinned session stays up: the marker is spent, so an unpin later
        // hands the session to the registry sweep, not to a stop that fires
        // the moment the pin lifts. The row's lineage counts, as in every
        // other reaper.
        let keep = match crate::reap_release::load_registry_row(home, &marker.uuid) {
            Some(row) => crate::keep_mark::for_row(home, &row),
            None => crate::keep_mark::for_session(home, &marker.uuid),
        };
        if let Some(keep) = keep {
            eprintln!(
                "daemon: terminal-stop sweep: {} kept ({keep}); marker spent, no stop",
                marker.uuid
            );
            crate::terminal_stop::remove_marker(home, &marker.uuid);
            continue;
        }
        let short = roster.find(&marker.uuid).map(|w| w.short_id().to_string());
        match crate::terminal_stop::stop_decision(short) {
            crate::terminal_stop::StopAction::Stop(short) => {
                // Bound the subprocess so a hung `claude` can never wedge the
                // sweep. A timeout leaves the marker for the next tick, since
                // it is retried every tick, which is the failure this feature
                // exists to prevent.
                let stopped =
                    crate::lifecycle_child::bounded_claude_stop(&short, Duration::from_secs(15))
                        .await;
                match stopped {
                    // retired-ok: a daemon log line naming its own teardown call.
                    Err(_) => eprintln!("daemon: claude stop {short} timed out (retry next tick)"),
                    Ok(Ok(o)) if o.status.success() => {
                        // A stop exit is a receipt, not a proof. The marker is
                        // only spent on a proved end: a survivor keeps its
                        // marker, so the next tick retries instead of the row
                        // reading stopped over a live process. No proof at
                        // all (roster unreadable, no worker named) refuses
                        // the same way: no record, no marker spend.
                        match claude_stop::prove_target(&short, Some(marker.uuid.as_str())) {
                            Ok(members) => {
                                let (_signalled, survivors) = end_survivors(&members).await;
                                if !survivors.is_empty() {
                                    let listed = survivors
                                        .iter()
                                        .map(|pid| pid.to_string())
                                        .collect::<Vec<_>>()
                                        .join(", ");
                                    eprintln!(
                                        // retired-ok: a daemon log line naming its own teardown call.
                                        "daemon: terminal-stop sweep: claude stop {short} returned \
                                         but pid {listed} survived the signal; the marker stays for \
                                         the next tick. The override for a session claude's own \
                                         supervisor respawns is `fno agents rm`."
                                    );
                                    continue;
                                }
                            }
                            Err(reason) => {
                                eprintln!(
                                    "daemon: terminal-stop sweep: no process proof for \
                                     {short} ({reason}); the marker stays for the next tick"
                                );
                                continue;
                            }
                        }
                        let _ = emitter.emit(
                            "bg_worker_terminal_stopped",
                            &json!({
                                "short_id": short,
                                "session_id": marker.uuid,
                                "reason": marker.reason,
                            }),
                        );
                        // The row learns fno did this: the stamp keeps the
                        // sweep from reading the harness `stopped` state as
                        // finished work on a later tick. The write is
                        // offloaded like every other registry write on the
                        // async runtime.
                        record_sweep_stop(home, &marker.uuid, &marker.reason).await;
                        // The row left the live set; its receipt must not sit
                        // around teaching the gate a skip that no longer
                        // applies.
                        crate::terminal_run::remove_receipt(home, &marker.uuid);
                        crate::terminal_stop::remove_marker(home, &marker.uuid);
                    }
                    // Non-fatal: leave the marker so the next tick retries.
                    Ok(Ok(o)) => eprintln!(
                        // retired-ok: a daemon log line naming its own teardown call.
                        "daemon: claude stop {short} failed: {}",
                        String::from_utf8_lossy(&o.stderr).trim()
                    ),
                    Ok(Err(e)) => eprintln!("daemon: could not exec `claude stop`: {e}"),
                }
            }
            // The session already exited on its own (or a prior tick stopped it):
            // drop the stale marker so the dir does not grow without bound.
            crate::terminal_stop::StopAction::RemoveStale => {
                crate::terminal_stop::remove_marker(home, &marker.uuid);
            }
        }
    }
}

/// The registry-side half of a sweep stop: stamp the row terminal under
/// `terminal-sweep` and journal the `agent_stopped` evidence quiet recovery's
/// skip arm reads. Every other registry stop writes that evidence; a sweep
/// stop that skipped the journal left the worker reading merely dead to the
/// resume ladder. The session rides the payload, so the reader needs no
/// registry join. Best-effort end to end, like the sweep tick around it.
async fn record_sweep_stop(home: &AgentsHome, session: &str, reason: &str) {
    let journal_home = home.clone();
    let _ = update_registry_offloaded(home.registry_json(), {
        let session = session.to_string();
        let reason = reason.to_string();
        move |r| {
            if let Some(entry) = r
                .entries
                .iter_mut()
                .find(|e| e.harness_session_id.as_deref() == Some(session.as_str()))
            {
                state::record_stop(entry, "terminal-sweep", Some(reason));
                // best-effort: a failed stamp costs a later wake its name,
                // never the stop.
                let _ = EventEmitter::new(journal_home.events_jsonl(), "daemon").emit(
                    "agent_stopped",
                    &json!({"name": entry.name.clone(), "session_id": session,
                        "reason": "terminal-sweep"}),
                );
                crate::wake_name::stamp_from_row(&journal_home, entry);
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[tokio::test]
    async fn a_sweep_stop_journals_agent_stopped_with_the_session() {
        let home = crate::daemon::tests::short_home("sweepstop");
        let mut row = crate::state::RegistryEntry::default();
        row.name = "t-swept".into();
        row.harness = Some("claude".into());
        row.harness_session_id = Some("d55e1111-1111-2222-3333-444444444444".into());
        row.created_at = "2020-01-01T00:00:00Z".into();
        state::update_registry(&home.registry_json(), |registry| registry.entries.push(row))
            .unwrap();

        record_sweep_stop(&home, "d55e1111-1111-2222-3333-444444444444", "pane-closed").await;

        let evidence =
            crate::watch_expiry::read_evidence(&home, chrono::Utc::now().timestamp_millis())
                .unwrap();
        let stopped: Vec<_> = evidence
            .iter()
            .filter(|e| e.kind == "agent_stopped")
            .map(|e| {
                (
                    e.data.get("name").and_then(Value::as_str),
                    e.data.get("session_id").and_then(Value::as_str),
                    e.data.get("reason").and_then(Value::as_str),
                )
            })
            .collect();
        assert_eq!(
            stopped,
            vec![(
                Some("t-swept"),
                Some("d55e1111-1111-2222-3333-444444444444"),
                Some("terminal-sweep")
            )],
            "the sweep stop is journaled where the skip arm reads"
        );
        let registry = state::load_registry(&home.registry_json()).unwrap();
        assert_eq!(
            registry.entries[0].stop.as_ref().map(|s| s.by.as_str()),
            Some("terminal-sweep"),
            "the row carries its stop record"
        );
        std::fs::remove_dir_all(home.root().parent().unwrap()).ok();
    }
}
