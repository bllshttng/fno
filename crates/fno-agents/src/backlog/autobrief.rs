//! The dispatch brief resolver: the auto-brief rung chain the advance-layer
//! dispatch call sites read in place of a bare dispatch_brief field.
//!
//! Ports `fno/provenance/autobrief.py` and the transcript-store resolution
//! its tail rung reads (`fno/provenance/resolver.py`):
//!
//! 1. `node.dispatch_brief` - superuser-authored, verbatim (the over-budget
//!    refusal is the resolver's downstream, so unclamped here);
//! 2. the sidecar brief (`has_brief` + `briefs/{id}.md`), clamped;
//! 3. mechanical synthesis - an `<fno_spawn>` envelope + node details +,
//!    when details are thin, the tail of the source conversation near the
//!    node's creation. Assembled, never summarized: no LLM at dispatch;
//! 4. nothing - the spawn proceeds brief-less, the tag makes it visible.
//!
//! The whole feature is best-effort: every failure degrades DOWN the chain.

use serde_json::Value;
use std::path::{Path, PathBuf};

/// Auto rungs clamp to the env budget on a UTF-8 byte boundary; the explicit
/// rung is left to the resolver's fail-closed gate.
const BRIEF_MAX_BYTES: usize = 8192;
/// Details get first claim on the budget; the transcript tail fills the rest.
const DETAILS_MAX_BYTES: usize = 6144;
/// Rough envelope-header reservation when estimating tail headroom.
const HEADER_ALLOWANCE_BYTES: usize = 512;
/// Below this headroom a tail fragment is not worth shipping.
const MIN_TAIL_HEADROOM: usize = 512;
/// Details shorter than this pull the transcript tail.
const THIN_DETAILS_BYTES: usize = 400;
/// The conversation that birthed the node sits at-or-before created_at + this.
const CREATED_AT_WINDOW_SECS: f64 = 120.0;
/// Records read off the tail of a transcript / rows off the opencode store.
const TAIL_RECORDS_READ: usize = 200;
/// Conversational turns kept after windowing.
const TAIL_PAIRS_KEEP: usize = 12;
/// Per-turn clamp: one huge message cannot swallow the tail budget.
const PER_LINE_MAX_BYTES: usize = 600;
const TRUNC_MARKER: &str = "\n[truncated]";

/// The resolved brief: the text (None = proceed brief-less) and the source
/// tag (`explicit | sidecar | synth-details | synth-details+tail |
/// synth-tail | none`).
#[derive(Debug, Clone)]
pub struct DispatchBrief {
    pub text: Option<String>,
    pub tag: String,
}

/// Clamp to `limit` UTF-8 bytes on a codepoint boundary, appending the
/// truncation marker. A multibyte char is never split.
fn clamp_bytes(s: &str, limit: usize) -> String {
    let b = s.as_bytes();
    if b.len() <= limit {
        return s.to_string();
    }
    let marker = TRUNC_MARKER.as_bytes().len();
    let keep = limit.saturating_sub(marker);
    let mut end = keep.min(b.len());
    while end > 0 && std::str::from_utf8(&b[..end]).is_err() {
        end -= 1;
    }
    format!("{}{TRUNC_MARKER}", String::from_utf8_lossy(&b[..end]))
}

fn text_field(node: &Value, key: &str) -> Option<String> {
    node.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The briefs dir: the config override, else the state root's `briefs/`.
pub(crate) fn briefs_dir() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    if let Some(over) = crate::agents_config::config_lookup(&cwd, &["paths", "briefs_dir"])
        .and_then(|v| v.as_str().map(str::to_string))
        .filter(|s| !s.trim().is_empty())
    {
        return Some(PathBuf::from(over));
    }
    crate::backlog::settings::state_dir().map(|d| d.join("briefs"))
}

/// The sidecar brief: `briefs/{id}.md` when `has_brief`.
fn read_sidecar(node: &Value) -> Option<String> {
    if !node
        .get("has_brief")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let nid = node.get("id").and_then(Value::as_str)?;
    let path = briefs_dir()?.join(format!("{nid}.md"));
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn details_text(node: &Value) -> String {
    for key in ["details", "description"] {
        if let Some(v) = text_field(node, key) {
            return v;
        }
    }
    String::new()
}

/// Keep an attribute value on one line and inside its quotes.
fn attr(v: &str) -> String {
    v.replace('"', "'")
        .replace('\n', " ")
        .replace('\r', " ")
        .trim()
        .to_string()
}

fn node_label(node: &Value) -> String {
    let Some(nid) = text_field(node, "id") else {
        return String::new();
    };
    match text_field(node, "slug") {
        Some(slug) => format!("{nid} {slug}"),
        None => nid,
    }
}

/// The reply-to handle: the live dispatching session (a team lead dispatching
/// its wave outranks the historical filer), else the node's provenance.
fn initiator_handle(node: &Value) -> Option<String> {
    if let Some(live) = std::env::var("FNO_AGENT_SELF")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        return Some(live);
    }
    text_field(node, "source_session_id")
}

/// The `<fno_spawn>` envelope header + body. Attributes degrade by omission
/// when provenance is missing; they are never fabricated.
fn envelope(node: &Value, tag: &str, details_block: &str, tail_block: &str) -> String {
    let mut attrs: Vec<String> = Vec::new();
    let handle = initiator_handle(node);
    if let Some(h) = &handle {
        attrs.push(format!("from=\"{}\"", attr(h)));
    }
    if let Some(harness) = text_field(node, "source_harness") {
        attrs.push(format!("harness=\"{}\"", attr(&harness)));
    }
    let label = node_label(node);
    if !label.is_empty() {
        attrs.push(format!("node=\"{}\"", attr(&label)));
    }
    attrs.push(format!("source=\"{tag}\""));

    let mut body = vec![format!("<fno_spawn {}>", attrs.join(" "))];
    if let Some(h) = &handle {
        body.push(format!("reply: fno agents mail send {h}"));
    }
    if let Some(title) = text_field(node, "title") {
        body.push(format!("title: {title}"));
    }
    if let Some(reading) = node.get("_reading").and_then(Value::as_str) {
        if !reading.is_empty() {
            body.push(reading.to_string());
        }
    }
    if !details_block.is_empty() {
        body.push("details:".to_string());
        body.push(details_block.to_string());
    }
    if !tail_block.is_empty() {
        body.push(tail_block.to_string());
    }
    body.push("</fno_spawn>".to_string());
    body.join("\n")
}

/// Mechanical synthesis: details first, the transcript tail when they are
/// thin enough to leave headroom.
fn synthesize(node: &Value) -> (Option<String>, String) {
    let details = details_text(node);
    let details_block = if details.is_empty() {
        String::new()
    } else {
        clamp_bytes(&details, DETAILS_MAX_BYTES)
    };

    let mut tail_block = String::new();
    if details.as_bytes().len() < THIN_DETAILS_BYTES {
        let used = details_block.as_bytes().len() + HEADER_ALLOWANCE_BYTES;
        let headroom = BRIEF_MAX_BYTES.saturating_sub(used);
        if headroom >= MIN_TAIL_HEADROOM {
            tail_block = transcript_tail_text(node, headroom);
        }
    }

    let have_details = !details_block.is_empty();
    let have_tail = !tail_block.is_empty();
    let tag = match (have_details, have_tail) {
        (true, true) => "synth-details+tail",
        (true, false) => "synth-details",
        (false, true) => "synth-tail",
        (false, false) => return (None, "none".to_string()),
    };
    (
        Some(clamp_bytes(
            &envelope(node, tag, &details_block, &tail_block),
            BRIEF_MAX_BYTES,
        )),
        tag.to_string(),
    )
}

/// Resolve the dispatch brief for the node: the first non-empty rung wins.
/// Never fails the caller: every read failure degrades down the chain.
pub fn resolve_dispatch_brief(node: &Value) -> DispatchBrief {
    // Rung 1: explicit dispatch_brief, verbatim (unclamped: the over-budget
    // error is the resolver gate's, not ours).
    if let Some(explicit) = text_field(node, "dispatch_brief") {
        return DispatchBrief {
            text: Some(explicit),
            tag: "explicit".to_string(),
        };
    }
    // Rung 2: the sidecar brief.
    if let Some(sidecar) = read_sidecar(node) {
        return DispatchBrief {
            text: Some(clamp_bytes(&sidecar, BRIEF_MAX_BYTES)),
            tag: "sidecar".to_string(),
        };
    }
    // Rung 3: mechanical synthesis; Rung 4: nothing.
    let (brief, tag) = synthesize(node);
    DispatchBrief { text: brief, tag }
}

// ---------------------------------------------------------------------------
// The tail rung
// ---------------------------------------------------------------------------

type Pair = (String, String, Option<f64>);

/// The formatted transcript tail, byte-clamped to the headroom. Ambiguous
/// resolution and unreadable stores are a miss: wrong-session context is
/// worse than none.
fn transcript_tail_text(node: &Value, headroom: usize) -> String {
    let Some(harness) = text_field(node, "source_harness") else {
        return String::new();
    };
    let Some(sid) = text_field(node, "source_session_id") else {
        return String::new();
    };
    let cwd = text_field(node, "source_cwd").unwrap_or_default();
    let rt = crate::transcript_resolve::resolve_transcript(
        &harness,
        &sid,
        &cwd,
        &crate::transcript_resolve::Roots::default(),
    );
    if !rt.resolved || rt.ambiguous {
        return String::new();
    }
    let Some(path) = rt.path else {
        return String::new();
    };
    let mut pairs = match rt.kind {
        "opencode-db" => opencode_pairs(&path, &sid),
        _ => {
            if harness == "codex" {
                codex_pairs(&path)
            } else {
                claude_pairs(&path)
            }
        }
    };
    if pairs.is_empty() {
        return String::new();
    }
    // One batched classify per read: delivered mail never counts as a pair.
    pairs.retain(|(_, text, _)| {
        matches!(
            crate::mail_header::classify(text),
            crate::mail_header::Framing::Bare
        )
    });
    let cutoff = node
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    if let Some(cutoff) = cutoff {
        if pairs.iter().any(|(_, _, ts)| ts.is_some()) {
            // Timestamped records but every one after the cutoff: the tail is
            // OMITTED rather than injecting a later conversation.
            let cutoff = cutoff + CREATED_AT_WINDOW_SECS;
            pairs.retain(|(_, _, ts)| ts.is_some_and(|t| t <= cutoff));
        }
    }
    if pairs.len() > TAIL_PAIRS_KEEP {
        pairs = pairs.split_off(pairs.len() - TAIL_PAIRS_KEEP);
    }
    if pairs.is_empty() {
        return String::new();
    }
    let short: String = sid.chars().take(8).collect();
    let mut lines = vec![format!(
        "--- source-transcript tail ({harness} {short}, near created_at) ---"
    )];
    for (role, text, _) in &pairs {
        let one = text.split_whitespace().collect::<Vec<&str>>().join(" ");
        lines.push(format!("{role}: {}", clamp_bytes(&one, PER_LINE_MAX_BYTES)));
    }
    clamp_bytes(&lines.join("\n"), headroom)
}

fn last_lines(path: &Path, count: usize) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

fn text_from_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.trim().to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        let btype = block.get("type").and_then(Value::as_str).unwrap_or("");
        if !btype.ends_with("text") {
            // text / input_text / output_text; tool_use, reasoning and the
            // rest are not conversational.
            continue;
        }
        if let Some(t) = block.get("text").and_then(Value::as_str) {
            let t = t.trim();
            if !t.is_empty() {
                parts.push(t.to_string());
            }
        }
    }
    parts.join("\n")
}

fn claude_pairs(path: &Path) -> Vec<Pair> {
    let mut pairs: Vec<Pair> = Vec::new();
    for line in last_lines(path, TAIL_RECORDS_READ) {
        let Ok(rec) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if !matches!(
            rec.get("type").and_then(Value::as_str),
            Some("user") | Some("assistant")
        ) {
            continue;
        }
        let Some(msg) = rec.get("message") else {
            continue;
        };
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = text_from_content(msg.get("content"));
        if !text.is_empty() {
            let ts = rec
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts);
            pairs.push((role.to_string(), text, ts));
        }
    }
    pairs
}

fn codex_pairs(path: &Path) -> Vec<Pair> {
    let mut pairs: Vec<Pair> = Vec::new();
    for line in last_lines(path, TAIL_RECORDS_READ) {
        let Ok(rec) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if rec.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = rec.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(Value::as_str) != Some("message") {
            continue;
        }
        let role = payload.get("role").and_then(Value::as_str).unwrap_or("");
        if role != "user" && role != "assistant" {
            continue;
        }
        let text = text_from_content(payload.get("content"));
        if !text.is_empty() {
            let ts = rec
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(parse_ts);
            pairs.push((role.to_string(), text, ts));
        }
    }
    pairs
}

fn opencode_pairs(db_path: &Path, session_id: &str) -> Vec<Pair> {
    let Some(conn) = crate::graph_store::open_read_only_wal_safe(db_path) else {
        return Vec::new();
    };
    let mut stmt = match conn.prepare(
        "SELECT m.time_created, json_extract(m.data, '$.role'), \
         json_extract(p.data, '$.text') FROM message m \
         JOIN part p ON p.message_id = m.id \
         WHERE m.session_id = ?1 AND json_extract(p.data, '$.type') = 'text' \
         ORDER BY m.time_created DESC, p.time_created DESC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let rows = stmt.query_map([session_id, &format!("{TAIL_RECORDS_READ}")], |row| {
        Ok((
            row.get::<_, Option<rusqlite::types::Value>>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    });
    let mut pairs: Vec<Pair> = Vec::new();
    if let Ok(rows) = rows {
        for row in rows.flatten() {
            let (ts_raw, role, text) = row;
            let Some(role) = role else { continue };
            if role != "user" && role != "assistant" {
                continue;
            }
            let Some(text) = text.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) else {
                continue;
            };
            let ts = match ts_raw {
                Some(rusqlite::types::Value::Integer(n)) => Some(n as f64 / 1000.0),
                Some(rusqlite::types::Value::Real(f)) => Some(f / 1000.0),
                _ => None,
            };
            pairs.push((role, text, ts));
        }
    }
    // The store returns newest-first; restore chronological order.
    pairs.reverse();
    pairs
}

/// Epoch seconds from an ISO-8601 stamp or a numeric epoch; None otherwise.
fn parse_ts(value: &str) -> Option<f64> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    if let Ok(n) = v.parse::<f64>() {
        return Some(n);
    }
    crate::event_store::parse_rfc3339_ms(v).map(|ms| ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Redirect every store the resolver reads into a temp root and restore
    /// the process env on drop, mirroring the birth.rs guard.
    struct StoreGuard {
        home: PathBuf,
        state: PathBuf,
    }

    impl StoreGuard {
        fn new() -> StoreGuard {
            let tmp = TempDir::new().expect("temp dir");
            let home = tmp.path().join("home");
            let state = tmp.path().join("state");
            fs::create_dir_all(home.join(".claude").join("projects")).expect("projects root");
            fs::create_dir_all(&state).expect("state root");
            std::env::set_var("HOME", &home);
            std::env::set_var("FNO_STATE_DIR", &state);
            std::env::remove_var("FNO_CONFIG");
            std::env::remove_var("FNO_AGENT_SELF");
            std::env::remove_var("FNO_CODEX_SESSIONS_DIR");
            std::env::remove_var("CODEX_HOME");
            std::env::remove_var("FNO_OPENCODE_STORAGE_DIR");
            StoreGuard { home, state }
        }

        fn write_brief(&self, node_id: &str, body: &str) {
            let dir = self.state.join("briefs");
            fs::create_dir_all(&dir).expect("briefs dir");
            fs::write(dir.join(format!("{node_id}.md")), body).expect("brief");
        }

        fn write_claude_transcript(&self, sid: &str, records: &[Value]) {
            self.write_claude_transcript_named(sid, records)
        }

        fn write_claude_transcript_named(&self, stem: &str, records: &[Value]) {
            let proj = self
                .home
                .join(".claude")
                .join("projects")
                .join("-tmp-workspace");
            fs::create_dir_all(&proj).expect("project dir");
            let lines: Vec<String> = records
                .iter()
                .map(|r| serde_json::to_string(r).expect("record"))
                .collect();
            fs::write(proj.join(format!("{stem}.jsonl")), lines.join("\n")).expect("transcript");
        }
    }

    impl Drop for StoreGuard {
        fn drop(&mut self) {
            // TempDir removes the tree; only the env vars need restoring.
            std::env::remove_var("FNO_STATE_DIR");
        }
    }

    fn node(fields: &[(&str, Value)]) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("id".into(), Value::from("x-1"));
        for (k, v) in fields {
            m.insert((*k).into(), v.clone());
        }
        Value::Object(m)
    }

    fn claude_turn(role: &str, text: &str, ts: &str) -> Value {
        serde_json::json!({
            "type": role,
            "timestamp": ts,
            "message": {"role": role, "content": [{"type": "text", "text": text}]}
        })
    }

    #[test]
    fn explicit_rung_wins_verbatim_and_unclamped() {
        let _g = StoreGuard::new();
        let big = "路".repeat(4000); // 12000 bytes, over the 8192 budget
        let brief = resolve_dispatch_brief(&node(&[("dispatch_brief", Value::from(big.clone()))]));
        assert_eq!(brief.tag, "explicit");
        assert_eq!(brief.text.as_deref(), Some(big.as_str()));
    }

    #[test]
    fn sidecar_rides_and_clamps_on_a_codepoint_boundary() {
        let g = StoreGuard::new();
        let big = "γ".repeat(6000); // 12000 bytes
        g.write_brief("x-1", &big);
        let brief = resolve_dispatch_brief(&node(&[
            ("has_brief", Value::from(true)),
            (
                "details",
                Value::from("unused: the sidecar outranks synthesis"),
            ),
        ]));
        assert_eq!(brief.tag, "sidecar");
        let text = brief.text.expect("sidecar text");
        assert!(text.ends_with(TRUNC_MARKER));
        assert!(text.as_bytes().len() <= BRIEF_MAX_BYTES);
        // The last kept codepoint is whole, never a split multibyte char.
        text.chars().next_back().expect("non-empty");
    }

    #[test]
    fn synth_envelope_names_provenance_and_never_fabricates() {
        let g = StoreGuard::new();
        let brief = resolve_dispatch_brief(&node(&[
            ("title", Value::from("Ship the thing")),
            ("details", Value::from("short details")),
            ("source_harness", Value::from("claude")),
            ("source_session_id", Value::from("sess-abcd1234")),
            ("source_cwd", Value::from("/tmp/wk")),
            ("slug", Value::from("ship-the-thing")),
        ]));
        assert_eq!(brief.tag, "synth-details");
        let text = brief.text.expect("synth text");
        assert!(text.starts_with("<fno_spawn "));
        assert!(text.contains("from=\"sess-abcd1234\""));
        assert!(text.contains("harness=\"claude\""));
        assert!(text.contains("node=\"x-1 ship-the-thing\""));
        assert!(text.contains("source=\"synth-details\""));
        assert!(text.contains("title: Ship the thing"));
        assert!(text.contains("details:\nshort details"));
        assert!(text.ends_with("</fno_spawn>"));
        // The live dispatcher handle outranks the historical filer.
        std::env::set_var("FNO_AGENT_SELF", "live-lead-99");
        let live = resolve_dispatch_brief(&node(&[
            ("details", Value::from("short details")),
            ("source_session_id", Value::from("sess-abcd1234")),
        ]))
        .text
        .expect("live synth text");
        assert!(live.contains("from=\"live-lead-99\""));
        std::env::remove_var("FNO_AGENT_SELF");
        drop(g);
    }

    #[test]
    fn window_keeps_only_records_near_created_at() {
        let g = StoreGuard::new();
        g.write_claude_transcript(
            "11111111-2222-3333-4444-555555555555",
            &[
                claude_turn("user", "birth the node", "2026-01-01T00:00:00Z"),
                claude_turn("assistant", "node born", "2026-01-01T00:01:30Z"),
                claude_turn("user", "much later chatter", "2026-01-01T00:10:00Z"),
            ],
        );
        let brief = resolve_dispatch_brief(&node(&[
            ("details", Value::from("thin")),
            ("source_harness", Value::from("claude")),
            (
                "source_session_id",
                Value::from("11111111-2222-3333-4444-555555555555"),
            ),
            ("source_cwd", Value::from("/tmp/wk")),
            ("created_at", Value::from("2026-01-01T00:00:20Z")),
        ]));
        assert_eq!(brief.tag, "synth-details+tail");
        let text = brief.text.expect("tail text");
        assert!(text.contains("birth the node"));
        assert!(text.contains("node born"));
        assert!(!text.contains("much later chatter"));
    }
}
