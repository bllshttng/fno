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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_kind_from_the_string() {
        assert_eq!(classify("x-7145"), SubjectKind::Node);
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
        let mut stamped = json!({"subject": "x-7145", "subject_kind": "topic"});
        split_into(&mut stamped);
        assert_eq!(stamped["subject_text"], "x-7145");
    }
}
