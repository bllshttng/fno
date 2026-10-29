//! `fno inbox outstanding`: what is waiting on the operator, in one read.
//!
//! Five streams fold into one report: the operator lane, this project's
//! carve-outs, open operator questions, open prove-it FAIL verdicts and open
//! `fu-` captures across every known project. `--json` is the contract
//! `org_board` and `finalize` read, so its key order and shape are fixed.
//!
//! The clearing commands always existed. What was missing was anything that
//! asked a human to run them: the carve-out ledger once reached 39 rows over
//! 29 days with nobody told. So this is a surface, not a wire, and every
//! stream names the command that clears it.
//!
//! A store that exists but cannot be read is a failed read (exit 1), never
//! "nothing outstanding": an empty answer there would tell the operator the
//! pile is clear when it is merely unreadable. The verdict reader is the one
//! exception: its failure is a stated line, so it never blanks the others.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Rows rendered before the footer takes over. A block that scrolls the
/// operator's screen gets skipped, which is the failure this report exists
/// to fix.
pub const RENDER_CAP: usize = 3;
pub const QUESTION_RENDER_CAP: usize = 10;
/// A rendered row is a pointer, not the evidence: the full body is one `-J`
/// away. Human-authored bodies ran a 929-character median when measured, so
/// this cap binds on most rows by design.
pub const QUESTION_BODY_CAP: usize = 240;
/// `retro sweep-carveouts` skips this kind; `/fno:ship pr merged` owns it.
pub const BACKFILL_KIND: &str = "backfill";
/// SessionStart gives the hook 1.5s, and reachability can scan large harness
/// stores. The probe gets one bounded slice; an asker it does not answer in
/// time reads as unknown, never as stale.
pub const LIVENESS_BUDGET: Duration = Duration::from_millis(500);

const QUESTION_EVENT: &str = "operator_question";
const QUESTION_CLOSED_EVENT: &str = "operator_question_closed";

/// Every row type serializes in field order: that order is the `--json`
/// contract.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Question {
    pub id: String,
    pub ts: String,
    pub question: String,
    pub session_id: Option<String>,
    pub cwd: Option<String>,
    pub node: Option<String>,
    pub asker: Option<String>,
    pub ask: Option<String>,
    pub options: Vec<String>,
    pub blocks: Vec<String>,
    pub subject: Option<String>,
    pub live: Option<bool>,
    /// The asking machine on a copy pulled from another machine of the
    /// store; `None` (JSON null) for a question asked here.
    pub host: Option<String>,
    /// The person behind the asking machine; absent on a row written before
    /// the stamp, so old rows render byte-identical.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub person: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Capture {
    #[serde(rename = "id")]
    pub fu_id: String,
    pub title: String,
    pub project: String,
    pub added_at: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Verdict {
    pub node: String,
    pub report: String,
    pub verdict: String,
    pub claim: String,
    pub status: Option<String>,
    pub mtime: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LaneRow {
    pub text: String,
    pub line: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Outstanding {
    pub carveout_total: u64,
    /// Insertion order follows first sight in the ledger.
    pub carveout_by_kind: Vec<(String, u64)>,
    pub carveout_oldest_ts: Option<String>,
    pub carveout_root: Option<String>,
    pub questions: Vec<Question>,
    pub questions_store: String,
    pub captures: Vec<Capture>,
    pub capture_file_total: u64,
    pub capture_row_total: u64,
    pub lane: Vec<LaneRow>,
    pub lane_parked: u64,
    pub lane_store: String,
    pub verdicts: Vec<Verdict>,
    pub verdicts_error: Option<String>,
}

impl Outstanding {
    pub fn empty(&self) -> bool {
        self.carveout_total == 0
            && self.questions.is_empty()
            && self.captures.is_empty()
            && self.lane.is_empty()
            && self.verdicts.is_empty()
            && self.verdicts_error.is_none()
    }

    pub fn as_json(&self) -> Value {
        let mut by_project: Map<String, Value> = Map::new();
        for c in &self.captures {
            let n = by_project
                .get(&c.project)
                .and_then(Value::as_u64)
                .unwrap_or(0);
            by_project.insert(c.project.clone(), json!(n + 1));
        }
        let mut by_kind: Map<String, Value> = Map::new();
        for (kind, n) in &self.carveout_by_kind {
            by_kind.insert(kind.clone(), json!(n));
        }
        let questions: Vec<Value> = self
            .questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let mut row = json!(q);
                row["rank"] = json!(i + 1);
                row
            })
            .collect();
        let total = self.captures.len() as u64;
        json!({
            "roots": {
                "carveouts": {"scope": "project", "root": self.carveout_root},
                "questions": {"scope": "machine", "store": self.questions_store},
                "captures": {"scope": "machine", "files_read": self.capture_file_total},
                "lane": {"scope": "machine", "store": self.lane_store},
            },
            "carveouts": {
                "total": self.carveout_total,
                "by_kind": Value::Object(by_kind),
                "oldest_ts": self.carveout_oldest_ts,
            },
            "questions": questions,
            "captures": {
                "total": total,
                "resolved_files": self.capture_file_total,
                "parsed_open_rows": self.capture_row_total,
                "repeated_ids": self.capture_row_total.saturating_sub(total),
                "by_project": Value::Object(by_project),
                "items": self.captures,
            },
            "lane": {
                "total": self.lane.len(),
                "parked": self.lane_parked,
                "items": self.lane,
            },
            "verdicts": {
                "total": self.verdicts.len(),
                "error": self.verdicts_error,
                "items": self.verdicts,
            },
        })
    }
}

/// Where each stream reads from. The run path resolves the real stores;
/// tests point every field at a sandbox.
#[derive(Clone, Debug)]
pub struct Sources {
    /// The canonical root that owns the carve-out ledger.
    pub root: PathBuf,
    pub questions: PathBuf,
    pub lane: PathBuf,
    pub graph: PathBuf,
    /// `graph`, or an external tracker whose sidecars carry the checkouts.
    pub backend: String,
    pub sidecar_dir: PathBuf,
    /// `$HOME`, which names a shared capture store by its first segment.
    pub home: Option<PathBuf>,
}

/// Python truthiness for a JSON field rendered as `str(value or default)`.
fn py_str_or(v: Option<&Value>, default: &str) -> String {
    match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => default.to_string(),
        Some(Value::String(s)) if s.is_empty() => default.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) if n.as_f64() == Some(0.0) => default.to_string(),
        Some(Value::Array(a)) if a.is_empty() => default.to_string(),
        Some(Value::Object(o)) if o.is_empty() => default.to_string(),
        Some(other) => other.to_string(),
    }
}

/// `data.get(key) or None` for a string field.
fn opt_str(data: &Map<String, Value>, key: &str) -> Option<String> {
    match data.get(key) {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::String(_)) | Some(Value::Null) | None => None,
        Some(other) => Some(other.to_string()),
    }
}

fn str_list(data: &Map<String, Value>, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|v| {
                    v.as_str()
                        .map(str::to_string)
                        .unwrap_or_else(|| v.to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `<space>/<name>` for a project log, or the pre-space `<root>/.fno/<name>`
/// a checkout still holds before its first migrating write.
pub(crate) fn project_log(root: &Path, name: &str) -> PathBuf {
    if let Some(space) = crate::paths::space_dir_opt(root) {
        let path = space.join(name);
        if path.exists() || crate::event_store::store_path(&path).exists() {
            return path;
        }
    }
    root.join(".fno").join(name)
}

/// Collapse a linked worktree onto its canonical checkout without git: a
/// standard `.git` file points into `<canonical>/.git/worktrees/<name>`.
/// Other shapes keep the root.
pub fn canonical_checkout_for(root: &Path) -> PathBuf {
    let marker = root.join(".git");
    if marker.is_dir() {
        return root.to_path_buf();
    }
    let Ok(text) = std::fs::read_to_string(&marker) else {
        return root.to_path_buf();
    };
    let Some(line) = text.lines().next() else {
        return root.to_path_buf();
    };
    let Some(rest) = line.strip_prefix("gitdir:") else {
        return root.to_path_buf();
    };
    let mut git_dir = PathBuf::from(rest.trim());
    if !git_dir.is_absolute() {
        git_dir = root.join(git_dir);
    }
    let worktrees = git_dir.parent();
    let dot_git = worktrees.and_then(Path::parent);
    if worktrees.and_then(Path::file_name) != Some(std::ffi::OsStr::new("worktrees"))
        || dot_git.and_then(Path::file_name) != Some(std::ffi::OsStr::new(".git"))
    {
        return root.to_path_buf();
    }
    match dot_git.and_then(Path::parent) {
        Some(canonical) if canonical.join(".git").is_dir() => canonical.to_path_buf(),
        _ => root.to_path_buf(),
    }
}

/// The carve-out fold: (total, by_kind in first-seen order, oldest ts). A
/// missing ledger is empty; one that exists but cannot be read is an error.
/// A malformed line is skipped so one bad row cannot hide the rest.
fn read_carveouts(root: &Path) -> Result<(u64, Vec<(String, u64)>, Option<String>), String> {
    let path = project_log(root, "carveouts.jsonl");
    if !path.exists() {
        return Ok((0, Vec::new(), None));
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read carve-out ledger {}: {e}", path.display()))?;
    let mut total = 0u64;
    let mut by_kind: Vec<(String, u64)> = Vec::new();
    let mut oldest: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        total += 1;
        let kind = py_str_or(rec.get("kind"), "unknown");
        match by_kind.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, n)) => *n += 1,
            None => by_kind.push((kind, 1)),
        }
        let ts = py_str_or(rec.get("ts"), "");
        if !ts.is_empty() && oldest.as_ref().is_none_or(|o| ts < *o) {
            oldest = Some(ts);
        }
    }
    Ok((total, by_kind, oldest))
}

/// Neither the event store nor the journal file exists for `index`.
fn index_missing(index: &Path) -> bool {
    let store_present = crate::event_store::try_store_path(index)
        .map(|s| s.is_file())
        .unwrap_or(false);
    !store_present
        && matches!(std::fs::symlink_metadata(index), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Fold `operator_question` minus `operator_question_closed` from the
/// machine-wide index. A missing index reads as no questions (the command,
/// not this reader, prints the recovery hint: the board reads it in-process);
/// an unreadable one is an error naming it.
pub fn read_open_questions(index: &Path) -> Result<Vec<Question>, String> {
    if index_missing(index) {
        return Ok(Vec::new());
    }
    let store_present = crate::event_store::try_store_path(index)
        .map(|s| s.is_file())
        .unwrap_or(false);
    if !store_present {
        match std::fs::symlink_metadata(index) {
            Err(e) => {
                return Err(format!(
                    "cannot read question index {}: {e}",
                    index.display()
                ))
            }
            Ok(_) if !index.exists() => {
                return Err(format!(
                    "cannot read question index {}: dangling symlink",
                    index.display()
                ))
            }
            Ok(_) => {}
        }
    }
    let raw = crate::event_store::journal_text_checked(
        index,
        &crate::event_store::EventQuery::of_types(&[QUESTION_EVENT, QUESTION_CLOSED_EVENT]),
    )
    .map_err(|e| format!("cannot read question index {}: {e}", index.display()))?;
    Ok(fold_questions(&raw))
}

/// The open set from raw index text, newest first; more blocked nodes, then
/// id, break a same-second tie. Liveness is not applied here.
pub fn fold_questions(raw: &str) -> Vec<Question> {
    let mut asked: HashMap<String, Question> = HashMap::new();
    let mut closed: HashSet<String> = HashSet::new();
    for line in raw.lines() {
        if !line.contains(QUESTION_EVENT) {
            continue;
        }
        let Ok(Value::Object(rec)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(Value::Object(data)) = rec.get("data") else {
            continue;
        };
        let qid = py_str_or(data.get("question_id"), "");
        if qid.is_empty() {
            continue;
        }
        match rec.get("type").and_then(Value::as_str) {
            Some(QUESTION_EVENT) => {
                asked.insert(
                    qid.clone(),
                    Question {
                        id: qid,
                        ts: py_str_or(rec.get("ts"), ""),
                        question: py_str_or(data.get("question"), ""),
                        session_id: opt_str(data, "session_id"),
                        cwd: opt_str(data, "cwd"),
                        node: opt_str(data, "node"),
                        asker: opt_str(data, "asker"),
                        ask: opt_str(data, "ask"),
                        options: str_list(data, "options"),
                        blocks: str_list(data, "blocks"),
                        subject: opt_str(data, "subject"),
                        live: None,
                        host: opt_str(data, "origin_machine")
                            .filter(|m| *m != crate::claims::machine_id())
                            .map(|_| {
                                opt_str(data, "host").unwrap_or_else(|| "another machine".into())
                            }),
                        person: opt_str(data, "person"),
                    },
                );
            }
            Some(QUESTION_CLOSED_EVENT) => {
                closed.insert(qid);
            }
            _ => {}
        }
    }
    let mut open: Vec<Question> = asked
        .into_values()
        .filter(|q| !closed.contains(&q.id))
        .collect();
    // An empty ts sorts last.
    open.sort_by(|a, b| {
        b.ts.cmp(&a.ts)
            .then_with(|| b.blocks.len().cmp(&a.blocks.len()))
            .then_with(|| b.id.cmp(&a.id))
    });
    open
}

/// Stamp liveness and put the live lane first, keeping recency inside each
/// lane. No asker is stale; an asker missing from `answers` is unknown.
pub fn apply_liveness(questions: &mut [Question], answers: &HashMap<String, bool>) {
    for q in questions.iter_mut() {
        // A copy's asker runs on its own machine, where no probe here reaches.
        q.live = match &q.asker {
            _ if q.host.is_some() => None,
            None => Some(false),
            Some(a) => answers.get(a).copied(),
        };
    }
    questions.sort_by_key(|q| q.live != Some(true));
}

/// Resolve each unique asker inside one wall-clock slice. Only a complete
/// answer set lands: a probe still running at the deadline leaves every
/// asker unknown.
fn resolve_liveness(questions: &[Question], budget: Duration) -> HashMap<String, bool> {
    let mut askers: Vec<String> = Vec::new();
    for q in questions {
        if let Some(a) = &q.asker {
            if !askers.contains(a) {
                askers.push(a.clone());
            }
        }
    }
    if askers.is_empty() || budget.is_zero() {
        return HashMap::new();
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut answers = HashMap::new();
        for asker in askers {
            let r = crate::mail_cli::send::resolve::resolve_reachable(&asker);
            answers.insert(asker, r.hit.is_some() && r.ambiguous.is_empty());
        }
        let _ = tx.send(answers);
    });
    rx.recv_timeout(budget).unwrap_or_default()
}

/// This root plus every project root the backlog names (each node's `cwd`
/// and `source_cwd` that is still a directory). The backlog is advisory
/// here: an unreadable store reads as this root alone.
pub(crate) fn capture_project_roots(src: &Sources) -> Vec<PathBuf> {
    let mut roots: BTreeSet<PathBuf> = BTreeSet::new();
    roots.insert(crate::paths::resolve_loose(&src.root));
    let mut locations: Vec<Value> = Vec::new();
    if src.backend == "graph" {
        if src.graph.exists() {
            if let Ok(rows) = crate::graph_store::read_rows(&src.graph) {
                locations = rows;
            }
        }
    } else if let Ok(dir) = std::fs::read_dir(&src.sidecar_dir) {
        for entry in dir.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Some(v) = std::fs::read_to_string(&p)
                .ok()
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            {
                locations.push(v);
            }
        }
    }
    for row in &locations {
        for key in ["cwd", "source_cwd"] {
            if let Some(raw) = row.get(key).and_then(Value::as_str) {
                let p = PathBuf::from(raw);
                if !raw.is_empty() && p.is_dir() {
                    let abs = if p.is_absolute() {
                        p
                    } else {
                        std::env::current_dir().unwrap_or_default().join(p)
                    };
                    roots.insert(abs);
                }
            }
        }
    }
    roots.into_iter().collect()
}

/// fu_id -> the first `capture_add` ts in one root's events journal. The
/// inbox markdown carries no dates, so age comes from the capture event.
fn capture_added_at(root: &Path) -> HashMap<String, String> {
    let path = project_log(root, "events.jsonl");
    let raw = crate::event_store::journal_text(&path, &["capture_add"]);
    let mut added: HashMap<String, String> = HashMap::new();
    for line in raw.lines() {
        if !line.contains("capture_add") {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if rec.get("type").and_then(Value::as_str) != Some("capture_add") {
            continue;
        }
        let fu = rec
            .get("data")
            .and_then(|d| d.get("fu_id"))
            .and_then(Value::as_str);
        let ts = rec
            .get("ts")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        if let (Some(fu), Some(ts)) = (fu, ts) {
            added
                .entry(fu.to_string())
                .or_insert_with(|| ts.to_string());
        }
    }
    added
}

/// Name a capture pile by the store it is: under `$HOME` the first path
/// segment (one shared vault reached by many repos), else the first
/// contributing root's name.
fn store_label(path: &Path, roots: &[PathBuf], home: Option<&Path>) -> String {
    if let Some(home) = home {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        if let Ok(rel) = path.strip_prefix(&home) {
            if let Some(first) = rel.components().next() {
                return first.as_os_str().to_string_lossy().into_owned();
            }
        }
    }
    roots
        .first()
        .and_then(|r| r.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Fold every project's capture inbox: (captures, files read, parsed open
/// rows). Never fails: a missing or unreadable inbox is one project less.
///
/// Deduped twice, because the roots enumerate worktrees too and sibling
/// worktrees reach the same inbox file: first by resolved inbox path, then
/// by `fu_id` across the distinct files.
fn read_open_captures(src: &Sources) -> (Vec<Capture>, u64, u64) {
    let mut by_repo: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for root in capture_project_roots(src) {
        let canonical = canonical_checkout_for(&root);
        match by_repo.iter_mut().find(|(c, _)| *c == canonical) {
            Some((_, v)) => v.push(root),
            None => by_repo.push((canonical, vec![root])),
        }
    }
    let mut by_path: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for (config_root, roots) in by_repo {
        let Some(path) = crate::backlog::triage::inbox_markdown_path_at(&config_root) else {
            continue;
        };
        let Ok(path) = std::fs::canonicalize(&path) else {
            continue;
        };
        by_path.entry(path).or_default().extend(roots);
    }

    let mut captures: Vec<Capture> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let (mut file_total, mut row_total) = (0u64, 0u64);
    for (path, roots) in by_path {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        file_total += 1;
        let items = crate::backlog::capture::parse_items(&text, false);
        row_total += items.len() as u64;
        let project = store_label(&path, &roots, src.home.as_deref());
        // Canonical checkouts lead the events lookup. Every distinct journal
        // folds, because projects can share one capture file while writing
        // capture events to separate journals.
        let mut ordered = roots.clone();
        ordered.sort_by_key(|r| !r.join(".git").is_dir());
        let mut added: HashMap<String, String> = HashMap::new();
        let mut seen_journals: HashSet<PathBuf> = HashSet::new();
        for r in &ordered {
            let journal = crate::paths::resolve_loose(&project_log(r, "events.jsonl"));
            if !seen_journals.insert(journal) {
                continue;
            }
            for (fu, ts) in capture_added_at(r) {
                match added.get(&fu) {
                    Some(prev) if *prev <= ts => {}
                    _ => {
                        added.insert(fu, ts);
                    }
                }
            }
        }
        for item in items {
            let Some(id) = item.get("id").and_then(Value::as_str) else {
                continue;
            };
            if !seen_ids.insert(id.to_string()) {
                continue;
            }
            captures.push(Capture {
                fu_id: id.to_string(),
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                project: project.clone(),
                added_at: added.get(id).cloned(),
            });
        }
    }
    // Oldest first where the age is known, so the render cap keeps the items
    // that have waited longest.
    captures.sort_by(|a, b| {
        (a.added_at.is_none(), &a.added_at, &a.fu_id).cmp(&(
            b.added_at.is_none(),
            &b.added_at,
            &b.fu_id,
        ))
    });
    (captures, file_total, row_total)
}

/// Open FAIL rows from the prove-it reader rows.
fn open_verdicts(rows: &[Value]) -> Vec<Verdict> {
    rows.iter()
        .filter(|r| r.get("open").and_then(Value::as_bool).unwrap_or(false))
        .map(|r| {
            let s = |k: &str| py_str_or(r.get(k), "");
            let opt = |k: &str| r.get(k).and_then(Value::as_str).map(str::to_string);
            Verdict {
                node: s("node"),
                report: s("report"),
                verdict: py_str_or(r.get("verdict"), "FAIL"),
                claim: s("claim"),
                status: opt("status"),
                mtime: opt("mtime"),
            }
        })
        .collect()
}

/// Read every stream except liveness. `verdicts` is the prove-it reader's
/// answer, already fetched, so its failure stays a stated line.
pub fn collect(src: &Sources, verdicts: Result<Vec<Value>, String>) -> Result<Outstanding, String> {
    let (carveout_total, carveout_by_kind, carveout_oldest_ts) = read_carveouts(&src.root)?;
    let (captures, capture_file_total, capture_row_total) = read_open_captures(src);
    let lane_items = crate::org_board::queues::parse_lane(&src.lane)?;
    let lane: Vec<LaneRow> = lane_items
        .iter()
        .filter(|i| !i.done && i.node.is_none() && i.parked.is_none())
        .map(|i| LaneRow {
            text: i.text.clone(),
            line: i.line,
        })
        .collect();
    let lane_parked = lane_items.iter().filter(|i| i.parked.is_some()).count() as u64;
    let (verdict_rows, verdicts_error) = match verdicts {
        Ok(rows) => (open_verdicts(&rows), None),
        Err(e) => (Vec::new(), Some(e)),
    };
    Ok(Outstanding {
        carveout_total,
        carveout_by_kind,
        carveout_oldest_ts,
        carveout_root: Some(src.root.display().to_string()),
        questions: read_open_questions(&src.questions)?,
        questions_store: src.questions.display().to_string(),
        captures,
        capture_file_total,
        capture_row_total,
        lane,
        lane_parked,
        lane_store: src.lane.display().to_string(),
        verdicts: verdict_rows,
        verdicts_error,
    })
}

/// Whole days since `ts`, floored at zero; None when it does not parse.
pub fn age_days(ts: &str, now: DateTime<Utc>) -> Option<i64> {
    let when: DateTime<Utc> = if let Ok(t) = DateTime::parse_from_rfc3339(ts) {
        t.with_timezone(&Utc)
    } else if let Ok(t) = NaiveDateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%.f") {
        t.and_utc()
    } else if let Ok(d) = NaiveDate::parse_from_str(ts, "%Y-%m-%d") {
        d.and_hms_opt(0, 0, 0)?.and_utc()
    } else {
        return None;
    };
    Some((now - when).num_days().max(0))
}

fn plural(n: impl Into<i128>, word: &str) -> String {
    let n: i128 = n.into();
    if n == 1 {
        format!("{n} {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// First line only, cut at `cap` chars with an ellipsis that says it cut.
fn one_line(text: &str, cap: usize) -> (String, bool) {
    let line = text.split('\n').next().unwrap_or("").trim();
    if line.chars().count() > cap {
        let cut: String = line.chars().take(cap - 1).collect();
        (format!("{cut}\u{2026}"), true)
    } else {
        (line.to_string(), false)
    }
}

/// What the human block reads besides the fold.
#[derive(Clone, Debug)]
pub struct RenderCtx {
    pub session_id: Option<String>,
    /// Only a session holding a role gets the lane's action line.
    pub promoted: bool,
    /// The cwd's name: a capture from this project carries no suffix.
    pub cwd_name: String,
    pub now: DateTime<Utc>,
}

/// The human block. Empty when nothing is outstanding.
pub fn render(o: &Outstanding, ctx: &RenderCtx) -> String {
    if o.empty() {
        return String::new();
    }
    let mut lines: Vec<String> = vec!["## Outstanding for you".to_string(), String::new()];

    if let Some(top) = o.lane.first() {
        let mut head = format!(
            "{} on your lane, top first.",
            plural(o.lane.len() as u64, "item")
        );
        if o.lane_parked > 0 {
            head.push_str(&format!(" {} parked.", o.lane_parked));
        }
        lines.push(head);
        lines.push(format!("  {}", top.text));
        if ctx.promoted {
            lines.push(
                "  File one with: fno backlog idea \"<text>\", then stamp `-> <id>` onto its line, or park it with `-> parked: <reason>`."
                    .to_string(),
            );
        }
        lines.push(String::new());
    }

    if o.carveout_total > 0 {
        let mut kinds = o.carveout_by_kind.clone();
        kinds.sort();
        let split = kinds
            .iter()
            .map(|(k, n)| format!("{n} {k}"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut head = format!("{} unharvested", plural(o.carveout_total, "carve-out"));
        if let Some(root) = &o.carveout_root {
            head.push_str(&format!(" from {root}"));
        }
        if !split.is_empty() {
            head.push_str(&format!(" ({split})"));
        }
        if let Some(ts) = &o.carveout_oldest_ts {
            let oldest: String = ts.chars().take(10).collect();
            head.push_str(&format!(", oldest {oldest}"));
            if let Some(age) = age_days(ts, ctx.now) {
                head.push_str(&format!(" ({} ago)", plural(age, "day")));
            }
        }
        lines.push(format!("{head}."));
        // The sweep skips backfill rows, so naming it against a backfill-only
        // ledger would send the operator to a command that clears nothing.
        let sweepable: u64 = o
            .carveout_by_kind
            .iter()
            .filter(|(k, _)| k != BACKFILL_KIND)
            .map(|(_, n)| n)
            .sum();
        if sweepable > 0 {
            lines.push(
                "  Clear with: fno backlog retro sweep-carveouts (preview), --apply to file and consume."
                    .to_string(),
            );
        }
        if o.carveout_by_kind
            .iter()
            .any(|(k, n)| k == BACKFILL_KIND && *n > 0)
        {
            lines.push(
                "  Backfill rows are handled by /fno:ship pr merged, not by the sweep.".to_string(),
            );
        }
        lines.push(String::new());
    }

    if !o.questions.is_empty() {
        render_questions(o, ctx, &mut lines);
    }

    if !o.verdicts.is_empty() {
        lines.push(format!(
            "{} with no ruling.",
            plural(o.verdicts.len() as u64, "prove-it FAIL verdict")
        ));
        for v in o.verdicts.iter().take(RENDER_CAP) {
            let (claim, _) = one_line(&v.claim, 100);
            let where_ = v
                .status
                .as_ref()
                .map(|s| format!(" ({s})"))
                .unwrap_or_default();
            lines.push(format!("  {}{where_}: {claim}", v.node));
            lines.push(format!("    Read: {}", v.report));
        }
        lines.push(
            "  Rule with: fno inbox decide <node> naming the report, or re-run /fno:review prove-it."
                .to_string(),
        );
        lines.push(String::new());
    }

    if let Some(err) = &o.verdicts_error {
        lines.push(format!("prove-it verdicts could not be read ({err})."));
        lines.push(String::new());
    }

    if !o.captures.is_empty() {
        let total = o.captures.len() as u64;
        lines.push(format!(
            "{} awaiting triage (fu- items).",
            plural(total, "capture")
        ));
        let shown = o.captures.iter().take(RENDER_CAP).count();
        for c in o.captures.iter().take(RENDER_CAP) {
            let where_ = if c.project != ctx.cwd_name {
                format!("  ({})", c.project)
            } else {
                String::new()
            };
            lines.push(format!("  {}  {}{where_}", c.fu_id, c.title));
        }
        // The cap without the count is a lie by omission.
        let oldest = o
            .captures
            .iter()
            .filter_map(|c| c.added_at.as_deref().and_then(|t| age_days(t, ctx.now)))
            .max();
        let mut summary = format!("  Showing {shown} of {total}");
        if let Some(age) = oldest {
            summary.push_str(&format!(", oldest {}", plural(age, "day")));
        }
        summary.push_str(&format!(
            ". Count rule: {total} unique open fu-* IDs across {} ({} minus {}); post_merge.parking_lot_path is used when configured.",
            plural(o.capture_file_total, "resolved capture file"),
            plural(o.capture_row_total, "parsed open row"),
            plural(o.capture_row_total.saturating_sub(total), "repeated ID"),
        ));
        lines.push(summary);
        lines.push(
            "  Triage with: fno backlog triage, or fno backlog capture promote <fu-id>."
                .to_string(),
        );
    }

    let mut out = lines.join("\n").trim_end().to_string();
    out.push('\n');
    out
}

fn render_questions(o: &Outstanding, ctx: &RenderCtx, lines: &mut Vec<String>) {
    lines.push(format!(
        "{} awaiting you.",
        plural(o.questions.len() as u64, "open question")
    ));
    let shown = &o.questions[..o.questions.len().min(QUESTION_RENDER_CAP)];
    let mut body_trimmed = false;

    // Groups keep first-seen order: unaddressed, then each liveness value.
    #[derive(PartialEq, Clone, Copy)]
    enum Group {
        Unaddressed,
        Live(Option<bool>),
    }
    let mut order: Vec<Group> = Vec::new();
    for q in shown {
        let g = if q.asker.is_none() {
            Group::Unaddressed
        } else {
            Group::Live(q.live)
        };
        if !order.contains(&g) {
            order.push(g);
        }
    }
    let (mut has_stale, mut has_unknown) = (false, false);
    for g in &order {
        lines.push(
            match g {
                Group::Unaddressed => "  Questions for you (no agent is waiting):",
                Group::Live(Some(true)) => "  Live open questions:",
                Group::Live(Some(false)) => "  Stale questions:",
                Group::Live(None) => "  Questions with unknown liveness:",
            }
            .to_string(),
        );
        let stale_row = *g == Group::Live(Some(false));
        for q in shown.iter().filter(|q| {
            let qg = if q.asker.is_none() {
                Group::Unaddressed
            } else {
                Group::Live(q.live)
            };
            qg == *g
        }) {
            let mine = ctx.session_id.is_some() && q.session_id == ctx.session_id;
            let label = if mine { "[this session] " } else { "" };
            let where_ = q.node.clone().or_else(|| {
                q.cwd.as_deref().and_then(|c| {
                    Path::new(c)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                })
            });
            let mut details: Vec<String> = where_.into_iter().collect();
            if let Some(host) = &q.host {
                details.push(format!("on {host}"));
            }
            if stale_row {
                details.push(match age_days(&q.ts, ctx.now) {
                    Some(age) => format!("{} old", plural(age, "day")),
                    None => "age unknown".to_string(),
                });
            }
            let suffix = if details.is_empty() {
                String::new()
            } else {
                format!("  ({})", details.join("; "))
            };
            let (body, cut) = one_line(&q.question, QUESTION_BODY_CAP);
            let (action, action_cut) = match &q.ask {
                Some(a) => {
                    let (l, c) = one_line(a, QUESTION_BODY_CAP);
                    (Some(l), c)
                }
                None => (None, false),
            };
            if cut
                || action_cut
                || q.question.contains('\n')
                || q.ask.as_deref().is_some_and(|a| a.contains('\n'))
            {
                body_trimmed = true;
            }
            lines.push(format!("  {label}{}  {body}{suffix}", q.id));
            if let Some(action) = action.filter(|a| !a.is_empty()) {
                lines.push(format!("    Action: {action}"));
            }
        }
        has_stale |= stale_row;
        has_unknown |= *g == Group::Live(None);
    }
    if has_stale {
        lines.push(
            "  Answering a stale question records the decision but reaches nobody.".to_string(),
        );
    }
    if has_unknown {
        lines.push(
            "  Unknown means reachability did not finish inside the report budget; answering still records the decision."
                .to_string(),
        );
    }
    if o.questions.len() > QUESTION_RENDER_CAP {
        lines.push(format!(
            "  Showing {} of {} open questions.",
            shown.len(),
            o.questions.len()
        ));
    }
    lines.push("  Answer with: fno inbox outstanding clear <id> --answer \"...\"".to_string());
    lines.push(
        "  An agent relaying the user's chat answer adds: --authority chat_attested --quote \"<their words>\""
            .to_string(),
    );
    if body_trimmed {
        lines.push(
            "  Rows are trimmed to one line; read a full question with: fno inbox outstanding -J"
                .to_string(),
        );
    }
    lines.push(String::new());
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The canonical root that owns the project stores.
pub(crate) fn storage_root(cwd: &Path) -> PathBuf {
    if let Some(pin) = env_nonempty("FNO_REPO_ROOT") {
        return crate::paths::resolve_loose(Path::new(&pin));
    }
    let marker = cwd.join(".git");
    if marker.exists() {
        let canonical = canonical_checkout_for(cwd);
        if canonical != cwd || marker.is_dir() {
            return canonical;
        }
    }
    let root = crate::paths::worktree_repo_root(cwd);
    let canonical = canonical_checkout_for(&root);
    if canonical != root || root.join(".git").is_dir() {
        return canonical;
    }
    crate::paths::canonical_repo_root(cwd).unwrap_or(root)
}

/// This session's id: the target manifest's `fno_id` or `session_id`, else
/// `CLAUDECODE_SESSION_ID`.
pub(crate) fn session_id(cwd: &Path) -> Option<String> {
    let manifest = crate::state_path::resolve("target-state", cwd);
    if let Some(Ok(text)) = manifest.map(std::fs::read_to_string) {
        let mut lines = text.lines();
        if lines.next().map(str::trim) == Some("---") {
            let mut fields: HashMap<&str, String> = HashMap::new();
            let mut closed = false;
            for line in lines {
                let s = line.trim();
                if s == "---" {
                    closed = true;
                    break;
                }
                for key in ["fno_id", "session_id"] {
                    if let Some(v) = s.strip_prefix(key).and_then(|r| r.strip_prefix(':')) {
                        let v = v.trim().trim_matches(['"', '\'']);
                        if !v.is_empty() && v != "null" {
                            fields.entry(key).or_insert_with(|| v.to_string());
                        }
                    }
                }
            }
            if closed {
                if let Some(id) = fields.get("fno_id").or_else(|| fields.get("session_id")) {
                    return Some(id.clone());
                }
            }
        }
    }
    env_nonempty("CLAUDECODE_SESSION_ID")
}

/// True only when this session's registry row holds a role. Any failure
/// reads as not promoted: it costs one action line, never the lane.
fn is_promoted(home: &crate::paths::AgentsHome) -> bool {
    let Ok(registry) = crate::state::load_registry(&home.registry_json()) else {
        return false;
    };
    let (uuid, harness) = match env_nonempty("CODEX_THREAD_ID") {
        Some(t) => (Some(t), Some("codex")),
        None => (env_nonempty("CLAUDE_CODE_SESSION_ID"), None),
    };
    for token in [env_nonempty("FNO_AGENT_SESSION"), uuid]
        .into_iter()
        .flatten()
    {
        if let Some(row) = crate::lead_state::find_by_session(&registry.entries, &token, harness) {
            return row.role_level.is_some();
        }
    }
    false
}

fn prove_it_rows() -> Result<Vec<Value>, String> {
    let backend = env_nonempty("FNO_TRACKER_BACKEND").unwrap_or_else(|| "graph".to_string());
    if backend != "graph" {
        return Err(
            "the prove-it reader reads the graph store, which is not authoritative under an external tracker backend"
                .to_string(),
        );
    }
    crate::prove_it_verdicts::prove_it_verdict_rows()
}

/// The run-path `Sources`: the listing and the reindex arm resolve the same
/// stores once, through one builder.
pub(crate) fn sources(cwd: &Path) -> Sources {
    let home = crate::paths::AgentsHome::from_env();
    Sources {
        root: storage_root(cwd),
        questions: crate::provider_cap::questions_path(&home),
        lane: crate::org_board::scope::operator_lane_path(cwd),
        graph: crate::graph_get::default_graph_path(),
        backend: env_nonempty("FNO_TRACKER_BACKEND").unwrap_or_else(|| "graph".to_string()),
        sidecar_dir: crate::tracker::sidecar::root(cwd),
        home: std::env::var_os("HOME").map(PathBuf::from),
    }
}

/// The listing for `cwd`, liveness resolved within `liveness`. The board
/// calls this in-process, so its read cannot drift from the command's.
pub(crate) fn read(cwd: &Path, liveness: Duration) -> Result<Outstanding, String> {
    let mut o = collect(&sources(cwd), prove_it_rows())?;
    let answers = resolve_liveness(&o.questions, liveness);
    apply_liveness(&mut o.questions, &answers);
    Ok(o)
}

/// `fno-agents compaction outstanding [--json|-J]`, the native `fno inbox outstanding`.
pub fn run_outstanding(args: &[String]) -> i32 {
    if let Some((word, rest)) = args.split_first() {
        if let Some(code) = crate::outstanding_cmds::dispatch(word, rest) {
            return code;
        }
    }
    let mut as_json = false;
    for a in args {
        match a.as_str() {
            "--json" | "-J" => as_json = true,
            "-h" | "--help" => {
                println!("usage: fno inbox outstanding [--json|-J]");
                return 0;
            }
            other => {
                eprintln!("outstanding: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let o = match read(&cwd, LIVENESS_BUDGET) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("outstanding: failed to read: {e}");
            return 1;
        }
    };
    if index_missing(Path::new(&o.questions_store)) {
        eprintln!(
            "outstanding: question index {} is missing; run \
`fno inbox outstanding reindex` to recover project questions.",
            o.questions_store
        );
    }
    let mut out = std::io::stdout().lock();
    if as_json {
        let _ = writeln!(out, "{}", o.as_json());
        return 0;
    }
    let ctx = RenderCtx {
        session_id: session_id(&cwd),
        promoted: is_promoted(&crate::paths::AgentsHome::from_env()),
        cwd_name: cwd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        now: Utc::now(),
    };
    let _ = write!(out, "{}", render(&o, &ctx));
    0
}

#[cfg(test)]
#[path = "outstanding_tests.rs"]
mod tests;
