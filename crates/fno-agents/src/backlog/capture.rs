//! `fno backlog capture` - the capture tier below idea nodes.
//!
//! Ported from cli/src/fno/backlog/capture.py: the inbox is a markdown
//! holding-pen of self-contained checkbox items, each with a stable fu-id.
//! Capture is a cheap markdown append, not a graph mutation; triage promotes
//! warranted items into real graph nodes. Struck lines are never deleted, so
//! autocorrect provenance survives. Items are addressed by fu-id, never by
//! line number (Obsidian edits shift lines).
//!
//! The legacy em-dash (U+2014) separator still parses on read; every writer
//! emits the hyphen (Phase 2 separator migration). The managed data file
//! intentionally keeps its inbox.md name and the event vocabulary keeps the
//! legacy inbox_* spellings readable.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::graph_store::BoundedLock;

const MAX_WHY_LEN: usize = 120;
const SOFT_CEILING: usize = 100;
const VALID_PRIORITIES: [&str; 4] = ["p0", "p1", "p2", "p3"];

/// MINTED fu-id grammar: 6 lowercase hex. The RECOGNITION grammar is broader
/// (hand-authored slug ids like fu-cwd339 parse on read) while minting stays
/// strictly 6-hex.
const FU_MINTED_RE: &str = r"fu-[0-9a-f]{6}";
const FU_TOKEN: &str = r"fu-[a-z0-9][a-z0-9-]*";
/// Item line: `- [<mark>] fu-token <sep> <title-with-optional-(pN)-suffix>`.
/// Accepts BOTH separators (legacy em-dash and the target hyphen).
const ITEM_RE: &str = r#"^- \[([ x\-])\]\s+(fu-[a-z0-9][a-z0-9-]*)\s+(?:\u{2014}|-)\s+(.*?)\s*$"#;
const PRIORITY_SUFFIX_RE: &str = r"\s*\((p[0-3])\)\s*$";
/// Mechanical scan trigger language (the deterministic half; the LLM judges).
const SCAN_RE: &str = r"(?i)deferred|skipped|won't fix now|wont fix now|follow-up|followup|\bp2\b|\bp3\b|ship.{0,3}later|\bTODO\b";

/// Managed shortcode line. The wheel carried a negative lookahead so the
/// liberal node arm never steals a MALFORMED sibling (e.g. a 7-hex cv-);
/// this crate pins regex 1.x, which rejects lookarounds, so the guard is the
/// post-match cv shape check in `managed_shortcode_match`.
const MANAGED_SHORTCODE_RE: &str = r#"^- \[([ x\-])\]\s+(fu-[a-z0-9][a-z0-9-]*|cv-[0-9a-f]{8}|[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8})\s+(?:\u{2014}|-)\s+(.*?)\s*$"#;
const CHECKBOX_LINE_RE: &str = r#"^- \[([ x\-])\]\s+(.*?)\s*$"#;
const ANY_PRIORITY_RE: &str = r"\((p[0-3])\)";
const SECTION_HEADING_RE: &str = r"^#{2,6}\s+(.*\S)\s*$";
const POST_MERGE_MARKER_RE: &str = r"^<!--\s*(post-merge:\S+?)\s*-->\s*$";
const NODE_ID_RE: &str = r"->\s*([a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8})";
const MEMORY_SLUG_RE: &str = r"^\s*\[\[.*\]\]\s*$";
const JC_DATE_RE: &str = r"\u{1F4C5}\s*(\d{4}-\d{2}-\d{2})";
const WHERE_SUBLINE_RE: &str = r"^\s+where:\s*(.*\S)\s*$";
const H1_RE: &str = r"^# \S";

const DIGEST_START: &str = "<!-- inbox-digest:start -->";
const DIGEST_END: &str = "<!-- inbox-digest:end -->";

const BUCKET_ORDER: [&str; 4] = ["your_actions", "followups", "carveouts", "filed_nodes"];

/// Errors the inbox write paths surface to the CLI layer.
enum CaptureError {
    /// An inbox write violated a field rule (exit code 2).
    Validation(String),
    /// A birth refusal (node-origin / operator standing) (exit code 1).
    Refusal(String),
    /// The inbox file lock could not be acquired (exit code 1).
    Lock(String),
}

impl CaptureError {
    fn validation(message: impl Into<String>) -> CaptureError {
        CaptureError::Validation(message.into())
    }
}

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("capture regex compiles")
}

// ---------------------------------------------------------------------------
// fu-id minting + parsing
// ---------------------------------------------------------------------------

/// Mint a fresh `fu-XXXXXX` id not present in `existing`. 16M id space;
/// collision is effectively impossible, but the retry removes a
/// silent-failure class (two items sharing an id would make promote/dismiss
/// ambiguous).
fn mint_fu_id(existing: &std::collections::BTreeSet<String>) -> Result<String, String> {
    for _ in 0..1000 {
        let mut bytes = [0u8; 3];
        getrandom::fill(&mut bytes).map_err(|e| format!("getrandom failed: {e}"))?;
        let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let candidate = format!("fu-{hex}");
        if !existing.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err("could not mint a unique fu-id after 1000 attempts".to_string())
}

fn status_by_mark(mark: &str) -> &'static str {
    match mark {
        "x" => "promoted",
        "-" => "dismissed",
        _ => "open",
    }
}

/// Split a trailing `(pN)` suffix off a title. Returns (title, priority).
fn split_priority(raw_title: &str) -> (String, Option<String>) {
    let re = compile(PRIORITY_SUFFIX_RE);
    match re.find(raw_title) {
        Some(m) => (
            raw_title[..m.start()].trim().to_string(),
            Some(m.as_str().trim().trim_matches(['(', ')']).to_string()),
        ),
        None => (raw_title.trim().to_string(), None),
    }
}

/// Parse inbox markdown into item objects
/// (`{"id","title","priority","status","line"}`). By default only `open`
/// (`[ ]`) items are returned; `include_struck` also surfaces
/// promoted/dismissed lines.
fn parse_items(text: &str, include_struck: bool) -> Vec<Value> {
    let item_re = compile(ITEM_RE);
    let mut items = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        let Some(m) = item_re.captures(line) else {
            continue;
        };
        let status = status_by_mark(&m[1]);
        if status != "open" && !include_struck {
            continue;
        }
        let (title, priority) = split_priority(&m[3]);
        items.push(json!({
            "id": m[2].to_string(),
            "title": title,
            "priority": priority,
            "status": status,
            "line": idx as i64 + 1,
        }));
    }
    items
}

fn existing_ids(text: &str) -> std::collections::BTreeSet<String> {
    compile(FU_MINTED_RE)
        .find_iter(text)
        .map(|m| m.as_str().to_string())
        .collect()
}

/// Lines that carry a `fu-` token but fail the strict item regex are
/// invisible to parse_items/list/triage yet still count toward collision
/// avoidance. Surfacing them lets `list` warn instead of swallowing a
/// hand-edited item.
fn find_unparseable_fu_lines(text: &str) -> Vec<(usize, String)> {
    let fu_re = compile(FU_MINTED_RE);
    let item_re = compile(ITEM_RE);
    let mut bad = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if fu_re.is_match(line) && !item_re.is_match(line) {
            bad.push((idx + 1, line.trim().to_string()));
        }
    }
    bad
}

// ---------------------------------------------------------------------------
// Typed-item lens: generalized read-only parser
// ---------------------------------------------------------------------------
//
// A *managed item* is a checkbox line that carries a type token. Four types:
// followup (fu-), carveout (cv-, lifecycle owned by carveouts.jsonl + retro),
// node (ab-*, transient; tidy ejects completed filed nodes), and human (a
// configured maintainer-marker tag; inactive by default). The discriminator is
// the checkbox-line structure, never the bare token: a fu- mentioned inside a
// narrative paragraph is never an item.

/// Classify a managed shortcode token into its family. Keys on the full
/// reserved prefix, not the first two chars, so a configured node prefix that
/// merely starts with fu/cv is still a node.
fn token_type(token: &str) -> &'static str {
    if token.starts_with("fu-") {
        "followup"
    } else if token.starts_with("cv-") {
        "carveout"
    } else {
        "node"
    }
}

/// Split title/priority tolerating a non-trailing `(pN)` followed by prose.
/// Real post-merge ab-* lines carry the priority mid-line with prose after it,
/// so only the lenient first-`(pN)` split reads them right; for the common
/// trailing shape the first match IS the trailing one.
fn split_priority_lenient(rest: &str) -> (String, Option<String>) {
    let re = compile(ANY_PRIORITY_RE);
    match re.find(rest) {
        Some(m) => (
            rest[..m.start()].trim().to_string(),
            Some(m.as_str().trim_matches(['(', ')']).to_string()),
        ),
        None => (rest.trim().to_string(), None),
    }
}

/// One managed shortcode match: (mark, token, rest). The wheel's node arm
/// carried a negative lookahead so a MALFORMED sibling (a cv- token failing
/// its own 8-hex arm) is never stolen by the liberal node arm; this crate
/// pins regex 1.x, which rejects lookarounds, so the same rejection is the
/// explicit cv shape check here.
fn managed_shortcode_match(line: &str) -> Option<(String, String, String)> {
    let re = compile(MANAGED_SHORTCODE_RE);
    let m = re.captures(line)?;
    let token = m[2].to_string();
    if token.starts_with("cv-") {
        let hex = &token[3..];
        let is_eight_hex = hex.len() == 8 && hex.chars().all(|c| c.is_ascii_hexdigit());
        if !is_eight_hex {
            return None;
        }
    }
    Some((m[1].to_string(), token, m[3].to_string()))
}

/// The maintainer-marker tag regex, or None when the human arm is inactive
/// (no marker configured: a fresh install ships no one's initials). The tag
/// is escaped and whitespace/bos-bounded so a configured marker never
/// substring-matches unrelated text.
fn marker_tag_re(marker: Option<&str>) -> Option<Regex> {
    let marker = marker?;
    if marker.is_empty() {
        return None;
    }
    Some(compile(&format!(
        r"(?:^|\s){}(?:\s|$)",
        regex::escape(marker)
    )))
}

fn section_of(line: &str, heading_re: &Regex, pm_re: &Regex) -> Option<String> {
    if let Some(m) = pm_re.captures(line) {
        return Some(m[1].to_string());
    }
    heading_re.captures(line).map(|m| m[1].to_string())
}

/// Parse every *managed* checkbox line into a typed item object
/// (`{"type","id","title","priority","status","section","line"}`) where `id`
/// is null for human (maintainer-marker) lines. Mutates nothing and never
/// parses timeline prose. `section` is the nearest preceding `##`+ heading or
/// `<!-- post-merge:... -->` marker.
fn parse_managed_items(text: &str, include_struck: bool, marker: Option<&str>) -> Vec<Value> {
    let heading_re = compile(SECTION_HEADING_RE);
    let pm_re = compile(POST_MERGE_MARKER_RE);
    let marker_re = marker_tag_re(marker);
    let cb_re = compile(CHECKBOX_LINE_RE);
    let mut items = Vec::new();
    let mut section: Option<String> = None;
    for (idx, line) in text.lines().enumerate() {
        if let Some(sec) = section_of(line, &heading_re, &pm_re) {
            section = Some(sec);
            continue;
        }
        if let Some((mark, token, rest)) = managed_shortcode_match(line) {
            let status = status_by_mark(&mark);
            if status != "open" && !include_struck {
                continue;
            }
            // fu-/cv- carry their priority as the TRAILING (pN) suffix (add
            // mints them that way), so the strict trailing split preserves a
            // title containing a parenthetical like "(p2)". Only ab- lines
            // (post-merge prose) put (pN) mid-line, which needs the lenient
            // first-(pN) split.
            let (title, priority) = if token_type(&token) == "node" {
                split_priority_lenient(&rest)
            } else {
                split_priority(&rest)
            };
            items.push(json!({
                "type": token_type(&token),
                "id": token,
                "title": title,
                "priority": priority,
                "status": status,
                "section": section,
                "line": idx as i64 + 1,
            }));
            continue;
        }
        if let Some(marker_re) = &marker_re {
            if let Some(cb) = cb_re.captures(line) {
                if !marker_re.is_match(&cb[2]) {
                    continue;
                }
                let status = status_by_mark(&cb[1]);
                if status != "open" && !include_struck {
                    continue;
                }
                // Marker lines carry no minted shortcode; their priority is
                // the Obsidian-Tasks emoji vocabulary, not a (pN) token, so
                // the STRICT trailing split applies (a parenthetical inside
                // the readable text must not truncate the title).
                let (title, priority) = split_priority(&cb[2]);
                items.push(json!({
                    "type": "human",
                    "id": Value::Null,
                    "title": title,
                    "priority": priority,
                    "status": status,
                    "section": section,
                    "line": idx as i64 + 1,
                }));
            }
        }
    }
    items
}

/// Group managed items into the four labeled buckets, preserving order.
fn bucket_managed_items(items: Vec<Value>) -> Map<String, Value> {
    let mut buckets: Map<String, Value> = Map::new();
    for key in BUCKET_ORDER {
        buckets.insert(key.to_string(), Value::Array(Vec::new()));
    }
    for item in items {
        let bucket = match item.get("type").and_then(Value::as_str) {
            Some("human") => "your_actions",
            Some("followup") => "followups",
            Some("carveout") => "carveouts",
            _ => "filed_nodes",
        };
        if let Some(Value::Array(list)) = buckets.get_mut(bucket) {
            list.push(item);
        }
    }
    buckets
}

/// Mechanical regex pre-filter for deferral language: the deterministic,
/// provider-neutral half; the LLM filters these candidates and decides which
/// warrant an `add`.
fn scan_transcript(text: &str) -> Vec<Value> {
    let re = compile(SCAN_RE);
    let mut out = Vec::new();
    for (idx, line) in text.lines().enumerate() {
        if re.is_match(line) {
            out.push(json!({
                "line": idx as i64 + 1,
                "text": line.trim(),
            }));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Inbox file scaffold + locked write
// ---------------------------------------------------------------------------

const SCAFFOLD: &str = "---\ntitle: Abilities \u{2014} backlog inbox\ndescription: Capture tier below idea nodes. Items here are NOT graph nodes; triage promotes warranted items to ab-XXXXXXXX.\ncreated: {date}\nupdated: {ts}\n---\n\n# Abilities \u{2014} backlog inbox\n";

const ARCHIVE_SCAFFOLD: &str = "---\ntitle: Abilities \u{2014} backlog inbox archive\ncreated: {date}\n---\n\n# Abilities \u{2014} backlog inbox archive\n";

fn now_date() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn now_ts() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M").to_string()
}

/// Return `text` if it already has content, else a fresh scaffold.
fn ensure_scaffold(text: &str) -> String {
    if !text.trim().is_empty() {
        return text.to_string();
    }
    SCAFFOLD
        .replace("{date}", &now_date())
        .replace("{ts}", &now_ts())
}

fn has_today_heading(text: &str) -> bool {
    let today = now_date();
    text.lines()
        .any(|line| line.starts_with("## ") && line.contains(&today))
}

fn validate_add_fields(source: &str, why: &str, priority: &str) -> Result<(), CaptureError> {
    let src = source.trim();
    if src.is_empty() {
        return Err(CaptureError::validation("--source must not be empty"));
    }
    if compile(MEMORY_SLUG_RE).is_match(src) {
        return Err(CaptureError::validation(
            "--source must be a substrate reference (PR#, commit, file, URL), not a memory slug like [[feedback_x]]",
        ));
    }
    if why.trim().is_empty() {
        return Err(CaptureError::validation("--why must not be empty"));
    }
    if why.len() > MAX_WHY_LEN {
        return Err(CaptureError::validation(format!(
            "--why exceeds {MAX_WHY_LEN} chars (got {}); shorten it",
            why.len()
        )));
    }
    if !VALID_PRIORITIES.contains(&priority) {
        return Err(CaptureError::validation(format!(
            "--priority must be one of ['p0', 'p1', 'p2', 'p3'], got '{priority}'"
        )));
    }
    Ok(())
}

/// Whitespace-collapsed, lower-cased normalization for dedup/digest keys.
fn norm(s: &str) -> String {
    let re = regex::Regex::new(r"\s+").expect("whitespace regex compiles");
    re.replace_all(s.trim(), " ").to_lowercase()
}

fn extract_where(block: &[&str]) -> Option<String> {
    let re = compile(WHERE_SUBLINE_RE);
    for sub in block.iter().skip(1) {
        if let Some(m) = re.captures(sub) {
            return Some(m[1].to_string());
        }
    }
    None
}

/// Atomic text write: same-directory temp file, then rename into place.
fn atomic_write_text(path: &Path, content: &str) -> Result<(), String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "inbox".to_string());
    let tmp = path.with_file_name(format!(".{}.{}.tmp", name, std::process::id()));
    std::fs::write(&tmp, content).map_err(|e| format!("write {tmp:?}: {e}"))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename {tmp:?} -> {path:?}: {e}")
    })
}

struct InboxGuard {
    _lock: crate::graph_store::BoundedLock,
}

fn lock_inbox(path: &Path) -> Result<InboxGuard, CaptureError> {
    let lock_path = path.with_file_name(format!(
        "{}.lock",
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    ));
    match crate::graph_store::BoundedLock::acquire(
        &lock_path,
        crate::graph_store::DEFAULT_LOCK_TIMEOUT,
    ) {
        Ok(lock) => Ok(InboxGuard { _lock: lock }),
        Err(e) => Err(CaptureError::Lock(format!(
            "could not lock {}: {e}",
            lock_path.display()
        ))),
    }
}

// ---------------------------------------------------------------------------
// Add: dedup pre-check + shaped append
// ---------------------------------------------------------------------------

/// The EXISTING open fu- item that duplicates (title, where), if any. Uses the
/// SAME exact key as tidy's cluster dedup - (norm(where), norm(title)) - so
/// the capture-time pre-check and the report-only tidy dedup agree on what
/// "duplicate" means: a conservative exact match. Only OPEN `[ ]` followups
/// are candidates; a promoted/dismissed line, or a cv-/ab- line, never blocks
/// a fresh capture.
fn find_duplicate_open_fu(
    text: &str,
    title: &str,
    where_: Option<&str>,
) -> Option<(String, String, Option<String>, Option<String>)> {
    // (id, title, priority, where)
    let target_where = norm(where_.unwrap_or(""));
    let target_title = norm(title);
    let lines: Vec<&str> = text.lines().collect();
    let n = lines.len();
    let mut i = 0usize;
    while i < n {
        let Some((mark, token, rest)) = managed_shortcode_match(lines[i]) else {
            i += 1;
            continue;
        };
        if mark != " " || !token.starts_with("fu-") {
            i += 1;
            continue;
        }
        let mut block: Vec<&str> = vec![lines[i]];
        let mut j = i + 1;
        while j < n && lines[j].starts_with("  ") && !lines[j].trim().is_empty() {
            block.push(lines[j]);
            j += 1;
        }
        let (existing_title, existing_priority) = split_priority(&rest);
        let existing_where = extract_where(&block);
        if (
            norm(existing_where.as_deref().unwrap_or("")),
            norm(&existing_title),
        ) == (target_where.clone(), target_title.clone())
        {
            return Some((token, existing_title, existing_priority, existing_where));
        }
        i = j;
    }
    None
}

/// Validate, lock, append a shaped item line + sub-lines. Returns the item.
/// Creates the inbox file with a frontmatter scaffold on first use. The write
/// is serialized by an exclusive lock on the sibling `.lock` file so
/// concurrent adds (including sibling worktrees sharing the symlinked target)
/// never lose an append.
fn add_item(
    path: &Path,
    title: &str,
    source: &str,
    why: &str,
    where_: Option<&str>,
    priority: &str,
) -> Result<Value, CaptureError> {
    validate_add_fields(source, why, priority)?;
    let title = title.trim();
    if title.is_empty() {
        return Err(CaptureError::validation("title must not be empty"));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| CaptureError::Lock(format!("could not create {parent:?}: {e}")))?;
    }
    let guard = lock_inbox(path)?;
    let result = add_locked(path, title, source, why, where_, priority);
    drop(guard);
    result
}

fn add_locked(
    path: &Path,
    title: &str,
    source: &str,
    why: &str,
    where_: Option<&str>,
    priority: &str,
) -> Result<Value, CaptureError> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let text = ensure_scaffold(&raw);

    // Dedup pre-check (inside the lock so it is race-free with concurrent
    // adds): if an open followup already covers this (title, where), return
    // ITS fields and skip minting + the file write. id/title/priority/where
    // describe the EXISTING item; source/why record the deduped attempt.
    if let Some((dup_id, dup_title, dup_priority, dup_where)) =
        find_duplicate_open_fu(&text, title, where_)
    {
        return Ok(json!({
            "id": dup_id,
            "title": dup_title,
            "priority": dup_priority.or_else(|| Some(priority.to_string())),
            "source": source.trim(),
            "why": why.trim(),
            "where": dup_where,
            "status": "open",
            "deduped": true,
        }));
    }

    let fu_id = mint_fu_id(&existing_ids(&text)).map_err(CaptureError::validation)?;

    let mut block_lines: Vec<String> = Vec::new();
    if !has_today_heading(&text) {
        block_lines.push(String::new());
        block_lines.push(format!("## {}", now_date()));
    }
    block_lines.push(format!("- [ ] {fu_id} - {title} ({priority})"));
    block_lines.push(format!("  source: {}", source.trim()));
    block_lines.push(format!("  why: {}", why.trim()));
    if let Some(w) = where_.map(str::trim).filter(|w| !w.is_empty()) {
        block_lines.push(format!("  where: {w}"));
    }

    let mut next = text;
    if !next.ends_with('\n') {
        next.push('\n');
    }
    next.push_str(&block_lines.join("\n"));
    next.push('\n');
    atomic_write_text(path, &next).map_err(CaptureError::Lock)?;

    Ok(json!({
        "id": fu_id,
        "title": title,
        "priority": priority,
        "source": source.trim(),
        "why": why.trim(),
        "where": where_.map(str::trim).filter(|w| !w.is_empty()),
        "status": "open",
        "deduped": false,
    }))
}

// ---------------------------------------------------------------------------
// Line rewrite (promote / dismiss) + node creation
// ---------------------------------------------------------------------------

/// The item line whose id is `fu_id`, matched with the strict item regex.
fn find_item<'a>(text: &'a str, fu_id: &str) -> Option<regex::Captures<'a>> {
    let item_re = compile(ITEM_RE);
    for line in text.lines() {
        if let Some(m) = item_re.captures(line) {
            if &m[2] == fu_id {
                return Some(m);
            }
        }
    }
    None
}

/// Pure: return `text` with the line whose item id is `fu_id` replaced by
/// `new_line`. The caller holds the inbox lock around the read and the write,
/// so the check-then-write is atomic (no TOCTOU).
fn replace_item_line(text: &str, fu_id: &str, new_line: &str) -> Result<String, CaptureError> {
    let item_re = compile(ITEM_RE);
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let mut found = false;
    for line in lines.iter_mut() {
        if let Some(m) = item_re.captures(line) {
            if &m[2] == fu_id {
                *line = new_line.to_string();
                found = true;
                break;
            }
        }
    }
    if !found {
        return Err(CaptureError::validation(format!("unknown fu-id: {fu_id}")));
    }
    let mut new_text = lines.join("\n");
    if text.ends_with('\n') {
        new_text.push('\n');
    }
    Ok(new_text)
}

/// The item's `source:` sub-line value as birth evidence; None if absent.
fn item_source_ref(text: &str, fu_id: &str) -> Option<String> {
    let item_re = compile(ITEM_RE);
    let lines: Vec<&str> = text.lines().collect();
    for (idx, line) in lines.iter().enumerate() {
        let Some(m) = item_re.captures(line) else {
            continue;
        };
        if &m[2] != fu_id {
            continue;
        }
        for sub in lines.iter().skip(idx + 1) {
            if item_re.is_match(sub)
                || (!sub.is_empty() && !sub[..1].chars().all(char::is_whitespace))
            {
                return None;
            }
            if let Some(rest) = sub.trim().strip_prefix("source:") {
                let value = rest.trim();
                return if value.is_empty() {
                    None
                } else {
                    Some(value.to_string())
                };
            }
        }
        return None;
    }
    None
}

/// Strike an item as dismissed (`[-]`), preserving the line.
fn dismiss_item(path: &Path, fu_id: &str, reason: &str) -> Result<Value, CaptureError> {
    if reason.trim().is_empty() {
        return Err(CaptureError::validation(
            "dismiss requires a non-empty reason",
        ));
    }
    let reason = reason.trim();
    let guard = lock_inbox(path)?;
    let result = (|| {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let Some(m) = find_item(&text, fu_id) else {
            return Err(CaptureError::validation(format!("unknown fu-id: {fu_id}")));
        };
        match &m[1] {
            "-" => Ok(json!({"id": fu_id, "status": "already_dismissed"})),
            "x" => Err(CaptureError::validation(format!(
                "{fu_id} was promoted; cannot dismiss"
            ))),
            _ => {
                let new_line = format!("- [-] {} - {} (dismissed: {reason})", &m[2], &m[3]);
                let next = replace_item_line(&text, fu_id, &new_line)?;
                atomic_write_text(path, &next).map_err(CaptureError::Lock)?;
                Ok(json!({"id": fu_id, "status": "dismissed", "reason": reason}))
            }
        }
    })();
    drop(guard);
    result
}

// ---------------------------------------------------------------------------
// Promote: graph node creation + strike
// ---------------------------------------------------------------------------

/// Create a plan-less idea node on the graph; return its id. Reuses the
/// canonical mint + the shared write pipeline (status recompute) so a schema
/// addition shows up here too. The wheel's mutator appended the node only:
/// no autolink, no related edges.
fn create_graph_node(
    title: &str,
    priority: &str,
    difficulty: &str,
    blocks_everything: bool,
    source_kind: &str,
    origin_evidence: &str,
) -> Result<String, CaptureError> {
    if let Err(r) = super::create_cli::validate_priority(priority, blocks_everything) {
        return Err(CaptureError::validation(r.message));
    }
    let (origin, origin_evidence_ref, refused_note) = super::create_cli::stamp_request_origin(
        source_kind,
        "capture_promote",
        Some(origin_evidence),
    );
    if source_kind == "operator_request" && origin != "operator_request" {
        // The wheel refused at node build (exit 1): the stamp is the only
        // evidence behind operator standing, so a missing queue answer kills
        // the promotion before any write.
        return Err(CaptureError::Refusal(format!(
            "Error: {}",
            refused_note.unwrap_or_else(|| {
                "--source-kind operator_request could not be verified".to_string()
            })
        )));
    }
    let cwd_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let graph = super::settings::graph_path();
    let mut retry = 0u8;
    loop {
        retry += 1;
        let base_version = crate::graph_store::base_version(&graph)
            .map_err(|e| CaptureError::validation(format!("graph read failed: {e}")))?;
        let rows = crate::graph_store::read_rows(&graph)
            .map_err(|e| CaptureError::validation(format!("graph read failed: {e}")))?;
        let known_ids: std::collections::BTreeSet<String> = rows
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let minted =
            super::create_cli::mint_node_id(&known_ids).map_err(CaptureError::validation)?;
        let now = crate::graph_store::now_isoformat();
        let cwd_string = std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .to_string_lossy()
            .into_owned();
        let provenance = super::create_cli::session_provenance(&cwd_root, None, Some(&known_ids));
        let (
            source_session_id,
            source_harness,
            source_cwd,
            _source_node_id,
            source_plan_path,
            _dropped,
        ) = provenance;
        let node = build_capture_node(
            &minted,
            title,
            priority,
            difficulty,
            blocks_everything,
            source_kind,
            origin_evidence,
            &cwd_string,
            now.clone(),
            source_session_id.clone(),
            source_harness.clone(),
            source_cwd.clone(),
            source_plan_path.clone(),
            origin.clone(),
            origin_evidence_ref.clone(),
        );
        let mut working = rows.clone();
        working.push(node.clone());
        crate::graph_store::recompute_statuses_with_plan_rungs(&mut working, None);
        match crate::graph_store::locked_mutate(
            &graph,
            crate::graph_store::MutateInput {
                entries: working,
                canonical_path: None,
                base_version,
                plan_rungs: None,
            },
            crate::graph_store::DEFAULT_LOCK_TIMEOUT,
        ) {
            Ok(_) => return Ok(minted),
            Err(crate::graph_store::StoreError::Conflict) if retry < 5 => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(CaptureError::validation(format!("graph write failed: {e}"))),
        }
    }
}

/// The capture-born node's field set: the wheel's `_build_backlog_node` with
/// the capture call's arguments and defaults (type feature, domain code, no
/// parent, no tags). Session provenance rides in; the registry-row lookup the
/// native create path adds is a create surface, not part of this builder.
#[allow(clippy::too_many_arguments)]
fn build_capture_node(
    minted: &str,
    title: &str,
    priority: &str,
    difficulty: &str,
    blocks_everything: bool,
    source_kind: &str,
    origin_evidence: &str,
    cwd_string: &str,
    now: String,
    source_session_id: Option<String>,
    source_harness: Option<String>,
    source_cwd: Option<String>,
    source_plan_path: Option<String>,
    origin: String,
    origin_evidence_ref: Option<String>,
) -> Value {
    let mut n = Map::new();
    n.insert("id".into(), json!(minted));
    n.insert("parent".into(), Value::Null);
    n.insert("tags".into(), json!([]));
    n.insert("title".into(), json!(title));
    n.insert("type".into(), json!("feature"));
    n.insert(
        "project".into(),
        super::create_cli::detect_project(
            &std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        )
        .map(Value::String)
        .unwrap_or(Value::Null),
    );
    n.insert("cwd".into(), json!(cwd_string));
    n.insert("status".into(), json!("idea"));
    n.insert("priority".into(), json!(priority));
    n.insert("blocks_everything".into(), json!(blocks_everything));
    n.insert("difficulty".into(), json!(difficulty));
    let dh = json!([{"value": difficulty, "source": "filed", "ts": now}]);
    n.insert("difficulty_history".into(), dh);
    n.insert("domain".into(), json!("code"));
    n.insert("blocked_by".into(), json!([]));
    n.insert("session_id".into(), Value::Null);
    n.insert("locked_at".into(), Value::Null);
    n.insert("completed_at".into(), Value::Null);
    n.insert("has_brief".into(), json!(false));
    n.insert("roadmap_id".into(), Value::Null);
    n.insert("vision_path".into(), Value::Null);
    n.insert("details".into(), Value::Null);
    n.insert("size".into(), Value::Null);
    n.insert("batch".into(), Value::Null);
    n.insert("cost_usd".into(), Value::Null);
    n.insert("cost_sessions".into(), json!([]));
    n.insert("plan_path".into(), Value::Null);
    n.insert("pr_number".into(), Value::Null);
    n.insert("pr_url".into(), Value::Null);
    n.insert("merge_status".into(), Value::Null);
    n.insert("created_at".into(), json!(now.as_str()));
    n.insert("source".into(), Value::Null);
    n.insert("source_kind".into(), json!(source_kind));
    n.insert("source_project".into(), Value::Null);
    n.insert("source_inbox_msg".into(), Value::Null);
    n.insert("source_inbox_thread".into(), Value::Null);
    n.insert("artifact_url".into(), Value::Null);
    n.insert("completion_note".into(), Value::Null);
    n.insert("source_session_id".into(), json!(source_session_id));
    n.insert("source_harness".into(), json!(source_harness));
    n.insert("source_cwd".into(), json!(source_cwd));
    n.insert("source_node_id".into(), Value::Null);
    n.insert("source_plan_path".into(), json!(source_plan_path));
    n.insert("request_origin".into(), json!(origin));
    n.insert("origin_evidence".into(), json!(origin_evidence_ref));
    Value::Object(n)
}

// ---------------------------------------------------------------------------
// Promote: the locked strike path
// ---------------------------------------------------------------------------

/// Promote an inbox item to a graph node and strike its checkbox. Idempotent:
/// re-promoting an already-promoted item returns the existing node id without
/// creating a duplicate. The node is created BEFORE the strike so a failed
/// strike surfaces loudly (a node with an un-struck line). The whole critical
/// section runs under the inbox lock so two concurrent promotes of the same
/// id cannot both pass the check and mint duplicate nodes.
fn promote_item(
    path: &Path,
    fu_id: &str,
    priority: Option<&str>,
    difficulty_raw: &str,
    blocks_everything: bool,
    source_kind: &str,
) -> Result<Value, CaptureError> {
    if let Some(p) = priority {
        if !VALID_PRIORITIES.contains(&p) {
            return Err(CaptureError::validation(format!(
                "--priority must be one of ['p0', 'p1', 'p2', 'p3'], got '{p}'"
            )));
        }
    }
    let difficulty = match super::create_cli::normalize_difficulty(difficulty_raw) {
        Ok(d) => d,
        Err(r) => {
            return Err(CaptureError::validation(format!(
                "{}. {}",
                r.message,
                super::create_cli::DIFFICULTY_HELP
            )));
        }
    };
    let guard = lock_inbox(path)?;
    let result = (|| {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let Some(m) = find_item(&text, fu_id) else {
            return Err(CaptureError::validation(format!("unknown fu-id: {fu_id}")));
        };
        if &m[1] == "x" {
            let node_re = compile(NODE_ID_RE);
            let node_id = node_re.captures(&m[0]).map(|nm| nm[1].to_string());
            return Ok(json!({
                "id": fu_id,
                "node_id": node_id,
                "status": "already_promoted"
            }));
        }
        if &m[1] == "-" {
            return Err(CaptureError::validation(format!(
                "{fu_id} was dismissed; cannot promote"
            )));
        }
        let (title, parsed_priority) = split_priority(&m[3]);
        let node_priority = priority
            .map(str::to_string)
            .or(parsed_priority)
            .unwrap_or_else(|| "p2".to_string());
        let source_ref = item_source_ref(&text, fu_id);
        let origin_ref = match &source_ref {
            Some(src) => format!("{fu_id} source: {src}"),
            None => fu_id.to_string(),
        };
        let node_id = create_graph_node(
            &title,
            &node_priority,
            &difficulty,
            blocks_everything,
            source_kind,
            &origin_ref,
        )?;
        let new_line = format!("- [x] {} - {} -> {}", &m[2], &m[3], node_id);
        let next = replace_item_line(&text, fu_id, &new_line)?;
        atomic_write_text(path, &next).map_err(CaptureError::Lock)?;
        Ok(json!({
            "id": fu_id,
            "node_id": node_id,
            "status": "promoted",
        }))
    })();
    drop(guard);
    result
}

// ---------------------------------------------------------------------------
// Archive: sweep struck items into the sibling archive
// ---------------------------------------------------------------------------

/// Append ejected blocks under a timestamped heading in the sibling archive
/// (same format as archive_struck). Create-before-truncate: the caller
/// appends the archive BEFORE truncating the inbox so a crash leaves a
/// recoverable duplicate, never a hole where the line is gone from both
/// files.
fn archive_append(archive_path: &Path, moved_lines: &[String]) -> Result<(), String> {
    let mut existing = std::fs::read_to_string(archive_path).unwrap_or_default();
    if existing.trim().is_empty() {
        existing = ARCHIVE_SCAFFOLD.replace("{date}", &now_date());
    }
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    existing.push_str(&format!("\n## archived {}\n", now_ts()));
    existing.push_str(&moved_lines.join("\n"));
    existing.push('\n');
    if let Some(parent) = archive_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir {parent:?}: {e}"))?;
    }
    atomic_write_text(archive_path, &existing)
}

/// Sweep struck items (promoted/dismissed) into a sibling archive file. Open
/// items stay in place; each item's sub-lines (source/why/where) move with
/// it. Returns `{"archived": N, "archive_path": ...}`.
fn archive_struck(path: &Path) -> Result<Value, CaptureError> {
    let archive_path = path.with_file_name("inbox-archive.md");
    let guard = lock_inbox(path)?;
    let result = (|| {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let item_re = compile(ITEM_RE);
        let mut kept: Vec<String> = Vec::new();
        let mut moved: Vec<String> = Vec::new();
        let mut archived = 0usize;
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len();
        block_walk(&lines, &item_re, &mut |is_struck, block: Vec<String>| {
            if is_struck {
                moved.extend(block);
                archived += 1;
            } else {
                kept.extend(block);
            }
        });
        if archived == 0 {
            return Ok(json!({
                "archived": 0,
                "archive_path": archive_path.to_string_lossy(),
            }));
        }
        let mut new_text = kept.join("\n");
        if !new_text.is_empty() && !new_text.ends_with('\n') {
            new_text.push('\n');
        }
        atomic_write_text(path, &new_text).map_err(CaptureError::Lock)?;
        archive_append(&archive_path, &moved).map_err(CaptureError::Lock)?;
        Ok(json!({
            "archived": archived,
            "archive_path": archive_path.to_string_lossy(),
        }))
    })();
    drop(guard);
    result
}

/// Walk item blocks (item line + immediately-following indented sub-lines,
/// stopping at a blank line, heading, or next item) and hand each block to
/// `sink` with a struck flag. Mirrors the wheel's block walk in
/// archive_struck, _process_body, and the dedup pre-check.
fn block_walk(lines: &[&str], item_re: &Regex, sink: &mut dyn FnMut(bool, Vec<String>)) {
    let n = lines.len();
    let mut i = 0usize;
    while i < n {
        let line = lines[i];
        let Some(m) = item_re.captures(line) else {
            sink(false, vec![line.to_string()]);
            i += 1;
            continue;
        };
        let struck = &m[1] != " ";
        let mut block: Vec<String> = vec![line.to_string()];
        let mut j = i + 1;
        while j < n && lines[j].starts_with("  ") && !lines[j].trim().is_empty() {
            block.push(lines[j].to_string());
            j += 1;
        }
        sink(struck, block);
        i = j;
    }
}

// ---------------------------------------------------------------------------
// Tidy: one idempotent pass
// ---------------------------------------------------------------------------

/// Read the graph directly (not the status-recomputing reader) so a corrupt
/// graph surfaces here as a warning on the manual tidy path, never silently
/// skips eject. Returns (completed ids, warning).
fn completed_node_ids(
    graph_path: &Path,
    include_deferred: bool,
) -> (std::collections::BTreeSet<String>, Option<String>) {
    let mut done = std::collections::BTreeSet::new();
    let rows = match crate::graph_store::read_rows(graph_path) {
        Ok(rows) => rows,
        Err(e) => {
            return (
                done,
                Some(format!("graph.json unreadable ({e}); skipping eject")),
            );
        }
    };
    for row in &rows {
        let Some(nid) = row.get("id").and_then(Value::as_str) else {
            continue;
        };
        let done_at = row.get("completed_at").map(Value::is_null) == Some(false);
        let superseded = row.get("superseded_by").map(Value::is_null) == Some(false);
        let deferred = row.get("deferred_at").map(Value::is_null) == Some(false);
        if done_at || superseded || (include_deferred && deferred) {
            done.insert(nid.to_string());
        }
    }
    (done, None)
}

/// Remove an existing digest block (markers inclusive). Refuses on marker
/// drift: more than one start/end or an end before its start raises rather
/// than guessing (a hand-edited/duplicated block must not let tidy append a
/// third digest or clobber prose).
fn strip_digest_block(lines: Vec<String>) -> Result<Vec<String>, CaptureError> {
    let starts: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, ln)| ln.trim() == DIGEST_START)
        .map(|(i, _)| i)
        .collect();
    let ends: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, ln)| ln.trim() == DIGEST_END)
        .map(|(i, _)| i)
        .collect();
    if starts.is_empty() && ends.is_empty() {
        return Ok(lines);
    }
    if starts.len() != 1 || ends.len() != 1 || ends[0] < starts[0] {
        return Err(CaptureError::validation(format!(
            "inbox digest marker drift: expected exactly one {DIGEST_START} / {DIGEST_END} pair (found {} start, {} end); refusing to guess. Fix the markers by hand.",
            starts.len(),
            ends.len()
        )));
    }
    let mut out = lines[..starts[0]].to_vec();
    out.extend_from_slice(&lines[ends[0] + 1..]);
    Ok(out)
}

/// Split into (head, body). Head = frontmatter + the first H1 heading line;
/// body = everything after the H1. The digest is pinned at the top of body.
fn split_head_body(lines: &[String]) -> (Vec<String>, Vec<String>) {
    let mut idx = 0usize;
    if lines.first().map(|s| s.trim() == "---").unwrap_or(false) {
        for (j, line) in lines.iter().enumerate().skip(1) {
            if line.trim() == "---" {
                idx = j + 1;
                break;
            }
        }
    }
    let mut k = idx;
    while k < lines.len() && lines[k].trim().is_empty() {
        k += 1;
    }
    let h1 = compile(H1_RE);
    if k < lines.len() && h1.is_match(&lines[k]) {
        return (lines[..k + 1].to_vec(), lines[k + 1..].to_vec());
    }
    (lines[..idx].to_vec(), lines[idx..].to_vec())
}

fn rstrip_blank(lines: &[String]) -> Vec<String> {
    let mut end = lines.len();
    while end > 0 && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[..end].to_vec()
}

fn trim_blank_edges(lines: &[String]) -> Vec<String> {
    let mut start = 0usize;
    let mut end = lines.len();
    while start < end && lines[start].trim().is_empty() {
        start += 1;
    }
    while end > start && lines[end - 1].trim().is_empty() {
        end -= 1;
    }
    lines[start..end].to_vec()
}

/// Group open items by exact (normalized where + normalized title). Report
/// clusters of >=2 only. Report-only in v1 (no auto-merge) so a capture is
/// never silently lost.
fn cluster_dedup(items: &[(String, String, Option<String>)]) -> Vec<Vec<String>> {
    let mut groups: std::collections::BTreeMap<(String, String), Vec<String>> =
        std::collections::BTreeMap::new();
    let mut order: Vec<(String, String)> = Vec::new();
    for (token, title, where_) in items {
        let key = (norm(where_.as_deref().unwrap_or("")), norm(title));
        if !groups.contains_key(&key) {
            order.push(key.clone());
        }
        groups.entry(key).or_default().push(token.clone());
    }
    order
        .iter()
        .filter_map(|k| groups.get(k))
        .filter(|g| g.len() >= 2)
        .cloned()
        .collect()
}

/// Walk the timeline body. Eject completed filed nodes (block-aware),
/// normalize the separator on kept managed lines, and collect open items for
/// the report-only dedup. Returns (kept, moved, ejected, dedup_clusters).
fn process_body(
    body: &[String],
    complete_ids: &std::collections::BTreeSet<String>,
) -> (Vec<String>, Vec<String>, usize, Vec<Vec<String>>) {
    let managed_re = compile(MANAGED_SHORTCODE_RE);
    let node_re = compile(NODE_ID_RE);
    let mut kept: Vec<String> = Vec::new();
    let mut moved: Vec<String> = Vec::new();
    let mut ejected = 0usize;
    let mut open_items: Vec<(String, String, Option<String>)> = Vec::new();
    let n = body.len();
    let mut i = 0usize;
    while i < n {
        let line = &body[i];
        let Some(raw) = managed_re.captures(line) else {
            kept.push(line.clone());
            i += 1;
            continue;
        };
        let mark = raw[1].to_string();
        let token = raw[2].to_string();
        let rest = raw[3].to_string();
        let mut block: Vec<String> = vec![line.clone()];
        let mut j = i + 1;
        while j < n && body[j].starts_with("  ") && !body[j].trim().is_empty() {
            block.push(body[j].clone());
            j += 1;
        }
        let node_id: Option<String> = if token_type(&token) == "node" {
            Some(token.clone())
        } else {
            node_re.captures(line).map(|nm| nm[1].to_string())
        };
        if let Some(nid) = &node_id {
            if complete_ids.contains(nid) {
                moved.extend(block);
                ejected += 1;
                i = j;
                continue;
            }
        }
        // Keep: normalize the token separator to ' - ' (only here, never in
        // the title prose). Re-emitting in canonical form is a no-op once
        // hyphenated.
        let first = format!("- [{mark}] {token} - {rest}");
        kept.push(first);
        kept.extend(block.iter().skip(1).cloned());
        if mark == " " {
            let (title, _prio) = if token_type(&token) == "node" {
                split_priority_lenient(&rest)
            } else {
                split_priority(&rest)
            };
            let where_ = extract_where(&block);
            open_items.push((token, title, where_));
        }
        i = j;
    }
    let clusters = cluster_dedup(&open_items);
    (kept, moved, ejected, clusters)
}

/// Build the pinned digest block (markers inclusive) from open items.
/// Maintainer-marker actions: present ONLY when a marker is configured.
/// Deduped by normalized text, dated ascending by the calendar emoji then
/// undated in stable source order. Followups: count + ids per priority tier.
/// With no marker the actions section is omitted entirely (a fresh install
/// emits no one's tag).
fn build_digest(open_items: &[Value], marker: Option<&str>) -> Vec<String> {
    let mut block: Vec<String> = vec![DIGEST_START.to_string()];
    if let Some(marker) = marker {
        let jc_re = compile(JC_DATE_RE);
        let human: Vec<&Value> = open_items
            .iter()
            .filter(|it| it.get("type").and_then(Value::as_str) == Some("human"))
            .collect();
        let annotated: Vec<(Option<String>, usize, String)> = human
            .iter()
            .enumerate()
            .map(|(idx, it)| {
                let title = it.get("title").and_then(Value::as_str).unwrap_or("");
                (
                    jc_re.captures(title).map(|m| m[1].to_string()),
                    idx,
                    title.to_string(),
                )
            })
            .collect();
        let (dated, undated): (Vec<_>, Vec<_>) = annotated.iter().partition(|a| a.0.is_some());
        let mut dated = dated;
        dated.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut action_lines: Vec<String> = Vec::new();
        let mut seen: std::collections::BTreeSet<String> = Default::default();
        for (_date, _idx, title) in dated.iter().chain(undated.iter()) {
            let key = norm(title);
            if !seen.insert(key) {
                continue;
            }
            action_lines.push(format!("- {title}"));
        }
        block.push(format!("## Open {marker} actions"));
        if action_lines.is_empty() {
            block.push("_(none)_".to_string());
        } else {
            block.extend(action_lines);
        }
    }
    let fu_lines = digest_fu_lines(open_items);
    block.push("## Open followups by priority".to_string());
    block.extend(fu_lines);
    block.push(DIGEST_END.to_string());
    block
}

/// The digest's followup lines: count + ids per priority tier, then the
/// unprioritized residue.
fn digest_fu_lines(open_items: &[Value]) -> Vec<String> {
    let mut fu_lines: Vec<String> = Vec::new();
    for tier in VALID_PRIORITIES {
        let ids: Vec<&str> = open_items
            .iter()
            .filter(|it| it.get("type").and_then(Value::as_str) == Some("followup"))
            .filter(|it| it.get("priority").and_then(Value::as_str) == Some(tier))
            .map(|it| it.get("id").and_then(Value::as_str).unwrap_or(""))
            .collect();
        let suffix = if ids.is_empty() {
            String::new()
        } else {
            format!(" ({})", ids.join(", "))
        };
        fu_lines.push(format!("- {tier}: {}{suffix}", ids.len()));
    }
    let unprioritized: Vec<&str> = open_items
        .iter()
        .filter(|it| it.get("type").and_then(Value::as_str) == Some("followup"))
        .filter(|it| {
            let p = it.get("priority").and_then(Value::as_str).unwrap_or("");
            !VALID_PRIORITIES.contains(&p)
        })
        .map(|it| it.get("id").and_then(Value::as_str).unwrap_or(""))
        .collect();
    if !unprioritized.is_empty() {
        fu_lines.push(format!(
            "- (unprioritized): {} ({})",
            unprioritized.len(),
            unprioritized.join(", ")
        ));
    }
    fu_lines
}

/// Assemble head + digest + timeline into the new inbox text.
fn assemble(head: Vec<String>, digest: Vec<String>, timeline: Vec<String>) -> String {
    let head = rstrip_blank(&head);
    let timeline = trim_blank_edges(&timeline);
    let mut parts: Vec<String> = Vec::new();
    if !head.is_empty() {
        parts.extend(head);
        parts.push(String::new());
    }
    parts.extend(digest);
    if !timeline.is_empty() {
        parts.push(String::new());
        parts.extend(timeline);
    }
    let mut out = parts.join("\n");
    while out.ends_with('\n') {
        out.pop();
    }
    out.push('\n');
    out
}

/// Run one idempotent tidy pass over the inbox. Eject (completed filed
/// nodes) -> archive; dedup (report-only); separator normalize; rebuild the
/// pinned digest. Serialized by the same inbox lock the other write verbs
/// use so a concurrent add/promote append never races the read-modify-write.
fn tidy_pass(
    path: &Path,
    graph_path: &Path,
    include_deferred: bool,
    marker: Option<&str>,
) -> Result<Value, CaptureError> {
    let archive_path = path.with_file_name("inbox-archive.md");
    let (complete_ids, graph_warning) = completed_node_ids(graph_path, include_deferred);
    let guard = lock_inbox(path)?;
    let result = (|| {
        let raw = std::fs::read_to_string(path).unwrap_or_default();
        let text = ensure_scaffold(&raw);
        let lines: Vec<String> = text.lines().map(str::to_string).collect();
        let lines = strip_digest_block(lines)?;
        let (head, body) = split_head_body(&lines);
        let (kept, moved, ejected, clusters) = process_body(&body, &complete_ids);
        let open_items = parse_managed_items(&kept.join("\n"), false, marker);
        let digest = build_digest(&open_items, marker);
        // Durability ordering: append the archive BEFORE truncating the
        // inbox. A crash between the two leaves a recoverable duplicate (the
        // next idempotent tidy re-ejects it), never a hole.
        if !moved.is_empty() {
            archive_append(&archive_path, &moved).map_err(CaptureError::Lock)?;
        }
        let new_text = assemble(head, digest, kept);
        atomic_write_text(path, &new_text).map_err(CaptureError::Lock)?;
        let count_of = |t: &str| {
            open_items
                .iter()
                .filter(|it| it.get("type").and_then(Value::as_str) == Some(t))
                .count() as i64
        };
        Ok(json!({
            "ejected": ejected as i64,
            "archive_path": archive_path.to_string_lossy(),
            "dedup_clusters": clusters,
            "human_actions": count_of("human"),
            "followups_open": count_of("followup"),
            "graph_warning": graph_warning,
        }))
    })();
    drop(guard);
    result
}

// ---------------------------------------------------------------------------
// Session + event + artifact helpers
// ---------------------------------------------------------------------------

/// The capture-tier inbox path: the canonical resolver the triage port
/// already carries (config pin, parking-lot unification, vault/legacy
/// fallbacks).
fn inbox_path() -> PathBuf {
    super::triage::inbox_markdown_path().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".fno")
            .join("backlog")
            .join("inbox.md")
    })
}

/// The configured post_merge.maintainer_marker, or None (the default leaves
/// the capture human/maintainer arm inactive so a fresh install recognizes
/// nobody's initials).
fn maintainer_marker() -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let raw = crate::agents_config::config_lookup(&cwd, &["post_merge", "maintainer_marker"])?;
    let marker = raw.as_str()?.to_string();
    if marker.is_empty() {
        None
    } else {
        Some(marker)
    }
}

fn repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::paths::worktree_repo_root(&cwd)
}

fn events_path() -> PathBuf {
    crate::paths::events_path(&repo_root())
}

/// Read session_id from target-state.md; fall back to "manual". Resolves
/// through the hook-facing state resolver, never a hand-built state path.
fn detect_session_id() -> String {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let Some(state_path) = crate::state_path::resolve("target-state", &cwd) else {
        return "manual".to_string();
    };
    let Ok(text) = std::fs::read_to_string(&state_path) else {
        return "manual".to_string();
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("fno_id:") {
            let value = rest.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("session_id:") {
            let value = rest.trim();
            if !value.is_empty() {
                return value.to_string();
            }
        }
    }
    "manual".to_string()
}

/// Best-effort event emit. Returns true if the event was appended. For
/// add/dismiss/promote a missing event is telemetry loss, not data loss (the
/// markdown write already succeeded), so callers ignore the return. For
/// empty-pass the event IS the gate's third honesty factor, so that caller
/// checks the return and fails loudly.
fn emit(event_type: &str, data: Map<String, Value>) -> bool {
    let emitter = crate::events::EventEmitter::new(events_path(), "backlog");
    match emitter.emit_fields(event_type, data) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("warning: failed to emit {event_type} event: {e}");
            false
        }
    }
}

/// Write the deferrals gate artifact for an honest empty pass. Frontmatter
/// carries phase: deferrals + session_id (the stop-hook factor-2 check) plus
/// entries_written: 0 and scan_candidates: N (the anti-rubber-stamp audit
/// trail).
fn write_empty_pass_artifact(
    session_id: &str,
    reason: &str,
    scan_candidates: i64,
    artifacts_dir: &Path,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(artifacts_dir).map_err(|e| format!("mkdir {artifacts_dir:?}: {e}"))?;
    let artifact = artifacts_dir.join(format!("deferrals-{session_id}.md"));
    let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let content = format!(
        "---\nphase: deferrals\nsession_id: {session_id}\nentries_written: 0\nscan_candidates: {scan_candidates}\ncompleted_at: {completed_at}\n---\nEmpty deferrals pass: {reason}\n"
    );
    std::fs::write(&artifact, content).map_err(|e| format!("write {artifact:?}: {e}"))?;
    Ok(artifact)
}

/// Write the deferrals gate artifact for the capture path (>=1 item).
/// Symmetric with the empty-pass artifact but records the real capture
/// count; captured items are self-justifying so no reason is needed.
fn write_capture_artifact(
    session_id: &str,
    entries_written: i64,
    artifacts_dir: &Path,
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(artifacts_dir).map_err(|e| format!("mkdir {artifacts_dir:?}: {e}"))?;
    let artifact = artifacts_dir.join(format!("deferrals-{session_id}.md"));
    let completed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    let content = format!(
        "---\nphase: deferrals\nsession_id: {session_id}\nentries_written: {entries_written}\ncompleted_at: {completed_at}\n---\nCaptured {entries_written} deferral item(s) into the backlog capture tier.\n"
    );
    std::fs::write(&artifact, content).map_err(|e| format!("write {artifact:?}: {e}"))?;
    Ok(artifact)
}

/// Count events whose type is in `types` for `session_id`. Takes a list so
/// readers dual-accept the capture_* vocabulary and the legacy inbox_* one
/// (the store is append-only history; rows written by a pre-rename binary
/// keep their old type forever).
fn count_session_events(types: &[&str], session_id: &str, events_path: &Path) -> usize {
    let Ok(text) = std::fs::read_to_string(events_path) else {
        return 0;
    };
    text.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
        .filter(|ev| {
            let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
            types.contains(&kind)
        })
        .filter(|ev| {
            ev.get("data")
                .and_then(|d| d.get("session_id"))
                .and_then(Value::as_str)
                == Some(session_id)
        })
        .count()
}

// ---------------------------------------------------------------------------
// CLI layer
// ---------------------------------------------------------------------------

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(inline) = a.strip_prefix(&format!("{name}=")) {
            return Some(inline);
        }
        if a == name {
            return it.next().map(String::as_str);
        }
    }
    None
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

/// The `fno backlog capture` dispatcher arm: nine leaves over the same inbox
/// markdown the triage port already reads.
pub fn run_capture(tail: &[String]) -> i32 {
    let Some(cmd) = tail.first() else {
        eprint!("{}", include_str!("capture_help.txt"));
        return 2;
    };
    let rest = &tail[1..];
    match cmd.as_str() {
        "add" => cmd_add(rest),
        "list" => cmd_list(rest),
        "scan" => cmd_scan(rest),
        "empty-pass" => cmd_empty_pass(rest),
        "capture-pass" => cmd_capture_pass(rest),
        "promote" => cmd_promote(rest),
        "dismiss" => cmd_dismiss(rest),
        "archive" => cmd_archive(rest),
        "tidy" => cmd_tidy(rest),
        _ => {
            eprintln!("Error: No such command: {cmd}. Try `fno backlog capture --help`.");
            2
        }
    }
}

fn cmd_add(rest: &[String]) -> i32 {
    let Some(title) = rest.first() else {
        eprintln!("Error: Missing argument 'TITLE'.");
        return 2;
    };
    let Some(source) = flag_value(rest, "--source").or_else(|| flag_value(rest, "-s")) else {
        eprintln!("Error: Missing option '--source'.");
        return 2;
    };
    let Some(why) = flag_value(rest, "--why") else {
        eprintln!("Error: Missing option '--why'.");
        return 2;
    };
    let where_ = flag_value(rest, "--where").or_else(|| flag_value(rest, "-w"));
    let priority = flag_value(rest, "--priority")
        .or_else(|| flag_value(rest, "-p"))
        .unwrap_or("p2");
    let item = match add_item(&inbox_path(), title, source, why, where_, priority) {
        Ok(item) => item,
        Err(CaptureError::Validation(m)) => {
            eprintln!("error: {m}");
            return 2;
        }
        Err(CaptureError::Lock(m)) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    // Emit capture_add whether or not the item deduped: the capture pass ran
    // and the work is tracked, so the deferrals_captured gate's third factor
    // stays satisfied. The fu_id is the existing item's id on a dedup hit.
    emit(
        "capture_add",
        Map::from([
            ("session_id".into(), json!(detect_session_id())),
            (
                "fu_id".into(),
                item.get("id").cloned().unwrap_or(Value::Null),
            ),
            (
                "title".into(),
                item.get("title").cloned().unwrap_or(Value::Null),
            ),
            (
                "priority".into(),
                item.get("priority").cloned().unwrap_or(Value::Null),
            ),
        ]),
    );
    if item.get("deduped") == Some(&Value::Bool(true)) {
        eprintln!(
            "deduped: open item {} already covers this; no new fu- minted",
            item.get("id").and_then(Value::as_str).unwrap_or("?")
        );
    }
    println!("{}", serde_json::to_string(&item).unwrap_or_default());
    0
}

fn render_by_type(text: &str, as_json: bool, include_struck: bool, marker: Option<&str>) -> i32 {
    let buckets = bucket_managed_items(parse_managed_items(text, include_struck, marker));
    if as_json {
        println!(
            "{}",
            serde_json::to_string(&Value::Object(buckets)).unwrap_or_default()
        );
        return 0;
    }
    let total: usize = BUCKET_ORDER
        .iter()
        .filter_map(|k| buckets.get(*k))
        .filter_map(|v| v.as_array())
        .map(Vec::len)
        .sum();
    if total == 0 {
        eprintln!("(no managed items)");
        return 0;
    }
    let labels = [
        ("your_actions", "Your actions"),
        ("followups", "Followups (fu-*)"),
        ("carveouts", "Carveouts (cv-*)"),
        ("filed_nodes", "Filed nodes (ab-*)"),
    ];
    for (key, label) in labels {
        let Some(Some(bucket)) = buckets.get(key).map(|v| v.as_array()) else {
            continue;
        };
        let mut label = label.to_string();
        if key == "your_actions" {
            if let Some(marker) = marker {
                label = format!("{label} ({marker})");
            }
        }
        println!("{label}: {}", bucket.len());
        for it in bucket {
            let id = it.get("id").and_then(Value::as_str).unwrap_or("");
            let id_part = if id.is_empty() {
                String::new()
            } else {
                format!("{id}  ")
            };
            let prio = it
                .get("priority")
                .and_then(Value::as_str)
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            let status = match it.get("status").and_then(Value::as_str) {
                Some("open") | None => String::new(),
                Some(s) => format!(" [{s}]"),
            };
            let section = it
                .get("section")
                .and_then(Value::as_str)
                .map(|s| format!("  <{s}>"))
                .unwrap_or_default();
            let title = it.get("title").and_then(Value::as_str).unwrap_or("");
            println!("  {id_part}{title}{prio}{status}{section}");
        }
    }
    0
}

fn cmd_list(rest: &[String]) -> i32 {
    let as_json = has_flag(rest, "--json") || has_flag(rest, "-J");
    let include_struck = has_flag(rest, "--all") || has_flag(rest, "-A");
    let by_type = has_flag(rest, "--by-type");
    let path = inbox_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            eprintln!("error: cannot read inbox {}: {e}", path.display());
            return 1;
        }
    };
    if by_type {
        return render_by_type(
            &text,
            as_json,
            include_struck,
            maintainer_marker().as_deref(),
        );
    }
    let items = parse_items(&text, include_struck);
    for (lineno, raw) in find_unparseable_fu_lines(&text) {
        eprintln!(
            "warning: line {lineno} has a fu- id but is not a parseable item (check the '\u{2014}' separator / checkbox); it is hidden from listings: {raw}"
        );
    }
    let open_count = items
        .iter()
        .filter(|i| i.get("status").and_then(Value::as_str) == Some("open"))
        .count();
    if open_count > SOFT_CEILING {
        eprintln!(
            "warning: {open_count} open inbox items (soft ceiling {SOFT_CEILING}); run `fno backlog capture archive` or triage some out."
        );
    }
    if as_json {
        println!("{}", serde_json::to_string(&items).unwrap_or_default());
        return 0;
    }
    if items.is_empty() {
        eprintln!("(inbox empty)");
        return 0;
    }
    for i in &items {
        let id = i.get("id").and_then(Value::as_str).unwrap_or("");
        let status = i.get("status").and_then(Value::as_str).unwrap_or("");
        let title = i.get("title").and_then(Value::as_str).unwrap_or("");
        let prio = i
            .get("priority")
            .and_then(Value::as_str)
            .map(|p| format!(" ({p})"))
            .unwrap_or_default();
        println!("{id}  [{status}]  {title}{prio}");
    }
    0
}

fn cmd_scan(rest: &[String]) -> i32 {
    let text = match rest.first() {
        Some(path_str) => {
            let path = PathBuf::from(path_str);
            if !path.exists() {
                eprintln!("error: transcript not found: {path_str}");
                return 2;
            }
            match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("error: cannot read transcript {path_str}: {e}");
                    return 2;
                }
            }
        }
        None => {
            let mut buf = String::new();
            if std::io::stdin().read_to_string(&mut buf).is_err() {
                eprintln!("error: cannot read stdin");
                return 2;
            }
            buf
        }
    };
    let candidates = scan_transcript(&text);
    emit(
        "capture_scan",
        Map::from([
            ("session_id".into(), json!(detect_session_id())),
            ("candidates".into(), json!(candidates.len() as i64)),
        ]),
    );
    println!("{}", serde_json::to_string(&candidates).unwrap_or_default());
    0
}

fn cmd_empty_pass(rest: &[String]) -> i32 {
    let reason = match flag_value(rest, "--reason").or_else(|| flag_value(rest, "-R")) {
        Some(r) => r.to_string(),
        None => {
            eprintln!("error: --reason must not be empty (anti-rubber-stamp rule)");
            return 2;
        }
    };
    if reason.trim().is_empty() {
        eprintln!("error: --reason must not be empty (anti-rubber-stamp rule)");
        return 2;
    }
    let session_flag = flag_value(rest, "--session-id");
    let sid = session_flag
        .map(str::to_string)
        .unwrap_or_else(detect_session_id);
    let scan_candidates: i64 = flag_value(rest, "--scan-candidates")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let artifacts_dir = repo_root().join(".fno").join("artifacts");
    let artifact = match write_empty_pass_artifact(&sid, &reason, scan_candidates, &artifacts_dir) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    let emitted = emit(
        "capture_empty_pass",
        Map::from([
            ("session_id".into(), json!(sid)),
            ("reason".into(), json!(reason)),
            ("scan_candidates".into(), json!(scan_candidates)),
        ]),
    );
    // The capture_empty_pass event is the gate's third honesty factor. Some
    // inline emit paths return rc=0 without writing, so read back rather
    // than trust the emit: a missing event means the gate would later be
    // rejected far from here, so fail loudly now.
    let landed = emitted
        && count_session_events(
            &["capture_empty_pass", "inbox_empty_pass"],
            &sid,
            &events_path(),
        ) > 0;
    if !landed {
        eprintln!(
            "error: capture_empty_pass event did not land; the deferrals gate would be rejected later. Check .fno/events.jsonl and re-run."
        );
        return 1;
    }
    println!(
        "{}",
        serde_json::to_string(&json!({
            "session_id": sid,
            "artifact": artifact.to_string_lossy(),
            "entries_written": 0,
        }))
        .unwrap_or_default()
    );
    0
}

fn cmd_capture_pass(rest: &[String]) -> i32 {
    let session_flag = flag_value(rest, "--session-id");
    let sid = session_flag
        .map(str::to_string)
        .unwrap_or_else(detect_session_id);
    let n = count_session_events(&["capture_add", "inbox_add"], &sid, &events_path());
    if n == 0 {
        eprintln!(
            "error: no capture_add events for this session; use 'fno backlog capture empty-pass --reason ...' for an honest empty pass"
        );
        return 2;
    }
    let artifacts_dir = repo_root().join(".fno").join("artifacts");
    let artifact = match write_capture_artifact(&sid, n as i64, &artifacts_dir) {
        Ok(a) => a,
        Err(m) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    println!(
        "{}",
        serde_json::to_string(&json!({
            "session_id": sid,
            "artifact": artifact.to_string_lossy(),
            "entries_written": n as i64,
        }))
        .unwrap_or_default()
    );
    0
}

fn cmd_promote(rest: &[String]) -> i32 {
    let Some(fu_id) = rest.first() else {
        eprintln!("Error: Missing argument 'FU_ID'.");
        return 2;
    };
    let priority = flag_value(rest, "--priority");
    let difficulty_flag = flag_value(rest, "--difficulty");
    let blocks_everything = has_flag(rest, "--blocks-everything");
    let source_kind = flag_value(rest, "--source-kind").unwrap_or("organic");
    if !super::create_cli::SOURCE_KINDS.contains(&source_kind) {
        eprintln!(
            "error: invalid --source-kind '{source_kind}'. Must be one of: {}",
            {
                let mut names: Vec<&str> = super::create_cli::SOURCE_KINDS.to_vec();
                names.sort_unstable();
                names.join(", ")
            }
        );
        return 1;
    }
    let difficulty = match difficulty_flag {
        Some(d) => d.to_string(),
        None => {
            use std::io::IsTerminal;
            if !std::io::stdin().is_terminal() {
                eprintln!(
                    "Error: non-interactive filing requires --difficulty (low, medium, high). {}",
                    super::create_cli::DIFFICULTY_HELP
                );
                return 2;
            }
            match prompt_difficulty() {
                Some(d) => d,
                None => return 2,
            }
        }
    };
    let result = promote_item(
        &inbox_path(),
        fu_id,
        priority,
        &difficulty,
        blocks_everything,
        source_kind,
    );
    let result = match result {
        Ok(r) => r,
        Err(CaptureError::Validation(m)) => {
            eprintln!("error: {m}");
            return 2;
        }
        Err(CaptureError::Lock(m)) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    if result.get("status").and_then(Value::as_str) == Some("promoted") {
        emit(
            "capture_promote",
            Map::from([
                ("fu_id".into(), json!(fu_id)),
                (
                    "node_id".into(),
                    result.get("node_id").cloned().unwrap_or(Value::Null),
                ),
            ]),
        );
    }
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    0
}

/// The interactive difficulty prompt (only reached on a tty). Re-asks on an
/// invalid value, mirroring the wheel's value_proc loop.
fn prompt_difficulty() -> Option<String> {
    loop {
        eprint!("Difficulty (low|medium|high): ");
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        let value = line.trim();
        match super::create_cli::normalize_difficulty(value) {
            Ok(d) => return Some(d),
            Err(r) => eprintln!("{}", r.message),
        }
    }
}

fn cmd_dismiss(rest: &[String]) -> i32 {
    let Some(fu_id) = rest.first() else {
        eprintln!("Error: Missing argument 'FU_ID'.");
        return 2;
    };
    let Some(reason) = flag_value(rest, "--reason").or_else(|| flag_value(rest, "-R")) else {
        eprintln!("error: --reason must not be empty");
        return 2;
    };
    if reason.trim().is_empty() {
        eprintln!("error: --reason must not be empty");
        return 2;
    }
    let result = match dismiss_item(&inbox_path(), fu_id, reason) {
        Ok(r) => r,
        Err(CaptureError::Validation(m)) => {
            eprintln!("error: {m}");
            return 2;
        }
        Err(CaptureError::Lock(m)) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    if result.get("status").and_then(Value::as_str) == Some("dismissed") {
        emit(
            "capture_dismiss",
            Map::from([
                ("fu_id".into(), json!(fu_id)),
                (
                    "reason".into(),
                    result.get("reason").cloned().unwrap_or(Value::Null),
                ),
            ]),
        );
    }
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    0
}

fn cmd_archive(_rest: &[String]) -> i32 {
    match archive_struck(&inbox_path()) {
        Ok(result) => {
            println!("{}", serde_json::to_string(&result).unwrap_or_default());
            0
        }
        Err(CaptureError::Validation(m)) | Err(CaptureError::Lock(m)) => {
            eprintln!("error: {m}");
            1
        }
    }
}

fn cmd_tidy(rest: &[String]) -> i32 {
    let as_json = has_flag(rest, "--json") || has_flag(rest, "-J");
    let include_deferred = has_flag(rest, "--include-deferred");
    let marker = maintainer_marker();
    let result = match tidy_pass(
        &inbox_path(),
        &super::settings::graph_path(),
        include_deferred,
        marker.as_deref(),
    ) {
        Ok(r) => r,
        Err(CaptureError::Validation(m)) => {
            eprintln!("error: {m}");
            return 2;
        }
        Err(CaptureError::Lock(m)) => {
            eprintln!("error: {m}");
            return 1;
        }
    };
    if let Some(warning) = result.get("graph_warning").and_then(Value::as_str) {
        eprintln!("warning: {warning}");
    }
    let ejected = result.get("ejected").and_then(Value::as_i64).unwrap_or(0);
    emit(
        "capture_tidy",
        Map::from([
            ("session_id".into(), json!(detect_session_id())),
            ("ejected".into(), json!(ejected)),
        ]),
    );
    if as_json {
        println!("{}", serde_json::to_string(&result).unwrap_or_default());
        return 0;
    }
    let human_actions = result
        .get("human_actions")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let followups_open = result
        .get("followups_open")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let actions_phrase = match &marker {
        Some(marker) => format!("{human_actions} {marker} action(s)"),
        None => format!("{human_actions} maintainer action(s)"),
    };
    let clusters = result
        .get("dedup_clusters")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    println!(
        "tidy: ejected {ejected} filed node(s), {followups_open} open followup(s), {actions_phrase}, {} dedup cluster(s).",
        clusters.len()
    );
    for cluster in &clusters {
        let ids: Vec<&str> = cluster
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<&str>>())
            .unwrap_or_default();
        eprintln!("  dup: {}", ids.join(", "));
    }
    0
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    const LEGACY_DASH: char = '\u{2014}';

    #[test]
    fn parse_items_filters_struck_and_accepts_both_separators() {
        let text = format!(
            "- [ ] fu-3a8c1f - First (p1)\n\
             - [x] fu-3a8c1e - Promoted (p2) -> node-12345678\n\
             - [-] fu-3a8c1d - Gone (p3) (dismissed: nah)\n\
             - [ ] fu-3a8c1c {LEGACY_DASH} Legacy dash\n"
        );
        let open = parse_items(&text, false);
        assert_eq!(open.len(), 2);
        assert_eq!(open[0].get("id").and_then(Value::as_str), Some("fu-3a8c1f"));
        assert_eq!(open[0].get("priority").and_then(Value::as_str), Some("p1"));
        assert_eq!(open[1].get("id").and_then(Value::as_str), Some("fu-3a8c1c"));
        assert!(open[1].get("priority").and_then(Value::as_str).is_none());
        let all = parse_items(&text, true);
        assert_eq!(all.len(), 4);
        let statuses: Vec<&str> = all
            .iter()
            .filter_map(|i| i.get("status").and_then(Value::as_str))
            .collect();
        assert!(statuses.contains(&"promoted") && statuses.contains(&"dismissed"));
    }

    #[test]
    fn managed_lens_classifies_four_types_and_rejects_malformed_cv() {
        let marker = "#maintainer";
        let text = format!(
            "## Section A\n\
             - [ ] fu-abc123 - Followup (p1)\n\
             - [ ] cv-1a2b3c4d - Carveout\n\
             - [x] node-1234abcd - Node line (p2). post-merge prose here\n\
             - [ ] task with {marker} tag\n\
             - [ ] cv-abc12 - Malformed seven\n\
             prose mentioning fu-abc999 in a paragraph is not an item\n"
        );
        let items = parse_managed_items(&text, true, Some(marker));
        let kinds: Vec<&str> = items
            .iter()
            .filter_map(|i| i.get("type").and_then(Value::as_str))
            .collect();
        assert_eq!(kinds, vec!["followup", "carveout", "node", "human"]);
        let node = items
            .iter()
            .find(|i| i.get("type").and_then(Value::as_str) == Some("node"));
        assert_eq!(
            node.and_then(|n| n.get("priority")).and_then(Value::as_str),
            Some("p2")
        );
        let human = items
            .iter()
            .find(|i| i.get("type").and_then(Value::as_str) == Some("human"));
        assert!(human
            .and_then(|h| h.get("id"))
            .map(Value::is_null)
            .unwrap_or(false));
        assert!(human.and_then(|h| h.get("section")).and_then(Value::as_str) == Some("Section A"));
    }

    #[test]
    fn add_mints_once_then_dedups_on_title_and_where() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = dir.path().join("inbox.md");
        let first = add_item(
            &inbox,
            "Ship the thing",
            "PR #12",
            "because",
            Some("cli/"),
            "p1",
        )
        .expect("first add");
        assert_eq!(first.get("deduped"), Some(&Value::Bool(false)));
        let minted = first
            .get("id")
            .and_then(Value::as_str)
            .expect("minted id")
            .to_string();
        assert_eq!(minted.len(), 9);
        let second = add_item(
            &inbox,
            "SHIP  THE   thing",
            "PR #13",
            "duplicate attempt",
            Some("cli/"),
            "p2",
        )
        .expect("second add");
        assert_eq!(second.get("deduped"), Some(&Value::Bool(true)));
        assert_eq!(
            second.get("id").and_then(Value::as_str),
            Some(minted.as_str())
        );
        let text = std::fs::read_to_string(&inbox).expect("read inbox");
        assert!(text.contains(&minted), "the minted id landed on a line");
        assert!(!text.contains("duplicate attempt"), "dedup must not append");
    }

    #[test]
    fn dismiss_strikes_preserves_and_archives() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = dir.path().join("inbox.md");
        let item = add_item(&inbox, "Temp item", "src", "why", None, "p2").expect("add");
        let fu_id = item
            .get("id")
            .and_then(Value::as_str)
            .expect("id")
            .to_string();
        let again = dismiss_item(&inbox, &fu_id, "not worth it").expect("dismiss");
        assert_eq!(
            again.get("status"),
            Some(&Value::String("dismissed".into()))
        );
        let text = std::fs::read_to_string(&inbox).expect("read");
        assert!(
            text.contains("(dismissed: not worth it)"),
            "the reason is preserved on the line"
        );
        let twice = dismiss_item(&inbox, &fu_id, "other").expect("second dismiss");
        assert_eq!(
            twice.get("status"),
            Some(&Value::String("already_dismissed".into()))
        );
        let result = archive_struck(&inbox).expect("archive");
        assert_eq!(result.get("archived"), Some(&Value::from(1)));
        let kept = std::fs::read_to_string(&inbox).expect("kept");
        assert!(!kept.contains(&fu_id));
        let archive =
            std::fs::read_to_string(dir.path().join("inbox-archive.md")).expect("archive file");
        assert!(archive.contains(&fu_id));
    }

    #[test]
    fn tidy_normalizes_separators_and_is_byte_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = dir.path().join("inbox.md");
        let dash = LEGACY_DASH;
        let seed = format!(
            "---\ntitle: Abilities {dash} backlog inbox\n---\n\n# Abilities {dash} backlog inbox\n\n## 2026-01-01\n\n- [ ] fu-aaa111 {dash} Legacy line (p0)\n  source: somewhere\n"
        );
        std::fs::write(&inbox, &seed).expect("seed");
        let graph = dir.path().join("absent-graph.db");
        let first = tidy_pass(&inbox, &graph, false, None).expect("tidy");
        assert_eq!(first.get("ejected"), Some(&Value::from(0)));
        let once = std::fs::read_to_string(&inbox).expect("once");
        assert!(
            once.contains("- [ ] fu-aaa111 - Legacy line (p0)"),
            "separator normalized to the hyphen"
        );
        assert_eq!(once.matches(DIGEST_START).count(), 1);
        assert_eq!(once.matches(DIGEST_END).count(), 1);
        assert!(once.contains("- p0: 1 (fu-aaa111)"));
        let second = tidy_pass(&inbox, &graph, false, None).expect("tidy again");
        assert_eq!(second.get("ejected"), Some(&Value::from(0)));
        let twice = std::fs::read_to_string(&inbox).expect("twice");
        assert_eq!(
            once, twice,
            "a second tidy reproduces the first byte-for-byte"
        );
    }

    #[test]
    fn unparseable_fu_lines_warn_without_hiding_the_token() {
        let text = format!(
            "- [ ] fu-3a8c1f - Fine\nfu-3a8c1e no checkbox here\n- [ ] fu-3a8c1e Broken (no sep)\n"
        );
        let bad = find_unparseable_fu_lines(&text);
        assert_eq!(bad.len(), 2);
        assert_eq!(bad[0].0, 2);
    }
}
