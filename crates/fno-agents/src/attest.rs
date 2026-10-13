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
use std::path::Path;

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
/// live in both when this machine did the review).
pub(crate) fn pull_lines(slug: &str, remote: &Remote) -> Result<Vec<String>, String> {
    ensure_table(remote)?;
    let reply = remote.execute(
        &format!("SELECT envelope FROM {TABLE} WHERE repo = ? ORDER BY ts, rowid"),
        &[SqlValue::Text(slug.to_string())],
    )?;
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
        _ => {
            eprintln!("usage: fno-agents attest push|pull [--repo <slug>]  (push reads one envelope line on stdin)");
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
    fn push_then_pull_roundtrips_and_stamps_the_repo() {
        let primary = test_primary::start();
        push_line(
            &line("2026-10-13T09:00:00Z", "code-review", "abc123"),
            "github.com/o/r",
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

    #[test]
    fn the_same_line_pushes_once() {
        let primary = test_primary::start();
        let once = line("2026-10-13T09:00:00Z", "peer", "def456");
        push_line(&once, "github.com/o/r", &primary.remote).unwrap();
        push_line(&once, "github.com/o/r", &primary.remote).unwrap();
        assert_eq!(
            pull_lines("github.com/o/r", &primary.remote).unwrap().len(),
            1
        );
    }

    #[test]
    fn pull_scopes_by_repo() {
        let primary = test_primary::start();
        push_line(
            &line("2026-10-13T09:00:00Z", "peer", "aaa"),
            "github.com/o/r",
            &primary.remote,
        )
        .unwrap();
        push_line(
            &line("2026-10-13T09:00:01Z", "peer", "bbb"),
            "github.com/o/other",
            &primary.remote,
        )
        .unwrap();
        assert_eq!(
            pull_lines("github.com/o/r", &primary.remote).unwrap().len(),
            1
        );
    }

    #[test]
    fn a_non_attestation_line_refuses() {
        let primary = test_primary::start();
        let wrong = json!({"type": "review_coverage", "ts": "t", "data": {}}).to_string();
        assert!(push_line(&wrong, "github.com/o/r", &primary.remote).is_err());
    }
}
