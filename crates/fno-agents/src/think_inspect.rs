//! `fno do think inspect`: the read-only design-discovery receipt.
//!
//! The model still judges relevance and chooses a design. This module only
//! gathers facts whose truth should not depend on prompt compliance:
//! repository state, graph overlap, PR search availability, schema
//! grounding, and the project lesson corpus. Every external command is
//! argv-only and every missing source is typed. The receipt's key order and
//! byte shape are the deleted Python command's (`json.dumps(indent=2)`).

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::backlog::relatedness;

const COMMAND: &str = "fno do think inspect";
const COMMAND_TIMEOUT_S: u64 = 10;
// The receipt stays small without silently dropping an old ruling off the
// end unannounced (decisions_truncated).
const DECISIONS_CAP: usize = 20;
const DECISIONS_TEXT_WIDTH: usize = 200;
// The decisions door's default listing limit, which the receipt read rode.
const DECISIONS_DOOR_LIMIT: usize = 20;
// relatedness.rs keeps these private; the seed lane's floor is defined by
// exactly these two numbers.
const MIN_SCORE: f64 = 0.15;
const DOMAIN_BONUS: f64 = 0.10;

const NODE_SUMMARY_KEYS: [&str; 14] = [
    "id",
    "slug",
    "title",
    "status",
    "superseded_by",
    "deferred_kind",
    "domain",
    "project",
    "parent",
    "plan_path",
    "pr_number",
    "pr_url",
    "additional_prs",
    "merge_status",
];
// The gate reads id, score, reason, superseded_by; title/status/lanes let it
// judge liveness. The seed lane is floor-bounded, so rows stay lean.
const DUPLICATE_KEYS: [&str; 7] = [
    "id",
    "slug",
    "title",
    "status",
    "superseded_by",
    "deferred_kind",
    "domain",
];
const FIXED_DB_SIGNALS: [&str; 7] = [
    "prisma/schema.prisma",
    "drizzle.config.ts",
    "drizzle.config.js",
    "drizzle.config.mjs",
    "drizzle.config.cjs",
    "supabase/migrations",
    "drizzle",
];
const DB_ENV_FILES: [&str; 4] = [".env", ".env.local", ".env.development", ".env.test"];
const PITFALL_MARKER: &str = "## Pitfalls corpus (capped)";
const HELP: &str = "Usage: fno do think inspect [OPTIONS] SEED\n\n  Collect graph, PR, schema, history, and pitfall evidence without writes.\n\nArguments:\n  SEED  Node id, slug, or free-text design seed.  [required]\n\nOptions:\n  --repo TEXT  Repository to inspect (default: current repository).\n  -J, --json   Emit the versioned receipt as JSON.\n  --help       Show this message and exit.";

/// One command's answer: stdout on success, else the typed failure detail.
pub type Outcome = (Option<String>, Option<String>);

/// The injectable sources of one receipt. `None` reads the live source.
pub struct Inputs<'a> {
    /// Injected graph rows plus the error that stands for them when absent.
    pub graph: Option<(Option<Vec<Value>>, Option<String>)>,
    pub archive: Option<Vec<Value>>,
    pub plans_path: Option<PathBuf>,
    pub home: Option<PathBuf>,
    /// The worktree space dir that holds `codemap.md`.
    pub space_dir: Option<PathBuf>,
    pub run: &'a dyn Fn(&[&str], &Path) -> Outcome,
    pub decisions: &'a dyn Fn(&str) -> Result<Vec<Value>, String>,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn present<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get(key).filter(|x| !x.is_null())
}

fn truthy(v: Option<&Value>) -> bool {
    crate::backlog_ready::truthy(v)
}

/// Python `str(v)` for the shapes a row carries.
fn py_str(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python `str(v or "")`.
fn py_str_or_empty(v: Option<&Value>) -> String {
    match v {
        Some(x) if truthy(Some(x)) => py_str(x),
        _ => String::new(),
    }
}

fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// Python `str.splitlines()`: every line boundary Python recognizes.
fn py_splitlines(s: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let boundary = matches!(
            c,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if !boundary {
            continue;
        }
        lines.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some((j, '\n')) = chars.peek().copied() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        lines.push(&s[start..]);
    }
    lines
}

/// `Path.read_text(encoding="utf-8")`: strict UTF-8, universal newlines.
fn read_text(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let raw = String::from_utf8(bytes).ok()?;
    Some(raw.replace("\r\n", "\n").replace('\r', "\n"))
}

fn regex(pattern: &str) -> Option<regex::Regex> {
    regex::Regex::new(pattern).ok()
}

// ---------------------------------------------------------------------------
// repository
// ---------------------------------------------------------------------------

/// The repository section from the four git answers, in argv order:
/// branch, head, status, log.
pub fn repository_section(
    root: &str,
    branch: &Outcome,
    head: &Outcome,
    status: &Outcome,
    log: &Outcome,
) -> Value {
    let mut errors: Vec<String> = Vec::new();
    for (_, error) in [branch, head, status, log] {
        if let Some(e) = error.as_deref().filter(|e| !e.is_empty()) {
            if !errors.iter().any(|seen| seen == e) {
                errors.push(e.to_string());
            }
        }
    }
    let mut dirty_paths: Vec<Value> = Vec::new();
    for line in py_splitlines(status.0.as_deref().unwrap_or("")) {
        let mut path = if line.chars().count() >= 4 {
            let tail: String = line.chars().skip(3).collect();
            py_strip(&tail).to_string()
        } else {
            py_strip(line).to_string()
        };
        if let Some(renamed) = path.split_once(" -> ").map(|(_, r)| r.to_string()) {
            path = renamed;
        }
        if !path.is_empty() {
            dirty_paths.push(json!(path));
        }
    }
    let mut commits: Vec<Value> = Vec::new();
    for line in py_splitlines(log.0.as_deref().unwrap_or("")) {
        let (sha, subject) = line.split_once('\t').unwrap_or((line, ""));
        if !sha.is_empty() {
            commits.push(json!({"sha": sha, "subject": subject}));
        }
    }
    let stripped = |o: &Outcome| -> Value {
        let s = py_strip(o.0.as_deref().unwrap_or(""));
        if s.is_empty() {
            Value::Null
        } else {
            json!(s)
        }
    };
    json!({
        "root": root,
        "status": if errors.is_empty() { "ok" } else { "error" },
        "branch": stripped(branch),
        "head": stripped(head),
        "dirty_paths": dirty_paths,
        "recent_commits": commits,
        "detail": if errors.is_empty() { Value::Null } else { json!(errors.join("; ")) },
    })
}

/// One external command under the ten-second budget. A gh spawn goes
/// through the fleet budget's admission inside `bounded_cmd`.
fn real_command(argv: &[&str], repo: &Path) -> Outcome {
    use std::os::unix::process::ExitStatusExt;
    let Some((program, rest)) = argv.split_first() else {
        return (None, Some("empty command".to_string()));
    };
    let mut cmd = std::process::Command::new(*program);
    cmd.args(rest).current_dir(repo);
    let started = std::time::Instant::now();
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, COMMAND_TIMEOUT_S) {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return (None, Some(format!("{program} not found")));
        }
        Err(e) => return (None, Some(e.to_string())),
    };
    if out.status.code().is_none()
        && started.elapsed() >= std::time::Duration::from_secs(COMMAND_TIMEOUT_S)
    {
        return (None, Some(format!("{program} timed out")));
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return (Some(stdout), None);
    }
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let code = out
        .status
        .code()
        .or_else(|| out.status.signal().map(|s| -s))
        .unwrap_or(-1);
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit {code}")
    };
    (None, Some(py_strip(&detail).to_string()))
}

// ---------------------------------------------------------------------------
// pull requests
// ---------------------------------------------------------------------------

/// The `gh pr list` answer typed: unavailable, malformed, or the matches.
pub fn pull_requests_section(outcome: &Outcome) -> Value {
    if let Some(e) = outcome.1.as_deref().filter(|e| !e.is_empty()) {
        return json!({"status": "unavailable", "matches": [], "detail": e});
    }
    let raw = outcome
        .0
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or("[]");
    match serde_json::from_str::<Value>(raw) {
        Err(e) => json!({
            "status": "error",
            "matches": [],
            "detail": format!("invalid gh JSON: {e}"),
        }),
        Ok(Value::Array(items)) => json!({"status": "ok", "matches": items, "detail": null}),
        Ok(_) => json!({
            "status": "error",
            "matches": [],
            "detail": "gh returned a non-list payload",
        }),
    }
}

// ---------------------------------------------------------------------------
// database
// ---------------------------------------------------------------------------

fn collect_mtimes(dir: &Path, out: &mut Vec<std::time::SystemTime>) -> std::io::Result<()> {
    // An unreadable directory contributes nothing (rglob's own degrade); a
    // file whose stat fails reads as stale.
    let Ok(read) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in read.flatten() {
        let path = entry.path();
        let is_link = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if path.is_dir() {
            if !is_link {
                collect_mtimes(&path, out)?;
            }
        } else if path.is_file() {
            out.push(std::fs::metadata(&path)?.modified()?);
        }
    }
    Ok(())
}

fn schema_is_stale(artifact: &Path, signal_paths: &[PathBuf]) -> std::io::Result<bool> {
    let artifact_mtime = std::fs::metadata(artifact)?.modified()?;
    let mut mtimes = Vec::new();
    for path in signal_paths {
        if path.is_dir() {
            collect_mtimes(path, &mut mtimes)?;
        } else if path.is_file() {
            mtimes.push(std::fs::metadata(path)?.modified()?);
        }
    }
    Ok(mtimes
        .iter()
        .max()
        .is_some_and(|newest| *newest > artifact_mtime))
}

/// Database detection and schema grounding against the space's codemap.
pub fn database_section(repo: &Path, artifact: Option<&Path>) -> Value {
    let mut signals: BTreeSet<String> = BTreeSet::new();
    let mut signal_paths: Vec<PathBuf> = Vec::new();
    for relative in FIXED_DB_SIGNALS {
        let path = repo.join(relative);
        if path.exists() {
            signals.insert(relative.to_string());
            signal_paths.push(path);
        }
    }
    if let Some(re) = regex(r"(?m)^\s*(DATABASE_URL|SUPABASE_DB_URL|POSTGRES_URL|DIRECT_URL)\s*=") {
        for name in DB_ENV_FILES {
            let path = repo.join(name);
            let Some(body) = read_text(&path) else {
                continue;
            };
            let mut hit = false;
            for caps in re.captures_iter(&body) {
                hit = true;
                if let Some(key) = caps.get(1) {
                    signals.insert(format!("{name}:{}", key.as_str()));
                }
            }
            if hit {
                signal_paths.push(path);
            }
        }
    }
    let grounded = artifact
        .and_then(read_text)
        .is_some_and(|body| body.contains("## Database Schema"));
    let stale = match artifact {
        Some(path) if grounded => schema_is_stale(path, &signal_paths).unwrap_or(true),
        _ => false,
    };
    let status = if stale {
        "stale"
    } else if grounded {
        "grounded"
    } else if !signals.is_empty() {
        "missing"
    } else {
        "not-applicable"
    };
    let artifact_exists = artifact.is_some_and(Path::exists);
    json!({
        "detected": !signals.is_empty() || grounded,
        "signals": signals.into_iter().collect::<Vec<String>>(),
        "schema_artifact": if artifact_exists { json!("codemap.md") } else { Value::Null },
        "schema_status": status,
    })
}

// ---------------------------------------------------------------------------
// pitfalls
// ---------------------------------------------------------------------------

/// The `###` headings of the capped pitfalls section, or None when the
/// marker is absent.
pub fn pitfall_headings_in(body: &str) -> Option<Vec<String>> {
    let start = body.find(PITFALL_MARKER)?;
    let tail = &body[start + PITFALL_MARKER.len()..];
    let section = match regex(r"(?m)^## (?:[^#]|\z)").and_then(|re| re.find(tail)) {
        Some(m) => &tail[..m.start()],
        None => tail,
    };
    let headings = match regex(r"(?m)^###\s+(.+?)\s*$") {
        Some(re) => re
            .captures_iter(section)
            .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
            .collect(),
        None => Vec::new(),
    };
    Some(headings)
}

/// The pitfall source file and its headings: AGENTS.md, then CLAUDE.md.
pub fn pitfall_headings(repo: &Path) -> (String, Vec<String>) {
    for name in ["AGENTS.md", "CLAUDE.md"] {
        let Some(body) = read_text(&repo.join(name)) else {
            continue;
        };
        if let Some(headings) = pitfall_headings_in(&body) {
            return (name.to_string(), headings);
        }
    }
    ("missing".to_string(), Vec::new())
}

fn is_retro_synthesis(name: &str) -> bool {
    name.strip_suffix(".md")
        .is_some_and(|stem| stem.contains("retro-synthesis"))
}

/// The lesson corpus: pitfall headings, newest retro syntheses, and the
/// pending lesson-candidate count.
pub fn pitfalls_section(repo: &Path, plans_path: &Path, home: &Path) -> Value {
    let (source, entries) = pitfall_headings(repo);
    let mut retros: Vec<PathBuf> = Vec::new();
    if plans_path.is_dir() {
        if let Ok(read) = std::fs::read_dir(plans_path) {
            for entry in read.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if is_retro_synthesis(&name) {
                    retros.push(plans_path.join(name));
                }
            }
        }
    }
    retros.sort();
    retros.reverse();
    retros.truncate(5);
    let candidates = home
        .join(".fno")
        .join("history")
        .join("lesson-candidates.jsonl");
    let count = read_text(&candidates)
        .map(|body| {
            py_splitlines(&body)
                .into_iter()
                .filter(|l| !py_strip(l).is_empty())
                .count()
        })
        .unwrap_or(0);
    json!({
        "source": source,
        "entries": entries,
        "retro_syntheses": retros
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<String>>(),
        "lesson_candidates": count,
    })
}

// ---------------------------------------------------------------------------
// graph
// ---------------------------------------------------------------------------

/// The node summary the gate reads: the named fields that carry a value,
/// plus `archived` for a row served only from the archive.
pub fn node_summary(entry: &Value, archived: bool) -> Value {
    let mut out = Map::new();
    for key in NODE_SUMMARY_KEYS {
        if let Some(v) = present(entry, key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    if archived {
        out.insert("archived".to_string(), Value::Bool(true));
    }
    Value::Object(out)
}

fn no_decisions() -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("decisions".to_string(), json!([]));
    out.insert("decisions_status".to_string(), json!("ok"));
    out.insert("decisions_truncated".to_string(), json!(false));
    out.insert("decisions_detail".to_string(), Value::Null);
    out
}

fn extend(map: &mut Map<String, Value>, other: Map<String, Value>) {
    for (k, v) in other {
        map.insert(k, v);
    }
}

/// The resolved node's own live rulings, newest first, from the rows the
/// decisions door answers. A ruling withdrawn via `--supersedes` drops on
/// the derived `superseded_by` field, never on its own prose; an unreadable
/// index is an error, never folded into an empty list.
pub fn decisions_section(rows: Result<Vec<Value>, String>) -> Map<String, Value> {
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => {
            let mut out = Map::new();
            out.insert("decisions".to_string(), json!([]));
            out.insert("decisions_status".to_string(), json!("error"));
            out.insert("decisions_truncated".to_string(), json!(false));
            out.insert(
                "decisions_detail".to_string(),
                json!(format!("decision index unreadable: {e}")),
            );
            return out;
        }
    };
    let unknown: Vec<&Value> = rows
        .iter()
        .filter(|r| r.get("lifecycle").and_then(Value::as_str) == Some("unknown"))
        .collect();
    let live: Vec<&Value> = rows
        .iter()
        .filter(|r| !truthy(r.get("superseded_by")))
        .collect();
    let field = |row: &Value, key: &str| row.get(key).cloned().unwrap_or(Value::Null);
    let mut out_rows: Vec<Value> = Vec::new();
    for row in live.iter().take(DECISIONS_CAP) {
        let mut body = py_str_or_empty(row.get("decision"));
        if body.chars().count() > DECISIONS_TEXT_WIDTH {
            body = body.chars().take(DECISIONS_TEXT_WIDTH - 1).collect();
            body.push('\u{2026}');
        }
        out_rows.push(json!({
            "decision_id": field(row, "decision_id"),
            "ts": field(row, "ts"),
            "lane": field(row, "lane"),
            "subject": field(row, "subject"),
            "text": body,
            "lifecycle": field(row, "lifecycle"),
        }));
    }
    let detail = match unknown.first() {
        Some(first) => json!(format!(
            "{}; {} coord ruling(s) listed with an unknown lifecycle",
            py_str(first.get("lifecycle_reason").unwrap_or(&Value::Null)),
            unknown.len()
        )),
        None => Value::Null,
    };
    let mut out = Map::new();
    out.insert("decisions".to_string(), json!(out_rows));
    out.insert(
        "decisions_status".to_string(),
        json!(if unknown.is_empty() { "ok" } else { "error" }),
    );
    out.insert(
        "decisions_truncated".to_string(),
        json!(live.len() > DECISIONS_CAP),
    );
    out.insert("decisions_detail".to_string(), detail);
    out
}

/// The deterministic resolution tiers, exact only: id, slug (case
/// insensitive), bare 4-8 lowercase hex re-prefixed by the configured
/// prefix then `ab-`. A miss leaves the seed as free text.
pub fn resolve_exact<'a>(query: &str, entries: &'a [Value]) -> Option<&'a Value> {
    let q = py_strip(query);
    if q.is_empty() || q.starts_with('-') {
        return None;
    }
    if let Some(hit) = entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(q))
    {
        return Some(hit);
    }
    let q_lc = q.to_lowercase();
    if let Some(hit) = entries.iter().find(|e| {
        e.get("slug")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty() && s == q_lc)
    }) {
        return Some(hit);
    }
    let bare_hex =
        (4..=8).contains(&q.len()) && q.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if bare_hex {
        let mut prefixes = vec![crate::backlog::settings::node_id_prefix()];
        prefixes.push("ab-".to_string());
        prefixes.dedup();
        for prefix in &prefixes {
            let candidate = format!("{prefix}{q}");
            // Last row wins, the dict-by-id the Python tier built.
            if let Some(hit) = entries
                .iter()
                .rev()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(candidate.as_str()))
            {
                return Some(hit);
            }
        }
    }
    None
}

/// Rows keyed by id in first-seen order, the last duplicate's value kept.
fn by_id(rows: &[Value]) -> Map<String, Value> {
    let mut out = Map::new();
    for row in rows {
        if let Some(id) = row.get("id").and_then(Value::as_str) {
            out.insert(id.to_string(), row.clone());
        }
    }
    out
}

/// The tokenizer's count for one row. score_pair against itself reports
/// every token as shared, so the one tokenizer stays relatedness's own.
fn token_count(row: &Value) -> usize {
    let (_, reason) = relatedness::score_pair(row, row, false);
    reason
        .split_once(" shared terms (")
        .and_then(|(n, _)| n.parse().ok())
        .unwrap_or(0)
}

struct Ranked {
    rows: Vec<(String, f64, String, Vec<&'static str>)>,
    degraded: bool,
    warning: Option<String>,
}

/// discovery.candidates: the fts lane unioned with relatedness recall,
/// ranked by relatedness score. `limit` None keeps every row above the
/// floor.
#[allow(clippy::too_many_arguments)]
fn discovery_candidates(
    title: &str,
    details: &str,
    entries: &[Value],
    graph_path: Option<&Path>,
    exclude_id: Option<&str>,
    limit: Option<usize>,
    floor: f64,
    domain: &str,
) -> Ranked {
    let pool: Vec<Value> = entries
        .iter()
        .filter(|e| {
            e.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| Some(id) != exclude_id)
        })
        .cloned()
        .collect();
    let mut pool_by_id: BTreeMap<String, &Value> = BTreeMap::new();
    for row in &pool {
        pool_by_id.insert(text(row, "id").to_string(), row);
    }
    let incoming = json!({
        "id": "__incoming__",
        "title": title,
        "details": details,
        "domain": domain,
    });
    let mut fts_ids: BTreeSet<String> = BTreeSet::new();
    let mut degraded = false;
    let mut warning: Option<String> = None;
    match graph_path {
        None => {
            degraded = true;
            warning = Some(
                "graph entries were injected without the graph file; the fts lane is unavailable"
                    .to_string(),
            );
        }
        Some(path) => {
            let query = [title, details]
                .iter()
                .filter(|p| !p.is_empty())
                .copied()
                .collect::<Vec<&str>>()
                .join(" ");
            let store = crate::backlog::api::Store::new(path);
            match crate::backlog::search::search(&store, &query, None) {
                Ok(ids) => {
                    fts_ids.extend(ids.into_iter().filter(|id| pool_by_id.contains_key(id)));
                }
                Err(e) => {
                    degraded = true;
                    warning = Some(e.0);
                }
            }
        }
    }
    let related: BTreeMap<String, (f64, String)> =
        relatedness::similar_nodes(&incoming, &pool, usize::MAX, Some(floor))
            .into_iter()
            .filter(|(id, _, _)| pool_by_id.contains_key(id))
            .map(|(id, score, reason)| (id, (score, reason)))
            .collect();
    let mut all_ids: BTreeSet<String> = fts_ids.clone();
    all_ids.extend(related.keys().cloned());
    let mut rows: Vec<(String, f64, String, Vec<&'static str>)> = Vec::new();
    for node_id in all_ids {
        let (score, reason) = match related.get(&node_id) {
            Some((score, reason)) => (*score, reason.clone()),
            None => match pool_by_id.get(&node_id) {
                Some(row) => relatedness::score_pair(&incoming, row, false),
                None => continue,
            },
        };
        let mut lanes: Vec<&'static str> = Vec::new();
        if fts_ids.contains(&node_id) {
            lanes.push("fts");
        }
        if related.contains_key(&node_id) {
            lanes.push("relatedness");
        }
        rows.push((node_id, score, reason, lanes));
    }
    rows.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    if let Some(limit) = limit {
        rows.truncate(limit);
    }
    Ranked {
        rows,
        degraded,
        warning,
    }
}

/// The graph section: resolution, ranked duplicate recall, rollup
/// candidates, parent, closure, and the resolved node's live rulings.
/// `graph_path` is the store the fts lane reads; injected rows pass None.
pub fn graph_section(
    seed: &str,
    entries: Option<&[Value]>,
    archive: &[Value],
    error: Option<&str>,
    archive_error: Option<&str>,
    graph_path: Option<&Path>,
    decisions: &dyn Fn(&str) -> Result<Vec<Value>, String>,
) -> Value {
    let archive_error = archive_error.filter(|e| !e.is_empty());
    let archive_status = if archive_error.is_some() {
        "error"
    } else {
        "ok"
    };
    let Some(entries) = entries else {
        let mut out = Map::new();
        out.insert("status".to_string(), json!("error"));
        out.insert("resolved".to_string(), Value::Null);
        out.insert("parent".to_string(), Value::Null);
        out.insert("closure".to_string(), Value::Null);
        out.insert("duplicates".to_string(), json!([]));
        out.insert("epic_candidates".to_string(), json!([]));
        out.insert("archive_status".to_string(), json!(archive_status));
        out.insert(
            "detail".to_string(),
            json!(error
                .filter(|e| !e.is_empty())
                .unwrap_or("graph unavailable")),
        );
        extend(&mut out, no_decisions());
        return Value::Object(out);
    };

    let active_by_id = by_id(entries);
    let archive_by_id = by_id(archive);
    let mut combined: Vec<Value> = active_by_id.values().cloned().collect();
    combined.extend(
        archive_by_id
            .iter()
            .filter(|(id, _)| !active_by_id.contains_key(id.as_str()))
            .map(|(_, row)| row.clone()),
    );
    let archived_only = |id: &str| archive_by_id.contains_key(id) && !active_by_id.contains_key(id);
    let row_for = |id: &str| active_by_id.get(id).or_else(|| archive_by_id.get(id));

    let resolved: Option<Value> = resolve_exact(seed, &combined).cloned();
    let probe = resolved.clone().unwrap_or_else(
        || json!({"id": "__think_seed__", "title": seed, "slug": seed, "details": seed}),
    );
    // A free-text seed has no domain to share, so it can never earn the
    // domain bonus a resolved row can; its floor drops by exactly that bonus.
    // The seed lane takes no cap: a small k lets higher-Jaccard rows evict
    // the fts-only family the lane exists to recover.
    let (floor, limit) = match &resolved {
        Some(_) => (MIN_SCORE, Some(5)),
        None => (MIN_SCORE - DOMAIN_BONUS, None),
    };
    let (probe_title, probe_details, probe_domain) = match &resolved {
        Some(r) => (
            py_str_or_empty(r.get("title")),
            py_str_or_empty(r.get("details")),
            py_str_or_empty(r.get("domain")),
        ),
        None => (seed.to_string(), seed.to_string(), String::new()),
    };
    // Lineage leaves the pool before the cap: a post-cap filter would let an
    // ancestor consume a node-lane slot and evict a legitimate duplicate.
    let lineage: BTreeSet<String> = match &resolved {
        Some(r) => {
            let mut merged: BTreeMap<String, Value> = BTreeMap::new();
            for (id, row) in archive_by_id.iter().chain(active_by_id.iter()) {
                merged.insert(id.clone(), row.clone());
            }
            relatedness::lineage_ids(r, &merged)
        }
        None => BTreeSet::new(),
    };
    let pool: Vec<Value> = combined
        .iter()
        .filter(|row| {
            !row.get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| lineage.contains(id))
        })
        .cloned()
        .collect();
    let exclude_id = resolved
        .as_ref()
        .map(|r| py_str(r.get("id").unwrap_or(&Value::Null)));
    let ranked = discovery_candidates(
        &probe_title,
        &probe_details,
        &pool,
        graph_path,
        exclude_id.as_deref(),
        limit,
        floor,
        &probe_domain,
    );

    let mut duplicates: Vec<Value> = Vec::new();
    for (node_id, score, reason, lanes) in &ranked.rows {
        let Some(row) = row_for(node_id) else {
            continue;
        };
        let mut summary = Map::new();
        for key in DUPLICATE_KEYS {
            if let Some(v) = present(row, key) {
                summary.insert(key.to_string(), v.clone());
            }
        }
        if archived_only(node_id) {
            summary.insert("archived".to_string(), Value::Bool(true));
        }
        summary.insert("score".to_string(), json!(score));
        summary.insert("reason".to_string(), json!(reason));
        summary.insert("lanes".to_string(), json!(lanes));
        duplicates.push(Value::Object(summary));
    }

    let mut rollups: Vec<Value> = Vec::new();
    for (node_id, score, reason) in relatedness::epic_candidates(&probe, &combined, 3, Some(floor))
    {
        let Some(row) = row_for(&node_id) else {
            continue;
        };
        let mut summary = node_summary(row, archived_only(&node_id));
        if let Some(obj) = summary.as_object_mut() {
            obj.insert("score".to_string(), json!(score));
            obj.insert("reason".to_string(), json!(reason));
        }
        rollups.push(summary);
    }

    let parent = resolved
        .as_ref()
        .and_then(|r| r.get("parent").and_then(Value::as_str))
        .and_then(|pid| row_for(pid).map(|row| node_summary(row, archived_only(pid))))
        .unwrap_or(Value::Null);

    // A done/superseded row here is a halt signal for the consolidation
    // gate, so the fields are read as stored on the row.
    let field = |row: &Value, key: &str| row.get(key).cloned().unwrap_or(Value::Null);
    let closure = match &resolved {
        Some(r) => json!({
            "status": field(r, "status"),
            "pr_number": field(r, "pr_number"),
            "superseded_by": field(r, "superseded_by"),
            "deferred_kind": field(r, "deferred_kind"),
        }),
        None => Value::Null,
    };
    let resolved_summary = match &resolved {
        Some(r) => node_summary(r, archived_only(text(r, "id"))),
        None => Value::Null,
    };

    let mut out = Map::new();
    out.insert(
        "status".to_string(),
        json!(if archive_error.is_some() {
            "partial"
        } else {
            "ok"
        }),
    );
    out.insert("resolved".to_string(), resolved_summary);
    out.insert("parent".to_string(), parent);
    out.insert("closure".to_string(), closure);
    out.insert("duplicates".to_string(), json!(duplicates));
    out.insert("epic_candidates".to_string(), json!(rollups));
    out.insert("archive_status".to_string(), json!(archive_status));
    out.insert(
        "detail".to_string(),
        match archive_error {
            Some(e) => json!(format!("archive unreadable: {e}")),
            None => Value::Null,
        },
    );
    out.insert(
        "recall".to_string(),
        json!({
            "lane": if resolved.is_some() { "node" } else { "seed" },
            "seed_tokens": token_count(&probe),
            "floor": floor,
            "fts": if ranked.degraded { "degraded" } else { "ok" },
            "fts_warning": if ranked.degraded { json!(ranked.warning) } else { Value::Null },
        }),
    );
    let node_id = resolved
        .as_ref()
        .map(|r| py_str_or_empty(r.get("id")))
        .unwrap_or_default();
    let section = if node_id.is_empty() {
        no_decisions()
    } else {
        decisions_section(decisions(&node_id))
    };
    extend(&mut out, section);
    Value::Object(out)
}

/// Sibling plans' `consolidation.rejected` rows naming the node. A scan
/// that could not run shows in status/detail, never as a clean "no ruling".
pub fn plan_rulings_section(node_id: Option<&str>, plans_dir: Option<&Path>) -> Map<String, Value> {
    let mut out = Map::new();
    let node_id = node_id.filter(|id| !id.is_empty());
    let (rulings, status, detail) = match (node_id, plans_dir) {
        (Some(id), Some(dir)) => {
            let result = crate::backlog::decisions_cli::plan_rulings_in(dir, id);
            if text(&result, "status") == "unavailable" {
                (
                    json!([]),
                    "error",
                    json!(format!(
                        "plans dir unreadable: {}: {}",
                        text(&result, "dir"),
                        py_str(result.get("detail").unwrap_or(&Value::Null))
                    )),
                )
            } else {
                (
                    result.get("rulings").cloned().unwrap_or_else(|| json!([])),
                    "ok",
                    Value::Null,
                )
            }
        }
        (Some(_), None) => (
            json!([]),
            "error",
            json!("plans dir unreadable: the plans directory could not be resolved"),
        ),
        (None, _) => (json!([]), "ok", Value::Null),
    };
    out.insert("plan_rulings".to_string(), rulings);
    out.insert("plan_rulings_status".to_string(), json!(status));
    out.insert("plan_rulings_detail".to_string(), detail);
    out
}

// ---------------------------------------------------------------------------
// live sources
// ---------------------------------------------------------------------------

/// The whole-graph read, archived residents hidden and the persisted status
/// shown, the way the keeper's wire rows read. An error keeps "graph
/// unreadable" apart from "node absent".
fn load_graph(path: &Path) -> Result<Vec<Value>, String> {
    let rows =
        crate::backlog::api::rows(&crate::backlog::api::Store::new(path)).map_err(|e| e.0)?;
    Ok(rows
        .into_iter()
        .filter(|row| !truthy(row.get("archived_at")))
        .map(|mut row| {
            let persisted = row
                .get("persisted_status")
                .filter(|v| truthy(Some(*v)))
                .cloned();
            if let (Some(status), Some(obj)) = (persisted, row.as_object_mut()) {
                obj.insert("status".to_string(), status);
            }
            row
        })
        .collect())
}

fn graph_path_for(repo: &Path) -> PathBuf {
    let root = crate::agents_config::state_dir(repo).unwrap_or_else(|| PathBuf::from(".fno"));
    crate::state_layout::graph_anchor(&root)
}

fn configured_plans_path(repo: &Path) -> PathBuf {
    let raw = crate::agents_config::config_lookup(repo, &["plans_dir"])
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_else(|| ".fno/plans/".to_string());
    crate::plans_path::resolve_template(&raw, Some(repo))
        .unwrap_or_else(|_| repo.join(".fno").join("plans"))
}

/// The decisions listing for one subject at `--state live`: the owner's read
/// in decisions_cli, capped at the door's default limit.
fn live_rows_for(subject: &str, rows: Vec<Value>, entries: &mut Option<Vec<Value>>) -> Vec<Value> {
    let mut kept = crate::backlog::decisions_cli::live_rows_over(rows, entries, subject);
    kept.truncate(DECISIONS_DOOR_LIMIT);
    kept
}

/// The resolved node's live rulings from the machine decision store.
fn live_decisions(node_id: &str) -> Result<Vec<Value>, String> {
    let mut entries = match crate::backlog::decisions_cli::read_graph_entries() {
        Ok(rows) if !rows.is_empty() => Some(rows),
        _ => None,
    };
    let rows = match crate::decision_index::read_store_rows(
        &crate::graph_get::default_graph_path(),
        &crate::decision_index::default_state_path("decisions.jsonl"),
    ) {
        Ok((rows, _)) => rows,
        // No store on this machine is an honest empty, not a failed read.
        Err(reason) if reason.starts_with("no decision store") => Vec::new(),
        Err(reason) => return Err(format!("cannot read the decision index: {reason}")),
    };
    Ok(live_rows_for(node_id, rows, &mut entries))
}

// ---------------------------------------------------------------------------
// the receipt
// ---------------------------------------------------------------------------

/// Collect a deterministic, read-only design-discovery receipt.
pub fn build_receipt(seed: &str, repo: &Path, inputs: &Inputs<'_>) -> Value {
    let repo = crate::paths::resolve_loose(repo);
    let needs_file = inputs.graph.is_none() || inputs.archive.is_none();
    let graph_path = if needs_file {
        Some(graph_path_for(&repo))
    } else {
        None
    };
    let (graph_entries, graph_error, graph_from_file) = match (&inputs.graph, &graph_path) {
        (Some((entries, error)), _) => (entries.clone(), error.clone(), false),
        (None, Some(path)) => match load_graph(path) {
            Ok(rows) => (Some(rows), None, true),
            Err(e) => (None, Some(e), true),
        },
        (None, None) => (None, Some("graph unavailable".to_string()), false),
    };
    let (archive, archive_error) = match (&inputs.archive, &graph_path) {
        (Some(rows), _) => (rows.clone(), None),
        (None, Some(path)) => {
            let archive_path = path
                .parent()
                .map(|p| p.join("graph-archive.json"))
                .unwrap_or_else(|| PathBuf::from("graph-archive.json"));
            if archive_path.exists() {
                match load_graph(&archive_path) {
                    Ok(rows) => (rows, None),
                    Err(e) => (Vec::new(), Some(e)),
                }
            } else {
                (Vec::new(), None)
            }
        }
        (None, None) => (Vec::new(), None),
    };
    // Injected entries own their pool; the fts lane indexes the graph file,
    // so it runs only when the pool actually came from that file.
    let fts_path = if graph_from_file {
        graph_path.as_deref()
    } else {
        None
    };
    let graph_value = graph_section(
        seed,
        graph_entries.as_deref(),
        &archive,
        graph_error.as_deref(),
        archive_error.as_deref(),
        fts_path,
        inputs.decisions,
    );
    let mut graph = graph_value.as_object().cloned().unwrap_or_default();
    let resolved_id = graph
        .get("resolved")
        .and_then(|r| r.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let rulings_dir = inputs
        .plans_path
        .clone()
        .or_else(|| crate::plans_path::plans_content_dir(&repo));
    extend(
        &mut graph,
        plan_rulings_section(resolved_id.as_deref(), rulings_dir.as_deref()),
    );

    let run = |argv: &[&str]| (inputs.run)(argv, &repo);
    let repository = repository_section(
        &repo.display().to_string(),
        &run(&["git", "rev-parse", "--abbrev-ref", "HEAD"]),
        &run(&["git", "rev-parse", "HEAD"]),
        &run(&["git", "status", "--porcelain"]),
        &run(&["git", "log", "-5", "--pretty=format:%h%x09%s"]),
    );
    let pr_seed = graph
        .get("resolved")
        .and_then(|r| r.get("title"))
        .and_then(Value::as_str)
        .unwrap_or(seed)
        .to_string();
    let pull_requests = pull_requests_section(&run(&[
        "gh",
        "pr",
        "list",
        "--state",
        "all",
        "--search",
        pr_seed.as_str(),
        "--limit",
        "20",
        "--json",
        "number,title,state,url",
    ]));
    let artifact = inputs
        .space_dir
        .clone()
        .or_else(|| crate::paths::worktree_space_dir_opt(&repo))
        .map(|dir| dir.join("codemap.md"));
    let database = database_section(&repo, artifact.as_deref());
    let pitfalls_plans = inputs
        .plans_path
        .clone()
        .unwrap_or_else(|| configured_plans_path(&repo));
    let home = inputs.home.clone().unwrap_or_else(crate::paths::dirs_home);
    let pitfalls = pitfalls_section(&repo, &pitfalls_plans, &home);

    let mut warnings: Vec<String> = Vec::new();
    if text(&repository, "status") != "ok" {
        warnings.push("repository evidence is incomplete".to_string());
    }
    if graph.get("status").and_then(Value::as_str) != Some("ok") {
        warnings.push("backlog graph evidence is unavailable".to_string());
    }
    if text(&pull_requests, "status") != "ok" {
        warnings.push("pull request evidence is unavailable".to_string());
    }
    match text(&database, "schema_status") {
        "missing" => warnings.push("database schema evidence is missing".to_string()),
        "stale" => warnings.push("database schema evidence is stale".to_string()),
        _ => {}
    }
    let recall = graph.get("recall").cloned().unwrap_or(Value::Null);
    let no_duplicates = !graph
        .get("duplicates")
        .and_then(Value::as_array)
        .is_some_and(|d| !d.is_empty());
    if text(&recall, "lane") == "seed" && no_duplicates {
        warnings.push(
            "seed lane returned no candidates; this is not a measured absence. \
             Widen the seed to the design body, or run: fno backlog find '<2-3 salient terms>'"
                .to_string(),
        );
    }
    if text(&recall, "fts") == "degraded" {
        let why = recall
            .get("fts_warning")
            .filter(|w| truthy(Some(*w)))
            .map(py_str)
            .unwrap_or_else(|| "the lane did not run".to_string());
        warnings.push(format!("duplicate recall ran without the fts lane: {why}"));
    }
    json!({
        "version": 1,
        "seed": seed,
        "complete": warnings.is_empty(),
        "repository": repository,
        "graph": Value::Object(graph),
        "pull_requests": pull_requests,
        "database": database,
        "pitfalls": pitfalls,
        "warnings": warnings,
    })
}

fn len_of(v: &Value, key: &str) -> usize {
    v.get(key).and_then(Value::as_array).map_or(0, Vec::len)
}

/// Python f-string interpolation of an optional field with a fallback for
/// a missing key.
fn shown(v: &Value, key: &str, missing: &str) -> String {
    match v.get(key) {
        Some(x) => py_str(x),
        None => missing.to_string(),
    }
}

/// Render the receipt for a human without hiding typed source status.
pub fn render_receipt(receipt: &Value) -> String {
    let empty = Value::Object(Map::new());
    let repo = receipt.get("repository").unwrap_or(&empty);
    let graph = receipt.get("graph").unwrap_or(&empty);
    let prs = receipt.get("pull_requests").unwrap_or(&empty);
    let database = receipt.get("database").unwrap_or(&empty);
    let pitfalls = receipt.get("pitfalls").unwrap_or(&empty);
    let resolved = graph
        .get("resolved")
        .filter(|r| truthy(Some(*r)))
        .unwrap_or(&empty);
    let recall = graph.get("recall").unwrap_or(&empty);
    let or_dash = |v: &Value, key: &str| match v.get(key) {
        Some(x) if truthy(Some(x)) => py_str(x),
        _ => "-".to_string(),
    };
    let complete = receipt
        .get("complete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let detected = database
        .get("detected")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    // "candidates", never "duplicates": the list is ranked at the reading
    // floor, so a seed with no true duplicate still returns a full top-K.
    let mut lines = vec![
        format!(
            "think inspection: {}",
            if complete { "complete" } else { "incomplete" }
        ),
        format!(
            "repository: {} branch={} head={} dirty={}",
            shown(repo, "status", ""),
            or_dash(repo, "branch"),
            or_dash(repo, "head"),
            len_of(repo, "dirty_paths")
        ),
        format!(
            "graph: {} lane={} tokens={} resolved={} candidates={} (ranked, judge before trusting) epics={}",
            shown(graph, "status", ""),
            shown(recall, "lane", "-"),
            shown(recall, "seed_tokens", "-"),
            shown(resolved, "id", "-"),
            len_of(graph, "duplicates"),
            len_of(graph, "epic_candidates")
        ),
        format!(
            "pull requests: {} matches={}",
            shown(prs, "status", ""),
            len_of(prs, "matches")
        ),
        format!(
            "database: detected={} schema={}",
            detected,
            shown(database, "schema_status", "")
        ),
        format!(
            "pitfalls: source={} entries={} syntheses={} candidates={}",
            shown(pitfalls, "source", ""),
            len_of(pitfalls, "entries"),
            len_of(pitfalls, "retro_syntheses"),
            shown(pitfalls, "lesson_candidates", "0")
        ),
    ];
    if let Some(warnings) = receipt.get("warnings").and_then(Value::as_array) {
        for warning in warnings {
            lines.push(format!("warning: {}", py_str(warning)));
        }
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// the command
// ---------------------------------------------------------------------------

fn usage_error(message: &str) -> i32 {
    eprintln!("Usage: {COMMAND} [OPTIONS] SEED");
    eprintln!("Try '{COMMAND} --help' for help.");
    eprintln!();
    eprintln!("Error: {message}");
    2
}

fn absolute(path: PathBuf) -> PathBuf {
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    };
    crate::paths::resolve_loose(&path)
}

/// `resolve_repo_root`: the `FNO_REPO_ROOT` pin, then the git toplevel,
/// then the cwd.
fn default_repo_root() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        return absolute(PathBuf::from(pin));
    }
    let toplevel = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    match toplevel {
        Some(root) => absolute(PathBuf::from(root)),
        None => absolute(PathBuf::from(".")),
    }
}

/// `fno do think inspect SEED [--repo DIR] [--json]`, argv after the
/// command words.
pub fn run(args: &[String]) -> i32 {
    let mut seed: Option<&str> = None;
    let mut repo: Option<&str> = None;
    let mut json_out = false;
    let mut positional_only = false;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        i += 1;
        if !positional_only {
            match arg {
                "--help" | "-h" => {
                    println!("{HELP}");
                    return 0;
                }
                "--json" | "-J" => {
                    json_out = true;
                    continue;
                }
                "--" => {
                    positional_only = true;
                    continue;
                }
                "--repo" => {
                    match args.get(i) {
                        Some(value) => repo = Some(value.as_str()),
                        None => return usage_error("Option '--repo' requires an argument."),
                    }
                    i += 1;
                    continue;
                }
                _ => {}
            }
            if let Some(value) = arg.strip_prefix("--repo=") {
                repo = Some(value);
                continue;
            }
            if arg.starts_with('-') && arg.len() > 1 {
                return usage_error(&format!("No such option: {arg}"));
            }
        }
        if seed.is_some() {
            return usage_error(&format!("Got unexpected extra argument ({arg})"));
        }
        seed = Some(arg);
    }
    let Some(seed) = seed else {
        return usage_error("Missing argument 'SEED'.");
    };
    let target = match repo {
        Some(raw) => absolute(crate::plans_path::expanduser(raw)),
        None => default_repo_root(),
    };
    if !target.is_dir() {
        eprintln!(
            "{COMMAND}: repository directory not found: {}",
            target.display()
        );
        return 2;
    }
    let inputs = Inputs {
        graph: None,
        archive: None,
        plans_path: None,
        home: None,
        space_dir: None,
        run: &real_command,
        decisions: &live_decisions,
    };
    let receipt = build_receipt(seed, &target, &inputs);
    if json_out {
        println!("{}", crate::backlog::render::py_json_pretty(&receipt));
    } else {
        println!("{}", render_receipt(&receipt));
    }
    0
}

#[cfg(test)]
#[path = "think_inspect_tests.rs"]
mod tests;
