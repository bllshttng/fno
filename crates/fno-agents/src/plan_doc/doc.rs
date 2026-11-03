//! The single-doc plan parser: YAML frontmatter plus `## `-heading sections.
//!
//! The port of `cli/src/fno/plan/_doc.py` (deleted 2026-11-02). Section
//! detection respects fenced code blocks: a `## Foo` line inside a fence is
//! body content, not a heading. Only `## ` (exactly two hash + space) at the
//! start of a line outside a fence opens a section; deeper headings and
//! fenced headings stay in the enclosing section's body.
//!
//! One recorded departure from the Python leg: YAML syntax-error text comes
//! from serde_yaml_ng where the Python leg quoted PyYAML. The refusal shape
//! (a named frontmatter error carrying a best-effort line number) is
//! unchanged; the wording is the parser's own.

use serde_json::{Map, Value};
use serde_yaml_ng::Value as Yaml;

/// A malformed-frontmatter refusal, carrying a best-effort 0-based line.
#[derive(Debug, Clone)]
pub struct FrontmatterError {
    pub message: String,
    pub line: Option<usize>,
}

impl std::fmt::Display for FrontmatterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

/// A parsed plan doc: frontmatter mapping plus ordered `## ` sections.
#[derive(Debug, Clone)]
pub struct PlanDocument {
    pub frontmatter: Value,
    /// `(heading, trimmed body)` in document order.
    pub sections: Vec<(String, String)>,
}

impl PlanDocument {
    pub fn get_section(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find(|(h, _)| h == name)
            .map(|(_, b)| b.as_str())
    }

    pub fn has_section(&self, name: &str) -> bool {
        self.sections.iter().any(|(h, _)| h == name)
    }
}

/// Split `---\\n ... \\n--- ` frontmatter from the body. A file that does not
/// open with `---`, or carries no closing fence, is all body.
fn split_frontmatter(text: &str) -> (String, &str) {
    if !text.starts_with("---") {
        return (String::new(), text);
    }
    let Some(first_nl) = text.find('\n') else {
        return (String::new(), text);
    };
    let rest = &text[first_nl + 1..];
    let Some(close_idx) = rest.find("\n---") else {
        return (String::new(), text);
    };
    let frontmatter = rest[..close_idx].to_string();
    let mut body = &rest[close_idx + 4..];
    if let Some(stripped) = body.strip_prefix('\n') {
        body = stripped;
    }
    (frontmatter, body)
}

/// Python `str()` semantics for a scalar JSON value read from YAML: bools
/// spell `True`/`False`, null spells `None`, numbers stringify.
pub(crate) fn py_str(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::Bool(true)) => "True".to_string(),
        Some(Value::Bool(false)) => "False".to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// The YAML mapping key as the Python-side dict key would read.
pub(crate) fn py_key(k: &serde_yaml_ng::Value) -> String {
    match k {
        serde_yaml_ng::Value::String(s) => s.clone(),
        other => match yaml_to_json(other) {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if b { "True" } else { "False" }.to_string(),
            _ => String::new(),
        },
    }
}

pub(crate) fn yaml_to_json(v: &Yaml) -> Value {
    match v {
        Yaml::Null => Value::Null,
        Yaml::Bool(b) => Value::Bool(*b),
        Yaml::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Number(i.into())
            } else if let Some(u) = n.as_u64() {
                Value::Number(u.into())
            } else {
                match n.as_f64() {
                    Some(f) => serde_json::Number::from_f64(f)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    None => Value::Null,
                }
            }
        }
        Yaml::String(s) => Value::String(s.clone()),
        Yaml::Sequence(seq) => Value::Array(seq.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(map) => {
            let mut out = Map::new();
            for (k, val) in map {
                // Non-string keys (Python dicts allow them; the frontmatter
                // contract does not) stringify the way str() would.
                let key = match k {
                    Yaml::String(s) => s.clone(),
                    other => match yaml_to_json(other) {
                        Value::String(s) => s,
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => String::new(),
                    },
                };
                out.insert(key, yaml_to_json(val));
            }
            Value::Object(out)
        }
        Yaml::Tagged(t) => yaml_to_json(&t.value),
    }
}

/// Parse the frontmatter block into a JSON mapping. `{}` for an empty block.
fn parse_frontmatter(yaml_text: &str) -> Result<Value, FrontmatterError> {
    if yaml_text.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let parsed: Yaml = serde_yaml_ng::from_str(yaml_text).map_err(|e| {
        let line = e.location().map(|l| l.line().saturating_sub(1));
        FrontmatterError {
            message: format!("{e}"),
            line,
        }
    })?;
    match parsed {
        Yaml::Null => Ok(Value::Object(Map::new())),
        Yaml::Mapping(map) => Ok(yaml_to_json(&Yaml::Mapping(map))),
        other => Err(FrontmatterError {
            message: format!(
                "Frontmatter must be a YAML mapping, got {}",
                yaml_type_name(&other)
            ),
            line: Some(0),
        }),
    }
}

pub(crate) fn yaml_type_name(v: &Yaml) -> &'static str {
    match v {
        Yaml::Null => "NoneType",
        Yaml::Bool(_) => "bool",
        Yaml::Number(n) => {
            if n.is_i64() || n.is_u64() {
                "int"
            } else {
                "float"
            }
        }
        Yaml::String(_) => "str",
        Yaml::Sequence(_) => "list",
        Yaml::Mapping(_) => "dict",
        Yaml::Tagged(_) => "str",
    }
}

/// Fence-aware `## ` section extraction, the port of `_extract_sections`.
fn extract_sections(body: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut in_fence = false;
    let mut fence_marker = String::new();

    let flush = |heading: &Option<String>, lines: &Vec<&str>, out: &mut Vec<(String, String)>| {
        if let Some(h) = heading {
            let trimmed = lines.join("\n").trim().to_string();
            // A repeated heading keeps its first position and takes the last
            // body, the dict-assignment answer the Python parser gave.
            match out.iter_mut().find(|(seen, _)| seen == h) {
                Some(slot) => slot.1 = trimmed,
                None => out.push((h.clone(), trimmed)),
            }
        }
    };

    for raw_line in body.lines() {
        let stripped = raw_line.trim();
        if !in_fence {
            if stripped.starts_with("```") || stripped.starts_with("~~~") {
                in_fence = true;
                fence_marker = stripped.chars().take(3).collect();
                current_lines.push(raw_line);
                continue;
            }
            if raw_line.starts_with("## ") {
                flush(&current_heading, &current_lines, &mut sections);
                current_heading = Some(raw_line[3..].trim_end().to_string());
                current_lines.clear();
                continue;
            }
            current_lines.push(raw_line);
        } else {
            current_lines.push(raw_line);
            let same_or_longer_close = stripped == fence_marker
                || (stripped.starts_with(&fence_marker)
                    && stripped
                        .trim_matches(fence_marker.chars().next().unwrap_or('`'))
                        .is_empty());
            if same_or_longer_close {
                in_fence = false;
                fence_marker.clear();
            }
        }
    }
    flush(&current_heading, &current_lines, &mut sections);
    sections
}

/// Parse plan text (frontmatter + body) without touching disk, the port of
/// `load_plan_text`.
pub fn load_plan_text(text: &str) -> Result<PlanDocument, FrontmatterError> {
    let (yaml_text, body) = split_frontmatter(text);
    let frontmatter = parse_frontmatter(&yaml_text)?;
    let sections = extract_sections(body);
    Ok(PlanDocument {
        frontmatter,
        sections,
    })
}

/// Parse the plan file at `path`, the port of `load_plan`. Read errors
/// surface as a frontmatter-shaped error naming the path, one error channel
/// for door callers.
pub fn load_plan(path: &std::path::Path) -> Result<PlanDocument, FrontmatterError> {
    let text = std::fs::read_to_string(path).map_err(|e| FrontmatterError {
        message: format!("[Errno {}] {}", e.raw_os_error().unwrap_or(0), e),
        line: None,
    })?;
    load_plan_text(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_sections_in_order() {
        let text = "---\nstatus: design\nnode: x-1234\ntags: [a, b]\n---\n\n## Overview\n\nFirst body.\n\n## Execution Strategy\n\n```yaml\nwaves: []\n```\n\n## Deferred\n\nlater\n";
        let doc = load_plan_text(text).unwrap();
        assert_eq!(doc.frontmatter["status"], "design");
        assert_eq!(doc.frontmatter["node"], "x-1234");
        let names: Vec<&str> = doc.sections.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(names, vec!["Overview", "Execution Strategy", "Deferred"]);
        assert_eq!(doc.get_section("Overview").unwrap(), "First body.");
        assert!(doc
            .get_section("Execution Strategy")
            .unwrap()
            .contains("waves: []"));
        assert!(doc.has_section("Deferred"));
        assert!(!doc.has_section("Missing"));
    }

    #[test]
    fn a_heading_inside_a_fence_stays_body() {
        let text = "---\nstatus: design\n---\n\n## Context\n\n```md\n## Not A Section\n```\n\n## Changes\n\nreal\n";
        let doc = load_plan_text(text).unwrap();
        let names: Vec<&str> = doc.sections.iter().map(|(h, _)| h.as_str()).collect();
        assert_eq!(names, vec!["Context", "Changes"]);
        assert!(doc
            .get_section("Context")
            .unwrap()
            .contains("## Not A Section"));
    }

    #[test]
    fn no_frontmatter_is_all_body_and_frontmatter_must_be_a_mapping() {
        let doc = load_plan_text("## Overview\n\nbody").unwrap();
        assert!(doc.sections.len() == 1);
        assert!(doc.frontmatter.as_object().unwrap().is_empty());

        let err = load_plan_text("---\n- a\n- b\n---\nbody").unwrap_err();
        assert!(err.message.contains("Frontmatter must be a YAML mapping"));
    }

    #[test]
    fn malformed_yaml_names_a_line() {
        let err = load_plan_text("---\nstatus: [unclosed\n---\nbody").unwrap_err();
        assert!(!err.message.is_empty());
    }

    #[test]
    fn an_empty_frontmatter_block_reads_as_empty_mapping() {
        let doc = load_plan_text("---\n---\n\n## A\n\nb\n").unwrap();
        assert!(doc.frontmatter.as_object().unwrap().is_empty());
        assert_eq!(doc.get_section("A").unwrap(), "b");
    }
}

#[cfg(test)]
#[path = "doc_tests.rs"]
mod contract_tests;
