//! Review attestations on the shared primary.
//!
//! The event store stays local per machine (multiplayer.md), but a
//! head-pinned `review_attestation` is gate evidence for a merge that may
//! run on another machine: reviewed on the iMac, merged from the MacBook,
//! the merge read only the MacBook's journals and paid a second review
//! round. So every committed attestation ALSO lands here when
//! `store.remote_url` is set, and every gate read (loop-check's attestation
//! chain, the merge coverage read) pulls the rows back. The local journal
//! stays the durable record: a push is best-effort after the local commit,
//! and a pull failure degrades to machine-local coverage, never a refusal.
//!
//! The primary table is not a `SHARED_TABLES` member: those ride graph.db's
//! outbox and change log, and the event store is a different database. This
//! carrier writes through its own verbs, so an older build cannot corrupt
//! rows and needs no guard trigger.

use crate::finalize::repo_identity_from_git_remote;
use crate::store_remote::{configured, Remote, SqlValue};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub(crate) const TABLE: &str = "review_attestation";

fn ensure_table(remote: &Remote) -> Result<(), String> {
    remote.script(&format!(
        "CREATE TABLE IF NOT EXISTS {TABLE} (\
             repo TEXT NOT NULL, \
             ts TEXT NOT NULL DEFAULT '', \
             payload_hash TEXT NOT NULL, \
             envelope TEXT NOT NULL, \
             PRIMARY KEY (repo, payload_hash))"
    ))
}

/// One committed envelope line onto the primary. Idempotent: the same
/// committed line hashes the same, so a retry after an ambiguous timeout is
/// a no-op (the same at-least-once posture the mail bus takes). The repo
/// slug is stamped into `data.repo` when the envelope lacks it, because the
/// pull scopes by repo and the local journal does not always carry the
/// field.
pub(crate) fn push_line(line: &str, slug: &str, remote: &Remote) -> Result<(), String> {
    let mut envelope: Value = serde_json::from_str(line.trim())
        .map_err(|e| format!("attest push: envelope is not JSON: {e}"))?;
    if envelope.get("type").and_then(Value::as_str) != Some("review_attestation") {
        return Err("attest push: only review_attestation lines ride the primary".to_string());
    }
    if slug.is_empty() {
        return Err(
            "attest push: no repo identity here; the row would be unreachable on the primary, so the journal copy alone carries it"
                .to_string(),
        );
    }
    let ts = envelope
        .get("ts")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if !slug.is_empty() {
        if let Some(data) = envelope.get_mut("data").and_then(Value::as_object_mut) {
            let needs_stamp = data
                .get("repo")
                .and_then(Value::as_str)
                .map(str::is_empty)
                .unwrap_or(true);
            if needs_stamp {
                data.insert("repo".to_string(), json!(slug));
            }
            // The writing machine id: a primary row is a review some machine
            // wrote, and the gate's coverage listing names which. A row
            // already carrying one keeps its original writer (a peer
            // re-pushing a pulled line never claims it).
            let machine = crate::claims::machine_id();
            if !machine.is_empty() && !data.contains_key("attest_machine") {
                data.insert("attest_machine".to_string(), json!(machine));
            }
        }
    }
    let payload_hash = blake3::hash(envelope.to_string().as_bytes())
        .to_hex()
        .to_string();
    ensure_table(remote)?;
    remote.execute(
        &format!(
            "INSERT OR IGNORE INTO {TABLE} (repo, ts, payload_hash, envelope) VALUES (?, ?, ?, ?)"
        ),
        &[
            SqlValue::Text(slug.to_string()),
            SqlValue::Text(ts),
            SqlValue::Text(payload_hash),
            SqlValue::Text(envelope.to_string()),
        ],
    )?;
    Ok(())
}

/// This repo's attestation rows on the primary, envelope lines oldest
/// first. The caller dedups against the local journals (the same row can
/// live in both when this machine did the review). No `ensure_table` here:
/// a primary that never saw a push has no table, and a missing table is NO
/// ROWS, not an outage - the SELECT answers `no such table` and the read
/// degrades to machine-local coverage. (Creating the table on read would
/// make every gate read a schema write on the shared store.)
pub(crate) fn pull_lines(slug: &str, remote: &Remote) -> Result<Vec<String>, String> {
    let reply = match remote.execute(
        &format!("SELECT envelope FROM {TABLE} WHERE repo = ? ORDER BY ts, rowid"),
        &[SqlValue::Text(slug.to_string())],
    ) {
        Ok(reply) => reply,
        Err(e) if e.contains("no such table") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(reply
        .rows
        .iter()
        .filter_map(|row| row.first())
        .filter_map(|cell| cell.text().map(str::to_string))
        .collect())
}

fn repo_slug(cwd: &Path) -> String {
    repo_identity_from_git_remote(cwd).unwrap_or_default()
}

/// The client's one-line dispatch for `fno-agents attest <action>`, here so
/// client.rs stays at its file budget. The cwd resolves here too: the slug
/// comes from the caller's checkout, and the client grows by three lines.
pub fn dispatch(rest: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    match rest.first().map(String::as_str) {
        Some("push") => run_push(&cwd),
        Some("pull") => run_pull(&rest[1..], &cwd),
        Some("retract") => run_retract(&rest[1..], &cwd),
        _ => {
            eprintln!(
                "usage: fno-agents attest push|pull|retract [--repo <slug>]  (push reads one envelope line on stdin)"
            );
            2
        }
    }
}

/// The in-process mirror the commit path calls (both crates name this
/// symbol; the generated copy in the native CLI crate shells out to this
/// binary instead). Fire-and-forget: the local row is already committed,
/// and the store's own connect and reply waits (3s and 15s) bound the
/// thread, so the emitting process never blocks on the network for it.
pub fn maybe_push(ty: &str, line: &str, suppressed: bool) {
    if suppressed || ty != "review_attestation" {
        return;
    }
    let line = line.to_string();
    std::thread::Builder::new()
        .name("attest-push".to_string())
        .spawn(move || {
            let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let slug = repo_slug(&cwd);
            let Ok(Some(remote)) = configured() else {
                return;
            };
            if let Err(e) = push_line(&line, &slug, &remote) {
                eprintln!("attest push: skipped ({e})");
            }
        })
        .ok();
}

/// `fno-agents attest push`: one envelope line on stdin. Unset
/// `store.remote_url` is the stock install, so it exits 0 with a note; a
/// malformed URL or an unreachable primary exits nonzero and names the
/// cause, and the caller's push stays best-effort.
pub fn run_push(cwd: &Path) -> i32 {
    use std::io::Read;

    let mut line = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut line) {
        eprintln!("error: attest push: stdin read failed: {e}");
        return 1;
    }
    let line = line.trim().to_string();
    if line.is_empty() {
        return 0;
    }
    match configured() {
        Ok(None) => {
            eprintln!("attest push: store.remote_url unset; the attestation stays local");
            0
        }
        Ok(Some(remote)) => match push_line(&line, &repo_slug(cwd), &remote) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

/// `fno-agents attest pull [--repo <slug>]`: this repo's primary rows as
/// envelope lines on stdout. Exit 3 on an unreachable primary, the same
/// code a shared claim verb answers with.
pub fn run_pull(args: &[String], cwd: &Path) -> i32 {
    let debug = std::env::var_os("FNO_ATTEST_DEBUG").is_some();
    let mut slug = repo_slug(cwd);
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        if tok == "--repo" {
            if let Some(v) = it.next() {
                slug = v.clone();
            }
        }
    }
    if slug.is_empty() {
        eprintln!("error: attest pull: no repo identity (no git remote, no --repo)");
        return 2;
    }
    match configured() {
        Ok(None) => {
            if debug {
                eprintln!("attest pull: store.remote_url unset");
            }
            0
        }
        Ok(Some(remote)) => match pull_lines(&slug, &remote) {
            Ok(lines) => {
                if debug {
                    eprintln!(
                        "attest pull: slug={slug} rows={} url={}",
                        lines.len(),
                        remote.url()
                    );
                }
                for line in lines {
                    println!("{line}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                3
            }
        },
        Err(e) => {
            eprintln!("error: {e}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store_remote::test_primary;

    fn line(ts: &str, reviewer: &str, head: &str) -> String {
        json!({
            "type": "review_attestation",
            "ts": ts,
            "data": {"reviewer": reviewer, "head_sha": head, "verdict": "pass"},
        })
        .to_string()
    }

    #[test]
    fn the_push_stamps_the_writing_machine_and_keeps_the_original_writer() {
        let primary = test_primary::start();
        // A row already carrying a machine keeps its writer: a peer re-pushing
        // a pulled line never claims it.
        let mut foreign: Value =
            serde_json::from_str(&line("2026-10-13T09:00:00Z", "peer", "bbb")).unwrap();
        foreign["data"]["attest_machine"] = json!("foreign-machine");
        push_line(&foreign.to_string(), "github.com/o/r", &primary.remote).unwrap();
        push_line(
            &line("2026-10-13T09:00:01Z", "code-review", "ccc"),
            "github.com/o/r",
            &primary.remote,
        )
        .unwrap();
        let rows = pull_lines("github.com/o/r", &primary.remote).unwrap();
        let first: Value = serde_json::from_str(&rows[0]).unwrap();
        assert_eq!(
            first
                .pointer("/data/attest_machine")
                .and_then(Value::as_str),
            Some("foreign-machine")
        );
        // This machine's own row carries its id, whatever the host answers:
        // an unreadable host id is unknown, not a wrong stamp.
        let second: Value = serde_json::from_str(&rows[1]).unwrap();
        let machine = crate::claims::machine_id();
        assert_eq!(
            second
                .pointer("/data/attest_machine")
                .and_then(Value::as_str),
            if machine.is_empty() {
                None
            } else {
                Some(machine.as_str())
            },
        );
    }

    #[test]
    fn push_then_pull_roundtrips_once_scoped_and_refuses_non_attestations() {
        let primary = test_primary::start();
        let wrong = json!({"type": "review_coverage", "ts": "t", "data": {}}).to_string();
        assert!(push_line(&wrong, "github.com/o/r", &primary.remote).is_err());
        let once = line("2026-10-13T09:00:00Z", "code-review", "abc123");
        push_line(&once, "github.com/o/r", &primary.remote).unwrap();
        // The same committed line hashes the same: the retry is a no-op.
        push_line(&once, "github.com/o/r", &primary.remote).unwrap();
        // Another repo's row stays scoped out of this repo's pull.
        push_line(
            &line("2026-10-13T09:00:01Z", "peer", "bbb"),
            "github.com/o/other",
            &primary.remote,
        )
        .unwrap();
        let rows = pull_lines("github.com/o/r", &primary.remote).unwrap();
        assert_eq!(rows.len(), 1);
        let val: Value = serde_json::from_str(&rows[0]).unwrap();
        assert_eq!(
            val.pointer("/data/repo").and_then(Value::as_str),
            Some("github.com/o/r")
        );
        assert_eq!(
            val.pointer("/data/head_sha").and_then(Value::as_str),
            Some("abc123")
        );
    }

    /// The conflict refusal byte-matches the Python exception text the
    /// attest_retract golden froze (repr-style single quotes); real ancestry
    /// cannot be faked from an integration process.
    #[test]
    fn retraction_conflict_message_matches_the_python_text() {
        assert_eq!(
            retraction_conflict_message(
                "CLAUDE_CODE_SESSION_ID",
                "ancestor-session",
                "env-session"
            ),
            "error: attester identity conflict on CLAUDE_CODE_SESSION_ID: process ancestry \
carries 'ancestor-session', the emitting environment carries 'env-session'. No retraction \
emitted under an identity the caller overrode."
        );
    }
}

// ── retraction: revoke a forged pass by naming the pair that passed ───────

fn resolve_retractor_identity(
) -> Result<(Option<String>, crate::spawn_context::AttesterWitness), String> {
    if std::env::var("FNO_ATTEST_ENV_ONLY").as_deref() == Ok("1") {
        return Ok((None, crate::spawn_context::AttesterWitness::EnvOnly));
    }
    let get = |name: &str| std::env::var(name).ok();
    let (id, witness) = crate::spawn_context::resolve_attester_identity(&get);
    Ok((id, witness))
}

/// `fno-agents attest retract --reviewer R --attester A --head H --reason W
/// [--events <journal>]`. Ports cli/src/fno/pr/_attestation.py (deleted in
/// the same change): scans the named journal plus the machine-global mirror
/// for a passing attestation matching the triple, refuses rather than
/// writing a revocation for a pair that never passed, and records the
/// RETRACTING process's own identity so the audit trail says who revoked.
fn run_retract(args: &[String], cwd: &Path) -> i32 {
    let mut reviewer = String::new();
    let mut attester = String::new();
    let mut head = String::new();
    let mut reason = String::new();
    let mut events_arg: Option<PathBuf> = None;
    let mut it = args.iter();
    while let Some(tok) = it.next() {
        match tok.as_str() {
            "--reviewer" => reviewer = it.next().cloned().unwrap_or_default(),
            "--attester" => attester = it.next().cloned().unwrap_or_default(),
            "--head" => head = it.next().cloned().unwrap_or_default(),
            "--reason" | "-R" => reason = it.next().cloned().unwrap_or_default(),
            "--events" => events_arg = it.next().map(PathBuf::from),
            _ => {}
        }
    }
    retract_body(
        cwd,
        &reviewer,
        &attester,
        &head,
        &reason,
        events_arg.as_deref(),
    )
}

fn retract_body(
    cwd: &Path,
    reviewer: &str,
    attester: &str,
    head: &str,
    reason: &str,
    events: Option<&Path>,
) -> i32 {
    let journal = match events {
        Some(p) => p.to_path_buf(),
        None => crate::paths::events_path(cwd),
    };
    let pass = find_pass(&journal, reviewer, attester, head);
    let Some(pass) = pass else {
        eprintln!(
            "no passing attestation for reviewer '{reviewer}' attester '{attester}' at head {} in {}; nothing retracted",
            head.get(..12).unwrap_or(head),
            journal.display()
        );
        return 1;
    };
    let _ = pass;
    retract_emit(cwd, &journal, reviewer, attester, head, reason)
}

/// The newest passing attestation matching the (reviewer, attester, head)
/// triple, over the journal plus the machine-global mirror. A pass the verb
/// cannot see is one it cannot revoke - the irretractable-forgery shape
/// the verb exists to close.
fn find_pass(journal: &Path, reviewer: &str, attester: &str, head: &str) -> Option<Value> {
    let key = reviewer.trim_start_matches('/');
    let mut logs = vec![journal.to_path_buf()];
    if let Ok(global) = crate::paths_cli::global_events_json(Path::new(".")).canonicalize() {
        if global
            != journal
                .canonicalize()
                .unwrap_or_else(|_| journal.to_path_buf())
        {
            logs.push(global);
        }
    }
    let mut found: Option<Value> = None;
    for log in logs {
        let Ok(text) = crate::event_store::journal_text_checked(
            &log,
            &crate::event_store::EventQuery::of_types(&["review_attestation"]),
        ) else {
            continue;
        };
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let Ok(val) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if val.get("type").and_then(Value::as_str) != Some("review_attestation") {
                continue;
            }
            let data = val.get("data").cloned().unwrap_or(Value::Null);
            let row_reviewer = data
                .get("reviewer")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim_start_matches('/');
            if row_reviewer != key
                || data.get("attester_session_id").and_then(Value::as_str) != Some(attester)
                || data.get("head_sha").and_then(Value::as_str) != Some(head)
                || data.get("verdict").and_then(Value::as_str) != Some("pass")
            {
                continue;
            }
            found = Some(data);
        }
    }
    found
}

/// Emit the retraction: identity, manifest keys, the fail row, the append,
/// the global mirror, the receipt. Ports `_attestation.retract`'s second
/// half; a failed mirror prints the same warning and still exits 0.
fn retract_emit(
    cwd: &Path,
    journal: &Path,
    reviewer: &str,
    attester: &str,
    head: &str,
    reason: &str,
) -> i32 {
    let (resolved_id, witness) = match resolve_retractor_identity() {
        Ok((id, w)) => (id, w),
        Err(e) => {
            eprintln!("error: {e}. No retraction emitted under an identity the caller overrode.");
            return 1;
        }
    };
    if let crate::spawn_context::AttesterWitness::Conflict(marker, ancestor, env) = witness {
        eprintln!("{}", retraction_conflict_message(&marker, &ancestor, &env));
        return 1;
    }
    let (session_id, harness) = manifest_identity(cwd);
    let branch = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let mut data = serde_json::Map::new();
    data.insert("reviewer".into(), json!(reviewer.trim_start_matches('/')));
    data.insert("head_sha".into(), json!(head));
    data.insert("verdict".into(), json!("fail"));
    data.insert("session_id".into(), json!(session_id));
    data.insert("retracts_attester".into(), json!(attester));
    data.insert(
        "attester_session_id".into(),
        json!(resolved_id.clone().unwrap_or_default()),
    );
    data.insert("attester_witness".into(), json!(witness_name(&witness)));
    data.insert("retraction_reason".into(), json!(reason));
    data.insert("branch".into(), json!(branch));
    if !harness.is_empty() {
        data.insert("harness".into(), json!(harness));
    }
    let source = if session_id.is_empty() {
        "test"
    } else {
        "target"
    };
    let event = json!({
        "ts": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        "type": "review_attestation",
        "source": source,
        "data": Value::Object(data),
    });
    let line = event.to_string();
    if let Err(e) = crate::event_store::append_envelope(journal, &line, None) {
        eprintln!("error: failed to append retraction: {e}");
        return 1;
    }
    let global = crate::paths_cli::global_events_json(cwd);
    if global != journal {
        if let Err(e) = crate::event_store::append_envelope(&global, &line, None) {
            eprintln!("warning: retraction not mirrored to the global log: {e}");
        }
    }
    let id_shown = resolved_id.unwrap_or_else(|| "unattributed".into());
    println!(
        "retracted: reviewer={reviewer} attester={attester} head={} by={id_shown} reason={reason}",
        head.get(..12).unwrap_or(head)
    );
    0
}

fn witness_name(w: &crate::spawn_context::AttesterWitness) -> String {
    match w {
        crate::spawn_context::AttesterWitness::Process => "process".to_string(),
        crate::spawn_context::AttesterWitness::EnvOnly => "env_only".to_string(),
        crate::spawn_context::AttesterWitness::Conflict(..) => "conflict".to_string(),
    }
}

/// The conflict refusal, verbatim the Python exception text (repr-style
/// single quotes) so the golden holds byte-for-byte. Extracted for the unit
/// test: real ancestry cannot be faked from an integration process.
fn retraction_conflict_message(marker: &str, ancestor: &str, env: &str) -> String {
    format!(
        "error: attester identity conflict on {marker}: process ancestry carries '{ancestor}', \
the emitting environment carries '{env}'. No retraction emitted under an identity the caller overrode."
    )
}

/// session_id/harness from the worktree manifest when one is bound - the
/// same grep the attestation producer performs. A bare-shell retraction
/// leaves them empty and the journal stays honest about it.
fn manifest_identity(cwd: &Path) -> (String, String) {
    let manifest = cwd.join(".fno/target-state.md");
    let Ok(text) = std::fs::read_to_string(&manifest) else {
        return (String::new(), String::new());
    };
    let mut session_id = String::new();
    let mut harness = String::new();
    for line in text.lines() {
        let stripped = line.trim();
        for key in ["session_id", "harness"] {
            let prefix = format!("{key}:");
            if let Some(value) = stripped.strip_prefix(&prefix) {
                let value = value.trim();
                if !value.is_empty() && value != "null" {
                    if key == "session_id" {
                        session_id = value.to_string();
                    } else {
                        harness = value.to_string();
                    }
                }
            }
        }
    }
    (session_id, harness)
}
