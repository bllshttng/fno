//! The typed receipt model: build, parse, integrity, filename.
//!
//! Ported from the Python leg's `receipt.py` (the module this family's port
//! deletes). Identity is (node, session, phase, generation, candidate_sha)
//! where candidate_sha is the 12-char git HEAD at write time. One immutable
//! file per identity; a second write refuses.
//!
//! The two invariants the receipt exists to enforce (the deleted module's
//! own words): a receipt is EVIDENCE, not AUTHORITY, and there is no resume
//! database - observations canonicalize the way the scoreboard folds them,
//! dedup by signature, latest by parsed timestamp.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const RECEIPT_VERSION: i64 = 1;

/// Events that, observed for a node AFTER a receipt was written, mean the
/// receipt is stale authority (a later terminal/delegation already happened).
pub(super) const SUPERSEDED_KINDS: [&str; 4] = [
    "termination",
    "session_satisfied",
    "delegated",
    "loop_check",
];

#[derive(Debug, Clone, PartialEq)]
pub enum ReceiptError {
    /// A present-but-corrupt receipt: fail closed, never read as empty
    /// evidence.
    Malformed(String),
    /// A completed receipt already exists for the identity.
    AlreadyExists(String),
    /// A required input failed validation before any file was touched.
    InvalidInput(String),
}

impl std::fmt::Display for ReceiptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReceiptError::Malformed(m)
            | ReceiptError::AlreadyExists(m)
            | ReceiptError::InvalidInput(m) => f.write_str(m),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NextAction {
    pub command: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptIdentity {
    pub node: String,
    pub session: String,
    pub phase: String,
    pub generation: i64,
    pub candidate_sha: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResumeReceipt {
    pub version: i64,
    pub identity: ReceiptIdentity,
    pub repo: String,
    pub worktree: String,
    pub branch: String,
    pub head: String,
    pub next_action: NextAction,
    pub written_at: String,
    pub completed_tasks: Vec<String>,
    pub remaining_tasks: Vec<String>,
    pub open_findings: Vec<String>,
    pub known_reds: Vec<String>,
    pub claims: Vec<Value>,
    pub watchers: Vec<String>,
    /// External-effect keys (pr_create:<sha>, comment:<sha>, merge:<sha>, ...).
    /// A resumed worker checks these before performing the effect so a worker
    /// that died after output but before journaling cannot replay it.
    pub idempotency_keys: Vec<String>,
    pub content_sha: String,
    /// Task-context execution binding when written under one; None = explicitly
    /// UNBOUND (ordinary legacy behavior). Corrupt bindings refuse natively.
    pub task_context: Option<Value>,
}

fn short_sha(sha: &str) -> String {
    sha.replace('\n', "").trim().chars().take(12).collect()
}

/// The wire shape: the JSON object every producer writes and every consumer
/// parses. `next_action` spells its command `verb` on the wire (receipts
/// cross binary ages).
pub fn receipt_to_value(receipt: &ResumeReceipt) -> Value {
    let mut d = Map::new();
    d.insert("version".into(), json!(receipt.version));
    d.insert(
        "identity".into(),
        json!({
            "node": receipt.identity.node,
            "session": receipt.identity.session,
            "phase": receipt.identity.phase,
            "generation": receipt.identity.generation,
            "candidate_sha": receipt.identity.candidate_sha,
        }),
    );
    d.insert("repo".into(), json!(receipt.repo));
    d.insert("worktree".into(), json!(receipt.worktree));
    d.insert("branch".into(), json!(receipt.branch));
    d.insert("head".into(), json!(receipt.head));
    d.insert(
        "next_action".into(),
        json!({
            "verb": receipt.next_action.command, // verb-wording: allow (receipts cross binary ages)
            "target": receipt.next_action.target,
        }),
    );
    d.insert("written_at".into(), json!(receipt.written_at));
    d.insert("completed_tasks".into(), json!(receipt.completed_tasks));
    d.insert("remaining_tasks".into(), json!(receipt.remaining_tasks));
    d.insert("open_findings".into(), json!(receipt.open_findings));
    d.insert("known_reds".into(), json!(receipt.known_reds));
    d.insert("claims".into(), json!(receipt.claims));
    d.insert("watchers".into(), json!(receipt.watchers));
    d.insert("idempotency_keys".into(), json!(receipt.idempotency_keys));
    d.insert("content_sha".into(), json!(receipt.content_sha));
    d.insert(
        "task_context".into(),
        receipt.task_context.clone().unwrap_or(Value::Null),
    );
    Value::Object(d)
}

/// Stable serialization for the content_sha integrity tag: excludes
/// content_sha itself (circular), drops null values (an absent optional field
/// must not change a legacy receipt's digest), sorts keys.
fn canonical_payload(receipt_value: &Value) -> String {
    let Some(obj) = receipt_value.as_object() else {
        return String::new();
    };
    let filtered: Map<String, Value> = obj
        .iter()
        .filter(|(k, v)| k.as_str() != "content_sha" && !v.is_null())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    // BTreeMap-backed serde_json preserves sorted keys when the map feature
    // is preserve-order-less; build the JSON through a sorted writer to be
    // explicit about the contract.
    let sorted = Value::Object(filtered);
    compact_sorted_json(&sorted)
}

/// A JSON string literal with Python's ensure_ascii escaping: non-ASCII
/// escapes to \\uXXXX (lowercase hex, surrogate pairs past the BMP), controls
/// to the short forms, 0x20-0x7E verbatim - `json.dumps` byte parity.
pub fn py_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        let cp = ch as u32;
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            _ if cp < 0x20 || cp > 0x7e => {
                if cp > 0xFFFF {
                    let v = cp - 0x10000;
                    let hi = 0xD800 + (v >> 10);
                    let lo = 0xDC00 + (v & 0x3FF);
                    out.push_str(&format!("\\u{hi:04x}\\u{lo:04x}"));
                } else {
                    out.push_str(&format!("\\u{cp:04x}"));
                }
            }
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// serde_json with the preserve-order feature keeps insertion order, so the
/// canonical form walks the object tree and sorts keys itself.
fn compact_sorted_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        py_json_string(k),
                        compact_sorted_json(&map[k.clone()])
                    )
                })
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(items) => {
            let body: Vec<String> = items.iter().map(compact_sorted_json).collect();
            format!("[{}]", body.join(","))
        }
        Value::String(s) => py_json_string(s),
        other => other.to_string(),
    }
}

pub fn content_sha_of(receipt_value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(canonical_payload(receipt_value).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// A required string field: present, a string, and not blank.
fn req_str(d: &Map<String, Value>, key: &str, origin: &str) -> Result<String, ReceiptError> {
    let v = d.get(key);
    match v.and_then(Value::as_str) {
        Some(s) if !s.trim().is_empty() => Ok(s.to_string()),
        _ => Err(ReceiptError::Malformed(format!(
            "{origin}: missing/empty string field '{key}'"
        ))),
    }
}

/// A required integer field. JSON booleans never pose: serde's as_i64 reads
/// neither true nor false, so the bool guard the Python leg needed is the
/// type system here.
fn req_int(d: &Map<String, Value>, key: &str, origin: &str) -> Result<i64, ReceiptError> {
    d.get(key).and_then(Value::as_i64).ok_or_else(|| {
        ReceiptError::Malformed(format!("{origin}: field '{key}' must be an integer"))
    })
}

fn opt_str(d: &Map<String, Value>, key: &str) -> Option<String> {
    d.get(key).and_then(Value::as_str).map(str::to_string)
}

fn opt_obj(d: &Map<String, Value>, key: &str, origin: &str) -> Result<Option<Value>, ReceiptError> {
    match d.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) if v.is_object() => Ok(Some(v.clone())),
        Some(_) => Err(ReceiptError::Malformed(format!(
            "{origin}: field '{key}' must be an object when present"
        ))),
    }
}

fn str_tuple(d: &Map<String, Value>, key: &str, origin: &str) -> Result<Vec<String>, ReceiptError> {
    match d.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(ReceiptError::Malformed(format!(
                            "{origin}: field '{key}' must be a list of strings"
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(ReceiptError::Malformed(format!(
            "{origin}: field '{key}' must be a list of strings"
        ))),
    }
}

fn claims_tuple(d: &Map<String, Value>, origin: &str) -> Result<Vec<Value>, ReceiptError> {
    match d.get("claims") {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            if items.iter().all(Value::is_object) {
                Ok(items.clone())
            } else {
                Err(ReceiptError::Malformed(format!(
                    "{origin}: field 'claims' must be a list of objects"
                )))
            }
        }
        Some(_) => Err(ReceiptError::Malformed(format!(
            "{origin}: field 'claims' must be a list of objects"
        ))),
    }
}

/// Assemble a receipt from validated inputs and compute its content_sha.
/// Pure (no I/O). `head` becomes the candidate_sha in the identity: a receipt
/// is versioned per (node, session, phase, generation, HEAD), so a new commit
/// at the same phase/generation is a new version, not an overwrite.
#[allow(clippy::too_many_arguments)]
pub fn build_receipt(
    node: &str,
    session: &str,
    phase: &str,
    generation: i64,
    repo: &str,
    worktree: &str,
    branch: &str,
    head: &str,
    next_command: &str,
    next_target: Option<&str>,
    written_at: &str,
    completed_tasks: Vec<String>,
    remaining_tasks: Vec<String>,
    open_findings: Vec<String>,
    known_reds: Vec<String>,
    watchers: Vec<String>,
    idempotency_keys: Vec<String>,
    task_context: Option<Value>,
) -> Result<ResumeReceipt, ReceiptError> {
    if next_command.trim().is_empty() {
        return Err(ReceiptError::Malformed(
            "next_action.command must be a non-empty string".to_string(),
        ));
    }
    if node.trim().is_empty() || session.trim().is_empty() || phase.trim().is_empty() {
        return Err(ReceiptError::Malformed(
            "identity.node/session/phase must be non-empty".to_string(),
        ));
    }
    if generation < 1 {
        return Err(ReceiptError::InvalidInput(format!(
            "identity.generation must be a positive int, got {generation}"
        )));
    }
    if head.trim().is_empty() {
        return Err(ReceiptError::Malformed(
            "identity.candidate_sha must be non-empty".to_string(),
        ));
    }
    let mut receipt = ResumeReceipt {
        version: RECEIPT_VERSION,
        identity: ReceiptIdentity {
            node: node.to_string(),
            session: session.to_string(),
            phase: phase.to_string(),
            generation,
            candidate_sha: short_sha(head),
        },
        repo: repo.to_string(),
        worktree: worktree.to_string(),
        branch: branch.to_string(),
        head: head.to_string(),
        next_action: NextAction {
            command: next_command.to_string(),
            target: next_target.map(str::to_string),
        },
        written_at: written_at.to_string(),
        completed_tasks,
        remaining_tasks,
        open_findings,
        known_reds,
        claims: Vec::new(),
        watchers,
        idempotency_keys,
        content_sha: String::new(),
        task_context,
    };
    let mut value = receipt_to_value(&receipt);
    receipt.content_sha = content_sha_of(&value);
    value["content_sha"] = json!(receipt.content_sha);
    Ok(receipt)
}

pub fn receipt_filename(identity: &ReceiptIdentity) -> String {
    format!(
        "receipt-{}-{}-g{}-{}.json",
        identity.node, identity.phase, identity.generation, identity.candidate_sha
    )
}

pub fn receipt_path(
    identity: &ReceiptIdentity,
    artifacts_dir: &std::path::Path,
) -> std::path::PathBuf {
    artifacts_dir.join(receipt_filename(identity))
}

/// Atomically write an immutable receipt. Refuses to overwrite: tmp-then-
/// rename is atomic on POSIX, and the refuse-overwrite guard makes a
/// completed receipt immutable for its identity. A second write for the same
/// identity is a programming error (re-deriving a completed boundary), not a
/// silent clobber.
pub fn write_receipt(
    receipt: &ResumeReceipt,
    artifacts_dir: &std::path::Path,
) -> Result<std::path::PathBuf, ReceiptError> {
    std::fs::create_dir_all(artifacts_dir).map_err(|e| {
        ReceiptError::InvalidInput(format!("cannot create {}: {e}", artifacts_dir.display()))
    })?;
    let path = receipt_path(&receipt.identity, artifacts_dir);
    if path.exists() {
        // The message embeds the deleted leg's dataclass repr: successors
        // and tests match on it.
        return Err(ReceiptError::AlreadyExists(format!(
            "resume receipt already exists for ReceiptIdentity(node='{}', session='{}', phase='{}', generation={}, candidate_sha='{}'): {} (completed receipts are immutable)",
            receipt.identity.node,
            receipt.identity.session,
            receipt.identity.phase,
            receipt.identity.generation,
            receipt.identity.candidate_sha,
            path.display()
        )));
    }
    let mut value = receipt_to_value(receipt);
    value["content_sha"] = json!(receipt.content_sha);
    // serde_json pretty: 2-space indent with sorted keys, matching the
    // deleted producer's `json.dumps(..., sort_keys=True, indent=2)`.
    let body = format!("{}\n", pretty_sorted_json(&value, 0));
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(ReceiptError::InvalidInput(format!(
            "cannot write {}: {e}",
            path.display()
        )));
    }
    Ok(path)
}

/// `json.dumps(sort_keys=True, indent=2)`: sorted keys, two-space indent,
/// `": "` separators, empty container as `[]`/`{}` (json.dumps prints no
/// inner newline for empties). ensure_ascii escaping throughout.
pub(super) fn pretty_sorted_json(value: &Value, depth: usize) -> String {
    let pad = |n: usize| " ".repeat(n * 2);
    match value {
        Value::Object(map) => {
            if map.is_empty() {
                return "{}".to_string();
            }
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner: Vec<String> = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}{}: {}",
                        pad(depth + 1),
                        py_json_string(k),
                        pretty_sorted_json(&map[k.clone()], depth + 1)
                    )
                })
                .collect();
            format!("{{\n{}\n{}}}", inner.join(",\n"), pad(depth))
        }
        Value::Array(items) => {
            if items.is_empty() {
                return "[]".to_string();
            }
            let inner: Vec<String> = items
                .iter()
                .map(|v| format!("{}{}", pad(depth + 1), pretty_sorted_json(v, depth + 1)))
                .collect();
            format!("[\n{}\n{}]", inner.join(",\n"), pad(depth))
        }
        Value::String(s) => py_json_string(s),
        other => other.to_string(),
    }
}

/// Translate a serde_json parse error into the deleted leg's
/// `json.JSONDecodeError` display shape: `<phrase>: line L column C (char N)`.
/// Positions are byte/character indices, identical on ASCII; receipts are
/// ASCII in practice. Unmapped categories keep serde's own words rather than
/// inventing a shape nothing pins.
pub(crate) fn py_parse_error(raw: &str, e: &serde_json::Error) -> String {
    let text = e.to_string();
    let Some((msg, rest)) = text.split_once(" at line ") else {
        return text;
    };
    let Some((line_s, col_s)) = rest.split_once(" column ") else {
        return text;
    };
    let (Ok(line), Ok(col)) = (line_s.parse::<usize>(), col_s.parse::<usize>()) else {
        return text;
    };
    let phrase = match msg {
        "expected value" | "EOF while parsing a value" => "Expecting value",
        "expected `:`" => "Expecting ':' delimiter",
        "expected `,` or `}`" | "expected `,` or `]`" => "Expecting ',' delimiter",
        "key must be a string" => "Expecting property name enclosed in double quotes",
        "trailing characters" => "Extra data",
        "invalid escape" => "Invalid \\escape",
        m if m.starts_with("control character (") => "Invalid control character at",
        // Python prints a two-position shape here ("Unterminated string
        // starting at: line .. column .. (char ..)"). One position is the
        // honest approximation; nothing pins the second.
        "EOF while parsing a string" => "Unterminated string starting at",
        _ => return text,
    };
    let line_start: usize = raw
        .lines()
        .take(line.saturating_sub(1))
        .map(|l| l.len() + 1)
        .sum();
    let ch = line_start + col;
    format!("{phrase}: line {line} column {} (char {ch})", col + 1)
}

/// Parse + validate a receipt file's bytes. A MISSING file is NOT malformed
/// (the caller distinguishes); a present-but-corrupt file IS malformed and
/// must fail closed, never read as empty evidence.
pub fn load_receipt_bytes(raw: &str, origin: &str) -> Result<ResumeReceipt, ReceiptError> {
    let value: Value = serde_json::from_str(raw).map_err(|e| {
        ReceiptError::Malformed(format!(
            "{origin}: not valid JSON: {}",
            py_parse_error(raw, &e)
        ))
    })?;
    receipt_from_value(&value, origin)
}

pub fn load_receipt(path: &std::path::Path) -> Result<ResumeReceipt, ReceiptError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ReceiptError::Malformed(format!("{}: cannot read: {e}", path.display())))?;
    load_receipt_bytes(&raw, &path.display().to_string())
}

/// Parse a receipt JSON value with the deleted leg's strictness. Integrity:
/// when a content_sha is recorded, the loaded payload must re-derive it.
pub fn receipt_from_value(value: &Value, origin: &str) -> Result<ResumeReceipt, ReceiptError> {
    let Some(d) = value.as_object() else {
        return Err(ReceiptError::Malformed(format!(
            "{origin}: top-level is not an object"
        )));
    };
    // The deleted leg indexed d["identity"]/d["next_action"] directly: an
    // ABSENT key raised KeyError (wrapped as `<origin>: 'identity'`), while a
    // present-but-non-object value fell into the req_str chain and failed on
    // its first field. Both shapes keep that exact precedence here.
    let empty = Map::new();
    let (ident_raw, next_raw) = {
        let absent = |k: &str| ReceiptError::Malformed(format!("{origin}: '{k}'"));
        if !d.contains_key("identity") {
            return Err(absent("identity"));
        }
        if !d.contains_key("next_action") {
            return Err(absent("next_action"));
        }
        (
            d.get("identity")
                .and_then(Value::as_object)
                .unwrap_or(&empty),
            d.get("next_action")
                .and_then(Value::as_object)
                .unwrap_or(&empty),
        )
    };
    let identity = ReceiptIdentity {
        node: req_str(ident_raw, "node", origin)?,
        session: req_str(ident_raw, "session", origin)?,
        phase: req_str(ident_raw, "phase", origin)?,
        generation: req_int(ident_raw, "generation", origin)?,
        candidate_sha: req_str(ident_raw, "candidate_sha", origin)?,
    };
    let next_action = NextAction {
        command: req_str(next_raw, "verb", origin)?,
        target: opt_str(next_raw, "target"),
    };
    let version = req_int(d, "version", origin)?;
    if version != RECEIPT_VERSION {
        // A newer-than-known version is additive (a future writer); an older
        // or junk version is drift. Either way fail closed rather than guess.
        return Err(ReceiptError::Malformed(format!(
            "{origin}: receipt version {version} != supported {RECEIPT_VERSION}"
        )));
    }
    let receipt = ResumeReceipt {
        version,
        identity,
        repo: req_str(d, "repo", origin)?,
        worktree: req_str(d, "worktree", origin)?,
        branch: opt_str(d, "branch").unwrap_or_default(),
        head: req_str(d, "head", origin)?,
        next_action,
        written_at: req_str(d, "written_at", origin)?,
        completed_tasks: str_tuple(d, "completed_tasks", origin)?,
        remaining_tasks: str_tuple(d, "remaining_tasks", origin)?,
        open_findings: str_tuple(d, "open_findings", origin)?,
        known_reds: str_tuple(d, "known_reds", origin)?,
        claims: claims_tuple(d, origin)?,
        watchers: str_tuple(d, "watchers", origin)?,
        idempotency_keys: str_tuple(d, "idempotency_keys", origin)?,
        content_sha: opt_str(d, "content_sha").unwrap_or_default(),
        task_context: opt_obj(d, "task_context", origin)?,
    };
    if !receipt.content_sha.is_empty() {
        let mut value = receipt_to_value(&receipt);
        value["content_sha"] = json!(receipt.content_sha);
        let recomputed = content_sha_of(&value);
        if recomputed != receipt.content_sha {
            return Err(ReceiptError::Malformed(format!(
                "{origin}: content_sha mismatch (payload tampered or rewritten)"
            )));
        }
    }
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ResumeReceipt {
        build_receipt(
            "x-1",
            "sess-1",
            "do",
            2,
            "fno",
            "/tmp/wt",
            "feature/x",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "/fno:ship pr check 12",
            Some("x-1"),
            "2026-10-24T12:00:00Z",
            vec!["1.1".into()],
            vec!["1.2".into()],
            vec![],
            vec![],
            vec![],
            vec![],
            None,
        )
        .unwrap()
    }

    #[test]
    fn build_stamps_a_verifiable_content_sha() {
        let receipt = sample();
        assert_eq!(receipt.identity.candidate_sha, "aaaaaaaaaaaa");
        let mut value = receipt_to_value(&receipt);
        value["content_sha"] = json!(receipt.content_sha);
        assert_eq!(content_sha_of(&value), receipt.content_sha);
    }

    #[test]
    fn parse_round_trips_and_integrity_refuses_tamper() {
        let receipt = sample();
        let mut value = receipt_to_value(&receipt);
        value["content_sha"] = json!(receipt.content_sha);
        let body = pretty_sorted_json(&value, 0);
        let parsed = load_receipt_bytes(&body, "<test>").unwrap();
        assert_eq!(parsed, receipt);

        let mut tampered = value.clone();
        tampered["repo"] = json!("other");
        tampered["content_sha"] = json!(receipt.content_sha);
        let err = load_receipt_bytes(&pretty_sorted_json(&tampered, 0), "<test>").unwrap_err();
        assert!(matches!(err, ReceiptError::Malformed(m) if m.contains("content_sha mismatch")));
    }

    #[test]
    fn write_refuses_an_immutable_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let receipt = sample();
        let first = write_receipt(&receipt, tmp.path()).unwrap();
        assert!(first.exists());
        let second = write_receipt(&receipt, tmp.path()).unwrap_err();
        assert!(matches!(second, ReceiptError::AlreadyExists(_)));
    }

    #[test]
    fn generation_zero_and_blank_command_refuse() {
        let err = build_receipt(
            "x-1",
            "s",
            "do",
            0,
            "r",
            "w",
            "b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "cmd",
            None,
            "2026-10-24T12:00:00Z",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ReceiptError::InvalidInput(_)));
        let err = build_receipt(
            "x-1",
            "s",
            "do",
            1,
            "r",
            "w",
            "b",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "  ",
            None,
            "2026-10-24T12:00:00Z",
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            vec![],
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ReceiptError::Malformed(_)));
    }
}
