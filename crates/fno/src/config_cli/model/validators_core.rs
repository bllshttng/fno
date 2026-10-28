//! The validators `fno/config/__init__.py` declares from `PathsBlock`
//! through `ReviewBlock`, ported one function per decorator.

use super::{py_repr, py_str, Ctx, Fail, Registry};
use serde_json::{Map, Value};
use std::collections::HashMap;

const PATH_MAX: usize = 4096;

const PATHS_FIELDS: [&str; 17] = [
    "ledger_json",
    "evals_history",
    "briefs_dir",
    "fleet_dir",
    "postmortems_dir",
    "worktrees_base",
    "cargo_targets_base",
    "memory_dir",
    "hook_logs_dir",
    "inbox_path",
    "handoffs_dir",
    "retro_pending_dir",
    "bus_dir",
    "chats",
    "observer_reports_dir",
    "operator_lane",
    "spaces_dir",
];

const RESERVED_PREFIXES: [&str; 3] = ["cv-", "fu-", "tgt-"];

/// `REVIEW_POSTURES` keys, sorted.
const POSTURES: [&str; 9] = [
    "github_review",
    "independent_review",
    "no_review",
    "peer_review",
    "self_and_github",
    "self_and_peer",
    "self_github_and_peer",
    "self_review",
    "tests_pass",
];

/// `_RESOLVABLE_REVIEWERS` keys.
const BUILTIN_REVIEWERS: [&str; 3] = ["sigma", "code-review", "declare"];

const EXTERNAL_REVIEWER_VOCABULARY: [&str; 4] = ["gemini", "codex", "coderabbit", "claude"];

const RENDER_PROJECTIONS: [&str; 3] = ["backlog", "roadmap", "local"];

const DEFAULT_POST_MERGE_MODEL: &str = "claude-opus-5";

pub(super) fn register(reg: &mut Registry) {
    reg.before("PathsBlock", &PATHS_FIELDS, paths_reject_glob_chars);
    reg.before("ObsidianBlock", &["vault"], obsidian_reject_glob_chars);
    reg.model_after("ObsidianBlock", vault_required_when_enabled);
    reg.model_before("ProjectBlock", coerce_string_shorthand);
    reg.before("ProjectBlock", &["id"], validate_project_id);
    reg.after(
        "BlueprintBlock",
        &["max_prs_per_epic"],
        max_prs_per_epic_positive,
    );
    reg.after(
        "MaintainBlock",
        &[
            "staleness_days",
            "max_failed_attempts",
            "abandoned_do_row_hours",
            "budget_seconds",
        ],
        maintain_positive_counts,
    );
    reg.model_before("RenderTargetConfig", warn_unknown_render_keys);
    reg.after("RenderTargetConfig", &["path"], plain_path);
    reg.after("RenderTargetConfig", &["project"], nonempty_project);
    reg.after("RenderTargetConfig", &["scope"], nonempty_scope);
    reg.model_after("RenderTargetConfig", normalize_scope);
    reg.after("RenderTargetConfig", &["projection"], known_projection);
    reg.before("BacklogBlock", &["render_targets"], coerce_render_targets);
    reg.after("BacklogBlock", &["staleness_days"], staleness_days_positive);
    reg.after("BacklogBlock", &["id_prefix"], validate_id_prefix);
    reg.after("BacklogBlock", &["id_hex_width"], validate_id_hex_width);
    reg.before(
        "PostMergeBlock",
        &["parking_lot_path"],
        validate_parking_lot_path,
    );
    reg.before(
        "PostMergeBlock",
        &["maintainer_marker"],
        validate_maintainer_marker,
    );
    reg.before("PostMergeBlock", &["self_reap"], coerce_affirmative);
    reg.before("PostMergeBlock", &["auto_run"], coerce_affirmative);
    reg.before("PostMergeBlock", &["model"], coerce_post_merge_model);
    reg.before("ResearchBlock", &["output_dir"], validate_output_dir);
    reg.before("CrossModelBlock", &["enabled"], coerce_affirmative);
    reg.after("WordCapBlock", &["mail", "encounter"], cap_is_positive);
    reg.before("ReviewBlock", &["posture"], validate_posture);
    reg.before(
        "ReviewBlock",
        &["github_apps", "required_bots"],
        coerce_malformed_to_default,
    );
    reg.before("ReviewBlock", &["optional_apps"], coerce_optional_apps);
    reg.before("ReviewBlock", &["reviewers"], coerce_and_resolve_reviewers);
    reg.before("ReviewBlock", &["peers"], coerce_peers);
    reg.model_after("ReviewBlock", resolve_github_apps_alias);
    reg.before(
        "ReviewBlock",
        &["external_reviewers"],
        coerce_external_reviewers,
    );
    reg.before(
        "ReviewBlock",
        &["agent_harnesses", "agent_providers"],
        coerce_agent_providers,
    );
    reg.model_after("ReviewBlock", resolve_agent_harnesses_alias);
    reg.before("ReviewBlock", &["agent_routes"], validate_agent_routes);
    reg.before("ReviewBlock", &["cross_model"], coerce_cross_model);
}

// ---------------------------------------------------------------------------
// Python builtins the validators lean on
// ---------------------------------------------------------------------------

/// `str.isspace` per char: Unicode White_Space plus the C0 separators
/// U+001C..U+001F, which Python counts and Rust does not.
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn py_strip(text: &str) -> &str {
    text.trim_matches(py_isspace)
}

fn repr_text(text: &str) -> String {
    py_repr(&Value::String(text.to_string()))
}

fn repr_list<S: AsRef<str>>(items: &[S]) -> String {
    py_repr(&Value::Array(
        items
            .iter()
            .map(|s| Value::String(s.as_ref().to_string()))
            .collect(),
    ))
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "float",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn below_one(value: &Value) -> bool {
    value.as_i64().is_some_and(|n| n < 1)
}

fn check_no_glob(value: &str, field_name: &str) -> Result<(), Fail> {
    let found: Vec<String> = ['*', '?', '[']
        .iter()
        .filter(|c| value.contains(**c))
        .map(|c| repr_text(&c.to_string()))
        .collect();
    if found.is_empty() {
        return Ok(());
    }
    Err(Fail::Value(format!(
        "{} contains glob character(s) {}; glob characters are not allowed in path values",
        repr_text(field_name),
        found.join(", ")
    )))
}

fn check_path_max(value: &str, field_name: &str) -> Result<(), Fail> {
    if value.len() > PATH_MAX {
        return Err(Fail::Value(format!(
            "{} exceeds PATH_MAX ({PATH_MAX} bytes): {} bytes",
            repr_text(field_name),
            value.len()
        )));
    }
    Ok(())
}

/// `os.path.isabs(os.path.expanduser(path))`.
fn is_abs_after_expanduser(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let Some(tail) = path.strip_prefix('~') else {
        return false;
    };
    let cut = tail.find('/').unwrap_or(tail.len());
    let (name, rest) = tail.split_at(cut);
    if !name.is_empty() {
        return user_exists(name);
    }
    match std::env::var("HOME") {
        Ok(home) => {
            let joined = format!("{}{rest}", home.trim_end_matches('/'));
            joined.is_empty() || joined.starts_with('/')
        }
        Err(_) => true,
    }
}

fn user_exists(name: &str) -> bool {
    let Ok(name) = std::ffi::CString::new(name) else {
        return false;
    };
    // SAFETY: `name` is NUL-terminated and outlives the call; the returned
    // record is only tested for null, never read.
    unsafe { !libc::getpwnam(name.as_ptr()).is_null() }
}

// ---------------------------------------------------------------------------
// Paths, obsidian, project, blueprint, maintain
// ---------------------------------------------------------------------------

fn paths_reject_glob_chars(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        check_no_glob(s, "paths override")?;
    }
    Ok(v)
}

fn obsidian_reject_glob_chars(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        check_no_glob(s, "obsidian.vault")?;
    }
    Ok(v)
}

fn vault_required_when_enabled(_ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let enabled = m.get("enabled").is_some_and(truthy);
    let vault = m.get("vault").is_some_and(truthy);
    if enabled && !vault {
        return Err(Fail::Value(
            "obsidian.enabled is true but obsidian.vault is not set".into(),
        ));
    }
    Ok(())
}

fn coerce_string_shorthand(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::String(id) => {
            let mut map = Map::new();
            map.insert("id".into(), Value::String(id));
            Ok(Value::Object(map))
        }
        other => Ok(other),
    }
}

fn validate_project_id(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let bad = matches!(&v, Value::String(s)
        if !s.is_empty() && !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
    if bad {
        return Err(Fail::Value(format!(
            "project.id {} contains invalid characters; only [A-Za-z0-9._-] are allowed",
            py_repr(&v)
        )));
    }
    Ok(v)
}

fn max_prs_per_epic_positive(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if below_one(&v) {
        return Err(Fail::Value(
            "config.blueprint.max_prs_per_epic must be >= 1".into(),
        ));
    }
    Ok(v)
}

fn maintain_positive_counts(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if below_one(&v) {
        return Err(Fail::Value(format!(
            "config.backlog.maintain.{} must be >= 1",
            ctx.field_name
        )));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Render targets and the backlog block
// ---------------------------------------------------------------------------

fn warn_unknown_render_keys(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::Object(map) = &v {
        let path = map.get("path").cloned().unwrap_or(Value::Null);
        for key in map.keys() {
            if !matches!(key.as_str(), "path" | "project" | "scope" | "projection") {
                ctx.note(format!(
                    "config.backlog.render_targets: unknown key {} in row {} - the row will not render (allowed: path, project, projection, scope)",
                    repr_text(key),
                    py_repr(&path)
                ));
            }
        }
    }
    Ok(v)
}

fn plain_path(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::String(raw) = &v else { return Ok(v) };
    let path = py_strip(raw).to_string();
    if path.is_empty() {
        return Err(Fail::Value(
            "backlog.render_targets path must not be empty".into(),
        ));
    }
    check_no_glob(&path, "backlog.render_targets path")?;
    check_path_max(&path, "backlog.render_targets path")?;
    if !is_abs_after_expanduser(&path) {
        return Err(Fail::Value(format!(
            "backlog.render_targets path must be absolute or ~/-rooted (it is written from arbitrary cwds), got: {}",
            repr_text(&path)
        )));
    }
    Ok(Value::String(path))
}

fn nonempty_project(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::String(raw) = &v else { return Ok(v) };
    let project = py_strip(raw);
    if project.is_empty() {
        return Err(Fail::Value(
            "backlog.render_targets project must not be empty".into(),
        ));
    }
    Ok(Value::String(project.to_string()))
}

fn nonempty_scope(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::String(raw) = &v else { return Ok(v) };
    let scope = py_strip(raw);
    if scope.is_empty() {
        return Err(Fail::Value(
            "backlog.render_targets scope must not be empty".into(),
        ));
    }
    if scope == "*" {
        return Ok(Value::String("all".into()));
    }
    Ok(Value::String(scope.to_string()))
}

fn normalize_scope(_ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let scope = m.get("scope").cloned().unwrap_or(Value::Null);
    let project = m.get("project").cloned().unwrap_or(Value::Null);
    if !scope.is_null() && !project.is_null() && scope != project {
        return Err(Fail::Value(
            "backlog.render_targets scope and project must name the same value".into(),
        ));
    }
    if !scope.is_null() {
        m.insert("project".into(), scope);
    } else if project.is_null() {
        m.insert("scope".into(), Value::String("all".into()));
        m.insert("project".into(), Value::String("all".into()));
    }
    Ok(())
}

fn known_projection(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let known = matches!(&v, Value::String(s) if RENDER_PROJECTIONS.contains(&s.as_str()));
    if !known {
        return Err(Fail::Value(format!(
            "unknown backlog.render_targets projection {} (allowed: {})",
            py_repr(&v),
            RENDER_PROJECTIONS.join(", ")
        )));
    }
    Ok(v)
}

fn coerce_render_targets(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Array(items) => Ok(Value::Array(items)),
        Value::Null => Ok(Value::Array(Vec::new())),
        other => {
            ctx.note(format!(
                "config.backlog.render_targets is {}, not an array of tables - ignoring it (use [[backlog.render_targets]], not [backlog.render_targets.<name>])",
                type_name(&other)
            ));
            Ok(Value::Array(Vec::new()))
        }
    }
}

fn staleness_days_positive(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if below_one(&v) {
        return Err(Fail::Value(
            "config.backlog.staleness_days must be >= 1".into(),
        ));
    }
    Ok(v)
}

/// `[a-z][a-z0-9]{0,6}-?`, matched whole.
fn id_prefix_shape(raw: &str) -> bool {
    let body = raw.strip_suffix('-').unwrap_or(raw);
    let mut chars = body.chars();
    matches!(chars.next(), Some('a'..='z'))
        && body.chars().count() <= 7
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn validate_id_prefix(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::String(given) = &v else {
        return Ok(v);
    };
    let raw = py_strip(given).to_lowercase();
    if !id_prefix_shape(&raw) {
        return Err(Fail::Value(format!(
            "config.backlog.id_prefix must be a lowercase token of 1-7 letters/digits (letter-led) with an optional trailing '-', got: {}",
            py_repr(&v)
        )));
    }
    let normalized = if raw.ends_with('-') {
        raw
    } else {
        format!("{raw}-")
    };
    if RESERVED_PREFIXES.contains(&normalized.as_str()) {
        return Err(Fail::Value(format!(
            "config.backlog.id_prefix must not collide with a reserved family {}, got: {}",
            repr_list(&RESERVED_PREFIXES),
            py_repr(&v)
        )));
    }
    Ok(Value::String(normalized))
}

fn validate_id_hex_width(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if !v.as_i64().is_some_and(|n| (4..=8).contains(&n)) {
        return Err(Fail::Value(format!(
            "config.backlog.id_hex_width must be an integer in [4, 8], got: {}",
            py_repr(&v)
        )));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Post-merge, research, cross-model, style
// ---------------------------------------------------------------------------

fn validate_parking_lot_path(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        check_no_glob(s, "config.post_merge.parking_lot_path")?;
        check_path_max(s, "config.post_merge.parking_lot_path")?;
        if s.starts_with('/') || s.starts_with('~') {
            return Err(Fail::Value(format!(
                "config.post_merge.parking_lot_path must be repo-relative (no leading '/' or '~'); got: {}",
                py_repr(&v)
            )));
        }
        if s.split('/').any(|segment| segment == "..") {
            return Err(Fail::Value(format!(
                "config.post_merge.parking_lot_path must not contain a '..' path segment (no repo escape); got: {}",
                py_repr(&v)
            )));
        }
    }
    Ok(v)
}

fn validate_maintainer_marker(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let Value::String(raw) = &v else {
        return Ok(Value::Null);
    };
    let cleaned = py_strip(raw);
    if cleaned.is_empty() {
        return Ok(Value::Null);
    }
    if cleaned.chars().any(py_isspace) {
        return Err(Fail::Value(format!(
            "config.post_merge.maintainer_marker must be a single whitespace-free token (e.g. '#maintainer'); got: {}",
            py_repr(&v)
        )));
    }
    Ok(Value::String(cleaned.to_string()))
}

/// Fail-safe to false: only a bool, the int 1, or an affirmative word enables.
fn coerce_affirmative(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let on = match &v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_i64() == Some(1),
        Value::String(s) => matches!(
            py_strip(s).to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    };
    Ok(Value::Bool(on))
}

fn coerce_post_merge_model(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match &v {
        Value::String(s) if !py_strip(s).is_empty() => Ok(Value::String(py_strip(s).to_string())),
        _ => Ok(Value::String(DEFAULT_POST_MERGE_MODEL.into())),
    }
}

fn validate_output_dir(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if let Value::String(s) = &v {
        check_no_glob(s, "config.research.output_dir")?;
        check_path_max(s, "config.research.output_dir")?;
    }
    Ok(v)
}

fn cap_is_positive(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if below_one(&v) {
        return Err(Fail::Value(
            "config.style.word_cap.<surface> must be >= 1".into(),
        ));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Review
// ---------------------------------------------------------------------------

fn validate_posture(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match &v {
        Value::Null => Ok(Value::Null),
        Value::String(s) => {
            let stripped = py_strip(s);
            if POSTURES.contains(&stripped) {
                return Ok(Value::String(stripped.to_string()));
            }
            Err(Fail::Value(format!(
                "review.posture {} is not one of {}",
                py_repr(&v),
                repr_list(&POSTURES)
            )))
        }
        _ => Err(Fail::Value(format!(
            "review.posture must be a posture name or be unset, got {}",
            py_repr(&v)
        ))),
    }
}

fn scalar_login(v: &Value) -> Option<Value> {
    matches!(v, Value::String(_) | Value::Number(_))
        .then(|| Value::Array(vec![Value::String(py_str(v))]))
}

fn coerce_malformed_to_default(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if matches!(v, Value::Null | Value::Array(_)) {
        return Ok(v);
    }
    if let Some(list) = scalar_login(&v) {
        return Ok(list);
    }
    ctx.note(format!(
        "settings.yaml: config.review.github_apps/required_bots is not a list or scalar login ({}); ignoring it",
        py_repr(&v)
    ));
    Ok(Value::Null)
}

fn coerce_optional_apps(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if matches!(v, Value::Null | Value::Array(_)) {
        return Ok(v);
    }
    if let Some(list) = scalar_login(&v) {
        return Ok(list);
    }
    ctx.note(format!(
        "settings.yaml: config.review.optional_apps is not a list or scalar login ({}); ignoring it",
        py_repr(&v)
    ));
    Ok(Value::Array(Vec::new()))
}

fn coerce_and_resolve_reviewers(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let raw = match v {
        Value::Null => return Ok(Value::Array(Vec::new())),
        Value::String(s) => vec![Value::String(s)],
        Value::Array(items) => items,
        other => {
            return Err(Fail::Value(format!(
                "config.review.reviewers must be a scalar or list of reviewer names (got {})",
                py_repr(&other)
            )))
        }
    };
    let mut known: Vec<String> = BUILTIN_REVIEWERS.iter().map(|s| s.to_string()).collect();
    if let Some(Value::Object(registry)) = ctx.data.get("reviewer_registry") {
        for name in registry.keys() {
            if !known.contains(name) {
                known.push(name.clone());
            }
        }
    }
    known.sort();
    let mut cleaned = Vec::with_capacity(raw.len());
    for entry in &raw {
        let Value::String(text) = entry else {
            return Err(Fail::Value(format!(
                "config.review.reviewers entry must be a string (got {})",
                py_repr(entry)
            )));
        };
        let name = py_strip(text).trim_start_matches('/').to_string();
        if !known.contains(&name) {
            return Err(Fail::Value(format!(
                "config.review.reviewers names an unresolvable reviewer {} (expected one of {}); a typo would leave the gate permanently unsatisfiable{}",
                py_repr(entry),
                repr_list(&known),
                reviewer_hint(&name, &known)
            )));
        }
        cleaned.push(Value::String(name));
    }
    Ok(Value::Array(cleaned))
}

/// `_reviewer_hint`: the wrong-key guess first, then difflib's closest name.
fn reviewer_hint(name: &str, known: &[String]) -> String {
    if EXTERNAL_REVIEWER_VOCABULARY.contains(&name.to_lowercase().as_str()) {
        return format!(
            ". {} is an AI reviewer name - you probably wanted config.review.external_reviewers, which names who to REQUEST a PR review from; config.review.reviewers is the local-attestation gate",
            repr_text(name)
        );
    }
    match close_match(name, known) {
        Some(guess) => format!(". Did you mean {}?", repr_text(guess)),
        None => String::new(),
    }
}

/// `difflib.get_close_matches(word, possibilities, n=1)`: the highest
/// (ratio, candidate) pair at or above the 0.6 cutoff.
fn close_match<'a>(word: &str, possibilities: &'a [String]) -> Option<&'a str> {
    let b: Vec<char> = word.chars().collect();
    let mut best: Option<(f64, &str)> = None;
    for candidate in possibilities {
        let a: Vec<char> = candidate.chars().collect();
        let score = sequence_ratio(&a, &b);
        if score < 0.6 {
            continue;
        }
        let better = match best {
            None => true,
            Some((top, top_name)) => score > top || (score == top && candidate.as_str() > top_name),
        };
        if better {
            best = Some((score, candidate.as_str()));
        }
    }
    best.map(|(_, name)| name)
}

/// `SequenceMatcher(None, a, b).ratio()`, autojunk included.
fn sequence_ratio(a: &[char], b: &[char]) -> f64 {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    if b.len() >= 200 {
        let ntest = b.len() / 100 + 1;
        b2j.retain(|_, idxs| idxs.len() <= ntest);
    }
    let mut matched = 0usize;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some(span) = queue.pop() {
        let (alo, ahi, blo, bhi) = span;
        let (i, j, k) = longest_match(a, b, &b2j, span);
        if k > 0 {
            matched += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    2.0 * matched as f64 / total as f64
}

fn longest_match(
    a: &[char],
    b: &[char],
    b2j: &HashMap<char, Vec<usize>>,
    (alo, ahi, blo, bhi): (usize, usize, usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, ch) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut next: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(ch) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let k = if j == 0 {
                    1
                } else {
                    j2len.get(&(j - 1)).copied().unwrap_or(0) + 1
                };
                next.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = next;
    }
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi
        && bestj + bestsize < bhi
        && a[besti + bestsize] == b[bestj + bestsize]
    {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}

fn coerce_peers(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    let items = match v {
        Value::Null => return Ok(Value::Array(Vec::new())),
        Value::String(s) => vec![Value::String(s)],
        Value::Object(map) => vec![Value::Object(map)],
        Value::Array(items) => items,
        other => {
            return Err(Fail::Value(format!(
                "config.review.peers must be a scalar or list (got {})",
                py_repr(&other)
            )))
        }
    };
    for entry in &items {
        if let Value::Object(map) = entry {
            if !map.contains_key("provider") {
                return Err(Fail::Value(format!(
                    "config.review.peers map entry is missing 'provider': {}",
                    py_repr(entry)
                )));
            }
        }
        if !matches!(entry, Value::String(_) | Value::Object(_)) {
            return Err(Fail::Value(format!(
                "config.review.peers entry must be a string or map: {}",
                py_repr(entry)
            )));
        }
    }
    Ok(Value::Array(items))
}

fn resolve_github_apps_alias(ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let github = m.get("github_apps").cloned().unwrap_or(Value::Null);
    let required = m.get("required_bots").cloned().unwrap_or(Value::Null);
    let resolved = if !github.is_null() && !required.is_null() {
        ctx.note(
            "settings.yaml: both config.review.github_apps and the legacy config.review.required_bots are set; using github_apps",
        );
        github
    } else if github.is_null() && !required.is_null() {
        required
    } else {
        github
    };
    m.insert("github_apps".into(), resolved.clone());
    m.insert("required_bots".into(), resolved);

    let peers = match m.get("peers") {
        Some(Value::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    for e in &peers {
        let (provider, model) = match e {
            Value::String(_) => (Some(e), None),
            Value::Object(map) => (map.get("provider"), map.get("model")),
            _ => continue,
        };
        let claude =
            matches!(provider, Some(Value::String(p)) if py_strip(p).to_lowercase() == "claude");
        if !claude {
            continue;
        }
        let route = match model {
            Some(Value::String(s)) => py_strip(s).to_string(),
            _ => String::new(),
        };
        let parts: Vec<&str> = route.split(',').map(py_strip).collect();
        if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
            return Err(Fail::Value(format!(
                "config.review.peers has a claude peer with no valid model route (need \"route_provider,route_model\", e.g. \"zai,glm-5.3\"); a bare claude peer is the same model as the author, breaking the distinct-model trust invariant: {}",
                py_repr(e)
            )));
        }
        if matches!(parts[0].to_lowercase().as_str(), "anthropic" | "claude") {
            return Err(Fail::Value(format!(
                "config.review.peers claude peer routes to the author's own provider ({}), which is not a distinct model - it breaks the distinct-model trust invariant; route to a different provider (e.g. zai): {}",
                repr_text(parts[0]),
                py_repr(e)
            )));
        }
    }
    Ok(())
}

fn coerce_external_reviewers(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Null => Ok(Value::Array(Vec::new())),
        Value::String(s) => Ok(Value::Array(vec![Value::String(s)])),
        Value::Array(items) => Ok(Value::Array(items)),
        other => {
            ctx.note(format!(
                "settings.yaml: config.review.external_reviewers is not a list ({}); treating external review as disabled",
                py_repr(&other)
            ));
            Ok(Value::Array(Vec::new()))
        }
    }
}

fn coerce_agent_providers(ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Null => Ok(Value::Object(Map::new())),
        Value::Object(map) => Ok(Value::Object(map)),
        other => {
            ctx.note(format!(
                "settings.yaml: config.review.agent_harnesses is not a mapping ({}); ignoring it (cross-model stays off)",
                py_repr(&other)
            ));
            Ok(Value::Object(Map::new()))
        }
    }
}

fn resolve_agent_harnesses_alias(ctx: &mut Ctx, m: &mut Map<String, Value>) -> Result<(), Fail> {
    let harnesses = m
        .get("agent_harnesses")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let providers = m
        .get("agent_providers")
        .cloned()
        .unwrap_or_else(|| Value::Object(Map::new()));
    let resolved = if truthy(&harnesses) && truthy(&providers) && harnesses != providers {
        ctx.note(
            "settings.yaml: both config.review.agent_harnesses and the pre-rename config.review.agent_providers are set and differ; using agent_harnesses",
        );
        harnesses
    } else if !truthy(&harnesses) && truthy(&providers) {
        providers
    } else {
        harnesses
    };
    m.insert("agent_harnesses".into(), resolved.clone());
    m.insert("agent_providers".into(), resolved);
    Ok(())
}

fn validate_agent_routes(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    if !truthy(&v) {
        return Ok(Value::Object(Map::new()));
    }
    Err(Fail::Value(
        "config.review.agent_routes retired with the sigma panel; the review-posture ladder (config.review.posture) replaced per-agent review routing. Remove the key."
            .into(),
    ))
}

fn coerce_cross_model(_ctx: &mut Ctx, v: Value) -> Result<Value, Fail> {
    match v {
        Value::Object(map) => Ok(Value::Object(map)),
        _ => Ok(Value::Object(Map::new())),
    }
}
