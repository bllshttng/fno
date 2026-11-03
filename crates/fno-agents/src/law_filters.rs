//! Rule filters: where a law applies, beside its scope and its guard.
//!
//! A law row may carry `filters: {<key>: <value>, not_<key>: <value>}`. Every
//! filter must match for the law to apply, and a law with no filter applies
//! everywhere. A value is one choice or a comma-separated list of choices. A
//! `branch` choice ending in `*` matches by prefix.
//!
//! A filter reads a fact about the reading session. A session that cannot
//! name the fact fails open, the posture every law reader keeps. A key with no
//! fact source today is refused at record time and names the node that will
//! supply it, so a law never stores a filter nothing can enforce.

use serde_json::Value;

/// Keys a session can answer today, with where the answer comes from.
pub const SOURCED_KEYS: &[(&str, &str)] = &[
    ("node", "FNO_NODE"),
    ("epic", "the parent of the FNO_NODE node"),
    ("project", "the project of the FNO_NODE node"),
    ("priority", "the priority of the FNO_NODE node"),
    ("size", "the size of the FNO_NODE node"),
    ("difficulty", "the difficulty of the FNO_NODE node"),
    ("domain", "the domain of the FNO_NODE node"),
    ("harness", "FNO_AGENT_HARNESS"),
    ("provider", "FNO_ROUTE_PROVIDER"),
    ("model", "FNO_ROUTE_MODEL"),
    ("branch", "the checked-out branch"),
];

/// Keys the spec names that no session fact backs yet, with the node that
/// will supply the source.
pub const UNSOURCED_KEYS: &[(&str, &str)] = &[
    ("lead", "x-ffc5"),
    ("role", "x-ffc5"),
    ("lane", "x-5428"),
    ("pr_label", "x-5428"),
    ("pr_over_lines", "x-5428"),
];

/// Parse `key=value` for a record flag: the key must be sourced, and the
/// `not_` form is the caller's job (the flag decides it, not the key).
pub fn parse_pair(spec: &str) -> Result<(String, String), String> {
    let Some((key, value)) = spec.split_once('=') else {
        return Err(format!("{spec:?} is not key=value"));
    };
    let (key, value) = (key.trim(), value.trim());
    if value.is_empty() {
        return Err(format!("filter {key} needs a value"));
    }
    if SOURCED_KEYS.iter().any(|(k, _)| *k == key) {
        return Ok((key.to_string(), value.to_string()));
    }
    if let Some((_, node)) = UNSOURCED_KEYS.iter().find(|(k, _)| *k == key) {
        return Err(format!(
            "filter {key} has no identity source yet; node {node} supplies it"
        ));
    }
    let known: Vec<&str> = SOURCED_KEYS.iter().map(|(k, _)| *k).collect();
    Err(format!(
        "no such filter {key}; filters are {}",
        known.join(", ")
    ))
}

fn choice_matches(key: &str, choice: &str, have: &str) -> bool {
    let choice = choice.trim();
    if key == "branch" {
        if let Some(prefix) = choice.strip_suffix('*') {
            return have.starts_with(prefix);
        }
    }
    choice == have
}

fn any_choice(key: &str, want: &str, have: &str) -> bool {
    want.split(',').any(|c| choice_matches(key, c, have))
}

/// The matcher. `fact` answers one key for this session, `None` when it
/// cannot. A missing fact passes the filter, positive or not.
pub fn filters_match_with(row: &Value, fact: &mut dyn FnMut(&str) -> Option<String>) -> bool {
    let Some(filters) = row.get("filters").and_then(Value::as_object) else {
        return true;
    };
    for (name, want) in filters {
        let Some(want) = want.as_str() else { continue };
        let (negated, key) = match name.strip_prefix("not_") {
            Some(k) => (true, k),
            None => (false, name.as_str()),
        };
        let Some(have) = fact(key) else { continue };
        if any_choice(key, want, &have) == negated {
            return false;
        }
    }
    true
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// The checked-out branch of `dir`, read from the HEAD file so a law read
/// never shells out. A linked worktree's `.git` is a file naming its gitdir.
pub fn branch_of(dir: &std::path::Path) -> Option<String> {
    let dot_git = dir
        .ancestors()
        .map(|d| d.join(".git"))
        .find(|p| p.exists())?;
    let head_dir = if dot_git.is_file() {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.trim().strip_prefix("gitdir:")?.trim().to_string();
        let target = std::path::PathBuf::from(target);
        if target.is_absolute() {
            target
        } else {
            dot_git.parent()?.join(target)
        }
    } else {
        dot_git
    };
    let head = std::fs::read_to_string(head_dir.join("HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

fn node_entry() -> &'static Option<Value> {
    static ENTRY: std::sync::OnceLock<Option<Value>> = std::sync::OnceLock::new();
    ENTRY.get_or_init(|| {
        let id = nonempty(std::env::var("FNO_NODE").ok())?;
        let entries = crate::backlog::decisions_cli::read_graph_entries().ok()?;
        entries
            .into_iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(id.as_str()))
    })
}

fn node_field(field: &str) -> Option<String> {
    let value = node_entry().as_ref()?.get(field)?;
    nonempty(value.as_str().map(str::to_string))
}

/// The live facts of this session, read lazily so a row with no filter
/// costs nothing.
pub fn session_fact(key: &str) -> Option<String> {
    match key {
        "node" => nonempty(std::env::var("FNO_NODE").ok()),
        "epic" => node_field("parent"),
        "project" | "priority" | "size" | "difficulty" | "domain" => node_field(key),
        "harness" => nonempty(std::env::var("FNO_AGENT_HARNESS").ok())
            .or_else(|| nonempty(std::env::var("FNO_AGENT_PROVIDER").ok())),
        "provider" => nonempty(std::env::var("FNO_ROUTE_PROVIDER").ok()),
        "model" => nonempty(std::env::var("FNO_ROUTE_MODEL").ok()),
        "branch" => branch_of(&std::env::current_dir().ok()?),
        _ => None,
    }
}

pub fn filters_match(row: &Value) -> bool {
    filters_match_with(row, &mut session_fact)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(row: Value, facts: &[(&str, &str)]) -> bool {
        let mut fact = |k: &str| {
            facts
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
        };
        filters_match_with(&row, &mut fact)
    }

    #[test]
    fn every_filter_must_match_and_the_not_form_excludes() {
        let row = json!({"filters": {"harness": "codex", "not_model": "glm-5.3-flash", "priority": "p0,p1"}});
        let ok = [
            ("harness", "codex"),
            ("model", "gpt-6.1-sol"),
            ("priority", "p1"),
        ];
        assert!(run(row.clone(), &ok));
        assert!(!run(
            row.clone(),
            &[
                ("harness", "claude"),
                ("model", "gpt-6.1-sol"),
                ("priority", "p1")
            ]
        ));
        assert!(!run(
            row.clone(),
            &[
                ("harness", "codex"),
                ("model", "glm-5.3-flash"),
                ("priority", "p1")
            ]
        ));
        assert!(!run(
            row,
            &[
                ("harness", "codex"),
                ("model", "gpt-6.1-sol"),
                ("priority", "p2")
            ]
        ));
    }

    #[test]
    fn a_missing_fact_and_a_missing_filter_both_pass() {
        let row = json!({"filters": {"epic": "x-aaaa", "not_provider": "zai"}});
        assert!(run(row, &[]));
        assert!(run(json!({}), &[("epic", "x-bbbb")]));
    }

    #[test]
    fn a_branch_choice_may_end_in_a_star() {
        let row = json!({"filters": {"branch": "release/*"}});
        assert!(run(row.clone(), &[("branch", "release/0.4.5")]));
        assert!(!run(row, &[("branch", "feature/x")]));
    }

    #[test]
    fn a_pair_names_its_source_gap() {
        assert_eq!(
            parse_pair("harness=codex").unwrap(),
            ("harness".into(), "codex".into())
        );
        assert!(parse_pair("lead=rowan").unwrap_err().contains("x-ffc5"));
        assert!(parse_pair("pr_label=paint").unwrap_err().contains("x-5428"));
        assert!(parse_pair("colour=red")
            .unwrap_err()
            .contains("no such filter"));
        assert!(parse_pair("harness").is_err());
    }

    #[test]
    fn branch_reads_a_plain_and_a_linked_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("plain");
        std::fs::create_dir_all(plain.join(".git")).unwrap();
        std::fs::write(plain.join(".git/HEAD"), "ref: refs/heads/feature/a\n").unwrap();
        assert_eq!(branch_of(&plain).as_deref(), Some("feature/a"));
        let gitdir = dir.path().join("gitdirs/w");
        std::fs::create_dir_all(&gitdir).unwrap();
        std::fs::write(gitdir.join("HEAD"), "ref: refs/heads/release/1\n").unwrap();
        let linked = dir.path().join("linked");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\n", gitdir.display()),
        )
        .unwrap();
        assert_eq!(branch_of(&linked).as_deref(), Some("release/1"));
    }
}
