//! The blueprint-stall lane: the machine guard for the 2026-10-10 finding
//! that codex blueprint workers pause or loop in blueprint verification for
//! hours, and only a lead peeking every 15 minutes was the guard. Per codex
//! native-thread worker row it decides: stall evidence holds, it does not,
//! or the row already fired and is terminal.
//!
//! Two evidence arms, either one enough:
//! - verify-loop: the transcript tail carries `validate-plan.sh` calls
//!   spanning the 30-minute stall bound, newest inside the fresh window,
//!   and no commit past the base in the row's checkout. The skill caps
//!   verification at two runs, so a loop that long is off-protocol by
//!   construction, and the loop keeps the transcript moving, which is why
//!   the quiet ladder never sees this shape.
//! - goal-paused: the thread's native goal reads `Paused`. Nothing in fno
//!   pauses a worker's goal (only a teamed lead parks its own), so a paused
//!   worker goal is a stall. The 30-minute hold is episode time: the pause
//!   must hold across the bound before the arm fires.
//!
//! One fire: a single nudge and one lead mail, each carrying the
//! reseat-on-claude remedy. The episode is terminal once the lead mail
//! lands: the remedy is a respawn under a new session id, which re-arms
//! the lane on its new row. Evidence clearing before the fire only resets
//! the observation window; once the lead owns it, this lane does not re-ask.

use crate::paths::AgentsHome;
use crate::state::RegistryEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::Path;

const SENDER: &str = "fno/blueprint-stall";
/// The operator's bound: blueprint verification past this is a stall.
const STALL_AFTER_S: i64 = 1800;
/// The quiet lane's own activity window: a verify loop whose newest marker
/// is older than this has ended, and the quiet ladder owns the worker.
const FRESH_S: i64 = 900;
const VERIFY_MARKER: &str = "validate-plan.sh";

#[derive(Default, Serialize, Deserialize)]
struct Episode {
    first_seen: i64,
    nudged: bool,
    lead_notified: bool,
}

/// One arm's held evidence: what fired and the one-line basis the lead mail
/// and the event journal both carry.
struct Stall {
    kind: &'static str,
    basis: String,
}

/// `(first, last)` timestamps of the verification marker in a rollout tail.
/// `None` is no marker, not "too old": absence never fires an arm.
fn verify_span(text: &str, skip_first_line: bool) -> Option<(i64, i64)> {
    let mut span: Option<(i64, i64)> = None;
    for line in text.lines().skip(usize::from(skip_first_line)) {
        if !line.contains(VERIFY_MARKER) {
            continue;
        }
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Only a tool CALL carries the marker: prompt and message rows name
        // validate-plan.sh too (the skill text rides the seed), and a
        // mention is not a run.
        let shape = row
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !matches!(shape, "custom_tool_call" | "function_call") {
            continue;
        }
        let Some(at) = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp())
        else {
            continue;
        };
        span = Some(match span {
            None => (at, at),
            Some((first, last)) => (first, at.max(last)),
        });
    }
    span
}

/// The held stall evidence for one row, or `None` while no arm holds. The
/// goal arm's duration is episode time and lives in `fire_due`.
fn evidence(
    span: Option<(i64, i64)>,
    commits: Option<i64>,
    goal_paused: bool,
    now: i64,
) -> Option<Stall> {
    if let Some((first, last)) = span {
        if last - first >= STALL_AFTER_S && now - last <= FRESH_S && commits == Some(0) {
            return Some(Stall {
                kind: "verify-loop",
                basis: format!(
                    "verification looped {VERIFY_MARKER} {} min, no commit past base",
                    (last - first) / 60
                ),
            });
        }
    }
    if goal_paused {
        return Some(Stall {
            kind: "goal-paused",
            basis: "native goal reads paused".to_string(),
        });
    }
    None
}

/// Whether the held evidence has earned its fire: the verify loop carries
/// its own duration proof, the paused goal must hold across the bound.
fn fire_due(episode: &Episode, stall: &Stall, now: i64) -> bool {
    match stall.kind {
        "verify-loop" => true,
        "goal-paused" => episode.first_seen > 0 && now - episode.first_seen >= STALL_AFTER_S,
        _ => false,
    }
}

/// The bounded tail read both lanes see the transcript through.
fn tail_text(path: &Path) -> Option<(String, bool)> {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_BYTES: u64 = 256 * 1024;
    let mut file = std::fs::File::open(path).ok()?;
    let size = file.metadata().ok()?.len();
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    Some((
        String::from_utf8_lossy(&bytes).into_owned(),
        size > TAIL_BYTES,
    ))
}

/// Commits past the base in the row's own checkout, the burn arm's
/// first-edit read. A checkout git cannot read is never evidence.
fn commits_past_base(cwd: &str) -> Option<i64> {
    let base = crate::burn_watch::run_command(
        &[
            "git".into(),
            "rev-parse".into(),
            "--abbrev-ref".into(),
            "origin/HEAD".into(),
        ],
        cwd,
    )
    .1
    .lines()
    .rev()
    .find(|l| !l.trim().is_empty())?
    .trim()
    .to_string();
    crate::burn_watch::run_command(
        &[
            "git".into(),
            "rev-list".into(),
            "--count".into(),
            format!("{base}..HEAD"),
        ],
        cwd,
    )
    .1
    .lines()
    .rev()
    .find(|l| !l.trim().is_empty())
    .and_then(|l| l.trim().parse().ok())
}

/// The row is this session's own live worker claim, the same rule the quiet
/// lane applies: another session's live or suspect claim is not ours to
/// drive, and a corrupted claim holds everything.
fn own_live_claim(sid: &str, node: &str) -> bool {
    let (state, claim) = crate::claims::status(&format!("node:{node}"), None);
    if state == crate::claims::ClaimState::Corrupted {
        return false;
    }
    if matches!(
        state,
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
    ) {
        return claim.as_ref().is_some_and(|c| {
            c.session_id.as_deref() == Some(sid) || c.holder == format!("target-session:{sid}")
        });
    }
    true
}

/// One `fno agents mail send` through the scheduler sender: `to` is a
/// session id, or `--to-lead <scope>` when `to` is a lead scope.
fn mail(to: Option<&str>, lead_scope: Option<&str>, text: &str, cwd: &str) -> bool {
    let mut args = vec![
        "fno".to_string(),
        "agents".to_string(),
        "mail".to_string(),
        "send".to_string(),
        "--from-name".to_string(),
        SENDER.to_string(),
        "--origin".to_string(),
        "scheduler".to_string(),
    ];
    if let Some(scope) = lead_scope {
        args.push("--to-lead".to_string());
        args.push(scope.to_string());
    }
    if let Some(sid) = to {
        args.push(sid.to_string());
    }
    args.push(text.to_string());
    let (code, out, _error) = crate::burn_watch::run_command(&args, cwd);
    crate::mail_inject::mail_send_accepted(code, &out)
}

/// The worker nudge: one mail to the stalled worker's session.
fn nudge(entry: &RegistryEntry, sid: &str, node: &str, stall: &Stall) -> bool {
    let text = format!(
        "control: blueprint stall on {node}: finish or emit <help>; the lead may reseat this work on claude. [{basis}]",
        basis = stall.basis
    );
    mail(Some(sid), None, &text, &entry.cwd)
}

/// The lead mail: the reseat-on-claude line is the remedy this lane exists
/// to deliver, so the text carries it verbatim.
fn mail_lead(entry: &RegistryEntry, scope: &str, node: &str, stall: &Stall) -> bool {
    let text = format!(
        "control: <help class=\"stuck\" reason=\"blueprint-stall\">{node}: codex worker {} ({}) {} in {}. Remedy: reseat on claude - stop the codex worker and respawn the node's work with -H claude. Nudged once.</help>",
        entry.name, entry.harness_session_id.as_deref().unwrap_or("?"), stall.basis, entry.cwd
    );
    mail(None, Some(scope), &text, &entry.cwd)
}

/// The episode file under `<agents home>/blueprint-stall/`: one json per
/// session id, written atomically like the quiet lane's episode state.
fn save_episode(dir: &Path, sid: &str, episode: &Episode) -> Result<(), String> {
    let bytes = serde_json::to_vec(episode).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{sid}.tmp"));
    std::fs::write(&tmp, bytes)
        .and_then(|_| std::fs::rename(tmp, dir.join(format!("{sid}.json"))))
        .map_err(|e| e.to_string())
}

pub(super) fn run(home: &AgentsHome) -> Result<(), String> {
    let registry = crate::state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let rows = crate::graph_store::read_rows_where(
        &crate::gc_sweep::graph_path(home),
        &crate::backlog::RowQuery {
            fields: Some(
                ["id", "status", "merge_status", "superseded_by", "type"]
                    .into_iter()
                    .map(str::to_string)
                    .collect(),
            ),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let watches =
        crate::watch_expiry::read_evidence(home, now * 1000).map_err(|e| e.to_string())?;
    let stopped: std::collections::HashSet<_> = watches
        .iter()
        .filter(|e| e.kind == "agent_stopped")
        .filter_map(|e| e.session_id.as_deref())
        .collect();
    let settle_watches = super::settle_watches(&watches);
    let dir = home.root().join("blueprint-stall");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let mut owners = None;
    for entry in &registry.entries {
        let result = (|| -> Result<(), String> {
            let Some(sid) = entry
                .harness_session_id
                .as_deref()
                .filter(|s| crate::resume_wake::is_uuid_shaped(s))
            else {
                return Ok(());
            };
            if stopped.contains(sid) {
                return Ok(());
            }
            if settle_watches.iter().any(|w| {
                (w.watch.session_id == sid || w.codex_thread_id.as_deref() == Some(sid))
                    && super::quiet::watch_shielded(&w.watch, &watches, now * 1000)
            }) {
                return Ok(());
            }
            // Population: native codex thread WORKER rows. Mux rows have a
            // pane owner, titled roles own their own lanes, and a lead's
            // parked goal is the lead loop's deliberate quiet park.
            if entry.harness_name() != "codex" || entry.mux.is_some() || entry.role_level.is_some()
            {
                return Ok(());
            }
            let Some(node) = entry.node.as_deref() else {
                return Ok(());
            };
            let Some(row) = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(node))
            else {
                return Ok(());
            };
            if row.get("status").and_then(Value::as_str) == Some("deferred")
                || super::quiet::closed(row).is_some()
            {
                return Ok(());
            }
            if !own_live_claim(sid, node) {
                return Ok(());
            }
            let dead = matches!(
                entry.status,
                crate::AgentStatus::Failed
                    | crate::AgentStatus::Exited
                    | crate::AgentStatus::PermanentDead
            ) || entry.pid.is_some_and(|pid| crate::daemon::pid_is_gone(pid));
            if dead {
                return Ok(());
            }
            // A goal read that fails or answers no goal is never evidence.
            let goal_paused = entry
                .harness_session_id
                .as_deref()
                .and_then(|sid| crate::lead_goal::read_codex_goal_for_stop(sid).ok())
                .flatten()
                .is_some_and(|goal| goal.status == crate::codex_thread::GoalStatus::Paused);
            let mut episode: Episode = match std::fs::read(dir.join(format!("{sid}.json"))) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Episode::default(),
                Err(e) => return Err(e.to_string()),
            };
            let phase_blueprint =
                crate::spawn_phase::seed_phase(entry.spawn_payload.as_deref().unwrap_or(""))
                    == Some("blueprint");
            let stall = if phase_blueprint {
                let path = entry
                    .transcript_path
                    .as_deref()
                    .filter(|p| Path::new(p).is_file());
                let (span, commits) = match path.and_then(|p| tail_text(Path::new(p))) {
                    Some((text, skip)) => (verify_span(&text, skip), commits_past_base(&entry.cwd)),
                    None => (None, None),
                };
                evidence(span, commits, goal_paused, now)
            } else if goal_paused {
                Some(Stall {
                    kind: "goal-paused",
                    basis: "native goal reads paused".to_string(),
                })
            } else {
                None
            };
            let Some(stall) = stall else {
                // Never fired and nothing observed: drop the episode file.
                if episode.first_seen == 0 && !episode.nudged {
                    let _ = std::fs::remove_file(dir.join(format!("{sid}.json")));
                    return Ok(());
                }
                // Observed then cleared before the fire: reset the window.
                if !episode.nudged {
                    episode.first_seen = 0;
                    save_episode(&dir, sid, &episode)?;
                }
                return Ok(());
            };
            let fresh_episode = episode.first_seen == 0;
            if fresh_episode {
                episode.first_seen = now;
            }
            if !fire_due(&episode, &stall, now) {
                save_episode(&dir, sid, &episode)?;
                if fresh_episode {
                    emitter
                        .emit(
                            "blueprint_stall",
                            &json!({"session_id": sid, "node": node, "kind": stall.kind,
                                "basis": stall.basis, "stage": "observed"}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            if !episode.nudged {
                episode.nudged = nudge(entry, sid, node, &stall);
                if episode.nudged {
                    emitter
                        .emit(
                            "blueprint_stall",
                            &json!({"session_id": sid, "node": node, "kind": stall.kind,
                                "basis": stall.basis, "stage": "nudged"}),
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            if !episode.lead_notified {
                if let Some(scope) = crate::burn_watch::node_owner(
                    Path::new(&entry.cwd),
                    &home.registry_json(),
                    &rows,
                    &mut owners,
                    node,
                ) {
                    episode.lead_notified = mail_lead(entry, &scope, node, &stall);
                    if episode.lead_notified {
                        emitter
                            .emit(
                                "blueprint_stall",
                                &json!({"session_id": sid, "node": node, "kind": stall.kind,
                                    "basis": stall.basis, "stage": "lead-notified", "lead": scope}),
                            )
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            save_episode(&dir, sid, &episode)?;
            Ok(())
        })();
        if let Err(error) = result {
            emitter
                .emit(
                    "blueprint_stall_error",
                    &json!({"name": entry.name, "reason": error}),
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stall_of(kind: &str) -> Stall {
        Stall {
            kind,
            basis: "basis".into(),
        }
    }

    #[test]
    fn verify_loop_needs_span_freshness_and_an_empty_checkout() {
        let now = 10_000;
        let stall = evidence(Some((now - 2400, now - 300)), Some(0), false, now);
        assert!(stall.is_some(), "40 min loop, fresh, no commits");
        assert_eq!(stall.unwrap().kind, "verify-loop");
        assert!(
            evidence(Some((now - 600, now - 300)), Some(0), false, now).is_none(),
            "a 10-minute span is protocol (two runs)"
        );
        assert!(
            evidence(Some((now - 4800, now - 3600)), Some(0), false, now).is_none(),
            "a loop that ended an hour ago is the quiet ladder's case"
        );
        assert!(
            evidence(Some((now - 2400, 300)), Some(0), false, now).is_none(),
            "the newest marker must sit inside the fresh window"
        );
        assert!(
            evidence(Some((now - 2400, now - 300)), Some(3), false, now).is_none(),
            "commits past the base are progress: the deliverable moved"
        );
    }

    #[test]
    fn goal_pause_holds_across_the_bound_before_it_fires() {
        let now = 10_000;
        let paused = stall_of("goal-paused");
        let mut episode = Episode::default();
        // First sight: the window opens, nothing fires yet.
        assert!(!fire_due(&episode, &paused, now));
        // The pause held across the 30-minute bound.
        episode.first_seen = now - STALL_AFTER_S;
        assert!(fire_due(&episode, &paused, now));
        // The verify-loop arm needs no episode time: the transcript span
        // IS the duration proof.
        assert!(fire_due(&episode, &stall_of("verify-loop"), now));
    }

    #[test]
    fn marker_span_reads_a_fixture_tail_and_refuses_untimestamped_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(
            &path,
            concat!(
                "{\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\"}}\n",
                "{\"timestamp\":\"2026-01-01T00:40:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"input\":\"bash validate-plan.sh plan.md\"}}\n",
                "{\"timestamp\":\"2026-01-01T01:15:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"input\":\"echo hi\"}}\n",
                "{\"timestamp\":\"2026-01-01T01:15:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"input\":\"bash validate-plan.sh plan.md\"}}\n"
            ),
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let span = verify_span(&text, false).unwrap();
        assert_eq!(span.0, 1767228000);
        assert_eq!(span.1, 1767230100);
        assert_eq!((span.1 - span.0) / 60, 35);
        // A marker line with no timestamp never poisons the span.
        std::fs::write(
            &path,
            "{\"type\":\"response_item\",\"payload\":{\"type\":\"custom_tool_call\",\"input\":\"validate-plan.sh\"}}\n",
        )
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(verify_span(&text, false).is_none());
    }
}
