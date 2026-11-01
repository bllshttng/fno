//! The correction fold and its daemon arm (task 1.2). Off-loop, one
//! in flight, reading every harness's transcripts through the
//! provenance sources, appending agent_corrected rows to the global
//! event store through the store's idempotent append. Cursors in
//! <state>/cache/corrections/ carry each file's byte offset plus a head
//! sample, so a tick reads only appended bytes and a replaced file never
//! double counts (the transcript_activity pattern). Cadence: a 60 s
//! floor, a 64 MiB read budget, a 200-row append cap, newest files
//! first. A held cursor lock skips the tick (AC4); the remainder lands
//! on a later tick. A turn older than first-run lands marked backfilled
//! and its detect-time facts read unmeasured (AC5). Sources the render
//! strips (opencode, pi, agy) scan without assistant context, so no
//! interrupt rows come from a render and the question filter runs with
//! assistant_ended_question false.

use crate::correction_detect::{detect_user, scrub_quote, ScrubCounts, RULE_VERSION};
use crate::correction_readings::{readings_for, HarnessReadings};
use crate::provenance::{classify_turn, is_user_turn, turn_text, turn_ts_epoch, BusIndex, HarnessKind, Provenance, RelayKind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) const TICK_READ_BUDGET: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_ROWS_PER_TICK: usize = 200;
pub(crate) const TICK_INTERVAL_S: u64 = 60;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Cursor {
    offset: u64,
    head: String,
    head_len: u64,
}

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct CacheFile {
    version: u32,
    first_run_epoch: Option<u64>,
    cursors: BTreeMap<String, Cursor>,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct FoldReceipt {
    pub(crate) skipped: bool,
    journal_error: Option<String>,
    files_read: u64,
    bytes_read: u64,
    rows_appended: u64,
    rows_capped: u64,
    pending_files: u64,
    pending_bytes: u64,
    replaced: u64,
}
/// The per-session fold state: the assistant turn since the last
/// user-shaped boundary plus the session facts the 2026-10-31 lead
/// ruling adds: skills, references, subagents.
struct FoldState {
    turn_text: String,
    turn_ts: Option<f64>,
    turn_uuid: String,
    assistant_ended_question: bool,
    reacted: bool,
    skills: Vec<String>,
    references: Vec<String>,
    subagents: Vec<String>,
    headless: bool,
    first_user_seen: bool,
    counts: ScrubCounts,
}

impl FoldState {
    fn new() -> Self {
        FoldState {
            turn_text: String::new(),
            turn_ts: None,
            turn_uuid: String::new(),
            assistant_ended_question: false,
            reacted: false,
            skills: Vec::new(),
            references: Vec::new(),
            subagents: Vec::new(),
            headless: false,
            first_user_seen: false,
            counts: ScrubCounts::default(),
        }
    }
}
/// The event id: `corr:` plus blake3 of session/turn/kind, the corpus
/// row-id key. A replay is ignored by the store's idempotent append.
fn correction_id(session: &str, turn: &str, kind: &str) -> String {
    let key = format!("{session}\0{turn}\0{kind}\0user");
    let digest = blake3::hash(key.as_bytes()).to_hex()[..16].to_string();
    format!("corr:{digest}")
}

/// One registry row's detect-time facts.
#[derive(Clone)]
struct RegistryFacts {
    account: Option<String>,
    node: Option<String>,
}

fn registry_facts() -> BTreeMap<String, RegistryFacts> {
    let home = crate::paths::AgentsHome::from_env();
    let path = home.registry_json();
    let Ok(value) = crate::registry_store::read(&path) else {
        return BTreeMap::new();
    };
    let rows = value
        .get("agents")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = BTreeMap::new();
    for row in rows {
        let Some(sid) = row.get("harness_session_id").and_then(Value::as_str) else {
            continue;
        };
        let account = ["/account_record_id", "/launch_account", "/provider"]
            .iter()
            .find_map(|p| {
                row.pointer(p)
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            });
        let node = row
            .get("node")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        out.insert(sid.to_string(), RegistryFacts { account, node });
    }
    out
}
/// The detect-time head reading: git -C <cwd> rev-parse HEAD, stamped at
/// detect time. Backfilled or unreadable reads unmeasured.
fn head_reading(cwd: &Path, backfilled: bool) -> Value {
    if backfilled || !cwd.is_dir() {
        return json!({"value": null, "basis": "unmeasured", "reason": "backfilled or no cwd"});
    }
    match std::process::Command::new("git")
        .arg("-C").arg(cwd).args(["rev-parse", "HEAD"]).output()
    {
        Ok(out) if out.status.success() => {
            let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
            json!({"value": sha, "basis": "measured", "measured_at": now_rfc3339()})
        }
        _ => json!({"value": null, "basis": "unmeasured", "reason": "git unreadable"}),
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// An epoch seconds float as RFC3339.
fn rfc3339_from_epoch(ts: f64) -> String {
    chrono::DateTime::from_timestamp(ts as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(now_rfc3339)
}
/// One emitted correction row: the envelope body plus the event id.
struct CorrectionRow {
    event_id: String,
    ts: String,
    data: Value,
}

#[allow(clippy::too_many_arguments)]
fn build_row(
    session: &str,
    turn: &str,
    ts_turn: Option<f64>,
    corrected_by: &str,
    kind: &str,
    quote_raw: &str,
    typed: Option<&str>,
    after_question: bool,
    st: &FoldState,
    readings: &HarnessReadings,
    reg: Option<&RegistryFacts>,
    cwd: Option<&str>,
    backfilled: bool,
    source_file: &str,
) -> CorrectionRow {
    let mut counts = st.counts.clone();
    let (quote, quote_status) = scrub_quote(quote_raw, &mut counts);
    let id = correction_id(session, turn, kind);
    let mut data = json!({
        "correction_id": id,
        "corrected_by": corrected_by,
        "kind": kind,
        "session_id": session,
        "rule_version": RULE_VERSION,
        "quote": quote,
        "quote_status": quote_status,
        "after_question": after_question,
        "backfilled": backfilled,
    });
    if let Some(ts) = ts_turn {
        data["ts_turn"] = json!(rfc3339_from_epoch(ts));
    }
    if let Some(t) = typed {
        data["typed"] = json!(t);
    }
    data["model"] = readings.model.to_json();
    data["effort"] = readings.effort.to_json();
    data["provider"] = readings.provider.to_json();
    data["harness_version"] = readings.harness_version.to_json();
    data["harness"] = json!(readings.harness);
    if let Some(account) = reg.and_then(|r| r.account.as_deref()) {
        data["account"] = json!(account);
    }
    if let Some(node) = reg.and_then(|r| r.node.as_deref()) {
        data["node"] = json!(node);
    }
    data["head_sha"] = head_reading(cwd.map(Path::new).unwrap_or(Path::new("")), backfilled);
    if !st.skills.is_empty() {
        data["skills_loaded"] = json!(st.skills);
    }
    if !st.references.is_empty() {
        data["skills_referenced"] = json!(st.references);
    }
    if !st.subagents.is_empty() {
        data["subagents"] = json!(st.subagents);
    }
    data["source_file"] = json!(source_file);
    CorrectionRow {
        event_id: id,
        ts: rfc3339_from_epoch(ts_turn.unwrap_or(now_epoch_secs() as f64)),
        data,
    }
}
/// The shared per-line scan: user turns close the assistant turn and
/// may emit a row; assistant text accumulates; tool calls feed the
/// ruling's session facts. opencode, pi and agy run the claude-shaped
/// render: no assistant text, so no interrupt rows and the question
/// filter runs with assistant_ended_question false.
fn scan_line(
    st: &mut FoldState,
    obj: &Value,
    harness: &str,
    session: &str,
    bus: &BusIndex,
    rows: &mut Vec<CorrectionRow>,
    first_run: u64,
    readings: &HarnessReadings,
    reg_map: &BTreeMap<String, RegistryFacts>,
    cwd: Option<&str>,
    backfilled: bool,
    source_file: &str,
) {
    let ts = turn_ts_epoch(obj);
    let uuid = obj
        .get("uuid")
        .and_then(Value::as_str)
        .or_else(|| obj.pointer("/id").and_then(Value::as_str))
        .map(str::to_string)
        .unwrap_or_else(|| "L".to_string());
    let is_rendered = matches!(harness, "opencode" | "pi" | "agy");
    let text = assistant_text_of(obj, harness);
    if !is_rendered && !text.trim().is_empty() {
        if !st.turn_text.is_empty() {
            st.turn_text.push('\n');
        }
        st.turn_text.push_str(&text);
        st.turn_ts = ts.or(st.turn_ts);
        st.turn_uuid = uuid.clone();
        st.assistant_ended_question = text.trim_end().ends_with('?'));
    }
    collect_facts(st, obj, harness);
    if !is_user_turn(obj) {
        return;
    }
    let p = classify_turn(obj, bus, session);
    let typed_shape = matches!(p, Provenance::Unknown | Provenance::Operator);
    let first = typed_shape && !st.first_user_seen;
    st.first_user_seen |= typed_shape;
    let seed = first && st.headless;
    let typed = p == Provenance::Operator || (p == Provenance::Unknown && !seed);
    let interrupt = p == Provenance::Harness(HarnessKind::InterruptMarker);
    let boundary = typed || interrupt || matches!(p,
        Provenance::Relay(_) | Provenance::Keepalive | Provenance::Harness(_));
    if !boundary {
        return;
    }
    let has_prior = !st.turn_text.is_empty();
    if interrupt && !st.reacted && has_prior {
        st.reacted = true;
        rows.push(build_row(session, &uuid, ts, "user", "interrupt",
            "[Request interrupted by user]", None, false, st,
            readings, reg_map.get(session),
            cwd, backfilled, source_file));
    }
    if typed && !st.reacted && has_prior {
        if let Some(c) = detect_user(&text, st.assistant_ended_question) {
            st.reacted = true;
            rows.push(build_row(session, &uuid, ts, "user", c.kind,
                &text, Some(typed_word(p)), c.after_question, st,
                readings, reg_map.get(session),
                cwd, backfilled, source_file));
        }
    }
    st.turn_text.clear();
    st.turn_ts = None;
    st.assistant_ended_question = false;
    st.reacted = false;
}


/// Assistant prose of one row, per harness shape; empty otherwise.
fn assistant_text_of(obj: &Value, harness: &str) -> String {
    match harness {
        "claude" => {
            if obj.get("type").and_then(Value::as_str) != Some("assistant") {
                return String::new();
            }
            text_blocks(obj.pointer("/message/content").and_then(Value::as_array))
        }
        "codex" => {
            let p = obj.get("payload");
            let ok = p.and_then(|p| p.get("type")).and_then(Value::as_str) == Some("message")
                && p.and_then(|p| p.get("role")).and_then(Value::as_str) == Some("assistant");
            if !ok {
                return String::new();
            }
            crate::lead_hygiene::message_text(p.and_then(|p| p.get("content")).unwrap_or(&Value::Null))
        }
        "footnote" => {
            let ok = obj.get("type").and_then(Value::as_str) == Some("model_response");
            if !ok {
                return String::new();
            }
            text_blocks(obj.pointer("/data/content").and_then(Value::as_array))
        }
        _ => String::new(),
    }
}

/// Text blocks of a claude-shaped content array, joined with spaces.
fn text_blocks(content: Option<&Vec<Value>>) -> String {
    content
        .map(|blocks| {
            blocks.iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}
/// The typed witness word for the row: witnessed is the Operator verdict,
/// unwitnessed is the residual that never claims a person typed it.
fn typed_word(p: Provenance) -> Option<&'static str> {
    if p == Provenance::Operator {
        return Some("witnessed");
    }
    Some("unwitnessed")
}