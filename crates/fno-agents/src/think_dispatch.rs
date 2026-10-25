//! `fno do think dispatch <node>`: hand a backlog node to a background
//! `/think` worker that carries THIS session's live transcript pointer.
//!
//! Ported from `fno.provenance.cli.dispatch` and the conversational tail of
//! `fno.provenance.spawn_think.maybe_spawn_think`. The birth branch stays in
//! `backlog::birth`; this reuses its gate, presence and transcript reads, and
//! `think_daily` for the per-day ceiling. The explicit command is its own
//! opt-in: the gate is forced on and an attended session still spawns. Every
//! other guard holds: the per-invocation dedup token, the per-day ceiling,
//! the forward stamp, and exactly one decision event once the gate is on.
//!
//! Exit codes: 0 spawned or noop, 1 skipped (dedup, daily cap, ...), 2 bad
//! input (no live session id, node not found, unsupported provider).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::backlog::birth;

pub const EVENT_SPAWNED: &str = "think_spawned";
pub const EVENT_SKIPPED: &str = birth::EVENT_SKIPPED;
/// The trigger every event and the dedup token carry.
pub const REASON: &str = "conversational";
const DISPATCH_TTL_MS: i64 = 180_000;
const SPAWN_ALREADY_EXISTS: &str = "already exists";
const PREFIX: &str = "fno do think dispatch";
const USAGE: &str = "Usage: fno do think dispatch [OPTIONS] NODE";
const HELP: &str = "Usage: fno do think dispatch [OPTIONS] NODE

  Dispatch a bg /think for NODE carrying THIS session's live transcript pointer.

  Exit codes: 0 spawned or noop, 1 skipped (e.g. dedup / daily-cap), 2 bad
  input (no live session id or node not found).

Options:
  --session-id TEXT    Live transcript session id (default: ambient
                       $CODEX_THREAD_ID / $CLAUDE_CODE_SESSION_ID /
                       $CODEX_SESSION_ID / $GEMINI_SESSION_ID).
  --cwd TEXT           Live session cwd (default: the current working dir).
  --harness TEXT       Live session harness (default: ambiently detected).
  -m, --model TEXT     Model for the spawned worker (exact passthrough).
  -p, --provider TEXT  Harness for the spawned worker (default: claude; a
                       detached /think refuses any other).
  -J, --json           Emit the dispatch result as JSON.
  --help               Show this message and exit.";
const NO_SESSION: &str = "fno do think dispatch: no live session id - set --session-id or run \
     inside a claude/codex/gemini session ($CODEX_THREAD_ID / $CLAUDE_CODE_SESSION_ID / \
     $CODEX_SESSION_ID / $GEMINI_SESSION_ID). There is nothing to carry without a live pointer.";
const NOT_CLAUDE: &str = "fno do think dispatch: detached /think uses Claude bg; omit --provider \
     to use the Claude fallback; non-Claude headless is a one-shot with no live think-session \
     receipt and is not supported by this command.";
const EXTERNAL_BACKEND: &str = "fno do think dispatch: provenance.think_dispatch: \
     footnote-minted node metadata (size/type/project/dep/model pins/persistent slug) lives \
     only in the default graph store and is unavailable under an external tracker backend";

/// One dispatch evaluation, in the `--json` field order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DispatchResult {
    pub decision: &'static str,
    pub event: Option<&'static str>,
    pub reason: Option<String>,
    pub node_id: Option<String>,
    pub presence: Option<String>,
    pub resolved: Option<bool>,
    pub think_session: Option<String>,
    pub detail: Option<String>,
}

impl DispatchResult {
    pub fn to_json(&self) -> Value {
        json!({
            "decision": self.decision,
            "event": self.event,
            "reason": self.reason,
            "node_id": self.node_id,
            "presence": self.presence,
            "resolved": self.resolved,
            "think_session": self.think_session,
            "detail": self.detail,
        })
    }
}

/// A transcript resolution as the seed reads it.
#[derive(Debug, Clone, Default)]
pub struct SeedTranscript {
    pub path: Option<String>,
    pub resolved: bool,
    pub reason: Option<String>,
    pub ambiguous: bool,
}

/// The spawn prompt (it carries the transcript POINTER, never a paraphrase)
/// and the one copy-pasteable offer line.
#[derive(Debug, Clone)]
pub struct ThinkSeed {
    pub prompt: String,
    pub offer_line: String,
    pub resolved: bool,
    pub output_path: String,
}

fn text<'v>(node: &'v Value, key: &str) -> Option<&'v str> {
    node.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Build the /think seed for a node whose pointer resolved to `transcript`,
/// writing its doc to `output_path` (empty omits the line).
pub fn assemble_seed(node: &Value, transcript: &SeedTranscript, output_path: &str) -> ThinkSeed {
    let node_id = text(node, "id").unwrap_or("?");
    let slug = text(node, "slug").unwrap_or("");
    let title = text(node, "title").unwrap_or("");
    let details = text(node, "details").unwrap_or("").trim();
    let pointer = transcript
        .path
        .as_deref()
        .filter(|p| transcript.resolved && !p.is_empty());
    let mut why: Vec<String> = Vec::new();
    match pointer {
        Some(path) => {
            why.push(format!("  origin transcript: {path}"));
            if transcript.ambiguous {
                why.push(
                    "  (note: session-id prefix matched multiple transcripts; this is the first)"
                        .to_string(),
                );
            }
        }
        None => {
            let reason = transcript
                .reason
                .as_deref()
                .filter(|r| !r.is_empty())
                .unwrap_or("unknown");
            why.push(format!(
                "  origin transcript UNRESOLVED (reason: {reason}); fall back to the stored \
                 pointer:"
            ));
            why.push(format!(
                "  origin session: {}:{} @ {}",
                text(node, "source_harness").unwrap_or("?"),
                text(node, "source_session_id").unwrap_or("?"),
                text(node, "source_cwd").unwrap_or("?")
            ));
        }
    }
    if let Some(origin) = text(node, "source_node_id") {
        why.push(format!("  origin node chain: {origin}"));
    }
    let mut prompt = format!(
        "/think {node_id}\n\nWHY THIS NODE EXISTS - read the originating context below for the \
         full reasoning that justified filing it. Do NOT work from the title alone; the \
         transcript holds the discovery / failure-mode / tradeoff that made this worth filing.\n"
    );
    prompt.push_str(&why.join("\n"));
    let slug_part = if slug.is_empty() {
        String::new()
    } else {
        format!("({slug})")
    };
    prompt.push_str(&format!(
        "\n\nNode: {node_id} {slug_part}\nTitle: {title}\n"
    ));
    if !details.is_empty() {
        prompt.push_str(&format!("\nDetails:\n{details}\n"));
    }
    if !output_path.is_empty() {
        prompt.push_str(&format!(
            "\nWRITE YOUR /think OUTPUT to this exact path so node {node_id} can point at it:\n  \
             {output_path}\n"
        ));
    }
    let offer_line = match pointer {
        Some(path) => format!("/think {node_id}  # origin transcript: {path}"),
        None => format!("/think {node_id}"),
    };
    ThinkSeed {
        prompt,
        offer_line,
        resolved: transcript.resolved,
        output_path: output_path.to_string(),
    }
}

/// Whether a file name matches the `????-??-??-*.md` plans-dir glob. A
/// dot-leading name is hidden from the glob.
fn dated_name(name: &str) -> bool {
    let chars: Vec<char> = name.chars().collect();
    chars.len() >= 14
        && chars[0] != '.'
        && chars[4] == '-'
        && chars[7] == '-'
        && chars[10] == '-'
        && name.ends_with(".md")
}

fn dated_docs(pdir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(pdir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(dated_name)
        })
        .collect();
    out.sort();
    out
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// True when the file's frontmatter (never its body) names the node under
/// `claims:` or `graph_node_id:`.
fn frontmatter_claims_node(path: &Path, node_id: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    if !text.starts_with("---") {
        return false;
    }
    let fm: &str = match text[3..].find("\n---") {
        Some(i) => &text[..i + 3],
        None => match text.char_indices().nth(2000) {
            Some((i, _)) => &text[..i],
            None => &text,
        },
    };
    let Ok(re) = regex::Regex::new(r"^(?:claims|graph_node_id):\s*(.+?)\s*$") else {
        return false;
    };
    fm.lines().any(|line| {
        re.captures(line)
            .and_then(|c| c.get(1))
            .is_some_and(|m| m.as_str().trim().trim_matches(|c| c == '"' || c == '\'') == node_id)
    })
}

/// The plans-dir doc that already belongs to `node_id`: a frontmatter claim
/// wins over a `-<node_id>.md` name, first in sorted order either way.
pub fn find_node_doc(pdir: &Path, node_id: &str) -> Option<PathBuf> {
    let suffix = format!("-{node_id}.md");
    let mut by_name: Option<PathBuf> = None;
    for path in dated_docs(pdir) {
        if frontmatter_claims_node(&path, node_id) {
            return Some(path);
        }
        if by_name.is_none() && file_name(&path).ends_with(&suffix) {
            by_name = Some(path);
        }
    }
    by_name
}

/// Where the worker writes its /think doc inside `pdir`: this node's doc,
/// else a legacy `<date>-<slug>.md`, else a fresh `<date>-<slug>-<id>.md`
/// minted on `date`. Reuse keys on the stable node id, not the mutable slug.
pub fn output_path_in(pdir: &Path, node_id: &str, slug: &str, date: &str) -> String {
    if let Some(existing) = find_node_doc(pdir, node_id) {
        return existing.to_string_lossy().into_owned();
    }
    let s = slug.trim_matches('-');
    if !s.is_empty() {
        let legacy = format!("{s}.md");
        let hit = dated_docs(pdir).into_iter().find(|p| {
            let name = file_name(p);
            name.chars().count() == 11 + legacy.chars().count()
                && name.chars().skip(11).collect::<String>() == legacy
        });
        if let Some(path) = hit {
            return path.to_string_lossy().into_owned();
        }
    }
    let tail = if s.is_empty() {
        node_id.to_string()
    } else {
        format!("{s}-{node_id}")
    };
    pdir.join(format!("{date}-{tail}.md"))
        .to_string_lossy()
        .into_owned()
}

/// The plans-dir answer for the repo at `root`, else the briefs fallback with
/// a visible warning; empty only when even briefs is unresolvable.
fn think_output_path(node_id: &str, slug: &str, root: &Path, note: &dyn Fn(&str)) -> String {
    if let Some(pdir) = crate::plans_path::plans_content_dir(root) {
        let date = chrono::Utc::now().format("%Y-%m-%d").to_string();
        return output_path_in(&pdir, node_id, slug, &date);
    }
    note(&format!(
        "warn: plans dir unresolvable (could not resolve the plans dir for {}); /think doc \
         falls back to briefs/ - relocate before /blueprint",
        root.display()
    ));
    crate::backlog::autobrief::briefs_dir()
        .map(|d| {
            d.join(format!("think-{node_id}.md"))
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default()
}

/// The provenance-carrying worker name. The reason and the session suffix
/// are load-bearing, so an over-budget identity refuses rather than shaves.
pub fn worker_agent_name(
    node_id: &str,
    slug: Option<&str>,
    suffix: Option<&str>,
    model: Option<&str>,
) -> Result<String, String> {
    fn set(v: Option<&str>) -> Option<&str> {
        v.filter(|s| !s.trim().is_empty())
    }
    crate::naming::dispatch_agent_name(
        Some("th"),
        "th",
        node_id,
        set(slug),
        Some(REASON),
        set(suffix),
        set(model),
    )
    .map_err(|e| e.to_string())
}

fn receipt_id(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Number(n)) if n.as_f64() != Some(0.0) => Some(n.to_string()),
        _ => None,
    }
}

/// The spawn receipt's `short_id`: the whole stdout as one JSON object
/// (compact or pretty), else the first JSON line that carries one. A real
/// parse, never a regex, so a log line that only mentions the key is noise.
pub fn parse_short_id(stdout: &str) -> String {
    if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(stdout) {
        if let Some(id) = receipt_id(obj.get("short_id")) {
            return id;
        }
    }
    for line in stdout.lines() {
        if !line.contains("\"short_id\"") {
            continue;
        }
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) {
            if let Some(id) = receipt_id(obj.get("short_id")) {
                return id;
            }
        }
    }
    String::new()
}

/// The live session id's head: it discriminates the dedup token and the
/// worker name, so a later conversation can re-dispatch the same node.
pub fn invocation_suffix(session_id: &str) -> Option<String> {
    let head: String = session_id.trim().chars().take(8).collect();
    Some(head).filter(|h| !h.is_empty())
}

/// The reason-scoped dedup token.
pub fn dedup_key(node_id: &str, suffix: Option<&str>) -> String {
    match suffix.filter(|s| !s.is_empty()) {
        Some(s) => format!("dispatch:think:{node_id}:{REASON}:{s}"),
        None => format!("dispatch:think:{node_id}:{REASON}"),
    }
}

/// One dedup-token acquire.
pub enum ClaimTry {
    Acquired,
    Held,
    Error(String),
}

/// A spawn that did not launch: a peer already owns the name, or anything
/// else (the node stays re-spawnable either way).
pub enum SpawnError {
    AlreadyRunning,
    Failed(String),
}

/// What the spawn seam launches.
#[derive(Debug, Clone)]
pub struct SpawnRequest {
    pub node_id: String,
    pub prompt: String,
    pub node_cwd: Option<String>,
    pub node_slug: Option<String>,
    pub suffix: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

/// The tail's reads and side effects, one seam each.
pub struct TailSeams<'a> {
    pub armed: bool,
    pub rank: &'a str,
    pub max_per_run: usize,
    pub day_cap: i64,
    pub presence: &'a dyn Fn() -> String,
    pub transcript: &'a dyn Fn(&str, &str, &str) -> SeedTranscript,
    pub output_path: &'a dyn Fn(&str, &str) -> String,
    pub day_count: &'a dyn Fn() -> i64,
    pub claim_live: &'a dyn Fn(&str) -> bool,
    /// (key, holder, reason).
    pub acquire: &'a dyn Fn(&str, &str, &str) -> ClaimTry,
    pub release: &'a dyn Fn(&str, &str),
    pub spawn: &'a dyn Fn(&SpawnRequest) -> Result<String, SpawnError>,
    pub bump: &'a dyn Fn(),
    /// (node id, think session, output path); empty means absent.
    pub stamp: &'a dyn Fn(&str, &str, &str),
    pub emit: &'a dyn Fn(&str, Map<String, Value>),
    pub note: &'a dyn Fn(&str),
}

/// Overlay the LIVE pointer onto the node, so the seed resolves this
/// conversation's transcript and never the node's birth origin, then run
/// the decision ladder.
pub fn conversational_tail(
    node: &Value,
    session_id: &str,
    cwd: &str,
    harness: &str,
    seams: &TailSeams,
) -> DispatchResult {
    let mut live = node.as_object().cloned().unwrap_or_default();
    live.insert("source_harness".into(), json!(harness));
    live.insert("source_session_id".into(), json!(session_id));
    live.insert("source_cwd".into(), json!(cwd));
    let suffix = invocation_suffix(session_id);
    run_tail(&Value::Object(live), suffix.as_deref(), seams)
}

fn run_tail(node: &Value, suffix: Option<&str>, s: &TailSeams) -> DispatchResult {
    let raw_id = node.get("id").and_then(Value::as_str).map(str::to_string);
    if !s.armed {
        return DispatchResult {
            decision: "noop",
            reason: Some("disabled".into()),
            node_id: raw_id,
            ..Default::default()
        };
    }
    let skip = |reason: &str, presence: Option<&str>, detail: Option<String>| {
        let mut data = Map::new();
        data.insert("reason".into(), json!(reason));
        data.insert("trigger".into(), json!(REASON));
        data.insert("rank".into(), json!(s.rank));
        if let Some(id) = raw_id.as_deref().filter(|i| !i.is_empty()) {
            data.insert("node_id".into(), json!(id));
        }
        if let Some(p) = presence {
            data.insert("presence".into(), json!(p));
        }
        if let Some(d) = &detail {
            data.insert("detail".into(), json!(d));
        }
        (s.emit)(EVENT_SKIPPED, data);
        DispatchResult {
            decision: "skipped",
            event: Some(EVENT_SKIPPED),
            reason: Some(reason.to_string()),
            node_id: raw_id.clone(),
            presence: presence.map(str::to_string),
            detail,
            ..Default::default()
        }
    };
    let Some(id) = raw_id.as_deref().filter(|i| !i.is_empty()) else {
        return skip("no-node-id", None, None);
    };
    if py_truthy(node.get("roadmap_id")) || py_truthy(node.get("vision_path")) {
        return skip("bulk-intake", None, None);
    }
    let origin = text(node, "source_session_id").unwrap_or("").trim();
    if origin.is_empty() {
        return skip("no-origin", None, None);
    }
    // One node per run, so the blast-radius cap binds only at a cap of 0.
    if s.max_per_run == 0 {
        (s.note)(&format!(
            "spawn_think: blast-radius cap reached ({}); skipping further /think spawns this run",
            s.max_per_run
        ));
        return skip(
            "cap-exceeded",
            None,
            Some(format!("max_per_run={}", s.max_per_run)),
        );
    }
    let presence = (s.presence)();
    let transcript = (s.transcript)(
        text(node, "source_harness").unwrap_or(""),
        text(node, "source_session_id").unwrap_or(""),
        text(node, "source_cwd").unwrap_or(""),
    );
    let output_path = (s.output_path)(id, text(node, "slug").unwrap_or(""));
    let seed = assemble_seed(node, &transcript, &output_path);
    if s.day_cap > 0 && (s.day_count)() >= s.day_cap {
        return skip(
            "daily-cap",
            Some(presence.as_str()),
            Some(format!("daily_cap={}", s.day_cap)),
        );
    }
    let key = dedup_key(id, suffix);
    let holder = format!("think-spawn:{}", std::process::id());
    if (s.claim_live)(&key) {
        return skip("already-claimed", Some(presence.as_str()), None);
    }
    let claim_reason = format!("context /think dispatch ({REASON}) for {id}");
    match (s.acquire)(&key, &holder, &claim_reason) {
        ClaimTry::Acquired => {}
        ClaimTry::Held => return skip("already-claimed", Some(presence.as_str()), None),
        ClaimTry::Error(e) => return skip("claim-error", Some(presence.as_str()), Some(e)),
    }
    let node_slug = text(node, "slug")
        .or_else(|| text(node, "title"))
        .map(str::to_string);
    let request = SpawnRequest {
        node_id: id.to_string(),
        prompt: seed.prompt.clone(),
        node_cwd: text(node, "_resolved_cwd")
            .or_else(|| text(node, "cwd"))
            .map(str::to_string),
        node_slug: node_slug.clone(),
        suffix: suffix.map(str::to_string),
        model: text(node, "model").map(str::to_string),
        provider: text(node, "provider").map(str::to_string),
    };
    let short_id = match (s.spawn)(&request) {
        Ok(short_id) => short_id,
        Err(SpawnError::AlreadyRunning) => {
            (s.release)(&key, &holder);
            return skip("already-claimed", Some(presence.as_str()), None);
        }
        Err(SpawnError::Failed(detail)) => {
            (s.release)(&key, &holder);
            return skip("spawn-failed", Some(presence.as_str()), Some(detail));
        }
    };
    (s.bump)();
    (s.stamp)(id, &short_id, &seed.output_path);
    // The event names the worker without its model tag, as the Python leg did.
    let agent_name = worker_agent_name(id, node_slug.as_deref(), suffix, None).unwrap_or_default();
    let mut data = Map::new();
    data.insert("node_id".into(), json!(id));
    data.insert("trigger".into(), json!(REASON));
    data.insert("think_session".into(), json!(short_id));
    data.insert("presence".into(), json!(presence));
    data.insert("resolved".into(), json!(seed.resolved));
    let output = Some(seed.output_path.as_str()).filter(|p| !p.is_empty());
    data.insert("output_path".into(), json!(output));
    data.insert("agent_name".into(), json!(agent_name));
    data.insert("rank".into(), json!(s.rank));
    (s.emit)(EVENT_SPAWNED, data);
    DispatchResult {
        decision: "spawned",
        event: Some(EVENT_SPAWNED),
        node_id: raw_id.clone(),
        presence: Some(presence),
        resolved: Some(seed.resolved),
        think_session: Some(short_id),
        ..Default::default()
    }
}

/// The spawn argv for a substrate. A thread is the door's default lane; a
/// pane needs `--as pane` before its placement flag or the door refuses.
fn substrate_lane_flags(substrate: &str) -> Vec<String> {
    match substrate.trim() {
        "headless" => vec!["--once".to_string()],
        "pane" => vec![
            "--as".to_string(),
            "pane".to_string(),
            "--bounded-placement".to_string(),
        ],
        _ => Vec::new(),
    }
}

/// The deprecated `think_spawn.substrate` alias, consulted only when the
/// shared `dispatch.substrate` is unset.
fn legacy_substrate(node_cwd: Option<&str>) -> Option<String> {
    let cwd = node_cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    crate::agents_config::config_lookup(&cwd, &["think_spawn", "substrate"])
        .and_then(|v| v.as_str().map(str::to_string))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Launch the fire-and-forget worker through the shared dispatch resolver
/// and the one `fno agents spawn` exec. Returns the receipt's short id; a
/// pane answers its addressable name, and a headless one-shot answers empty
/// because it has already torn down and a pointer must resolve or be absent.
fn spawn_think_worker(req: &SpawnRequest) -> Result<String, SpawnError> {
    let agent_name = worker_agent_name(
        &req.node_id,
        req.node_slug.as_deref(),
        req.suffix.as_deref(),
        req.model.as_deref(),
    )
    .map_err(SpawnError::Failed)?;
    let mut cfg =
        crate::backlog::dispatch_resolve::dispatch_cfg_for(req.node_cwd.as_deref(), "/think");
    if let Some(allowed) = cfg.allowed_commands.as_mut() {
        allowed.push("/think".to_string());
    }
    let legacy = if cfg.substrate.trim().is_empty() {
        legacy_substrate(req.node_cwd.as_deref())
    } else {
        None
    };
    let input = crate::backlog::dispatch_resolve::DispatchInput {
        harness: req
            .provider
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty()),
        substrate: legacy.as_deref(),
        node_id: Some(req.node_id.as_str()),
        command: None,
        command_name: Some("/think"),
        lifecycle: None,
        brief: None,
        merge_posture: None,
        trigger: "autonomous".to_string(),
    };
    let resolved = crate::backlog::dispatch_resolve::resolve_dispatch(&input, &cfg)
        .map_err(SpawnError::Failed)?;
    let portable = format!("/think {}", req.node_id);
    let prompt = match req.prompt.strip_prefix(portable.as_str()) {
        Some(rest) => format!("{}{rest}", resolved.command),
        None => req.prompt.clone(),
    };
    let mut argv = vec!["--harness".to_string(), resolved.harness.clone()];
    argv.extend(substrate_lane_flags(&resolved.substrate));
    match req.node_cwd.as_deref() {
        Some(cwd) => argv.extend(["--cwd".to_string(), cwd.to_string()]),
        None => argv.push("--fresh".to_string()),
    }
    // Behind `--`, a flag-leading seed stays the prompt positional.
    argv.extend([
        "--name".to_string(),
        agent_name.clone(),
        "--".to_string(),
        prompt,
    ]);
    let mut env = BTreeMap::new();
    env.insert(
        "FNO_SPAWN_TRIGGER".to_string(),
        format!("think_spawn:{REASON}"),
    );
    let out = crate::backlog::advance_dispatch::exec_agents_spawn(&argv, &env, false)
        .map_err(|e| SpawnError::Failed(format!("fno agents spawn: {e}")))?;
    let code = out.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    if code != 0 {
        let stderr_s = stderr.trim();
        if code == 2 && stderr_s.contains(SPAWN_ALREADY_EXISTS) {
            return Err(SpawnError::AlreadyRunning);
        }
        let raw = if stderr_s.is_empty() {
            stdout.as_str()
        } else {
            stderr_s
        };
        let detail = crate::backlog::advance::gate_refusal_detail(raw);
        return Err(SpawnError::Failed(format!(
            "fno agents spawn exited {code}: {detail}"
        )));
    }
    let short_id = parse_short_id(&stdout);
    if !short_id.is_empty() {
        return Ok(short_id);
    }
    match resolved.substrate.as_str() {
        "pane" => Ok(agent_name),
        "headless" => Ok(String::new()),
        _ => {
            let raw = if stdout.is_empty() { &stderr } else { &stdout };
            Err(SpawnError::Failed(format!(
                "fno agents spawn exit 0 but no short_id receipt: {}",
                raw.trim().chars().take(200).collect::<String>()
            )))
        }
    }
}

/// Point the node at its /think session and output doc. Best-effort: a
/// failed stamp never unwinds a spawn that already launched.
fn stamp_forward(node_id: &str, think_session: &str, output_path: &str) {
    if think_session.is_empty() && output_path.is_empty() {
        return;
    }
    let graph = crate::backlog::settings::graph_path();
    let _ = crate::graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |rows| {
            let Some(obj) = rows
                .iter_mut()
                .find(|r| crate::graph_store::entry_id(r) == Some(node_id))
                .and_then(|r| r.as_object_mut())
            else {
                return Ok(false);
            };
            if !think_session.is_empty() {
                obj.insert("think_session_id".into(), json!(think_session));
            }
            if !output_path.is_empty() {
                obj.insert("think_output_path".into(), json!(output_path));
            }
            Ok(true)
        },
    );
}

fn acquire_claim(key: &str, holder: &str, reason: &str) -> ClaimTry {
    let opts = crate::claims::AcquireOpts {
        ttl_ms: Some(DISPATCH_TTL_MS),
        reason: Some(reason.to_string()),
        ..Default::default()
    };
    match crate::claims::acquire(key, holder, opts) {
        crate::claims::AcquireOutcome::Acquired(_) => ClaimTry::Acquired,
        crate::claims::AcquireOutcome::HeldByOther { .. } => ClaimTry::Held,
        crate::claims::AcquireOutcome::Error(e) => ClaimTry::Error(e),
    }
}

fn seed_transcript(t: birth::Transcript) -> SeedTranscript {
    SeedTranscript {
        resolved: t.path.is_some(),
        path: t.path.map(|p| p.to_string_lossy().into_owned()),
        reason: t.reason.map(str::to_string),
        ambiguous: t.ambiguous,
    }
}

/// The tail over the live seams: the forced gate and attended opt-in, the
/// config reads, the claims store, the spawn door, the graph and the journal.
fn dispatch_live(
    node: &Value,
    pre: &Preflight,
    anchor: &Path,
    note: &dyn Fn(&str),
) -> DispatchResult {
    let get = |name: &str| -> Option<String> {
        match name {
            "FNO_THINK_SPAWN" => Some("1".to_string()),
            "FNO_THINK_SPAWN_ATTENDED" => Some("spawn".to_string()),
            _ => std::env::var(name).ok(),
        }
    };
    let (armed, rank) = birth::think_spawn_resolve(&get);
    let root = crate::accounts::records::resolve_repo_root(anchor);
    let events = std::env::var_os("FNO_EVENTS_PATH")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::paths::events_path(&root));
    let presence = || birth::classify_presence(anchor, &get);
    let transcript = |harness: &str, session: &str, cwd: &str| {
        seed_transcript(birth::resolve_transcript_detail(harness, session, cwd))
    };
    let output_path = |id: &str, slug: &str| think_output_path(id, slug, &root, note);
    let emit = |kind: &str, data: Map<String, Value>| birth::emit(kind, data, &events);
    let seams = TailSeams {
        armed,
        rank,
        max_per_run: birth::max_per_run(),
        day_cap: crate::think_daily::daily_cap(Some(root.as_path())),
        presence: &presence,
        transcript: &transcript,
        output_path: &output_path,
        day_count: &crate::think_daily::count,
        claim_live: &crate::backlog::advance::claim_is_live,
        acquire: &acquire_claim,
        release: &crate::backlog::advance::safe_release,
        spawn: &spawn_think_worker,
        bump: &crate::think_daily::bump,
        stamp: &stamp_forward,
        emit: &emit,
        note,
    };
    conversational_tail(node, &pre.session_id, &pre.cwd, &pre.harness, &seams)
}

/// The parsed argv after `fno do think dispatch`.
#[derive(Debug, Clone, Default)]
pub struct DispatchArgs {
    pub node: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub harness: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub json: bool,
}

pub enum Parsed {
    Args(DispatchArgs),
    Help,
}

/// Parse the command's argv: `--opt value`, `--opt=value`, `-m value`,
/// `-mvalue`, and `--` ending the options.
pub fn parse_args(argv: &[String]) -> Result<Parsed, String> {
    let mut args = DispatchArgs::default();
    let mut node: Option<String> = None;
    let mut positional_only = false;
    let mut i = 0;
    while i < argv.len() {
        let arg = argv[i].as_str();
        i += 1;
        if positional_only || !arg.starts_with('-') || arg == "-" {
            if node.is_some() {
                return Err(format!("Got unexpected extra argument ({arg})"));
            }
            node = Some(arg.to_string());
            continue;
        }
        match arg {
            "--" => {
                positional_only = true;
                continue;
            }
            "--help" => return Ok(Parsed::Help),
            "--json" | "-J" => {
                args.json = true;
                continue;
            }
            _ => {}
        }
        let (flag, inline) = if arg.starts_with("--") {
            match arg.split_once('=') {
                Some((f, v)) => (f, Some(v.to_string())),
                None => (arg, None),
            }
        } else if arg.len() > 2 && arg.is_char_boundary(2) {
            (&arg[..2], Some(arg[2..].to_string()))
        } else {
            (arg, None)
        };
        let value = match inline {
            Some(v) => v,
            None => match argv.get(i) {
                Some(v) => {
                    i += 1;
                    v.clone()
                }
                None => return Err(format!("Option '{flag}' requires an argument.")),
            },
        };
        match flag {
            "--session-id" => args.session_id = Some(value),
            "--cwd" => args.cwd = Some(value),
            "--harness" => args.harness = Some(value),
            "--model" | "-m" => args.model = Some(value),
            "--provider" | "-p" => args.provider = Some(value),
            _ => return Err(format!("No such option: {flag}")),
        }
    }
    match node {
        Some(n) => {
            args.node = n;
            Ok(Parsed::Args(args))
        }
        None => Err("Missing argument 'NODE'.".to_string()),
    }
}

/// The ambient live-session pointer (every key degrades to None).
#[derive(Debug, Clone, Default)]
pub struct Provenance {
    pub session_id: Option<String>,
    pub harness: Option<String>,
    pub cwd: Option<String>,
}

/// The validated dispatch inputs: the live pointer and the worker pins.
#[derive(Debug, Clone)]
pub struct Preflight {
    pub session_id: String,
    pub harness: String,
    pub cwd: String,
    pub model: Option<String>,
    pub provider: Option<String>,
}

fn first_set<'a>(explicit: Option<&'a str>, ambient: Option<&'a str>) -> Option<&'a str> {
    explicit
        .filter(|s| !s.is_empty())
        .or(ambient.filter(|s| !s.is_empty()))
}

/// `os.path.abspath`: join a relative path to `base`, then fold `.`, `..`
/// and repeated slashes lexically.
fn abspath(raw: &str, base: &str) -> String {
    let joined = if raw.starts_with('/') {
        raw.to_string()
    } else {
        format!("{}/{raw}", base.trim_end_matches('/'))
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let lead = if joined.starts_with("//") && !joined.starts_with("///") {
        "//"
    } else {
        "/"
    };
    format!("{lead}{}", parts.join("/"))
}

/// Flag validation and the live pointer, before any graph read. Explicit
/// flags win over the ambient pointer; the refusal lines go to stderr.
pub fn preflight(
    args: &DispatchArgs,
    prov: &Provenance,
    process_cwd: &str,
) -> Result<Preflight, Vec<String>> {
    if args.model.as_deref().is_some_and(|m| m.trim().is_empty()) {
        return Err(vec![format!("{PREFIX}: --model must not be empty")]);
    }
    let provider = match args.provider.as_deref() {
        Some(p) if p.trim().is_empty() => {
            return Err(vec![format!("{PREFIX}: --provider must not be empty")]);
        }
        Some(p) => Some(p.trim().to_string()),
        None => None,
    };
    let session_id = first_set(args.session_id.as_deref(), prov.session_id.as_deref())
        .unwrap_or("")
        .trim()
        .to_string();
    let harness = first_set(args.harness.as_deref(), prov.harness.as_deref())
        .unwrap_or("claude")
        .trim()
        .to_string();
    // `--provider` carries a HARNESS value, and the detached /think substrate
    // hosts claude only.
    if provider.as_deref().is_some_and(|p| p != "claude") {
        let mut lines = Vec::new();
        if harness == "codex" {
            lines.push("codex posture: think source=codex; dispatch=unsupported".to_string());
        }
        lines.push(NOT_CLAUDE.to_string());
        return Err(lines);
    }
    if session_id.is_empty() {
        return Err(vec![NO_SESSION.to_string()]);
    }
    let cwd = match args.cwd.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        Some(c) => abspath(c, process_cwd),
        None => prov
            .cwd
            .clone()
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| process_cwd.to_string()),
    };
    Ok(Preflight {
        session_id,
        harness,
        cwd,
        model: args.model.clone(),
        provider,
    })
}

/// The `--json` line or the human line, and the exit code.
pub fn render(result: &DispatchResult, json_output: bool) -> (String, i32) {
    let shown = |v: &Option<String>| v.clone().unwrap_or_else(|| "None".to_string());
    let text = if json_output {
        crate::backlog::render::py_json_compact(&result.to_json())
    } else if result.decision == "spawned" {
        let resolved = match result.resolved {
            Some(true) => "True",
            Some(false) => "False",
            None => "None",
        };
        format!(
            "think dispatched: {} -> bg /think {} (live pointer resolved={resolved}). Watch it \
             with: fno agents watch",
            shown(&result.node_id),
            shown(&result.think_session)
        )
    } else {
        format!(
            "think dispatch {}: {} ({})",
            result.decision,
            shown(&result.node_id),
            shown(&result.reason)
        )
    };
    (text, if result.decision == "skipped" { 1 } else { 0 })
}

/// Everything the command reads outside its argv, one seam each.
pub struct CliEnv<'a> {
    pub provenance: Provenance,
    pub process_cwd: String,
    pub external_backend: bool,
    pub entries: &'a dyn Fn() -> Result<Vec<Value>, String>,
    pub project_root: &'a dyn Fn(&str) -> Option<String>,
    pub out: &'a dyn Fn(&str),
    pub err: &'a dyn Fn(&str),
}

/// The command over its seams: parse, validate, resolve the node (exact id,
/// alias, slug or bare hex only), overlay the worker pins, dispatch, render.
/// Returns the exit code and the dispatch result when one ran.
pub fn run_with(
    argv: &[String],
    env: &CliEnv,
    dispatch: &dyn Fn(&Value, &Preflight) -> DispatchResult,
) -> (i32, Option<DispatchResult>) {
    let args = match parse_args(argv) {
        Ok(Parsed::Args(args)) => args,
        Ok(Parsed::Help) => {
            (env.out)(HELP);
            return (0, None);
        }
        Err(msg) => {
            (env.err)(&format!(
                "{USAGE}\nTry 'fno do think dispatch --help' for help.\n\nError: {msg}"
            ));
            return (2, None);
        }
    };
    let pre = match preflight(&args, &env.provenance, &env.process_cwd) {
        Ok(pre) => pre,
        Err(lines) => {
            for line in &lines {
                (env.err)(line);
            }
            return (2, None);
        }
    };
    if env.external_backend {
        (env.err)(EXTERNAL_BACKEND);
        return (2, None);
    }
    let entries = match (env.entries)() {
        Ok(entries) => entries,
        Err(e) => {
            (env.err)(&format!("{PREFIX}: could not read the graph: {e}"));
            return (2, None);
        }
    };
    let query = args.node.trim();
    let hit = if query.is_empty() {
        None
    } else {
        crate::backlog::node_ref::resolve_tiers(&entries, query)
    };
    let Some(hit) = hit else {
        (env.err)(&format!(
            "{PREFIX}: no node matches {} (id/slug/bare-hex).",
            crate::claude_ask::py_repr(&args.node)
        ));
        return (2, None);
    };
    let mut target = hit.clone();
    // The worker runs in the node's project root (work map), not whatever
    // stale cwd the node carries.
    let root = text(&target, "project")
        .and_then(env.project_root)
        .filter(|r| !r.is_empty());
    if let Some(obj) = target.as_object_mut() {
        if let Some(root) = root {
            obj.insert("_resolved_cwd".into(), json!(root));
        }
        if let Some(model) = &pre.model {
            obj.insert("model".into(), json!(model));
        }
        if let Some(provider) = &pre.provider {
            obj.insert("provider".into(), json!(provider));
        }
    }
    if pre.harness == "codex" {
        (env.err)("codex posture: think source=codex; dispatch=claude-bg-fallback");
    }
    let result = dispatch(&target, &pre);
    let (text, code) = render(&result, args.json);
    (env.out)(&text);
    (code, Some(result))
}

fn run_live(argv: &[String], anchor: &Path, out: &dyn Fn(&str), err: &dyn Fn(&str)) -> i32 {
    let (session_id, harness, cwd, _, _, _) =
        crate::backlog::create_cli::session_provenance(anchor, None, None);
    let entries = || {
        crate::graph_store::read_rows(&crate::backlog::settings::graph_path())
            .map_err(|e| e.to_string())
    };
    let env = CliEnv {
        provenance: Provenance {
            session_id,
            harness,
            cwd,
        },
        process_cwd: anchor.to_string_lossy().into_owned(),
        external_backend: crate::graph_get::external_backend_selected(),
        entries: &entries,
        project_root: &crate::backlog::intake::project_root_from_settings,
        out,
        err,
    };
    run_with(argv, &env, &|node: &Value, pre: &Preflight| {
        dispatch_live(node, pre, anchor, err)
    })
    .0
}

/// `fno-agents think-dispatch <argv after "fno do think dispatch">`.
pub fn run(args: &[String]) -> i32 {
    let anchor = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let out = |line: &str| println!("{line}");
    let err = |line: &str| eprintln!("{line}");
    run_live(args, &anchor, &out, &err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn the_parse_takes_every_option_spelling_and_refuses_the_rest() {
        let Ok(Parsed::Args(a)) = parse_args(&argv(&[
            "x-1",
            "--session-id=s",
            "-mglm",
            "-p",
            "claude",
            "-J",
            "--cwd",
            "/c",
        ])) else {
            panic!("expected args");
        };
        assert_eq!(a.node, "x-1");
        assert_eq!(a.session_id.as_deref(), Some("s"));
        assert_eq!(a.model.as_deref(), Some("glm"));
        assert_eq!(a.provider.as_deref(), Some("claude"));
        assert_eq!(a.cwd.as_deref(), Some("/c"));
        assert!(a.json);
        assert!(parse_args(&argv(&[])).is_err());
        assert!(parse_args(&argv(&["a", "b"])).is_err());
        assert!(parse_args(&argv(&["a", "--bogus", "v"])).is_err());
        assert!(parse_args(&argv(&["a", "--model"])).is_err());
        assert!(matches!(
            parse_args(&argv(&["--", "-dash"])),
            Ok(Parsed::Args(DispatchArgs { ref node, .. })) if node == "-dash"
        ));
    }
}
