//! Read-confirmed recovery for unfinished workers. Runs under the settle arm's
//! existing cadence and one-in-flight gate; a quiet transcript is not death.

use crate::paths::AgentsHome;
use crate::state::RegistryEntry;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SENDER: &str = "fno/quiet-recovery";
const TAIL_BYTES: u64 = 256 * 1024;

/// A parked session stays shielded for one quiet interval past its watch's
/// ring: the moment its alarm last went off. The ring is the bound itself,
/// or an earlier wake row's stamp when the watch woke ahead of its bound.
/// The worker then needs that interval to wake, act and re-arm (or proceed)
/// before its silence reads as stuck. The shield still requires the watch
/// to be the session's current one, so a superseded watch releases the seat
/// at once; the wake row is not superseding evidence, so it never lifts the
/// shield, and a re-arm supersedes the row inside `settle_watches`. Without
/// the linger, every watch bound evaluated the ladder against a pre-watch
/// activity stamp, flagged the healthy parked worker stuck, and each failed
/// resume salvaged a recovery branch.
const WATCH_LINGER_MS: i64 = 900 * 1000;

fn watch_shielded(
    watch: &crate::watch_expiry::Watch,
    evidence: &[crate::watch_expiry::Evidence],
    now_ms: i64,
) -> bool {
    let ring = wake_ring(watch, evidence)
        .unwrap_or(watch.expires_at_ms)
        .min(watch.expires_at_ms);
    now_ms < ring + WATCH_LINGER_MS && crate::watch_expiry::is_current_watch(watch, evidence)
}

/// The newest wake row stamped for this watch, when one exists. The same
/// identity `woken_already` matches: session plus `watch_event_id`.
fn wake_ring(
    watch: &crate::watch_expiry::Watch,
    evidence: &[crate::watch_expiry::Evidence],
) -> Option<i64> {
    evidence
        .iter()
        .filter(|row| {
            row.session_id.as_deref() == Some(watch.session_id.as_str())
                && row.kind == crate::watch_expiry::WAKE_EVENT
                && row.data.get("watch_event_id").and_then(Value::as_str)
                    == Some(watch.event_id.as_str())
        })
        .map(|row| row.ts_ms)
        .max()
}

#[derive(Default, Serialize, Deserialize)]
struct Episode {
    activity: i64,
    read_stage: u8,
    pending: Option<(String, i64, u8)>,
    escalated: bool,
    #[serde(default)]
    recovered: bool,
    #[serde(default)]
    recovery_reason: Option<String>,
}

#[derive(Default)]
struct Tail {
    activity: i64,
    started_at: Option<i64>,
    failed: bool,
    read: bool,
    touched: i64,
}

fn tail(path: &Path, marker: Option<&str>) -> Option<Tail> {
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let size = meta.len();
    let touched = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs() as i64);
    file.seek(SeekFrom::Start(size.saturating_sub(TAIL_BYTES)))
        .ok()?;
    let mut bytes = Vec::new();
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Tail {
        touched,
        ..Default::default()
    };
    for line in text.lines().skip(usize::from(size > TAIL_BYTES)) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if marker.is_some_and(|marker| line.contains(marker)) {
            out.read = true;
            continue;
        }
        let payload = row.get("payload").unwrap_or(&row);
        let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
        let event = payload.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = row
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|t| t.timestamp());
        if event == "task_started" {
            out.failed = false;
            out.started_at = timestamp.or(out.started_at);
        }
        if event == "task_complete" {
            out.failed = payload.get("error").is_some_and(|e| !e.is_null());
        }
        if matches!(kind, "response_item" | "assistant" | "user" | "tool")
            || matches!(event, "item_completed" | "task_started" | "task_complete")
        {
            if event != "task_complete" {
                out.failed = false;
            }
            if let Some(at) = timestamp {
                out.activity = out.activity.max(at);
            }
        }
    }
    (out.activity > 0 || out.started_at.is_some() || out.read || out.failed || out.touched > 0)
        .then_some(out)
}

#[derive(Debug, PartialEq)]
enum Action {
    Wait,
    Nudge(u8),
    Recover,
    Help,
    /// The node closed under an idle worker: stop it so the seat frees.
    Stop(&'static str),
}

/// The reason a node is closed to its worker, or `None` while work remains.
/// Deferred stays a silent skip: a parked node may come back to the same seat.
fn closed(row: &Value) -> Option<&'static str> {
    let field = |k: &str| row.get(k).and_then(Value::as_str);
    match field("status") {
        Some("done") => Some("done"),
        Some("superseded") => Some("superseded"),
        _ if field("superseded_by").is_some_and(|s| !s.is_empty()) => Some("superseded"),
        _ if field("merge_status") == Some("merged") => Some("merged"),
        _ => None,
    }
}

/// On a closed node every move past `Wait` would resume or nudge a worker
/// into work that is already over, so it becomes a stop. A worker still
/// writing its transcript reads `Wait` and is left to finish.
fn on_closed(decision: Action, closed: Option<&'static str>) -> Action {
    match (closed, decision) {
        (_, Action::Wait) => Action::Wait,
        (Some(why), _) => Action::Stop(why),
        (None, decision) => decision,
    }
}

/// The skip a deliberately-stopped row records when its node also delivered:
/// the revival rule's "record a skip reason instead". `None` proceeds into
/// the ladder - an open node's revival stands.
fn stopped_skip_reason(closed: Option<&'static str>) -> Option<String> {
    closed.map(|why| format!("worker stopped on purpose; node {why}; never revived"))
}

fn action(episode: &mut Episode, tail: &Tail, now: i64, live: bool, dead: bool) -> Action {
    if episode.activity == 0 {
        episode.activity = tail.started_at.unwrap_or_default();
    }
    if tail.activity > episode.activity {
        *episode = Episode {
            activity: tail.activity,
            ..Default::default()
        };
    }
    if episode.escalated {
        return Action::Wait;
    }
    // A pid probe can read a working worker as gone (a launcher pid, a tool
    // call in flight). A live own claim or a transcript written inside the
    // first nudge window outranks it: no resume, no salvage, no help.
    let fresh = now.saturating_sub(tail.activity.max(tail.touched)) < 900;
    if dead && !live && !fresh {
        return if episode.recovered {
            Action::Help
        } else {
            Action::Recover
        };
    }
    if let Some((_, _, stage)) = episode.pending.as_ref() {
        if tail.read {
            episode.read_stage = *stage;
            episode.pending = None;
        } else if fresh {
            return Action::Wait;
        } else {
            return if episode.recovered {
                Action::Help
            } else {
                Action::Recover
            };
        }
    }
    if !live {
        return Action::Wait;
    }
    if episode.activity == 0 && !tail.failed {
        return Action::Wait;
    }
    if tail.failed && episode.read_stage == 0 {
        return Action::Nudge(1);
    }
    let quiet = now.saturating_sub(episode.activity);
    if quiet >= 2700 && episode.read_stage >= 2 {
        Action::Help
    } else if quiet >= 1800 && episode.read_stage == 1 {
        Action::Nudge(2)
    } else if quiet >= 900 && episode.read_stage == 0 {
        Action::Nudge(1)
    } else {
        Action::Wait
    }
}

fn command(args: &[&str], cwd: &str) -> (i32, String, String) {
    crate::burn_watch::run_command(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>(), cwd)
}

/// `{out} {error}` condensed to its last non-empty line - stderr's when
/// stderr carries one, else stdout's - so a Python traceback's dozens of
/// `File "..."` frames never reach a help reason, only its final line.
fn condensed(out: &str, error: &str) -> String {
    let from_error = crate::attention_arm::last_line(error);
    if !from_error.is_empty() {
        from_error
    } else {
        crate::attention_arm::last_line(out)
    }
}

/// The thread resume argv `restore` runs with no session identity of its
/// own: named `fno/quiet-recovery` so the mail envelope's sender guard sees
/// a real arm, not a bare `fno`.
fn resume_argv<'a>(sid: &'a str, text: &'a str) -> [&'a str; 9] {
    [
        "fno",
        "agents",
        "session",
        "resume",
        sid,
        "--message",
        text,
        "--from-name",
        SENDER,
    ]
}

fn help(entry: &RegistryEntry, scope: &str, node: &str, reason: &str) -> Result<(), String> {
    let text = format!("control: <help class=\"stuck\" reason=\"unfinished-worker\">{node}: {} in {}: {reason}. Review the unfinished work and take ownership of this node.</help>", entry.name, entry.cwd);
    let (code, out, error) = command(
        &[
            "fno",
            "agents",
            "mail",
            "send",
            "--from-name",
            SENDER,
            "--origin",
            "scheduler",
            "--to-lead",
            scope,
            &text,
        ],
        &entry.cwd,
    );
    if crate::mail_inject::mail_send_accepted(code, &out) {
        Ok(())
    } else {
        Err(format!("lead delivery refused: {out} {error}"))
    }
}

fn send_recovery(entry: &RegistryEntry, sid: &str, text: &str) -> Result<(), String> {
    let (code, out, error) = command(
        &[
            "fno",
            "agents",
            "mail",
            "send",
            "--from-name",
            SENDER,
            "--origin",
            "scheduler",
            sid,
            text,
        ],
        &entry.cwd,
    );
    if crate::mail_inject::mail_send_accepted(code, &out)
        || crate::mail_inject::mail_send_receipt(&out).contains("durable")
    {
        Ok(())
    } else {
        Err(format!(
            "recovery message refused: {}",
            condensed(&out, &error)
        ))
    }
}

fn restore(entry: &RegistryEntry, sid: &str, text: &str) -> Result<(), String> {
    let receipt =
        crate::receipt::build_reap_receipt(entry, None, crate::receipt::Writer::RegistryWrite)?;
    if !Path::new(&receipt.cwd).is_dir() {
        return Err(format!(
            "recorded worktree is gone; resume: {}",
            receipt.resume
        ));
    }
    if let Some(mux) = entry.mux.as_ref() {
        let (code, out, error) = command(
            &[
                "fno",
                "mux",
                "workspace",
                "restore",
                "--session",
                &mux.session,
                "--member-session",
                sid,
                "--json",
            ],
            &receipt.cwd,
        );
        let answer: Value = serde_json::from_str(out.trim()).unwrap_or(Value::Null);
        if code == 0
            && answer
                .get("members")
                .and_then(Value::as_array)
                .is_some_and(|members| {
                    members.iter().any(|row| {
                        matches!(
                            row.get("outcome").and_then(Value::as_str),
                            Some("resumed" | "focused")
                        )
                    })
                })
        {
            return send_recovery(entry, sid, text);
        }
        return Err(format!(
            "workspace restore refused: {}; resume: {}",
            condensed(&out, &error),
            receipt.resume
        ));
    }
    let (code, out, error) = command(&resume_argv(sid, text), &receipt.cwd);
    if code == 0 {
        Ok(())
    } else {
        Err(format!(
            "thread resume refused: {}; resume: {}",
            condensed(&out, &error),
            receipt.resume
        ))
    }
}

fn salvage(
    entry: &RegistryEntry,
    sid: &str,
    now: i64,
    claude_home: &crate::claude_ask::ClaudeHome,
) -> Result<String, String> {
    // The registry's recorded cwd can still be canonical (a worker launched
    // there, then ran EnterWorktree); the transcript names where it actually
    // ended up. Salvaging the recorded cwd in that case commits canonical's
    // own stray files under a `recovery/` ref - never the worker's.
    let tree = crate::reentry::resolve_resume_cwd(claude_home, entry.cwd.as_str(), sid);
    if crate::canonical_check::is_canonical_checkout(&tree) {
        return Err(format!(
            "salvage skipped: {} is the main worktree, not the worker's",
            tree.display()
        ));
    }
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let index_path = temp.path().join("index");
    let run = |args: &[&str]| -> Result<String, String> {
        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&tree)
            .env("GIT_INDEX_FILE", &index_path)
            .args(args);
        let out =
            crate::bounded_cmd::output_with_timeout_result(cmd, 30).map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).to_string())
        }
    };
    let head = run(&["rev-parse", "HEAD"])?.trim().to_string();
    run(&["read-tree", "HEAD"])?;
    run(&["add", "--update"])?;
    let untracked = run(&["ls-files", "--others", "--exclude-standard", "-z"])?;
    let mut add = vec!["add", "--"];
    add.extend(untracked.split('\0').filter(|name| !name.is_empty()));
    if add.len() > 2 {
        run(&add)?;
    }
    let tree = run(&["write-tree"])?.trim().to_string();
    let commit = run(&[
        "commit-tree",
        &tree,
        "-p",
        &head,
        "-m",
        "chore: preserve unfinished worker changes",
    ])?
    .trim()
    .to_string();
    let branch = format!("refs/heads/recovery/{sid}-{now}");
    run(&[
        "update-ref",
        &branch,
        &commit,
        "0000000000000000000000000000000000000000",
    ])?;
    Ok(format!(
        "salvaged {branch} at {commit}; original index and worktree preserved"
    ))
}

pub(super) fn run(home: &AgentsHome) -> Result<(), String> {
    let registry = crate::state::load_registry(&home.registry_json()).map_err(|e| e.to_string())?;
    let rows = crate::graph_store::read_rows_where(
        &crate::gc_sweep::graph_path(home),
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "status",
                    "merge_status",
                    "superseded_by",
                    "sessions",
                    "parent",
                    "project",
                    "type",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            ..Default::default()
        },
    )
    .map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().timestamp();
    let claude_home = crate::claude_ask::ClaudeHome::from_env();
    let watches = crate::watch_expiry::read_evidence(home, now * 1000)?;
    // Every `agent_stopped` emit carries only the row's `name`, never a
    // `session_id` (daemon.rs, claude_stop.rs, keeper_stop.rs all stamp
    // `{"name": ...}`), so this keys on name - keying on `session_id` here
    // leaves the set permanently empty.
    let stopped: std::collections::HashSet<_> = watches
        .iter()
        .filter(|e| e.kind == "agent_stopped")
        .filter_map(|e| e.data.get("name").and_then(Value::as_str))
        .collect();
    let mut stores = crate::gc_inventory::HarnessStoreIndex::default();
    let mut loaded = None;
    let mut codex_daemon_dead = None;
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let dir = home.root().join("quiet-recovery");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let settle_watches = super::settle_watches(&watches);
    let mut owners = None;
    let mut acted = 0;
    for entry in &registry.entries {
        let result = (|| -> Result<(), String> {
            let Some(sid) = entry
                .harness_session_id
                .as_deref()
                .filter(|s| crate::resume_wake::is_uuid_shaped(s))
            else {
                return Ok(());
            };
            let Some(node) = entry.node.as_deref() else {
                return Ok(());
            };
            let Some(row) = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(node))
            else {
                return Ok(());
            };
            if row.get("status").and_then(Value::as_str) == Some("deferred") {
                return Ok(());
            }
            let closed = closed(row);
            if settle_watches.iter().any(|w| {
                (w.watch.session_id == sid || w.codex_thread_id.as_deref() == Some(sid))
                    && watch_shielded(&w.watch, &watches, now * 1000)
            }) {
                return Ok(());
            }
            let (claim_state, claim) = crate::claims::status(&format!("node:{node}"), None);
            let own = claim.as_ref().is_some_and(|c| {
                c.session_id.as_deref() == Some(sid) || c.holder == format!("target-session:{sid}")
            });
            if matches!(
                claim_state,
                crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
            ) && !own
            {
                return Ok(());
            }
            if claim_state == crate::claims::ClaimState::Corrupted {
                return Ok(());
            }
            // A row a lead stopped on purpose is the lead's, not quiet
            // recovery's: send no nudge, no resume and no help for it. When
            // its work also delivered, the revival rule records the skip it
            // mandates; an open node stays the silent grace skip.
            if stopped.contains(entry.name.as_str()) {
                if let Some(reason) = stopped_skip_reason(closed) {
                    emitter
                        .emit(
                            "quiet_worker_skip",
                            &json!({"session_id": sid, "node": node, "reason": reason}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                return Ok(());
            }
            let state_path = dir.join(format!("{sid}.json"));
            let mut episode: Episode = match std::fs::read(&state_path) {
                Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string())?,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Episode::default(),
                Err(e) => return Err(e.to_string()),
            };
            let path = entry
                .transcript_path
                .as_deref()
                .filter(|p| Path::new(p).is_file())
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    stores
                        .matches(entry)?
                        .into_iter()
                        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
                });
            let native_codex_thread = entry.harness_name() == "codex" && entry.mux.is_none();
            let daemon_dead = native_codex_thread
                && *codex_daemon_dead.get_or_insert_with(|| {
                    let adapter = crate::codex_inject::CodexDaemonAdapter::from_environment();
                    adapter.provider_pid_start().map_or_else(
                        || !adapter.socket_up(),
                        |(pid, start)| !crate::daemon::pid_is_ours(pid, Some(start)),
                    )
                });
            let thread_unloaded = native_codex_thread
                && !daemon_dead
                && loaded
                    .get_or_insert_with(crate::codex_inject::loaded_thread_ids)
                    .as_ref()
                    .is_ok_and(|roster| !roster.contains(sid));
            let dead = matches!(
                entry.status,
                crate::AgentStatus::Failed
                    | crate::AgentStatus::Exited
                    | crate::AgentStatus::PermanentDead
            ) || entry.pid.is_some_and(|pid| crate::daemon::pid_is_gone(pid))
                || daemon_dead
                || thread_unloaded;
            let transcript_tail = match path.as_deref() {
                Some(path) => tail(path, episode.pending.as_ref().map(|p| p.0.as_str())),
                None if dead => Some(Tail::default()),
                None => None,
            };
            let Some(tail) = transcript_tail else {
                return Ok(());
            };
            let read_receipt = episode
                .pending
                .as_ref()
                .filter(|_| tail.read)
                .map(|(marker, _, stage)| (marker.clone(), *stage));
            let decision = on_closed(
                action(
                    &mut episode,
                    &tail,
                    now,
                    own && claim_state == crate::claims::ClaimState::Live,
                    dead,
                ),
                closed,
            );
            if decision != Action::Wait {
                acted += 1;
            }
            let mut handoff = None;
            match decision {
                Action::Wait => {}
                Action::Nudge(stage) => {
                    let marker = format!("quiet-recovery:{sid}:{}:{stage}", episode.activity);
                    let receipt = crate::receipt::build_reap_receipt(
                        entry,
                        None,
                        crate::receipt::Writer::RegistryWrite,
                    )?;
                    let text = if stage == 1 {
                        if tail.failed {
                            format!("control: previous turn failed on {node}. Continue the assigned work or emit <help>. [{marker}]")
                        } else {
                            format!("control: quiet 15 min on {node}, what blocks you? continue or emit <help>. [{marker}]")
                        }
                    } else {
                        format!("control: quiet 30 min on {node}. Continue or emit <help>. Exact harness resume: {} [{marker}]", receipt.resume)
                    };
                    // A transport ack is only handed. The next transcript read
                    // confirms read before this attempt can advance the ladder.
                    episode.pending = Some((marker.clone(), now, stage));
                    let (code, out, error) = command(
                        &[
                            "fno",
                            "agents",
                            "mail",
                            "send",
                            "--from-name",
                            SENDER,
                            "--origin",
                            "scheduler",
                            sid,
                            &text,
                        ],
                        &entry.cwd,
                    );
                    let delivery = if crate::mail_inject::mail_send_accepted(code, &out) {
                        "handed"
                    } else if crate::mail_inject::mail_send_receipt(&out).contains("durable") {
                        "queued"
                    } else {
                        "failed"
                    };
                    emitter
                        .emit(
                            "quiet_worker_nudge",
                            &json!({"session_id": sid, "node": node, "stage": stage,
                    "marker": marker, "delivery": delivery, "exit": code, "receipt": out, "error": error}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Action::Recover => {
                    let marker = format!("quiet-recovery:{sid}:{}:resume", episode.activity);
                    let text = format!("control: The previous worker stopped with unfinished work. Continue the assigned work in this same worktree from the last completed step, preserving all changes. [{marker}]");
                    let result = restore(entry, sid, &text);
                    episode.recovered = true;
                    let reason = match &result {
                        Ok(()) => format!("resume sent; transcript read pending [{marker}]"),
                        Err(error) => format!(
                            "{error}; {}",
                            salvage(entry, sid, now, &claude_home).unwrap_or_else(|e| format!(
                                "salvage refused: {e}; preserve worktree {}",
                                entry.cwd
                            ))
                        ),
                    };
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node, "reason": reason}),
                        )
                        .map_err(|e| e.to_string())?;
                    if result.is_err() {
                        episode.recovery_reason = Some(reason.clone());
                        handoff = Some(reason);
                    } else {
                        episode.recovery_reason = Some(reason.clone());
                        episode.pending = Some((marker, now, 3));
                    }
                }
                Action::Stop(why) => {
                    let (code, out, error) = command(&["fno", "agents", "stop", sid], &entry.cwd);
                    episode.escalated = true;
                    let receipt =
                        crate::attention_arm::last_line(if code == 0 { &out } else { &error });
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node,
                        "reason": format!("node {why}: stopped the idle worker instead of resuming it (exit {code}: {receipt})")}),
                        )
                        .map_err(|e| e.to_string())?;
                }
                Action::Help => {
                    handoff = Some(episode.recovery_reason.clone().unwrap_or_else(|| {
                        "worker stayed quiet after read-confirmed nudges".to_string()
                    }));
                }
            }
            if let Some((marker, stage)) = read_receipt {
                if stage == 3 {
                    emitter
                        .emit(
                            "quiet_worker_recovery",
                            &json!({"session_id": sid, "node": node,
                        "reason": format!("recipient transcript confirms recovery read [{marker}]"),
                        "delivery": "read"}),
                        )
                        .map_err(|e| e.to_string())?;
                } else {
                    emitter
                        .emit(
                            "quiet_worker_nudge",
                            &json!({"session_id": sid, "node": node,
                        "stage": stage, "marker": marker, "delivery": "read"}),
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
            let bytes = serde_json::to_vec(&episode).map_err(|e| e.to_string())?;
            let tmp = state_path.with_extension("tmp");
            std::fs::write(&tmp, bytes)
                .and_then(|_| std::fs::rename(tmp, &state_path))
                .map_err(|e| e.to_string())?;
            if let Some(reason) = handoff {
                let scope = crate::burn_watch::node_owner(
                    Path::new(&entry.cwd),
                    &home.registry_json(),
                    &rows,
                    &mut owners,
                    node,
                )
                .ok_or_else(|| format!("no live team owner resolved for node {node}"))?;
                help(entry, &scope, node, &reason)?;
                episode.escalated = true;
                let bytes = serde_json::to_vec(&episode).map_err(|e| e.to_string())?;
                let tmp = dir.join(format!("{sid}.tmp"));
                std::fs::write(&tmp, bytes)
                    .and_then(|_| std::fs::rename(tmp, &state_path))
                    .map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        if let Err(error) = result {
            emitter
                .emit(
                    "quiet_worker_error",
                    &json!({"name": entry.name, "reason": error}),
                )
                .map_err(|e| e.to_string())?;
        }
        if acted >= 4 {
            break;
        }
    }
    prune_salvage_refs(&registry, &dir, now)?;
    Ok(())
}

/// Salvage refs are checked against git at most once an hour: every check
/// reads each stray path, too much to pay on every daemon tick.
const SALVAGE_PRUNE_INTERVAL_SECS: i64 = 3600;

fn prune_salvage_refs(
    registry: &crate::state::Registry,
    dir: &Path,
    now: i64,
) -> Result<(), String> {
    let stamp = dir.join("salvage-prune.epoch");
    let last = std::fs::read_to_string(&stamp)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    if matches!(last, Some(t) if now - t < SALVAGE_PRUNE_INTERVAL_SECS) {
        return Ok(());
    }
    let repos: std::collections::BTreeSet<&str> =
        registry.entries.iter().map(|e| e.cwd.as_str()).collect();
    for repo in repos {
        let pruned = match super::salvage_prune::prune(Path::new(repo), "origin/main", now) {
            Ok(pruned) => pruned,
            Err(error) => {
                eprintln!("salvage prune skipped {repo}: {error}");
                continue;
            }
        };
        for p in pruned {
            eprintln!("salvage ref pruned {}: {}", p.refname, p.sha);
        }
    }
    std::fs::write(&stamp, now.to_string()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_requires_reads_resets_on_items_and_recovers_unfinished_death() {
        let mut e = Episode {
            activity: 100,
            ..Default::default()
        };
        let mut t = Tail {
            activity: 100,
            ..Default::default()
        };
        assert_eq!(action(&mut e, &t, 1000, true, false), Action::Nudge(1));
        e.pending = Some(("marker".into(), 1000, 1));
        t.touched = 950;
        assert_eq!(
            action(&mut e, &t, 1001, true, false),
            Action::Wait,
            "an unread nudge never recovers a worker still writing its transcript"
        );
        t.touched = 0;
        assert_eq!(action(&mut e, &t, 1001, true, false), Action::Recover);
        assert_eq!(action(&mut e, &t, 1600, true, false), Action::Recover);
        t.read = true;
        assert_eq!(action(&mut e, &t, 1900, true, false), Action::Nudge(2));
        e.pending = Some(("second".into(), 1900, 2));
        assert_eq!(action(&mut e, &t, 2800, true, false), Action::Help);
        e.pending = Some(("unread".into(), 2700, 2));
        t.read = false;
        assert_eq!(action(&mut e, &t, 2801, false, true), Action::Recover);
        e.pending = None;
        t.activity = 2800;
        assert_eq!(action(&mut e, &t, 2801, true, false), Action::Wait);
        assert_eq!(e.read_stage, 0);
        assert_eq!(
            action(&mut e, &t, 2801, false, true),
            Action::Wait,
            "fresh transcript work outranks a gone pid"
        );
        assert_eq!(
            action(&mut e, &t, 4000, true, true),
            Action::Nudge(1),
            "a live own claim outranks a gone pid: the quiet ladder runs, not recovery"
        );
        assert_eq!(action(&mut e, &t, 4000, false, true), Action::Recover);
        t.touched = 3900;
        e.recovered = false;
        assert_eq!(
            action(&mut e, &t, 4000, false, true),
            Action::Wait,
            "a transcript file write mid tool call is progress"
        );
        assert_eq!(action(&mut e, &t, 5000, false, false), Action::Wait);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rollout.jsonl");
        std::fs::write(&path, concat!(
            "{\"timestamp\":\"2026-01-01T00:00:00Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"reasoning\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"text\":\"marker\"}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:02Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\",\"error\":{\"message\":\"routing timeout\"}}}\n",
            "{\"timestamp\":\"2026-01-01T00:15:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"text\":\"retry started\"}}\n"
        )).unwrap();
        let evidence = tail(&path, Some("marker")).unwrap();
        assert_eq!(evidence.activity, 1767226503);
        assert!(
            !evidence.failed,
            "new transcript work clears stale failure state"
        );
        assert!(
            evidence.read,
            "receipt bytes in the recipient transcript confirm the nudge"
        );

        std::fs::write(
            &path,
            "{\"timestamp\":\"2026-01-01T00:15:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n",
        )
        .unwrap();
        assert_eq!(tail(&path, None).unwrap().activity, 1767226500);
    }

    #[test]
    fn closed_node_stops_an_idle_worker_instead_of_resuming_it() {
        let superseded = json!({"id": "x-1", "status": "in_progress", "superseded_by": "x-2"});
        assert_eq!(closed(&superseded), Some("superseded"));
        assert_eq!(
            on_closed(Action::Recover, closed(&superseded)),
            Action::Stop("superseded")
        );
        assert_eq!(
            on_closed(Action::Nudge(1), closed(&json!({"status": "done"}))),
            Action::Stop("done")
        );
        assert_eq!(
            on_closed(Action::Wait, Some("merged")),
            Action::Wait,
            "a worker still writing on a merged node finishes its ritual"
        );
        let open = json!({"status": "in_progress", "superseded_by": null});
        assert_eq!(on_closed(Action::Recover, closed(&open)), Action::Recover);
    }

    #[test]
    fn a_stopped_row_records_its_skip_only_on_a_delivered_node() {
        for why in ["done", "superseded", "merged"] {
            let reason = stopped_skip_reason(Some(why)).expect("delivered nodes record a skip");
            assert!(
                reason.contains(why) && reason.contains("never revived"),
                "{reason}"
            );
        }
        assert!(
            stopped_skip_reason(None).is_none(),
            "an open node's stopped worker keeps the silent grace skip"
        );
    }

    #[test]
    fn salvage_preserves_untracked_work_and_the_original_branch_and_index() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().to_str().unwrap();
        assert_eq!(command(&["git", "init", "-q"], cwd).0, 0);
        assert_eq!(
            command(&["git", "config", "user.name", "Fixture"], cwd).0,
            0
        );
        assert_eq!(
            command(
                &["git", "config", "user.email", "fixture@example.test"],
                cwd
            )
            .0,
            0
        );
        std::fs::write(dir.path().join("tracked"), "base").unwrap();
        assert_eq!(command(&["git", "add", "tracked"], cwd).0, 0);
        assert_eq!(command(&["git", "commit", "-qm", "base"], cwd).0, 0);

        // A linked worktree off the canonical repo above: this is the
        // worker's own tree, AC7-HP's "transcript names its node worktree".
        let worker_dir = dir.path().join("worker-wt");
        assert_eq!(
            command(
                &[
                    "git",
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    "feature/x",
                    worker_dir.to_str().unwrap(),
                ],
                cwd,
            )
            .0,
            0
        );
        let worker = worker_dir.to_str().unwrap();
        let head = command(&["git", "rev-parse", "HEAD"], worker).1;
        std::fs::write(worker_dir.join("tracked"), "staged").unwrap();
        assert_eq!(command(&["git", "add", "tracked"], worker).0, 0);
        std::fs::write(worker_dir.join("tracked"), "working").unwrap();
        std::fs::write(worker_dir.join(" new file"), "untracked").unwrap();

        let sid = "00000000-0000-4000-8000-000000000001";
        let home_root = tempfile::tempdir().unwrap();
        let claude_home = crate::claude_ask::ClaudeHome::at(home_root.path());
        let transcript_dir = claude_home
            .projects_dir()
            .join(crate::claude_ask::claude_cwd_slug(&worker_dir));
        std::fs::create_dir_all(&transcript_dir).unwrap();
        std::fs::write(transcript_dir.join(format!("{sid}.jsonl")), "[]").unwrap();

        // The registry's recorded cwd is still the canonical repo; the
        // transcript resolves salvage to the worker's own linked worktree.
        let entry = RegistryEntry {
            cwd: cwd.into(),
            ..Default::default()
        };
        let receipt = salvage(&entry, sid, 1, &claude_home).unwrap();
        let branch = format!("refs/heads/recovery/{sid}-1");
        assert!(receipt.contains(&branch));
        assert_eq!(
            command(&["git", "show", &format!("{branch}:tracked")], worker).1,
            "working"
        );
        assert_eq!(
            command(&["git", "show", &format!("{branch}: new file")], worker).1,
            "untracked"
        );
        assert_eq!(command(&["git", "show", ":tracked"], worker).1, "staged");
        assert_eq!(command(&["git", "rev-parse", "HEAD"], worker).1, head);
        assert_eq!(
            std::fs::read_to_string(worker_dir.join("tracked")).unwrap(),
            "working"
        );

        // AC7-EDGE: a second row whose recorded cwd resolves to nothing but
        // itself (no transcript, no linked worktree it owns) must skip -
        // never mint a recovery ref from the canonical repo's own files.
        let canonical_only = RegistryEntry {
            cwd: cwd.into(),
            ..Default::default()
        };
        let other_sid = "00000000-0000-4000-8000-000000000002";
        let err = salvage(&canonical_only, other_sid, 1, &claude_home).unwrap_err();
        assert!(
            err.contains("salvage skipped") && err.contains("the main worktree"),
            "{err}"
        );
    }

    #[test]
    fn watch_shield_covers_the_bound_and_one_quiet_interval_past_it() {
        let watch = crate::watch_expiry::Watch {
            event_id: "e1".into(),
            seq: 1,
            session_id: "sess-a".into(),
            pr: Some(258),
            node: "x-1".into(),
            blocker: "ci".into(),
            task_id: None,
            reason: None,
            expires_at_ms: 1_000_000,
            ts_ms: 100_000,
        };
        assert!(watch_shielded(&watch, &[], watch.expires_at_ms - 1));
        assert!(watch_shielded(&watch, &[], watch.expires_at_ms));
        assert!(watch_shielded(
            &watch,
            &[],
            watch.expires_at_ms + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(
            &watch,
            &[],
            watch.expires_at_ms + WATCH_LINGER_MS
        ));
    }

    #[test]
    fn the_expiry_wake_does_not_unshield_a_parked_session() {
        // The incident: a session parks on ci, the bound fires, the daemon
        // emits its wake row (which ends is_current_watch), and the worker's
        // next transcript write is still minutes out. The shield must hold
        // through the wake and one quiet interval past the bound.
        let evidence = vec![
            crate::watch_expiry::Evidence {
                event_id: "e1".into(),
                seq: 1,
                ts_ms: 100_000,
                kind: crate::watch_expiry::WATCH_IDLE.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({
                    "harness": "claude",
                    "expires_at_ms": 1_000_000,
                    "blocker": "ci",
                    "node": "x-1",
                }),
            },
            crate::watch_expiry::Evidence {
                event_id: "e2".into(),
                seq: 2,
                ts_ms: 1_000_050,
                kind: crate::watch_expiry::WAKE_EVENT.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({"watch_event_id": "e1"}),
            },
        ];
        let watch = &crate::codex_watch::settle_watches(&evidence)[0].watch;
        assert!(
            crate::watch_expiry::is_current_watch(watch, &evidence),
            "the wake row is not superseding evidence, so the watch stays current"
        );
        assert!(watch_shielded(watch, &evidence, 1_000_050));
        assert!(watch_shielded(
            watch,
            &evidence,
            watch.expires_at_ms + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(
            watch,
            &evidence,
            watch.expires_at_ms + WATCH_LINGER_MS
        ));
    }

    #[test]
    fn a_superseded_watch_never_shields_the_seat() {
        // A watch a later idle row superseded (the worker re-armed or moved
        // on) releases the seat at once, however far its own bound still
        // sits in the future.
        let evidence = vec![crate::watch_expiry::Evidence {
            event_id: "e2".into(),
            seq: 2,
            ts_ms: 200_000,
            kind: crate::watch_expiry::WATCH_IDLE.into(),
            session_id: Some("sess-a".into()),
            data: serde_json::json!({
                "harness": "claude",
                "expires_at_ms": 1_100_000,
                "blocker": "ci",
                "node": "x-1",
            }),
        }];
        let superseded = crate::watch_expiry::Watch {
            event_id: "e1".into(),
            seq: 1,
            session_id: "sess-a".into(),
            pr: Some(258),
            node: "x-1".into(),
            blocker: "ci".into(),
            task_id: None,
            reason: None,
            expires_at_ms: 1_000_000,
            ts_ms: 100_000,
        };
        assert!(!crate::watch_expiry::is_current_watch(
            &superseded,
            &evidence
        ));
        assert!(!watch_shielded(&superseded, &evidence, 150_000));
    }

    #[test]
    fn an_early_wake_lights_the_shield_from_the_wake_not_the_bound() {
        // A ci watch settled ahead of its bound: the worker got its answer
        // early, so the linger runs from the wake's own stamp, not from the
        // bound it never reached. A worker that stalls after answering is
        // read on the same cadence as any other quiet worker.
        let evidence = vec![
            crate::watch_expiry::Evidence {
                event_id: "e1".into(),
                seq: 1,
                ts_ms: 100_000,
                kind: crate::watch_expiry::WATCH_IDLE.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({
                    "harness": "claude",
                    "expires_at_ms": 1_000_000,
                    "blocker": "ci",
                    "node": "x-1",
                }),
            },
            crate::watch_expiry::Evidence {
                event_id: "e2".into(),
                seq: 2,
                ts_ms: 500_000,
                kind: crate::watch_expiry::WAKE_EVENT.into(),
                session_id: Some("sess-a".into()),
                data: serde_json::json!({"watch_event_id": "e1"}),
            },
        ];
        let watch = &crate::codex_watch::settle_watches(&evidence)[0].watch;
        assert!(watch_shielded(watch, &evidence, 500_000));
        assert!(watch_shielded(
            watch,
            &evidence,
            500_000 + WATCH_LINGER_MS - 1
        ));
        assert!(!watch_shielded(watch, &evidence, 500_000 + WATCH_LINGER_MS));
    }
}
