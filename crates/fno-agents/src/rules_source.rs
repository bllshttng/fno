//! Where the rules decide() reads come from: the shipped default table, the
//! project rule files (a tracked `.fno-rules/` dot-dir, the codex `rules/`
//! precedent), and the live laws with their imported triggers. An unreadable
//! store is `Err` naming the path; every other source degrades to an
//! `unread` entry and the rest still ship, so a caller renders `Unread:`
//! lines, never zero-rule silence.

use crate::decision_index;
use fno::rules::{parse_file, Rule};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The shipped default table, beside `event_rules.toml`. Project-neutral
/// rows every install gets; the shape follows how codex and opencode define
/// rules (ruling d-6fb65dbc): structural pattern in, one decision verb out.
/// `event_rules.<id>` config flips any row, the Stop table's namespace.
pub(crate) const SHIPPED: &str = include_str!("rules-defaults.toml");

/// The assembled rule set decide() reads. `version` folds the law count and
/// the trigger count, so a future resident host can cache on it.
pub struct RuleSet {
    pub rules: Vec<Rule>,
    /// The law subject behind each law-sourced rule id, for the stage lines.
    pub subjects: BTreeMap<String, String>,
    /// Per-source reasons a source answered nothing; callers render `Unread:`.
    pub unread: Vec<String>,
    pub version: String,
}

/// The Route 3 seam: the set builds in process on every fire. A resident
/// host (the keeper socket) slots in here when the release gate measures
/// the build over budget, and nothing else changes.
pub fn rule_set_fast(cwd: &Path) -> Result<RuleSet, String> {
    rule_set(cwd)
}

pub fn rule_set(cwd: &Path) -> Result<RuleSet, String> {
    let graph = crate::graph_get::default_graph_path();
    let jsonl = decision_index::default_state_path("decisions.jsonl");
    let (rows, damaged) = decision_index::read_store_rows(&graph, &jsonl)?;
    let laws = decision_index::laws_of(decision_index::derive_rows(rows, damaged));
    let mut unread = Vec::new();
    let triggers = match decision_index::read_law_triggers(&jsonl) {
        Ok(t) => t,
        Err(reason) => {
            unread.push(format!("the law_trigger journal ({reason})"));
            BTreeMap::new()
        }
    };

    let mut rules = Vec::new();
    let mut subjects: BTreeMap<String, String> = BTreeMap::new();

    // (a) The shipped table, with `event_rules.<id>` config flips.
    let shipped = parse_file(SHIPPED).map_err(|e| format!("the shipped rule table: {e}"))?;
    let flips = event_rule_flips(cwd);
    for rule in shipped.rule {
        if flips.get(&rule.id).copied().unwrap_or(rule.default) {
            rules.push(rule);
        }
    }

    // (b) The project's own rule files.
    if let Some(dir) = project_rules_dir(cwd) {
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().map(|x| x == "toml").unwrap_or(false))
                .collect(),
            Err(reason) => {
                unread.push(format!("the project rule dir ({reason})"));
                Vec::new()
            }
        };
        files.sort();
        for file in files {
            match std::fs::read_to_string(&file)
                .map_err(|e| e.to_string())
                .and_then(|text| fno::rules::parse_table(&text))
            {
                Ok(mut r) => rules.append(&mut r),
                Err(e) => unread.push(format!("{} ({e})", file.display())),
            }
        }
    }

    // (c) The live laws: an imported trigger wins, a `paths` row becomes a
    // pre-tool path rule, and a law with neither is the implicit tier-1
    // inform rule (the STAGES keyword fallback's replacement).
    for row in &laws.rows {
        let Some(id) = row.get("decision_id").and_then(Value::as_str) else {
            continue;
        };
        let subject = row.get("subject").and_then(Value::as_str).unwrap_or("");
        subjects.insert(id.to_string(), subject.to_string());
        if let Some(tr) = triggers.get(id) {
            match tr.get("trigger_kind").and_then(Value::as_str) {
                // grant, retire and superseded-pair are record-keeping; the
                // live-laws read already applied the retirement. A native
                // family yields no rule: the door's own probes read those rows.
                Some("disposition") => {}
                Some("rule") => match imported_rule(tr) {
                    Some(mut r) => {
                        cap_law_deny(&mut r);
                        rules.push(r);
                    }
                    None => unread.push(format!("law {id}: its imported rule row does not parse")),
                },
                _ => unread.push(format!("law {id}: unknown trigger_kind")),
            }
            continue;
        }
        let paths = row_paths(row);
        let scope = decision_index::row_scope(row);
        let scope_line = if scope == "global" {
            String::new()
        } else {
            format!("\nscope = \"{}\"", toml_escape(scope))
        };
        let body = if paths.is_empty() {
            format!(
                "id = \"law-{id}\"\nlaw = \"{id}\"{scope_line}\naction = \"inform\"\n[rule.when]\nretrieval = \"{}\"\nevents = [\"prompt\", \"skill\"]",
                toml_escape(&format!(
                    "{subject} {}",
                    law_decision(row)
                )),
            )
        } else {
            let globs = paths
                .iter()
                .map(|p| format!("\"{}\"", toml_escape(p)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "id = \"law-{id}\"\nlaw = \"{id}\"{scope_line}\naction = \"inform\"\n[rule.when]\nevents = [\"pre-tool\"]\ntools = [\"Edit\", \"Write\"]\npaths = [{globs}]",
            )
        };
        match toml_rule(&body) {
            Some(r) => rules.push(r),
            None => unread.push(format!("law {id}: its implicit rule row does not parse")),
        }
    }

    // `until <node> closes` resolves at build: a closed node drops its rule.
    rules.retain(|r| !until_node_closed(r));

    let version = format!("laws={};triggers={}", laws.rows.len(), triggers.len());
    Ok(RuleSet {
        rules,
        subjects,
        unread,
        version,
    })
}

/// One config read per assembly: the whole `event_rules` table, the same
/// namespace `event_rules.rs` reads for the Stop rows.
fn event_rule_flips(cwd: &Path) -> std::collections::HashMap<String, bool> {
    let mut out = std::collections::HashMap::new();
    if let Some(table) = crate::agents_config::config_value_deep(cwd, &["event_rules"]) {
        if let Some(map) = table.as_table() {
            for (key, value) in map {
                if let Some(flag) = value.as_bool() {
                    out.insert(key.clone(), flag);
                }
            }
        }
    }
    out
}

/// The rule row an imported `law_trigger` event carries, parsed and popped.
fn imported_rule(trigger: &Value) -> Option<Rule> {
    let text = trigger.get("rule_toml").and_then(Value::as_str)?;
    let mut file = parse_file(&format!("[[rule]]\n{text}")).ok()?;
    file.rule.pop()
}

/// Law-sourced deny rules wait until x-3284 lifts the cap per family: the
/// engine ships without changing what binds today (Handoff answer 1). The
/// class clears too, so it derives from the warn action.
fn cap_law_deny(rule: &mut Rule) {
    if rule.law.is_some() && matches!(rule.action.as_str(), "block" | "require-ack") {
        rule.action = "warn".to_string();
        rule.class = None;
    }
}

fn row_paths(row: &Value) -> Vec<String> {
    row.get("paths")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn law_decision(row: &Value) -> &str {
    row.get("decision").and_then(Value::as_str).unwrap_or("")
}

fn toml_rule(text: &str) -> Option<Rule> {
    let mut file = parse_file(&format!("[[rule]]\n{text}")).ok()?;
    file.rule.pop()
}

/// A TOML basic-string body: the two escapes a law subject or decision can
/// carry. Control characters do not survive the record door, so no more.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// `until <node> closes` resolves at build time: a node close is a store
/// write, so a closed node drops its rule until the ruling is renewed. An
/// unreadable graph keeps the rule (the ruling source is explicit).
fn until_node_closed(rule: &Rule) -> bool {
    let Some(expires) = &rule.expires else {
        return false;
    };
    let Some(node) = expires
        .strip_prefix("until ")
        .and_then(|rest| rest.strip_suffix(" closes"))
        .map(str::trim)
    else {
        return false;
    };
    crate::backlog::api::node(
        &crate::backlog::api::Store::new(&crate::graph_get::default_graph_path()),
        node,
    )
    .ok()
    .flatten()
    .map(|n| n.completed_at.is_some())
    .unwrap_or(false)
}

/// Where a project's own rules live: a tracked dot-dir (`.fno/` is
/// gitignored, so it cannot hold them), read by every harness through decide.
pub fn project_rules_dir(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        if dir.join(".git").exists() {
            return Some(dir.join(".fno-rules"));
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_shipped_defaults_parse_and_validate() {
        let rules = fno::rules::parse_table(SHIPPED).expect("the shipped table validates");
        assert!(!rules.is_empty());
        assert!(rules.iter().all(|r| r.problems().is_empty()));
    }

    #[test]
    fn an_unreadable_store_is_err_naming_the_paths() {
        let lock = crate::claims::test_env_lock();
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", td.path().join("fno-home"));
        let err = rule_set(Path::new("/")).expect_err("no store at all");
        assert!(err.contains("no decision store"), "{err}");
        match prev {
            Some(v) => std::env::set_var("FNO_HOME", v),
            None => std::env::remove_var("FNO_HOME"),
        }
    }

    fn envelope(ts: &str, data: Value) -> String {
        json!({"ts": ts, "type": "operator_decision", "source": "backlog", "data": data})
            .to_string()
    }

    #[test]
    fn a_triggered_law_and_a_plain_law_yield_their_rules() {
        let lock = crate::claims::test_env_lock();
        let _held = lock.lock().unwrap_or_else(|e| e.into_inner());
        let td = tempfile::TempDir::new().unwrap();
        let prev = std::env::var_os("FNO_HOME");
        std::env::set_var("FNO_HOME", td.path().join("fno-home"));
        let jsonl = td.path().join("fno-home").join("decisions.jsonl");
        std::fs::create_dir_all(jsonl.parent().unwrap()).unwrap();
        let mut text = envelope(
            "2026-10-24T00:00:00Z",
            json!({
                "decision_id": "d-aaaa1111",
                "subject": "graph writes",
                "decision": "write the graph through fno backlog only",
                "authority_source": "operator",
                "scope": "project:fno",
            }),
        );
        text.push('\n');
        text.push_str(&envelope(
            "2026-10-24T01:00:00Z",
            json!({
                "decision_id": "d-bbbb2222",
                "subject": "release tags",
                "decision": "no release tag until the TUI is proven",
                "authority_source": "chat_attested",
                "scope": "global",
            }),
        ));
        text.push('\n');
        text.push_str(
            &json!({
                "ts": "2026-10-24T02:00:00Z",
                "type": "law_trigger",
                "source": "backlog",
                "data": {
                    "decision_id": "d-aaaa1111",
                    "trigger_kind": "rule",
                    "rule_toml": "id = \"law-triggered\"\nlaw = \"d-aaaa1111\"\ntext = \"graph writes\"\naction = \"warn\"\nscope = \"project:fno\"\n[rule.when]\nevents = [\"pre-tool\"]\ntools = [\"Bash\"]\ncommands = [\"cargo build*\"]",
                },
            })
            .to_string(),
        );
        text.push('\n');
        std::fs::write(&jsonl, text).unwrap();

        let set = rule_set(Path::new("/")).expect("the jsonl store reads");
        let find = |id: &str| {
            set.rules
                .iter()
                .find(|r| r.id == id)
                .cloned()
                .expect("rule present")
        };
        // The imported trigger rides verbatim, capped at warn because it is
        // law-sourced and deny was asked for... it asked for warn, so it
        // stays warn.
        let triggered = find("law-triggered");
        assert_eq!(triggered.action, "warn");
        assert_eq!(triggered.law.as_deref(), Some("d-aaaa1111"));
        // The plain law becomes the implicit tier-1 inform rule.
        let implicit = find("law-d-bbbb2222");
        assert_eq!(implicit.action, "inform");
        assert_eq!(implicit.tier(), 1);
        assert_eq!(
            implicit.when.retrieval.as_deref(),
            Some("release tags no release tag until the TUI is proven")
        );
        assert_eq!(implicit.scope.as_deref(), None, "global scope rides open");
        assert!(set
            .subjects
            .get("d-bbbb2222")
            .is_some_and(|s| s == "release tags"));
        match prev {
            Some(v) => std::env::set_var("FNO_HOME", v),
            None => std::env::remove_var("FNO_HOME"),
        }
    }
}
