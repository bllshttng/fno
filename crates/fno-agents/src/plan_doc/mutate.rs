//! `fno do plan mutate <doc> [--finalize]`: the /blueprint single-doc writer
//! (hidden, beside `parse`). A plain run mints the Execution Strategy draft
//! from the User Stories and keeps the doc at `design`; `--finalize` checks
//! the enriched draft against the compiled-v1 contract and promotes it to
//! `ready`. Frontmatter edits go through the codec, so every key the author
//! wrote keeps its own spelling.
//!
//! Exit codes: 0 written (or printed with --no-emit); 1 not a path, no doc,
//! or already past the rung; 2 a section the blueprint may not write, user
//! stories it cannot read, or a draft that is not ready; 3 frontmatter
//! missing, invalid or unreadable.

use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value};
use serde_yaml_ng::Value as Yaml;

use super::cli::{BLUEPRINT_WRITE_ALLOWLIST, STATUS_PROGRESSION};
use super::codec::{self, Fields, Value as Fm};

type Outcome = (i32, String);

const PAST_BLUEPRINT: &[&str] = &[
    "in_progress",
    "in_review",
    "reviewing",
    "shipping",
    "shipped",
];
/// The floor band when the difficulty gate would refuse a band-less write:
/// a mis-tiered cheap run costs more than a mis-tiered strong one.
const DEFAULT_DIFFICULTY: &str = "high";
const DEFAULT_KILL_CRITERIA: &str = "  - name: iteration_ceiling\n    predicate: iteration > 15\n    reason: Too many iterations - planning likely wrong\n  - name: stuck_test\n    predicate: same_test_failing_for >= 3\n    reason: Same test failing 3+ iterations - root cause unclear";
const US_ID: &str = r"US\d+[a-z]*(?:\.\d+)?";

fn allowed(name: &str) -> Result<(), Outcome> {
    if BLUEPRINT_WRITE_ALLOWLIST.contains(&name) {
        return Ok(());
    }
    let mut sorted: Vec<&str> = BLUEPRINT_WRITE_ALLOWLIST.to_vec();
    sorted.sort_unstable();
    Err((2, format!("/blueprint attempted to write section {name:?}, which is not in BLUEPRINT_WRITE_ALLOWLIST. Allowed sections: {sorted:?}")))
}

fn alias(status: &str) -> String {
    crate::graph_store::PLAN_STATUS_ALIASES
        .iter()
        .find(|(from, _)| *from == status)
        .map(|(_, to)| to.to_string())
        .unwrap_or_else(|| status.to_string())
}

/// The YAML status as a known rung; an unquoted `true` reads as "true".
fn coerce_status(value: &Value) -> Result<String, String> {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Null => {
            return Err(format!(
                "Status value is None; expected one of: {}",
                STATUS_PROGRESSION
                    .iter()
                    .map(|s| format!("'{s}'"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
        other => other.to_string(),
    };
    let coerced = alias(&raw);
    if STATUS_PROGRESSION.contains(&coerced.as_str()) {
        Ok(coerced)
    } else {
        Err(format!("Unknown status '{coerced}' (coerced from {value}). Valid statuses: {STATUS_PROGRESSION:?}"))
    }
}

fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
    }
}

fn looks_like_path(arg: &str) -> bool {
    arg.contains('/') || arg.ends_with(".md") || arg.starts_with('~')
}

fn first_fill_block() -> String {
    let today = chrono::Local::now().date_naive();
    format!("---\nstatus: design\ncreated: {today}\ntype: blueprint\nsources: []\n---\n\n")
}

/// Path-looking tokens in the Architecture section: a slash plus an
/// extension, or absolute. The backtick branch stays on one line, so a fenced
/// diagram is never read as one giant path.
fn architecture_paths(section: &str) -> Vec<String> {
    let re =
        Regex::new(r"`([^`\n]+)`|((?:[a-zA-Z0-9_.@-]+/)+[a-zA-Z0-9_.-]+)|((?:/[a-zA-Z0-9_.-]+)+)")
            .unwrap();
    let ext = Regex::new(r"\.[a-zA-Z0-9]+$").unwrap();
    let mut out: Vec<String> = Vec::new();
    for caps in re.captures_iter(section) {
        if let Some(bare) = caps.get(2) {
            // The bare branch never starts mid-word.
            let before = section[..bare.start()].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
        }
        let Some(hit) = caps.get(1).or(caps.get(2)).or(caps.get(3)) else {
            continue;
        };
        let candidate = hit
            .as_str()
            .trim_matches(|c| c == '`' || c == ' ' || c == '\t');
        let keep =
            (candidate.contains('/') && ext.is_match(candidate)) || candidate.starts_with('/');
        if keep && !out.iter().any(|p| p == candidate) {
            out.push(candidate.to_string());
        }
    }
    out
}

fn path_exists(path: &str, root: &Path) -> bool {
    let p = Path::new(path);
    if p.is_absolute() {
        p.exists()
    } else {
        root.join(p).exists()
    }
}

/// Brownfield when at least half the Architecture paths already exist.
fn detect_mode(architecture: Option<&str>, root: &Path) -> &'static str {
    let paths = architecture.map(architecture_paths).unwrap_or_default();
    if paths.is_empty() {
        return "greenfield";
    }
    let existing = paths.iter().filter(|p| path_exists(p, root)).count();
    if existing * 2 >= paths.len() {
        "brownfield"
    } else {
        "greenfield"
    }
}

fn ownership_map(architecture: Option<&str>, root: &Path) -> String {
    let paths = architecture.map(architecture_paths).unwrap_or_default();
    if paths.is_empty() {
        return "| File | Action | Owner |\n|---|---|---|\n".into();
    }
    let mut rows = vec![
        "| File | Action | Owner |".to_string(),
        "|---|---|---|".to_string(),
    ];
    for p in paths {
        let action = if path_exists(&p, root) {
            "modify"
        } else {
            "create"
        };
        rows.push(format!("| `{p}` | {action} | /blueprint |"));
    }
    rows.join("\n")
}

/// The first non-blank line after `end`, or "" when the next thing is a
/// heading or another story anchor, so an empty marker never steals the
/// next story's text. A blockquote marker is stripped.
fn first_line_after(body: &str, end: usize, anchor: &Regex) -> String {
    for raw in body[end..].lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("##") || anchor.is_match(line) {
            break;
        }
        return line.trim_start_matches('>').trim_start().to_string();
    }
    String::new()
}

/// User stories in document order, first occurrence of an id winning:
/// `**US1:** d`, `### US1: T`, `| US1 | d |`, `- US1: d` and `1. US1: d`.
pub(crate) fn parse_user_stories(body: &str) -> Vec<(String, String)> {
    let heading = Regex::new(&format!(
        r"(?m)^###[ \t]+({US_ID})[ \t]*[:.\-–—]?[ \t]*([^\n]*)$"
    ))
    .unwrap();
    let anchor = Regex::new(&format!(
        r"^(?:###\s+{US_ID}\b|\*\*{US_ID}\b|\||[-*+]\s+{US_ID}\b|\d+\.\s+{US_ID}\b)"
    ))
    .unwrap();
    let bold = Regex::new(&format!(
        r"(?m)\*\*({US_ID})(?:[:.][^*\n]*|[ \t]+[-–—][ \t]+[^*\n]+|)\*\*[: \t]*([^\n]*)$"
    ))
    .unwrap();
    let table = Regex::new(&format!(
        r"(?m)^[ \t]*\|?[ \t]*({US_ID})[ \t]*\|[ \t]*([^|\n]*)"
    ))
    .unwrap();
    let list = Regex::new(&format!(
        r"(?m)^[ \t]*(?:[-*+]|\d+\.)[ \t]+({US_ID})[ \t]*[:.\-–—]?[ \t]*([^\n]*)$"
    ))
    .unwrap();
    let mut found: Vec<(usize, String, String)> = Vec::new();
    for (re, follow) in [
        (&heading, true),
        (&bold, true),
        (&table, false),
        (&list, true),
    ] {
        for caps in re.captures_iter(body) {
            let whole = caps.get(0).unwrap();
            let mut text = caps[2].trim().to_string();
            if text.is_empty() && follow {
                text = first_line_after(body, whole.end(), &anchor);
            }
            if !text.is_empty() {
                found.push((whole.start(), caps[1].to_string(), text));
            }
        }
    }
    found.sort_by_key(|(pos, ..)| *pos);
    let mut seen = std::collections::BTreeSet::new();
    found
        .into_iter()
        .filter(|(_, id, _)| seen.insert(id.clone()))
        .map(|(_, id, d)| (id, d))
        .collect()
}

fn scalar(value: &Yaml) -> String {
    match value {
        Yaml::String(s) if s.contains('\n') => serde_json::to_string(s).unwrap_or_default(),
        Yaml::Sequence(s) if s.is_empty() => "[]".into(),
        Yaml::Mapping(m) if m.is_empty() => "{}".into(),
        other => serde_yaml_ng::to_string(other)
            .unwrap_or_default()
            .trim_end()
            .to_string(),
    }
}

fn emit_map(map: &serde_yaml_ng::Mapping, indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for (key, value) in map {
        match value {
            Yaml::Sequence(seq) if !seq.is_empty() => {
                out.push_str(&format!("{pad}{}:\n", scalar(key)));
                emit_seq(seq, indent + 2, out);
            }
            Yaml::Mapping(inner) if !inner.is_empty() => {
                out.push_str(&format!("{pad}{}:\n", scalar(key)));
                emit_map(inner, indent + 2, out);
            }
            other => out.push_str(&format!("{pad}{}: {}\n", scalar(key), scalar(other))),
        }
    }
}

fn emit_seq(seq: &[Yaml], indent: usize, out: &mut String) {
    let pad = " ".repeat(indent);
    for item in seq {
        match item {
            Yaml::Mapping(map) if !map.is_empty() => {
                let mut inner = String::new();
                emit_map(map, indent + 2, &mut inner);
                out.push_str(&format!("{pad}- {}", &inner[indent + 2..]));
            }
            Yaml::Sequence(inner) if !inner.is_empty() => {
                out.push_str(&format!("{pad}-\n"));
                emit_seq(inner, indent + 2, out);
            }
            other => out.push_str(&format!("{pad}- {}\n", scalar(other))),
        }
    }
}

/// Block YAML with indented sequences: the one shape the frontmatter reader
/// round-trips, and the one plans already carry.
pub(crate) fn dump_yaml(value: &Yaml) -> String {
    let mut out = String::new();
    match value {
        Yaml::Mapping(map) => emit_map(map, 0, &mut out),
        other => out.push_str(&format!("{}\n", scalar(other))),
    }
    out
}

fn yaml_map(pairs: Vec<(&str, Yaml)>) -> Yaml {
    let mut map = serde_yaml_ng::Mapping::new();
    for (k, v) in pairs {
        map.insert(Yaml::String(k.into()), v);
    }
    Yaml::Mapping(map)
}

fn ystr(s: &str) -> Yaml {
    Yaml::String(s.to_string())
}

fn draft_task(id: &str, title: &str, notes: &str) -> Yaml {
    yaml_map(vec![
        ("id", ystr(id)),
        ("title", ystr(title)),
        ("surface", Yaml::Sequence(vec![])),
        ("verify", ystr("# fill in verify command")),
        ("acceptance", Yaml::Sequence(vec![])),
        ("notes", ystr(notes)),
    ])
}

/// The Execution Strategy fence, one task per user story in one wave. A
/// non-empty User Stories body with no story the parser reads refuses: a
/// plausible one-task plan would silently under-scope the build.
pub(crate) fn build_execution_strategy(
    stories_body: &str,
    difficulty: &str,
) -> Result<String, String> {
    let stories = parse_user_stories(stories_body);
    let tasks: Vec<Yaml> = if stories.is_empty() {
        if !stories_body.trim().is_empty() {
            return Err("## User Stories has content but no parseable stories were found. Rewrite each story in one of the accepted shapes (US1, US2, ...):\n  - **US1:** description            (inline bold)\n  - ### US1: Title                  (h3 heading)\n  - | US1 | description |           (table row)\n  - - US1: description              (dash / asterisk / plus bullet)\n  - 1. US1: description             (numbered list)\nOr leave the section truly empty if the work is not yet decomposed.".into());
        }
        eprintln!("WARNING: no ## User Stories entries found; emitting single default task");
        vec![draft_task(
            "1.1",
            "implement feature",
            "Default task; no User Stories found in design doc.",
        )]
    } else {
        stories
            .iter()
            .enumerate()
            .map(|(i, (id, desc))| {
                let title: String = desc.chars().take(80).collect();
                draft_task(&format!("1.{}", i + 1), &title, &format!("Implement {id}."))
            })
            .collect()
    };
    let ids: Vec<Yaml> = tasks
        .iter()
        .map(|t| t.get("id").cloned().unwrap_or(Yaml::Null))
        .collect();
    let mut wave = vec![
        ("wave", Yaml::Number(1.into())),
        ("mode", ystr("sequential")),
        ("name", ystr("Implementation")),
        ("tasks", Yaml::Sequence(ids)),
    ];
    if !difficulty.is_empty() {
        wave.push(("difficulty", ystr(difficulty)));
    }
    let strategy = yaml_map(vec![
        ("execution_mode", ystr("mixed")),
        ("waves", Yaml::Sequence(vec![yaml_map(wave)])),
        ("tasks", Yaml::Sequence(tasks)),
    ]);
    Ok(format!("```yaml\n{}```", dump_yaml(&strategy)))
}

/// Recompute each wave's mode in the Execution Strategy fence: parallel only
/// when every task names files and no two share one. An unreadable or
/// missing fence is left as written; the execution validator owns refusals.
pub(crate) fn apply_wave_modes(doc: &str) -> String {
    let fence =
        Regex::new(r"(?ms)(^## Execution Strategy\s*$.*?^```yaml[ \t]*$\n)(.*?)(^```[ \t]*$)")
            .unwrap();
    let Some(caps) = fence.captures(doc) else {
        return doc.to_string();
    };
    let body = caps.get(2).unwrap();
    let Ok(mut strategy) = serde_yaml_ng::from_str::<Yaml>(body.as_str()) else {
        return doc.to_string();
    };
    let text = |v: &Yaml| match v {
        Yaml::String(s) => s.clone(),
        other => scalar(other),
    };
    let mut surfaces: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        Default::default();
    for task in strategy
        .get("tasks")
        .and_then(Yaml::as_sequence)
        .into_iter()
        .flatten()
    {
        if let Some(id) = task.get("id").filter(|v| !v.is_null()) {
            let set = task
                .get("surface")
                .and_then(Yaml::as_sequence)
                .map(|s| s.iter().map(text).collect())
                .unwrap_or_default();
            surfaces.insert(text(id), set);
        }
    }
    let Some(waves) = strategy.get_mut("waves").and_then(Yaml::as_sequence_mut) else {
        return doc.to_string();
    };
    let mut changed = false;
    for wave in waves.iter_mut().filter(|w| w.is_mapping()) {
        let ids: Vec<String> = wave
            .get("tasks")
            .and_then(Yaml::as_sequence)
            .map(|s| s.iter().map(text).collect())
            .unwrap_or_default();
        let mut seen = std::collections::BTreeSet::new();
        let parallel = !ids.is_empty()
            && ids.iter().all(|id| {
                let set = surfaces.get(id).cloned().unwrap_or_default();
                let fresh = !set.is_empty() && set.is_disjoint(&seen);
                seen.extend(set);
                fresh
            });
        let mode = if parallel { "parallel" } else { "sequential" };
        if wave.get("mode").and_then(Yaml::as_str) != Some(mode) {
            wave.as_mapping_mut()
                .unwrap()
                .insert(ystr("mode"), ystr(mode));
            changed = true;
        }
    }
    if !changed {
        return doc.to_string();
    }
    format!(
        "{}{}{}",
        &doc[..body.start()],
        dump_yaml(&strategy),
        &doc[body.end()..]
    )
}

fn remove_blueprint_sections(body: &str) -> String {
    let mut skip = false;
    let mut out = String::new();
    for line in body.split_inclusive('\n') {
        if let Some(heading) = line.strip_prefix("## ") {
            skip = BLUEPRINT_WRITE_ALLOWLIST.contains(&heading.trim_end());
            if skip {
                continue;
            }
        }
        if !skip {
            out.push_str(line);
        }
    }
    out
}

/// New sections land right after `## Open Questions`, else before
/// Deferred / Out of Scope / References, else at the end.
fn append_sections(body: &str, sections: &[(String, String)]) -> String {
    let lines: Vec<&str> = body.split_inclusive('\n').collect();
    let heading = |l: &str| l.strip_prefix("## ").map(|h| h.trim_end().to_string());
    let mut at = lines.len();
    let mut in_questions = false;
    let mut questions_end = None;
    for (i, line) in lines.iter().enumerate() {
        if let Some(h) = heading(line) {
            if h == "Open Questions" {
                in_questions = true;
                continue;
            }
            if in_questions {
                questions_end = Some(i);
                break;
            }
        }
    }
    if let Some(end) = questions_end {
        at = end;
    } else if let Some(i) = lines.iter().position(|l| {
        heading(l).is_some_and(|h| {
            ["deferred", "out of scope", "references"].contains(&h.to_lowercase().as_str())
        })
    }) {
        at = i;
    }
    let added: String = sections
        .iter()
        .map(|(n, c)| format!("\n## {n}\n\n{c}\n"))
        .collect();
    let before = lines[..at].concat();
    let after = lines[at..].concat();
    let tail = if after.trim().is_empty() {
        String::new()
    } else {
        format!("\n{}", after.trim_start_matches('\n'))
    };
    format!("{}\n{added}{tail}", before.trim_end_matches('\n'))
}

fn split_doc(text: &str) -> Result<(Fields, String), String> {
    let parsed = codec::parse_frontmatter(text)?;
    Ok((
        parsed.fields,
        parsed.rest.trim_start_matches('\n').to_string(),
    ))
}

fn render(fields: &Fields, body: &str) -> String {
    format!(
        "---\n{}\n---\n\n{body}",
        codec::serialize_frontmatter(fields)
    )
}

fn typed(doc: &str) -> Map<String, Value> {
    super::doc::load_plan_text(doc)
        .ok()
        .and_then(|d| d.frontmatter.as_object().cloned())
        .unwrap_or_default()
}

/// Stamp the floor band only when the band is absent or blank and adding one
/// is exactly what clears the gate: a pre-gate plan passes band-less by
/// design, and a typo'd band must reach the schema check that names it.
fn ensure_difficulty(fields: &mut Fields, body: &str) {
    let fm = typed(&render(fields, body));
    let absent = match fm.get("difficulty") {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        _ => false,
    };
    if !absent {
        return;
    }
    let mut banded = fm.clone();
    banded.insert("difficulty".into(), Value::String("low".into()));
    if crate::plan_schema::difficulty_gate_error(&fm, true).is_some()
        && crate::plan_schema::difficulty_gate_error(&banded, true).is_none()
    {
        fields.insert("difficulty", Fm::Scalar(DEFAULT_DIFFICULTY.into()));
    }
}

/// Present-but-invalid fields refuse; a missing required field does not
/// (a design doc binds its node later). `skip_status` matches the reader
/// that tolerates status drift.
fn schema_refusal(doc: &str) -> Option<String> {
    let lines: Vec<String> = crate::plan_schema::validate_frontmatter(&typed(doc))
        .iter()
        .filter(|v| v.get("message").and_then(Value::as_str) != Some("Field required"))
        .map(|v| {
            let field = v
                .get("field")
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty())
                .unwrap_or("<root>");
            format!(
                "  {field}: {} (got {})",
                v.get("message").and_then(Value::as_str).unwrap_or(""),
                v.get("got").cloned().unwrap_or(Value::Null)
            )
        })
        .collect();
    (!lines.is_empty()).then(|| {
        format!(
            "proposed frontmatter fails schema validation:\n{}",
            lines.join("\n")
        )
    })
}

/// The bound node: `node` and `claims` are one fact under two spellings; two
/// ids that disagree bind nothing and say so.
pub(crate) fn bind_node(fm: &Map<String, Value>) -> Result<Option<String>, String> {
    let clean = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty() && s != "null");
    let node = fm.get("node").and_then(Value::as_str).and_then(clean);
    let claims = match fm.get("claims") {
        Some(Value::String(s)) => clean(s),
        Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).find_map(clean),
        _ => None,
    };
    match (node, claims) {
        (Some(n), Some(c)) if n != c => Err(format!("plan frontmatter binds two different ids (node: {n}, claims: {c}); binding nothing - fix the frontmatter")),
        (n, c) => Ok(n.or(c)),
    }
}

/// Re-assert plan_path on the bound node so the graph recomputes its status
/// from the doc just written. Best-effort: the doc is already durable.
fn sync_graph(doc: &str, plan: &Path) {
    let node = match bind_node(&typed(doc)) {
        Err(warning) => return eprintln!("Warning: {warning}"),
        Ok(None) => return,
        Ok(Some(node)) => node,
    };
    let mut cmd = std::process::Command::new("fno");
    cmd.args([
        "backlog",
        "update",
        &node,
        &format!("plan_path={}", plan.display()),
    ]);
    match crate::bounded_cmd::output_with_timeout_result(cmd, 30) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("Warning: graph status refresh failed for {node}: {e}"),
        Ok(out) if !out.status.success() => {
            let detail: String = String::from_utf8_lossy(&out.stderr)
                .trim()
                .chars()
                .take(200)
                .collect();
            eprintln!("Warning: graph status refresh failed for {node}: {detail}");
        }
        Ok(_) => {}
    }
}

fn write(path: &Path, doc: &str) -> Result<(), Outcome> {
    codec::atomic_write(path, doc).map_err(|e| (3, format!("Atomic write failed: {e}")))
}

fn violations(path: &Path) -> Vec<Value> {
    match super::exec_validate::op_validate_execution(path, true, Some("compiled-v1")) {
        Ok(report) => report
            .get("violations")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        Err(e) => vec![
            serde_json::json!({"field": "door", "message": format!("execution validator unavailable: {e}")}),
        ],
    }
}

fn not_ready(found: &[Value]) -> Outcome {
    let lines: Vec<String> = found
        .iter()
        .map(|v| {
            format!(
                "  {}: {}",
                v.get("field").and_then(Value::as_str).unwrap_or("None"),
                v.get("message").and_then(Value::as_str).unwrap_or("")
            )
        })
        .collect();
    (
        2,
        format!("Execution Strategy is not ready:\n{}", lines.join("\n")),
    )
}

/// The draft write: a design doc gains the Execution Strategy skeleton (plus
/// File Ownership Map and Patterns to Reuse in brownfield) and stays design.
pub(crate) fn draft(arg: &str, mode: &str, rewrite: bool, no_emit: bool, root: &Path) -> Outcome {
    if !looks_like_path(arg) {
        return (1, "Blueprint owns the whole plan, but needs a save path. Resolve one with `fno do plan path --slug \"<slug>\"` (add `--node <id>` for a node-seeded plan), then pass that path. Or pass a node id directly.".into());
    }
    let resolved = expand(arg);
    if !resolved.exists() {
        return (1, format!("No doc at {}. Blueprint creates plans via `fno do plan path`; pass a resolved path, or pass a node id and let the skill seed the doc.", resolved.display()));
    }
    let mut text = match std::fs::read_to_string(&resolved) {
        Ok(t) => t,
        Err(e) => return (3, format!("Cannot read {}: {e}", resolved.display())),
    };
    // A research doc carries no frontmatter; one that opens with `---` is
    // parsed as written, so malformed YAML still refuses.
    if !text.starts_with("---") {
        text = first_fill_block() + &text;
    }
    let doc = match super::doc::load_plan_text(&text) {
        Ok(doc) => doc,
        Err(e) => return (3, format!("Frontmatter parse error: {}", e.message)),
    };
    let Some(raw_status) = doc.frontmatter.get("status") else {
        return (
            3,
            "Frontmatter 'status' is missing. Add `status: design` to the frontmatter and retry."
                .into(),
        );
    };
    let mut status = match coerce_status(raw_status) {
        Ok(s) => s,
        Err(e) => return (3, format!("Frontmatter status invalid: {e}")),
    };
    let drafted = doc.has_section("Execution Strategy");
    // in_progress comes from holding the node claim, not from blueprint: with
    // no Execution Strategy the doc is still at its design rung.
    if status == "in_progress" && !drafted {
        status = "design".into();
    }
    if PAST_BLUEPRINT.contains(&status.as_str()) {
        return (1, format!("doc is in `{status}` status (past blueprint phase); cannot mutate even with --rewrite."));
    }
    if status == "ready" && !rewrite {
        return (
            1,
            "doc already in `ready` status; pass `rewrite` to regenerate execution sections."
                .into(),
        );
    }
    if status == "design" && drafted && !rewrite {
        return (1, "doc already has a Blueprint execution draft; pass `rewrite` to regenerate it or `--finalize` after enrichment.".into());
    }
    let architecture = doc.get_section("Architecture");
    let mode = if mode == "auto" {
        detect_mode(architecture, root)
    } else {
        mode
    };
    for name in [
        "Execution Strategy",
        "kill_criteria",
        "execution_mode",
        "waves",
        "difficulty",
    ] {
        if let Err(refused) = allowed(name) {
            return refused;
        }
    }
    let band = doc
        .frontmatter
        .get("difficulty")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let strategy =
        match build_execution_strategy(doc.get_section("User Stories").unwrap_or(""), &band) {
            Ok(s) => s,
            Err(e) => return (2, e),
        };
    let mut sections = vec![("Execution Strategy".to_string(), strategy)];
    if mode == "brownfield" {
        sections.push((
            "File Ownership Map".into(),
            ownership_map(architecture, root),
        ));
        sections.push((
            "Patterns to Reuse".into(),
            "*(populate during implementation)*".into(),
        ));
    }
    let (mut fields, mut body) = match split_doc(&text) {
        Ok(split) => split,
        Err(e) => return (3, format!("Frontmatter parse error: {e}")),
    };
    // Author-set values win; a default fills only an unset or empty key.
    let fm = &doc.frontmatter;
    if !truthy(fm.get("kill_criteria")) {
        fields.insert("kill_criteria", Fm::Raw(DEFAULT_KILL_CRITERIA.into()));
    }
    if !truthy(fm.get("execution_mode")) {
        fields.insert("execution_mode", Fm::Scalar("mixed".into()));
    }
    if !truthy(fm.get("waves")) {
        fields.insert("waves", Fm::BlockList(vec!["1".into()]));
    }
    ensure_difficulty(&mut fields, &body);
    if rewrite {
        body = remove_blueprint_sections(&body);
    }
    let proposed = render(&fields, &append_sections(&body, &sections));
    if let Some(refusal) = schema_refusal(&proposed) {
        return (3, refusal);
    }
    if no_emit {
        return (0, proposed);
    }
    if let Err(refused) = write(&resolved, &proposed) {
        return refused;
    }
    sync_graph(&proposed, &resolved);
    if !crate::backlog::collision::has_file_surface(&resolved) {
        let name = resolved
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        eprintln!("WARNING: {name} states no file surface (no parseable '## File Ownership Map' or '## Files to Modify' table). Collision detection cannot compare this plan against in-flight work; fill the table in before dispatch.");
    }
    (0, proposed)
}

/// Promote an enriched draft from design to ready with the compiled-v1
/// stamp. An unstamped ready doc finalizes to itself (the historical
/// permissive read); a stamped one must still satisfy its contract.
pub(crate) fn finalize(arg: &str, no_emit: bool) -> Outcome {
    let resolved = match expand(arg).canonicalize() {
        Ok(p) => p,
        Err(e) => return (3, format!("Cannot read {arg}: {e}")),
    };
    let text = match std::fs::read_to_string(&resolved) {
        Ok(t) => t,
        Err(e) => return (3, format!("Cannot read {arg}: {e}")),
    };
    let doc = match super::doc::load_plan_text(&text) {
        Ok(doc) => doc,
        Err(e) => return (3, format!("Cannot read {arg}: {}", e.message)),
    };
    let mut status = match coerce_status(doc.frontmatter.get("status").unwrap_or(&Value::Null)) {
        Ok(s) => s,
        Err(e) => return (3, format!("Frontmatter status invalid: {e}")),
    };
    let stamped = doc
        .frontmatter
        .get("acceptance_contract")
        .and_then(Value::as_str)
        .map(str::trim)
        == Some("compiled-v1");
    // A doc that really reached ready carries the stamp, so an unstamped
    // in_progress came from the claim and never had a contract to run.
    if status == "in_progress" && !stamped {
        status = "design".into();
    }
    if status != "design" && status != "ready" {
        return (
            1,
            format!("cannot finalize Blueprint draft from status '{status}'"),
        );
    }
    if status == "ready" && !stamped {
        return (0, text);
    }
    let found = violations(&resolved);
    if status == "ready" && found.is_empty() {
        return (0, text);
    }
    if !found.is_empty() {
        return not_ready(&found);
    }
    for name in ["acceptance_contract", "difficulty"] {
        if let Err(refused) = allowed(name) {
            return refused;
        }
    }
    // Wave fan-out is decided here, the first point where surfaces exist.
    let (mut fields, body) = match split_doc(&apply_wave_modes(&text)) {
        Ok(split) => split,
        Err(e) => return (3, format!("Cannot read {arg}: {e}")),
    };
    fields.insert("status", Fm::Scalar("ready".into()));
    fields.insert("acceptance_contract", Fm::Scalar("compiled-v1".into()));
    ensure_difficulty(&mut fields, &body);
    let proposed = render(&fields, &body);
    if let Some(refusal) = schema_refusal(&proposed) {
        return (3, refusal);
    }
    if no_emit {
        return (0, proposed);
    }
    if let Err(refused) = write(&resolved, &proposed) {
        return refused;
    }
    sync_graph(&proposed, &resolved);
    (0, proposed)
}

fn expand(arg: &str) -> PathBuf {
    match (arg.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => {
            let p = PathBuf::from(arg);
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().map(|d| d.join(&p)).unwrap_or(p)
            }
        }
    }
}

const USAGE: &str = "usage: fno do plan mutate <doc> [--mode greenfield|brownfield|auto] [--rewrite] [--no-emit] [--finalize]";

pub fn run(rest: &[String]) -> i32 {
    let mut doc = None;
    let (mut mode, mut rewrite, mut no_emit, mut finalize_it) =
        ("auto".to_string(), false, false, false);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--mode" => match it.next().map(String::as_str) {
                Some(m @ ("greenfield" | "brownfield" | "auto")) => mode = m.into(),
                other => {
                    eprintln!("{USAGE}\n--mode: invalid choice {other:?}");
                    return 2;
                }
            },
            "--rewrite" => rewrite = true,
            "--no-emit" => no_emit = true,
            "--finalize" => finalize_it = true,
            // The draft is the only mint shape: a plain run keeps design.
            "--draft" => {}
            other if doc.is_none() && !other.starts_with("--") => doc = Some(other.to_string()),
            other => {
                eprintln!("{USAGE}\nunrecognized argument {other:?}");
                return 2;
            }
        }
    }
    let Some(doc) = doc else {
        eprintln!("{USAGE}");
        return 2;
    };
    let root = crate::paths_cli::repo_root().unwrap_or_else(|| PathBuf::from("."));
    let (code, result) = if finalize_it {
        finalize(&doc, no_emit)
    } else {
        draft(&doc, &mode, rewrite, no_emit, &root)
    };
    if code != 0 {
        eprintln!("{result}");
    } else if no_emit {
        println!("{result}");
    } else {
        // The script-direct path skips the skill's collision gate; name it
        // at the one point every direct mutation passes.
        eprintln!("blueprint: run `fno backlog collisions check {doc}` before `fno backlog intake` - the collision gate does not fire on the script-direct path.");
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    const DESIGN: &str = "---\nstatus: design\ncreated: 2026-08-01\nnode: x-aaaa\n---\n\n# T\n\n## User Stories\n\n- **US1:** first story\n- US2: second story\n| US3 | table story |\n\n## Open Questions\n\nnone\n\n## References\n\nr\n";

    #[test]
    fn stories_parse_every_shape_in_document_order() {
        let body = "### US1\n\nheading story\n\n**US2:** bold\n**US1 baseline**\n| US3 | table |\n1. US4. numbered\n- US5\n- US6: next\n";
        let got: Vec<String> = parse_user_stories(body)
            .into_iter()
            .map(|(i, d)| format!("{i}={d}"))
            .collect();
        assert_eq!(
            got,
            [
                "US1=heading story",
                "US2=bold",
                "US3=table",
                "US4=numbered",
                "US6=next"
            ]
        );
        assert!(build_execution_strategy("just prose", "").is_err());
    }

    #[test]
    fn draft_appends_after_open_questions_and_keeps_design() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        std::fs::write(&path, DESIGN).unwrap();
        let (code, out) = draft(
            path.to_str().unwrap(),
            "greenfield",
            false,
            true,
            dir.path(),
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("status: design"));
        assert!(out.contains("kill_criteria:\n  - name: iteration_ceiling"));
        let qs = out.find("## Open Questions").unwrap();
        let es = out.find("## Execution Strategy").unwrap();
        assert!(qs < es && es < out.find("## References").unwrap());
        assert!(
            out.contains("    tasks:\n      - '1.1'\n      - '1.2'\n      - '1.3'\n"),
            "{out}"
        );
        assert!(
            out.contains("  - id: '1.1'\n    title: first story\n"),
            "{out}"
        );
        // A second draft without rewrite refuses once the doc carries one.
        std::fs::write(&path, &out).unwrap();
        assert_eq!(
            draft(path.to_str().unwrap(), "auto", false, true, dir.path()).0,
            1
        );
    }

    #[test]
    fn author_values_survive_and_a_description_is_not_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        std::fs::write(
            &path,
            DESIGN.replace(
                "node: x-aaaa\n",
                "node: x-aaaa\nexecution_mode: sequential\ndifficulty: low\n",
            ),
        )
        .unwrap();
        let (_, out) = draft(
            path.to_str().unwrap(),
            "greenfield",
            false,
            true,
            dir.path(),
        );
        assert!(
            out.contains("execution_mode: sequential") && !out.contains("execution_mode: mixed")
        );
        assert!(out.contains("difficulty: low\n") && out.contains("    difficulty: low\n"));
        assert_eq!(
            draft("add a login page", "auto", false, true, dir.path()).0,
            1
        );
    }

    #[test]
    fn research_doc_is_first_filled_and_malformed_frontmatter_exits_three() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("r.md");
        std::fs::write(&path, "# Research\n\n## User Stories\n\n- US1: a\n").unwrap();
        let (code, out) = draft(
            path.to_str().unwrap(),
            "greenfield",
            false,
            true,
            dir.path(),
        );
        assert_eq!(code, 0, "{out}");
        assert!(out.starts_with("---\nstatus: design\ncreated: "));
        std::fs::write(&path, "---\nstatus: [design\n---\n").unwrap();
        assert_eq!(
            draft(
                path.to_str().unwrap(),
                "greenfield",
                false,
                true,
                dir.path()
            )
            .0,
            3
        );
    }

    #[test]
    fn claim_derived_in_progress_drafts_but_a_drafted_one_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.md");
        std::fs::write(
            &path,
            DESIGN.replace("status: design", "status: in_progress"),
        )
        .unwrap();
        assert_eq!(
            draft(
                path.to_str().unwrap(),
                "greenfield",
                false,
                true,
                dir.path()
            )
            .0,
            0
        );
        std::fs::write(
            &path,
            DESIGN.replace("status: design", "status: in_progress")
                + "\n## Execution Strategy\n\n```yaml\nwaves: []\n```\n",
        )
        .unwrap();
        assert_eq!(
            draft(
                path.to_str().unwrap(),
                "greenfield",
                false,
                true,
                dir.path()
            )
            .0,
            1
        );
    }

    #[test]
    fn wave_mode_follows_surfaces_and_dump_round_trips() {
        let doc = "## Execution Strategy\n\n```yaml\nwaves:\n  - wave: 1\n    mode: sequential\n    tasks: ['1.1', '1.2']\n  - wave: 2\n    mode: parallel\n    tasks: ['2.1', '2.2']\ntasks:\n  - {id: '1.1', surface: [a.rs]}\n  - {id: '1.2', surface: [b.rs]}\n  - {id: '2.1', surface: [c.rs]}\n  - {id: '2.2', surface: [c.rs]}\n```\n";
        let out = apply_wave_modes(doc);
        let parsed = crate::plan_doc::brief::parse_execution_strategy(&out).unwrap();
        assert_eq!(parsed["waves"][0]["mode"], "parallel");
        assert_eq!(parsed["waves"][1]["mode"], "sequential");
        assert_eq!(apply_wave_modes(&out), out, "a settled fence is left alone");
    }

    #[test]
    fn two_spellings_of_the_node_bind_as_one() {
        let fm = |v: Value| v.as_object().cloned().unwrap();
        assert_eq!(
            bind_node(&fm(serde_json::json!({"claims": ["x-1"]}))),
            Ok(Some("x-1".into()))
        );
        assert_eq!(
            bind_node(&fm(serde_json::json!({"node": "x-1", "claims": "x-1"}))),
            Ok(Some("x-1".into()))
        );
        assert!(bind_node(&fm(serde_json::json!({"node": "x-1", "claims": ["x-2"]}))).is_err());
        assert_eq!(
            bind_node(&fm(serde_json::json!({"node": "null"}))),
            Ok(None)
        );
    }
}
