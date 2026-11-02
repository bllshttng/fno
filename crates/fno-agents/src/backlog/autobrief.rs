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
// Transcript-store resolution (the resolver port)
// ---------------------------------------------------------------------------

/// The transcript-store answer for one provenance pointer.
struct ResolvedTranscript {
    resolved: bool,
    path: Option<PathBuf>,
    kind: String,
    ambiguous: bool,
}

fn unresolved() -> ResolvedTranscript {
    ResolvedTranscript {
        resolved: false,
        path: None,
        kind: String::new(),
        ambiguous: false,
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// The codex rollout store: the env override, `$CODEX_HOME/sessions`, else
/// `~/.codex/sessions`.
fn codex_sessions_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_CODEX_SESSIONS_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v));
    }
    if let Some(home) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("sessions"));
    }
    home().map(|h| h.join(".codex").join("sessions"))
}

/// The opencode SQLite store, the sibling of the legacy storage tree.
fn opencode_db_path() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_OPENCODE_STORAGE_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(v).parent()?.join("opencode.db"));
    }
    home().map(|h| {
        h.join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db")
    })
}

/// True iff the transcript holds at least one user/assistant turn: a real
/// transcript, not the metadata-only stub a worktree re-key leaves behind.
fn claude_has_conversation(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    for line in text.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if matches!(
            rec.get("type").and_then(Value::as_str),
            Some("user") | Some("assistant")
        ) {
            return true;
        }
    }
    false
}

/// Resolve a provenance pointer to its on-disk transcript. resolved=true only
/// when an actual store entry was found; ambiguous is a miss for the tail rung
/// (wrong-session context is worse than none).
fn resolve_transcript(harness: &str, session_id: &str, cwd: &str) -> ResolvedTranscript {
    if session_id.is_empty() {
        return unresolved();
    }
    if harness == "codex" {
        return resolve_codex(session_id, cwd);
    }
    if harness == "opencode" {
        return resolve_opencode(session_id, cwd);
    }
    if harness != "claude" || cwd.is_empty() {
        return unresolved();
    }
    resolve_claude(session_id, cwd)
}

fn resolve_codex(session_id: &str, cwd: &str) -> ResolvedTranscript {
    let Some(root) = codex_sessions_dir() else {
        return unresolved();
    };
    let mut rollouts: Vec<PathBuf> = Vec::new();
    collect_files(&root, "rollout-", ".jsonl", &mut rollouts);
    // Fast path: codex embeds the session uuid in the rollout filename.
    for path in &rollouts {
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.contains(session_id) {
                return ResolvedTranscript {
                    resolved: true,
                    path: Some(path.clone()),
                    kind: "jsonl".to_string(),
                    ambiguous: false,
                };
            }
        }
    }
    // Fallback: a rollout named by a turn id carries the session id in its
    // first session_meta line; scan newest-first.
    rollouts.sort_by_key(|p| {
        std::cmp::Reverse(
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    });
    for path in &rollouts {
        if codex_meta_session(path).as_deref() == Some(session_id) {
            return ResolvedTranscript {
                resolved: true,
                path: Some(path.clone()),
                kind: "jsonl".to_string(),
                ambiguous: false,
            };
        }
    }
    let _ = cwd;
    unresolved()
}

fn collect_files(root: &Path, prefix: &str, suffix: &str, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_files(&p, prefix, suffix, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
            if name.starts_with(prefix) && name.ends_with(suffix) {
                out.push(p);
            }
        }
    }
}

/// The session id the rollout's first session_meta line carries.
fn codex_meta_session(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().take(4) {
        let Ok(rec) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        if rec.get("type").and_then(Value::as_str) == Some("session_meta") {
            return text_field(&rec, "id");
        }
    }
    None
}

fn resolve_opencode(session_id: &str, cwd: &str) -> ResolvedTranscript {
    let Some(store) = opencode_db_path().filter(|p| p.exists()) else {
        return unresolved();
    };
    let ok = crate::graph_store::open_read_only_wal_safe(&store)
        .ok_or_else(|| rusqlite::Error::InvalidPath(store.clone()))
        .and_then(|conn| {
            let mut stmt = conn.prepare("SELECT 1 FROM session WHERE id = ?1 LIMIT 1")?;
            let mut rows = stmt.query([session_id])?;
            Ok(rows.next()?.is_some())
        });
    match ok {
        Ok(true) => ResolvedTranscript {
            resolved: true,
            path: Some(store),
            kind: "opencode-db".to_string(),
            ambiguous: false,
        },
        _ => {
            let _ = cwd;
            unresolved()
        }
    }
}

/// Claude resolution searches EVERY project dir: a session's transcript can
/// exist in more than one (a worktree re-key leaves a stub in the other).
/// cwd stays required but no longer scopes the search.
fn resolve_claude(session_id: &str, _cwd: &str) -> ResolvedTranscript {
    let Some(root) = home().map(|h| h.join(".claude").join("projects")) else {
        return unresolved();
    };
    let mut matches: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(&dir) {
                for f in files.flatten() {
                    let p = f.path();
                    let Some(name) = p.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let Some(stem) = name.strip_suffix(".jsonl") else {
                        continue;
                    };
                    // A stem carrying a dot is a sibling artifact, never a
                    // transcript; dropping it keeps a full uuid from matching
                    // its own artifacts and reading as ambiguous.
                    if !stem.contains('.') && stem.starts_with(session_id) {
                        matches.push(p);
                    }
                }
            }
        }
    }
    matches.sort();
    if matches.is_empty() {
        return unresolved();
    }
    let stems: std::collections::BTreeSet<&str> = matches
        .iter()
        .filter_map(|p| p.file_name().and_then(|n| n.to_str()))
        .collect();
    if stems.len() > 1 {
        // A short prefix matched two DISTINCT session uuids: genuinely
        // ambiguous. First-sorted + ambiguous, never a guess.
        return ResolvedTranscript {
            resolved: true,
            path: matches.first().cloned(),
            kind: "jsonl".to_string(),
            ambiguous: true,
        };
    }
    if matches.len() == 1 {
        return ResolvedTranscript {
            resolved: true,
            path: matches.into_iter().next(),
            kind: "jsonl".to_string(),
            ambiguous: false,
        };
    }
    // Same session copied across canonical + worktree dirs: prefer the copies
    // that actually carry conversation, then the newest write.
    let mut with_convo: Vec<PathBuf> = matches
        .iter()
        .filter(|p| claude_has_conversation(p))
        .cloned()
        .collect();
    if with_convo.is_empty() {
        with_convo = matches;
    }
    with_convo.sort_by_key(|p| {
        std::cmp::Reverse(
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0),
        )
    });
    ResolvedTranscript {
        resolved: true,
        path: with_convo.into_iter().next(),
        kind: "jsonl".to_string(),
        ambiguous: false,
    }
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
    let rt = resolve_transcript(&harness, &sid, &cwd);
    if !rt.resolved || rt.ambiguous {
        return String::new();
    }
    let Some(path) = rt.path else {
        return String::new();
    };
    let mut pairs = match rt.kind.as_str() {
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
    fn sidecar_file_missing_falls_through_to_synthesis() {
        let _g = StoreGuard::new();
        let brief = resolve_dispatch_brief(&node(&[
            ("has_brief", Value::from(true)),
            ("details", Value::from("the only material present")),
            ("source_harness", Value::from("claude")),
            ("source_session_id", Value::from("sess-abcd1234")),
            ("source_cwd", Value::from("/tmp/wk")),
        ]));
        assert_eq!(brief.tag, "synth-details");
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
    fn synth_none_without_material() {
        let _g = StoreGuard::new();
        let brief = resolve_dispatch_brief(&node(&[("title", Value::from("bare node"))]));
        assert_eq!(brief.tag, "none");
        assert_eq!(brief.text, None);
    }

    #[test]
    fn clamp_bytes_never_splits_a_multibyte_char() {
        let s = "γ".repeat(30); // 60 bytes
        let out = clamp_bytes(&s, 30);
        assert!(out.as_bytes().len() <= 30);
        assert!(out.ends_with(TRUNC_MARKER));
        // Decoding the kept head must not yield U+FFFD: the boundary is whole.
        assert!(!out.contains('\u{FFFD}'));
        assert_eq!(clamp_bytes("short", 100), "short");
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

    #[test]
    fn fully_windowed_out_tail_is_omitted_not_injected() {
        let g = StoreGuard::new();
        g.write_claude_transcript(
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            &[claude_turn(
                "user",
                "later conversation",
                "2026-06-01T00:00:00Z",
            )],
        );
        let brief = resolve_dispatch_brief(&node(&[
            ("details", Value::from("thin")),
            ("source_harness", Value::from("claude")),
            (
                "source_session_id",
                Value::from("aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"),
            ),
            ("source_cwd", Value::from("/tmp/wk")),
            ("created_at", Value::from("2026-01-01T00:00:00Z")),
        ]));
        // Timestamped records all after the cutoff: the tail is OMITTED.
        assert_eq!(brief.tag, "synth-details");
        let text = brief.text.expect("details-only text");
        assert!(!text.contains("later conversation"));
    }

    #[test]
    fn ambiguous_claude_resolution_skips_tail() {
        let g = StoreGuard::new();
        // Two DISTINCT stems matching the short prefix read as ambiguous.
        g.write_claude_transcript_named(
            "11111111aaaa",
            &[claude_turn("user", "one session", "2026-01-01T00:00:00Z")],
        );
        g.write_claude_transcript_named(
            "11111111bbbb",
            &[claude_turn("user", "other session", "2026-01-01T00:00:00Z")],
        );
        let brief = resolve_dispatch_brief(&node(&[
            ("details", Value::from("thin")),
            ("source_harness", Value::from("claude")),
            ("source_session_id", Value::from("11111111")),
            ("source_cwd", Value::from("/tmp/wk")),
        ]));
        assert_eq!(brief.tag, "synth-details");
    }

    #[test]
    fn codex_rollout_tail_read() {
        let g = StoreGuard::new();
        let root = g.home.join("codex-sessions");
        std::fs::create_dir_all(&root).expect("codex root");
        std::env::set_var("FNO_CODEX_SESSIONS_DIR", &root);
        let rollout = serde_json::json!({
            "type": "response_item",
            "timestamp": "2026-01-01T00:00:00Z",
            "payload": {"type": "message", "role": "user",
                        "content": [{"type": "input_text", "text": "codex turn"}]}
        });
        let sid = "11111111-2222-3333-4444-555555555555";
        let path = root.join(format!("rollout-{sid}.jsonl"));
        std::fs::write(&path, serde_json::to_string(&rollout).expect("record")).expect("rollout");
        let brief = resolve_dispatch_brief(&node(&[
            ("details", Value::from("thin")),
            ("source_harness", Value::from("codex")),
            ("source_session_id", Value::from(sid)),
            ("source_cwd", Value::from("/tmp/wk")),
        ]));
        assert_eq!(brief.tag, "synth-details+tail");
        let text = brief.text.expect("codex tail text");
        assert!(text.contains("codex turn"));
        std::env::remove_var("FNO_CODEX_SESSIONS_DIR");
        drop(g);
    }

    #[test]
    fn mail_framed_turns_omitted_from_tail() {
        let g = StoreGuard::new();
        g.write_claude_transcript_named(
            "cccccccc-dddd-eeee-ffff-000000000000",
            &[claude_turn(
                "user",
                "<fno_mail from=\"peer\" id=\"m1\">relay body</fno_mail>",
            )],
        );
        let brief = resolve_dispatch_brief(&node(&[
            ("details", Value::from("thin")),
            ("source_harness", Value::from("claude")),
            (
                "source_session_id",
                Value::from("cccccccc-dddd-eeee-ffff-000000000000"),
            ),
            ("source_cwd", Value::from("/tmp/wk")),
        ]));
        // The relay shape is not a conversational pair; the tail is empty.
        assert_eq!(brief.tag, "synth-details");
    }
}
