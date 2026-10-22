//! The post-merge dispatch seam: one warm/cold/defer verdict, the dedup
//! layers (marker + claim + flight), the attribution receipt, and the
//! warm/cold execution. Ported from Python `post_merge_route.py`; the
//! `pr-watch post-merge-dispatch` arm is the pr-watch tick's only door.
//!
//! The decision order, the marker path, the claim/flight dedup and the
//! fail-closed reserve are contract; the transports (control-sock, pane,
//! codex daemon, the cold self-exec) call the shared cores and own no copy.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// The injected turn: the SAME mechanical command the cold path self-execs.
/// Warm and cold differ only in transport, never in what runs.
const WARM_PROMPT_TEMPLATE: &str = "fno do pr ritual {pr} --autonomous";

const RECEIPT_EVENT: &str = "post_merge_dispatch_receipt";
const DETAIL_MAX: usize = 512;
const DISPATCH_TTL_MS: i64 = 15 * 60 * 1000;
const DISPATCH_SUBDIR: &str = "post-merge-dispatched";

fn warm_prompt(pr: i64) -> String {
    WARM_PROMPT_TEMPLATE.replace("{pr}", &pr.to_string())
}

// -- the arm ---------------------------------------------------------------

/// The `pr-watch post-merge-dispatch` arm: the `dispatch_post_merge_ritual`
/// keyword set (minus `auto_run`, which this arm reads from config itself,
/// fail closed) plus `timeout_s`; JSON out {outcome, pr_number, short_id,
/// detail}.
pub fn run(_args: &[String]) -> i32 {
    let mut buf = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_err() {
        eprintln!("pr-watch post-merge-dispatch: cannot read stdin");
        return 2;
    }
    let req: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-watch post-merge-dispatch: bad payload: {e}");
            return 2;
        }
    };
    let str_field = |k: &str| {
        req.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let Some(pr) = req.get("pr_number").and_then(Value::as_i64) else {
        eprintln!("pr-watch post-merge-dispatch: pr_number is required");
        return 2;
    };
    let node_cwd = str_field("node_cwd");
    let canonical = resolve_canonical(
        str_field("canonical_root").map(PathBuf::from),
        node_cwd.as_deref(),
    );
    let auto_run = read_auto_run(&canonical).unwrap_or(false);
    let timeout_s = req
        .get("timeout_s")
        .and_then(Value::as_f64)
        .unwrap_or(300.0);
    let result = dispatch(
        pr,
        str_field("dedup_key"),
        auto_run,
        &canonical,
        str_field("ship_session_id"),
        str_field("ship_harness"),
        str_field("source_session_id"),
        str_field("source_harness"),
        str_field("source_cwd"),
        str_field("node_id"),
        str_field("repo_slug"),
        timeout_s,
    );
    println!(
        "{}",
        json!({
            "outcome": result.outcome,
            "pr_number": result.pr_number,
            "short_id": result.short_id,
            "detail": result.detail,
        })
    );
    0
}

// -- the one warm/cold/defer decision ---------------------------------------

struct RouteDecision {
    route: &'static str,
    reason: String,
    delivering_session_id: Option<String>,
    delivering_harness: Option<String>,
    borrowed_session_id: Option<String>,
    borrowed_harness: Option<String>,
    origin_dead: bool,
}

fn decide<R, D>(
    auto_run: bool,
    ship_session_id: Option<&str>,
    ship_harness: Option<&str>,
    source_session_id: Option<&str>,
    source_harness: Option<&str>,
    source_cwd: Option<&str>,
    resolve_warm: R,
    death_confirmed: D,
) -> RouteDecision
where
    R: Fn(&str, Option<&str>) -> Option<String>,
    D: Fn(&str, Option<&str>, &str) -> bool,
{
    if !auto_run {
        return decision("defer", "auto-run-disabled");
    }
    let ship_sid = nonempty(ship_session_id);
    let source_sid = nonempty(source_session_id);
    let provenance = if ship_sid.is_some() {
        "ship-provenance"
    } else {
        "no-ship-provenance"
    };
    let mut candidates: Vec<(&str, Option<&str>)> = Vec::new();
    for (sid, harness) in [
        (ship_sid.as_deref(), ship_harness),
        (source_sid.as_deref(), source_harness),
    ] {
        if let Some(sid) = sid {
            if !candidates.iter().any(|(s, _)| *s == sid) {
                candidates.push((sid, harness));
            }
        }
    }
    let mut cold_reason = "no-live-source-session".to_string();
    for (sid, harness) in &candidates {
        if let Some(warm_sid) = resolve_warm(sid, *harness) {
            return RouteDecision {
                route: "warm",
                reason: provenance.to_string(),
                delivering_session_id: ship_sid.clone(),
                delivering_harness: ship_sid
                    .as_ref()
                    .map(|_| ship_harness.unwrap_or(""))
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                borrowed_session_id: Some(warm_sid),
                borrowed_harness: harness.filter(|h| !h.is_empty()).map(str::to_string),
                origin_dead: false,
            };
        }
    }
    let origin_dead = match (source_sid.as_deref(), source_cwd) {
        (Some(sid), Some(cwd)) => death_confirmed(sid, source_harness, cwd),
        _ => false,
    };
    if ship_sid.is_none() {
        cold_reason = format!("{cold_reason}; {provenance}");
    }
    RouteDecision {
        route: "cold",
        reason: cold_reason,
        delivering_session_id: ship_sid.clone(),
        delivering_harness: ship_sid
            .as_ref()
            .map(|_| ship_harness.unwrap_or(""))
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        borrowed_session_id: None,
        borrowed_harness: None,
        origin_dead,
    }
}

fn decision(route: &'static str, reason: &str) -> RouteDecision {
    RouteDecision {
        route,
        reason: reason.to_string(),
        delivering_session_id: None,
        delivering_harness: None,
        borrowed_session_id: None,
        borrowed_harness: None,
        origin_dead: false,
    }
}

fn nonempty(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// -- the durable attribution receipt ----------------------------------------

struct Receipt<'a> {
    phase: &'a str,
    dispatch_id: &'a str,
    attempt_id: &'a str,
    pr: i64,
    route: &'a str,
    outcome: &'a str,
    node_id: Option<&'a str>,
    repo_slug: Option<&'a str>,
    delivering_session_id: Option<&'a str>,
    delivering_harness: Option<&'a str>,
    borrowed_session_id: Option<&'a str>,
    borrowed_harness: Option<&'a str>,
    detail: Option<String>,
}

fn emit_receipt(path: Option<&Path>, r: &Receipt) -> bool {
    let Some(path) = path else {
        return false;
    };
    let opt = |v: Option<&str>| v.filter(|s| !s.is_empty()).map(|s| json!(s));
    let mut data = serde_json::Map::new();
    // Absent identities are omitted rather than written empty: an invented
    // attribution is worse than a recorded miss. Alphabetical inserts keep
    // the row stable under the preserve-order map.
    for (k, v) in [
        ("attempt_id", Some(json!(r.attempt_id))),
        ("borrowed_harness", opt(r.borrowed_harness)),
        ("borrowed_session_id", opt(r.borrowed_session_id)),
        ("delivering_harness", opt(r.delivering_harness)),
        ("delivering_session_id", opt(r.delivering_session_id)),
        (
            "detail",
            match &r.detail {
                Some(d) => {
                    let cut: String = d.chars().take(DETAIL_MAX).collect();
                    if cut.is_empty() {
                        None
                    } else {
                        Some(json!(cut))
                    }
                }
                None => None,
            },
        ),
        ("detector", Some(json!("pr-watch"))),
        ("dispatch_id", Some(json!(r.dispatch_id))),
        ("node_id", opt(r.node_id)),
        ("outcome", Some(json!(r.outcome))),
        ("phase", Some(json!(r.phase))),
        ("pr", Some(json!(r.pr))),
        ("repo_slug", opt(r.repo_slug)),
        ("route", Some(json!(r.route))),
    ] {
        if let Some(v) = v {
            data.insert(k.to_string(), v);
        }
    }
    let emitter = crate::events::EventEmitter::new(path.to_path_buf(), "daemon");
    emitter.emit_fields(RECEIPT_EVENT, data).is_ok()
}

fn new_attempt_id() -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    nanos.hash(&mut h);
    std::process::id().hash(&mut h);
    format!("{:012x}", h.finish() & 0xffff_ffff_ffff)
}

// -- dispatch: marker + claim dedup, receipt, warm/cold execution -----------

pub(crate) struct DispatchResult {
    outcome: String,
    pr_number: i64,
    short_id: String,
    detail: Option<String>,
}

fn result(outcome: &str, pr: i64) -> DispatchResult {
    DispatchResult {
        outcome: outcome.to_string(),
        pr_number: pr,
        short_id: String::new(),
        detail: None,
    }
}

/// The transport seams, split from the decision so tests fake the edges the
/// oracle faked (resolver, inject, command runner, finalize, receipt) and the
/// dedup layers stay real on-disk state.
struct Seams {
    resolve_warm: Box<dyn Fn(&str, Option<&str>) -> Option<String>>,
    death_confirmed: Box<dyn Fn(&str, Option<&str>, &str) -> bool>,
    inject: Box<dyn Fn(&str, i64, Option<&str>) -> (bool, String)>,
    run_command: Box<dyn Fn(i64, &Path, f64) -> (bool, String)>,
    finalize_origin: Box<dyn Fn(&str, &str, Option<&str>) -> bool>,
    emit: Box<dyn Fn(Option<&Path>, &Receipt) -> bool>,
}

fn sanitize_key(raw: Option<&str>, pr: i64) -> String {
    let fallback = format!("pr-{pr}");
    let key = raw.filter(|s| !s.trim().is_empty()).unwrap_or(&fallback);
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn marker_path(canonical: &Path, key: &str) -> PathBuf {
    canonical.join(".fno").join(DISPATCH_SUBDIR).join(key)
}

fn persist_marker(canonical: &Path, key: &str) {
    let marker = marker_path(canonical, key);
    if let Some(dir) = marker.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::File::create(&marker);
}

/// The canonical root the marker lives under: an explicit pin, else the
/// worktree's canonical repo (the --git-common-dir walk), else the node cwd.
fn resolve_canonical(pinned: Option<PathBuf>, node_cwd: Option<&str>) -> PathBuf {
    if let Some(p) = pinned {
        return p;
    }
    if let Some(cwd) = node_cwd {
        if let Some(canon) = crate::post_merge::ritual::canonical_root(Path::new(cwd)) {
            return canon;
        }
        return PathBuf::from(cwd);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// The arm reads `post_merge.auto_run` itself, fail closed: a config-load
/// error is an unconfigured repo, never an armed one. The merged table IS
/// the post_merge fields; the missing section reads false here because a
/// repo that never opted in must not dispatch.
fn read_auto_run(canonical: &Path) -> Option<bool> {
    let table = crate::agents_config::config_table_merged(canonical, &["post_merge"])?;
    Some(match table.get("auto_run") {
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::String(s)) => {
            matches!(
                s.trim().to_lowercase().as_str(),
                "true" | "1" | "yes" | "on"
            )
        }
        _ => false,
    })
}

/// Step 0.5 read: the ritual's mutex claim on the GLOBAL root. Live means the
/// command is running right now; suspect means a crashed run's TTL still covers.
fn ritual_claim_state(pr: i64, claim_root: Option<&Path>) -> Option<&'static str> {
    let key = format!("reconcile:pr-{pr}");
    let (state, _) = crate::claims::status(&key, claim_root);
    match state {
        crate::claims::ClaimState::Live => Some("live"),
        crate::claims::ClaimState::Suspect => Some("suspect"),
        _ => None,
    }
}

fn dispatch(
    pr: i64,
    dedup_key: Option<String>,
    auto_run: bool,
    canonical: &Path,
    ship_session_id: Option<String>,
    ship_harness: Option<String>,
    source_session_id: Option<String>,
    source_harness: Option<String>,
    source_cwd: Option<String>,
    node_id: Option<String>,
    repo_slug: Option<String>,
    timeout_s: f64,
) -> DispatchResult {
    let seams = real_seams();
    dispatch_with(
        pr,
        dedup_key,
        auto_run,
        canonical,
        ship_session_id,
        ship_harness,
        source_session_id,
        source_harness,
        source_cwd,
        node_id,
        repo_slug,
        timeout_s,
        &seams,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_with(
    pr: i64,
    dedup_key: Option<String>,
    auto_run: bool,
    canonical: &Path,
    ship_session_id: Option<String>,
    ship_harness: Option<String>,
    source_session_id: Option<String>,
    source_harness: Option<String>,
    source_cwd: Option<String>,
    node_id: Option<String>,
    repo_slug: Option<String>,
    timeout_s: f64,
    seams: &Seams,
) -> DispatchResult {
    dispatch_claim_root(
        pr,
        dedup_key,
        auto_run,
        canonical,
        ship_session_id,
        ship_harness,
        source_session_id,
        source_harness,
        source_cwd,
        node_id,
        repo_slug,
        timeout_s,
        seams,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn dispatch_claim_root(
    pr: i64,
    dedup_key: Option<String>,
    auto_run: bool,
    canonical: &Path,
    ship_session_id: Option<String>,
    ship_harness: Option<String>,
    source_session_id: Option<String>,
    source_harness: Option<String>,
    source_cwd: Option<String>,
    node_id: Option<String>,
    repo_slug: Option<String>,
    timeout_s: f64,
    seams: &Seams,
    claim_root: Option<&Path>,
) -> DispatchResult {
    let dispatch_id = sanitize_key(dedup_key.as_deref(), pr);
    // Defer: one receipt, no marker, no work. Sits before the marker/claim
    // dedup so a disabled repo does not pay for a probe, and the receipt
    // records the deliberate no-op, not an already-dispatched skip.
    if !auto_run {
        let attempt = new_attempt_id();
        let r = Receipt {
            phase: "deferred",
            dispatch_id: &dispatch_id,
            attempt_id: &attempt,
            pr,
            route: "defer",
            outcome: "auto-run-disabled",
            node_id: node_id.as_deref(),
            repo_slug: repo_slug.as_deref(),
            detail: Some("auto-run-disabled".to_string()),
            delivering_session_id: None,
            delivering_harness: None,
            borrowed_session_id: None,
            borrowed_harness: None,
        };
        let _ = (seams.emit)(events_path().as_deref(), &r);
        let mut out = result("disabled", pr);
        out.detail = Some("auto-run-disabled".to_string());
        return out;
    }
    let marker = marker_path(canonical, &dispatch_id);
    // Cross-session / cross-trigger dedup: a persisted marker means the
    // ritual already ran for this merge SHA (a completed no-op).
    if marker.exists() {
        let mut r = result("already-dispatched", pr);
        r.detail = Some("marker-exists".to_string());
        return r;
    }
    // Re-entrancy guard: a live holder of the ritual's mutex means the command
    // is already running for this PR. Global claims root.
    if ritual_claim_state(pr, claim_root) == Some("live") {
        persist_marker(canonical, &dispatch_id);
        let mut r = result("already-dispatched", pr);
        r.detail = Some("ritual-claim-live".to_string());
        return r;
    }
    if ritual_claim_state(pr, claim_root) == Some("suspect") {
        let mut r = result("already-dispatched", pr);
        r.detail = Some("lock-contention".to_string());
        return r;
    }
    let flight_key = format!("post-merge-ritual:{dispatch_id}");
    // Unique per invocation, matching the python leg: a same-holder acquire
    // reads as an idempotent reacquire, so a literal would let two concurrent
    // watchers both pass this gate. The release below must name the same one.
    let flight_holder = format!("pr-watch:{}", std::process::id());
    let flight = crate::flight_gate::acquire_flight(
        &flight_key,
        &flight_holder,
        DISPATCH_TTL_MS,
        "post-merge ritual dispatch",
        Some(std::process::id()),
        Some(canonical.to_path_buf()),
        None,
    );
    if !matches!(flight, crate::flight_gate::FlightGate::Acquired(_)) {
        // In-flight, not done, not a reason to crash the tick loop.
        let mut r = result("already-dispatched", pr);
        r.detail = Some("lock-contention".to_string());
        return r;
    }
    let outcome = run_dispatch(
        pr,
        &dispatch_id,
        canonical,
        ship_session_id.as_deref(),
        ship_harness.as_deref(),
        source_session_id.as_deref(),
        source_harness.as_deref(),
        source_cwd.as_deref(),
        node_id.as_deref(),
        repo_slug.as_deref(),
        timeout_s,
        seams,
    );
    // This process holds the dispatch lease for its whole run.
    let _ = crate::claims::release(&flight_key, &flight_holder, Some(canonical), None);
    outcome
}

/// The post-lease body: the decision, the fail-closed reserve, then warm
/// or cold execution. Every phase after the reserve is best-effort.
#[allow(clippy::too_many_arguments)]
fn run_dispatch(
    pr: i64,
    dispatch_id: &str,
    canonical: &Path,
    ship_session_id: Option<&str>,
    ship_harness: Option<&str>,
    source_session_id: Option<&str>,
    source_harness: Option<&str>,
    source_cwd: Option<&str>,
    node_id: Option<&str>,
    repo_slug: Option<&str>,
    timeout_s: f64,
    seams: &Seams,
) -> DispatchResult {
    // Double-checked under the lock.
    if marker_path(canonical, dispatch_id).exists() {
        let mut r = result("already-dispatched", pr);
        r.detail = Some("marker-exists".to_string());
        return r;
    }
    let verdict = decide(
        true,
        ship_session_id,
        ship_harness,
        source_session_id,
        source_harness,
        source_cwd,
        |sid, h| (seams.resolve_warm)(sid, h),
        |sid, h, cwd| (seams.death_confirmed)(sid, h, cwd),
    );
    let attempt = new_attempt_id();
    let base = Receipt {
        phase: "",
        dispatch_id,
        attempt_id: &attempt,
        pr,
        route: verdict.route,
        outcome: "",
        node_id,
        repo_slug,
        delivering_session_id: verdict.delivering_session_id.as_deref(),
        delivering_harness: verdict.delivering_harness.as_deref(),
        borrowed_session_id: verdict.borrowed_session_id.as_deref(),
        borrowed_harness: verdict.borrowed_harness.as_deref(),
        detail: None,
    };
    // Reserve the receipt BEFORE any action. Fail-closed: a write failure
    // starts no work and writes no marker, so the merge stays retryable.
    let reserved = base.clone_receipt("reserved", "reserved");
    if !(seams.emit)(events_path().as_deref(), &reserved) {
        let mut r = result("failed", pr);
        r.detail = Some("receipt-reservation-failed".to_string());
        return r;
    }
    let mut cold_reason = verdict.reason.clone();
    let mut finalized_origin = false;
    if verdict.route == "warm" {
        if let Some(borrowed) = verdict.borrowed_session_id.as_deref() {
            let (delivered, reason) =
                (seams.inject)(borrowed, pr, verdict.borrowed_harness.as_deref());
            if delivered || reason == "queue-timeout" {
                persist_marker(canonical, dispatch_id);
                let outcome = if reason == "queue-timeout" {
                    "queued"
                } else {
                    "delivered"
                };
                let mut acc = base.clone_receipt("accepted", outcome);
                acc.detail = Some(reason.clone());
                let _ = (seams.emit)(events_path().as_deref(), &acc);
                let mut r = result("routed-warm", pr);
                r.short_id = crate::identity::canonical_handle(borrowed);
                r.detail = Some(if reason == "queue-timeout" {
                    "queued".to_string()
                } else {
                    reason
                });
                return r;
            }
            cold_reason = reason;
        }
    }
    // Direct-finalize rung (cold prelude): a provably dead origin whose
    // manifest + transcript survive gets its full-fidelity ledger row, then
    // control FALLS THROUGH to the command. A finalize failure degrades to cold.
    if verdict.origin_dead
        && source_cwd.is_some()
        && origin_transcript_exists(source_session_id, source_cwd, source_harness)
    {
        if let (Some(sid), Some(cwd)) = (source_session_id, source_cwd) {
            let tpath = origin_transcript_path(Some(sid), cwd, source_harness);
            if let Some(tpath) = tpath {
                finalized_origin =
                    (seams.finalize_origin)(cwd, &tpath.display().to_string(), source_harness);
            }
        }
    }
    let canonical_str = canonical.display().to_string();
    let (ok, tail) = (seams.run_command)(pr, Path::new(&canonical_str), timeout_s);
    if !ok {
        let mut failed = base.clone_receipt("failed", "command-nonzero");
        failed.detail = Some(tail.clone());
        let _ = (seams.emit)(events_path().as_deref(), &failed);
        let mut r = result("failed", pr);
        let cut: String = tail.chars().take(200).collect();
        r.detail = Some(if cut.is_empty() {
            "command-nonzero".to_string()
        } else {
            cut
        });
        return r;
    }
    persist_marker(canonical, dispatch_id);
    let mut accepted = base.clone_receipt("accepted", "completed");
    accepted.detail = Some(tail.clone());
    let _ = (seams.emit)(events_path().as_deref(), &accepted);
    let mut r = result(
        if finalized_origin {
            "finalized-origin"
        } else {
            "dispatched"
        },
        pr,
    );
    r.short_id = "verb".to_string();
    r.detail = Some(format!("cold: {cold_reason}"));
    r
}

impl Receipt<'_> {
    fn clone_receipt<'a>(&'a self, phase: &'a str, outcome: &'a str) -> Receipt<'a> {
        Receipt {
            phase,
            outcome,
            detail: None,
            dispatch_id: self.dispatch_id,
            attempt_id: self.attempt_id,
            pr: self.pr,
            route: self.route,
            node_id: self.node_id,
            repo_slug: self.repo_slug,
            delivering_session_id: self.delivering_session_id,
            delivering_harness: self.delivering_harness,
            borrowed_session_id: self.borrowed_session_id,
            borrowed_harness: self.borrowed_harness,
        }
    }
}

fn events_path() -> Option<PathBuf> {
    Some(crate::paths::AgentsHome::from_env().events_jsonl())
}

// -- the real transports -----------------------------------------------------

/// The cold run self-execs the binary's own `pr-watch ritual` arm from the
/// canonical root under the caller's timeout: a self call, not a seam
/// crossing.
fn run_ritual_command(pr: i64, cwd: &Path, timeout: f64) -> (bool, String) {
    let Ok(exe) = std::env::current_exe() else {
        return (false, "command-error: no current exe".to_string());
    };
    let mut cmd = Command::new(&exe);
    cmd.args(["pr-watch", "ritual", &pr.to_string(), "--autonomous"])
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let run = crate::post_merge::ritual::bounded(
        cmd,
        std::time::Duration::from_secs_f64(timeout.max(1.0)),
    );
    // The oracle's forensic tail: the last 200 chars of the stripped stdout,
    // not the last line - a mid-receipt cut still names the failing leg.
    let stripped = run.stdout.trim();
    let count = stripped.chars().count();
    let tail: String = if count > 200 {
        stripped.chars().skip(count - 200).collect()
    } else {
        stripped.to_string()
    };
    (run.ok(), tail)
}

/// The claude warm probe and inject share one truth: the daemon roster.
/// Resolvable means deliverable; every resolution failure is not-live.
fn inject_claude(session: &str, pr: i64) -> (bool, String) {
    let cmd = warm_prompt(pr);
    match crate::mail_inject::deliver_via_control_sock(
        session,
        &cmd,
        crate::mail_inject::DEFAULT_ATTEMPTS,
        crate::mail_inject::DEFAULT_INTERVAL_MS,
        crate::mail_inject::default_enter_delay_ms(crate::mail_inject::MailInjectHarness::Claude),
    ) {
        Ok(()) => (true, "delivered".to_string()),
        Err("not-confirmed") => (false, "queue-timeout".to_string()),
        Err(_) => (false, "not-live".to_string()),
    }
}

fn codex_registry_entry(session_id: &str) -> Option<crate::state::RegistryEntry> {
    let rows = crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .ok()?
        .entries;
    let matches: Vec<crate::state::RegistryEntry> = rows
        .into_iter()
        .filter(|e| {
            e.harness.as_deref() == Some("codex")
                && e.harness_session_id.as_deref() == Some(session_id)
        })
        .collect();
    matches
        .iter()
        .find(|e| e.mux.is_some())
        .cloned()
        .or_else(|| matches.into_iter().next())
}

/// The family-1 state read, in process: transcript truth, never status.
fn family1_state(handle: &str) -> Option<String> {
    let rows = crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .ok()
        .map(|r| r.entries);
    let stores = crate::session_truth::Stores::ambient();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    let cursors = crate::session_truth::cursor::global();
    let payload =
        crate::session_truth::resolve_payload(rows.as_deref(), handle, now, &stores, cursors);
    payload
        .get("state")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The codex inject: prefer the transport-bearing (mux) row and pane-send
/// into the pane lane; a transportless row rides the daemon RPC. Any miss
/// is not-live and the caller cold-dispatches.
fn inject_codex(session: &str, pr: i64) -> (bool, String) {
    let Some(entry) = codex_registry_entry(session) else {
        return (false, "not-live".to_string());
    };
    let cmd = warm_prompt(pr);
    if let Some(mux) = &entry.mux {
        let address = format!("{}:{}", mux.session, mux.pane_id);
        match crate::mail_inject::deliver_via_pane(
            session,
            &address,
            Some("codex"),
            &cmd,
            crate::mail_inject::DEFAULT_ATTEMPTS,
            crate::mail_inject::DEFAULT_INTERVAL_MS,
            crate::mail_inject::default_enter_delay_ms(crate::mail_inject::MailInjectHarness::Pane),
        ) {
            Ok(()) => (true, "delivered".to_string()),
            Err(_) => (false, "not-live".to_string()),
        }
    } else {
        match crate::codex_inject::deliver_via_codex_daemon_sync(session, &cmd) {
            Ok(()) => (true, "delivered".to_string()),
            // A transportless row is a hosted thread: its delivery lane was
            // the switchboard exchange, which has no native arm yet. Name
            // the lane, not the session; the cold path still runs the command.
            Err(_) => (false, "unsupported-lane:codex-thread".to_string()),
        }
    }
}

/// The finalize rung: `fno-agents finalize` against a dead origin's manifest
/// + transcript, in process. `--reason DoneAwaitingMerge` is not a
/// SHIP_REASON, so finalize runs the always-branch ledger row only.
fn finalize_origin_impl(cwd: &str, transcript: &str, harness: Option<&str>) -> bool {
    let _ = harness;
    crate::finalize::run_finalize(&[
        "--state".to_string(),
        format!("{cwd}/.fno/target-state.md"),
        "--cwd".to_string(),
        cwd.to_string(),
        "--reason".to_string(),
        "DoneAwaitingMerge".to_string(),
        "--transcript".to_string(),
        transcript.to_string(),
    ]) == 0
}

/// The on-disk claude transcript for a (session, cwd) origin, or None.
/// Liveness-independent; a non-claude harness yields None.
fn origin_transcript_path(
    session_id: Option<&str>,
    cwd: &str,
    harness: Option<&str>,
) -> Option<PathBuf> {
    let sid = session_id.filter(|s| !s.is_empty())?;
    if let Some(h) = harness {
        if h != "claude" {
            return None;
        }
    }
    let projects = crate::claude_drive::claude_projects_dir();
    for name in candidate_dir_names(cwd) {
        let candidate = projects.join(name).join(format!("{sid}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// The finalize rung gate: transcript AND manifest both survive.
fn origin_transcript_exists(
    session_id: Option<&str>,
    cwd: Option<&str>,
    harness: Option<&str>,
) -> bool {
    let Some(cwd) = cwd else {
        return false;
    };
    match origin_transcript_path(session_id, cwd, harness) {
        Some(_) => crate::target_orient::manifest_path(Path::new(cwd)).is_some(),
        None => false,
    }
}

/// The two claude projects-dir spellings Python's `_candidate_dir_names`
/// produced; one is the cwd with every non-alphanumeric collapsed to `-`.
fn candidate_dir_names(cwd: &str) -> Vec<String> {
    let mut names = Vec::new();
    for keep_underscore in [false, true] {
        let name: String = cwd
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || (keep_underscore && c == '_') {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// The production seams: roster-backed claude probe+inject, the codex
/// registry row (pane lane first), the self-exec cold command, in-process
/// finalize, and the global events log.
fn real_seams() -> Seams {
    Seams {
        resolve_warm: Box::new(|sid, harness| {
            let harness = harness.unwrap_or("claude");
            match harness {
                "claude" => {
                    // One truth: resolvable on the roster = deliverable.
                    if crate::mail_inject::resolve_target(sid).is_ok() {
                        Some(sid.to_string())
                    } else {
                        None
                    }
                }
                "codex" => {
                    // No registry row means no codex lane to deliver through.
                    codex_registry_entry(sid)?;
                    let state = family1_state(sid).unwrap_or_default();
                    if ["working", "watching", "your-move"].contains(&state.as_str()) {
                        Some(sid.to_string())
                    } else {
                        None
                    }
                }
                // gemini/unknown: no live-inject vehicle, cold path.
                _ => None,
            }
        }),
        death_confirmed: Box::new(|sid, _harness, _cwd| {
            if sid.is_empty() {
                return false;
            }
            // Only an explicit family-1 verdict confirms death; "unknown"
            // never does, whatever the registry row looks like.
            let state = family1_state(sid).unwrap_or_default();
            ["done", "stalled"].contains(&state.as_str())
        }),
        inject: Box::new(|session, pr, harness| match harness.unwrap_or("claude") {
            "claude" => inject_claude(session, pr),
            "codex" => inject_codex(session, pr),
            other => (false, format!("unsupported-harness:{other}")),
        }),
        run_command: Box::new(run_ritual_command),
        finalize_origin: Box::new(finalize_origin_impl),
        emit: Box::new(|path, r| emit_receipt(path, r)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording_seams(command_ok: bool) -> (Seams, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&log);
        (
            Seams {
                resolve_warm: Box::new(|_, _| None),
                death_confirmed: Box::new(|_, _, _| false),
                inject: Box::new(|_, _, _| (false, "not-live".to_string())),
                run_command: Box::new(move |_, _, _| {
                    if command_ok {
                        (true, "step=reap-rows status=ok".to_string())
                    } else {
                        (false, String::new())
                    }
                }),
                finalize_origin: Box::new(|_, _, _| false),
                emit: Box::new(move |_, r| {
                    if let Ok(mut l) = sink.lock() {
                        l.push(format!("{}:{}", r.phase, r.outcome));
                    }
                    true
                }),
            },
            log,
        )
    }

    #[test]
    fn decide_prefers_ship_over_source() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = std::sync::Arc::clone(&seen);
        let d = decide(
            true,
            Some("ship-1"),
            Some("claude"),
            Some("src-1"),
            Some("claude"),
            None,
            move |sid, _| {
                if let Ok(mut s) = sink.lock() {
                    s.push(sid.to_string());
                }
                (sid == "ship-1").then(|| sid.to_string())
            },
            |_, _, _| false,
        );
        assert_eq!(d.route, "warm");
        assert_eq!(d.borrowed_session_id.as_deref(), Some("ship-1"));
        assert_eq!(seen.lock().unwrap().as_slice(), ["ship-1"]);
    }

    #[test]
    fn decide_origin_dead_needs_death_verdict() {
        let d = decide(
            true,
            None,
            None,
            Some("dead-1"),
            Some("claude"),
            Some("/repo"),
            |_, _| None,
            |sid, _, _| sid == "dead-1",
        );
        assert!(d.origin_dead);
        assert_eq!(d.reason, "no-live-source-session; no-ship-provenance");
    }

    fn dispatch_all(pr: i64, auto_run: bool, canonical: &Path, seams: &Seams) -> DispatchResult {
        dispatch_claim_root(
            pr,
            Some(format!("sha-{pr}")),
            auto_run,
            canonical,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            300.0,
            seams,
            None,
        )
    }

    #[test]
    fn dispatch_defer_writes_receipt_and_no_marker() {
        let (seams, log) = recording_seams(true);
        let canon = tempfile::tempdir().expect("tempdir");
        let out = dispatch_all(41, false, canon.path(), &seams);
        assert_eq!(out.outcome, "disabled");
        assert_eq!(out.detail.as_deref(), Some("auto-run-disabled"));
        assert!(!marker_path(canon.path(), "sha-41").exists());
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["deferred:auto-run-disabled"]
        );
    }

    #[test]
    fn dispatch_marker_blocks_and_claim_live_persists_one() {
        let (seams, log) = recording_seams(true);
        let canon = tempfile::tempdir().expect("tempdir");
        persist_marker(canon.path(), "sha-42");
        let out = dispatch_all(42, true, canon.path(), &seams);
        assert_eq!(out.outcome, "already-dispatched");
        assert_eq!(out.detail.as_deref(), Some("marker-exists"));
        assert!(
            log.lock().unwrap().is_empty(),
            "no receipt on a marker skip"
        );

        let claim_dir = tempfile::tempdir().expect("claims tempdir");
        let opts = crate::claims::AcquireOpts {
            pid: Some(1),
            ttl_ms: Some(900_000),
            ..Default::default()
        };
        let seeded = crate::claims::acquire(
            "reconcile:pr-43",
            "other-runner",
            crate::claims::AcquireOpts {
                root: Some(claim_dir.path().to_path_buf()),
                ..opts
            },
        );
        assert!(matches!(seeded, crate::claims::AcquireOutcome::Acquired(_)));
        let out = dispatch_claim_root(
            43,
            Some("sha-43".to_string()),
            true,
            canon.path(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            300.0,
            &seams,
            Some(claim_dir.path()),
        );
        assert_eq!(out.outcome, "already-dispatched");
        assert_eq!(out.detail.as_deref(), Some("ritual-claim-live"));
        assert!(
            marker_path(canon.path(), "sha-43").exists(),
            "live claim persists the marker"
        );
    }

    #[test]
    fn dispatch_cold_command_decides_marker_and_receipt() {
        let canon = tempfile::tempdir().expect("tempdir");
        let (ok_seams, ok_log) = recording_seams(true);
        let out = dispatch_all(44, true, canon.path(), &ok_seams);
        assert_eq!(out.outcome, "dispatched");
        assert_eq!(out.short_id, "verb");
        // No ship provenance on the dispatch_all path: the reason carries
        // the suffix decide() appends when ship_session_id is absent.
        assert_eq!(
            out.detail.as_deref(),
            Some("cold: no-live-source-session; no-ship-provenance")
        );
        assert!(marker_path(canon.path(), "sha-44").exists());
        assert_eq!(
            ok_log.lock().unwrap().as_slice(),
            ["reserved:reserved", "accepted:completed"]
        );

        let canon2 = tempfile::tempdir().expect("tempdir");
        let (bad_seams, bad_log) = recording_seams(false);
        let out = dispatch_all(45, true, canon2.path(), &bad_seams);
        assert_eq!(out.outcome, "failed");
        assert_eq!(out.detail.as_deref(), Some("command-nonzero"));
        assert!(
            !marker_path(canon2.path(), "sha-45").exists(),
            "no marker on failure"
        );
        assert_eq!(
            bad_log.lock().unwrap().as_slice(),
            ["reserved:reserved", "failed:command-nonzero"]
        );
    }
}
