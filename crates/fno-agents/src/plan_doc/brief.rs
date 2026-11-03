//! Scoped task-brief generator for single-doc plans.
//!
//! The port of `cli/src/fno/plan/brief.py` (deleted 2026-11-02). `fno do plan
//! brief` output and the orchestrator's strategy read share this module. YAML
//! parsing runs on serde_yaml_ng, and a duplicate top-level key in the
//! strategy mapping refuses the way the Python leg's unique-key loader did.

use regex::Regex;
use serde_json::{json, Map, Value};
use std::sync::OnceLock;

/// Contract violation (exit 2 at the CLI boundary).
#[derive(Debug)]
pub struct BriefError(pub String);

impl std::fmt::Display for BriefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Malformed content (exit 3 at the CLI boundary).
#[derive(Debug)]
pub struct BriefParseError(pub String);

impl std::fmt::Display for BriefParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// --- execution strategy parsing -----------------------------------------

static FENCE_YAML_RE: OnceLock<Regex> = OnceLock::new();
fn fence_yaml_re() -> &'static Regex {
    FENCE_YAML_RE.get_or_init(|| Regex::new(r"(?si)```ya?ml\s*\n(.*?)```").unwrap())
}

/// Parse the Execution Strategy section's YAML block. Prose around a fenced
/// ```yaml block is allowed; fenceless text is treated as YAML directly.
pub fn parse_execution_strategy(yaml_text: &str) -> Result<Value, BriefParseError> {
    let raw_yaml = match fence_yaml_re().captures(yaml_text) {
        Some(m) => m.get(1).unwrap().as_str().to_string(),
        None => yaml_text.to_string(),
    };
    let parsed: serde_yaml_ng::Value = match serde_yaml_ng::from_str(&raw_yaml) {
        Ok(v) => v,
        Err(e) => {
            let line = e.location().map(|l| l.line());
            let suffix = line.map(|l| format!(" at line {l}")).unwrap_or_default();
            let msg = format!("Execution Strategy YAML is malformed{suffix}: {e}");
            return Err(BriefParseError(msg));
        }
    };
    if parsed.is_null() {
        return Ok(json!({"tasks": [], "waves": []}));
    }
    let map = match parsed {
        serde_yaml_ng::Value::Mapping(map) => map,
        other => {
            return Err(BriefParseError(format!(
                "Execution Strategy must be a YAML mapping, got {}",
                crate::plan_doc::doc::yaml_type_name(&other)
            )));
        }
    };
    build_strategy_from_mapping(&map)
}

/// Duplicate-key-refusing mapping build, the unique-key loader's contract.
fn build_strategy_from_mapping(map: &serde_yaml_ng::Mapping) -> Result<Value, BriefParseError> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for k in map.keys() {
        let key = crate::plan_doc::doc::py_key(k);
        if !seen.insert(key.clone()) {
            return Err(BriefParseError(format!("duplicate YAML key {key:?}")));
        }
    }
    let root = crate::plan_doc::doc::yaml_to_json(&serde_yaml_ng::Value::Mapping(map.clone()));
    normalize_strategy(root)
}

/// The parse + normalize half of the Python `parse_execution_strategy`.
fn normalize_strategy(mut root: Value) -> Result<Value, BriefParseError> {
    let tasks_v = root.get("tasks").cloned().unwrap_or(Value::Array(vec![]));
    let tasks_arr = match tasks_v {
        Value::Array(a) => a,
        _ => {
            return Err(BriefParseError(
                "Execution Strategy 'tasks' must be a list".into(),
            ));
        }
    };
    let mut normalized: Vec<Value> = Vec::new();
    for (index, t) in tasks_arr.iter().enumerate() {
        let t = match t {
            Value::Object(o) => o,
            _ => {
                let msg = format!("Execution Strategy task {} must be a mapping", index + 1);
                return Err(BriefParseError(msg));
            }
        };
        let tid = py_field(t.get("id"));
        let surface = match t.get("surface") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                let msg = format!("Execution Strategy task '{tid}' surface must be a list");
                return Err(BriefParseError(msg));
            }
        };
        for (i, item) in surface.iter().enumerate() {
            let ok = matches!(item, Value::String(s) if !s.trim().is_empty());
            if !ok {
                let msg = format!(
                    "Execution Strategy task '{tid}' surface item {} must be a non-empty path string",
                    i + 1
                );
                return Err(BriefParseError(msg));
            }
        }
        let acceptance = match t.get("acceptance") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                let msg = format!("Execution Strategy task '{tid}' acceptance must be a list");
                return Err(BriefParseError(msg));
            }
        };
        for (i, item) in acceptance.iter().enumerate() {
            let ok = matches!(item, Value::String(s) if !s.trim().is_empty());
            if !ok {
                let msg = format!(
                    "Execution Strategy task '{tid}' acceptance item {} must be a non-empty string",
                    i + 1
                );
                return Err(BriefParseError(msg));
            }
        }
        let blocked = match t.get("blocked_by") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.clone(),
            Some(_) => {
                let msg = format!(
                    "Execution Strategy task '{tid}' blocked_by must be a list of task ids"
                );
                return Err(BriefParseError(msg));
            }
        };
        for (i, item) in blocked.iter().enumerate() {
            let ok = matches!(item, Value::String(s) if !s.trim().is_empty());
            if !ok {
                let msg = format!(
                    "Execution Strategy task '{tid}' blocked_by item {} must be a non-empty task id",
                    i + 1
                );
                return Err(BriefParseError(msg));
            }
        }
        let entry = json!({
            "id": tid,
            "title": py_field(t.get("title")),
            "surface": surface,
            "verify": py_field(t.get("verify")),
            "acceptance": acceptance.iter().map(|v| py_string(Some(v))).collect::<Vec<_>>(),
            "notes": py_field(t.get("notes")).trim().to_string(),
            "blocked_by": blocked.iter().map(|v| py_string(Some(v))).collect::<Vec<_>>(),
            "blocked_by_declared": t.get("blocked_by").is_some(),
        });
        normalized.push(entry);
    }
    let waves_v = root.get("waves").cloned().unwrap_or(Value::Array(vec![]));
    let waves = match waves_v {
        Value::Array(a) => a,
        _ => {
            return Err(BriefParseError(
                "Execution Strategy 'waves' must be a list".into(),
            ));
        }
    };
    let mode = root
        .get("execution_mode")
        .cloned()
        .unwrap_or(Value::String("sequential".into()));
    let obj = root
        .as_object_mut()
        .ok_or_else(|| BriefParseError("Execution Strategy must be a YAML mapping".into()))?;
    obj.insert("tasks".into(), Value::Array(normalized));
    obj.insert("waves".into(), Value::Array(waves));
    obj.insert("execution_mode".into(), mode);
    Ok(root)
}

/// Python `str()` semantics for a scalar JSON value read from YAML.
fn py_string(v: Option<&Value>) -> String {
    crate::plan_doc::doc::py_str(v)
}

/// `str(t.get(key, ""))`: an absent key reads "", an explicit null "None".
fn py_field(v: Option<&Value>) -> String {
    v.map(|v| py_string(Some(v))).unwrap_or_default()
}

/// The task dict for `task_id`, or None.
pub fn find_task<'a>(parsed: &'a Value, task_id: &str) -> Option<&'a Value> {
    parsed
        .get("tasks")
        .and_then(|t| t.as_array())
        .and_then(|arr| arr.iter().find(|t| py_field(t.get("id")) == task_id))
}

/// All task ids from the parsed execution strategy.
pub fn list_task_ids(parsed: &Value) -> Vec<String> {
    parsed
        .get("tasks")
        .and_then(|t| t.as_array())
        .map(|arr| arr.iter().map(|t| py_field(t.get("id"))).collect())
        .unwrap_or_default()
}

/// One error string per bad per-task `blocked_by` edge; empty when clean.
/// Unknown ids are checked on declared edges; cycles on effective edges
/// (inherited previous-wave blockers included).
pub fn validate_task_edges(parsed: &Value) -> Vec<String> {
    let tasks = parsed.get("tasks").and_then(|t| t.as_array());
    let Some(tasks) = tasks else {
        return Vec::new();
    };
    let ids = list_task_ids(parsed);
    let known: std::collections::HashSet<&str> = ids.iter().map(|s| s.as_str()).collect();
    let mut declared: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for task in tasks {
        let id = py_string(task.get("id"));
        let declared_key = match task.get("blocked_by_declared") {
            Some(Value::Bool(b)) => *b,
            _ => task.get("blocked_by").is_some(),
        };
        if declared_key {
            let deps = task
                .get("blocked_by")
                .and_then(|b| b.as_array())
                .map(|a| a.iter().map(|v| py_string(Some(v))).collect())
                .unwrap_or_default();
            declared.insert(id, deps);
        }
    }
    let mut errors: Vec<String> = Vec::new();
    for (task_id, deps) in &declared {
        for dep in deps {
            if !known.contains(dep.as_str()) {
                errors.push(format!("task {task_id} blocked_by unknown task {dep}"));
            }
        }
    }
    // Wave membership: first occurrence wins (`setdefault`).
    let mut wave_tasks: Vec<Vec<String>> = Vec::new();
    let mut task_wave: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    if let Some(waves) = parsed.get("waves").and_then(|w| w.as_array()) {
        for wave in waves {
            let Some(wave) = wave.as_object() else {
                continue;
            };
            let raw = wave.get("tasks").cloned().unwrap_or(Value::Array(vec![]));
            let current: Vec<String> = match raw {
                Value::Array(a) => a.iter().map(|v| py_string(Some(v))).collect(),
                other => vec![py_string(Some(&other))],
            };
            for tid in &current {
                task_wave.entry(tid.clone()).or_insert(wave_tasks.len());
            }
            wave_tasks.push(current);
        }
    }
    let mut effective: Vec<(String, Vec<String>)> = Vec::new();
    for tid in &ids {
        if let Some(deps) = declared.get(tid) {
            effective.push((tid.clone(), deps.clone()));
            continue;
        }
        let position = task_wave.get(tid).copied();
        let deps = match position {
            Some(p) if p > 0 => wave_tasks[p - 1].clone(),
            _ => Vec::new(),
        };
        effective.push((tid.clone(), deps));
    }
    // Iterative DFS with a colour map; a grey hit spells the cycle out.
    let mut colour: std::collections::HashMap<&str, u8> =
        effective.iter().map(|(id, _)| (id.as_str(), 0u8)).collect();
    let order_index: std::collections::HashMap<&str, usize> = effective
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.as_str(), i))
        .collect();
    let mut path: Vec<&str> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    for root_idx in 0..effective.len() {
        if colour[effective[root_idx].0.as_str()] != 0 {
            continue;
        }
        colour.insert(effective[root_idx].0.as_str(), 1);
        path.clear();
        path.push(effective[root_idx].0.as_str());
        stack.clear();
        stack.push(0usize);
        while let Some(&top) = stack.last() {
            let node = path[path.len() - 1];
            let deps = &effective[order_index[node]].1;
            if top < deps.len() {
                *stack.last_mut().unwrap() += 1;
                let dependency = deps[top].as_str();
                let state = colour.get(dependency).copied();
                if state == Some(1) {
                    let start = path.iter().position(|p| *p == dependency).unwrap();
                    let mut cycle: Vec<&str> = path[start..].to_vec();
                    cycle.push(dependency);
                    errors.push(format!("cycle: {}", cycle.join(" -> ")));
                } else if state == Some(0) {
                    colour.insert(dependency, 1);
                    path.push(dependency);
                    stack.push(0usize);
                }
            } else {
                stack.pop();
                if let Some(p) = path.pop() {
                    colour.insert(p, 2);
                }
            }
        }
    }
    errors
}

// --- section parsers ------------------------------------------------------

/// The first non-empty paragraph from the Overview section.
pub fn extract_overview_paragraph(overview_text: &str) -> String {
    overview_text
        .trim()
        .split("\n\n")
        .map(|p| p.trim())
        .find(|p| !p.is_empty())
        .unwrap_or("")
        .to_string()
}

/// One numbered item's start: the regex crate has no lookahead, so items
/// split at these line starts instead of `(?=^\d+\.\s+\*\*|\Z)`.
fn item_start_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"(?m)^\d+\.\s+\*\*").unwrap())
}

fn numbered_decision_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"(?s)\A(\d+)\.\s+\*\*(.+?)\*\*\s*(.*)").unwrap())
}

fn why_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"(?s)\*Why:\*\s*(.+?)(?:\*How to apply:\*|\z)").unwrap())
}

fn apply_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"(?s)\*How to apply:\*\s*(.+)").unwrap())
}

/// The numbered items of a section, each from its start to the next start.
fn numbered_items(text: &str) -> Vec<&str> {
    let starts: Vec<usize> = item_start_re().find_iter(text).map(|m| m.start()).collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, &at)| &text[at..starts.get(i + 1).copied().unwrap_or(text.len())])
        .collect()
}

/// Parse the Locked Decisions section into structured entries.
pub fn parse_locked_decisions(text: &str) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    for item in numbered_items(text) {
        let Some(m) = numbered_decision_re().captures(item) else {
            continue;
        };
        let number: usize = m.get(1).unwrap().as_str().parse().unwrap_or(0);
        let title = m
            .get(2)
            .unwrap()
            .as_str()
            .trim_end_matches('.')
            .trim()
            .to_string();
        let body = m.get(3).unwrap().as_str().trim().to_string();
        let rationale = match why_re().captures(&body) {
            Some(wm) => wm
                .get(1)
                .unwrap()
                .as_str()
                .trim()
                .trim_end_matches('.')
                .to_string(),
            None => body.clone(),
        };
        let application = match apply_re().captures(&body) {
            Some(am) => am
                .get(1)
                .unwrap()
                .as_str()
                .trim()
                .trim_end_matches('.')
                .to_string(),
            None => String::new(),
        };
        entries.push(json!({
            "number": number,
            "title": title,
            "rationale": rationale,
            "application": application,
            "tags": [],
            "body": body,
        }));
    }
    entries
}

/// Parse the Failure Modes section: `**Category**` headers plus bullets.
pub fn parse_failure_modes(text: &str) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    let mut current_category = "General".to_string();
    for line in text.lines() {
        let line = line.trim_end();
        let cat_re: &Regex = bold_line_re();
        if let Some(cm) = cat_re.captures(line) {
            current_category = cm.get(1).unwrap().as_str().trim().to_string();
            continue;
        }
        if let Some(bm) = bullet_line_re().captures(line) {
            entries.push(json!({
                "category": current_category,
                "bullet": bm.get(1).unwrap().as_str().trim().to_string(),
                "tags": [],
            }));
        }
    }
    entries
}

fn bold_line_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\*\*(.+?)\*\*\s*$").unwrap())
}

fn bullet_line_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^[-*]\s+(.+)$").unwrap())
}

/// Parse the Patterns to Reuse section's markdown table.
pub fn parse_patterns(text: &str) -> Vec<Value> {
    let mut entries: Vec<Value> = Vec::new();
    let mut header_seen = false;
    let mut separator_seen = false;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line.split('|').collect();
        let cells: Vec<&str> = if cells.len() >= 3 {
            cells[1..cells.len() - 1].iter().map(|c| c.trim()).collect()
        } else {
            continue;
        };
        if cells.len() < 3 {
            continue;
        }
        if !header_seen {
            header_seen = true;
            continue;
        }
        if !separator_seen && cells.iter().all(|c| c.is_empty() || sep_cell_ok(c)) {
            separator_seen = true;
            continue;
        }
        entries.push(json!({
            "pattern": cells[0].trim_matches('`').to_string(),
            "source": cells[1],
            "why": cells[2],
        }));
    }
    entries
}

fn sep_cell_ok(c: &str) -> bool {
    static CELL: OnceLock<Regex> = OnceLock::new();
    let re = CELL.get_or_init(|| Regex::new(r"^:?-+:?$").unwrap());
    re.is_match(c.trim())
}

/// Legacy-shaped AC entries over the canonical compiler; refusal degrades to
/// "no criteria" (a worker brief never crashes on a legacy section).
pub fn parse_acceptance_criteria(text: &str) -> Vec<Value> {
    match crate::plan_doc::criteria::compile_criteria(text, "Acceptance Criteria") {
        Ok(criteria) => criteria
            .iter()
            .map(|c| {
                json!({
                    "ac_type": c.kind,
                    "code": c.code,
                    "text": c.text,
                    "tags": [],
                })
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

// --- filtering ------------------------------------------------------------

/// True when any surface path's last 1-2 components appear in text.
fn surface_path_matches(text: &str, surface_paths: &[&str]) -> bool {
    let text_lower = text.to_lowercase();
    for sp in surface_paths {
        let parts: Vec<&str> = sp.split('/').collect();
        if let Some(last) = parts.last() {
            if !last.is_empty() && text_lower.contains(&last.to_lowercase()) {
                return true;
            }
        }
        if parts.len() >= 2 {
            let last2 = parts[parts.len() - 2..].join("/");
            if text_lower.contains(&last2.to_lowercase()) {
                return true;
            }
        }
        if parts.len() == 1 && parts[0].is_empty() {
            return false;
        }
    }
    false
}

/// Mode-filter entries by surface relevance: all / none / relevant.
pub fn filter_entries(entries: Vec<Value>, mode: &str, surface_paths: &[&str]) -> Vec<Value> {
    if mode == "all" {
        return entries;
    }
    if mode == "none" {
        return Vec::new();
    }
    let mut result = Vec::new();
    for entry in entries {
        let body = entry
            .get("body")
            .and_then(|b| b.as_str())
            .or_else(|| entry.get("bullet").and_then(|b| b.as_str()))
            .or_else(|| entry.get("rationale").and_then(|b| b.as_str()))
            .unwrap_or("");
        let title = entry.get("title").and_then(|t| t.as_str()).unwrap_or("");
        let combined = format!("{title} {body}");
        if surface_path_matches(&combined, surface_paths)
            || entry
                .get("tags")
                .map(|t| t.as_array().map(|a| a.is_empty()).unwrap_or(true))
                .unwrap_or(true)
        {
            result.push(entry);
        }
    }
    result
}

/// Patterns whose source path is within the task's surface paths.
pub fn filter_patterns_by_surface(entries: Vec<Value>, surface_paths: &[&str]) -> Vec<Value> {
    let mut result = Vec::new();
    for entry in entries {
        let source = entry.get("source").and_then(|s| s.as_str()).unwrap_or("");
        let matched = surface_path_matches(source, surface_paths)
            || surface_paths
                .iter()
                .any(|sp| source.contains(sp) || sp.contains(source));
        if matched {
            result.push(entry);
        }
    }
    result
}

/// ACs relevant to this task under the fail-open contract.
pub fn filter_acs_for_task(
    entries: Vec<Value>,
    task_acceptance: &[String],
    globally_tagged_codes: Option<&std::collections::HashSet<String>>,
) -> Vec<Value> {
    if task_acceptance.is_empty() {
        return entries;
    }
    let mut result = Vec::new();
    for entry in entries {
        let code = entry.get("code").and_then(|c| c.as_str()).unwrap_or("");
        if task_acceptance.iter().any(|t| t == code) {
            result.push(entry);
            continue;
        }
        let untagged = match globally_tagged_codes {
            None => true,
            Some(set) => !set.contains(code),
        };
        if untagged {
            result.push(entry);
        }
    }
    result
}

// --- BriefResult ---------------------------------------------------------

/// Structured brief for a single task, with the Python leg's two renderings.
pub struct BriefResult {
    pub project_context: String,
    pub task_spec: Value,
    pub acceptance_criteria: Vec<Value>,
    pub locked_decisions: Vec<Value>,
    pub failure_modes: Vec<Value>,
    pub files: Vec<Value>,
    pub patterns: Vec<Value>,
    pub verify_command: String,
}

fn s<'a>(v: Option<&'a Map<String, Value>>, key: &'static str) -> &'a str {
    v.and_then(|o| o.get(key))
        .and_then(|x| x.as_str())
        .unwrap_or("")
}

impl BriefResult {
    /// The fixed JSON schema dict, the port of `to_json_dict`.
    pub fn to_json(&self) -> Value {
        let t = self.task_spec.as_object();
        json!({
            "project_context": self.project_context,
            "task_spec": {
                "id": s(t, "id"),
                "title": s(t, "title"),
                "surface": self.task_spec.get("surface").cloned().unwrap_or(json!([])),
                "verify": s(t, "verify"),
                "acceptance": self.task_spec.get("acceptance").cloned().unwrap_or(json!([])),
                "notes": s(t, "notes"),
            },
            "acceptance_criteria": self.acceptance_criteria.iter().map(|ac| json!({
                "ac_type": ac.get("ac_type").cloned().unwrap_or(json!("")),
                "code": ac.get("code").cloned().unwrap_or(json!("")),
                "text": ac.get("text").cloned().unwrap_or(json!("")),
                "tags": ac.get("tags").cloned().unwrap_or(json!([])),
            })).collect::<Vec<_>>(),
            "locked_decisions": self.locked_decisions.iter().map(|ld| json!({
                "number": ld.get("number").cloned().unwrap_or(json!(0)),
                "title": ld.get("title").cloned().unwrap_or(json!("")),
                "rationale": ld.get("rationale").cloned().unwrap_or(json!("")),
                "application": ld.get("application").cloned().unwrap_or(json!("")),
                "tags": ld.get("tags").cloned().unwrap_or(json!([])),
            })).collect::<Vec<_>>(),
            "failure_modes": self.failure_modes.iter().map(|fm| json!({
                "category": fm.get("category").map(|v| v.clone()).unwrap_or(json!("")),
                "bullet": fm.get("bullet").cloned().unwrap_or(json!("")),
                "tags": fm.get("tags").cloned().unwrap_or(json!([])),
            })).collect::<Vec<_>>(),
            "files": self.files.iter().map(|f| json!({
                "path": f.get("path").cloned().unwrap_or(json!("")),
                "action": f.get("action").cloned().unwrap_or(json!("")),
                "notes": f.get("notes").cloned().unwrap_or(json!("")),
            })).collect::<Vec<_>>(),
            "patterns": self.patterns.iter().map(|p| json!({
                "pattern": p.get("pattern").cloned().unwrap_or(json!("")),
                "source": p.get("source").cloned().unwrap_or(json!("")),
                "why": p.get("why").cloned().unwrap_or(json!("")),
            })).collect::<Vec<_>>(),
            "verify_command": self.verify_command,
        })
    }

    /// The ~500-800 word markdown rendering, byte-shaped like the Python.
    pub fn to_markdown(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let t = self.task_spec.as_object();
        lines.push("## Project Context".into());
        lines.push(String::new());
        lines.push(self.project_context.clone());
        lines.push(String::new());
        lines.push(format!("## Task {}: {}", s(t, "id"), s(t, "title")));
        lines.push(String::new());
        let notes = s(t, "notes");
        if !notes.is_empty() {
            lines.push(notes.to_string());
            lines.push(String::new());
        }
        if !self.files.is_empty() {
            lines.push("### Files".into());
            lines.push(String::new());
            for f in &self.files {
                let path = f.get("path").and_then(Value::as_str).unwrap_or("");
                let notes_str = match f.get("notes").and_then(Value::as_str) {
                    Some(n) if !n.is_empty() => format!(" ({n})"),
                    _ => String::new(),
                };
                lines.push(format!("- `{path}`{notes_str}"));
            }
            lines.push(String::new());
        }
        if !self.acceptance_criteria.is_empty() {
            lines.push("### Acceptance Criteria".into());
            lines.push(String::new());
            for ac in &self.acceptance_criteria {
                let code = ac.get("code").and_then(Value::as_str).unwrap_or("");
                let text = ac.get("text").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("**{code}:** {text}"));
                lines.push(String::new());
            }
        }
        if !self.locked_decisions.is_empty() {
            lines.push("### Locked Decisions".into());
            lines.push(String::new());
            for ld in &self.locked_decisions {
                let num = ld.get("number").cloned().unwrap_or(json!(0));
                let title = ld.get("title").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("{num}. **{title}**"));
                let rationale = ld.get("rationale").and_then(Value::as_str).unwrap_or("");
                if !rationale.is_empty() {
                    lines.push(format!("   *Why:* {rationale}"));
                }
                let application = ld.get("application").and_then(Value::as_str).unwrap_or("");
                if !application.is_empty() {
                    lines.push(format!("   *How to apply:* {application}"));
                }
            }
            lines.push(String::new());
        }
        if !self.failure_modes.is_empty() {
            lines.push("### Failure Modes".into());
            lines.push(String::new());
            let mut current_cat: Option<String> = None;
            for fm in &self.failure_modes {
                let cat = fm.get("category").and_then(Value::as_str).unwrap_or("");
                if current_cat.as_deref() != Some(cat) {
                    current_cat = Some(cat.to_string());
                    lines.push(format!("**{cat}**"));
                }
                let bullet = fm.get("bullet").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("- {bullet}"));
            }
            lines.push(String::new());
        }
        if !self.patterns.is_empty() {
            lines.push("### Patterns to Reuse".into());
            lines.push(String::new());
            for p in &self.patterns {
                let pattern = p.get("pattern").and_then(Value::as_str).unwrap_or("");
                let source = p.get("source").and_then(Value::as_str).unwrap_or("");
                let why = p.get("why").and_then(Value::as_str).unwrap_or("");
                lines.push(format!("- `{pattern}` from `{source}`: {why}"));
            }
            lines.push(String::new());
        }
        lines.push("### Verify Command".into());
        lines.push(String::new());
        lines.push("```".into());
        lines.push(self.verify_command.clone());
        lines.push("```".into());
        lines.push(String::new());
        lines.join("\n")
    }
}

// --- builder -------------------------------------------------------------

/// Find the Locked Decisions text, trying both heading variants.
fn locked_decisions_text(doc: &crate::plan_doc::doc::PlanDocument) -> String {
    for name in ["Locked Decisions (DO NOT revisit)", "Locked Decisions"] {
        if let Some(t) = doc.get_section(name) {
            return t.to_string();
        }
    }
    String::new()
}

/// Build the task brief for `task_id` from a parsed plan doc.
pub fn build_brief(
    doc: &crate::plan_doc::doc::PlanDocument,
    task_id: &str,
    include_failure_modes: &str,
    include_locked_decisions: &str,
) -> Result<BriefResult, BriefError> {
    build_brief_inner(
        doc,
        task_id,
        include_failure_modes,
        include_locked_decisions,
    )
}

fn build_brief_inner(
    doc: &crate::plan_doc::doc::PlanDocument,
    task_id: &str,
    include_failure_modes: &str,
    include_locked_decisions: &str,
) -> Result<BriefResult, BriefError> {
    let exec_section = doc.get_section("Execution Strategy");
    if exec_section.is_none() {
        return Err(BriefError(
            "Plan is missing ## Execution Strategy section. Run /blueprint to generate it.".into(),
        ));
    }
    let parsed = match parse_execution_strategy(exec_section.unwrap()) {
        Ok(p) => p,
        Err(e) => return Err(BriefError(format!("exit3:{e}"))),
    };
    let task = match find_task(&parsed, task_id) {
        Some(t) => t,
        None => {
            let repr = crate::client_commands::py_repr_str;
            let valid: Vec<String> = list_task_ids(&parsed).iter().map(|id| repr(id)).collect();
            return Err(BriefError(format!(
                "Task {} not found in Execution Strategy. Valid task-ids: [{}]",
                repr(task_id),
                valid.join(", ")
            )));
        }
    };
    let surface_paths: Vec<String> = task
        .get("surface")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| py_string(Some(v))).collect())
        .unwrap_or_default();
    let surface_refs: Vec<&str> = surface_paths.iter().map(|p| p.as_str()).collect();
    let overview = doc.get_section("Overview").unwrap_or("");
    let project_context = extract_overview_paragraph(overview);
    let ac_text = doc.get_section("Acceptance Criteria").unwrap_or("");
    let all_acs = parse_acceptance_criteria(ac_text);
    let task_acceptance: Vec<String> = task
        .get("acceptance")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| py_string(Some(v))).collect())
        .unwrap_or_default();
    let mut globally_tagged: std::collections::HashSet<String> = std::collections::HashSet::new();
    if let Some(tasks) = parsed.get("tasks").and_then(Value::as_array) {
        for t in tasks {
            for code in t
                .get("acceptance")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                globally_tagged.insert(py_string(Some(code)));
            }
        }
    }
    let acceptance_criteria =
        filter_acs_for_task(all_acs, &task_acceptance, Some(&globally_tagged));
    let ld_text = locked_decisions_text(doc);
    let all_lds = parse_locked_decisions(&ld_text);
    let locked_decisions = filter_entries(all_lds, include_locked_decisions, &surface_refs);
    let fm_text = doc.get_section("Failure Modes").unwrap_or("");
    let all_fms = parse_failure_modes(&fm_text);
    let failure_modes = filter_entries(all_fms, include_failure_modes, &surface_refs);
    let files: Vec<Value> = surface_paths
        .iter()
        .map(|p| json!({"path": p, "action": "modify", "notes": ""}))
        .collect();
    let patterns_text = doc.get_section("Patterns to Reuse").unwrap_or("");
    let all_patterns = parse_patterns(&patterns_text);
    let patterns = filter_patterns_by_surface(all_patterns, &surface_refs);
    let verify = task
        .get("verify")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Ok(BriefResult {
        project_context,
        task_spec: task.clone(),
        acceptance_criteria,
        locked_decisions,
        failure_modes,
        files,
        patterns,
        verify_command: verify,
    })
}

#[cfg(test)]
#[path = "brief_tests.rs"]
mod tests;
