//! The question ladder's vocabulary: the one class list every question
//! reader shares, and the `[questions.rules]` config that sorts each class
//! into who decides it. Defaults live here, so an unset table means them.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// Every class a question, an escalation note or a traced decision names.
pub const CLASSES: &[&str] = &[
    "public-surface",
    "irreversible",
    "money-security",
    "outside-reach",
    "taste",
    "law-change",
    "gate-override",
    "none",
];

/// Who decides each class: `allow` the asker alone, `ask` a lead, `deny`
/// only the user (no agent rules it; it climbs to the user).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rules {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
}

impl Default for Rules {
    fn default() -> Self {
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect();
        Rules {
            allow: words(&["none"]),
            ask: words(&["public-surface", "gate-override"]),
            deny: words(&[
                "irreversible",
                "money-security",
                "outside-reach",
                "taste",
                "law-change",
            ]),
        }
    }
}

impl Rules {
    pub fn user_only(&self, class: &str) -> bool {
        self.deny.iter().any(|c| c == class)
    }

    pub fn asker_decides(&self, class: &str) -> bool {
        self.allow.iter().any(|c| c == class)
    }
}

/// The class `why_user:` names: exactly one class word, nothing else.
pub fn class_word(why: &str) -> Option<&'static str> {
    let why = why.trim();
    CLASSES
        .iter()
        .find(|c| c.eq_ignore_ascii_case(why))
        .copied()
}

/// The rules for a caller in `cwd`: the project config, then home, per key.
pub fn rules_for(cwd: &Path) -> Result<Rules, String> {
    rules_from_configs(&crate::lead_escalation::config_candidates(cwd))
}

fn rules_from_configs(candidates: &[PathBuf]) -> Result<Rules, String> {
    let tables: Vec<toml::Table> = candidates
        .iter()
        .filter_map(|c| crate::finalize::load_config_toml(c))
        .collect();
    let mut rules = Rules::default();
    let mut set: Vec<(&str, Vec<String>)> = Vec::new();
    for key in ["allow", "ask", "deny"] {
        let Some(value) = tables.iter().find_map(|t| {
            t.get("questions")?
                .as_table()?
                .get("rules")?
                .as_table()?
                .get(key)
                .cloned()
        }) else {
            continue;
        };
        let words: Vec<String> = value
            .as_array()
            .ok_or_else(|| format!("questions.rules.{key} must be a list of class words"))?
            .iter()
            .map(|w| w.as_str().unwrap_or("").to_string())
            .collect();
        if let Some(bad) = words.iter().find(|w| !CLASSES.contains(&w.as_str())) {
            return Err(format!(
                "questions.rules.{key} names {bad:?}, which is not a class; the classes are {}",
                CLASSES.join(", ")
            ));
        }
        set.push((key, words));
    }
    // A class a set key names leaves the default lists of the unset keys.
    for (key, words) in &set {
        for (other, list) in [
            ("allow", &mut rules.allow),
            ("ask", &mut rules.ask),
            ("deny", &mut rules.deny),
        ] {
            if other == *key {
                *list = words.clone();
            } else if !set.iter().any(|(k, _)| *k == other) {
                list.retain(|c| !words.contains(c));
            }
        }
    }
    for (key, words) in &set {
        for (other, others) in &set {
            if let Some(both) = (key < other)
                .then(|| words.iter().find(|w| others.contains(w)))
                .flatten()
            {
                return Err(format!(
                    "{both} is in both questions.rules.{key} and questions.rules.{other}"
                ));
            }
        }
    }
    Ok(rules)
}

/// The lease a rung holder gets: past it, an unanswered row shows on the
/// user's board whatever rung it sits at, so nothing a lead ignores is lost.
pub const LEASE_SECS: i64 = 600;

/// True when `questions.route` is `ladder` for a caller in `cwd`.
pub fn ladder_on(cwd: &Path) -> bool {
    crate::lead_escalation::route_for(cwd).as_deref() == Some("ladder")
}

/// The first rung for a question a worker asks from `session` on `node`: the
/// node's lead, its higher-up, or the least-loaded lead. `None` is the user
/// rung: the asker is a lead, or no lead holds a session to wake.
pub fn place(cwd: &Path, node: Option<&str>, session: Option<&str>) -> Option<Value> {
    use crate::owner_ladder::{resolve, world, Ask, Rung};
    let w = world(&crate::paths::AgentsHome::from_env(), cwd).ok()?;
    // A lead files to the user board itself, after it tries the answer.
    if session.is_some_and(|s| {
        w.teams
            .iter()
            .any(|t| t.holder_session.as_deref() == Some(s))
    }) {
        return None;
    }
    let owner = resolve(
        &Ask {
            node,
            from_session: session,
            start: Rung::NodeLead,
        },
        &w,
    );
    let holder_session = owner.session.filter(|_| owner.rung != Rung::User)?;
    let level = match owner.rung {
        Rung::NodeLead => "lead",
        Rung::HigherUp => "higher",
        _ => "least_loaded",
    };
    Some(json!({
        "level": level,
        "holder_session": holder_session,
        "holder_name": owner.holder,
        "scope": owner.scope,
    }))
}

/// The one wake a rung holder gets for a question placed on it.
pub fn wake_holder(rung: &Value, qid: &str, node: Option<&str>, question: &str) {
    let Some(sid) = rung.get("holder_session").and_then(Value::as_str) else {
        return;
    };
    let node = node.unwrap_or("<node>");
    let text = format!(
        "fno/questions: a worker asked {qid}, and it sits with you: {}\n\nRule it \
         with `fno backlog decide {node} \"<ruling>\"` and tell the asker. Unanswered, \
         it reaches the user's board {} minutes after it was asked.",
        question.chars().take(600).collect::<String>(),
        LEASE_SECS / 60
    );
    let runner: crate::burn_watch::Runner = &mut crate::burn_watch::run_command;
    let _ = crate::burn_watch::wake_with_text(sid, false, &text, "fno/questions", runner);
}

/// Whether a question row shows on the user's board at `now`: a row with no
/// rung (asked before the ladder, or under `route = "direct"`), a row at the
/// user rung, or a row whose lease ran out.
pub fn on_board(data: &Value, ts: &str, now_secs: i64) -> bool {
    match data.pointer("/rung/level").and_then(Value::as_str) {
        None | Some("user") => true,
        Some(_) => crate::event_store::parse_rfc3339_ms(ts)
            .is_none_or(|ms| ms / 1000 + LEASE_SECS <= now_secs),
    }
}

/// [`on_board`] over a whole `operator_question` envelope, at the wall clock.
pub fn envelope_on_board(v: &Value) -> bool {
    on_board(
        v.get("data").unwrap_or(&Value::Null),
        v.get("ts").and_then(Value::as_str).unwrap_or(""),
        chrono::Utc::now().timestamp(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_from(text: &str) -> Result<Rules, String> {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.toml");
        std::fs::write(&path, text).expect("writes");
        rules_from_configs(&[path])
    }

    #[test]
    fn rules_default_when_unset_and_refuse_a_word_outside_the_list() {
        assert_eq!(rules_from("").unwrap(), Rules::default());
        let moved =
            rules_from("[questions.rules]\ndeny = [\"taste\", \"public-surface\"]\n").unwrap();
        assert_eq!(moved.deny, ["taste", "public-surface"]);
        assert_eq!(moved.ask, ["gate-override"]);
        let err = rules_from("[questions.rules]\ndeny = [\"taste\", \"frobnicate\"]\n")
            .expect_err("unknown word");
        assert!(
            err.contains("questions.rules.deny") && err.contains("frobnicate"),
            "{err}"
        );
        let err = rules_from("[questions.rules]\nallow = [\"taste\"]\ndeny = [\"taste\"]\n")
            .expect_err("two lists");
        assert!(err.contains("taste is in both"), "{err}");
        assert_eq!(class_word(" Taste "), Some("taste"));
        assert_eq!(class_word("product call"), None);
    }

    #[test]
    fn a_lead_rung_row_reaches_the_board_when_its_lease_runs_out() {
        let ts = "2026-10-28T10:00:00Z";
        let asked = crate::event_store::parse_rfc3339_ms(ts).unwrap() / 1000;
        let lead = json!({"rung": {"level": "lead", "holder_session": "s-lead"}});
        assert!(!on_board(&lead, ts, asked + LEASE_SECS - 1));
        assert!(on_board(&lead, ts, asked + LEASE_SECS));
        assert!(on_board(&json!({"rung": {"level": "user"}}), ts, asked));
        assert!(on_board(&json!({}), ts, asked));
    }
}
