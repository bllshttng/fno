//! The You-thread splitter: the operator's typed turns and the assistant's
//! replies to them, read from one claude transcript and classified by the
//! same provenance rules `fno-agents intel` folds with. This module is a
//! consumer of [`crate::provenance`], never a second classifier: a rule
//! change lands there once, and both readers move together (node x-cecf).
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
use serde_json::Value;
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
}

/// Where a walk stopped: the trailing item's resume state. `item_start` is
/// the byte offset of the typed row that opened it, `consumed` the offset
/// through the last complete row (a partial trailing line is re-read next
/// time). Serialized into the cache entry, so field names are contract. The
/// reply is deliberately not carried: a resume re-reads from `item_start`
/// and rebuilds the reply from the same rows, so carrying it would double.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub(crate) struct WalkTail {
    pub item_start: u64,
    pub consumed: u64,
    pub ts: String,
    pub typed_text: String,
}

/// One walk's output: complete items plus the trailing item's resume state.
pub(crate) struct WalkOut {
    pub items: Vec<YouItem>,
    pub tail: Option<WalkTail>,
    /// Bytes consumed through the last complete row, absolute.
    pub consumed: u64,
}

/// Walk one transcript's text into You items. `base` is the absolute offset
/// `raw` starts at; `resume`, when set, re-opens the trailing item the tail
/// describes - the text then begins at that item's typed row, which the walk
/// re-reads as the same opener while the reply rebuilds from the same rows.
pub(crate) fn walk(
    raw: &str,
    base: u64,
    bus: &BusIndex,
    session: &str,
    resume: Option<WalkTail>,
) -> WalkOut {
    let mut items: Vec<YouItem> = Vec::new();
    let mut open: Option<(String, String)> = None;
    let mut reply: Vec<String> = Vec::new();
    let mut item_start: Option<u64> = None;
    let mut headless = false;
    let mut first_user_seen = false;
    let mut resuming = resume.is_some();
    if let Some(tail) = resume {
        // The reply is NOT seeded: the re-read from item_start rebuilds it
        // from the same rows, and seeding would double every block.
        open = Some((tail.ts, tail.typed_text));
        item_start = Some(tail.item_start);
    }

    let close = |open: &mut Option<(String, String)>,
                 reply: &mut Vec<String>,
                 item_start: &mut Option<u64>,
                 items: &mut Vec<YouItem>| {
        if let Some((ts, typed_text)) = open.take() {
            items.push(YouItem {
                ts,
                typed_text,
                reply_text: reply.join("\n\n"),
            });
        }
        reply.clear();
        *item_start = None;
    };
    // A typed source arrives: push the previous item (or, on a resume, the
    // opener re-reads its own row and the reply rebuilds from scratch), then
    // open the new one.
    let open_new = |open: &mut Option<(String, String)>,
                    reply: &mut Vec<String>,
                    item_start: &mut Option<u64>,
                    items: &mut Vec<YouItem>,
                    resuming: &mut bool,
                    ts: String,
                    text: String,
                    line_start: u64| {
        if *resuming {
            *resuming = false;
        } else if let Some((ts, typed_text)) = open.take() {
            items.push(YouItem {
                ts,
                typed_text,
                reply_text: reply.join("\n\n"),
            });
        }
        reply.clear();
        *open = Some((ts, text));
        *item_start = Some(line_start);
    };

    let bytes = raw.as_bytes();
    let mut pos: usize = 0;
    while pos < bytes.len() {
        let line_start = base + pos as u64;
        let Some(nl) = bytes[pos..].iter().position(|&b| b == b'\n') else {
            break; // a partial trailing line is re-read on the next advance
        };
        let line = &raw[pos..pos + nl];
        pos += nl + 1;
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
                    // Hook context and every other attachment is machine
                    // shape: it closes the span like any injected row.
                    close(&mut open, &mut reply, &mut item_start, &mut items);
                    continue;
                }
                let prompt = att.get("prompt").and_then(Value::as_str).unwrap_or("");
                match classify_text(prompt, Some((bus, session))) {
                    // The typed shape intel's corpus trusts. A bus-joined
                    // prompt is relay the person pasted into the pane; it
                    // closes the span like any machine row.
                    crate::provenance::Verdict::Operator(_) => {}
                    _ => {
                        close(&mut open, &mut reply, &mut item_start, &mut items);
                        continue;
                    }
                }
                open_new(
                    &mut open,
                    &mut reply,
                    &mut item_start,
                    &mut items,
                    &mut resuming,
                    turn_stamp(&obj).unwrap_or("").to_string(),
                    prompt.to_string(),
                    line_start,
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
                open_new(
                    &mut open,
                    &mut reply,
                    &mut item_start,
                    &mut items,
                    &mut resuming,
                    turn_stamp(&obj).unwrap_or("").to_string(),
                    text,
                    line_start,
                );
            }
            Some("assistant") => {
                if open.is_none() {
                    continue;
                }
                let text = turn_text(&obj);
                if !text.trim().is_empty() {
                    reply.push(text);
                }
            }
            _ => {}
        }
    }

    let consumed = base + pos as u64;
    let mut tail = None;
    if let Some((ts, typed_text)) = open {
        items.push(YouItem {
            reply_text: reply.join("\n\n"),
            ts: ts.clone(),
            typed_text: typed_text.clone(),
        });
        tail = Some(WalkTail {
            item_start: item_start.unwrap_or(base),
            consumed,
            ts,
            typed_text,
            reply,
        });
    }
    WalkOut {
        items,
        tail,
        consumed,
    }
}

/// The items only, oldest first: the trailing open item rides in as the
/// last element with the reply gathered so far.
pub(crate) fn split_you_items(raw: &str, bus: &BusIndex, session: &str) -> Vec<YouItem> {
    walk(raw, 0, bus, session, None).items
}

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
    head_sha: String,
    head_len: u64,
    items: Vec<YouItem>,
    /// The trailing item's resume state, `None` when the transcript ended on
    /// a machine row (the next advance reads only new bytes).
    open: Option<WalkTail>,
}

const YOU_CACHE_VERSION: u32 = 1;

/// The loaded cache plus its lock discipline. `save` is best effort: an
/// unwritable cache degrades the next projection to a full re-walk.
pub(crate) struct YouStore {
    path: PathBuf,
    cache: YouCache,
    /// Persisted on save; a contended lock runs advance read-only.
    persist: bool,
}

impl YouStore {
    /// The production cache path, beside intel's rollups.json. `None` when
    /// no agents home is declared (tests run without a cache at all).
    pub(crate) fn default_path() -> Option<PathBuf> {
        crate::paths::AgentsHome::from_env_opt()
            .map(|home| home.root().join("intel").join("you-threads.json"))
    }

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
            let start = e.open.as_ref().map(|t| t.item_start).unwrap_or(e.offset);
            match read_range(path, start, size) {
                Ok(text) => walk(&text, start, bus, session, e.open.clone()),
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
        Ok(text) => walk(&text, 0, bus, session, None),
        Err(_) => WalkOut {
            items: Vec::new(),
            tail: None,
            consumed: 0,
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
    /// reply (tool results keep the span open), a machine row that closes
    /// the span, and a second typed turn after it.
    #[test]
    fn typed_reply_machine_close_and_a_second_item() {
        let raw = [
            row("user", "why did the gate refuse?", "2026-10-27T00:00:00Z"),
            assistant_row("Because the base was stale.", "2026-10-27T00:00:01Z"),
            tool_result_row(),
            assistant_row("I merged main into the branch.", "2026-10-27T00:00:02.5Z"),
            meta_row(),
            assistant_row("Acked the hook.", "2026-10-27T00:00:05Z"),
            row("user", "ship it then", "2026-10-27T00:00:09Z"),
            assistant_row("Opening the PR.", "2026-10-27T00:00:10Z"),
        ]
        .join("\n");
        let items = split_you_items(&raw, &BusIndex::empty(), "sess");
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
    /// instead of opening; a hook attachment closes but never opens.
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
            assistant_row("Answering both.", "2026-10-27T00:00:06Z"),
            queued_row(bus_text, "2026-10-27T00:00:07Z"),
            assistant_row("Relay ack text.", "2026-10-27T00:00:08Z"),
        ]
        .join("\n");
        let items = split_you_items(&raw, &bus, "sess");
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
        let items = split_you_items(&raw, &BusIndex::empty(), "sess");
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
}
