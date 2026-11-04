//! Semantic validation for executable single-doc plans, the port of
//! `cli/src/fno/plan/execution_validation.py`. Violations refuse the plan;
//! warnings name a smell and let it through (the total-chain check fires on
//! 4 plans in a fortnight, where a refusal would fire on 172 and teach
//! authors to route around the validator).

use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use super::brief::{parse_execution_strategy, validate_task_edges, BriefParseError};
use super::criteria::{compile_criteria, CriteriaParseError};
use super::doc::PlanDocument;

/// A single validation finding; `field` names the offending axis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionViolation {
    pub field: String,
    pub message: String,
}

fn violation(field: &str, message: impl Into<String>) -> ExecutionViolation {
    ExecutionViolation {
        field: field.to_string(),
        message: message.into(),
    }
}

/// Violations refuse the plan; warnings name a smell and let it through.
#[derive(Debug, Clone, Default)]
pub struct ExecutionValidationResult {
    pub violations: Vec<ExecutionViolation>,
    pub warnings: Vec<ExecutionViolation>,
}

const QUICK_REQUIRED_SECTIONS: [&str; 4] =
    ["Context", "Changes", "Files to Modify", "Verification"];
const EXECUTION_MODES: [&str; 3] = ["mixed", "parallel", "sequential"];
const WAVE_MODES: [&str; 2] = ["parallel", "sequential"];
const WAVE_DIFFICULTIES: [&str; 3] = ["high", "low", "medium"];

fn task_id_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\d+(?:\.\d+)*[A-Za-z]*$").unwrap())
}

const QUICK_PLACEHOLDERS: [&str; 7] = [
    "[change name]",
    "[describe the change",
    "[concrete runnable check]",
    "[how to verify",
    "path/to/",
    "replace me",
    "fill in",
];

/// Command names that are also ordinary English commands, so a prose line can
/// lead with one, plus the determiners that follow them in a sentence but
/// never in an invocation. Matching the PAIR is what keeps `cargo check` and
/// `make test` runnable while `make sure the button works` is not.
const AMBIGUOUS_COMMANDS: [&str; 11] = [
    "cd", "check", "ensure", "go", "java", "just", "make", "manually", "node", "test", "verify",
];

/// No `all`: `make all` is a real default target, and it outranks `make all
/// the tests pass` as a thing an author writes.
const PROSE_FOLLOWERS: [&str; 16] = [
    "a", "an", "each", "every", "inspect", "it", "its", "my", "that", "the", "tests", "this", "to",
    "into", "your", "sure",
];

const PROSE_ONLY_COMMANDS: [&str; 2] = ["handwave", "manually"];
const COMMAND_WRAPPERS: [&str; 5] = ["command", "env", "gtimeout", "sudo", "timeout"];
const CONVENTIONAL_FILENAMES: [&str; 13] = [
    "brewfile",
    "changelog",
    "codeowners",
    "dockerfile",
    "gemfile",
    "justfile",
    "license",
    "makefile",
    "notice",
    "procfile",
    "rakefile",
    "readme",
    "vagrantfile",
];

const NON_EXECUTABLE_SUFFIXES: [&str; 9] = [
    ".md", ".txt", ".rst", ".json", ".yaml", ".yml", ".toml", ".lock", ".csv",
];

fn env_assignment_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*=").unwrap())
}

fn shell_parameter_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| {
        Regex::new(r"\$(?:\{[A-Za-z_][A-Za-z0-9_]*\}|[A-Za-z_][A-Za-z0-9_]*)").unwrap()
    })
}

fn shell_segment_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"&&|\|\||[;|]").unwrap())
}

fn file_reject_re() -> &'static Regex {
    // Brackets stay legal: `app/[slug]/page.tsx` is a routing convention, not
    // a glob.
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r#"[\s`$&|;<>(){}*?"'!]"#).unwrap())
}

fn file_extension_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^[\w.+-]+\.[A-Za-z0-9]+$").unwrap())
}

fn duration_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\d+(?:\.\d+)?[smhd]?$").unwrap())
}

fn evidence_ref_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^(done_probes|close_probes)\[(\d+)\]$").unwrap())
}

fn backtick_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"`([^`\n]+)`").unwrap())
}

fn bullet_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^[-*+]\s+([^\s]+)").unwrap())
}

fn list_prefix_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\s*(?:[-*+] |\d+[.)] )").unwrap())
}

fn table_rule_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^\|?[\s:|-]+\|?$").unwrap())
}

/// Python repr for a scalar JSON value, the `{x!r}` spelling in refusals.
fn python_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

/// Python str() for a scalar JSON value, the `{str(x)}` spelling in labels.
fn python_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Warn when every task declares one blocker and the graph is a total order.
///
/// Per-task `blocked_by` shipped as a precision tool and became an authoring
/// reflex: declared chains override wave inheritance outright, so a reflex
/// chain is not merely redundant with the waves. WARN, never refuse. The
/// message asks for the artifact each edge consumes, because that is the
/// question whose answer either justifies the edge or deletes it.
fn total_chain_warning(tasks: &[Value]) -> Option<ExecutionViolation> {
    let mut edges: HashMap<String, String> = HashMap::new();
    let mut ids: Vec<String> = Vec::new();
    let mut unblocked: Vec<String> = Vec::new();
    for task in tasks {
        let Some(task) = task.as_object() else {
            return None;
        };
        let tid = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if tid.is_empty() {
            return None;
        }
        ids.push(tid.clone());
        // An absent key inherits the previous wave and is the shape this check
        // exists to preserve, so only an EXPLICIT declaration counts.
        // `parse_execution_strategy` normalizes `blocked_by` to a list either
        // way and records the presence of the key separately, so testing the
        // value's type reads an undeclared task as an empty list and counts it
        // as the chain head - a "total order" whose first edge nobody authored.
        // `blocked_by_declared` is the only honest absence marker.
        let declared_flag = task
            .get("blocked_by_declared")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| task.contains_key("blocked_by"));
        if !declared_flag {
            return None;
        }
        let declared = match task.get("blocked_by").and_then(Value::as_array) {
            Some(a) => a,
            None => return None,
        };
        // The head of a real chain declares `blocked_by: []` (the live
        // specimen does exactly that), so an empty list is the chain's start,
        // not a disqualifier - demanding one blocker from every task rejects
        // the very head that makes the order total.
        if declared.is_empty() {
            unblocked.push(tid);
            continue;
        }
        if declared.len() != 1 {
            return None;
        }
        let blocker = declared[0].as_str().unwrap_or("").trim().to_string();
        if blocker.is_empty() {
            return None;
        }
        edges.insert(tid, blocker);
    }
    // Two tasks and one edge is a pair, not a reflex; the signal starts where
    // a chain spans a wave boundary it could not have inherited.
    let unique: HashSet<&String> = ids.iter().collect();
    if ids.len() < 3 || unique.len() != ids.len() {
        return None;
    }
    // Exactly one head, and every other task on a single-blocker edge.
    if unblocked.len() != 1 || edges.len() != ids.len() - 1 {
        return None;
    }
    let known: HashSet<&String> = ids.iter().collect();
    if edges.values().any(|b| !known.contains(b)) {
        return None;
    }
    // A total order starts at the one head and, following the edges, visits
    // every task exactly once.
    let mut reverse: HashMap<&String, &String> = HashMap::new();
    for (tid, blocker) in &edges {
        if reverse.insert(blocker, tid).is_some() {
            return None; // a fan-out: two tasks share one blocker, not a chain
        }
    }
    let mut order = vec![unblocked[0].clone()];
    while let Some(next) = reverse.get(order.last().unwrap()) {
        let next = (*next).clone();
        if order.contains(&next) {
            return None; // a cycle; validate_task_edges already refuses it
        }
        order.push(next);
    }
    if order.len() != ids.len() {
        return None;
    }
    Some(violation(
        "tasks.blocked_by",
        format!(
            "every task declares exactly one blocker and the chain is total ({}), so this plan \
             measures width 1 and `fno backlog join` has nothing to hand a second worker. A \
             declared blocker wins outright over wave inheritance, so these lines override the \
             waves rather than restate them. Name the artifact each edge consumes; delete the \
             edges whose artifact you cannot name.",
            order.join(" -> ")
        ),
    ))
}

fn is_placeholder_verify(value: &str) -> bool {
    let normalized = value.trim().to_lowercase();
    normalized.is_empty()
        || normalized.starts_with('#')
        || normalized.contains("fill in")
        || matches!(
            normalized.as_str(),
            "none" | "null" | "todo" | "tbd" | "replace me"
        )
}

/// Strip leading `!`, env assignments, wrappers (and their flags / a leading
/// duration for `timeout`/`gtimeout`), then judge the first real token.
fn runs_a_command(tokens: &mut Vec<String>) -> bool {
    while !tokens.is_empty() && (tokens[0] == "!" || env_assignment_re().is_match(&tokens[0])) {
        tokens.remove(0);
    }
    while !tokens.is_empty() && COMMAND_WRAPPERS.contains(&tokens[0].as_str()) {
        let wrapper = tokens.remove(0);
        while !tokens.is_empty()
            && (tokens[0].starts_with('-') || env_assignment_re().is_match(&tokens[0]))
        {
            tokens.remove(0);
        }
        // `timeout`/`gtimeout` take a positional duration before the command,
        // and `-k` takes one of its own, so drop the whole leading run.
        while !tokens.is_empty()
            && wrapper.ends_with("timeout")
            && duration_re().is_match(&tokens[0])
        {
            tokens.remove(0);
        }
    }
    let Some(command) = tokens.first() else {
        return false;
    };
    let command = command.clone();
    let command_lower = command.to_lowercase();
    let Some(first_char) = command.chars().next() else {
        return false;
    };
    if first_char.is_uppercase() {
        return false;
    }
    if AMBIGUOUS_COMMANDS.contains(&command_lower.as_str())
        && tokens.len() > 1
        && PROSE_FOLLOWERS.contains(&tokens[1].to_lowercase().as_str())
    {
        return false;
    }
    if PROSE_ONLY_COMMANDS.contains(&command_lower.as_str()) || command.starts_with('-') {
        return false;
    }
    let mut command_for_reject = command.clone();
    if command.contains('$') {
        if !command.contains('/') {
            return false;
        }
        command_for_reject = shell_parameter_re().replace_all(&command, "").to_string();
        if command_for_reject.contains('$') {
            return false;
        }
    }
    if file_reject_re().is_match(&command_for_reject) {
        return false;
    }
    // A document-shaped first token is prose that happens to lead with a path;
    // other bare tokens remain valid without an ecosystem-specific inventory.
    !NON_EXECUTABLE_SUFFIXES
        .iter()
        .any(|suffix| command_lower.ends_with(suffix))
}

fn is_runnable_verify(value: &str) -> bool {
    if is_placeholder_verify(value) {
        return false;
    }
    // Whole-value parse first: a well-formed segment must not rescue a value
    // bash itself would reject, such as `pytest tests/ && echo "unclosed`.
    let whole = match shlex::split(value) {
        Some(t) => t,
        None => return false,
    };
    for segment in shell_segment_re().split(value) {
        let Some(tokens) = shlex::split(segment) else {
            continue;
        };
        if runs_a_command(&mut tokens.clone()) {
            return true;
        }
    }
    // Segment splitting is quoting-blind, so `rg -n 'a|b' src/` shreds into
    // two unparseable halves. The whole value already parsed; judge that.
    runs_a_command(&mut whole.clone())
}

fn normalize_refs(value: Option<&Value>) -> Option<Vec<String>> {
    let arr = value?.as_array()?;
    Some(
        arr.iter()
            .map(|v| python_str(v).trim().to_string())
            .collect(),
    )
}

fn has_quick_placeholder(value: &str) -> bool {
    let lowered = value.trim().to_lowercase();
    QUICK_PLACEHOLDERS.iter().any(|m| lowered.contains(m))
        || matches!(
            lowered.trim_matches(|c| "[] :.".contains(c)),
            "todo" | "tbd"
        )
}

/// Quick-plan-only lexical path predicate. Never touches the filesystem: a
/// quick plan may name a file it is about to create, so validity must not
/// depend on checkout state.
fn is_concrete_file(value: &str) -> bool {
    let token = value.trim().trim_matches('`').trim();
    if token.is_empty() || has_quick_placeholder(token) {
        return false;
    }
    // Matched whole, never as a substring: `src/en/api.ts` contains "n/a".
    if matches!(token.to_lowercase().as_str(), "n/a" | "na" | "none" | "nil") {
        return false;
    }
    if file_reject_re().is_match(token) || token.contains("://") || token.starts_with('-') {
        return false;
    }
    // Directory syntax names no file. `cli/src` stays accepted because an
    // extensionless path is a legal file (`bin/mycli`) and nothing lexical
    // separates the two; only unambiguous directory shapes are refused.
    let last = token.rsplit('/').next().unwrap_or("");
    if token.ends_with('/') || matches!(last, "" | "." | "..") {
        return false;
    }
    if token.contains('/') {
        return true;
    }
    CONVENTIONAL_FILENAMES.contains(&token.to_lowercase().as_str())
        || (token.starts_with('.') && token.len() > 1)
        || file_extension_re().is_match(token)
}

fn quick_file_candidates(body: &str) -> Vec<String> {
    let mut candidates: Vec<String> = backtick_re()
        .captures_iter(body)
        .map(|c| c[1].to_string())
        .collect();
    for raw_line in body.lines() {
        let line = raw_line.trim();
        if line.is_empty() || table_rule_re().is_match(line) {
            continue;
        }
        if line.starts_with('|') {
            let cells: Vec<String> = line
                .trim_matches('|')
                .split('|')
                .map(|c| c.trim().trim_matches('`').to_string())
                .collect();
            if let Some(first) = cells.first() {
                if !first.to_lowercase().eq("file") {
                    candidates.push(first.clone());
                }
            }
            continue;
        }
        if let Some(c) = bullet_re().captures(line) {
            candidates.push(c[1].trim_matches('`').to_string());
        }
    }
    candidates
}

fn quick_verification_candidates(body: &str) -> Vec<String> {
    let mut candidates: Vec<String> = backtick_re()
        .captures_iter(body)
        .map(|c| c[1].to_string())
        .collect();
    for raw_line in body.lines() {
        let line = list_prefix_re().replace(raw_line, "").trim().to_string();
        if !line.is_empty() && !line.starts_with("```") && !line.starts_with('|') {
            candidates.push(line);
        }
    }
    candidates
}

fn validate_quick(doc: &PlanDocument) -> Vec<ExecutionViolation> {
    let mut violations = Vec::new();
    for section in QUICK_REQUIRED_SECTIONS {
        if doc
            .get_section(section)
            .map(|s| s.trim().is_empty())
            .unwrap_or(true)
        {
            violations.push(violation(
                section,
                format!("missing or empty ## {section} section"),
            ));
        }
    }
    for section in ["Changes", "Files to Modify", "Verification"] {
        let body = doc.get_section(section).unwrap_or("").trim().to_string();
        if !body.is_empty() && has_quick_placeholder(&body.to_lowercase()) {
            violations.push(violation(
                section,
                format!("## {section} contains template placeholder content"),
            ));
        }
    }

    let files_body = doc
        .get_section("Files to Modify")
        .unwrap_or("")
        .trim()
        .to_string();
    let file_tokens = quick_file_candidates(&files_body);
    if !files_body.is_empty() && !file_tokens.iter().any(|t| is_concrete_file(t)) {
        violations.push(violation(
            "Files to Modify",
            "must name at least one concrete file path",
        ));
    }

    let verification = doc
        .get_section("Verification")
        .unwrap_or("")
        .trim()
        .to_string();
    let commands = quick_verification_candidates(&verification);
    if !verification.is_empty() && !commands.iter().any(|c| is_runnable_verify(c)) {
        violations.push(violation(
            "Verification",
            "must contain at least one concrete runnable command",
        ));
    }
    violations
}

/// compiled-v1 reference resolution: every task acceptance ref resolves.
///
/// Compiles the Acceptance Criteria section through the canonical compiler and
/// refuses on a malformed/duplicate section or on any task reference that maps
/// to no compiled criterion. The diagnostic names the task and the unresolved
/// code so an author can fix the reference without decoding the whole plan.
fn validate_compiled_acceptance(doc: &PlanDocument, tasks: &[Value]) -> Vec<ExecutionViolation> {
    let mut out = Vec::new();
    let ac_section = doc.get_section("Acceptance Criteria").unwrap_or("");
    let criteria = match compile_criteria(ac_section, "Acceptance Criteria") {
        Ok(c) => c,
        Err(CriteriaParseError(msg)) => {
            out.push(violation("Acceptance Criteria", msg));
            return out;
        }
    };
    let codes: HashSet<&str> = criteria.iter().map(|c| c.code.as_str()).collect();
    for task in tasks {
        let task_id = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let task_id = if task_id.is_empty() {
            "?".to_string()
        } else {
            task_id
        };
        let Some(refs) = task.get("acceptance").and_then(Value::as_array) else {
            continue;
        };
        for r in refs {
            let ref_str = python_str(r).trim().to_string();
            if !ref_str.is_empty() && !codes.contains(ref_str.as_str()) {
                out.push(violation(
                    &format!("tasks.{task_id}.acceptance"),
                    format!("references '{ref_str}', which resolves to no compiled criterion"),
                ));
            }
        }
    }
    let evidence = validate_acceptance_evidence(doc, &codes);
    out.extend(evidence);
    out
}

/// Validate `acceptance_evidence` bindings against compiled criteria.
///
/// Unknown AC ids and malformed or out-of-range probe references refuse by
/// name. `required` must arm the session terminal: a required plan with no
/// done_probes binding is a session gate that could never pass. A criterion
/// with no binding stays unmeasured - an unarmed label, never an invented
/// execution.
fn validate_acceptance_evidence(
    doc: &PlanDocument,
    codes: &HashSet<&str>,
) -> Vec<ExecutionViolation> {
    let mut out = Vec::new();
    let fm = doc.frontmatter.as_object();
    let Some(decl) = fm.and_then(|m| m.get("acceptance_evidence")) else {
        return out;
    };
    let Some(decl) = decl.as_object() else {
        out.push(violation("acceptance_evidence", "must be a mapping"));
        return out;
    };
    let required = decl
        .get("required")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(bindings) = decl.get("bindings").and_then(Value::as_object) else {
        out.push(violation(
            "acceptance_evidence",
            "`bindings` must be a map of AC id to probe reference",
        ));
        return out;
    };
    let mut probe_lists: HashMap<&str, Vec<String>> = HashMap::new();
    for key in ["done_probes", "close_probes"] {
        let raw = fm.and_then(|m| m.get(key));
        let items: Vec<String> = match raw {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|p| python_str(p))
                .filter(|p| !p.trim().is_empty())
                .collect(),
            _ => Vec::new(),
        };
        probe_lists.insert(key, items);
    }
    let mut binds_done = false;
    for (ac, probe_ref) in bindings {
        let ac_str = ac.trim();
        let ref_str = probe_ref.as_str().unwrap_or("").trim();
        if !codes.contains(ac_str) {
            out.push(violation(
                "acceptance_evidence",
                format!("binds '{ac_str}', which resolves to no compiled criterion"),
            ));
            continue;
        }
        let m = match evidence_ref_re().captures(ref_str) {
            Some(m) => m,
            None => {
                out.push(violation(
                    "acceptance_evidence",
                    format!("binds '{ac_str}' to '{ref_str}': expected `done_probes[n]` or `close_probes[n]`"),
                ));
                continue;
            }
        };
        let key = m.get(1).unwrap().as_str();
        let index: usize = m[2].parse().unwrap_or(usize::MAX);
        let declared = probe_lists.get(key).map(Vec::len).unwrap_or(0);
        if index >= declared {
            out.push(violation(
                "acceptance_evidence",
                format!(
                    "binds '{ac_str}' to {ref_str}, but {key} declares {declared} probe(s): the bound probe is missing"
                ),
            ));
            continue;
        }
        if key == "done_probes" {
            binds_done = true;
        }
    }
    if required && !binds_done {
        out.push(violation(
            "acceptance_evidence",
            "asserts required evidence but has no done_probes binding: the session terminal could never pass",
        ));
    }
    out
}

/// Validate the representation consumed by workers and wave scheduling.
///
/// `acceptance_contract` overrides the frontmatter value so a caller can
/// validate a PROPOSED state without writing it first (`plan-cli mutate --finalize`
/// passes `compiled-v1` to validate a design->ready promotion before it
/// lands). When None the contract is read from the frontmatter. Only a
/// `compiled-v1` plan gets strict acceptance-reference resolution; an
/// unstamped historical plan keeps the permissive legacy read (AC3-COMPAT).
pub fn validate_execution(
    doc: &PlanDocument,
    acceptance_contract: Option<&str>,
) -> ExecutionValidationResult {
    let strategy_text = doc.get_section("Execution Strategy");
    let fm = doc.frontmatter.as_object().cloned().unwrap_or_default();
    let is_quick = fm.get("kind").and_then(Value::as_str) == Some("quick-plan");
    // One door read serves both date gates for this whole validation: the
    // waves gate (with the body fact passed in) and the per-wave band
    // requirement's created_after answer. AUTHORING lenience throughout, the
    // same posture the plan CLI applies one scope up: validate-plan.sh dates
    // undatable plans itself. A door failure reads as no gate error and no
    // band requirement, which is what an undatable plan read as before.
    // The door's gates op is the same gate the model validator ran; the
    // plan-level difficulty refusal is the CLI scope's own pre-check, not
    // validate_execution's, so this binding stays unread here.
    let _difficulty_gate = crate::plan_schema::difficulty_gate_error(&fm, false);
    let waves_gate = crate::plan_schema::waves_gate_error(&fm, strategy_text.is_some(), false);
    let created_after = crate::plan_schema::created_after_gate(&fm);
    // The quick BODY checks - required sections, template placeholders left in
    // place, a concrete file path, a runnable verification - are orthogonal to
    // the Execution Strategy and must run for EVERY quick plan.
    let mut quick_violations = Vec::new();
    if is_quick {
        quick_violations = validate_quick(doc);
    }

    let Some(strategy_text) = strategy_text else {
        if is_quick {
            // The one escape hatch, now date-keyed. `quick` was told to skip
            // the section on the assertion that a quick plan is single-task.
            // Pre-gate plans keep the hatch; backfilling their topology would
            // fabricate waves nobody authored. AUTHORING scope, so undatable
            // frontmatter DEFERS rather than refusing.
            if let Some(gate_error) = waves_gate {
                quick_violations.push(violation("Execution Strategy", gate_error));
                return ExecutionValidationResult {
                    violations: quick_violations,
                    warnings: Vec::new(),
                };
            }
            return ExecutionValidationResult {
                violations: quick_violations,
                warnings: Vec::new(),
            };
        }
        return ExecutionValidationResult {
            violations: vec![violation(
                "Execution Strategy",
                "missing ## Execution Strategy section",
            )],
            warnings: Vec::new(),
        };
    };

    let strategy = match parse_execution_strategy(strategy_text) {
        Ok(v) => v,
        Err(BriefParseError(msg)) => {
            quick_violations.push(violation("Execution Strategy", msg));
            return ExecutionValidationResult {
                violations: quick_violations,
                warnings: Vec::new(),
            };
        }
    };
    // Seeded, not started empty: a quick plan that now CARRIES a strategy
    // still owes its body checks, and this is the path it takes.
    let mut violations: Vec<ExecutionViolation> = quick_violations;
    let mode = strategy
        .get("execution_mode")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if !EXECUTION_MODES.contains(&mode.as_str()) {
        violations.push(violation(
            "execution_mode",
            format!("expected one of ['mixed', 'parallel', 'sequential'], got '{mode}'"),
        ));
    }

    let tasks: Vec<Value> = strategy
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if tasks.is_empty() {
        violations.push(violation(
            "tasks",
            "Execution Strategy must declare at least one task",
        ));
    }

    let task_ids: Vec<String> = tasks
        .iter()
        .map(|t| {
            t.get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .collect();
    let mut counts: HashMap<&String, usize> = HashMap::new();
    for id in &task_ids {
        *counts.entry(id).or_insert(0) += 1;
    }
    let mut duplicate_task_ids: Vec<String> = counts
        .iter()
        .filter(|(id, c)| **c > 1 && !id.is_empty())
        .map(|(id, _)| (*id).clone())
        .collect();
    duplicate_task_ids.sort();
    for task_id in &duplicate_task_ids {
        violations.push(violation(
            &format!("tasks.{task_id}.id"),
            format!("duplicate task id '{task_id}'"),
        ));
    }

    let mut tasks_by_id: HashMap<String, &Value> = HashMap::new();
    for (index, task) in tasks.iter().enumerate() {
        let task_id = task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let label = if task_id.is_empty() {
            (index + 1).to_string()
        } else {
            task_id.clone()
        };
        if task_id.is_empty() {
            violations.push(violation(
                &format!("tasks.{label}.id"),
                "task id is required",
            ));
        } else if !task_id_re().is_match(&task_id) {
            violations.push(violation(
                &format!("tasks.{label}.id"),
                "task id must match the durable state grammar (for example 1.1 or 02b)",
            ));
        } else {
            tasks_by_id.entry(task_id).or_insert(task);
        }

        if task
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .is_empty()
        {
            violations.push(violation(
                &format!("tasks.{label}.title"),
                "task title is required",
            ));
        }

        let surface_ok = task
            .get("surface")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|i| !python_str(i).trim().is_empty()))
            .unwrap_or(false);
        if !surface_ok {
            violations.push(violation(
                &format!("tasks.{label}.surface"),
                "task surface must name at least one file",
            ));
        }

        let verify = task.get("verify").map(python_str).unwrap_or_default();
        if !is_runnable_verify(&verify) {
            violations.push(violation(
                &format!("tasks.{label}.verify"),
                "task verify must be a concrete runnable command",
            ));
        }

        let acceptance_ok = task
            .get("acceptance")
            .and_then(Value::as_array)
            .map(|a| a.iter().any(|i| !python_str(i).trim().is_empty()))
            .unwrap_or(false);
        if !acceptance_ok {
            violations.push(violation(
                &format!("tasks.{label}.acceptance"),
                "task acceptance must contain at least one criterion",
            ));
        }
    }

    for edge_error in validate_task_edges(&strategy) {
        violations.push(violation("tasks.blocked_by", edge_error));
    }

    let mut warnings: Vec<ExecutionViolation> = Vec::new();
    if let Some(chain_warning) = total_chain_warning(&tasks) {
        warnings.push(chain_warning);
    }

    // compiled-v1: every task acceptance reference must resolve to exactly one
    // compiled criterion (AC5-ERR). Unstamped plans skip this (AC3-COMPAT).
    let contract = acceptance_contract.map(|c| c.to_string()).or_else(|| {
        fm.get("acceptance_contract")
            .map(python_str)
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_string())
    });
    if contract.as_deref() == Some("compiled-v1") {
        violations.extend(validate_compiled_acceptance(doc, &tasks));
    }

    let waves: Vec<Value> = strategy
        .get("waves")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if waves.is_empty() {
        violations.push(violation(
            "waves",
            "Execution Strategy must declare at least one wave",
        ));
        return ExecutionValidationResult {
            violations,
            warnings,
        };
    }

    // Wave ids the Python leg counts: positive-int `wave` fields only. serde
    // cannot produce a bool here, so the int guard needs no bool exclusion.
    let wave_ids: Vec<String> = waves
        .iter()
        .filter_map(|w| {
            let n = w.get("wave")?.as_i64()?;
            if n > 0 {
                Some(n.to_string())
            } else {
                None
            }
        })
        .collect();
    let mut wave_counts: HashMap<&String, usize> = HashMap::new();
    for id in &wave_ids {
        *wave_counts.entry(id).or_insert(0) += 1;
    }
    let mut duplicate_wave_ids: Vec<String> = wave_counts
        .iter()
        .filter(|(id, c)| **c > 1 && !id.is_empty())
        .map(|(id, _)| (*id).clone())
        .collect();
    duplicate_wave_ids.sort();
    for wave_id in &duplicate_wave_ids {
        violations.push(violation(
            &format!("waves.{wave_id}.wave"),
            format!("duplicate wave id '{wave_id}'"),
        ));
    }

    let declared_wave_ids: HashSet<&String> = wave_ids.iter().collect();
    let wave_positions: HashMap<&String, usize> =
        wave_ids.iter().enumerate().map(|(i, id)| (id, i)).collect();
    let mut referenced_tasks: Vec<String> = Vec::new();
    let mut dependency_graph: HashMap<String, HashSet<String>> = declared_wave_ids
        .iter()
        .map(|id| ((*id).clone(), HashSet::new()))
        .collect();

    for (index, wave) in waves.iter().enumerate() {
        let Some(wave) = wave.as_object() else {
            violations.push(violation(
                &format!("waves.{}", index + 1),
                "wave must be a mapping",
            ));
            continue;
        };
        let wave_value = wave.get("wave");
        let wave_id = wave_value
            .map(python_str)
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        let label = if wave_id.is_empty() {
            (index + 1).to_string()
        } else {
            wave_id.clone()
        };
        let wave_is_positive_int = wave_value
            .and_then(Value::as_i64)
            .map(|n| n > 0)
            .unwrap_or(false);
        if !wave_is_positive_int {
            violations.push(violation(
                &format!("waves.{label}.wave"),
                "wave id must be a positive integer",
            ));
        }

        let wave_mode = wave
            .get("mode")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        if !WAVE_MODES.contains(&wave_mode.as_str()) {
            violations.push(violation(
                &format!("waves.{label}.mode"),
                format!("expected one of ['parallel', 'sequential'], got '{wave_mode}'"),
            ));
        }

        // Blueprint is the PM: the wave's band is decided in the Execution
        // Strategy, and it is the ONLY routing axis a plan may carry (models
        // and routes stay in config). Post-gate plans must stamp every wave;
        // the frontmatter difficulty_gate_error covers the plan-level band.
        let wave_band = wave.get("difficulty");
        let band_blank = wave_band
            .and_then(Value::as_str)
            .map(|s| s.trim().is_empty())
            .unwrap_or(false);
        if wave_band.is_none() || band_blank {
            if created_after == Some(true) {
                violations.push(violation(
                    &format!("waves.{label}.difficulty"),
                    "wave difficulty is required for plans created after the difficulty gate; \
                     set it to one of low, medium, high",
                ));
            }
        } else {
            let band_str = wave_band.and_then(Value::as_str);
            match band_str {
                Some(b) if WAVE_DIFFICULTIES.contains(&b.trim().to_lowercase().as_str()) => {}
                _ => {
                    violations.push(violation(
                        &format!("waves.{label}.difficulty"),
                        format!(
                            "must be one of low, medium, high, got {}",
                            python_repr(wave_band.unwrap())
                        ),
                    ));
                }
            }
        }

        let mut refs = normalize_refs(wave.get("tasks")).unwrap_or_default();
        let refs_ok = wave.get("tasks").and_then(Value::as_array).is_some()
            && refs.iter().any(|r| !r.is_empty());
        if !refs_ok {
            violations.push(violation(
                &format!("waves.{label}.tasks"),
                "wave must reference at least one task",
            ));
            refs.clear();
        }
        referenced_tasks.extend(refs.clone());
        for r in &refs {
            if !tasks_by_id.contains_key(r) {
                violations.push(violation(
                    &format!("waves.{label}.tasks"),
                    format!("wave references unknown task '{r}'"),
                ));
            }
        }

        let dependencies = wave.get("depends_on");
        // `dependencies not in (None, "")`: a null or empty-string field is
        // "no dependencies declared", anything else is a declaration.
        let deps_declared = !matches!(dependencies, None | Some(Value::Null))
            && dependencies.and_then(Value::as_str) != Some("");
        if deps_declared {
            let list: Vec<Value> = match dependencies {
                Some(Value::Array(a)) => a.clone(),
                Some(other) => vec![other.clone()],
                None => Vec::new(),
            };
            for dependency in &list {
                let dependency_id = python_str(dependency).trim().to_string();
                let declared = declared_wave_ids.contains(&dependency_id);
                if !declared {
                    violations.push(violation(
                        &format!("waves.{label}.depends_on"),
                        format!("wave references unknown dependency '{dependency_id}'"),
                    ));
                } else if let Some(pos) = wave_positions.get(&wave_id) {
                    if let Some(dep_pos) = wave_positions.get(&dependency_id) {
                        dependency_graph
                            .entry(wave_id.clone())
                            .or_default()
                            .insert(dependency_id.clone());
                        if dep_pos >= pos {
                            violations.push(violation(
                                &format!("waves.{label}.depends_on"),
                                format!(
                                    "dependency '{dependency_id}' must reference an earlier declared wave"
                                ),
                            ));
                        }
                    }
                }
            }
        }
    }

    // Multi-reference and unreferenced-task checks, sorted like the Counter
    // iteration they replace.
    let mut ref_counts: HashMap<&String, usize> = HashMap::new();
    for r in &referenced_tasks {
        *ref_counts.entry(r).or_insert(0) += 1;
    }
    let mut multi: Vec<(&String, usize)> = ref_counts.into_iter().collect();
    multi.sort();
    for (task_id, count) in multi {
        if tasks_by_id.contains_key(task_id) && count > 1 {
            violations.push(violation(
                &format!("tasks.{task_id}"),
                format!("task is referenced {count} times"),
            ));
        }
    }
    let referenced_set: HashSet<&String> = referenced_tasks.iter().collect();
    let mut unreferenced: Vec<&String> = tasks_by_id
        .keys()
        .filter(|id| !referenced_set.contains(*id))
        .collect();
    unreferenced.sort();
    unreferenced.dedup();
    for task_id in unreferenced {
        violations.push(violation(
            &format!("tasks.{task_id}"),
            "task is not referenced by any wave",
        ));
    }

    // DFS cycle detection over the wave dependency graph, visiting/visited
    // sets as the Python closure kept them.
    let mut visiting: HashSet<String> = HashSet::new();
    let mut visited: HashSet<String> = HashSet::new();
    fn has_cycle(
        wave_id: &str,
        graph: &HashMap<String, HashSet<String>>,
        visiting: &mut HashSet<String>,
        visited: &mut HashSet<String>,
    ) -> bool {
        if visiting.contains(wave_id) {
            return true;
        }
        if visited.contains(wave_id) {
            return false;
        }
        visiting.insert(wave_id.to_string());
        if let Some(deps) = graph.get(wave_id) {
            for dep in deps {
                if has_cycle(dep, graph, visiting, visited) {
                    return true;
                }
            }
        }
        visiting.remove(wave_id);
        visited.insert(wave_id.to_string());
        false
    }
    let mut roots: Vec<&String> = dependency_graph.keys().collect();
    roots.sort();
    if roots
        .iter()
        .any(|id| has_cycle(id, &dependency_graph, &mut visiting, &mut visited))
    {
        violations.push(violation(
            "waves.depends_on",
            "wave dependency cycle detected",
        ));
    }

    ExecutionValidationResult {
        violations,
        warnings,
    }
}

/// The `plan_docs validate` op: load the doc, run one scope, return the JSON
/// report the CLI prints. `json` shapes the payload the caller renders.
///
/// A proposed `acceptance_contract` validates the body alone: /blueprint's
/// finalize stamps the frontmatter it proposes, difficulty band included,
/// only after the body passes, so the frontmatter gate would refuse a doc
/// the same call is about to fix.
pub fn op_validate_execution(
    plan_path: &std::path::Path,
    json_out: bool,
    acceptance_contract: Option<&str>,
) -> Result<Value, String> {
    let doc = match super::doc::load_plan(plan_path) {
        Ok(d) => d,
        Err(e) => return Err(format!("cannot load plan {}: {e}", plan_path.display())),
    };
    // Frontmatter outranks body, so the plan-level difficulty gate fires
    // first (the creation-time half of the difficulty gate, round-5). The
    // creation-time half of the difficulty gate: validate-plan.sh dates
    // undatable plans itself, so an unreadable created defers here rather
    // than refusing; the minting lanes keep the strict default.
    let fm = doc.frontmatter.as_object().cloned().unwrap_or_default();
    let gate = match acceptance_contract {
        Some(_) => None,
        None => crate::plan_schema::difficulty_gate_error(&fm, false),
    };
    if let Some(gate_error) = gate {
        let mut report = json!({
            "valid": false,
            "path": plan_path.display().to_string(),
            "scope": "execution",
            "violations": [{"field": "difficulty", "message": gate_error}],
            "warnings": [],
        });
        if !json_out {
            report["text"] = Value::String(format!(
                "invalid execution: {} (difficulty gate)\n  difficulty: {gate_error}",
                plan_path.display()
            ));
        }
        return Ok(report);
    }
    let result = validate_execution(&doc, acceptance_contract);
    let violations: Vec<Value> = result
        .violations
        .iter()
        .map(|v| json!({"field": v.field, "message": v.message}))
        .collect();
    let warnings: Vec<Value> = result
        .warnings
        .iter()
        .map(|w| json!({"field": w.field, "message": w.message}))
        .collect();
    let valid = result.violations.is_empty();
    let mut report = json!({
        "valid": valid,
        "path": plan_path.display().to_string(),
        "scope": "execution",
        "violations": violations,
        "warnings": warnings,
    });
    if !json_out {
        // The text rendering rides along; the caller prints it verbatim.
        report["text"] = Value::String(render_text(&report));
    }
    Ok(report)
}

fn render_text(report: &Value) -> String {
    let mut lines = Vec::new();
    if report["valid"].as_bool() == Some(true) {
        for w in report["warnings"].as_array().unwrap_or(&Vec::new()) {
            lines.push(format!(
                "  warning: {}: {}",
                w["field"].as_str().unwrap_or(""),
                w["message"].as_str().unwrap_or("")
            ));
        }
        lines.push(format!(
            "valid execution: {}",
            report["path"].as_str().unwrap_or("")
        ));
    } else {
        let violations = report["violations"].as_array().cloned().unwrap_or_default();
        lines.push(format!(
            "invalid execution: {} ({} violation(s))",
            report["path"].as_str().unwrap_or(""),
            violations.len()
        ));
        for v in &violations {
            lines.push(format!(
                "  {}: {}",
                v["field"].as_str().unwrap_or(""),
                v["message"].as_str().unwrap_or("")
            ));
        }
        for w in report["warnings"].as_array().unwrap_or(&Vec::new()) {
            lines.push(format!(
                "  warning: {}: {}",
                w["field"].as_str().unwrap_or(""),
                w["message"].as_str().unwrap_or("")
            ));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
#[path = "exec_validate_tests.rs"]
mod tests;
