//! Detective attribution check for replay-run isolation.
//!
//! Ported from `cli/src/fno/observer/isolation.py` (deleted in the same
//! change; itself resurrected verbatim from `cli/src/fno/evals/isolation.py`,
//! cut in 675b24e). "eval run" reads as "replay run".
//!
//! Scans real operator state files for replay session ids that should never
//! appear there. If any replay session id is found in a real-state surface,
//! the run has violated isolation. The PREVENTIVE layer (the replay's
//! workdir settings redirect) covers fno-paths-aware writers that read the
//! workdir-local settings.yaml when CWD is the workdir. The DETECTIVE layer
//! here is the load-bearing backstop for the writers that cannot be
//! redirected (register-task.py's hardcoded ledger, the bash paths.sh
//! STATE_DIR writers).

use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// One occurrence of an eval session id in a real operator state file
/// (isolation.py's `Violation`).
#[derive(Debug, Clone)]
pub(crate) struct Violation {
    pub path: String,
    pub session_id: String,
    pub line_number: usize,
    pub detail: String,
}

/// Result of a single isolation check (isolation.py's `IsolationResult`).
/// The coherence rule - violated iff violations is non-empty - is enforced
/// by construction in [`check_isolation`], and re-checked by its tests.
#[derive(Debug, Clone)]
pub(crate) struct IsolationResult {
    pub verdict: &'static str,
    pub violations: Vec<Violation>,
}

/// Maps a "surface key" to a one-line escape explanation used in violation
/// details (isolation.py `_ESCAPE_EXPLANATIONS` + `_DEFAULT_ESCAPE_EXPLANATION`).
fn escape_explanation(surface_key: &str) -> String {
    match surface_key {
        "ledger_json" => "writer bypasses project-local path overrides - known escapee: register-task.py ledger registration (scripts/metrics/register-task.py:23)".to_string(),
        "graph_json" => "writer bypasses project-local path overrides - known escapee: paths.sh STATE_DIR (scripts/lib/paths.sh:11-13)".to_string(),
        "repo_events_jsonl" | "global_events_jsonl" => "writer bypasses project-local path overrides - known escapee: paths.sh STATE_DIR bash-layer writers".to_string(),
        "memory_dir" => "memory-pass or autocorrect writer may have used real session context".to_string(),
        "corrections_log" => "autocorrect corrections.log writer may bypass project-local isolation".to_string(),
        _ => "writer bypasses project-local path overrides - check known escapees: register-task.py (scripts/metrics/register-task.py:23) and paths.sh STATE_DIR (scripts/lib/paths.sh:11-13)".to_string(),
    }
}

/// `_scan_whole_file` (isolation.py:192): scan a non-JSONL file (whole
/// content) for eval session ids. Reports the first matching line (1-based).
/// Missing or unreadable files are silently skipped.
fn scan_whole_file(
    path: &Path,
    eval_session_ids: &BTreeSet<String>,
    surface_key: &str,
) -> Vec<Violation> {
    let mut violations = Vec::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return violations;
    };
    let lines: Vec<&str> = text.lines().collect();
    for sid in eval_session_ids {
        if !text.contains(sid.as_str()) {
            continue;
        }
        // Find the first line containing the id.
        let line_number = lines
            .iter()
            .position(|line| line.contains(sid.as_str()))
            .map_or(1, |i| i + 1);
        violations.push(Violation {
            path: path.display().to_string(),
            session_id: sid.clone(),
            line_number,
            detail: escape_explanation(surface_key),
        });
    }
    violations
}

/// `_scan_memory_dir` (isolation.py:235): scan all files inside the memory
/// directory for eval session ids. Missing directory is silently skipped.
fn scan_memory_dir(memory_dir: &Path, eval_session_ids: &BTreeSet<String>) -> Vec<Violation> {
    if !memory_dir.is_dir() {
        return Vec::new();
    }
    let mut names: Vec<PathBuf> = std::fs::read_dir(memory_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|p| p.is_file())
        .collect();
    names.sort();
    names
        .iter()
        .flat_map(|f| scan_whole_file(f, eval_session_ids, "memory_dir"))
        .collect()
}

/// `_scan_state_file` (isolation.py:149): the four state surfaces. The graph
/// surface scans the raw file text: the Python leg parsed then repr()'d the
/// graph, which for ASCII session ids (the only shape the ledger and events
/// carry) detects the same occurrences as a raw scan, and the read-failure
/// behavior is preserved - a graph read failure is one violation per id,
/// never a silent clean.
fn scan_state_file(
    path: &Path,
    eval_session_ids: &BTreeSet<String>,
    surface_key: &str,
) -> Vec<Violation> {
    let explanation = escape_explanation(surface_key);
    let graph = surface_key == "graph_json";
    if !graph && !path.exists() {
        return Vec::new();
    }
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) if graph => {
            // The Python leg's graph read failure: every id counts as a
            // violation - never a silent clean (fail loud, fail
            // conservative).
            return eval_session_ids
                .iter()
                .map(|sid| Violation {
                    path: path.display().to_string(),
                    session_id: sid.clone(),
                    line_number: 1,
                    detail: explanation.clone(),
                })
                .collect();
        }
        Err(_) => return Vec::new(),
    };
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut violations: Vec<Violation> = Vec::new();
    for (lineno, line) in text.lines().enumerate() {
        let stripped = line.trim();
        if stripped.is_empty() {
            continue;
        }
        for sid in eval_session_ids {
            if seen_ids.contains(sid) {
                continue;
            }
            if line.contains(sid.as_str()) {
                violations.push(Violation {
                    path: path.display().to_string(),
                    session_id: sid.clone(),
                    line_number: lineno + 1,
                    detail: explanation.clone(),
                });
                seen_ids.insert(sid.clone());
            }
        }
    }
    violations
}

/// `check_isolation` (isolation.py:263): scan real operator state surfaces
/// for any eval session id leaks. Unknown keys ignored, missing files
/// silently skipped (clean).
pub(crate) fn check_isolation(
    eval_session_ids: &BTreeSet<String>,
    real_state_paths: &Map<String, Value>,
) -> IsolationResult {
    if eval_session_ids.is_empty() {
        return IsolationResult {
            verdict: "clean",
            violations: Vec::new(),
        };
    }
    let mut all: Vec<Violation> = Vec::new();
    for key in [
        "ledger_json",
        "graph_json",
        "repo_events_jsonl",
        "global_events_jsonl",
    ] {
        if let Some(Value::String(raw)) = real_state_paths.get(key) {
            all.extend(scan_state_file(Path::new(raw), eval_session_ids, key));
        }
    }
    // memory dir (whole-file scan per file), then the corrections log.
    if let Some(Value::String(raw)) = real_state_paths.get("memory_dir") {
        all.extend(scan_memory_dir(Path::new(raw), eval_session_ids));
    }
    if let Some(Value::String(raw)) = real_state_paths.get("corrections_log") {
        all.extend(scan_whole_file(
            Path::new(raw),
            eval_session_ids,
            "corrections_log",
        ));
    }
    let verdict = if all.is_empty() { "clean" } else { "violated" };
    IsolationResult {
        verdict,
        violations: all,
    }
}

/// `format_violation_report` (isolation.py:463): the human-readable report.
/// Empty when the verdict is clean.
pub(crate) fn format_violation_report(result: &IsolationResult) -> String {
    if result.verdict == "clean" {
        return String::new();
    }
    let mut lines = vec![
        format!(
            "[eval] ISOLATION VIOLATED: {} leak(s) detected",
            result.violations.len()
        ),
        String::new(),
    ];
    for v in &result.violations {
        lines.push(format!("  file:       {}", v.path));
        lines.push(format!("  session_id: {}", v.session_id));
        lines.push(format!("  line:       {}", v.line_number));
        lines.push(format!("  suspected:  {}", v.detail));
        lines.push(String::new());
    }
    lines.push(
        "[eval] Action: check whether the eval workdir settings.yaml was loaded \
         by the session (CWD must equal the workdir). Known escapees that cannot \
         be redirected: register-task.py (scripts/metrics/register-task.py:23) \
         and paths.sh STATE_DIR (scripts/lib/paths.sh:11-13)."
            .to_string(),
    );
    lines.join("\n")
}

/// `default_real_state_paths(repo_root)` (isolation.py:436): the canonical
/// REAL paths - NOT the workdir-local redirects. The point is to scan the
/// actual operator state for any leaked replay session id.
pub(crate) fn default_real_state_paths(repo_root: &Path) -> Map<String, Value> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Map::new();
    };
    let fno_home = home.join(".fno");
    Map::from_iter([
        ("ledger_json".into(), json!(fno_home.join("ledger.json"))),
        ("graph_json".into(), json!(fno_home.join("graph.json"))),
        (
            "repo_events_jsonl".into(),
            json!(repo_root.join(".fno/events.jsonl")),
        ),
        (
            "global_events_jsonl".into(),
            json!(fno_home.join("events.jsonl")),
        ),
        ("memory_dir".into(), json!(fno_home.join("memory"))),
        // corrections.log moved under logs/ in the state-root wave; scan it
        // there.
        (
            "corrections_log".into(),
            json!(fno_home
                .join("logs")
                .join("corrections.log")
                .display()
                .to_string()),
        ),
    ])
}

/// `collect_eval_session_ids`'s transcript-dir encoding (isolation.py:319):
/// Claude maps every non-[a-zA-Z0-9] character in the absolute path to '-'.
pub(crate) fn encode_path_for_claude_projects(path: &Path) -> String {
    path.display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// `collect_eval_session_ids` (isolation.py:328): gather every session id
/// attributable to the replay run in *workdir*, from three sources: the
/// workdir target-state manifest's `session_id:` frontmatter line, the
/// workdir events journal (every session_id field value), and transcript
/// filenames under `<claude_projects_dir>/<encoded-workdir>/`. Returns
/// (ids, transcript_path) where transcript_path is the first transcript file
/// found (fills the history row's transcript_path field).
pub(crate) fn collect_eval_session_ids(
    workdir: &Path,
    claude_projects_dir: Option<&Path>,
) -> (BTreeSet<String>, Option<String>) {
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut transcript_path: Option<String> = None;

    // 1. target-state.md manifest
    let manifest = workdir.join(".fno").join("target-state.md");
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        for line in text.lines() {
            let stripped = line.trim();
            if let Some(value) = stripped.strip_prefix("session_id:") {
                // Strip surrounding quotes: manifest values may be quoted
                // (other state files store the id unquoted).
                let value = value.trim().trim_matches(['\'', '"']);
                if !value.is_empty() {
                    ids.insert(value.to_string());
                }
            }
        }
    }

    // 2. workdir events journal: the store commit is the write boundary -
    // committed rows carry the history, raw bytes are only the fallback.
    let events = workdir.join(".fno").join("events.jsonl");
    if events.exists() {
        let routed = crate::event_store::journal_text(&events, &[]);
        let raw = if routed.is_empty() {
            std::fs::read_to_string(&events).unwrap_or_default()
        } else {
            routed
        };
        for line in raw.lines() {
            let stripped = line.trim();
            if stripped.is_empty() {
                continue;
            }
            let Ok(event) = serde_json::from_str::<Value>(stripped) else {
                continue;
            };
            let sid = event
                .get("session_id")
                .or_else(|| event.get("data").and_then(|d| d.get("session_id")))
                .and_then(Value::as_str);
            if let Some(sid) = sid.filter(|s| !s.is_empty()) {
                ids.insert(sid.to_string());
            }
        }
    }

    // 3. Transcript files under <claude_projects_dir>/<encoded-workdir>/
    if let Some(dir) = claude_projects_dir {
        let encoded = encode_path_for_claude_projects(workdir);
        let project_dir = dir.join(encoded);
        if project_dir.is_dir() {
            let mut transcripts: Vec<PathBuf> = std::fs::read_dir(&project_dir)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.path())
                .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
                .collect();
            transcripts.sort();
            if let Some(primary) = transcripts.first() {
                transcript_path = Some(primary.display().to_string());
            }
            // Also collect session ids from transcript filenames (UUID stems).
            for tf in transcripts {
                if let Some(stem) = tf.file_stem().and_then(|s| s.to_str()) {
                    if !stem.is_empty() {
                        ids.insert(stem.to_string());
                    }
                }
            }
        }
    }

    (ids, transcript_path)
}
