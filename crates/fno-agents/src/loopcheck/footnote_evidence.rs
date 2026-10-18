//! What did the node's footnote session say about its own claims?
//!
//! fnh checks every claim in its final reply against its records and writes
//! one `footnotes` record per ended run. The done read reports the newest one
//! beside CI and review. It is advisory: a line on the done reason and a
//! `footnote_evidence` object on the terminal `loop_check` row, never a block.
//! The row's `agrees_with_ci_and_review` gives the false-hold rate a
//! denominator before anyone makes the read blocking.

use crate::footnote_harness::transcript::{find_session_dir, transcript_file};
use crate::state::RegistryEntry;
use serde::Serialize;
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The claims the reason line names before it folds the rest into a count.
const NAMED_CLAIMS: usize = 3;
const CLAIM_TEXT_CHARS: usize = 120;
/// A run writes its `footnotes` record at its end, so the newest one sits in
/// the tail. The bound keeps an advisory read inside the stop-hook budget
/// however long the transcript grows.
const TAIL_BYTES: u64 = 1024 * 1024;

#[derive(Debug, PartialEq, Serialize)]
pub(super) struct Flagged {
    pub verdict: String,
    pub text: String,
    pub sources: Vec<String>,
}

#[derive(Debug, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(super) enum FootnoteEvidence {
    /// No footnote session is bound to the node, or none wrote verdicts.
    Absent,
    /// The newest verdicts name another commit, so they are never counted.
    Stale {
        session: String,
        head_sha: Option<String>,
        pr_head: String,
    },
    Counted {
        session: String,
        head_sha: String,
        claims_total: u64,
        claims_rejected: u64,
        claims_missing: u64,
        flagged: Vec<Flagged>,
        /// The row rides only a green terminal no reviewer objected to, so a
        /// verdict with a rejected or missing claim would have held a PR that
        /// CI and review passed.
        agrees_with_ci_and_review: bool,
    },
    /// A bound session's record could not be read. It wins over the rest:
    /// the unread record could hold the newest verdict.
    Unreadable { session: String, error: String },
}

/// The record file of every footnote session bound to `node`, plus the
/// footnote session running this target loop itself: its registry row may
/// name no node when the node came from the manifest. A pane row binds the
/// TUI's id as `harness_session_id`; a headless row sets both ids to one.
pub(super) fn records_for(
    entries: &[RegistryEntry],
    node: Option<&str>,
    own_session: Option<&str>,
    sessions_root: &Path,
) -> Vec<(String, PathBuf)> {
    entries
        .iter()
        .filter(|e| e.harness_name() == "footnote" && node.is_some() && e.node.as_deref() == node)
        .filter_map(|e| e.harness_session_id.clone().or_else(|| e.fno_id.clone()))
        .chain(own_session.map(str::to_string))
        .filter_map(|id| {
            let path = transcript_file(&find_session_dir(sessions_root, &id)?, &id);
            Some((id, path))
        })
        .collect()
}

/// The newest `footnotes` record in the tail of one transcript.
fn last_footnotes(path: &Path) -> Result<Option<Value>, String> {
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut file = std::fs::File::open(path).map_err(err)?;
    let len = file.metadata().map_err(err)?.len();
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(len.saturating_sub(TAIL_BYTES)))
        .and_then(|_| file.read_to_end(&mut tail))
        .map_err(err)?;
    Ok(String::from_utf8_lossy(&tail)
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|r| r["type"] == "footnotes"))
}

/// The newest `footnotes` record across `records`, judged against `pr_head`.
pub(super) fn read_evidence(records: &[(String, PathBuf)], pr_head: &str) -> FootnoteEvidence {
    let mut newest: Option<(i64, String, Value)> = None;
    for (session, path) in records {
        let rec = match last_footnotes(path) {
            Ok(Some(rec)) => rec,
            Ok(None) => continue,
            Err(error) => {
                return FootnoteEvidence::Unreadable {
                    session: session.clone(),
                    error,
                }
            }
        };
        let at = rec["ts"]
            .as_str()
            .and_then(|ts| chrono::DateTime::parse_from_rfc3339(ts).ok())
            .map_or(i64::MIN, |t| t.timestamp_millis());
        if newest.as_ref().is_none_or(|(best, _, _)| at > *best) {
            newest = Some((at, session.clone(), rec["data"].clone()));
        }
    }
    let Some((_, session, data)) = newest else {
        return FootnoteEvidence::Absent;
    };
    let head_sha = data["head_sha"].as_str().map(str::to_string);
    let Some(head_sha) = head_sha
        .clone()
        .filter(|h| !pr_head.is_empty() && h.eq_ignore_ascii_case(pr_head))
    else {
        return FootnoteEvidence::Stale {
            session,
            head_sha,
            pr_head: pr_head.to_string(),
        };
    };
    // The record's count fields restate `claims`, so the claims are the one
    // source; a check the harness did not run stays out of the total.
    let claims = data["claims"].as_array().cloned().unwrap_or_default();
    let tally = |verdict: &str| claims.iter().filter(|c| c["verdict"] == verdict).count() as u64;
    let (rejected, missing) = (tally("rejected"), tally("missing"));
    FootnoteEvidence::Counted {
        session,
        head_sha,
        claims_total: claims.len() as u64 - tally("not_run"),
        claims_rejected: rejected,
        claims_missing: missing,
        flagged: claims
            .iter()
            .filter(|c| c["verdict"] == "rejected" || c["verdict"] == "missing")
            .map(flag)
            .collect(),
        agrees_with_ci_and_review: rejected + missing == 0,
    }
}

/// A flagged claim and the sources of the footnotes that failed it. A claim
/// takes its worst footnote verdict, so a flagged claim with footnotes has a
/// failing one.
fn flag(claim: &Value) -> Flagged {
    let notes = claim["footnotes"].as_array().cloned().unwrap_or_default();
    Flagged {
        verdict: claim["verdict"].as_str().unwrap_or("").to_string(),
        text: claim["text"].as_str().unwrap_or("").to_string(),
        sources: notes
            .iter()
            .filter(|n| n["verdict"] != "accepted")
            .filter_map(|n| n["source"].as_str().map(str::to_string))
            .collect(),
    }
}

/// The evidence from the live registry and sessions root. An undeclared
/// home reads as absent before any root resolves, and an unreadable registry
/// binds no rows: an advisory line must not invent a session.
pub(super) fn read_live(
    node: Option<&str>,
    own_session: Option<&str>,
    pr_head: &str,
) -> FootnoteEvidence {
    if node.is_none() && own_session.is_none() {
        return FootnoteEvidence::Absent;
    }
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return FootnoteEvidence::Absent;
    };
    let entries = crate::state::load_registry(&home.registry_json())
        .map(|r| r.entries)
        .unwrap_or_default();
    let root = crate::footnote_harness::transcript::sessions_root();
    read_evidence(&records_for(&entries, node, own_session, &root), pr_head)
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

impl FootnoteEvidence {
    /// `done_msg` with the evidence line appended, unchanged when absent.
    pub(super) fn on_reason(&self, done_msg: String) -> String {
        let line = match self {
            FootnoteEvidence::Absent => return done_msg,
            FootnoteEvidence::Stale {
                head_sha, pr_head, ..
            } => format!(
                "footnote verdicts stale (advisory): record at {}, PR head {}; not counted",
                head_sha.as_deref().map_or("no commit", short),
                short(pr_head)
            ),
            FootnoteEvidence::Unreadable { session, error } => {
                format!("footnote verdicts unreadable (advisory): session {session}: {error}")
            }
            FootnoteEvidence::Counted {
                head_sha,
                claims_total,
                claims_rejected,
                claims_missing,
                flagged,
                ..
            } => {
                let mut line = format!(
                    "footnote verdicts at {} (advisory): {claims_total} claims, {claims_rejected} rejected, {claims_missing} missing",
                    short(head_sha)
                );
                for f in flagged.iter().take(NAMED_CLAIMS) {
                    let text: String = f.text.chars().take(CLAIM_TEXT_CHARS).collect();
                    line.push_str(&format!("; {} {text:?}", f.verdict));
                    if !f.sources.is_empty() {
                        line.push_str(&format!(" (source: {})", f.sources.join(", ")));
                    }
                }
                if flagged.len() > NAMED_CLAIMS {
                    line.push_str(&format!("; {} more", flagged.len() - NAMED_CLAIMS));
                }
                line
            }
        };
        format!("{done_msg}; {line}")
    }

    /// `row` with a `footnote_evidence` key, unchanged when absent.
    pub(super) fn on_row(&self, mut row: Value) -> Value {
        if let (false, Some(obj)) = (self == &FootnoteEvidence::Absent, row.as_object_mut()) {
            obj.insert("footnote_evidence".into(), serde_json::json!(self));
        }
        row
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write as _;

    const HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeef00000001";

    /// One footnote session bound to `x-aaaa` whose final `footnotes` record
    /// sits at `head`, with one accepted and one rejected claim.
    fn bound_at(root: &Path, head: &str) -> Vec<(String, PathBuf)> {
        let sid = "0199aaaa-0000-7000-8000-000000000001";
        let dir = root.join("proj").join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        let mut f = std::fs::File::create(transcript_file(&dir, sid)).unwrap();
        let claims = json!([
            {"text": "tests pass", "verdict": "accepted",
             "footnotes": [{"source": "run:cargo test", "verdict": "accepted"}]},
            {"text": "the reader handles stale heads", "verdict": "rejected",
             "footnotes": [{"source": "diff:src/a.rs", "verdict": "rejected"},
                           {"source": "read:src/b.rs", "verdict": "accepted"}]}
        ]);
        for (ty, data) in [
            ("header", json!({"cwd": "/w", "harness": "footnote"})),
            (
                "footnotes",
                json!({"ending": "done", "head_sha": head, "claims": claims}),
            ),
        ] {
            let rec = json!({"v": 1, "ts": "2026-10-18T00:00:00.000Z", "session_id": sid,
                "type": ty, "data": data, "ignorable": ty == "footnotes"});
            writeln!(f, "{rec}").unwrap();
        }
        let row = RegistryEntry {
            harness: Some("footnote".into()),
            node: Some("x-aaaa".into()),
            ..RegistryEntry::new(
                Some(sid.into()),
                crate::state::Lineage::captured((None, None, None)),
            )
        };
        let rows = std::slice::from_ref(&row);
        assert!(records_for(rows, Some("x-0000"), None, root).is_empty());
        // The loop's own footnote session binds with no node on its row.
        assert_eq!(records_for(&[], None, Some(sid), root).len(), 1);
        records_for(rows, Some("x-aaaa"), None, root)
    }

    /// AC5-HP: a rejected claim at the PR head is named in the done reason
    /// with its failing source, and its row disagrees with CI and review.
    #[test]
    fn a_rejected_claim_at_the_pr_head_is_named_in_the_done_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let ev = read_evidence(&bound_at(tmp.path(), HEAD), HEAD);
        let msg = ev.on_reason("PR #1 is green and reviewed".into());
        assert!(
            msg.ends_with(
                "at deadbee (advisory): 2 claims, 1 rejected, 0 missing; \
                 rejected \"the reader handles stale heads\" (source: diff:src/a.rs)"
            ),
            "{msg}"
        );
        let row = ev.on_row(json!({"done_probes": null}));
        assert_eq!(row["footnote_evidence"]["status"], "counted");
        assert_eq!(row["footnote_evidence"]["agrees_with_ci_and_review"], false);
    }

    /// AC5-ERR: a record at an older head reads stale and counts nothing;
    /// absent evidence leaves the reason and the row unchanged.
    #[test]
    fn an_older_head_reads_stale_and_absent_evidence_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let ev = read_evidence(&bound_at(tmp.path(), "abcdef0123456789"), HEAD);
        let msg = ev.on_reason("done".into());
        assert!(
            msg.ends_with("record at abcdef0, PR head deadbee; not counted"),
            "{msg}"
        );
        assert!(!msg.contains("rejected"), "{msg}");
        let none = FootnoteEvidence::Absent;
        assert_eq!(none.on_reason("done".into()), "done");
        assert_eq!(none.on_row(json!({"a": 1})), json!({"a": 1}));
    }
}
