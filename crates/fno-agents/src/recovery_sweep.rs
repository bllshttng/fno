//! Layer-2 provider failover and close surfacing for background sessions.
//!
//! One sweep over footnote-launched bg sessions (registry provenance joined
//! with live sessions). It cannot resume a stuck session: a bypass recipient
//! holds every socket message, so a stuck node-bound worker is surfaced as
//! `held-by-design`, never nudged. What acts is the refusal path, which records
//! a quota lock, and close surfacing for a finished session that still lingers.
//!
//! Every read and write crosses [`SweepSeams`], so the decisions replay offline
//! against the goldens in `tests/golden/recovery/`.

pub mod live;
pub mod mission;
pub mod redispatch;

use crate::accounts::error_class::{normalize, ErrorClass, NormalizedError, Outcome};
use crate::claude_ask::SessionLocator;
use crate::state::RegistryEntry;
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub use live::run_cli;

/// Decision words [`classify`] returns.
pub const NUDGE: &str = "nudge";
pub const NUDGE_CLOSE: &str = "nudge-close";
pub const SKIP_NEEDS_INPUT: &str = "skip-needs-input";
pub const SKIP_TERMINAL: &str = "skip-terminal";
pub const NOT_STALE: &str = "not-stale";

/// The surface a finished-but-lingering session raises: close it, not resume it.
pub const CLOSE_MESSAGE: &str = "mission complete but still open - run retro then /stop to close";

/// How much of a live worker's last turn counts as its refusal. Prose about a
/// cap usually buries the marker; a real refusal leads with it.
pub const REFUSAL_LEAD_CHARS: usize = 120;

/// The two `config.recovery` values the sweep reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecoveryCfg {
    pub idle_threshold_seconds: i64,
    pub max_nudges: i64,
}

/// A footnote-launched bg session reachable over its messaging socket.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    pub short_id: String,
    pub sock_path: String,
    pub jobs_dir: PathBuf,
    pub cwd: Option<String>,
    pub name: Option<String>,
    pub session_id: Option<String>,
    /// The caller's attributed account for a quota lock, never the
    /// process-global active one: a refusal belongs to the launch account.
    pub launch_account: Option<String>,
}

/// What the sweep reads from a session's `state.json`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SnapshotView {
    pub state: String,
    pub updated_at: Option<String>,
    pub output_result: Option<String>,
}

/// The sweep's I/O. [`live::LiveSeams`] is the real one.
pub trait SweepSeams {
    fn registry(&self) -> Result<Vec<RegistryEntry>, String>;
    fn locate(&self, short_id: &str) -> Option<SessionLocator>;
    fn read_state(&self, jobs_dir: &Path) -> SnapshotView;
    /// The session-truth payload for one candidate.
    fn truth(&self, candidate: &Candidate) -> Value;
    fn liveness(&self, sock_path: &str) -> bool;
    fn load_counts(&self) -> Map<String, Value>;
    fn save_counts(&self, counts: &Map<String, Value>);
    fn mission_complete(&self, candidate: &Candidate) -> Option<bool>;
    /// True only when the notice reached the operator.
    fn notify_close(&self, candidate: &Candidate) -> bool;
    fn reset_timezone(&self, cwd: Option<&str>) -> Option<String>;
    /// The account the lock was written for, or `None` when it was refused.
    fn record_quota_lock(
        &self,
        account: Option<&str>,
        text: &str,
        resets_at: Option<f64>,
    ) -> Option<String>;
    fn now(&self) -> f64;
}

/// Python truthiness for a JSON value: the persisted counts and graph rows were
/// written by more than one producer.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Classify from transcript truth; `state` is phase metadata. A terminal
/// `<promise>` alone says nothing about the mission: only `Some(false)` relaxes
/// it to the staleness gate, `Some(true)` flips to [`NUDGE_CLOSE`] once idle,
/// and `None` (unverifiable) fails closed to [`SKIP_TERMINAL`].
pub fn classify(
    state: &str,
    idle_threshold_s: i64,
    truth_state: &str,
    truth_age_s: Option<f64>,
    mission_complete: Option<bool>,
) -> &'static str {
    let idle = |age: Option<f64>| age.is_some_and(|a| a >= idle_threshold_s as f64);
    if state == "needs-input" || truth_state == "your-move" {
        return SKIP_NEEDS_INPUT;
    }
    if truth_state == "done" {
        match mission_complete {
            Some(false) => {}
            Some(true) => {
                return if idle(truth_age_s) {
                    NUDGE_CLOSE
                } else {
                    NOT_STALE
                };
            }
            None => return SKIP_TERMINAL,
        }
    }
    if truth_state == "unknown" || truth_state == "watching" {
        return NOT_STALE;
    }
    if truth_state == "stalled" {
        return NUDGE;
    }
    if idle(truth_age_s) {
        NUDGE
    } else {
        NOT_STALE
    }
}

/// Join footnote's claude registry rows with live bg sessions: a row that is
/// not claude, has no short id or has no located session is dropped.
pub fn iter_candidates(
    entries: &[RegistryEntry],
    locate: &dyn Fn(&str) -> Option<SessionLocator>,
) -> Vec<Candidate> {
    let filled = |value: &Option<String>| value.clone().filter(|v| !v.is_empty());
    let mut out = Vec::new();
    for entry in entries {
        if entry.harness.as_deref() != Some("claude") || entry.short_id.is_empty() {
            continue;
        }
        let Some(located) = locate(&entry.short_id) else {
            continue;
        };
        if located.messaging_socket_path.is_empty() {
            continue;
        }
        out.push(Candidate {
            short_id: entry.short_id.clone(),
            sock_path: located.messaging_socket_path,
            jobs_dir: located.jobs_dir,
            cwd: Some(entry.cwd.clone()).filter(|c| !c.is_empty()),
            name: Some(entry.name.clone()).filter(|n| !n.is_empty()),
            session_id: filled(&entry.harness_session_id)
                .or_else(|| filled(&entry.cc_session_id))
                .or_else(|| Some(entry.short_id.clone())),
            launch_account: filled(&entry.launch_account),
        });
    }
    out
}

/// Classify a dead session's last `output.result`. The error class carries
/// the swap read: `err.error_class.triggers_swap()`. Empty text is `None`.
pub fn classify_session_error(text: &str) -> Option<NormalizedError> {
    classify_session_error_at(text, None, crate::accounts::state::wall_now())
}

/// [`classify_session_error`] with the account's zone and the observation time.
pub fn classify_session_error_at(
    text: &str,
    reset_timezone: Option<&str>,
    now: f64,
) -> Option<NormalizedError> {
    if text.is_empty() {
        return None;
    }
    normalize(&Outcome {
        http_status: None,
        exit_code: None,
        body: text,
        parser_failed: false,
        model: None,
        reset_timezone,
        now,
    })
    .ok()
}

/// The provider refusal a candidate carries, with its source: `output_result`
/// (a dead session's own error, read whole and authoritative) or `transcript`
/// (a live worker's last turn, leading clause only).
pub fn classify_worker_refusal(
    output_result: Option<&str>,
    last_message: Option<&str>,
    reset_timezone: Option<&str>,
) -> Option<(NormalizedError, &'static str)> {
    classify_worker_refusal_at(
        output_result,
        last_message,
        reset_timezone,
        crate::accounts::state::wall_now(),
    )
}

/// [`classify_worker_refusal`] with the observation time.
pub fn classify_worker_refusal_at(
    output_result: Option<&str>,
    last_message: Option<&str>,
    reset_timezone: Option<&str>,
    now: f64,
) -> Option<(NormalizedError, &'static str)> {
    let swap = |text: &str| {
        classify_session_error_at(text, reset_timezone, now)
            .filter(|err| err.error_class.triggers_swap())
    };
    if let Some(err) = output_result.and_then(swap) {
        return Some((err, "output_result"));
    }
    let lead: Option<String> =
        last_message.map(|text| text.chars().take(REFUSAL_LEAD_CHARS).collect());
    lead.as_deref()
        .and_then(swap)
        .map(|err| (err, "transcript"))
}

/// Read `graph_node_id` from a worktree's `.fno/target-state.md`; any read or
/// parse miss is `None`.
pub fn node_id_from_worktree(cwd: &str) -> Option<String> {
    crate::backlog::worked::node_id_from_worktree(cwd)
}

/// True only when the worktree positively runs no `/target` session: a
/// reachable `.fno` with no manifest. A broken `.fno` symlink is ambiguous and
/// reads as node-bound.
fn worktree_is_node_less(cwd: &str) -> bool {
    let fno_dir = Path::new(cwd).join(".fno");
    fno_dir.is_dir() && !fno_dir.join("target-state.md").exists()
}

/// Keep a counts entry only while its session is still a candidate. A sentinel
/// key shares the flat dict: a short id never contains `:`.
pub fn prune_keep(key: &str, live: &BTreeSet<String>) -> bool {
    if key.starts_with("refused:") {
        return key.split(':').nth(1).is_some_and(|id| live.contains(id));
    }
    for prefix in ["capped:", "close:", "quota-locked:"] {
        if let Some(id) = key.strip_prefix(prefix) {
            return live.contains(id);
        }
    }
    live.contains(key)
}

fn flagged(counts: &Map<String, Value>, key: &str) -> bool {
    counts.get(key).is_some_and(truthy)
}

/// Classify each candidate and act. Every decision that matters emits at most
/// one event; a healthy session stays silent so quiet ticks do not spam the log.
pub fn recovery_sweep(
    cfg: &RecoveryCfg,
    candidates: &[Candidate],
    counts: &mut Map<String, Value>,
    seams: &dyn SweepSeams,
    emit: &mut dyn FnMut(&str, Value),
) {
    for c in candidates {
        let snap = seams.read_state(&c.jobs_dir);
        let truth = seams.truth(c);
        // A refusal is affirmative evidence and runs before both silence gates:
        // a capped worker's last turn has a fresh mtime, and a worker that
        // promised earlier reads done.
        let zone = seams.reset_timezone(c.cwd.as_deref());
        let refusal = classify_worker_refusal_at(
            snap.output_result.as_deref(),
            truth.get("last_message").and_then(Value::as_str),
            zone.as_deref(),
            seams.now(),
        );
        if let Some((err, source)) = refusal {
            surface_refusal(c, &truth, &err, source, counts, seams, emit);
        }

        let truth_state = truth
            .get("state")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        let mission = if truth_state == "done" {
            seams.mission_complete(c)
        } else {
            None
        };
        let decision = classify(
            &snap.state,
            cfg.idle_threshold_seconds,
            truth_state,
            truth.get("last_activity_age_s").and_then(Value::as_f64),
            mission,
        );
        match decision {
            SKIP_NEEDS_INPUT => {
                emit(
                    "recovery_skipped",
                    json!({"short_id": c.short_id, "reason": "needs-input"}),
                );
            }
            NUDGE_CLOSE => {
                let key = format!("close:{}", c.short_id);
                if !flagged(counts, &key) {
                    // Once only either way, so a host with no channel does not
                    // retry every tick; never claim a surface that did not land.
                    counts.insert(key, json!(true));
                    if seams.notify_close(c) {
                        emit("recovery_close_notify", json!({"short_id": c.short_id}));
                    } else {
                        emit(
                            "recovery_skipped",
                            json!({"short_id": c.short_id, "reason": "no-notify-channel"}),
                        );
                    }
                }
            }
            NUDGE => surface_stuck(cfg, c, counts, seams, emit),
            _ => {}
        }
    }
}

fn surface_refusal(
    c: &Candidate,
    truth: &Value,
    err: &NormalizedError,
    source: &str,
    counts: &mut Map<String, Value>,
    seams: &dyn SweepSeams,
    emit: &mut dyn FnMut(&str, Value),
) {
    let refused_key = format!("refused:{}:{}", c.short_id, err.error_class.as_str());
    let already = flagged(counts, &refused_key);
    // The event costs nothing when wrong; the lock does not. Prose about a cap
    // reads like a cap, so a live turn must still classify the same a tick
    // later: a capped worker's last turn never changes.
    let acts = source == "output_result" || already;
    if !already {
        counts.insert(refused_key, json!(true));
        let observed = truth.get("observed_model").filter(|m| m.is_object());
        let model = observed
            .and_then(|m| m.get("model"))
            .cloned()
            .unwrap_or(Value::Null);
        emit(
            "worker_refused",
            json!({
                "short_id": c.short_id,
                "node": c.cwd.as_deref().and_then(node_id_from_worktree),
                "error_class": err.error_class.as_str(),
                "source": source,
                "model": model,
                "resets_at": err.resets_at,
                "reset_is_derived": err.reset_is_derived,
                "reset_stamp_unparsed": err.reset_stamp_unparsed,
                "excerpt": err.body_excerpt,
            }),
        );
    }
    let quota_key = format!("quota-locked:{}", c.short_id);
    if acts && err.error_class == ErrorClass::Provider4xxQuota && !flagged(counts, &quota_key) {
        counts.insert(quota_key, json!(true));
        let wrote = seams.record_quota_lock(
            c.launch_account.as_deref(),
            &err.body_excerpt,
            err.resets_at,
        );
        let attributed = wrote.is_some();
        emit(
            "provider_quota_locked",
            json!({
                "short_id": c.short_id,
                "account": wrote,
                "attributed": attributed,
                "source": source,
                "resets_at": err.resets_at,
                "reset_is_derived": err.reset_is_derived,
            }),
        );
    }
}

/// A stuck node-bound worker is surfaced up to `max_nudges`, then capped. No
/// socket send: the recipient would hold it and stack an operator dialog. A
/// node-less bg thread is none of recovery's mission.
fn surface_stuck(
    cfg: &RecoveryCfg,
    c: &Candidate,
    counts: &mut Map<String, Value>,
    seams: &dyn SweepSeams,
    emit: &mut dyn FnMut(&str, Value),
) {
    let Some(cwd) = c.cwd.as_deref() else {
        return;
    };
    if worktree_is_node_less(cwd) {
        return;
    }
    let nudges = counts.get(&c.short_id).and_then(Value::as_i64).unwrap_or(0);
    if nudges >= cfg.max_nudges {
        let key = format!("capped:{}", c.short_id);
        if !flagged(counts, &key) {
            counts.insert(key, json!(true));
            emit(
                "recovery_capped",
                json!({"short_id": c.short_id, "nudges": nudges}),
            );
        }
        return;
    }
    if !seams.liveness(&c.sock_path) {
        emit(
            "recovery_skipped",
            json!({"short_id": c.short_id, "reason": "socket-unreachable"}),
        );
        return;
    }
    counts.insert(c.short_id.clone(), json!(nudges + 1));
    emit(
        "recovery_skipped",
        json!({"short_id": c.short_id, "reason": "held-by-design"}),
    );
}

/// Run one sweep and persist the counts, pruned to current candidates so the
/// file stays bounded. Returns the candidate count; a registry that cannot be
/// read is an error, never a quiet zero.
pub fn run_recovery_sweep(
    cfg: &RecoveryCfg,
    seams: &dyn SweepSeams,
    emit: &mut dyn FnMut(&str, Value),
) -> Result<usize, String> {
    let entries = seams.registry()?;
    let candidates = iter_candidates(&entries, &|short_id: &str| seams.locate(short_id));
    let live: BTreeSet<String> = candidates.iter().map(|c| c.short_id.clone()).collect();
    let mut counts: Map<String, Value> = seams
        .load_counts()
        .into_iter()
        .filter(|(key, _)| prune_keep(key, &live))
        .collect();
    recovery_sweep(cfg, &candidates, &mut counts, seams, emit);
    seams.save_counts(&counts);
    Ok(candidates.len())
}
