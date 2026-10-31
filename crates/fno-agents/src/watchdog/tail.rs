//! The one transcript read per tick: the tail classifier, the outage collector
//! and the poll detector all derive from this single parse.

use crate::backlog::worked::{
    claude_projects_root, normalize_record, record_epoch, record_text, resolve_transcript,
};
use crate::claude_transcript_paths::{store_listing, Hit};
use serde_json::Value;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Transcripts grow without bound and the classifier only asks about the last
/// turn, so read the trailing 256 KiB.
const TICK_TAIL_BYTES: u64 = 256 * 1024;
/// 60, not 15: a chatty attach must not push a still-live 429 out of the window
/// the wake gate reads.
pub(crate) const TAIL_RECORDS: usize = 60;
/// Confirmation scans deeper, so a landed wake message is never read as refused
/// because the attach that followed it was chatty.
pub(crate) const CONFIRM_RECORDS: usize = 120;

/// One PR fact from a transcript record.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrPoll {
    Read(i64),
    Settled(i64, String),
}

/// A resolved transcript's tail. `records` is newest-last; `last_role` and
/// `last_text` describe the last role-bearing record inside the window. No
/// transcript is `None` at the caller, a different fact from a quiet one.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct TailFacts {
    pub(crate) records: Vec<(Option<f64>, String)>,
    pub(crate) last_event_epoch: Option<f64>,
    pub(crate) tail_text: String,
    pub(crate) last_role: Option<String>,
    pub(crate) last_text: String,
    pub(crate) pr_polls: Vec<PrPoll>,
}

/// The transcript listing one sweep shares, so no row walks the store itself.
pub(crate) fn sweep_listing() -> Vec<Hit> {
    store_listing(&claude_projects_root())
}

/// A session's parsed tail records. None means the transcript could not be
/// resolved, read or decoded. `agent` has no default: a defaulted read opens the
/// claude store for every other harness and reads every such worker as a ghost.
pub(crate) fn tail_entries(
    session_id: &str,
    cwd: &str,
    agent: &str,
    listing: &[Hit],
) -> Option<Vec<Value>> {
    let path: PathBuf = resolve_transcript(agent, session_id, cwd, listing)?;
    read_tail(&path)
}

fn read_tail(path: &Path) -> Option<Vec<Value>> {
    let size = std::fs::metadata(path).ok()?.len();
    let start = size.saturating_sub(TICK_TAIL_BYTES);
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if size > TICK_TAIL_BYTES {
        // A mid-file seek lands inside a line; drop it.
        lines.next();
    }
    let mut parsed = Vec::new();
    for line in lines {
        if let Ok(record) = serde_json::from_str::<Value>(line) {
            if record.is_object() {
                parsed.push(record);
            }
        }
    }
    Some(parsed)
}

/// Pure derivation of the facts from a parsed tail. The window bounds everything
/// downstream, the role and text pair included: a pair read from an older record
/// would pair a stale text with the fresh age it is classified against.
pub(crate) fn facts_from_entries(
    entries: Option<&[Value]>,
    max_records: usize,
) -> Option<TailFacts> {
    let entries = entries?;
    let mut windowed: Vec<(Option<f64>, String, Option<String>, Vec<PrPoll>)> =
        Vec::with_capacity(entries.len());
    for record in entries {
        let record = normalize_record(record.clone());
        let role = record
            .get("message")
            .and_then(|m| m.get("role"))
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
        windowed.push((
            record_epoch(&record),
            record_text(&record),
            role,
            pr_poll_record(&record),
        ));
    }
    let skip = windowed.len().saturating_sub(max_records);
    let window = &windowed[skip..];
    let records: Vec<(Option<f64>, String)> =
        window.iter().map(|(e, t, _, _)| (*e, t.clone())).collect();
    let last_event_epoch = records.iter().rev().find_map(|(e, _)| *e);
    let mut last_role = None;
    let mut last_text = String::new();
    for (_, text, role, _) in window.iter().rev() {
        if role.is_some() {
            last_role = role.clone();
            last_text = text.clone();
            break;
        }
    }
    let pr_polls: Vec<PrPoll> = window.iter().flat_map(|w| w.3.iter().cloned()).collect();
    let tail_text = records
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    Some(TailFacts {
        records,
        last_event_epoch,
        tail_text,
        last_role,
        last_text,
        pr_polls,
    })
}

/// Resolve and tail-read a session. Never fails loudly: a missing transcript is
/// None, which the classifier renders as a fact.
pub(crate) fn tail_facts(
    session_id: &str,
    cwd: &str,
    agent: &str,
    max_records: usize,
    listing: &[Hit],
) -> Option<TailFacts> {
    let entries = tail_entries(session_id, cwd, agent, listing)?;
    facts_from_entries(Some(&entries), max_records)
}

fn pr_read_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"\b(?:fno\s+do\s+pr|gh\s+pr)\s+(?:status|info|view|checks|wait)\s+#?(\d+)",
        )
        .expect("pr read regex")
    })
}

fn pr_json_state_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#""state"\s*:\s*"(MERGED|CLOSED)""#).expect("pr state regex")
    })
}

fn pr_id_in_blob_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r#"(?i)(?:pr|pulls|number)[/":# =]+(\d+)"#).expect("pr id regex")
    })
}

/// PR facts from one raw record: a status-read command, or a tool result
/// asserting a terminal state. These are the two places the record text drops.
fn pr_poll_record(record: &Value) -> Vec<PrPoll> {
    let mut blobs: Vec<String> = Vec::new();
    let content = record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    for part in content.into_iter().flatten() {
        match part.get("type").and_then(Value::as_str) {
            Some("tool_use") => {
                if let Some(input) = part.get("input").filter(|i| i.is_object()) {
                    let command = input.get("command").and_then(Value::as_str).unwrap_or("");
                    blobs.push(command.to_string());
                }
            }
            Some("tool_result") => match part.get("content") {
                Some(Value::String(inner)) => blobs.push(inner.clone()),
                Some(Value::Array(inner)) => {
                    let joined: Vec<&str> = inner
                        .iter()
                        .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                        .map(|p| p.get("text").and_then(Value::as_str).unwrap_or(""))
                        .collect();
                    blobs.push(joined.join(" "));
                }
                _ => {}
            },
            _ => {}
        }
    }
    let mut facts = Vec::new();
    for blob in &blobs {
        for caps in pr_read_re().captures_iter(blob) {
            if let Ok(n) = caps[1].parse::<i64>() {
                facts.push(PrPoll::Read(n));
            }
        }
        if let Some(state) = pr_json_state_re().captures(blob) {
            let mut numbers: Vec<i64> = pr_id_in_blob_re()
                .captures_iter(blob)
                .filter_map(|c| c[1].parse::<i64>().ok())
                .collect();
            numbers.sort_unstable();
            numbers.dedup();
            for n in numbers {
                facts.push(PrPoll::Settled(n, state[1].to_string()));
            }
        }
    }
    facts
}

/// `(pr, state, reads-after-settle)` when the tail keeps reading a PR it itself
/// shows settled. The settle marker must be in the tail, and the live read must
/// confirm terminal now; unreadable is unknown, which never produces a verdict.
pub(crate) fn polling_basis(
    pr_polls: &[PrPoll],
    pr_state_for: &dyn Fn(&str, i64) -> Option<String>,
    cwd: &str,
) -> Option<(i64, String, i64)> {
    let mut settled: Vec<i64> = Vec::new();
    let mut after: Vec<(i64, i64)> = Vec::new();
    for poll in pr_polls {
        match poll {
            PrPoll::Settled(n, _) => {
                if !settled.contains(n) {
                    settled.push(*n);
                }
            }
            PrPoll::Read(n) if settled.contains(n) => {
                match after.iter_mut().find(|(seen, _)| seen == n) {
                    Some((_, count)) => *count += 1,
                    None => after.push((*n, 1)),
                }
            }
            PrPoll::Read(_) => {}
        }
    }
    for (n, count) in after {
        if count < 2 {
            continue;
        }
        if let Some(current) = pr_state_for(cwd, n) {
            let upper = current.to_uppercase();
            if upper == "MERGED" || upper == "CLOSED" {
                return Some((n, upper, count));
            }
        }
    }
    None
}
