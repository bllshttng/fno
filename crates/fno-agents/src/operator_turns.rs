//! The operator turn queue: which of this session's operator turns are still
//! undispositioned?
//!
//! The transcript is the ingest source, not the system of record: every turn
//! is held here as it is seen, keyed by turn id, and the ack ledger
//! (`<capture-dir>/<session>.jsonl`, written by `fno inbox user ack`)
//! disposes them. A per-session scan cursor
//! (`<capture-dir>/<session>.scan.json`, a deletable cache) remembers how far
//! the last read got, so no turn is lost to distance from EOF and later reads
//! parse only appended bytes.
//!
//! The reader rides the `compaction` action (as its `operator-turns`
//! sub-action) because the client action list may only shrink, and compaction
//! already reads one session's transcript and owns session-keyed state files.

use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use sha2::Sha256;

// The classifier is the promoted `provenance` module: same functions, same
// reason names, so the queue's skip counters and its 17 regression tests are
// unchanged by the move.
use crate::provenance::{classify, is_user_turn, turn_text, turn_ts_epoch};

/// A held turn in the scan cursor; the payload adds the rendered excerpt.
#[derive(Debug, Clone, Serialize)]
struct StoredTurn {
    turn_id: String,
    ts_epoch: Option<f64>,
    text: String,
}

#[derive(Debug, Serialize)]
struct QueuePayload {
    depth: usize,
    oldest_age_s: Option<i64>,
    oldest_excerpt: Option<String>,
    oldest_turn_id: Option<String>,
    skipped: BTreeMap<String, u64>,
    turns: Vec<PayloadTurn>,
    cursor_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct PayloadTurn {
    turn_id: String,
    ts_epoch: Option<f64>,
    text: String,
    excerpt: String,
    stand_down: bool,
}

struct QueueOutcome {
    payload: QueuePayload,
    /// Exact stderr lines the CLI entry echoes.
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ScanState {
    transcript: String,
    offset: u64,
    line_no: u64,
    head_sha256: String,
    turns: Vec<StoredTurn>,
    skipped: BTreeMap<String, u64>,
}

/// The excerpt width. Retained beside the cursor machinery the queue owns.
const EXCERPT_CHARS: usize = 160;
const HEAD_SAMPLE_BYTES: u64 = 4096;

fn scan_path(capture_dir: &Path, session: &str) -> PathBuf {
    capture_dir.join(format!("{session}.scan.json"))
}

fn ledger_path(capture_dir: &Path, session: &str) -> PathBuf {
    capture_dir.join(format!("{session}.jsonl"))
}

fn stand_down_re() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)overstay|stand(ing)?[ -]down|step(ping)?[ -]down|step_down|(end|stop) (the|your|this) lead|compact\w* (\w+ ){0,3}(degraded|diminish)",
        )
        .expect("valid pattern")
    })
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// sha256 over the first `min(offset, 4096)` bytes of `prefix`, the head
/// sample read once per invocation: the rotation tripwire. An in-place
/// rewrite at the same or larger size invalidates the cursor.
fn head_digest(prefix: &[u8], offset: u64) -> String {
    let cut = (offset as usize).min(prefix.len());
    let mut h = Sha256::new();
    h.update(&prefix[..cut]);
    to_hex(&h.finalize())
}

/// A stable ledger id: the row's own uuid/id, else a digest. The digest folds
/// the row's line number in, so byte-identical duplicate rows still get
/// distinct ids and one ack can never dispose two turns. sha1 (not sha256)
/// keeps every derived id an existing ledger already acked, and the absent
/// timestamp renders as the literal `None` exactly as the Python seed did.
fn turn_id(obj: &Value, text: &str, line_no: usize) -> String {
    for key in ["uuid", "id"] {
        if let Some(Value::String(s)) = obj.get(key) {
            let t = s.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
    }
    let ts = match obj.get("timestamp") {
        Some(Value::String(s)) => s.clone(),
        _ => "None".to_string(),
    };
    let mut h = Sha1::new();
    h.update(format!("{line_no}:{ts}:{text}").as_bytes());
    let hex = to_hex(&h.finalize());
    format!("derived-{}", &hex[..12])
}

/// One-line excerpt; whitespace runs collapse so a row stays one row.
fn excerpt(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= EXCERPT_CHARS {
        flat
    } else {
        let cut: String = flat.chars().take(EXCERPT_CHARS - 1).collect();
        format!("{cut}\u{2026}")
    }
}

fn fresh_state(transcript: &Path) -> ScanState {
    ScanState {
        transcript: transcript.display().to_string(),
        offset: 0,
        line_no: 0,
        head_sha256: String::new(),
        turns: Vec::new(),
        skipped: BTreeMap::new(),
    }
}

/// The saved cursor, else fresh: missing, invalid, wrong-typed, alien,
/// past-EOF, head-mismatched, or head-less all reset. `head_prefix` is the
/// transcript's first `min(size, 4096)` bytes, read once by the caller.
fn load_state(
    path: &Path,
    transcript_str: &str,
    head_prefix: &[u8],
    transcript_len: u64,
) -> ScanState {
    let fresh = fresh_state(Path::new(transcript_str));
    let Ok(raw) = std::fs::read_to_string(path) else {
        return fresh;
    };
    let Ok(saved) = serde_json::from_str::<Value>(&raw) else {
        return fresh;
    };
    let Some(obj) = saved.as_object() else {
        return fresh;
    };
    if obj.get("transcript").and_then(|v| v.as_str()) != Some(transcript_str) {
        return fresh;
    }
    let Some(offset) = obj.get("offset").and_then(|v| v.as_u64()) else {
        return fresh;
    };
    if offset > transcript_len {
        return fresh;
    }
    let Some(line_no) = obj.get("line_no").and_then(|v| v.as_u64()) else {
        return fresh;
    };
    let Some(Value::Array(raw_turns)) = obj.get("turns") else {
        return fresh;
    };
    let Some(Value::Object(raw_skipped)) = obj.get("skipped") else {
        return fresh;
    };
    let Some(head) = obj.get("head_sha256").and_then(|v| v.as_str()) else {
        return fresh;
    };
    if head != head_digest(head_prefix, offset) {
        return fresh;
    }
    let mut skipped = BTreeMap::new();
    for (k, v) in raw_skipped {
        if let Some(n) = v.as_u64() {
            skipped.insert(k.clone(), n);
        }
    }
    let turns = raw_turns
        .iter()
        .filter_map(|t| {
            let o = t.as_object()?;
            let turn_id = o.get("turn_id")?.as_str()?.to_string();
            Some(StoredTurn {
                turn_id,
                ts_epoch: o.get("ts_epoch").and_then(|v| v.as_f64()),
                text: o
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            })
        })
        .collect();
    ScanState {
        transcript: transcript_str.to_string(),
        offset,
        line_no,
        head_sha256: head.to_string(),
        turns,
        skipped,
    }
}

/// Atomic best-effort save; failure costs speed on the next read, never
/// correctness.
fn save_state(path: &Path, state: &ScanState) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("no parent directory for {}", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let body = serde_json::to_string(state).map_err(|e| format!("state encode failed: {e}"))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("scan"),
        std::process::id()
    ));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        f.write_all(body.as_bytes())
            .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot rename into place: {e}"))
}

/// Acked turn ids; an absent ledger means none have been acked, while malformed
/// or unreadable ledgers are errors.
fn read_acked_turn_ids(path: &Path) -> Result<HashSet<String>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(HashSet::new()),
        Err(error) => return Err(format!("ack ledger {} unreadable: {error}", path.display())),
    };
    let mut acked = HashSet::new();
    for (line_no, line) in raw.lines().enumerate() {
        let row = serde_json::from_str::<Value>(line).map_err(|error| {
            format!(
                "ack ledger {} line {} malformed: {error}",
                path.display(),
                line_no + 1
            )
        })?;
        if let Some(id) = row.get("turn_id").and_then(|v| v.as_str()) {
            acked.insert(id.to_string());
        }
    }
    Ok(acked)
}

fn build_payload(state: ScanState, now_epoch: f64, cursor_error: Option<String>) -> QueuePayload {
    let turns: Vec<PayloadTurn> = state
        .turns
        .iter()
        .map(|t| PayloadTurn {
            turn_id: t.turn_id.clone(),
            ts_epoch: t.ts_epoch,
            text: t.text.clone(),
            excerpt: excerpt(&t.text),
            stand_down: stand_down_re().is_match(&t.text),
        })
        .collect();
    let oldest = state.turns.first();
    QueuePayload {
        depth: turns.len(),
        oldest_age_s: oldest.and_then(|t| t.ts_epoch.map(|ts| ((now_epoch - ts) as i64).max(0))),
        oldest_excerpt: oldest.map(|t| excerpt(&t.text)),
        oldest_turn_id: oldest.map(|t| t.turn_id.clone()),
        skipped: state.skipped,
        turns,
        cursor_error,
    }
}

/// The pending turns (oldest first) and skip tally. # ponytail: the first
/// read is one full pass (1.07s at 220MB) with no mid-scan checkpoint -
/// checkpoint every 64MB in the loop if a transcript ever nears the hook's
/// 10s budget. A race is safe: two readers starting from the same offset save
/// in either order, and each saved state holds every turn in `[0, offset)`,
/// so a stale save only makes the next read parse more bytes.
fn read_queue(
    session: &str,
    transcript: &Path,
    capture_dir: &Path,
    now_epoch: f64,
) -> Result<QueueOutcome, String> {
    let unreadable =
        |e: std::io::Error| format!("transcript {} unreadable: {e}", transcript.display());
    let mut file = std::fs::File::open(transcript).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    // One head sample serves every rotation check this read makes.
    let want = size.min(HEAD_SAMPLE_BYTES) as usize;
    let mut head = vec![0u8; want];
    file.read_exact(&mut head).map_err(unreadable)?;
    let spath = scan_path(capture_dir, session);
    let transcript_str = transcript.display().to_string();
    let mut state = load_state(&spath, &transcript_str, &head, size);
    file.seek(SeekFrom::Start(state.offset))
        .map_err(unreadable)?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw).map_err(unreadable)?;
    if let Some(cut) = raw.iter().rposition(|&b| b == b'\n') {
        let kept = &raw[..cut + 1];
        let rows: Vec<&[u8]> = kept[..cut].split(|&b| b == b'\n').collect();
        let known: HashSet<String> = state.turns.iter().map(|t| t.turn_id.clone()).collect();
        for (i, row) in rows.iter().enumerate() {
            let Ok(obj) = serde_json::from_str::<Value>(&String::from_utf8_lossy(row)) else {
                continue;
            };
            if !obj.is_object() || !is_user_turn(&obj) {
                continue;
            }
            let text = match classify(&turn_text(&obj)) {
                Ok(t) => t,
                Err(reason) => {
                    *state.skipped.entry(reason.to_string()).or_insert(0) += 1;
                    continue;
                }
            };
            let id = turn_id(&obj, &text, state.line_no as usize + i);
            if known.contains(&id) {
                continue;
            }
            state.turns.push(StoredTurn {
                turn_id: id,
                ts_epoch: turn_ts_epoch(&obj),
                text,
            });
        }
        state.offset += kept.len() as u64;
        state.line_no += rows.len() as u64;
    }
    // A torn trailing row waits for its newline above, so a read never parses
    // a half-written row and derived turn ids stay stable across reads.
    let acked = read_acked_turn_ids(&ledger_path(capture_dir, session))?;
    state.turns.retain(|t| !acked.contains(&t.turn_id));
    state.head_sha256 = head_digest(&head, state.offset);
    let mut warnings = Vec::new();
    let mut cursor_error = None;
    if let Err(e) = save_state(&spath, &state) {
        cursor_error = Some(format!("{}: {e}", spath.display()));
        warnings.push(format!(
            "fno-agents compaction operator-turns: scan cursor not saved at {}: {e}",
            spath.display()
        ));
    }
    Ok(QueueOutcome {
        payload: build_payload(state, now_epoch, cursor_error),
        warnings,
    })
}

pub(crate) fn pending_stand_down(
    session: &str,
    transcript: &Path,
    capture_dir: &Path,
    now_epoch: f64,
) -> Result<Vec<(String, String)>, String> {
    let outcome = read_queue(session, transcript, capture_dir, now_epoch)?;
    Ok(outcome
        .payload
        .turns
        .into_iter()
        .filter(|turn| turn.stand_down)
        .map(|turn| (turn.turn_id, turn.excerpt))
        .collect())
}

pub(crate) fn capture_dir(cwd: &Path) -> Option<PathBuf> {
    std::env::var_os("FNO_OPERATOR_CAPTURE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::agents_config::state_dir(cwd).map(|dir| dir.join("operator-capture")))
}

/// One operator session resolved for a queue read or an ack.
pub(crate) struct OperatorSession {
    pub(crate) session: String,
    pub(crate) transcript: PathBuf,
    pub(crate) capture_dir: PathBuf,
}

pub(crate) enum ResolveError {
    NoIdentity,
    NoTranscript {
        session: String,
        harness: String,
        resolved: Option<PathBuf>,
    },
    Other(String),
}

impl ResolveError {
    /// The library wording; the backlog origin refusal goldens pin it.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::NoIdentity => "no resolvable session identity".to_string(),
            Self::NoTranscript {
                resolved: Some(path),
                ..
            } => format!("transcript {} unreadable: not a file", path.display()),
            Self::NoTranscript {
                session, harness, ..
            } => format!("no transcript found for {harness} session {session}"),
            Self::Other(message) => message.clone(),
        }
    }

    /// The `fno inbox user` wording, byte-equal to the retired Python leg.
    fn cli_message(&self) -> String {
        match self {
            Self::NoIdentity => "no resolvable session identity (FNO_OPERATOR_SESSION_ID or a harness session)".to_string(),
            Self::NoTranscript {
                session,
                harness,
                resolved,
            } => format!(
                "no readable transcript for session {session} ({harness}); resolved to {} - set FNO_OPERATOR_TRANSCRIPT",
                resolved
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "nothing".to_string())
            ),
            Self::Other(message) => message.clone(),
        }
    }
}

/// The session id and capture dir behind a queue read or an ack.
fn operator_identity<'a>(
    pin: &impl Fn(&str) -> Option<String>,
    ambient: &impl Fn() -> &'a crate::spawn_context::OwnedIdentity,
    cwd: &Path,
) -> Result<(String, PathBuf), ResolveError> {
    let session = pin("FNO_OPERATOR_SESSION_ID")
        .or_else(|| ambient().session_id.clone())
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or(ResolveError::NoIdentity)?;
    let capture_dir = pin("FNO_OPERATOR_CAPTURE_DIR")
        .map(PathBuf::from)
        .or_else(|| capture_dir(cwd))
        .ok_or_else(|| {
            ResolveError::Other("no operator capture directory could be resolved".to_string())
        })?;
    Ok((session, capture_dir))
}

fn trimmed_pin(get: &impl Fn(&str) -> Option<String>, key: &str) -> Option<String> {
    get(key)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// The ack's session and capture dir. The ledger outlives the transcript,
/// so an ack needs no transcript.
pub(crate) fn resolve_ack_target(
    get: &impl Fn(&str) -> Option<String>,
    home: &crate::paths::AgentsHome,
    cwd: &Path,
) -> Result<(String, PathBuf), ResolveError> {
    let identity = std::cell::OnceCell::new();
    let ambient = || {
        identity.get_or_init(|| crate::spawn_context::resolve_self_identity(get, None, None, home))
    };
    operator_identity(&|key: &str| trimmed_pin(get, key), &ambient, cwd)
}

/// The one operator session resolver: the `FNO_OPERATOR_*` pins first, then
/// the ambient identity, which is resolved only when a pin is missing.
pub(crate) fn resolve_operator_session(
    get: &impl Fn(&str) -> Option<String>,
    home: &crate::paths::AgentsHome,
    cwd: &Path,
) -> Result<OperatorSession, ResolveError> {
    let pin = |key: &str| trimmed_pin(get, key);
    let identity = std::cell::OnceCell::new();
    let ambient = || {
        identity.get_or_init(|| crate::spawn_context::resolve_self_identity(get, None, None, home))
    };
    let (session, capture_dir) = operator_identity(&pin, &ambient, cwd)?;
    let harness = || {
        pin("FNO_OPERATOR_HARNESS")
            .or_else(|| ambient().harness.clone())
            .unwrap_or_else(|| "claude".to_string())
    };
    let found = match pin("FNO_OPERATOR_TRANSCRIPT") {
        Some(path) => Some(PathBuf::from(path)),
        None => match harness().as_str() {
            "claude" => crate::claude_drive::find_transcript(&session),
            "codex" => crate::codex_store::codex_rollout_path(None, &session),
            other => {
                return Err(ResolveError::Other(format!(
                    "harness {other} keeps no transcript file"
                )))
            }
        },
    };
    let transcript = match found {
        Some(path) if path.is_file() => path,
        resolved => {
            return Err(ResolveError::NoTranscript {
                session,
                harness: harness(),
                resolved,
            })
        }
    };
    Ok(OperatorSession {
        session,
        transcript,
        capture_dir,
    })
}

pub(crate) fn session_queue_depth(
    get: &impl Fn(&str) -> Option<String>,
    home: &crate::paths::AgentsHome,
    cwd: &Path,
    now_epoch: f64,
) -> Result<usize, String> {
    let resolved = resolve_operator_session(get, home, cwd).map_err(|e| e.message())?;
    read_queue(
        &resolved.session,
        &resolved.transcript,
        &resolved.capture_dir,
        now_epoch,
    )
    .map(|outcome| outcome.payload.depth)
}

/// The status view: the queue payload without its turns, in the Python key
/// order the capture hook and its readers parse.
#[derive(Serialize)]
struct StatusView<'a> {
    depth: usize,
    oldest_age_s: Option<i64>,
    oldest_excerpt: Option<&'a str>,
    oldest_turn_id: Option<&'a str>,
    skipped: &'a BTreeMap<String, u64>,
    cursor_error: Option<&'a str>,
}

/// Python's `json.dumps(value, indent=2)`: serde's pretty form with every
/// non-ASCII char escaped, the way the default `ensure_ascii` writes it.
pub(crate) fn python_json(value: &impl Serialize) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_default();
    let mut out = String::with_capacity(pretty.len());
    for ch in pretty.chars() {
        if ch.is_ascii() && ch != '\u{7f}' {
            out.push(ch);
        } else {
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04x}"));
            }
        }
    }
    out
}

/// One line naming every refused shape and count; `None` when none.
fn skip_report(skipped: &BTreeMap<String, u64>) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let total: u64 = skipped.values().sum();
    let detail = skipped
        .iter()
        .map(|(reason, n)| format!("{reason}={n}"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("skipped {total} machine turn(s): {detail}"))
}

fn render_status(payload: &QueuePayload, json_out: bool) {
    if json_out {
        let view = StatusView {
            depth: payload.depth,
            oldest_age_s: payload.oldest_age_s,
            oldest_excerpt: payload.oldest_excerpt.as_deref(),
            oldest_turn_id: payload.oldest_turn_id.as_deref(),
            skipped: &payload.skipped,
            cursor_error: payload.cursor_error.as_deref(),
        };
        println!("{}", python_json(&view));
        return;
    }
    if let Some(report) = skip_report(&payload.skipped) {
        eprintln!("{report}");
    }
    if payload.depth == 0 {
        println!("user queue: 0");
        return;
    }
    let age = payload
        .oldest_age_s
        .map(|age| format!(", oldest {age}s old"))
        .unwrap_or_default();
    println!("user queue: {} undispositioned turn(s){age}", payload.depth);
    if let Some(oldest) = payload.oldest_excerpt.as_deref().filter(|e| !e.is_empty()) {
        println!("  oldest: {oldest}");
    }
}

/// `fno inbox user list`: the pending turns, oldest first, cut to `limit`.
fn render_list(payload: &QueuePayload, json_out: bool, limit: Option<usize>, now_epoch: f64) {
    if let Some(report) = skip_report(&payload.skipped) {
        eprintln!("{report}");
    }
    let turns = &payload.turns[..limit.map_or(payload.turns.len(), |n| n.min(payload.turns.len()))];
    if json_out {
        println!("{}", python_json(&turns));
        return;
    }
    if turns.is_empty() {
        println!("no undispositioned user turns");
        return;
    }
    for turn in turns {
        let age = turn
            .ts_epoch
            .map(|ts| format!("{}s", (now_epoch - ts) as i64))
            .unwrap_or_else(|| "age-unknown".to_string());
        let marker = if turn.stand_down { "[stand-down] " } else { "" };
        println!("{}\t{age}\t{marker}{}", turn.turn_id, turn.excerpt);
    }
}

/// CLI entry: `fno-agents compaction operator-turns [--session <id>]
/// [--transcript <path>] [--capture-dir <dir>] [--render status|list [--json]
/// [--limit <n>]]`. A missing flag resolves like its `FNO_OPERATOR_*` pin,
/// then from the ambient identity. Without `--render` stdout is the raw
/// queue payload; `--render` prints the `fno inbox user` answer the front
/// door execs this for.
pub fn run(args: &[String]) -> i32 {
    const USAGE: &str = "usage: compaction operator-turns [--session <id>] [--transcript <path>] [--capture-dir <dir>] [--render status|list [--json] [--limit <n>]]";
    let mut json_out = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session" | "--transcript" | "--capture-dir" | "--render" | "--limit" | "-L" => {
                it.next();
            }
            "--json" | "-J" => json_out = true,
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let flag = |name: &str| crate::compaction::flag_value(args, name);
    let render = flag("--render");
    if render
        .as_deref()
        .is_some_and(|mode| mode != "status" && mode != "list")
    {
        eprintln!("{USAGE}");
        return 2;
    }
    let limit = match flag("--limit").or_else(|| flag("-L")) {
        None => None,
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if n >= 1 => Some(n),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        },
    };
    let get = |key: &str| {
        let pinned = match key {
            "FNO_OPERATOR_SESSION_ID" => flag("--session"),
            "FNO_OPERATOR_TRANSCRIPT" => flag("--transcript"),
            "FNO_OPERATOR_CAPTURE_DIR" => flag("--capture-dir"),
            _ => None,
        };
        pinned.or_else(|| std::env::var(key).ok())
    };
    let home = crate::paths::AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_default();
    let resolved = match resolve_operator_session(&get, &home, &cwd) {
        Ok(resolved) => resolved,
        Err(e) if render.is_some() => {
            eprintln!("error: {}", e.cli_message());
            return 1;
        }
        Err(e) => {
            eprintln!("{}", e.message());
            return 1;
        }
    };
    let session = resolved.session;
    // The id lands in a path join; a crafted id must not escape the capture dir.
    if session.contains('/') || session.contains('\\') || session.contains("..") {
        eprintln!(
            "compaction operator-turns: the session id must be a bare id (no '/', '\\', '..'): {session}"
        );
        return 2;
    }
    let now_epoch = chrono::Utc::now().timestamp_millis() as f64 / 1000.0;
    let outcome = match read_queue(
        &session,
        &resolved.transcript,
        &resolved.capture_dir,
        now_epoch,
    ) {
        Ok(outcome) => outcome,
        Err(e) if render.is_some() => {
            eprintln!("error: operator turn reader failed: {e}");
            return 1;
        }
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    if render.is_none() {
        for line in &outcome.warnings {
            eprintln!("{line}");
        }
        println!(
            "{}",
            serde_json::to_string(&outcome.payload).unwrap_or_default()
        );
        return 0;
    }
    if let Some(error) = outcome.payload.cursor_error.as_deref() {
        eprintln!("warning: scan cursor not saved: {error}");
    }
    if render.as_deref() == Some("list") {
        render_list(&outcome.payload, json_out, limit, now_epoch);
    } else {
        render_status(&outcome.payload, json_out);
    }
    0
}

/// CLI entry: `fno-agents compaction ack <turn> --outcome <o> [--why <w>]`,
/// the native answer behind `fno inbox user ack`. `--session`, `--turn` and
/// `--capture-dir` pin what the resolver would otherwise find. The row and
/// the refusals print as the retired Python command printed them.
pub fn run_ack(args: &[String]) -> i32 {
    const USAGE: &str = "usage: compaction ack <turn> --outcome <o> [--why <w>] [--session <id>] [--capture-dir <dir>]";
    let mut positional: Vec<&String> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session" | "--turn" | "--outcome" | "--why" | "--capture-dir" => {
                it.next();
            }
            word if !word.starts_with('-') => positional.push(arg),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    let flag = |name: &str| crate::compaction::flag_value(args, name);
    let turn = match (positional.as_slice(), flag("--turn")) {
        ([word], None) => word.trim().to_string(),
        ([], Some(turn)) => turn,
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let Some(outcome) = flag("--outcome") else {
        eprintln!("{USAGE}");
        return 2;
    };
    let get = |key: &str| {
        let pinned = match key {
            "FNO_OPERATOR_SESSION_ID" => flag("--session"),
            "FNO_OPERATOR_CAPTURE_DIR" => flag("--capture-dir"),
            _ => None,
        };
        pinned.or_else(|| std::env::var(key).ok())
    };
    let home = crate::paths::AgentsHome::from_env();
    let cwd = std::env::current_dir().unwrap_or_default();
    let (session, capture_dir) = match resolve_ack_target(&get, &home, &cwd) {
        Ok(target) => target,
        Err(e) => {
            eprintln!("error: {}", e.cli_message());
            return 1;
        }
    };
    // The id lands in a path join; a crafted id must not escape the capture dir.
    if session.contains('/') || session.contains('\\') || session.contains("..") {
        eprintln!(
            "compaction ack: the session id must be a bare id (no '/', '\\', '..'): {session}"
        );
        return 2;
    }
    let why = flag("--why").unwrap_or_default();
    match ack_turn(&home, &capture_dir, &session, &turn, &outcome, &why) {
        Ok(row) => {
            println!(
                "{}",
                crate::backlog::maintenance::validity::py_dumps(&row, false, true)
            );
            0
        }
        Err(e) => {
            eprintln!("error: the ack write failed: {e}");
            1
        }
    }
}

/// The write behind `fno inbox user ack`. The kinds are law, capture, node
/// and answer; `nothing` stands alone. An answer ack also records one
/// `user_ask_answered` row in the machine question index, which is what the
/// attention arm folds into the file sink as a closed line.
pub fn ack_turn(
    home: &crate::paths::AgentsHome,
    capture_dir: &Path,
    session: &str,
    turn_id: &str,
    outcome: &str,
    why: &str,
) -> Result<Value, String> {
    const KINDS: [&str; 4] = ["law", "capture", "node", "answer"];
    let (kind, ref_part) = match outcome.trim().split_once(':') {
        Some((k, r)) => (k.trim().to_string(), r.trim().to_string()),
        None => (outcome.trim().to_string(), String::new()),
    };
    let outcome = if kind == "nothing" && ref_part.is_empty() {
        "nothing".to_string()
    } else if KINDS.contains(&kind.as_str()) && !ref_part.is_empty() {
        outcome.trim().to_string()
    } else {
        let legal = KINDS
            .iter()
            .map(|k| format!("{k}:<ref>"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "invalid --outcome {outcome:?}. Must be nothing or {legal}"
        ));
    };
    let row = json!({
        "turn_id": turn_id,
        "ts": chrono::Utc::now().to_rfc3339(),
        "outcome": outcome,
        "ref": if ref_part.is_empty() { None } else { Some(ref_part.clone()) },
        "why": if why.trim().is_empty() { None } else { Some(why.trim().to_string()) },
    });
    let path = ledger_path(capture_dir, session);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(f, "{row}").map_err(|e| e.to_string())?;

    if kind == "answer" {
        let answered = json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "type": "user_ask_answered",
            "source": "target",
            "data": {
                "session_id": session,
                "turn_id": turn_id,
                "excerpt": excerpt_of(capture_dir, session, turn_id),
                "answer": ref_part,
                "answered_at": chrono::Utc::now().to_rfc3339(),
            }
        });
        let _ = crate::provider_cap::append_questions_row(
            &crate::provider_cap::questions_path(home),
            &answered,
        );
    }
    Ok(row)
}

/// Best-effort: the scan cursor's stored text for the acked turn, cut to the
/// excerpt width. A rotated or never-scanned turn answers empty.
fn excerpt_of(capture_dir: &Path, session: &str, turn_id: &str) -> String {
    let Ok(raw) = std::fs::read_to_string(scan_path(capture_dir, session)) else {
        return String::new();
    };
    let Ok(v) = serde_json::from_str::<Value>(&raw) else {
        return String::new();
    };
    v.get("turns")
        .and_then(Value::as_array)
        .and_then(|turns| {
            turns
                .iter()
                .find(|t| t.get("turn_id").and_then(Value::as_str) == Some(turn_id))
        })
        .and_then(|t| t.get("text").and_then(Value::as_str))
        .map(|text| text.chars().take(EXCERPT_CHARS).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A fixed "now": 2026-09-06T21:00:00Z, the fixture rows' timestamp.
    const NOW: f64 = 1788728400.0;

    fn tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fno-operator-turns-test-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn user_row(text: Value, uuid: &str) -> Value {
        user_row_ts(text, uuid, "2026-09-06T21:00:00.000Z")
    }

    fn user_row_ts(text: Value, uuid: &str, ts: &str) -> Value {
        json!({"type": "user", "uuid": uuid, "timestamp": ts,
               "message": {"role": "user", "content": text}})
    }

    fn write_jsonl(path: &Path, rows: &[Value]) {
        let body = rows
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(path, body).unwrap();
    }

    fn append(path: &Path, extra: &str) {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        f.write_all(extra.as_bytes()).unwrap();
    }

    fn read(dir: &Path, transcript: &Path) -> QueueOutcome {
        read_queue("s", transcript, dir, NOW).unwrap()
    }

    fn ids(out: &QueueOutcome) -> Vec<String> {
        out.payload
            .turns
            .iter()
            .map(|t| t.turn_id.clone())
            .collect()
    }

    fn read_raw(name: &str, rows: &[Value]) -> QueueOutcome {
        let dir = tmp_dir(name);
        let tp = dir.join("transcript.jsonl");
        write_jsonl(&tp, rows);
        read(&dir, &tp)
    }

    fn pinned_operator_turns(
        dir: &Path,
        transcript: &Path,
    ) -> std::collections::HashMap<String, String> {
        std::collections::HashMap::from([
            ("FNO_OPERATOR_SESSION_ID".into(), "fixture-session".into()),
            ("FNO_OPERATOR_HARNESS".into(), "claude".into()),
            (
                "FNO_OPERATOR_TRANSCRIPT".into(),
                transcript.display().to_string(),
            ),
            ("FNO_OPERATOR_CAPTURE_DIR".into(), dir.display().to_string()),
        ])
    }

    #[test]
    fn session_queue_depth_refuses_unreadable_or_malformed_ack_ledger() {
        let dir = tmp_dir("session-depth-ack-errors");
        let transcript = dir.join("transcript.jsonl");
        write_jsonl(&transcript, &[user_row(json!("first ask"), "u-1")]);
        let ledger = ledger_path(&dir, "fixture-session");
        std::fs::create_dir(&ledger).unwrap();
        let vars = pinned_operator_turns(&dir, &transcript);
        let get = |key: &str| vars.get(key).cloned();
        let home = crate::paths::AgentsHome::at(&dir.join("home"));
        assert!(session_queue_depth(&get, &home, &dir, NOW)
            .unwrap_err()
            .contains("ack ledger"));
        std::fs::remove_dir(&ledger).unwrap();
        std::fs::write(&ledger, "not-json\n").unwrap();
        assert!(session_queue_depth(&get, &home, &dir, NOW)
            .unwrap_err()
            .contains("malformed"));
    }

    #[test]
    fn naive_timestamp_reads_as_utc_not_local() {
        let out = read_raw(
            "naive",
            &[user_row_ts(
                json!("ask"),
                "u-naive",
                "2026-09-06T21:00:00.000000",
            )],
        );
        assert_eq!(out.payload.turns[0].ts_epoch, Some(1788728400.0));
        let out = read_raw(
            "naive-date-only",
            &[user_row_ts(json!("ask"), "u-date", "2026-09-06")],
        );
        assert_eq!(out.payload.turns[0].ts_epoch, Some(1788652800.0));
    }

    #[test]
    fn ac2_scan_cursor_saves_state_and_second_read_stays_incremental() {
        let dir = tmp_dir("ac2");
        let tp = dir.join("transcript.jsonl");
        write_jsonl(&tp, &[user_row(json!("first ask"), "u-1")]);
        let first = read(&dir, &tp);
        assert_eq!(ids(&first), ["u-1"]);
        let scan = scan_path(&dir, "s");
        assert!(scan.is_file());
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(&scan).unwrap()).unwrap()
                ["offset"]
                .as_u64(),
            Some(tp.metadata().unwrap().len())
        );
        append(&tp, &format!("{}\n", user_row(json!("second ask"), "u-2")));
        let second = read(&dir, &tp);
        assert_eq!(ids(&second), ["u-1", "u-2"]);
        assert_eq!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(&scan).unwrap()).unwrap()
                ["offset"]
                .as_u64(),
            Some(tp.metadata().unwrap().len())
        );
    }

    #[test]
    fn ac3_scan_state_resets_on_invalid_alien_past_eof_or_rotation() {
        let dir = tmp_dir("ac3");
        let tp = dir.join("transcript.jsonl");
        write_jsonl(&tp, &[user_row(json!("ask"), "u-1")]);
        let fresh = read(&dir, &tp);
        let scan = scan_path(&dir, "s");

        let matches_baseline = |out: &QueueOutcome, baseline: &QueueOutcome| {
            out.payload.depth == baseline.payload.depth
                && ids(out) == ids(baseline)
                && out.payload.skipped == baseline.payload.skipped
        };

        std::fs::write(&scan, "{not json").unwrap();
        assert!(matches_baseline(&read(&dir, &tp), &fresh));

        std::fs::write(
            &scan,
            serde_json::to_string(&json!({
                "transcript": "/elsewhere.jsonl", "offset": 5, "line_no": 1,
                "head_sha256": "x", "turns": [], "skipped": {}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches_baseline(&read(&dir, &tp), &fresh));

        std::fs::write(
            &scan,
            serde_json::to_string(&json!({
                "transcript": tp.display().to_string(),
                "offset": tp.metadata().unwrap().len() + 10, "line_no": 1,
                "head_sha256": "x", "turns": [], "skipped": {}
            }))
            .unwrap(),
        )
        .unwrap();
        assert!(matches_baseline(&read(&dir, &tp), &fresh));

        // Rotation in place: the transcript is rewritten at a larger size, so
        // the saved head sample no longer matches the file's bytes.
        append(
            &tp,
            &format!("{}\n", user_row(json!("post-rewrite"), "u-2")),
        );
        let _ = std::fs::remove_file(&scan);
        let baseline = read(&dir, &tp);
        let saved =
            serde_json::from_str::<Value>(&std::fs::read_to_string(&scan).unwrap()).unwrap();
        let mut rotated = saved.clone();
        rotated["offset"] = json!(1);
        std::fs::write(&scan, rotated.to_string()).unwrap();
        assert!(matches_baseline(&read(&dir, &tp), &baseline));
    }

    #[test]
    fn ac5_torn_trailing_row_waits_for_its_newline() {
        let dir = tmp_dir("ac5");
        let tp = dir.join("transcript.jsonl");
        write_jsonl(&tp, &[user_row(json!("whole"), "u-1")]);
        append(&tp, &user_row(json!("torn"), "u-2").to_string());
        let first = read(&dir, &tp);
        assert_eq!(ids(&first), ["u-1"]);
        assert!(
            serde_json::from_str::<Value>(&std::fs::read_to_string(scan_path(&dir, "s")).unwrap())
                .unwrap()["offset"]
                .as_u64()
                .unwrap()
                < tp.metadata().unwrap().len()
        );
        append(&tp, "\n");
        let second = read(&dir, &tp);
        assert_eq!(ids(&second), ["u-1", "u-2"]);
    }

    #[test]
    fn ac6_derived_turn_id_stable_across_reads_and_matches_the_python_digest() {
        let dir = tmp_dir("ac6");
        let tp = dir.join("transcript.jsonl");
        write_jsonl(
            &tp,
            &[
                json!({"type": "user", "timestamp": "2026-09-06T21:00:00.000Z",
                     "message": {"role": "user", "content": "no id here"}}),
            ],
        );
        let first = read(&dir, &tp);
        assert_eq!(first.payload.turns[0].turn_id, "derived-4ee369cad436");
        append(&tp, &format!("{}\n", user_row(json!("later"), "u-later")));
        let second = read(&dir, &tp);
        assert_eq!(second.payload.turns[0].turn_id, "derived-4ee369cad436");
    }

    #[test]
    fn ac8_machine_shapes_never_queue_and_are_counted() {
        let out = read_raw("machine", &[
            user_row(
                json!("<task-notification><task-id>b55cj2z2z</task-id><output-file>/tmp/out</output-file></task-notification>"),
                "u-tn",
            ),
            user_row(
                json!("Another Claude session sent a message:\n<teammate-message teammate_id=\"t1\" color=\"blue\">{}</teammate-message>"),
                "u-tm",
            ),
            user_row(
                json!("This session is being continued from a previous conversation that ran out of context. The summary below covers the work."),
                "u-cp",
            ),
            user_row(json!("[Request interrupted by user]"), "u-int1"),
            user_row(json!("[Request interrupted by user for tool use]"), "u-int2"),
            user_row(json!("<bash-input>git status</bash-input>"), "u-bi"),
            user_row(json!("<bash-stdout>nothing to commit</bash-stdout>"), "u-bs"),
            user_row(
                json!("<command-message>fno:target</command-message>\n<command-name>/fno:target</command-name>"),
                "u-cm",
            ),
            user_row(json!("status on your nodes?"), "u-prose"),
        ]);
        assert_eq!(ids(&out), ["u-prose"]);
        let skipped = &out.payload.skipped;
        assert_eq!(skipped.get("task_notification"), Some(&1));
        assert_eq!(skipped.get("teammate_message"), Some(&1));
        assert_eq!(skipped.get("compaction_preamble"), Some(&1));
        assert_eq!(skipped.get("interrupt_marker"), Some(&2));
        assert_eq!(skipped.get("bash_echo"), Some(&2));
        assert_eq!(skipped.get("command_invocation"), Some(&1));

        // The status half: depth excludes the machine turn and names the skip.
        let dir = tmp_dir("ac8-status");
        let tp = dir.join("transcript.jsonl");
        write_jsonl(
            &tp,
            &[
                user_row_ts(
                    json!("<task-notification><task-id>t9</task-id></task-notification>"),
                    "u-tn",
                    "2026-09-06T20:30:00.000Z",
                ),
                user_row(json!("status on your nodes?"), "u-real"),
            ],
        );
        let out = read(&dir, &tp);
        assert_eq!(out.payload.depth, 1);
        assert_eq!(out.payload.oldest_turn_id.as_deref(), Some("u-real"));
        assert_eq!(out.payload.skipped.get("task_notification"), Some(&1));
    }

    #[test]
    fn prose_turn_queues_and_mail_turn_does_not() {
        let out = read_raw(
            "prose",
            &[
                user_row(json!("please widen the review gate"), "u-prose-1"),
                user_row(
                    json!(["<fno_mail from=\"peer\" harness=\"claude\">run the sweep</fno_mail>"]),
                    "u-mail-1",
                ),
            ],
        );
        assert_eq!(ids(&out), ["u-prose-1"]);
    }

    #[test]
    fn stand_down_turn_is_marked_in_the_payload() {
        let out = read_raw(
            "stand-down-payload",
            &[user_row(
                json!("Perhaps our lead has overstayed its welcome"),
                "u-stand-down",
            )],
        );
        let payload = serde_json::to_value(&out.payload).unwrap();
        assert_eq!(payload["turns"][0]["stand_down"], true);
    }

    #[test]
    fn ac14_err_empty_answer_ref_refuses_and_writes_nothing() {
        let dir = tmp_dir("ac14-err");
        let home = crate::paths::AgentsHome::at(&dir.join("home"));
        let capture = dir.join("capture");
        std::fs::create_dir_all(&capture).unwrap();
        let err = ack_turn(&home, &capture, "s", "u-1", "answer:", "").unwrap_err();
        assert!(err.contains("invalid --outcome"), "{err}");
        assert!(
            !capture.join("s.jsonl").exists(),
            "the refusal writes no ledger"
        );
        let index = crate::event_store::journal_text(
            &crate::provider_cap::questions_path(&home),
            &["user_ask_answered"],
        );
        assert!(
            !index.contains("user_ask_answered"),
            "the refusal writes no answered row: {index}"
        );
    }
}
