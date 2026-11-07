//! The You-thread splitter: the operator's typed turns and the assistant's
//! replies to them, read from one claude transcript and classified by the
//! same provenance rules `fno-agents intel` folds with. This module is a
//! consumer of [`crate::provenance`], never a second classifier: a rule
//! change lands there once, and both readers move together.
//!
//! The walk's shape, settled on the live lead transcript `ed2248ff` (49400
//! rows): a typed user row (or a human `queued_command` attachment, the
//! mid-turn shape) OPENS an item; assistant `type:"text"` blocks EXTEND the
//! open item's reply; every machine-shaped row CLOSES the reply span, and
//! text after it joins no item until the next typed source. The measurement
//! behind the close rule: an assistant text block never directly follows a
//! machine row (0 of 49400), so closing at the machine row skips exactly the
//! text written in reply to injected mail - and a mixed tail a person's work
//! continues is dropped whole, which is the preferred failure (the node asks
//! to skip mail replies, not to risk misattributing them).
//!
//! Deliberately out: slash-command invocations (`HarnessKind::
//! CommandInvocation`). The attended guard counts them as a person acting,
//! but the row's `<command-name>` envelope renders poorly as a You item's
//! typed text; a later change can parse `command-args` and let them open
//! items.

use crate::provenance::{
    classify_text, classify_turn, is_user_turn, turn_stamp, turn_text, BusIndex, HarnessKind,
    Provenance,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Seek};
use std::path::{Path, PathBuf};

/// One typed message with the assistant's visible reply to it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct YouItem {
    /// The typed row's stamp as written (claude `timestamp`).
    pub ts: String,
    /// What the person typed.
    pub typed_text: String,
    /// The assistant's visible text until the next typed source or the
    /// first machine row, whichever came first.
    pub reply_text: String,
    /// The ack-join id: the row's uuid/id, else the derived digest the
    /// queue's cursor folds (`operator_turns::turn_id`, same text, same
    /// absolute line index). Empty when the opener predates the field.
    #[serde(default)]
    pub turn_id: String,
    /// The end-of-turn reply: the claude row with `stop_reason: end_turn`,
    /// or the codex row with `phase: final_answer`. Empty when the reply
    /// never reached one before the span closed.
    #[serde(default)]
    pub end_reply: String,
}

/// Where a walk stopped: the trailing item's resume state. `item_start` is
/// the byte offset of the typed row that opened it, `consumed` the offset
/// through the last complete row (a partial trailing line is re-read next
/// time). Serialized into the cache entry, so field names are contract. The
/// reply is deliberately not carried: a resume re-reads from `item_start`
/// and rebuilds the reply from the same rows, so carrying it would double.
/// `item_line` is the opener row's absolute line index, the fold input a
/// derived turn id needs to survive a resumed walk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WalkTail {
    pub item_start: u64,
    pub consumed: u64,
    pub ts: String,
    pub typed_text: String,
    #[serde(default)]
    pub item_line: u64,
    #[serde(default)]
    pub turn_id: String,
}

/// The person's own words in a turn. Mail delivered into the pane lands as
/// a user row, alone or glued after a typed line when the harness merges
/// queued input, and its header (or a held-mail release line) marks where
/// another agent's words begin.
/// `None` when nothing typed comes before the first header.
fn typed_part(text: &str) -> Option<String> {
    let mut cut = text.len();
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        if crate::mail_header::is_held_release_line(line.trim()) {
            cut = at;
            break;
        }
        if let Some(i) = line.find("`@") {
            if crate::mail_header::is_header_line(&line[i..]) {
                cut = at + i;
                break;
            }
        }
        at += line.len();
    }
    let typed = text[..cut].trim();
    (!typed.is_empty()).then(|| typed.to_string())
}

/// The item a walk holds open: the opener row's facts until a typed row
/// replaces it or a machine row closes it into `items`.
#[derive(Debug, Clone)]
struct OpenItem {
    ts: String,
    typed_text: String,
    turn_id: String,
    end_reply: Option<String>,
}

/// One walk's output: complete items plus the trailing item's resume state.
pub(crate) struct WalkOut {
    pub items: Vec<YouItem>,
    pub tail: Option<WalkTail>,
    /// Bytes consumed through the last complete row, absolute.
    pub consumed: u64,
    /// Absolute line index the consumed offset sits at, the base the next
    /// suffix walk needs so derived turn ids fold a stable row number.
    pub lines: u64,
}

/// Walk one transcript's text into You items. `base` is the absolute offset
/// `raw` starts at and `line_base` the absolute line index it starts at (the
/// count of newlines before `base`, carried by the cache across resumes so a
/// derived turn id folds one stable row number); `resume`, when set, re-opens
/// the trailing item the tail describes - the text then begins at that item's
/// typed row, which the walk re-reads as the same opener while the reply
/// rebuilds from the same rows.
pub(crate) fn walk(
    raw: &str,
    base: u64,
    line_base: u64,
    bus: &BusIndex,
    session: &str,
    resume: Option<WalkTail>,
) -> WalkOut {
    let mut items: Vec<YouItem> = Vec::new();
    let mut open: Option<OpenItem> = None;
    let mut reply: Vec<String> = Vec::new();
    let mut item_start: Option<u64> = None;
    // The currently open item's opener row's absolute line index: the fold
    // input a derived turn id reports through the resume tail.
    let mut open_line: Option<u64> = None;
    let mut headless = false;
    let mut first_user_seen = false;
    let mut resuming = resume.is_some();
    if let Some(tail) = resume {
        // The reply is NOT seeded: the re-read from item_start rebuilds it
        // from the same rows, and seeding would offset every row number the
        // suffix fold computes.
        open = Some(OpenItem {
            ts: tail.ts,
            typed_text: tail.typed_text,
            turn_id: tail.turn_id,
            end_reply: None,
        });
        item_start = Some(tail.item_start);
        open_line = Some(tail.item_line);
    }

    let close = |open: &mut Option<OpenItem>,
                 reply: &mut Vec<String>,
                 item_start: &mut Option<u64>,
                 items: &mut Vec<YouItem>| {
        if let Some(o) = open.take() {
            items.push(YouItem {
                ts: o.ts,
                typed_text: o.typed_text,
                reply_text: reply.join("\n\n"),
                turn_id: o.turn_id,
                end_reply: o.end_reply.unwrap_or_default(),
            });
        }
        reply.clear();
        *item_start = None;
    };
    // A typed source arrives: push the previous item (or, on a resume, the
    // opener re-reads its own row and the reply rebuilds from scratch), then
    // open the new one.
    #[allow(clippy::too_many_arguments)]
    let open_new = |open: &mut Option<OpenItem>,
                    reply: &mut Vec<String>,
                    item_start: &mut Option<u64>,
                    open_line: &mut Option<u64>,
                    items: &mut Vec<YouItem>,
                    resuming: &mut bool,
                    ts: String,
                    text: String,
                    turn_id: String,
                    line_start: u64,
                    line_index: u64| {
        if *resuming {
            *resuming = false;
        } else if let Some(o) = open.take() {
            items.push(YouItem {
                ts: o.ts,
                typed_text: o.typed_text,
                reply_text: reply.join("\n\n"),
                turn_id: o.turn_id,
                end_reply: o.end_reply.unwrap_or_default(),
            });
        }
        reply.clear();
        *open = Some(OpenItem {
            ts,
            typed_text: text,
            turn_id,
            end_reply: None,
        });
        *item_start = Some(line_start);
        *open_line = Some(line_index);
    };

    let bytes = raw.as_bytes();
    let mut pos: usize = 0;
    let mut line_no = line_base;
    while pos < bytes.len() {
        let line_start = base + pos as u64;
        let line = match bytes[pos..].iter().position(|&b| b == b'\n') {
            Some(nl) => {
                let line = &raw[pos..pos + nl];
                pos += nl + 1;
                line
            }
            None => {
                // A complete final row may lack its terminator (the last
                // write of a closed transcript): parse it. A true partial
                // line fails the parse and waits for the next advance.
                let line = &raw[pos..];
                if serde_json::from_str::<Value>(line).is_err() {
                    break;
                }
                pos = bytes.len();
                line
            }
        };
        let opener_line = line_no;
        line_no += 1;
        let Ok(obj) = serde_json::from_str::<Value>(line) else {
            continue; // a row that does not parse is skipped, never fatal
        };
        match obj.get("type").and_then(Value::as_str) {
            Some("attachment") => {
                let att = obj.get("attachment").cloned().unwrap_or(Value::Null);
                let is_human_queued = att.get("type").and_then(Value::as_str)
                    == Some("queued_command")
                    && att.pointer("/origin/kind").and_then(Value::as_str) == Some("human");
                if !is_human_queued {
                    // Hook context and the tool listings ride every typed
                    // turn before its reply: context, never a turn, so the
                    // span stays open.
                    continue;
                }
                let prompt = att.get("prompt").and_then(Value::as_str).unwrap_or("");
                let Some(typed_prompt) = typed_part(prompt) else {
                    close(&mut open, &mut reply, &mut item_start, &mut items);
                    continue;
                };
                let cleaned = match classify_text(prompt, Some((bus, session))) {
                    // The typed shape intel's corpus trusts. A bus-joined
                    // prompt is relay the person pasted into the pane; it
                    // closes the span like any machine row. The cleaned text
                    // is the id fold: the queue derives its digest from the
                    // same classify output.
                    crate::provenance::Verdict::Operator(cleaned) => cleaned,
                    _ => {
                        close(&mut open, &mut reply, &mut item_start, &mut items);
                        continue;
                    }
                };
                open_new(
                    &mut open,
                    &mut reply,
                    &mut item_start,
                    &mut open_line,
                    &mut items,
                    &mut resuming,
                    turn_stamp(&obj).unwrap_or("").to_string(),
                    // argv-fence: exempt. A plain Rust call, not an argv
                    // build: open_new is the walk's own closure, no launcher
                    // parser touches this path. The .to_string() on the
                    // String is the element shape the fence-gate scanner
                    // reads; the marker keeps the site visible in its
                    // census, and the marker keeps the site visible in its census, and the exempt count floats.
                    typed_prompt.to_string(),
                    crate::operator_turns::turn_id(&obj, &cleaned, opener_line as usize),
                    line_start,
                    opener_line,
                );
            }
            Some("user") => {
                headless |= obj
                    .get("entrypoint")
                    .and_then(Value::as_str)
                    .is_some_and(|e| e.starts_with("sdk"));
                if crate::provenance::json_truthy(obj.get("isMeta")) {
                    close(&mut open, &mut reply, &mut item_start, &mut items);
                    continue;
                }
                let Some(p) = classify_user_row(&obj, bus, session) else {
                    continue; // tool results and empty rows never touch the walk
                };
                // The headless seed: a headless session's first user row is
                // the machine's dispatch payload (intel_corpus.rs:840), not a
                // person's turn.
                let seed = !first_user_seen && headless && p == Provenance::Unknown;
                first_user_seen |= matches!(p, Provenance::Unknown | Provenance::Operator);
                let typed = matches!(p, Provenance::Unknown | Provenance::Operator) && !seed;
                if !typed {
                    close(&mut open, &mut reply, &mut item_start, &mut items);
                    continue;
                }
                let text = turn_text(&obj);
                if text.trim().is_empty() {
                    continue;
                }
                let Some(text) = typed_part(&text) else {
                    close(&mut open, &mut reply, &mut item_start, &mut items);
                    continue;
                };
                let cleaned = crate::provenance::classify(&turn_text(&obj)).unwrap_or_default();
                open_new(
                    &mut open,
                    &mut reply,
                    &mut item_start,
                    &mut open_line,
                    &mut items,
                    &mut resuming,
                    turn_stamp(&obj).unwrap_or("").to_string(),
                    text,
                    crate::operator_turns::turn_id(&obj, &cleaned, opener_line as usize),
                    line_start,
                    opener_line,
                );
            }
            Some("assistant") => {
                let Some(o) = open.as_mut() else {
                    continue;
                };
                let text = turn_text(&obj);
                if !text.trim().is_empty() {
                    reply.push(text.clone());
                }
                // The end-of-turn reply: the row the harness closed the turn
                // with (repeated_asks.rs reads the same two fields). A
                // sidechain's end_turn is another agent's work, never this
                // reply.
                let sidechain = obj.get("isSidechain").and_then(Value::as_bool) == Some(true);
                if o.end_reply.is_none()
                    && !sidechain
                    && obj
                        .get("message")
                        .and_then(|m| m.get("stop_reason"))
                        .and_then(Value::as_str)
                        == Some("end_turn")
                    && !text.trim().is_empty()
                {
                    o.end_reply = Some(text);
                }
            }
            Some("response_item") => {
                // The codex rollout leg: a payload user message opens an
                // item, an assistant message extends the reply, and the
                // `final_answer` phase is the turn's end reply. Every other
                // payload kind is the agent's plumbing.
                let payload = obj.get("payload").cloned().unwrap_or(Value::Null);
                if payload.get("type").and_then(Value::as_str) != Some("message") {
                    continue;
                }
                match payload.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        let text = turn_text(&obj);
                        if text.trim().is_empty() {
                            continue;
                        }
                        if matches!(
                            classify_turn(&obj, bus, session),
                            Provenance::Harness(_) | Provenance::Relay(_)
                        ) {
                            close(&mut open, &mut reply, &mut item_start, &mut items);
                            continue;
                        }
                        if crate::provenance::classify(&text).is_err() {
                            // Harness machinery (bare commands, skip rules):
                            // the queue refuses it, so no ack join exists.
                            close(&mut open, &mut reply, &mut item_start, &mut items);
                            continue;
                        }
                        // Codex harness context: the rollout opens with the
                        // machine's own instructions payload, which reads as
                        // a user message. No person asked it; it opens no
                        // item. The prefixes are the measured envelope
                        // shapes (sampled off a live 2-day rollout).
                        let trimmed = text.trim_start();
                        if [
                            "# AGENTS.md instructions",
                            "<INSTRUCTIONS>",
                            "<ENVIRONMENT_CONTEXT>",
                            "<user_instructions>",
                            "<turn_context>",
                        ]
                        .iter()
                        .any(|p| trimmed.starts_with(p))
                        {
                            close(&mut open, &mut reply, &mut item_start, &mut items);
                            continue;
                        }
                        let Some(text) = typed_part(&text).or_else(|| {
                            let t = text.trim();
                            (!t.is_empty()).then(|| t.to_string())
                        }) else {
                            close(&mut open, &mut reply, &mut item_start, &mut items);
                            continue;
                        };
                        let cleaned =
                            crate::provenance::classify(&turn_text(&obj)).unwrap_or_default();
                        open_new(
                            &mut open,
                            &mut reply,
                            &mut item_start,
                            &mut open_line,
                            &mut items,
                            &mut resuming,
                            turn_stamp(&obj).unwrap_or("").to_string(),
                            text,
                            crate::operator_turns::turn_id(&obj, &cleaned, opener_line as usize),
                            line_start,
                            opener_line,
                        );
                    }
                    Some("assistant") => {
                        let Some(o) = open.as_mut() else {
                            continue;
                        };
                        let text = turn_text(&obj);
                        if !text.trim().is_empty() {
                            reply.push(text.clone());
                        }
                        if o.end_reply.is_none()
                            && payload.get("phase").and_then(Value::as_str) == Some("final_answer")
                            && !text.trim().is_empty()
                        {
                            o.end_reply = Some(text);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let consumed = base + pos as u64;
    let lines = line_no;
    let mut tail = None;
    if let Some(o) = open {
        items.push(YouItem {
            reply_text: reply.join("\n\n"),
            ts: o.ts.clone(),
            typed_text: o.typed_text.clone(),
            turn_id: o.turn_id.clone(),
            end_reply: o.end_reply.unwrap_or_default(),
        });
        tail = Some(WalkTail {
            item_start: item_start.unwrap_or(base),
            consumed,
            ts: o.ts,
            typed_text: o.typed_text,
            item_line: open_line.unwrap_or(0),
            turn_id: o.turn_id,
        });
    }
    WalkOut {
        items,
        tail,
        consumed,
        lines,
    }
}

/// The trailing item's opener row's absolute line index. A resume re-reads
/// from `item_start`, so the opener is the suffix's first row and its index
/// is the newline count before `base`; on a non-resume walk the opener is
/// found by the same count from zero.
/// Classify one non-meta user row. `None` for shapes that never touch the
/// walk: tool results (a turn's own plumbing) and rows with no text.
fn classify_user_row(obj: &Value, bus: &BusIndex, session: &str) -> Option<Provenance> {
    if !is_user_turn(obj) {
        return None;
    }
    let text = turn_text(obj);
    if text.trim().is_empty() {
        return None;
    }
    let p = classify_turn(obj, bus, session);
    if matches!(p, Provenance::Harness(HarnessKind::CommandInvocation)) {
        // Out of scope by the module doc: the envelope renders poorly.
        return None;
    }
    Some(p)
}

/// The per-session cache behind the projection: what one transcript's walk
/// produced, kept beside intel's rollup cache under `<home>/intel/`. A
/// sibling file with its own VERSION, never a shared one: intel's version
/// bumps must not invalidate You entries and vice versa. Shape follows
/// `RollupEntry` (offset + head sample), advanced offset-anchored: an alive
/// entry re-walks only from the trailing item's start, replaces it, and
/// appends the rest.
#[derive(Debug, Default, Serialize, Deserialize)]
struct YouCache {
    version: u32,
    entries: BTreeMap<String, YouEntry>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct YouEntry {
    /// Bytes consumed through the last complete row at cache time.
    offset: u64,
    /// Absolute line index `offset` sits at, the fold base for derived turn
    /// ids on the next suffix walk.
    #[serde(default)]
    lines: u64,
    head_sha: String,
    head_len: u64,
    items: Vec<YouItem>,
    /// The trailing item's resume state, `None` when the transcript ended on
    /// a machine row (the next advance reads only new bytes).
    open: Option<WalkTail>,
}

const YOU_CACHE_VERSION: u32 = 4;

/// The loaded cache plus its lock discipline. `save` is best effort: an
/// unwritable cache degrades the next projection to a full re-walk.
pub(crate) struct YouStore {
    path: PathBuf,
    cache: YouCache,
    /// Persisted on save; a contended lock runs advance read-only.
    persist: bool,
}

impl YouStore {
    pub(crate) fn open(path: PathBuf, persist: bool) -> Self {
        let cache = std::fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<YouCache>(&raw).ok())
            .filter(|c| c.version == YOU_CACHE_VERSION)
            .unwrap_or(YouCache {
                version: YOU_CACHE_VERSION,
                entries: Default::default(),
            });
        YouStore {
            path,
            cache,
            persist,
        }
    }

    /// One session's You items, oldest first, advancing the cache entry.
    pub(crate) fn advance(&mut self, path: &Path, session: &str, bus: &BusIndex) -> Vec<YouItem> {
        let Ok(meta) = std::fs::metadata(path) else {
            return self
                .cache
                .entries
                .get(session)
                .map(|e| e.items.clone())
                .unwrap_or_default();
        };
        let size = meta.len();
        let entry = self.cache.entries.get(session);
        let alive = entry.is_some_and(|e| {
            size >= e.offset
                && crate::transcript_activity::head_sample(path, e.head_len).as_deref()
                    == Some(e.head_sha.as_str())
        });
        let out = if alive {
            let e = entry.expect("checked");
            let (start, line_base) = match e.open.as_ref() {
                Some(t) => (t.item_start, t.item_line),
                None => (e.offset, e.lines),
            };
            match read_range(path, start, size) {
                // A resumed walk sees only the suffix, so the cached items
                // stay the prefix: drop exactly the open one it rebuilds,
                // or none when the previous entry closed cleanly.
                Ok(text) => {
                    let out = walk(&text, start, line_base, bus, session, e.open.clone());
                    let keep = e.items.len().saturating_sub(usize::from(e.open.is_some()));
                    let mut items = e.items[..keep].to_vec();
                    items.extend(out.items);
                    WalkOut {
                        items,
                        tail: out.tail,
                        consumed: out.consumed,
                        lines: out.lines,
                    }
                }
                Err(_) => full_walk(path, bus, session),
            }
        } else {
            full_walk(path, bus, session)
        };
        let mut head_len = size.min(crate::transcript_activity::HEAD_SAMPLE_BYTES);
        let head_sha = crate::transcript_activity::head_sample(path, head_len).unwrap_or_default();
        if head_sha.is_empty() {
            // No head, no entry: the next advance retries fresh instead of
            // carrying a head that reads as replaced forever.
            head_len = 0;
        }
        self.cache.entries.insert(
            session.to_string(),
            YouEntry {
                offset: out.consumed,
                lines: out.lines,
                head_sha,
                head_len,
                items: out.items.clone(),
                open: out.tail,
            },
        );
        out.items
    }

    /// Atomic save, mode 600. A read-only store (contended lock, no home)
    /// saves nothing.
    pub(crate) fn save(&self) {
        if !self.persist {
            return;
        }
        let Ok(doc) = serde_json::to_vec(&self.cache) else {
            return;
        };
        let Some(parent) = self.path.parent() else {
            return;
        };
        let _ = std::fs::create_dir_all(parent);
        let tmp = parent.join("you-threads.json.tmp");
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
        else {
            return;
        };
        if f.write_all(&doc).is_err() {
            return;
        }
        let _ = std::fs::rename(&tmp, &self.path);
    }
}

fn full_walk(path: &Path, bus: &BusIndex, session: &str) -> WalkOut {
    match std::fs::read_to_string(path) {
        Ok(text) => walk(&text, 0, 0, bus, session, None),
        Err(_) => WalkOut {
            items: Vec::new(),
            tail: None,
            consumed: 0,
            lines: 0,
        },
    }
}

fn read_range(path: &Path, start: u64, end: u64) -> Result<String, std::io::Error> {
    if start >= end {
        return Ok(String::new());
    }
    let mut file = std::fs::File::open(path)?;
    file.seek(std::io::SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((end - start) as usize);
    file.take(end - start).read_to_end(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// The freshness window: a session whose transcript mtime falls inside it
/// gets a You thread in the projection. Settled 2026-10-27 from the measured
/// cost ladder (2086 claude transcripts, 3.6 GB): the 2-day window held 19
/// files and 255 MB; the 7-day window held 173 files and 556 MB, too heavy
/// for a cold tab open.
const WINDOW_SECS: u64 = 2 * 86_400;

/// Everything the projection-facing walk needs, injectable so tests run
/// against fixtures instead of a live home.
pub(crate) struct YouInputs<'a> {
    pub registry: &'a [Value],
    pub projects_dir: PathBuf,
    /// The fno home (`~/.fno`): the bus log and the You cache resolve
    /// through it exactly as `journal_bus_path` builds them.
    pub home_root: PathBuf,
    pub cache_path: PathBuf,
    /// The operator-capture dir: the session enumerator (each
    /// `<session>.scan.json` names its transcript) and the ack-ledger home.
    pub capture_dir: PathBuf,
}

/// One You row per session whose transcript carries typed turns inside the
/// window, newest first: every registry-named claude session, plus every
/// capture-dir session (`*.scan.json` names the transcript) the registry
/// misses - codex rollouts and sessions no mail ever named. Each item embeds
/// its `state` (`answered` / `pending` / `acked`) read from the end reply
/// and the session's ack ledger. The caller embeds the rows as the
/// projection's `you` section. Fail-open end to end: a missing bus, a
/// contended cache lock, a missing transcript, an unreadable capture dir or
/// a registry with no claude rows all read as fewer or empty rows, never an
/// error.
pub(crate) fn project_you(inputs: &YouInputs) -> Vec<Value> {
    let dot_fno = inputs.home_root.parent().unwrap_or(&inputs.home_root);
    let bus = BusIndex::load(&crate::mail_paths::bus_log_path(dot_fno));
    let lock =
        crate::gh_budget::FileLock::try_acquire(&crate::gh_budget::lock_path(&inputs.cache_path));
    let persist = lock.is_ok();
    let mut store = YouStore::open(inputs.cache_path.clone(), persist);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Display names from the registry across every harness, keyed by session
    // id; the capture-dir leg falls back to the id's first eight chars.
    let names: BTreeMap<&str, &str> = inputs
        .registry
        .iter()
        .filter_map(|reg| {
            let sid = reg.get("harness_session_id")?.as_str()?;
            let name = reg.get("name")?.as_str()?;
            (!sid.is_empty() && !name.is_empty())
                .then(|| (sid, name))
                .or(None)
        })
        .collect();
    // The walk list: registry claude rows first (the shape the Me rows have
    // always read), then capture-dir sessions the registry missed.
    let mut sessions: Vec<(String, PathBuf)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = Default::default();
    for reg in inputs.registry {
        let harness = reg.get("harness").and_then(Value::as_str).unwrap_or("");
        let Some(sid) = reg
            .get("harness_session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if harness != "claude" {
            continue;
        }
        let cwd = reg.get("cwd").and_then(Value::as_str).unwrap_or("");
        let slug = crate::claude_ask::claude_cwd_slug(Path::new(cwd));
        let transcript = inputs.projects_dir.join(slug).join(format!("{sid}.jsonl"));
        if seen.insert(sid.to_string()) {
            sessions.push((sid.to_string(), transcript));
        }
    }
    for (sid, transcript) in capture_sessions(&inputs.capture_dir, now) {
        if seen.insert(sid.clone()) {
            sessions.push((sid, transcript));
        }
    }
    let mut rows: Vec<Value> = Vec::new();
    for (sid, transcript) in sessions {
        let Ok(meta) = std::fs::metadata(&transcript) else {
            continue;
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(mtime) > WINDOW_SECS {
            continue;
        }
        let acked = crate::operator_turns::read_acked_turn_ids(
            &crate::operator_turns::ledger_path(&inputs.capture_dir, &sid),
        )
        .unwrap_or_default();
        let items = store.advance(&transcript, &sid, &bus);
        if items.is_empty() {
            continue;
        }
        let items: Vec<Value> = items.iter().map(|i| item_json(i, &acked)).collect();
        rows.push(json!({
            "session_id": sid,
            "name": names
                .get(sid.as_str())
                .map(|n| n.to_string())
                .unwrap_or_else(|| sid.get(..8).unwrap_or(&sid).to_string()),
            "last_ts": items
                .last()
                .and_then(|i| i.get("ts").and_then(Value::as_str))
                .unwrap_or_default()
                .to_string(),
            "items": items,
        }));
    }
    if persist {
        store.save();
    }
    rows.sort_by(|a, b| {
        b.get("last_ts")
            .and_then(Value::as_str)
            .cmp(&a.get("last_ts").and_then(Value::as_str))
    });
    rows
}

/// Capture-dir sessions: each window-fresh `<session>.scan.json` names its
/// transcript, which is how codex rollouts and unregistered sessions enter.
/// A scan file that fails to parse or names a missing transcript is skipped.
fn capture_sessions(capture_dir: &Path, now: u64) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(capture_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(sid) = name.strip_suffix(".scan.json") else {
            continue;
        };
        if sid.is_empty() {
            continue;
        }
        let transcript = std::fs::read_to_string(entry.path())
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|v| {
                v.get("transcript")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            });
        // The window reads the TRANSCRIPT's mtime, never the scan file's:
        // a session typed into today whose last queue read is three days
        // old would otherwise fall out of the view until its next hook
        // beat rewrites the scan.
        if let Some(transcript) = transcript {
            let fresh = std::fs::metadata(&transcript)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .is_some_and(|mtime| now.saturating_sub(mtime) <= WINDOW_SECS);
            if fresh {
                out.push((sid.to_string(), transcript));
            }
        }
    }
    out
}

/// One item's projection shape: the YouItem fields plus the state the ack
/// ledger and the reply text decide.
fn item_json(item: &YouItem, acked: &std::collections::HashSet<String>) -> Value {
    let mut v = serde_json::to_value(item).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.insert(
            "state".into(),
            Value::String(item_state(item, acked).into()),
        );
    }
    v
}

/// An item's state: `acked` when its turn id sits in the session ledger;
/// else `pending` when the answer is empty or a deferral-only stub
/// ("Queued; I will report when it lands."); else `answered`. The deferral
/// flip rule the plan once sketched is deleted: measured on 133a419b (646
/// items), only two replies were deferral-shaped, neither a pure stub, and
/// the rule would read work text as a non-answer.
fn item_state(item: &YouItem, acked: &std::collections::HashSet<String>) -> &'static str {
    if acked.contains(&item.turn_id) {
        return "acked";
    }
    let text = if item.end_reply.trim().is_empty() {
        &item.reply_text
    } else {
        &item.end_reply
    };
    let t = text.trim();
    if t.is_empty() || is_deferral_only(t) {
        "pending"
    } else {
        "answered"
    }
}

/// A reply that is one short line opening with a deferral phrase and
/// carrying nothing else (AC3's "Queued; I will report when it lands.").
fn is_deferral_only(t: &str) -> bool {
    if t.contains('\n') || t.chars().count() > 120 {
        return false;
    }
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"(?i)^(queued|noted|on it|ack\w*|in progress)\b").expect("valid pattern")
    })
    .is_match(t)
}

/// The flat cross-session asks list behind `fno inbox user list --all`:
/// one row per item over every enumerated session, pending first, the rest
/// newest first. Row fields: `session_id`, `name`, `turn_id`, `ts`,
/// `typed_text`, `reply_excerpt`, `state`.
pub(crate) fn asks_rows(inputs: &YouInputs) -> Vec<Value> {
    let mut flat: Vec<Value> = project_you(inputs)
        .iter()
        .flat_map(|row| {
            let sid = row.get("session_id").and_then(Value::as_str).unwrap_or("");
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            row.get("items")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            json!({
                                "session_id": sid,
                                "name": name,
                                "turn_id": item.get("turn_id").and_then(Value::as_str).unwrap_or(""),
                                "ts": item.get("ts").and_then(Value::as_str).unwrap_or(""),
                                "typed_text": item.get("typed_text").and_then(Value::as_str).unwrap_or(""),
                                "reply_excerpt": reply_excerpt(item),
                                "state": item.get("state").and_then(Value::as_str).unwrap_or(""),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect();
    let state_rank = |v: &Value| match v.get("state").and_then(Value::as_str) {
        Some("pending") => 0,
        _ => 1,
    };
    fn row_ts(v: &Value) -> &str {
        v.get("ts").and_then(Value::as_str).unwrap_or("")
    }
    flat.sort_by(|a, b| {
        state_rank(a)
            .cmp(&state_rank(b))
            .then(row_ts(b).cmp(row_ts(a)))
    });
    flat
}

/// One line of the reply an ask got: the end reply when the turn reached
/// one, else the visible reply; newline runs collapse so a row stays one
/// row; cut at 200 chars.
fn reply_excerpt(item: &Value) -> String {
    let text = item
        .get("end_reply")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| item.get("reply_text").and_then(Value::as_str).unwrap_or(""));
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(200).collect();
    if flat.chars().count() > 200 {
        out.push('\u{2026}');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(type_: &str, text: &str, ts: &str) -> String {
        json!({
            "type": type_,
            "timestamp": ts,
            "message": {"role": if type_ == "assistant" { "assistant" } else { "user" }, "content": text},
        })
        .to_string()
    }

    fn tool_result_row() -> String {
        json!({
            "type": "user",
            "timestamp": "2026-10-27T00:00:02Z",
            "message": {"role": "user", "content": [{"type": "tool_result", "content": "ok"}]},
        })
        .to_string()
    }

    fn meta_row() -> String {
        json!({
            "type": "user",
            "isMeta": true,
            "timestamp": "2026-10-27T00:00:03Z",
            "message": {"role": "user", "content": "Stop hook feedback: allowed"},
        })
        .to_string()
    }

    fn queued_row(prompt: &str, ts: &str) -> String {
        json!({
            "type": "attachment",
            "timestamp": ts,
            "attachment": {
                "type": "queued_command",
                "prompt": prompt,
                "origin": {"kind": "human"},
                "humanTurn": true,
            },
        })
        .to_string()
    }

    fn hook_attachment_row() -> String {
        json!({
            "type": "attachment",
            "timestamp": "2026-10-27T00:00:04Z",
            "attachment": {"type": "hook_context", "context": "mail digest"},
        })
        .to_string()
    }

    fn assistant_row(text: &str, ts: &str) -> String {
        row("assistant", text, ts)
    }

    /// The walk scenario the node asks about: a typed question with its
    /// reply (tool results keep the span open), a machine row and a
    /// delivered mail that close the span, and a second typed turn after it.
    #[test]
    fn typed_reply_machine_close_and_a_second_item() {
        let raw = [
            row("user", "why did the gate refuse?", "2026-10-27T00:00:00Z"),
            assistant_row("Because the base was stale.", "2026-10-27T00:00:01Z"),
            tool_result_row(),
            assistant_row("I merged main into the branch.", "2026-10-27T00:00:02.5Z"),
            meta_row(),
            assistant_row("Acked the hook.", "2026-10-27T00:00:05Z"),
            // Mail delivered into the pane, no bus row to join: its header
            // marks it as another agent's, so it opens no item.
            row(
                "user",
                "`@tomas · fmail-0123456789ab`\nRun your check-in now.",
                "2026-10-27T00:00:06Z",
            ),
            assistant_row("Checked in.", "2026-10-27T00:00:07Z"),
            row(
                "user",
                "2 held messages · sent 17:32 to 17:41 · held 32m",
                "2026-10-27T00:00:08Z",
            ),
            // A typed line with queued mail merged after it keeps only the
            // typed words.
            row(
                "user",
                "ship it then `@ines · fmail-0123456789ac`\nFrom ines.",
                "2026-10-27T00:00:09Z",
            ),
            assistant_row("Opening the PR.", "2026-10-27T00:00:10Z"),
        ]
        .join("\n");
        let items = walk(&raw, 0, 0, &BusIndex::empty(), "sess", None).items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].typed_text, "why did the gate refuse?");
        assert_eq!(
            items[0].reply_text,
            "Because the base was stale.\n\nI merged main into the branch."
        );
        assert_eq!(items[1].typed_text, "ship it then");
        assert_eq!(items[1].reply_text, "Opening the PR.");
    }

    /// A human queued command opens its own item; a pasted bus row closes
    /// instead of opening; a hook attachment neither opens nor closes, so
    /// the reply after it stays with its turn.
    #[test]
    fn queued_opens_relay_closes_attachment_never_opens() {
        let bus_text = "`@worker · fmail-abc` status please";
        let bus_file =
            std::env::temp_dir().join(format!("you-threads-bus-{}.jsonl", std::process::id()));
        std::fs::write(
            &bus_file,
            json!({
                "id": "m1",
                "ts": "2026-10-27T00:00:00Z",
                "body": bus_text,
                "meta": {"to_session": "sess"},
            })
            .to_string(),
        )
        .expect("write bus fixture");
        let bus = BusIndex::load(&bus_file);
        let _ = std::fs::remove_file(&bus_file);
        let raw = [
            row("user", "before the digest", "2026-10-27T00:00:00Z"),
            hook_attachment_row(),
            queued_row("and the second thing", "2026-10-27T00:00:03Z"),
            hook_attachment_row(),
            assistant_row("Answering both.", "2026-10-27T00:00:06Z"),
            queued_row(bus_text, "2026-10-27T00:00:07Z"),
            assistant_row("Relay ack text.", "2026-10-27T00:00:08Z"),
        ]
        .join("\n");
        let items = walk(&raw, 0, 0, &bus, "sess", None).items;
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].typed_text, "before the digest");
        assert_eq!(items[0].reply_text, "");
        assert_eq!(items[1].typed_text, "and the second thing");
        assert_eq!(items[1].reply_text, "Answering both.");
    }

    /// A headless session's seed row is the machine's dispatch payload, and
    /// rows that fail to parse are skipped without ending the walk.
    #[test]
    fn headless_seed_and_unparsable_rows() {
        let raw = [
            "{not json".to_string(),
            json!({"type": "user", "timestamp": "2026-10-27T00:00:00Z", "entrypoint": "sdk-ts",
                   "message": {"role": "user", "content": "implement node x-1"}})
            .to_string(),
            assistant_row("Starting.", "2026-10-27T00:00:01Z"),
            row("user", "still counts", "2026-10-27T00:00:02Z"),
            assistant_row("Answered.", "2026-10-27T00:00:03Z"),
        ]
        .join("\n");
        let items = walk(&raw, 0, 0, &BusIndex::empty(), "sess", None).items;
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].typed_text, "still counts");
        assert_eq!(items[0].reply_text, "Answered.");
    }

    fn temp_transcript(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("you-threads-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    /// Advance appends across runs and resumes the trailing item: the
    /// second advance re-walks only from the open item's start and its
    /// reply grows in place, never duplicating.
    #[test]
    fn advance_resumes_trailing_item_and_appends() {
        let file = temp_transcript("resume.jsonl");
        let part1 = [
            row("user", "first question", "2026-10-27T00:00:00Z"),
            assistant_row("First answer.", "2026-10-27T00:00:01Z"),
            row("user", "second question", "2026-10-27T00:00:02Z"),
        ]
        .join("\n");
        std::fs::write(&file, format!("{part1}\n")).expect("write part 1");
        let mut store = YouStore::open(PathBuf::from("/dev/null"), false);
        let items = store.advance(&file, "s1", &BusIndex::empty());
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].typed_text, "second question");
        assert_eq!(items[1].reply_text, "");

        let part2 = [
            assistant_row("Second answer part one.", "2026-10-27T00:00:03Z"),
            assistant_row("Part two.", "2026-10-27T00:00:04Z"),
            row("user", "third question", "2026-10-27T00:00:05Z"),
            assistant_row("Third answer.", "2026-10-27T00:00:06Z"),
        ]
        .join("\n");
        let mut text = std::fs::read_to_string(&file).expect("read");
        text.push_str(&part2);
        text.push('\n');
        std::fs::write(&file, &text).expect("write part 2");
        let items = store.advance(&file, "s1", &BusIndex::empty());
        assert_eq!(items.len(), 3);
        assert_eq!(items[1].reply_text, "Second answer part one.\n\nPart two.");
        assert_eq!(items[2].typed_text, "third question");

        // An unchanged transcript re-serves the cached items byte for byte.
        let again = store.advance(&file, "s1", &BusIndex::empty());
        assert_eq!(again, items);
        let _ = std::fs::remove_file(&file);
    }

    /// A shrunken or rewritten transcript fails the alive check and the
    /// entry rebuilds from a full walk.
    #[test]
    fn shrunken_transcript_rebuilds_fresh() {
        let file = temp_transcript("shrink.jsonl");
        std::fs::write(
            &file,
            format!("{}\n", row("user", "gone soon", "2026-10-27T00:00:00Z")),
        )
        .expect("write big");
        let mut store = YouStore::open(PathBuf::from("/dev/null"), false);
        assert_eq!(store.advance(&file, "s2", &BusIndex::empty()).len(), 1);
        std::fs::write(
            &file,
            format!("{}\n", row("user", "fresh start", "2026-10-27T00:09:00Z")),
        )
        .expect("rewrite small");
        let items = store.advance(&file, "s2", &BusIndex::empty());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].typed_text, "fresh start");
        let _ = std::fs::remove_file(&file);
    }

    /// AC1: a claude typed turn with tool noise between its end_turn blocks
    /// carries the row uuid as turn_id and the end_turn text as end_reply.
    #[test]
    fn ac1_claude_item_carries_turn_id_and_end_reply() {
        let uuid = "u-ac1-typed";
        let raw = [
            format!(
                "{}",
                json!({"type": "user", "uuid": uuid, "timestamp": "2026-10-27T00:00:00Z",
                       "message": {"role": "user", "content": "what landed in the gate fix?"}})
            ),
            json!({"type": "assistant", "timestamp": "2026-10-27T00:00:01Z",
                   "message": {"role": "assistant", "content": [{"type": "text", "text": "Reading the log."}]}})
                .to_string(),
            tool_result_row(),
            json!({"type": "assistant", "timestamp": "2026-10-27T00:00:02Z",
                   "message": {"role": "assistant", "stop_reason": "end_turn",
                               "content": [{"type": "text", "text": "Two commits: the pin and the gate."}]}})
                .to_string(),
            // A sidechain end_turn is another agent's work, never the reply.
            json!({"type": "assistant", "isSidechain": true, "timestamp": "2026-10-27T00:00:03Z",
                   "message": {"role": "assistant", "stop_reason": "end_turn",
                               "content": [{"type": "text", "text": "sidechain text"}]}})
                .to_string(),
        ]
        .join("\n");
        let out = walk(&raw, 0, 0, &BusIndex::empty(), "sess", None);
        assert_eq!(out.items.len(), 1);
        let it = out.items.first().expect("one item");
        assert_eq!(it.turn_id, uuid);
        assert_eq!(it.end_reply, "Two commits: the pin and the gate.");
        // The reply_text keeps every visible block, unchanged for the paint.
        assert!(it.reply_text.contains("Reading the log."));
        assert!(it.reply_text.contains("Two commits: the pin and the gate."));
    }

    /// AC2: a codex rollout with a user message, commentary and a
    /// final_answer opens one item whose end_reply is the final_answer text;
    /// the commentary never reads as the end. The rollout's harness context
    /// payload opens no item either: the machine's instructions block is
    /// nobody's ask, and a real ask after it still opens.
    #[test]
    fn ac2_codex_pairs_final_answer_and_context_opens_nothing() {
        let payload_row = |role: &str, phase: Option<&str>, text: &str| {
            let mut p = json!({
                "type": "message",
                "role": role,
                "content": [{"type": "text", "text": text}],
            });
            if let Some(phase) = phase {
                p.as_object_mut()
                    .expect("object payload")
                    .insert("phase".into(), json!(phase));
            }
            json!({"timestamp": "2026-11-05T01:55:00Z", "type": "response_item",
                   "payload": p})
            .to_string()
        };
        let raw = [
            payload_row("user", None, "why did the codex leg refuse?"),
            payload_row(
                "assistant",
                Some("commentary"),
                "Checking the rollout shape.",
            ),
            payload_row(
                "assistant",
                Some("final_answer"),
                "The id sat at payload.id.",
            ),
        ]
        .join("\n");
        let out = walk(&raw, 0, 0, &BusIndex::empty(), "sess", None);
        assert_eq!(out.items.len(), 1);
        // No uuid on codex rows: the derived digest folds the line index.
        let it = out.items.first().expect("one item");
        assert!(it.turn_id.starts_with("derived-"));
        assert_eq!(it.end_reply, "The id sat at payload.id.");
        assert_eq!(
            it.reply_text,
            "Checking the rollout shape.\n\nThe id sat at payload.id."
        );
        // The context shape: the instructions payload opens nothing, and the
        // real ask after it still pairs with its final answer.
        let raw = [
            payload_row(
                "user",
                None,
                "# AGENTS.md instructions\n\n<INSTRUCTIONS>\nwork here\n</INSTRUCTIONS>",
            ),
            payload_row("assistant", None, "Understood the instructions."),
            payload_row("user", None, "status on the gate?"),
            payload_row("assistant", Some("final_answer"), "Green."),
        ]
        .join("\n");
        let out = walk(&raw, 0, 0, &BusIndex::empty(), "sess", None);
        assert_eq!(
            out.items.len(),
            1,
            "only the real ask opens: {:?}",
            out.items
        );
        let it = out.items.first().expect("one item");
        assert_eq!(it.typed_text, "status on the gate?");
        assert_eq!(it.end_reply, "Green.");
    }

    /// A derived turn id survives a resume: the cached suffix walk folds the
    /// opener's absolute line index from the tail, so the id matches the
    /// full walk's for the same row.
    #[test]
    fn codex_derived_turn_id_survives_resume() {
        let payload_row = |role: &str, text: &str| {
            let mut payload = json!({"type": "message", "role": role,
                     "content": [{"type": "text", "text": text}]});
            if role == "assistant" {
                payload
                    .as_object_mut()
                    .expect("object payload")
                    .insert("phase".into(), json!("final_answer"));
            }
            json!({"timestamp": "2026-11-05T02:00:00Z", "type": "response_item",
                   "payload": payload})
            .to_string()
        };
        let noise_row = || {
            json!({"timestamp": "2026-11-05T02:00:01Z", "type": "response_item",
                   "payload": {"type": "function_call", "name": "shell",
                               "arguments": "{}"}})
            .to_string()
        };
        let file = temp_transcript("codex-resume.jsonl");
        let part1 = (0..5)
            .map(|_| noise_row())
            .chain([payload_row("user", "first ask")])
            .collect::<Vec<_>>();
        std::fs::write(&file, part1.join("\n") + "\n").expect("write part 1");
        let mut store = YouStore::open(PathBuf::from("/dev/null"), false);
        let first = store.advance(&file, "cx", &BusIndex::empty());
        assert_eq!(first.len(), 1);
        let first_id = first.first().expect("one item").turn_id.clone();
        assert!(first_id.starts_with("derived-"));

        // The turn's reply lands; the resume re-walks from the opener and
        // must fold the same absolute row index.
        let part2 = [
            payload_row("assistant", "the answer"),
            payload_row("user", "second ask"),
        ]
        .join("\n");
        let mut text = std::fs::read_to_string(&file).expect("read");
        text.push_str(&part2);
        text.push('\n');
        std::fs::write(&file, &text).expect("write part 2");
        let second = store.advance(&file, "cx", &BusIndex::empty());
        assert_eq!(second.len(), 2);
        let second_first = second.first().expect("two items");
        assert_eq!(second_first.turn_id, first_id);
        assert_eq!(second_first.end_reply, "the answer");

        // A full walk over the same rows derives the same id: the queue's
        // ack join reads exactly this value.
        let full = walk(
            &std::fs::read_to_string(&file).expect("read"),
            0,
            0,
            &BusIndex::empty(),
            "cx",
            None,
        );
        assert_eq!(full.items.first().expect("one item").turn_id, first_id);
        let _ = std::fs::remove_file(&file);
    }

    /// AC3/AC4: the state rules - a deferral-only reply stays pending; the
    /// same turn acked through the ledger reads acked.
    #[test]
    fn ac3_ac4_state_rules() {
        let mk = |reply: &str, end: &str| YouItem {
            ts: "2026-11-06T10:00:00Z".into(),
            typed_text: "status on the gate?".into(),
            reply_text: reply.into(),
            turn_id: "t-3".into(),
            end_reply: end.into(),
        };
        // AC3: a pure "queued" stub is a non-answer.
        let queued = mk("Queued; I will report when it lands.", "");
        assert_eq!(item_state(&queued, &Default::default()), "pending");
        // A deferral that carries findings is an answer.
        let substantive = mk("Looking into the 127s first; the pin needs a rebase.", "");
        assert_eq!(item_state(&substantive, &Default::default()), "answered");
        // An empty reply is pending.
        assert_eq!(item_state(&mk("", ""), &Default::default()), "pending");
        // AC4: the same turn in the session's ledger reads acked.
        let mut acked = std::collections::HashSet::new();
        acked.insert("t-3".to_string());
        assert_eq!(item_state(&queued, &acked), "acked");
    }

    /// AC5: asks_rows flattens two fixture sessions, pending rows first
    /// even when the answered row is newer, newest first within each half.
    #[test]
    fn ac5_asks_rows_flatten_two_sessions_pending_first() {
        let dir = std::env::temp_dir().join(format!("you-threads-asks-{}-ac5", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let capture = dir.join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        // Session A: an answered ask with the newer stamp.
        let ta = dir.join("a.jsonl");
        std::fs::write(
            &ta,
            [
                row("user", "ship the asks view", "2026-11-06T10:00:00Z"),
                assistant_row("Shipped; the PR is open.", "2026-10-27T00:00:01Z"),
            ]
            .join("\n")
                + "\n",
        )
        .unwrap();
        // Session B: a pending ask with the older stamp; it still heads the
        // list.
        let tb = dir.join("b.jsonl");
        std::fs::write(
            &tb,
            row("user", "status on the gate?", "2026-11-06T09:00:00Z") + "\n",
        )
        .unwrap();
        for (sid, path) in [
            ("sess-a", ta.display().to_string()),
            ("sess-b", tb.display().to_string()),
        ] {
            std::fs::write(
                capture.join(format!("{sid}.scan.json")),
                json!({"transcript": path, "offset": 0}).to_string(),
            )
            .unwrap();
        }
        // The window reads mtimes; fresh temp files qualify.
        let registry: Vec<Value> = Vec::new();
        let inputs = YouInputs {
            registry: &registry,
            projects_dir: dir.join("projects"),
            home_root: dir.join("home"),
            cache_path: dir.join("you-cache.json"),
            capture_dir: capture.clone(),
        };
        let rows = asks_rows(&inputs);
        assert_eq!(rows.len(), 2, "one ask per session: {rows:?}");
        let pending = rows.first().expect("two rows");
        assert_eq!(pending["state"], "pending");
        assert_eq!(pending["session_id"], "sess-b");
        let answered = rows.get(1).expect("two rows");
        assert_eq!(answered["state"], "answered");
        assert_eq!(answered["session_id"], "sess-a");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
