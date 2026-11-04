//! The Decisions table model: one row per recorded decision, the filters the
//! view cycles, the audit queue, and the argv each action runs.
//!
//! The rows come from `fno backlog decisions --json`, the one reader, so the
//! mux view cannot drift from the CLI listing. This module reads; it never
//! writes. A retract or an overrule changes the law store only when a human
//! at a terminal runs the door: the door's operator proof is its worker's
//! stdin being a real tty (`law_cli::run_decide`). The view therefore hands
//! each action's argv to a pane and does not spawn it from the client.

use serde::Deserialize;
use std::collections::HashSet;

/// One decision as `fno backlog decisions --json` lists it.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DecisionRow {
    pub decision_id: String,
    #[serde(default)]
    pub decision: String,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub decided_by: Option<String>,
    #[serde(default)]
    pub authority_source: Option<String>,
    #[serde(default)]
    pub quote: Option<String>,
    #[serde(default)]
    pub rationale: Option<String>,
    #[serde(default)]
    pub ts: String,
    #[serde(default)]
    pub lifecycle: String,
    #[serde(default)]
    pub lane: String,
    #[serde(default)]
    pub number: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Listing {
    #[serde(default)]
    decisions: Vec<DecisionRow>,
}

/// Who decided. `User` is an attested human word; `Agent` any other stamped
/// authority; `Unknown` a row that carries no authority at all, the audit
/// queue's raw material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Author {
    User,
    Agent,
    Unknown,
}

impl DecisionRow {
    pub fn author(&self) -> Author {
        match self.authority_source.as_deref() {
            Some("chat_attested") | Some("operator") => Author::User,
            Some(s) if !s.is_empty() => Author::Agent,
            _ => Author::Unknown,
        }
    }

    pub fn has_quote(&self) -> bool {
        self.quote.as_deref().is_some_and(|q| !q.trim().is_empty())
    }

    /// The audit queue: rows in the `unattributed` lane or with no authority.
    pub fn needs_audit(&self) -> bool {
        self.lane == "unattributed" || self.author() == Author::Unknown
    }
}

/// The three filter axes the view cycles with one key each. `None` means all.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filter {
    pub lane: Option<&'static str>,
    pub state: Option<&'static str>,
    pub author: Option<Author>,
    pub audit_only: bool,
}

const LANES: [&str; 4] = ["law", "coord", "grant", "unattributed"];
const STATES: [&str; 6] = [
    "live",
    "expired",
    "superseded",
    "retracted",
    "retired",
    "unscoped",
];
const AUTHORS: [Author; 3] = [Author::User, Author::Agent, Author::Unknown];

fn next<T: Copy + PartialEq>(all: &[T], cur: Option<T>) -> Option<T> {
    match cur {
        None => all.first().copied(),
        Some(c) => all
            .iter()
            .position(|x| *x == c)
            .and_then(|i| all.get(i + 1).copied()),
    }
}

impl Filter {
    pub fn cycle_lane(&mut self) {
        self.lane = next(&LANES, self.lane);
    }
    pub fn cycle_state(&mut self) {
        self.state = next(&STATES, self.state);
    }
    pub fn cycle_author(&mut self) {
        self.author = next(&AUTHORS, self.author);
    }
    pub fn toggle_audit(&mut self) {
        self.audit_only = !self.audit_only;
    }

    pub fn admits(&self, r: &DecisionRow, reviewed: &HashSet<String>) -> bool {
        self.lane.is_none_or(|l| r.lane == l)
            && self.state.is_none_or(|s| r.lifecycle == s)
            && self.author.is_none_or(|a| r.author() == a)
            && (!self.audit_only || (r.needs_audit() && !reviewed.contains(&r.decision_id)))
    }
}

/// The rows a filter admits, newest first.
pub fn visible<'a>(
    rows: &'a [DecisionRow],
    f: &Filter,
    reviewed: &HashSet<String>,
) -> Vec<&'a DecisionRow> {
    let mut v: Vec<&DecisionRow> = rows.iter().filter(|r| f.admits(r, reviewed)).collect();
    v.sort_by(|a, b| b.ts.cmp(&a.ts));
    v
}

pub fn parse(stdout: &[u8]) -> Result<Vec<DecisionRow>, String> {
    serde_json::from_slice::<Listing>(stdout)
        .map(|l| l.decisions)
        .map_err(|e| format!("malformed decisions listing: {e}"))
}

/// Read every decision through the one CLI reader; the `Err` text names the
/// failure.
pub fn load() -> Result<Vec<DecisionRow>, String> {
    let output = std::process::Command::new(crate::server::fno_bin())
        .args([
            "backlog",
            "decisions",
            "--state",
            "all",
            "--limit",
            "0",
            "--json",
        ])
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot spawn fno: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "fno backlog decisions exited {}",
            output.status.code().unwrap_or(-1)
        ));
    }
    parse(&output.stdout)
}

/// The ids the user marked reviewed; a missing or unreadable file is empty.
pub fn load_reviewed(path: &std::path::Path) -> HashSet<String> {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<HashSet<String>>(&b).ok())
        .unwrap_or_default()
}

/// Add `id` to the reviewed set and write it back.
pub fn mark_reviewed(path: &std::path::Path, id: &str) -> std::io::Result<()> {
    let mut set = load_reviewed(path);
    set.insert(id.to_string());
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut ids: Vec<_> = set.into_iter().collect();
    ids.sort();
    std::fs::write(path, serde_json::to_vec(&ids).unwrap_or_default())
}

/// What the user can do to one row; each renders to an argv a pane runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Retract { reason: String },
    Overrule { new_text: String },
}

/// The argv for `action` on `row`, after `fno`. An overrule records the new
/// text on the same subject and marks the old id superseded.
pub fn action_argv(row: &DecisionRow, action: &Action) -> Vec<String> {
    let words = |w: &[&str]| w.iter().map(|v| v.to_string()).collect::<Vec<_>>();
    let law = row.lane == "law";
    let (mut argv, rest) = match action {
        Action::Retract { reason } if law => (
            words(&["inbox", "law", "retract", &row.decision_id]),
            words(&["--reason", reason]),
        ),
        Action::Retract { reason } => (
            words(&["backlog", "decide-retract", &row.decision_id]),
            words(&["--reason", reason, "--authority", "operator"]),
        ),
        Action::Overrule { new_text } if law => (
            words(&["inbox", "law", "set", &row.subject, new_text]),
            words(&["--supersedes", &row.decision_id]),
        ),
        Action::Overrule { new_text } => (
            words(&["inbox", "decide", &row.subject, new_text]),
            words(&["--supersedes", &row.decision_id, "--authority", "operator"]),
        ),
    };
    argv.extend(rest);
    argv
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, lane: &str, life: &str, auth: Option<&str>, ts: &str) -> DecisionRow {
        DecisionRow {
            decision_id: id.into(),
            lane: lane.into(),
            lifecycle: life.into(),
            authority_source: auth.map(str::to_string),
            ts: ts.into(),
            subject: "subj".into(),
            ..Default::default()
        }
    }

    #[test]
    fn author_reads_the_authority_stamp() {
        assert_eq!(
            row("a", "law", "live", Some("chat_attested"), "").author(),
            Author::User
        );
        assert_eq!(
            row("a", "coord", "live", Some("role"), "").author(),
            Author::Agent
        );
        assert_eq!(
            row("a", "coord", "live", None, "").author(),
            Author::Unknown
        );
    }

    #[test]
    fn filters_combine_and_sort_newest_first() {
        let rows = vec![
            row("old", "law", "live", Some("operator"), "2026-01-01"),
            row("new", "law", "live", Some("operator"), "2026-02-01"),
            row("gone", "law", "retracted", Some("operator"), "2026-03-01"),
            row("agent", "coord", "live", Some("role"), "2026-04-01"),
        ];
        let mut f = Filter::default();
        f.cycle_lane();
        f.cycle_state();
        let ids: Vec<_> = visible(&rows, &f, &HashSet::new())
            .iter()
            .map(|r| r.decision_id.clone())
            .collect();
        assert_eq!(ids, ["new", "old"]);
    }

    #[test]
    fn audit_queue_takes_unattributed_and_authorless_rows() {
        let rows = vec![
            row("u", "unattributed", "unscoped", Some("role"), "1"),
            row("n", "coord", "live", None, "2"),
            row("ok", "law", "live", Some("chat_attested"), "3"),
        ];
        let f = Filter {
            audit_only: true,
            ..Filter::default()
        };
        assert_eq!(visible(&rows, &f, &HashSet::new()).len(), 2);
    }

    #[test]
    fn actions_render_the_law_door_argv() {
        let r = row("d-1", "law", "live", Some("operator"), "1");
        assert_eq!(
            action_argv(
                &r,
                &Action::Retract {
                    reason: "wrong".into()
                }
            ),
            ["inbox", "law", "retract", "d-1", "--reason", "wrong"]
        );
        assert_eq!(
            action_argv(
                &r,
                &Action::Overrule {
                    new_text: "new".into()
                }
            ),
            ["inbox", "law", "set", "subj", "new", "--supersedes", "d-1"]
        );
        let c = row("d-2", "coord", "live", Some("role"), "1");
        assert_eq!(
            action_argv(
                &c,
                &Action::Retract {
                    reason: "wrong".into()
                }
            ),
            [
                "backlog",
                "decide-retract",
                "d-2",
                "--reason",
                "wrong",
                "--authority",
                "operator"
            ]
        );
        assert_eq!(
            action_argv(
                &c,
                &Action::Overrule {
                    new_text: "new".into()
                }
            ),
            [
                "inbox",
                "decide",
                "subj",
                "new",
                "--supersedes",
                "d-2",
                "--authority",
                "operator"
            ]
        );
    }

    #[test]
    fn reviewed_marks_leave_the_audit_queue() {
        let rows = vec![row("u", "unattributed", "unscoped", Some("role"), "1")];
        let f = Filter {
            audit_only: true,
            ..Filter::default()
        };
        let dir = std::env::temp_dir().join(format!("fno-dec-{}", std::process::id()));
        let path = dir.join("reviewed.json");
        mark_reviewed(&path, "u").unwrap();
        assert!(visible(&rows, &f, &load_reviewed(&path)).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn parses_the_cli_listing() {
        let json = br#"{"decisions":[{"decision_id":"d-1","decision":"x","lane":"law","lifecycle":"live","quote":"ok"}],"total":1}"#;
        let rows = parse(json).unwrap();
        assert!(rows[0].has_quote());
        assert!(parse(b"nope").is_err());
    }
}
