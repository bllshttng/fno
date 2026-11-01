//! The correction fold and its daemon arm. Off-loop, one in flight, reading
//! every harness's transcripts through the provenance sources and appending
//! `agent_corrected` rows to the global event store through its idempotent
//! append. Cursors in `<state>/cache/corrections/` carry each file's byte
//! offset, a head sample and the session state a tail read cannot rebuild, so
//! a tick reads only appended bytes and a replaced file never double counts.
//!
//! Cadence: a 60 s floor, a 64 MiB read budget, a 200-row append cap, newest
//! files first. A held cursor lock skips the tick; the remainder lands on a
//! later tick. A turn older than first run lands marked backfilled, with its
//! detect-time facts unmeasured.
//!
//! opencode and agy have no appendable file, so they render whole and keep an
//! offset into the render, re-read only when the store's stamp moves. Their
//! renders drop assistant text, so any earlier user row opens the turn and
//! the question filter reads false. pi keeps a file: its raw rows feed the
//! readings and the assistant side, then each row renders for the classifier.

use crate::correction_detect::{detect_agent, detect_user, scrub_quote, ScrubCounts, RULE_VERSION};
use crate::correction_readings::{HarnessReadings, Reading};
use crate::gh_budget::{lock_path, FileLock};
use crate::operator_witness::SubmitIndex;
use crate::paths::AgentsHome;
use crate::provenance::{
    classify_turn, is_user_turn, turn_text, turn_ts_epoch, BusIndex, ClaudeSource, CodexSource,
    HarnessKind, Provenance, RelayKind, SessionFile, TranscriptSource,
};
use crate::transcript_activity::{head_sample, HEAD_SAMPLE_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const TICK_READ_BUDGET: u64 = 64 * 1024 * 1024;
pub(crate) const MAX_ROWS_PER_TICK: usize = 200;
pub(crate) const TICK_INTERVAL_S: u64 = 60;
const OUTCOME_INTERVAL_S: u64 = 3_600;
/// A session's skill, reference and subagent lists stop growing here, so the
/// one cache file stays small and a row stays under the event size limit.
const FACT_LIST_CAP: usize = 40;
const QUIET_STATE_DROP_S: u64 = 2 * 86_400;

/// The session state a tail read cannot rebuild: the open assistant turn's
/// shape, the latest readings, and the ruling's session facts.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionState {
    line: u64,
    turn_open: bool,
    ended_question: bool,
    reacted: bool,
    headless: bool,
    first_user_seen: bool,
    readings: HarnessReadings,
    cwd: Option<String>,
    branch: Option<String>,
    active_skill: Option<String>,
    skills: Vec<String>,
    references: Vec<String>,
    subagents: Vec<String>,
}

impl SessionState {
    fn new(harness: &str) -> Self {
        SessionState {
            line: 0,
            turn_open: false,
            ended_question: false,
            reacted: false,
            headless: false,
            first_user_seen: false,
            readings: HarnessReadings::empty(harness),
            cwd: None,
            branch: None,
            active_skill: None,
            skills: Vec::new(),
            references: Vec::new(),
            subagents: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Cursor {
    offset: u64,
    /// Rendered sources: the store stamp the offset was read at.
    #[serde(default)]
    mtime: u64,
    #[serde(default)]
    head: String,
    #[serde(default)]
    head_len: u64,
    /// Dropped once a session goes quiet, so the one cache file stays small.
    /// A session that resumes rebuilds its readings from its next rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    state: Option<SessionState>,
}

/// Typed turns per day, harness and model: the rate's denominator.
type Tally = BTreeMap<String, u64>;

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    first_run_epoch: u64,
    cursors: BTreeMap<String, Cursor>,
    typed_turns: Tally,
}

#[derive(Debug, Default, Clone, Serialize)]
pub(crate) struct FoldReceipt {
    pub(crate) skipped: bool,
    pub(crate) append_error: Option<String>,
    pub(crate) cache_reset: Option<String>,
    pub(crate) files_read: u64,
    pub(crate) bytes_read: u64,
    pub(crate) rows_appended: u64,
    pub(crate) rows_capped: bool,
    pub(crate) pending_files: u64,
    pub(crate) replaced: u64,
}

/// Everything one fold pass reads and writes, injected so a test folds a
/// fixture on a temp store.
pub(crate) struct FoldInputs {
    pub(crate) cache: PathBuf,
    pub(crate) journal: PathBuf,
    pub(crate) sources: Vec<Box<dyn TranscriptSource>>,
    pub(crate) bus: BusIndex,
    pub(crate) witness: SubmitIndex,
    pub(crate) detect: DetectFacts,
    pub(crate) now_epoch: u64,
    pub(crate) budget: u64,
    pub(crate) row_cap: usize,
}

// ------------------------------------------------------------ detect-time facts

/// One registry row's facts at detect time.
#[derive(Debug, Clone, Default)]
pub(crate) struct RegistryFacts {
    pub(crate) account: Option<String>,
    pub(crate) route_provider: Option<String>,
    pub(crate) node: Option<String>,
}

/// The facts a row reads from outside its transcript, loaded once per pass.
#[derive(Debug, Default)]
pub(crate) struct DetectFacts {
    pub(crate) registry: BTreeMap<String, RegistryFacts>,
    /// node id -> PR number, from the graph.
    pub(crate) prs: BTreeMap<String, i64>,
    pub(crate) fno_version: String,
}

fn nonempty(row: &Value, pointer: &str) -> Option<String> {
    row.pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn load_detect_facts(home: &AgentsHome) -> DetectFacts {
    let mut facts = DetectFacts {
        fno_version: env!("CARGO_PKG_VERSION").to_string(),
        ..DetectFacts::default()
    };
    if let Ok(value) = crate::registry_store::read(&home.registry_json()) {
        let rows = value.get("agents").and_then(Value::as_array);
        for row in rows.into_iter().flatten() {
            let Some(sid) = nonempty(row, "/harness_session_id") else {
                continue;
            };
            facts.registry.insert(
                sid,
                RegistryFacts {
                    account: nonempty(row, "/account_record_id")
                        .or_else(|| nonempty(row, "/launch_account")),
                    route_provider: nonempty(row, "/route_provider_id"),
                    node: nonempty(row, "/node"),
                },
            );
        }
    }
    if let Ok(rows) = crate::graph_store::read_rows(&crate::backlog::settings::graph_path()) {
        for row in rows {
            let id = row.get("id").and_then(Value::as_str);
            let pr = row.get("pr_number").and_then(Value::as_i64);
            if let (Some(id), Some(pr)) = (id, pr) {
                facts.prs.insert(id.to_string(), pr);
            }
        }
    }
    facts
}

/// The node a branch names: `feature/x-1225-some-title` reads `x-1225`.
fn node_from_branch(branch: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?:^|/)([a-z]+-[0-9a-f]{4})(?:-|$)").ok()?;
    re.captures(branch).map(|c| c[1].to_string())
}

/// The cwd's head at detect time; unmeasured when the cwd is gone.
fn head_reading(cwd: Option<&str>, now: &str) -> Reading {
    let Some(cwd) = cwd.filter(|c| Path::new(c).is_dir()) else {
        return Reading::unmeasured("the session cwd no longer exists");
    };
    match std::process::Command::new("git")
        .args(["-C", cwd, "rev-parse", "HEAD"])
        .output()
    {
        Ok(out) if out.status.success() => Reading::with_basis(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
            Some(now.to_string()),
            "detect",
        ),
        _ => Reading::unmeasured("git could not read the session cwd"),
    }
}

// ------------------------------------------------------------------ row build

fn rfc3339(epoch: f64) -> Option<String> {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

/// A row before the fold stamps its session facts on it.
struct Detected<'a> {
    id_turn: String,
    id_kind: &'a str,
    corrected_by: &'static str,
    kind: &'static str,
    quote: &'a str,
    extra: Value,
}

struct PassCtx<'a> {
    harness: &'a str,
    session: &'a str,
    source_file: String,
    first_run: u64,
    now_epoch: u64,
    detect: &'a DetectFacts,
    /// A store-backed source's readings, read once per session.
    fixed_readings: Option<&'a HarnessReadings>,
}

/// The session's readings at this turn, with the provider derived when no row
/// named one.
fn readings_now(st: &SessionState, ctx: &PassCtx) -> HarnessReadings {
    let mut readings = ctx
        .fixed_readings
        .cloned()
        .unwrap_or_else(|| st.readings.clone());
    readings.derive_provider_from_model();
    readings
}

/// The whole event envelope for one detection. The quote is scrubbed here, so
/// no path past this point can write raw words.
fn build_row(
    st: &SessionState,
    ctx: &PassCtx,
    ts: Option<f64>,
    det: Detected,
    turn: &str,
) -> (String, Value) {
    let mut counts = ScrubCounts::default();
    let (quote, quote_status) = scrub_quote(det.quote, &mut counts);
    let id = correction_id(ctx.session, &det.id_turn, det.id_kind);
    let backfilled = ts.is_some_and(|t| (t as u64) < ctx.first_run);
    let mut readings = readings_now(st, ctx);
    let now = rfc3339(ctx.now_epoch as f64).unwrap_or_default();
    let unmeasured_backfill = || Reading::unmeasured("backfilled: read after the fact");
    let reg = ctx.detect.registry.get(ctx.session);
    // A routed session names its real provider on the registry row, and that
    // outranks anything a label can derive.
    if !backfilled {
        if let Some(route) = reg.and_then(|r| r.route_provider.clone()) {
            readings.provider = Reading::with_basis(route, Some(now.clone()), "registry");
        }
    }
    let no_row = "no registry row: the session was not started by a spawn";
    let (account, node_reading, head, fno_version, pr) = if backfilled {
        (
            unmeasured_backfill(),
            st.branch
                .as_deref()
                .and_then(node_from_branch)
                .map(|n| Reading::with_basis(n, None, "branch"))
                .unwrap_or_else(unmeasured_backfill),
            unmeasured_backfill(),
            unmeasured_backfill(),
            unmeasured_backfill(),
        )
    } else {
        let account = reg
            .and_then(|r| r.account.clone())
            .map(|a| Reading::with_basis(a, Some(now.clone()), "registry"))
            .unwrap_or_else(|| Reading::unmeasured(no_row));
        let node = reg
            .and_then(|r| r.node.clone())
            .map(|n| Reading::with_basis(n, Some(now.clone()), "registry"))
            .or_else(|| {
                st.branch
                    .as_deref()
                    .and_then(node_from_branch)
                    .map(|n| Reading::with_basis(n, Some(now.clone()), "branch"))
            })
            .unwrap_or_else(|| Reading::unmeasured("no registry node and no node in the branch"));
        let pr = node
            .value
            .as_deref()
            .and_then(|n| ctx.detect.prs.get(n))
            .map(|n| Reading::with_basis(n.to_string(), Some(now.clone()), "graph"))
            .unwrap_or_else(|| Reading::unmeasured("the graph names no PR for this node"));
        (
            account,
            node,
            head_reading(st.cwd.as_deref(), &now),
            Reading::with_basis(ctx.detect.fno_version.clone(), Some(now.clone()), "daemon"),
            pr,
        )
    };
    let mut data = json!({
        "correction_id": id,
        "corrected_by": det.corrected_by,
        "kind": det.kind,
        "session_id": ctx.session,
        "rule_version": RULE_VERSION,
        "turn": turn,
        "harness": ctx.harness,
        "model": readings.model.to_json(),
        "effort": readings.effort.to_json(),
        "provider": readings.provider.to_json(),
        "harness_version": readings.harness_version.to_json(),
        "account": account.to_json(),
        "node": node_reading.to_json(),
        "pr": pr.to_json(),
        "head_sha": head.to_json(),
        "fno_version": fno_version.to_json(),
        "active_skill": st.active_skill,
        "skills_loaded": st.skills,
        "skills_referenced": st.references,
        "subagents": st.subagents,
        "quote": quote,
        "quote_status": quote_status,
        "backfilled": backfilled,
        "source_file": crate::correction_detect::scrub_string(&ctx.source_file, &mut counts),
    });
    if let Some(stamp) = ts.and_then(rfc3339) {
        data["ts_turn"] = json!(stamp);
    }
    if let (Some(map), Some(extra)) = (data.as_object_mut(), det.extra.as_object()) {
        for (k, v) in extra {
            map.insert(k.clone(), v.clone());
        }
    }
    let stamp = ts
        .and_then(rfc3339)
        .unwrap_or_else(|| rfc3339(ctx.now_epoch as f64).unwrap_or_default());
    let envelope = json!({"ts": stamp, "type": "agent_corrected", "source": "daemon", "session_id": ctx.session, "data": data});
    (id, envelope)
}

/// The event id: `corr:` plus the corpus row id for the same turn, so a reader
/// can join an event to its corpus row and a replay is ignored by the store.
fn correction_id(session: &str, turn: &str, kind: &str) -> String {
    format!(
        "corr:{}",
        crate::intel_corpus::row_id(session, turn, kind, "0")
    )
}

// ---------------------------------------------------------------- session facts

fn push_capped(list: &mut Vec<String>, item: String) {
    if list.len() < FACT_LIST_CAP && !list.contains(&item) {
        list.push(item);
    }
}

/// A skill reference opened past SKILL.md: `.../skills/<name>/references/<f>`.
fn skill_reference(path: &str) -> Option<String> {
    let (_, tail) = path.rsplit_once("/skills/")?;
    let (skill, rest) = tail.split_once('/')?;
    rest.starts_with("references/")
        .then(|| format!("{skill}/{rest}"))
}

/// The skill a `<command-name>` invocation names.
fn command_skill(text: &str) -> Option<String> {
    let (_, tail) = text.split_once("<command-name>")?;
    let (name, _) = tail.split_once("</command-name>")?;
    Some(name.trim().trim_start_matches('/').to_string())
}

/// The ruling's session facts from one tool call, per harness shape. Rendered
/// stores drop tool input, so opencode, pi and agy read none of this.
fn collect_tool_call(st: &mut SessionState, tool: &str, input: &Value) {
    let field = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    match tool {
        "Skill" => {
            let skill = field("skill").to_string();
            if !skill.is_empty() {
                st.active_skill = Some(skill.clone());
                push_capped(&mut st.skills, skill);
            }
        }
        "Read" | "Grep" | "Glob" => {
            let path = if tool == "Read" {
                field("file_path")
            } else {
                field("path")
            };
            if let Some(reference) = skill_reference(path) {
                push_capped(&mut st.references, reference);
            }
        }
        "Task" | "Agent" => {
            let name = input
                .get("name")
                .or_else(|| input.get("subagent_type"))
                .or_else(|| input.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("subagent");
            push_capped(&mut st.subagents, name.to_string());
        }
        _ => {}
    }
}

/// `fno agents spawn` inside a codex shell call names a child session.
fn collect_codex_call(st: &mut SessionState, call: &str) {
    if let Some((_, tail)) = call.split_once("fno agents spawn") {
        let name = tail
            .split_once("--name")
            .and_then(|(_, n)| n.split_whitespace().next())
            .map(|n| n.trim_matches(|c| c == '"' || c == '\'' || c == '\\'))
            .unwrap_or("spawned");
        push_capped(&mut st.subagents, name.to_string());
    }
    if let Some(reference) = call
        .split_whitespace()
        .find_map(|w| skill_reference(w.trim_matches(|c| c == '"' || c == '\'' || c == '\\')))
    {
        push_capped(&mut st.references, reference);
    }
}

fn text_blocks(content: Option<&Vec<Value>>) -> String {
    content
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// Assistant prose of one raw row, per harness shape; empty otherwise.
fn assistant_text(obj: &Value, harness: &str) -> String {
    match harness {
        "claude" if obj.get("type").and_then(Value::as_str) == Some("assistant") => {
            text_blocks(obj.pointer("/message/content").and_then(Value::as_array))
        }
        "codex" => {
            let p = obj.get("payload");
            let field = |k: &str| p.and_then(|p| p.get(k)).and_then(Value::as_str);
            if field("type") == Some("message") && field("role") == Some("assistant") {
                crate::lead_hygiene::message_text(
                    p.and_then(|p| p.get("content")).unwrap_or(&Value::Null),
                )
            } else {
                String::new()
            }
        }
        "footnote" if obj.get("type").and_then(Value::as_str) == Some("model_response") => {
            text_blocks(obj.pointer("/data/content").and_then(Value::as_array))
        }
        "pi" if obj.pointer("/message/role").and_then(Value::as_str) == Some("assistant") => {
            text_blocks(obj.pointer("/message/content").and_then(Value::as_array))
        }
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------- the scan

/// One scan's outputs: the rows to append and the typed turns to tally.
#[derive(Default)]
struct ScanOut {
    rows: Vec<(String, Value)>,
    typed: Tally,
}

/// The tally key: UTC day, harness, model. An unmeasured model keeps its own
/// bucket and is never folded into a named one.
fn tally_key(ts: Option<f64>, harness: &str, model: &Reading) -> Option<String> {
    let day = chrono::DateTime::from_timestamp(ts? as i64, 0)?.format("%Y-%m-%d");
    let model = model.value.as_deref().unwrap_or("unmeasured");
    Some(format!("{day}|{harness}|{model}"))
}

/// A tool result's refusal class: a user refusal is a reject, a gate refusal
/// is a machine row. The guard's first line is the row's reason.
fn scan_tool_results(
    st: &mut SessionState,
    obj: &Value,
    ctx: &PassCtx,
    ts: Option<f64>,
    out: &mut ScanOut,
) -> bool {
    let Some(blocks) = obj.pointer("/message/content").and_then(Value::as_array) else {
        return false;
    };
    let mut any = false;
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        any = true;
        if !crate::provenance::json_truthy(block.get("is_error")) {
            continue;
        }
        let id = block
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let output = match block.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(c) => crate::lead_hygiene::message_text(c),
            None => String::new(),
        };
        match crate::session_activity::error_class(&output) {
            "user_rejected" if st.turn_open && !st.reacted => {
                st.reacted = true;
                out.rows.push(build_row(
                    st,
                    ctx,
                    ts,
                    Detected {
                        id_turn: format!("{id}:rejected"),
                        id_kind: "reaction",
                        corrected_by: "user",
                        kind: "reject",
                        quote: output.lines().next().unwrap_or(""),
                        extra: json!({"after_question": st.ended_question, "typed": "witnessed"}),
                    },
                    id,
                ));
            }
            class @ ("hook_blocked" | "permission_denied") => {
                let first = output.lines().next().unwrap_or("");
                let mut counts = ScrubCounts::default();
                let (reason, _) = scrub_quote(first, &mut counts);
                out.rows.push(build_row(
                    st,
                    ctx,
                    ts,
                    Detected {
                        id_turn: id.to_string(),
                        id_kind: "machine_refused",
                        corrected_by: "machine",
                        kind: "refused",
                        quote: first,
                        extra: json!({"reason": reason, "refusal_class": class}),
                    },
                    id,
                ));
            }
            _ => {}
        }
    }
    any
}

/// The corrector's readings on an agent row. A served reading from the
/// `<fno_mail>` tag when it names them, else from the bus sender's registry
/// row; otherwise unmeasured. Coverage on current mail is partial by measure.
fn corrector_facts(text: &str, sender: Option<&str>, ts: &str) -> Value {
    let attr = |name: &str| -> Option<String> {
        let (_, tag) = text.split_once("<fno_mail")?;
        let (tag, _) = tag.split_once('>')?;
        let (_, tail) = tag.split_once(&format!("{name}=\""))?;
        tail.split_once('"').map(|(v, _)| v.to_string())
    };
    let reading = |value: Option<String>, what: &str| match value {
        Some(v) => Reading::with_basis(v, Some(ts.to_string()), "mail_tag"),
        None => Reading::unmeasured(&format!("the mail tag names no {what}")),
    };
    json!({
        "corrected_by_session": sender,
        "corrected_by_model": reading(attr("model"), "model").to_json(),
        "corrected_by_harness": reading(attr("harness"), "harness").to_json(),
        "verified": false,
    })
}

/// One user-shaped row: classify, then close the assistant turn on a boundary.
#[allow(clippy::too_many_arguments)]
fn scan_user_turn(
    st: &mut SessionState,
    obj: &Value,
    uuid: &str,
    ts: Option<f64>,
    bus: &BusIndex,
    witness: &mut SubmitIndex,
    ctx: &PassCtx,
    out: &mut ScanOut,
) {
    let text = turn_text(obj);
    if text.trim().is_empty() {
        return;
    }
    let mut p = classify_turn(obj, bus, ctx.session);
    if p == Provenance::Unknown {
        let bound = ts
            .and_then(|t| witness.bind(ctx.session, (t * 1000.0) as i64))
            .is_some();
        if bound {
            p = Provenance::Operator;
        }
    }
    if p == Provenance::Harness(HarnessKind::CommandInvocation) {
        if let Some(skill) = command_skill(&text) {
            st.active_skill = Some(skill.clone());
            push_capped(&mut st.skills, skill);
        }
    }
    let typed_shape = matches!(p, Provenance::Unknown | Provenance::Operator);
    let first = typed_shape && !st.first_user_seen;
    st.first_user_seen |= typed_shape;
    let seed = first && st.headless && p == Provenance::Unknown;
    let typed = p == Provenance::Operator || (p == Provenance::Unknown && !seed);
    let interrupt = p == Provenance::Harness(HarnessKind::InterruptMarker);
    let relay = matches!(
        p,
        Provenance::Relay(
            RelayKind::FnoMail | RelayKind::BusRow | RelayKind::NativePeer | RelayKind::CrossSession
        )
    );
    let boundary = typed
        || interrupt
        || seed
        || matches!(
            p,
            Provenance::Relay(_)
                | Provenance::Keepalive
                | Provenance::Harness(
                    HarnessKind::CommandInvocation
                        | HarnessKind::CompactionPreamble
                        | HarnessKind::StopHook
                        | HarnessKind::LoopWakeup
                )
        );
    if !boundary {
        return;
    }
    if typed {
        if let Some(key) = tally_key(ts, ctx.harness, &readings_now(st, ctx).model) {
            *out.typed.entry(key).or_default() += 1;
        }
    }
    if st.turn_open && !st.reacted {
        let detected = if interrupt {
            Some(Detected {
                id_turn: uuid.to_string(),
                id_kind: "reaction",
                corrected_by: "user",
                kind: "interrupt",
                quote: "[Request interrupted by user]",
                extra: json!({"after_question": st.ended_question}),
            })
        } else if typed {
            detect_user(&text, st.ended_question).map(|c| Detected {
                id_turn: uuid.to_string(),
                id_kind: "reaction",
                corrected_by: "user",
                kind: c.kind,
                quote: &text,
                extra: json!({
                    "after_question": c.after_question,
                    "typed": if p == Provenance::Operator { "witnessed" } else { "unwitnessed" },
                }),
            })
        } else if relay {
            detect_agent(&text).map(|c| {
                let sender = bus
                    .rows()
                    .iter()
                    .find(|r| {
                        r.to_session.as_deref() == Some(ctx.session) && text.contains(r.body.trim())
                    })
                    .and_then(|r| r.from_session.clone());
                let stamp = ts.and_then(rfc3339).unwrap_or_default();
                Detected {
                    id_turn: uuid.to_string(),
                    id_kind: "agent_mail",
                    corrected_by: "agent",
                    kind: c,
                    quote: &text,
                    extra: corrector_facts(&text, sender.as_deref(), &stamp),
                }
            })
        } else {
            None
        };
        if let Some(det) = detected {
            st.reacted = true;
            let row = build_row(st, ctx, ts, det, uuid);
            out.rows.push(row);
        }
    }
    st.turn_open = false;
    st.ended_question = false;
    st.reacted = false;
    if matches!(ctx.harness, "opencode" | "agy" | "pi") {
        // These renders drop the assistant's words: the next user row can only
        // read as a reply to whatever came between.
        st.turn_open = true;
    }
}

/// One parsed row of a transcript, already claude-shaped for the classifier.
#[allow(clippy::too_many_arguments)]
fn scan_row(
    st: &mut SessionState,
    obj: &Value,
    bus: &BusIndex,
    witness: &mut SubmitIndex,
    ctx: &PassCtx,
    out: &mut ScanOut,
) {
    st.line += 1;
    if crate::provenance::json_truthy(obj.get("isSidechain")) {
        return;
    }
    st.readings.observe(obj);
    if st.cwd.is_none() {
        st.cwd = obj
            .get("cwd")
            .or_else(|| obj.pointer("/payload/cwd"))
            .or_else(|| obj.pointer("/data/cwd"))
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    if let Some(branch) = obj.get("gitBranch").and_then(Value::as_str) {
        st.branch = Some(branch.to_string());
    }
    st.headless |= obj
        .get("entrypoint")
        .and_then(Value::as_str)
        .is_some_and(|e| e.starts_with("sdk"));
    if obj.get("type").and_then(Value::as_str) == Some("session_meta") {
        st.headless = obj
            .pointer("/payload/originator")
            .and_then(Value::as_str)
            .is_some_and(|o| o.contains("exec"));
    }
    let ts = turn_ts_epoch(obj);
    // The corpus's turn key: a claude uuid, else the line number.
    let uuid = obj
        .get("uuid")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("L{}", st.line));
    let text = assistant_text(obj, ctx.harness);
    if !text.trim().is_empty() {
        st.turn_open = true;
        st.ended_question = text.trim_end().ends_with('?');
    }
    if obj.get("type").and_then(Value::as_str) == Some("assistant") {
        let blocks = obj.pointer("/message/content").and_then(Value::as_array);
        for b in blocks.into_iter().flatten() {
            if b.get("type").and_then(Value::as_str) == Some("tool_use") {
                let tool = b.get("name").and_then(Value::as_str).unwrap_or("");
                collect_tool_call(st, tool, b.get("input").unwrap_or(&Value::Null));
            }
        }
    }
    if ctx.harness == "codex" {
        if let Some(call) = crate::transcript_activity::codex_row_call_text(obj) {
            collect_codex_call(st, &call);
        }
        if obj.pointer("/payload/type").and_then(Value::as_str) == Some("turn_aborted") {
            let marker = json!({"type": "user", "message": {"role": "user",
                "content": "[Request interrupted by user]"}});
            scan_user_turn(st, &marker, &uuid, ts, bus, witness, ctx, out);
            return;
        }
    }
    if ctx.harness == "footnote" && obj.get("type").and_then(Value::as_str) == Some("child_session")
    {
        let name = obj
            .pointer("/data/session_id")
            .and_then(Value::as_str)
            .unwrap_or("child");
        push_capped(&mut st.subagents, name.to_string());
    }
    if !is_user_turn(obj) {
        return;
    }
    if scan_tool_results(st, obj, ctx, ts, out) {
        return;
    }
    scan_user_turn(st, obj, &uuid, ts, bus, witness, ctx, out);
}

/// One raw line: pi folds its raw row into the readings and the assistant
/// side, then each row it renders runs the shared scan.
fn scan_line(
    st: &mut SessionState,
    line: &str,
    bus: &BusIndex,
    witness: &mut SubmitIndex,
    ctx: &PassCtx,
    out: &mut ScanOut,
) {
    let Ok(obj) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if ctx.harness != "pi" {
        scan_row(st, &obj, bus, witness, ctx, out);
        return;
    }
    st.readings.observe(&obj);
    let text = assistant_text(&obj, "pi");
    if !text.trim().is_empty() {
        st.ended_question = text.trim_end().ends_with('?');
    }
    let was_question = st.ended_question;
    for rendered in crate::pi_transcript::render(line).lines() {
        if let Ok(row) = serde_json::from_str::<Value>(rendered) {
            st.ended_question = was_question;
            scan_row(st, &row, bus, witness, ctx, out);
        }
    }
}

// --------------------------------------------------------------------- the pass

fn append_rows(
    journal: &Path,
    rows: &[(String, Value)],
    receipt: &mut FoldReceipt,
) -> Result<(), String> {
    for (id, envelope) in rows {
        let line = envelope.to_string();
        let got = crate::event_store::append_envelope(journal, &line, Some(id))?;
        if got.inserted {
            receipt.rows_appended += 1;
        }
    }
    Ok(())
}

fn merge_tally(into: &mut Tally, from: Tally) {
    for (k, v) in from {
        *into.entry(k).or_default() += v;
    }
}

fn load_cache(path: &Path, now_epoch: u64, receipt: &mut FoldReceipt) -> CacheFile {
    let fresh = CacheFile {
        version: 1,
        first_run_epoch: now_epoch,
        ..CacheFile::default()
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return fresh,
        Err(e) => {
            receipt.cache_reset = Some(format!("unreadable: {e}"));
            return fresh;
        }
    };
    match serde_json::from_str::<CacheFile>(&text) {
        Ok(file) if file.version == 1 => file,
        Ok(file) => {
            receipt.cache_reset = Some(format!("version {} unsupported", file.version));
            fresh
        }
        Err(e) => {
            receipt.cache_reset = Some(format!("unparseable: {e}"));
            fresh
        }
    }
}

/// One append-only file from its cursor to the budget or the row cap. An
/// append failure leaves the prior cursor in place: the replay is idempotent.
#[allow(clippy::too_many_arguments)]
fn fold_file(
    file: &SessionFile,
    cur: &mut Cursor,
    state: &mut SessionState,
    budget: u64,
    row_cap: usize,
    bus: &BusIndex,
    witness: &mut SubmitIndex,
    ctx: &PassCtx,
    out: &mut ScanOut,
    bytes: &mut u64,
) {
    let Ok(mut f) = std::fs::File::open(&file.path) else {
        return;
    };
    if f.seek(SeekFrom::Start(cur.offset)).is_err() {
        return;
    }
    let mut reader = std::io::BufReader::new(&mut f);
    let mut buf = Vec::new();
    while *bytes < budget && out.rows.len() < row_cap {
        buf.clear();
        let Ok(n) = reader.read_until(b'\n', &mut buf) else {
            break;
        };
        // A trailing line with no newline waits for the next tick.
        if n == 0 || buf.last() != Some(&b'\n') {
            break;
        }
        let line = String::from_utf8_lossy(&buf);
        scan_line(state, line.trim_end(), bus, witness, ctx, out);
        cur.offset += n as u64;
        *bytes += n as u64;
    }
}

/// A rendered source: re-read only when the store stamp moved, resume at the
/// offset into the render.
#[allow(clippy::too_many_arguments)]
fn fold_rendered(
    source: &dyn TranscriptSource,
    file: &SessionFile,
    cur: &mut Cursor,
    state: &mut SessionState,
    budget: u64,
    row_cap: usize,
    bus: &BusIndex,
    witness: &mut SubmitIndex,
    ctx: &PassCtx,
    out: &mut ScanOut,
    bytes: &mut u64,
) {
    let text = source.read(file);
    let tail = text.get(cur.offset as usize..).unwrap_or("");
    let mut done = true;
    for line in tail.split_inclusive('\n') {
        if *bytes >= budget || out.rows.len() >= row_cap {
            done = false;
            break;
        }
        scan_line(state, line.trim_end(), bus, witness, ctx, out);
        cur.offset += line.len() as u64;
        *bytes += line.len() as u64;
    }
    if done {
        cur.mtime = file.mtime;
    }
}

/// One pass. Holds the cursor lock for the whole read-modify-write: a held
/// lock skips the tick at once and writes nothing.
pub(crate) fn fold_once(mut inputs: FoldInputs) -> FoldReceipt {
    let mut receipt = FoldReceipt::default();
    if let Some(parent) = inputs.cache.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(_guard) = FileLock::try_acquire(&lock_path(&inputs.cache)) else {
        receipt.skipped = true;
        return receipt;
    };
    let mut cache = load_cache(&inputs.cache, inputs.now_epoch, &mut receipt);
    let first_run = cache.first_run_epoch;
    let mut listed: Vec<(usize, SessionFile)> = Vec::new();
    for (i, source) in inputs.sources.iter().enumerate() {
        listed.extend(source.sessions(0).into_iter().map(|f| (i, f)));
    }
    listed.sort_by(|a, b| b.1.mtime.cmp(&a.1.mtime));
    let mut changed = receipt.cache_reset.is_some();
    let mut bytes = 0u64;
    for (i, file) in &listed {
        let source = inputs.sources[*i].as_ref();
        let harness = source.harness();
        let rendered = matches!(harness, "opencode" | "agy");
        let key = file.path.display().to_string();
        // Nothing to read in a file already at its end: no head sample, no
        // clone. With the corpus listed every tick, this is the common path.
        if let Some(c) = cache.cursors.get_mut(&key) {
            let idle = if rendered {
                c.mtime == file.mtime
            } else {
                c.offset == file.size
            };
            if idle {
                let quiet = inputs.now_epoch.saturating_sub(file.mtime) > QUIET_STATE_DROP_S;
                if quiet && c.state.take().is_some() {
                    changed = true;
                }
                continue;
            }
        }
        // A spent tick leaves the file for later: an unread file gets no
        // cursor, because a cursor is only worth its bytes once it has moved.
        if bytes >= inputs.budget || receipt.rows_capped {
            receipt.pending_files += 1;
            continue;
        }
        let mut cur = match cache.cursors.get(&key) {
            Some(c) if rendered => c.clone(),
            Some(c) => {
                let sampled = head_sample(&file.path, c.head_len);
                if file.size < c.offset || sampled.as_deref() != Some(c.head.as_str()) {
                    // A replaced file restarts from its current end, so old
                    // content is never counted twice.
                    receipt.replaced += 1;
                    let mut replaced = c.clone();
                    replaced.offset = file.size;
                    replaced.head = sampled.unwrap_or_default();
                    replaced
                } else {
                    c.clone()
                }
            }
            None => {
                let head_len = file.size.min(HEAD_SAMPLE_BYTES);
                let head = if rendered {
                    String::new()
                } else {
                    let Some(head) = head_sample(&file.path, head_len) else {
                        continue;
                    };
                    head
                };
                Cursor {
                    offset: 0,
                    mtime: 0,
                    head,
                    head_len,
                    state: None,
                }
            }
        };
        let fixed = (harness == "opencode")
            .then(|| {
                let display = file.path.to_string_lossy().into_owned();
                display.rsplit_once('#').map(|(db, sid)| {
                    crate::correction_readings::opencode_readings(Path::new(db), sid)
                })
            })
            .flatten();
        let ctx = PassCtx {
            harness,
            session: &file.session_id,
            source_file: key.clone(),
            first_run,
            now_epoch: inputs.now_epoch,
            detect: &inputs.detect,
            fixed_readings: fixed.as_ref(),
        };
        let mut state = cur
            .state
            .take()
            .unwrap_or_else(|| SessionState::new(harness));
        let mut out = ScanOut::default();
        let room = inputs
            .row_cap
            .saturating_sub(receipt.rows_appended as usize);
        if rendered {
            fold_rendered(
                source,
                file,
                &mut cur,
                &mut state,
                inputs.budget,
                room,
                &inputs.bus,
                &mut inputs.witness,
                &ctx,
                &mut out,
                &mut bytes,
            );
        } else {
            fold_file(
                file,
                &mut cur,
                &mut state,
                inputs.budget,
                room,
                &inputs.bus,
                &mut inputs.witness,
                &ctx,
                &mut out,
                &mut bytes,
            );
        }
        // The cursor advances only once its rows are stored: a failed append
        // replays this tail next tick, and the store ignores what already landed.
        if let Err(e) = append_rows(&inputs.journal, &out.rows, &mut receipt) {
            receipt.append_error = Some(e);
            break;
        }
        cur.state = Some(state);
        merge_tally(&mut cache.typed_turns, out.typed);
        receipt.files_read += 1;
        receipt.rows_capped = receipt.rows_appended as usize >= inputs.row_cap;
        let unfinished = if rendered {
            cur.mtime != file.mtime
        } else {
            cur.offset < file.size
        };
        if unfinished {
            receipt.pending_files += 1;
        }
        cache.cursors.insert(key, cur);
        changed = true;
    }
    receipt.bytes_read = bytes;
    if changed {
        let body = serde_json::to_string(&cache).unwrap_or_else(|_| "{}".to_string());
        let _ = crate::rundown::write_atomic(&inputs.cache, &body);
    }
    receipt
}

// ----------------------------------------------------------------------- the arm

/// The global store's journal path; `events.jsonl` routes to `db/events.db`.
fn global_journal(home: &AgentsHome) -> Option<PathBuf> {
    home.root().parent().map(|p| p.join("events.jsonl"))
}

/// Every harness, read through its own source. A store that is absent lists
/// nothing, so a machine without a harness costs one failed directory read.
fn all_sources() -> Vec<Box<dyn TranscriptSource>> {
    let (summaries_db, history, conversations_dir) = crate::agy_transcript::agy_stores();
    vec![
        Box::new(ClaudeSource {
            projects_dir: crate::claude_drive::claude_projects_dir(),
            roots: None,
        }),
        Box::new(CodexSource {
            sessions_dir: None,
            roots: None,
        }),
        Box::new(crate::pi_transcript::PiSource {
            sessions_dir: None,
            roots: None,
        }),
        Box::new(crate::opencode_transcript::OpencodeSource {
            dbs: crate::opencode_transcript::opencode_stores(),
            roots: None,
        }),
        Box::new(crate::agy_transcript::AgySource {
            summaries_db,
            history,
            conversations_dir,
            roots: None,
        }),
        Box::new(crate::footnote_harness::source::FootnoteSource {
            sessions_root: crate::footnote_harness::transcript::sessions_root(),
            roots: None,
        }),
    ]
}

/// One pass into a chosen directory, for a precision sample: real transcripts,
/// a separate store and cursor file, and the live store untouched.
pub(crate) fn sample_pass(dir: &Path, budget_mib: u64, rows: usize) -> FoldReceipt {
    let home = AgentsHome::from_env();
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    fold_once(FoldInputs {
        cache: dir.join("cursors.json"),
        journal: dir.join("events.jsonl"),
        sources: all_sources(),
        bus: BusIndex::load(&crate::intel::bus_log_path(&fno_dir)),
        witness: SubmitIndex::load(&home.events_jsonl()),
        detect: load_detect_facts(&home),
        now_epoch: chrono::Utc::now().timestamp().max(0) as u64,
        budget: budget_mib * 1024 * 1024,
        row_cap: rows,
    })
}

/// The cache path under the state root, and the tally the report reads.
pub(crate) fn cache_path(state_dir: &Path) -> PathBuf {
    crate::state_layout::place_in(state_dir, "corrections", "cursors.json")
}

fn run_pass(home: &AgentsHome) -> Option<FoldReceipt> {
    let journal = global_journal(home)?;
    let fno_dir = home.root().parent()?.to_path_buf();
    let inputs = FoldInputs {
        cache: cache_path(&fno_dir),
        journal,
        sources: all_sources(),
        bus: BusIndex::load(&crate::intel::bus_log_path(&fno_dir)),
        witness: SubmitIndex::load(&home.events_jsonl()),
        detect: load_detect_facts(home),
        now_epoch: chrono::Utc::now().timestamp().max(0) as u64,
        budget: TICK_READ_BUDGET,
        row_cap: MAX_ROWS_PER_TICK,
    };
    Some(fold_once(inputs))
}

/// The outcome pass, at most hourly, on the fold's own off-loop thread.
fn run_outcome_pass(home: &AgentsHome) {
    let Some(journal) = global_journal(home) else {
        return;
    };
    let Some(fno_dir) = home.root().parent() else {
        return;
    };
    let graph = crate::backlog::settings::graph_path();
    let rows = crate::graph_store::read_rows(&graph).unwrap_or_default();
    let merges = crate::correction_outcome::node_merges(&rows);
    let bus = BusIndex::load(&crate::intel::bus_log_path(fno_dir));
    let now = chrono::Utc::now().timestamp();
    if let Err(error) = crate::correction_outcome::outcome_pass(&journal, &bus, &merges, now) {
        eprintln!("correction outcome: append failed: {error}");
    }
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    last_outcome: Arc<Mutex<Option<Instant>>>,
    in_flight: Arc<AtomicBool>,
}

/// The fleet tick's entry: a 60 s floor, one pass in flight, off the core loop.
pub fn maybe_tick(arm: &Arm, home: AgentsHome) {
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(TICK_INTERVAL_S))
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    let last_outcome = Arc::clone(&arm.last_outcome);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        if let Some(receipt) = run_pass(&home) {
            if let Some(error) = receipt.append_error {
                eprintln!("correction fold: append failed: {error}");
            }
        }
        let due = {
            let mut last = last_outcome.lock().unwrap_or_else(|e| e.into_inner());
            let due = last.is_none_or(|t| t.elapsed() >= Duration::from_secs(OUTCOME_INTERVAL_S));
            if due {
                *last = Some(Instant::now());
            }
            due
        };
        if due {
            run_outcome_pass(&home);
        }
    });
}

#[cfg(test)]
#[path = "correction_fold_tests.rs"]
mod tests;
