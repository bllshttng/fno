//! The typed subject of a decision record.
//!
//! A decision's `subject` has always been one free string: a node id, a PR
//! ref, or a topic. The door now stamps `subject_kind` beside it, and the
//! listing splits it into node, PR and description columns. A row recorded
//! before the stamp carries no kind, so the kind is read from the string.

use serde_json::{json, Value};

/// The kind a subject string names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubjectKind {
    Node,
    Pr,
    Topic,
}

impl SubjectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Pr => "pr",
            Self::Topic => "topic",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "node" => Some(Self::Node),
            "pr" => Some(Self::Pr),
            "topic" => Some(Self::Topic),
            _ => None,
        }
    }
}

/// The kind a subject string names. A node id needs a dash, so a bare word
/// that happens to be hex never reads as a node.
pub fn classify(subject: &str) -> SubjectKind {
    let s = subject.trim();
    if pr_number(s).is_some() {
        SubjectKind::Pr
    } else if s.contains('-')
        && !s.contains(char::is_whitespace)
        && super::node_ref::is_wellformed_node_id(s)
    {
        SubjectKind::Node
    } else {
        SubjectKind::Topic
    }
}

/// The PR number of `owner/repo#N`, a `/pull/N` URL, `PR N`, or `PR #N`.
fn pr_number(s: &str) -> Option<i64> {
    let lower = s.to_ascii_lowercase();
    let tail = if let Some((_, n)) = lower.split_once("/pull/") {
        n
    } else if let Some(n) = lower.strip_prefix("pr-") {
        n
    } else if let Some(n) = lower.strip_prefix("pr ") {
        n.trim_start_matches('#')
    } else {
        let (repo, n) = lower.split_once('#')?;
        if !repo.contains('/') || repo.contains(char::is_whitespace) {
            return None;
        }
        n
    };
    let digits: String = tail.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Add `subject_kind` to a decision event's data, next to `subject`.
pub fn stamp(data: &mut Value, subject: &str) {
    data["subject_kind"] = json!(classify(subject).as_str());
}

/// Add the split columns to a listing row: `subject_kind`, `subject_node`,
/// `subject_pr`, `subject_text`. The stored kind wins; a row with none, or an
/// unknown one, is read from its subject string.
pub fn split_into(row: &mut Value) {
    let subject = row
        .get("subject")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let kind = row
        .get("subject_kind")
        .and_then(Value::as_str)
        .and_then(SubjectKind::parse)
        .unwrap_or_else(|| classify(&subject));
    let (node, pr, text) = match kind {
        SubjectKind::Node => (subject, String::new(), String::new()),
        SubjectKind::Pr => (
            String::new(),
            pr_number(&subject)
                .map(|n| format!("#{n}"))
                .unwrap_or_default(),
            String::new(),
        ),
        SubjectKind::Topic => (String::new(), String::new(), subject),
    };
    row["subject_kind"] = json!(kind.as_str());
    row["subject_node"] = json!(node);
    row["subject_pr"] = json!(pr);
    row["subject_text"] = json!(text);
}

/// Fill the side the subject did not name from the graph: a known node names
/// its PR and a known PR names its node. Advisory: an entry the graph did
/// not supply, or a PR no row claims, leaves the row one-sided.
pub fn cross_fill(row: &mut Value, entries: &[Value]) {
    let node = row
        .get("subject_node")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let pr = row
        .get("subject_pr")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    match (node.is_empty(), pr.is_empty()) {
        (false, true) => {
            let entry = entries
                .iter()
                .find(|e| e.get("id").and_then(Value::as_str) == Some(node.as_str()));
            let number = entry
                .and_then(|e| e.get("pr_number").and_then(Value::as_i64))
                .or_else(|| {
                    entry.and_then(|e| e.get("additional_prs")).and_then(|a| {
                        a.as_array().and_then(|rows| {
                            rows.iter()
                                .find_map(|p| p.get("number").and_then(Value::as_i64))
                        })
                    })
                });
            if let Some(n) = number {
                row["subject_pr"] = json!(format!("#{n}"));
            }
        }
        (true, false) => {
            let Ok(n) = pr.trim_start_matches('#').parse::<i64>() else {
                return;
            };
            let primary = entries
                .iter()
                .find(|e| e.get("pr_number").and_then(Value::as_i64) == Some(n));
            let hit = primary.or_else(|| {
                entries.iter().find(|e| {
                    e.get("additional_prs")
                        .and_then(Value::as_array)
                        .is_some_and(|rows| {
                            rows.iter()
                                .any(|p| p.get("number").and_then(Value::as_i64) == Some(n))
                        })
                })
            });
            if let Some(id) = hit.and_then(|e| e.get("id")).and_then(Value::as_str) {
                row["subject_node"] = json!(id);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_kind_from_the_string() {
        assert_eq!(classify("x-aaaa"), SubjectKind::Node);
        assert_eq!(classify("fnodotdev/fno#4414"), SubjectKind::Pr);
        assert_eq!(classify("PR 4414"), SubjectKind::Pr);
        assert_eq!(
            classify("https://github.com/fnodotdev/fno/pull/4414"),
            SubjectKind::Pr
        );
        assert_eq!(classify("revert-dispute policy"), SubjectKind::Topic);
        assert_eq!(classify("beefcafe"), SubjectKind::Topic);
    }

    #[test]
    fn an_old_row_splits_and_a_stamped_row_keeps_its_kind() {
        let mut old = json!({"subject": "fnodotdev/fno#4414"});
        split_into(&mut old);
        assert_eq!(old["subject_pr"], "#4414");
        assert_eq!(old["subject_node"], "");
        let mut stamped = json!({"subject": "x-aaaa", "subject_kind": "topic"});
        split_into(&mut stamped);
        assert_eq!(stamped["subject_text"], "x-aaaa");
    }

    #[test]
    fn cross_fill_fills_both_sides_from_graph_rows() {
        let entries = vec![json!({"id": "x-cccc", "pr_number": 4414})];
        let mut pr_row = json!({"subject": "PR 4414"});
        split_into(&mut pr_row);
        cross_fill(&mut pr_row, &entries);
        assert_eq!(pr_row["subject_pr"], "#4414");
        assert_eq!(pr_row["subject_node"], "x-cccc");

        let mut node_row = json!({"subject": "x-cccc"});
        split_into(&mut node_row);
        cross_fill(&mut node_row, &entries);
        assert_eq!(node_row["subject_node"], "x-cccc");
        assert_eq!(node_row["subject_pr"], "#4414");
    }

    #[test]
    fn cross_fill_prefers_the_primary_pr_on_a_reverse_lookup() {
        let entries = vec![
            json!({"id": "x-addl", "additional_prs": [{"number": 4414}]}),
            json!({"id": "x-primary", "pr_number": 4414}),
        ];
        let mut row = json!({"subject": "PR 4414"});
        split_into(&mut row);
        cross_fill(&mut row, &entries);
        assert_eq!(row["subject_node"], "x-primary");
    }

    #[test]
    fn cross_fill_touches_nothing_it_cannot_resolve() {
        let mut row = json!({"subject": "PR 4414"});
        split_into(&mut row);
        cross_fill(&mut row, &[]);
        assert_eq!(row["subject_node"], "");
        assert_eq!(row["subject_pr"], "#4414");

        let entries = vec![json!({"id": "x-cccc", "pr_number": 4414})];
        let mut filled = json!({"subject": "x-cccc"});
        split_into(&mut filled);
        filled["subject_pr"] = json!("#9999");
        cross_fill(&mut filled, &entries);
        assert_eq!(filled["subject_pr"], "#9999");
    }
}
