//! Rule filters: where a law applies, beside its scope and its guard.
//!
//! A law row may carry `filters: {<key>: <value>, not_<key>: <value>}`. Every
//! filter must match for the law to apply, and a law with no filter applies
//! everywhere. A value is one choice or a comma-separated list of choices. A
//! `branch` choice ending in `*` matches by prefix.
//!
//! A filter reads a fact about the reading session. A session that cannot
//! name the fact fails open, the posture every law reader keeps. A key with no
//! fact source today is refused at record time, so a law never stores a
//! filter nothing can enforce.

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
    (
        "role",
        "lead when the session holds a live team, else worker; or the session handle",
    ),
    (
        "lead",
        "the holder of the live team whose territory covers the FNO_NODE node",
    ),
];

/// Keys the spec names that no session fact backs yet. The door refuses
/// them, so a law never stores a filter nothing can enforce.
pub const UNSOURCED_KEYS: &[&str] = &["lane", "pr_label", "pr_over_lines"];

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
    if UNSOURCED_KEYS.contains(&key) {
        return Err(format!(
            "filter {key} has no identity source yet, so the door cannot enforce it"
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

/// The matcher. `fact` answers one key for this session with every name the
/// session goes by, `None` when it cannot. A missing fact passes the filter,
/// positive or not.
pub fn filters_match_with(row: &Value, fact: &mut dyn FnMut(&str) -> Option<Vec<String>>) -> bool {
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
        if have.iter().any(|h| any_choice(key, want, h)) == negated {
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

fn live_teams() -> &'static Option<Vec<crate::territory::Team>> {
    static TEAMS: std::sync::OnceLock<Option<Vec<crate::territory::Team>>> =
        std::sync::OnceLock::new();
    TEAMS.get_or_init(|| {
        let registry = crate::paths::AgentsHome::from_env().registry_json();
        crate::territory::live_teams(&registry).ok()
    })
}

fn holds(team: &crate::territory::Team, handle: &str) -> bool {
    team.holder_session
        .as_deref()
        .is_some_and(|s| crate::identity::canonical_handle(s) == handle)
}

/// `lead` when the session holds a live team, `worker` otherwise, and the
/// session handle, so a rule can name one session.
fn role_names() -> Option<Vec<String>> {
    let handle = crate::decide_door::ambient_agent_handle()?;
    let is_lead = live_teams()
        .as_ref()
        .is_some_and(|teams| teams.iter().any(|t| holds(t, &handle)));
    let role = if is_lead { "lead" } else { "worker" };
    Some(vec![role.to_string(), handle])
}

/// The holder of every live team whose territory covers this session's node,
/// by name and by session handle. A session with no node, or a node no team
/// covers, has no lead to name.
fn lead_names() -> Option<Vec<String>> {
    let node = nonempty(std::env::var("FNO_NODE").ok())?;
    let teams = live_teams().as_ref()?;
    let entries = crate::backlog::decisions_cli::read_graph_entries().ok()?;
    let projects = std::env::current_dir()
        .map_err(|e| e.to_string())
        .and_then(|cwd| crate::org_board::scope::project_map(&cwd));
    let mut names: Vec<String> = Vec::new();
    for team in teams {
        if crate::decide_door::role_covers_subject(
            std::slice::from_ref(team),
            &node,
            &entries,
            &projects,
        ) {
            names.push(team.holder.clone());
            if let Some(s) = team.holder_session.as_deref() {
                names.push(crate::identity::canonical_handle(s));
            }
        }
    }
    (!names.is_empty()).then_some(names)
}

/// The live facts of this session, read lazily so a row with no filter
/// costs nothing.
pub fn session_fact(key: &str) -> Option<Vec<String>> {
    let one = |v: Option<String>| v.map(|s| vec![s]);
    match key {
        "node" => one(nonempty(std::env::var("FNO_NODE").ok())),
        "epic" => one(node_field("parent")),
        "project" | "priority" | "size" | "difficulty" | "domain" => one(node_field(key)),
        "harness" => one(nonempty(std::env::var("FNO_AGENT_HARNESS").ok())
            .or_else(|| nonempty(std::env::var("FNO_AGENT_PROVIDER").ok()))),
        "provider" => one(nonempty(std::env::var("FNO_ROUTE_PROVIDER").ok())),
        "model" => one(nonempty(std::env::var("FNO_ROUTE_MODEL").ok())),
        "branch" => one(branch_of(&std::env::current_dir().ok()?)),
        "role" => role_names(),
        "lead" => lead_names(),
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
            let have: Vec<String> = facts
                .iter()
                .filter(|(key, _)| *key == k)
                .map(|(_, v)| v.to_string())
                .collect();
            (!have.is_empty()).then_some(have)
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
        let names = json!({"filters": {"role": "lead", "not_lead": "rowan"}});
        assert!(run(
            names.clone(),
            &[("role", "lead"), ("role", "ab12cd34"), ("lead", "mina")]
        ));
        assert!(!run(
            names.clone(),
            &[("role", "worker"), ("role", "ab12cd34")]
        ));
        assert!(!run(
            names,
            &[("role", "lead"), ("lead", "rowan"), ("lead", "ef56ab78")]
        ));
        let branch = json!({"filters": {"branch": "release/*"}});
        assert!(run(branch.clone(), &[("branch", "release/0.4.5")]));
        assert!(!run(branch, &[("branch", "feature/x")]));
    }

    #[test]
    fn a_missing_fact_and_a_missing_filter_both_pass() {
        let row = json!({"filters": {"epic": "x-aaaa", "not_provider": "zai"}});
        assert!(run(row, &[]));
        assert!(run(json!({}), &[("epic", "x-bbbb")]));
    }
}
