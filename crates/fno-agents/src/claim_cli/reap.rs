//! `claim reap` leaf: the ported Python `fno.claims.cli.reap_cmd` +
//! `fno.claims.core.reap_dead_claims` loop (claims-port epic).
//!
//! Sweep-read each claims directory in-process (`list_in` + the sweep
//! payload builder), judge every claim through the abandonment probe, and
//! force-release the proven-dead ones. The daemon's witness-based engine in
//! `claim_store` (`reap` / `reap_in_directory`) is a separate surface and is
//! deliberately NOT reused here: the leaf implements the Python loop's
//! semantics line for line.
//!
//! Documented port divergences (all unreachable from the frozen goldens):
//! - Roster-backed probe arms degrade fail-closed to
//!   `roster-read-degraded` (no `RosterReading` in crates).
//! - A sweep-read failure prints one stderr line and exits 1 where Python's
//!   traceback escapes (same exit code, different stderr).
//! - Root dedup keys on `fs::canonicalize` of the resolved claims dir;
//!   Python's `resolve(strict=False)` also resolves symlinked ancestors of
//!   missing paths, so two spellings of a not-yet-created dir can dedup in
//!   Python and stay separate here.

use crate::claims::{encode_key, read_claim_file, ClaimRecord, ReadError};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Redefined here: the engine's copy in `claim_commands` is private.
const HANDOVER_HOLDER_PREFIX: &str = "spawn-handover:";
/// Mirrors `truth_status._HOLDER_PREFIX`.
const SESSION_HOLDER_PREFIX: &str = "target-session:";

const SWEEP_FIELDS_MSG: &str = "native claim verdict omitted sweep fields";

struct LeafArgs {
    apply: bool,
    json_output: bool,
    roots: Option<Vec<PathBuf>>,
}

fn usage_error(detail: &str) -> i32 {
    super::usage_refusal(
        "fno agents claim reap [OPTIONS]",
        "fno agents claim reap --help",
        detail,
    )
}

fn parse_args(args: &[String]) -> Result<LeafArgs, i32> {
    let mut apply = false;
    let mut json_output = false;
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut have_root = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--apply" => apply = true,
            "--json" | "-J" => json_output = true,
            "--root" => match args.get(i + 1) {
                Some(value) => {
                    roots.push(PathBuf::from(value));
                    have_root = true;
                    i += 1;
                }
                None => return Err(usage_error("Option '--root' requires an argument.")),
            },
            a if a.starts_with("--root=") => {
                let value = &a["--root=".len()..];
                roots.push(PathBuf::from(value));
                have_root = true;
            }
            a if a.starts_with('-') && a != "-" => {
                return Err(usage_error(&format!("No such option: {a}")));
            }
            a => return Err(usage_error(&format!("Got unexpected extra argument ({a})"))),
        }
        i += 1;
    }
    Ok(LeafArgs {
        apply,
        json_output,
        roots: if have_root { Some(roots) } else { None },
    })
}

struct ReapSummary {
    scanned: u64,
    reaped: u64,
    would_reap: u64,
    kept_offhost: u64,
    kept_suspect: u64,
    kept_live: u64,
    kept_suspect_alive: u64,
    kept_suspect_unprobed: u64,
    kept_unclassified: u64,
    corrupted: u64,
    vanished: u64,
    contended: u64,
    roots: Vec<String>,
    reap_failed: Vec<(String, String)>,
    // Transient: reaped config-optout records awaiting the restore fold.
    // Never serialized - Python's summary dict has no sink key either.
    optout_sink: Vec<ClaimRecord>,
    unclassified_dirs: BTreeMap<String, u64>,
    kept_suspect_unprobed_by: BTreeMap<String, u64>,
    apply: bool,
}

impl ReapSummary {
    fn new(roots: Vec<String>, apply: bool) -> Self {
        ReapSummary {
            scanned: 0,
            reaped: 0,
            would_reap: 0,
            kept_offhost: 0,
            kept_suspect: 0,
            kept_live: 0,
            kept_suspect_alive: 0,
            kept_suspect_unprobed: 0,
            kept_unclassified: 0,
            corrupted: 0,
            vanished: 0,
            contended: 0,
            roots,
            reap_failed: Vec::new(),
            optout_sink: Vec::new(),
            unclassified_dirs: BTreeMap::new(),
            kept_suspect_unprobed_by: BTreeMap::new(),
            apply,
        }
    }

    fn note_kept(&mut self, bucket: &str) {
        match bucket {
            "live" => self.kept_live += 1,
            "offhost" => self.kept_offhost += 1,
            "suspect" => self.kept_suspect += 1,
            "suspect_alive" => self.kept_suspect_alive += 1,
            "suspect_unprobed" => self.kept_suspect_unprobed += 1,
            "unclassified" => self.kept_unclassified += 1,
            // A throwaway bucket word: Python would mint an unprinted
            // kept_<word> key on the summary; the classifier's enumerated
            // buckets never reach here.
            _ => {}
        }
    }

    fn note_token(&mut self, key: &str, reasons: &BTreeMap<String, String>) {
        if let Some(token) = reasons.get(key) {
            *self
                .kept_suspect_unprobed_by
                .entry(token.clone())
                .or_insert(0) += 1;
        }
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "scanned": self.scanned,
            "reaped": self.reaped,
            "would_reap": self.would_reap,
            "kept_offhost": self.kept_offhost,
            "kept_suspect": self.kept_suspect,
            "kept_live": self.kept_live,
            "kept_suspect_alive": self.kept_suspect_alive,
            "kept_suspect_unprobed": self.kept_suspect_unprobed,
            "kept_unclassified": self.kept_unclassified,
            "corrupted": self.corrupted,
            "vanished": self.vanished,
            "contended": self.contended,
            "roots": self.roots,
            "reap_failed": self
                .reap_failed
                .iter()
                .map(|(path, reason)| serde_json::json!([path, reason]))
                .collect::<Vec<_>>(),
            "unclassified_dirs": self.unclassified_dirs,
            "kept_suspect_unprobed_by": self.kept_suspect_unprobed_by,
            "apply": self.apply,
        })
    }
}

/// Shared mutable state behind the probe closure: the last verdict token per
/// key (last-wins overwrite, read back by the token accounting) and the
/// per-worker pane-absent cache.
#[derive(Default)]
struct ProbeCtx {
    reasons: BTreeMap<String, String>,
    pane_cache: BTreeMap<String, Option<bool>>,
}

#[derive(Debug)]
enum PerKeyError {
    GoneAway,
    Corrupted,
    Other(String),
}

fn per_key_read(err: ReadError) -> PerKeyError {
    match err {
        ReadError::GoneAway => PerKeyError::GoneAway,
        ReadError::Corrupted(_) => PerKeyError::Corrupted,
    }
}

fn row_basis(row: &Value) -> String {
    row.get("basis")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string()
}

/// Port of `core.sweep_verdict`: coerce the native verdict row, engage the
/// abandonment probe only for node:-prefixed suspects.
fn judge(rec: &ClaimRecord, row: &Value, ctx: &mut ProbeCtx) -> Result<(bool, String), String> {
    let Some(obj) = row.as_object() else {
        return Err(SWEEP_FIELDS_MSG.to_string());
    };
    if !obj.contains_key("provably_dead") || !obj.contains_key("bucket") {
        return Err(SWEEP_FIELDS_MSG.to_string());
    }
    let dead = obj
        .get("provably_dead")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let bucket = obj
        .get("bucket")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if dead || bucket != "suspect" {
        return Ok((dead, bucket));
    }
    if !rec.key.starts_with("node:") {
        return Ok((false, bucket));
    }
    let native_state = obj.get("state").and_then(Value::as_str);
    match probe_abandonment(rec, native_state, ctx) {
        (Some(true), _) => Ok((true, String::new())),
        (Some(false), _) => Ok((false, "suspect_alive".to_string())),
        (None, _) => Ok((false, "suspect_unprobed".to_string())),
    }
}

/// [`judge`] for one key outside the sweep loop (the spawn guard's
/// recovery), with a fresh probe context.
pub(crate) fn judge_key(rec: &ClaimRecord, row: &Value) -> Result<(bool, String), String> {
    judge(rec, row, &mut ProbeCtx::default())
}

/// Record a tokened probe arm (reasons are last-wins per key) and report it.
fn note_token(
    ctx: &mut ProbeCtx,
    rec: &ClaimRecord,
    token: &'static str,
    answer: Option<bool>,
) -> (Option<bool>, Option<&'static str>) {
    ctx.reasons.insert(rec.key.clone(), token.to_string());
    (answer, Some(token))
}

/// Port of `cli._abandonment_probe`. Arm order is load-bearing: every arm
/// returns immediately, so a handover holder never reaches the session or
/// roster arms.
fn probe_abandonment(
    rec: &ClaimRecord,
    native_state: Option<&str>,
    ctx: &mut ProbeCtx,
) -> (Option<bool>, Option<&'static str>) {
    let Some(native_state) = native_state else {
        return note_token(ctx, rec, "native-verdict-missing", None);
    };
    if let Some(worker) = rec.holder.strip_prefix(HANDOVER_HOLDER_PREFIX) {
        if native_state == "live" {
            return note_token(ctx, rec, "handover-live", None);
        }
        if handover_pane_probe_blocked(rec) {
            return note_token(ctx, rec, "handover-pane-probe-blocked", None);
        }
        let node_id = rec.key.strip_prefix("node:").unwrap_or("");
        let cache_key = format!("pane_absent:{worker}");
        let answer = match ctx.pane_cache.get(&cache_key) {
            Some(cached) => *cached,
            None => {
                let answer = mux_pane_absent_for(worker, node_id);
                ctx.pane_cache.insert(cache_key, answer);
                answer
            }
        };
        if answer != Some(true) {
            return note_token(ctx, rec, "handover-pane-not-absent", None);
        }
        // A pane probe that proves absence IS the death proof: no token.
        return (Some(true), None);
    }
    let session = rec
        .holder
        .strip_prefix(SESSION_HOLDER_PREFIX)
        .filter(|s| !s.is_empty());
    if session.is_none() {
        return note_token(ctx, rec, "holder-session-unparsed", None);
    }
    // Roster-backed arms (roster read, per-row finished checks) have no Rust
    // leg yet; degrade fail-closed so an unprovable suspect is never reaped.
    note_token(ctx, rec, "roster-read-degraded", None)
}

/// Port of `cli._claim_worktree_cwd`: first non-empty string among the
/// metadata `worktree` and `cwd` fields.
fn claim_meta_repo(rec: &ClaimRecord) -> Option<PathBuf> {
    for field in ["worktree", "cwd"] {
        if let Some(Value::String(s)) = rec.metadata.get(field) {
            if !s.is_empty() {
                return Some(PathBuf::from(s.as_str()));
            }
        }
    }
    None
}

/// Port of `cli._handover_pane_probe_blocked`: consult the worktree policy
/// through the Python command (single-resolver doctrine); blocked for `never`
/// and every failure path, matching Python's `except: return True` (a dead
/// handover holder is kept, not probed).
fn handover_pane_probe_blocked(rec: &ClaimRecord) -> bool {
    let repo = claim_meta_repo(rec).unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
    let bin = crate::scrape::fno_bin().to_string_lossy().into_owned();
    let repo_arg = repo.to_string_lossy().into_owned();
    let Some((code, stdout)) = run_with_timeout(
        &bin,
        &[
            "agents",
            "workspace",
            "worktree",
            "policy",
            "--repo",
            &repo_arg,
        ],
        10,
    ) else {
        return true;
    };
    if code != 0 {
        return true;
    }
    match stdout.lines().next() {
        Some("never") => true,
        Some(_) => false,
        None => true,
    }
}

/// Port of `cli._mux_pane_absent_for`: None = the pane question went
/// unanswered, Some(false) = a pane of this worker was seen, Some(true) =
/// every live session's pane listing was seen and none matched.
fn mux_pane_absent_for(worker: &str, node_id: &str) -> Option<bool> {
    let bin = crate::scrape::fno_bin().to_string_lossy().into_owned();
    let (_, stdout) = run_with_timeout(&bin, &["mux", "ls", "--json"], 10)?;
    let rows: Vec<Value> = serde_json::from_str(&stdout).ok()?;
    let mut names: Vec<&str> = Vec::new();
    for candidate in [worker, node_id] {
        if !candidate.is_empty() && !names.contains(&candidate) {
            names.push(candidate);
        }
    }
    let mut saw_content = false;
    for row in rows {
        let Some(obj) = row.as_object() else { continue };
        if obj.get("state").and_then(Value::as_str) != Some("live") {
            continue;
        }
        let session = obj
            .get("session")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())?;
        let panes_val = obj.get("panes")?;
        let pane_count = parse_count(panes_val)?;
        if pane_count == 0 {
            continue;
        }
        let (_, pane_stdout) = run_with_timeout(
            &bin,
            &["mux", "pane", "ls", "--server", session, "--json"],
            10,
        )?;
        let panes: Vec<Value> = serde_json::from_str(&pane_stdout).ok()?;
        if panes.is_empty() {
            return None;
        }
        saw_content = true;
        for pane in &panes {
            let Some(pane_obj) = pane.as_object() else {
                continue;
            };
            let cwd_name = pane_obj
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or("");
            for field in ["fno_id", "title"] {
                let value = pane_obj.get(field).and_then(Value::as_str).unwrap_or("");
                if names.contains(&value) {
                    return Some(false);
                }
            }
            if names.contains(&cwd_name) {
                return Some(false);
            }
        }
    }
    if saw_content {
        Some(true)
    } else {
        None
    }
}

/// Python `int()`: a JSON number, or a decimal string; no floats, no bools.
fn parse_count(value: &Value) -> Option<i64> {
    if let Some(n) = value.as_i64() {
        return Some(n);
    }
    value.as_str()?.trim().parse::<i64>().ok()
}

/// Spawn `bin args` with a wall-clock kill deadline; None = spawn failure,
/// timeout, or a failed wait. Stdout is read after exit (the JSON listings
/// this feeds are bounded by the mux registry size; an output that would
/// fill the pipe deadlocks the child past the deadline and reads as None,
/// which matches Python's TimeoutExpired handling).
fn run_with_timeout(bin: &str, args: &[&str], timeout_secs: u64) -> Option<(i32, String)> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return None,
        }
    };
    let mut stdout = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    Some((status.code().unwrap_or(-1), stdout))
}

/// The keyed fresh sweep, mirrors of `verdict.claim_verdicts(..., key=...)`:
/// rows come from the same in-process records, a vanished requested key
/// mints the synthetic free row, an impossible omission lands in
/// reap_failed.
pub(crate) fn keyed_fresh_row(directory: &Path, key: &str) -> Result<Value, String> {
    let records =
        crate::claims::list_in(std::slice::from_ref(&PathBuf::from(directory)), None, true)
            .map_err(|err| format!("reap fresh sweep failed: {err}"))?;
    if !records.iter().any(|rec| rec.key == key) {
        return Ok(serde_json::json!({
            "key": key,
            "state": "free",
            "basis": "free",
            "bucket": "free",
            "provably_dead": false,
        }));
    }
    let payload = crate::claim_commands::claim_sweep_payload_from_records(
        &records,
        None,
        &[key.to_string()],
        false,
    );
    payload
        .get("claims")
        .and_then(Value::as_array)
        .and_then(|rows| {
            rows.iter()
                .find(|row| row.get("key").and_then(Value::as_str) == Some(key))
        })
        .cloned()
        .ok_or_else(|| format!("native claim sweep omitted requested key {key}"))
}

/// One key's full body (the Python try block). Err maps to the per-key
/// accounting: GoneAway/corrupted counters, anything else reap_failed.
#[allow(clippy::too_many_arguments)]
fn reap_one(
    rec: &crate::claims::ClaimRecord,
    row: &Value,
    directory: &Path,
    apply: bool,
    ts: i64,
    ctx: &mut ProbeCtx,
    summary: &mut ReapSummary,
) -> Result<(), PerKeyError> {
    let claim = read_claim_file(&locator(directory, &rec.key)).map_err(per_key_read)?;
    let (dead, bucket) = judge(rec, row, ctx).map_err(PerKeyError::Other)?;
    if !dead {
        summary.note_kept(&bucket);
        if bucket == "suspect_unprobed" {
            summary.note_token(&rec.key, &ctx.reasons);
        }
        return Ok(());
    }
    if !apply {
        summary.would_reap += 1;
        return Ok(());
    }
    let fresh = read_claim_file(&locator(directory, &rec.key)).map_err(per_key_read)?;
    let mut fresh_row = keyed_fresh_row(directory, &rec.key).map_err(PerKeyError::Other)?;
    // pid-shared downgrade: the keyed re-sweep saw the shared live pid, but
    // the pid IS the expired holder, so the fresh verdict collapses back to
    // a suspect.
    if fresh.pid.is_some()
        && fresh.pid == claim.pid
        && row_basis(row) == "pid-shared"
        && row_basis(&fresh_row) == "live"
    {
        fresh_row = serde_json::json!({
            "state": "suspect",
            "basis": "pid-shared",
            "bucket": "suspect",
            "provably_dead": false,
        });
    }
    let (fresh_dead, fresh_bucket) = judge(&fresh, &fresh_row, ctx).map_err(PerKeyError::Other)?;
    if !fresh_dead {
        summary.note_kept(&fresh_bucket);
        if fresh_bucket == "suspect_unprobed" {
            summary.note_token(&fresh.key, &ctx.reasons);
        }
        return Ok(());
    }
    let released = crate::claim_store::force_release_observed_in_directory(
        directory,
        &rec.key,
        "reap proven-dead claim",
        &fresh,
    )
    .map_err(PerKeyError::Other)?;
    let archived = released
        .get("archived")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !archived {
        summary.contended += 1;
        return Ok(());
    }
    summary.reaped += 1;
    if rec.key.starts_with("config-optout:") {
        // Python hands the fresh record to its opt-out sink here (the fold
        // after the sweep restores the recorded prior value through the
        // Python config writer, one restore implementation).
        summary.optout_sink.push(fresh.clone());
    }
    emit_claim_reaped(
        &fresh,
        directory,
        ts - fresh.acquired_at,
        &row_basis(&fresh_row),
    );
    Ok(())
}

fn locator(directory: &Path, key: &str) -> PathBuf {
    directory.join(format!("{}.lock", encode_key(key)))
}

fn emit_claim_reaped(rec: &crate::claims::ClaimRecord, root: &Path, age_ms: i64, basis: &str) {
    let mut data = crate::claims::common_event_data(rec);
    data.insert("root".into(), Value::String(root.display().to_string()));
    data.insert("age_ms".into(), Value::from(age_ms.max(0)));
    data.insert("basis".into(), Value::String(basis.to_string()));
    crate::claims::emit_audit_event(None, "claim_reaped", data);
}

fn emit_claim_reap_swept(summary: &ReapSummary) {
    let value = summary.to_value();
    let data = match value {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    crate::claims::emit_audit_event(None, "claim_reap_swept", data);
}

fn kept_line(summary: &ReapSummary) -> String {
    let mut suspect = format!("{} suspect", summary.kept_suspect);
    if summary.kept_suspect_alive > 0 {
        suspect.push_str(&format!(
            ", {} suspect (worker alive)",
            summary.kept_suspect_alive
        ));
    }
    if summary.kept_suspect_unprobed > 0 {
        let detail = summary
            .kept_suspect_unprobed_by
            .iter()
            .map(|(token, count)| {
                if token == "roster-read-degraded" {
                    format!("{token} {count} (roster not consulted)")
                } else {
                    format!("{token} {count}")
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        suspect.push_str(&format!(
            ", {} suspect (probe unanswered{})",
            summary.kept_suspect_unprobed, suffix
        ));
    }
    if summary.kept_unclassified > 0 {
        let dirs = summary
            .unclassified_dirs
            .iter()
            .map(|(path, count)| format!("{path} {count}"))
            .collect::<Vec<_>>()
            .join(", ");
        suspect.push_str(&format!(
            ", {} unclassified (no native verdict: {dirs})",
            summary.kept_unclassified
        ));
    }
    format!(
        "kept: {} live, {}, {} off-host, {} corrupted, {} vanished, {} contended  |  roots: {}",
        summary.kept_live,
        suspect,
        summary.kept_offhost,
        summary.corrupted,
        summary.vanished,
        summary.contended,
        summary.roots.join(", "),
    )
}

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    // Candidate roots: an explicit --root list, else Python's default pair -
    // the global root plus the None leg, whose claims_dir(None) lands on the
    // $FNO_CLAIMS_ROOT override or the repo's space. Candidates keep Python's
    // raw-root shape; resolution and dedup below turn them into claims dirs,
    // so a cwd whose space IS the global root collapses to one sweep.
    let candidates: Vec<Option<PathBuf>> = match &parsed.roots {
        Some(roots) => roots.iter().map(|p| Some(p.clone())).collect(),
        None => vec![crate::claims::global_claims_root(), None],
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for raw in candidates {
        let dir = match raw {
            Some(root) => root.join(crate::claims::CLAIMS_DIRNAME),
            None => match super::release::default_claims_dir() {
                Some(dir) => dir,
                None => {
                    eprintln!("claims: reap sweep failed: no claims root to sweep");
                    return 1;
                }
            },
        };
        let key = std::fs::canonicalize(&dir).unwrap_or_else(|_| dir.clone());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        dirs.push(dir);
    }
    // The summary carries the resolved claims dirs, never the raw roots
    // (Python's `roots=[str(d) for d in directories]`).
    let mut summary = ReapSummary::new(
        dirs.iter().map(|p| p.display().to_string()).collect(),
        parsed.apply,
    );
    let mut ctx = ProbeCtx::default();
    let ts = crate::claims::now_ms();
    for directory in &dirs {
        // The full sweep runs OUTSIDE the per-key body: a failure hard-fails
        // the whole run (Python's exception escapes; here one line, exit 1).
        let records = match crate::claims::list_in(std::slice::from_ref(directory), None, true) {
            Ok(records) => records,
            Err(err) => {
                eprintln!(
                    "claims: reap sweep failed for {}: {}",
                    directory.display(),
                    err
                );
                return 1;
            }
        };
        let payload =
            crate::claim_commands::claim_sweep_payload_from_records(&records, None, &[], true);
        let rows = payload
            .get("claims")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for (rec, row) in records.iter().zip(rows.iter()) {
            if row.get("state").and_then(Value::as_str) == Some("free") {
                continue;
            }
            summary.scanned += 1;
            let path = locator(directory, &rec.key);
            match reap_one(
                rec,
                row,
                directory,
                parsed.apply,
                ts,
                &mut ctx,
                &mut summary,
            ) {
                Ok(()) => {}
                Err(PerKeyError::GoneAway) => summary.vanished += 1,
                Err(PerKeyError::Corrupted) => summary.corrupted += 1,
                Err(PerKeyError::Other(reason)) => {
                    summary
                        .reap_failed
                        .push((path.display().to_string(), reason));
                }
            }
        }
    }
    if parsed.apply {
        emit_claim_reap_swept(&summary);
    }
    restore_reaped_optouts(&mut summary);
    if parsed.json_output {
        println!("{}", serde_json::to_string(&summary.to_value()).unwrap());
    } else {
        for (path, reason) in &summary.reap_failed {
            eprintln!("FAILED  {path}  ({reason})");
        }
        if parsed.apply {
            println!("reaped {} of {} scanned", summary.reaped, summary.scanned);
        } else {
            println!(
                "would reap {} of {} scanned (dry-run; pass --apply)",
                summary.would_reap, summary.scanned
            );
        }
        println!("{}", kept_line(&summary));
    }
    i32::from(!summary.reap_failed.is_empty())
}

/// The opt-out restore fold: each reaped `config-optout:` claim restores the
/// value its metadata recorded through the native config writer, behind
/// `fno config unset --restore-optout <record>` (this crate does not link
/// the fno crate). The child folds its own failure into one stderr line
/// (`opt-out restore failed: ...`). Failures land in `reap_failed` after the
/// sweep's own failures and before render, so the FAILED lines and the exit
/// code keep their order.
fn restore_reaped_optouts(summary: &mut ReapSummary) {
    for claim in std::mem::take(&mut summary.optout_sink) {
        let payload = match serde_json::to_string(&claim) {
            Ok(payload) => payload,
            Err(err) => {
                summary
                    .reap_failed
                    .push((claim.key.clone(), format!("opt-out restore failed: {err}")));
                continue;
            }
        };
        let output = std::process::Command::new(crate::scrape::fno_bin())
            .args(["config", "unset", "--restore-optout", payload.as_str()])
            .stdin(std::process::Stdio::null())
            .output();
        match output {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let reason = String::from_utf8_lossy(&output.stderr).trim().to_string();
                let reason = if reason.is_empty() {
                    format!(
                        "opt-out restore failed: exit code {}",
                        output.status.code().unwrap_or(-1)
                    )
                } else {
                    reason
                };
                summary.reap_failed.push((claim.key.clone(), reason));
            }
            Err(err) => {
                summary
                    .reap_failed
                    .push((claim.key.clone(), format!("opt-out restore failed: {err}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::{hostname, SCHEMA_VERSION};

    fn now_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    /// The pid-shared downgrade (the Python TestSharedPid shape): the sweep
    /// row judged the claim dead on a shared pid, and the keyed re-sweep
    /// reads the same pid live. The collapse back to suspect must win over
    /// the fresh live verdict, and the unprobed-suspect accounting must show
    /// it. The `target-session:` holder reaches the roster degrade arm with no
    /// subprocess, so the kept bucket is `suspect_unprobed`, never `live`.
    #[test]
    fn pid_shared_downgrade_collapses_fresh_live_verdict() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id() as i32;
        let rec = ClaimRecord {
            schema_version: SCHEMA_VERSION,
            key: "node:test-e".into(),
            holder: "target-session:abc".into(),
            // Same clock domain as the probe: a backward clock step between
            // process start and this line must not read this pid as reuse.
            acquired_at: crate::claims::process_create_time_ms(pid).unwrap_or_else(now_ms),
            pid: Some(pid),
            host: hostname(),
            pid_unavailable: false,
            expires_at: None,
            reason: None,
            harness: None,
            session_id: None,
            pid_provenance: None,
            machine_id: Some(crate::claims::machine_id()),
            metadata: serde_json::Map::new(),
        };
        let path = locator(dir.path(), &rec.key);
        crate::claim_store::seed_at_path(&path, &rec);
        let row = serde_json::json!({
            "key": rec.key,
            "state": "dead",
            "basis": "pid-shared",
            "bucket": "dead",
            "provably_dead": true,
        });
        let mut ctx = ProbeCtx::default();
        let mut summary = ReapSummary::new(Vec::new(), true);
        reap_one(
            &rec,
            &row,
            dir.path(),
            true,
            now_ms(),
            &mut ctx,
            &mut summary,
        )
        .unwrap();
        assert!(
            read_claim_file(&path).is_ok(),
            "claim file must survive the downgrade"
        );
        assert_eq!(summary.reaped, 0, "downgraded suspect is never reaped");
        assert_eq!(summary.kept_live, 0, "fresh live verdict must collapse");
        assert_eq!(summary.kept_suspect_unprobed, 1);
        assert_eq!(
            ctx.reasons.get("node:test-e").map(String::as_str),
            Some("roster-read-degraded")
        );
        assert_eq!(
            summary.kept_suspect_unprobed_by.get("roster-read-degraded"),
            Some(&1)
        );
    }
}
