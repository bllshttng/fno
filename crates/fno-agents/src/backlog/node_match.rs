//! Loose node matching for the commands a person types: exact id, the `ab-`
//! id prefix, the title substring, and the git-branch derivation. The matcher
//! never picks one of many: ambiguity is its own answer. The deterministic
//! tiers (id, slug, bare hex) stay in `node_ref::resolve_tiers`.

use serde_json::{json, Value};

const BRANCH_PREFIXES: [&str; 15] = [
    "feat/",
    "feature/",
    "fno/",
    "fix/",
    "hotfix/",
    "bugfix/",
    "docs/",
    "chore/",
    "refactor/",
    "test/",
    "ci/",
    "perf/",
    "build/",
    "style/",
    "revert/",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchKind {
    Exact,
    Fuzzy,
    BranchDerived,
    None,
    Ambiguous,
}

impl MatchKind {
    fn word(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Fuzzy => "fuzzy",
            Self::BranchDerived => "branch_derived",
            Self::None => "none",
            Self::Ambiguous => "ambiguous",
        }
    }
}

pub(crate) struct NodeMatch {
    pub kind: MatchKind,
    pub id: Option<String>,
    pub candidates: Vec<Value>,
    pub note: String,
}

fn none(note: String) -> NodeMatch {
    NodeMatch {
        kind: MatchKind::None,
        id: None,
        candidates: Vec::new(),
        note,
    }
}

fn id_of(entry: &Value) -> Option<String> {
    entry.get("id").and_then(Value::as_str).map(str::to_string)
}

fn title_of(entry: &Value) -> &str {
    entry.get("title").and_then(Value::as_str).unwrap_or("")
}

fn branch_tokens(branch: &str) -> Vec<String> {
    let lowered = branch.to_lowercase();
    let stripped = BRANCH_PREFIXES
        .iter()
        .find(|p| lowered.starts_with(*p))
        .map_or(lowered.as_str(), |p| &lowered[p.len()..]);
    stripped
        .replace(['_', '-'], " ")
        .split_whitespace()
        .filter(|t| t.len() > 2 && !t.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_string)
        .collect()
}

fn all_tokens_hit(tokens: &[String], entries: &[&Value]) -> Vec<Value> {
    entries
        .iter()
        .filter(|e| {
            let title = title_of(e).to_lowercase();
            tokens.iter().all(|t| title.contains(t.as_str()))
        })
        .map(|e| (*e).clone())
        .collect()
}

fn best_scored(tokens: &[String], entries: &[&Value]) -> Vec<Value> {
    let score = |e: &Value| {
        let title = title_of(e).to_lowercase();
        tokens.iter().filter(|t| title.contains(t.as_str())).count()
    };
    let top = entries.iter().map(|e| score(e)).max().unwrap_or(0);
    if top == 0 {
        return Vec::new();
    }
    entries
        .iter()
        .filter(|e| score(e) == top)
        .map(|e| (*e).clone())
        .collect()
}

fn split_by_status(entries: &[Value]) -> (Vec<&Value>, Vec<&Value>) {
    entries
        .iter()
        .partition(|e| e.get("status").and_then(Value::as_str) != Some("done"))
}

fn build(matches: Vec<Value>, single: MatchKind, prefix: &str) -> NodeMatch {
    match matches.len() {
        0 => none(format!("{prefix}: no matches")),
        1 => NodeMatch {
            kind: single,
            id: id_of(&matches[0]),
            note: format!("{prefix}: matched '{}'", title_of(&matches[0])),
            candidates: matches,
        },
        n => NodeMatch {
            kind: MatchKind::Ambiguous,
            id: id_of(&matches[0]),
            candidates: matches,
            note: format!("{prefix}: {n} candidates"),
        },
    }
}

/// Exact id first; an empty query plus a branch derives title tokens from the
/// branch; any other query is a token-subset title match. Open rows outrank
/// done rows on every title tier.
pub(crate) fn resolve_id(
    query: Option<&str>,
    entries: &[Value],
    branch: Option<&str>,
) -> NodeMatch {
    let q = query.unwrap_or("").trim();
    if q.starts_with('-') {
        return none(format!("flag-shaped query '{q}' is not a node id"));
    }
    if let Some(hit) = entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(q))
    {
        return NodeMatch {
            kind: MatchKind::Exact,
            id: Some(q.to_string()),
            candidates: vec![hit.clone()],
            note: format!("exact id match: {}", title_of(hit)),
        };
    }
    if let Some(suffix) = q.strip_prefix("ab-") {
        let hex = suffix
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        if hex && suffix.len() == 8 {
            return none(format!("no entry with id '{q}'"));
        }
        if hex && (4..=7).contains(&suffix.len()) {
            let matches: Vec<Value> = entries
                .iter()
                .filter(|e| {
                    e.get("id")
                        .and_then(Value::as_str)
                        .is_some_and(|id| id.starts_with(q))
                })
                .cloned()
                .collect();
            return build(matches, MatchKind::Fuzzy, &format!("id prefix '{q}'"));
        }
        return none(format!(
            "malformed ab- query '{q}' (suffix must be 4-8 hex chars)"
        ));
    }
    let (open, done) = split_by_status(entries);
    if q.is_empty() {
        let Some(branch) = branch.filter(|b| !b.is_empty()) else {
            return none("empty query and no git_branch provided".to_string());
        };
        let tokens = branch_tokens(branch);
        if tokens.is_empty() {
            return none(format!("branch '{branch}' produced no searchable tokens"));
        }
        let mut matches = best_scored(&tokens, &open);
        if matches.is_empty() {
            matches = best_scored(&tokens, &done);
        }
        return build(
            matches,
            MatchKind::BranchDerived,
            &format!("branch '{branch}'"),
        );
    }
    let tokens: Vec<String> = q
        .to_lowercase()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut matches = all_tokens_hit(&tokens, &open);
    if matches.is_empty() {
        matches = all_tokens_hit(&tokens, &done);
    }
    build(matches, MatchKind::Fuzzy, &format!("query '{q}'"))
}

/// `fno backlog script-door match [<query>] [--branch B]` prints one JSON match
/// and exits 0 on an exact or single match, 1 on none or ambiguous, 3 on an
/// unreadable graph.
pub fn run(tail: &[String]) -> i32 {
    let mut query: Option<&str> = None;
    let mut branch: Option<&str> = None;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        if arg == "--branch" {
            branch = it.next().map(String::as_str);
        } else if query.is_none() {
            query = Some(arg);
        } else {
            eprintln!("usage: fno backlog script-door match [<query>] [--branch B]");
            return 2;
        }
    }
    let graph = super::settings::graph_path();
    let rows = match crate::graph_store::read_rows(&graph) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno backlog script-door match: the graph could not be read: {e}");
            return super::get_cli::GRAPH_UNREADABLE_EXIT;
        }
    };
    let live: Vec<Value> = rows
        .into_iter()
        .filter(|r| r.get("archived_at").is_none_or(Value::is_null))
        .collect();
    // The deterministic tiers answer first: slug, alias and bare hex are exact.
    let exact = query.and_then(|q| super::node_ref::resolve_tiers(&live, q));
    let found = match exact {
        Some(hit) => NodeMatch {
            kind: MatchKind::Exact,
            id: id_of(hit),
            candidates: vec![hit.clone()],
            note: String::new(),
        },
        None => resolve_id(query, &live, branch),
    };
    println!(
        "{}",
        json!({
            "kind": found.kind.word(),
            "id": found.id,
            "candidates": found.candidates.iter().map(|c| json!({
                "id": c.get("id"), "status": c.get("status"), "title": c.get("title"),
            })).collect::<Vec<_>>(),
            "note": found.note,
        })
    );
    i32::from(matches!(found.kind, MatchKind::None | MatchKind::Ambiguous))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Value> {
        vec![
            json!({"id": "x-aaaa", "title": "Port the graph core", "status": "ready"}),
            json!({"id": "x-bbbb", "title": "Port the graph store", "status": "done"}),
            json!({"id": "x-cccc", "title": "Fix login", "status": "ready"}),
            json!({"id": "x-dddd", "title": "Fix logout", "status": "ready"}),
        ]
    }

    #[test]
    fn an_exact_id_wins_and_a_title_prefers_the_open_row() {
        let exact = resolve_id(Some("x-cccc"), &rows(), None);
        assert_eq!(exact.kind, MatchKind::Exact);
        let open = resolve_id(Some("port graph"), &rows(), None);
        assert_eq!(open.kind, MatchKind::Fuzzy);
        assert_eq!(open.id.as_deref(), Some("x-aaaa"));
    }

    #[test]
    fn two_equal_matches_are_ambiguous_and_a_flag_never_matches() {
        assert_eq!(
            resolve_id(Some("fix"), &rows(), None).kind,
            MatchKind::Ambiguous
        );
        assert_eq!(
            resolve_id(Some("--list"), &rows(), None).kind,
            MatchKind::None
        );
    }
}
