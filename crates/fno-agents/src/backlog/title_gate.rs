//! The write-time title leak gate.
//!
//! The public roadmap render (`projections::run_roadmap`) omits rows whose
//! titles carry a leak class, but that gate fires at PUBLISH time: the page
//! silently loses a row until a human reads the warning. This gate runs at
//! the publication seam instead, so a leaking title never enters the graph.
//! The render calls `title_leak_classes` too, so the two cannot disagree.
//! The front binary's public snapshot keeps its own copy of the patterns in
//! `crates/fno/src/backlog_snapshot.rs`. Node ids and PR numbers are ruled
//! public; home paths and session ids are not.

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

/// Titles are one line; the renderers cut display at 90-120 chars. Anything
/// past this bound is details, not a title.
pub const TITLE_MAX_CHARS: usize = 200;

/// The one leak vocabulary, shared with the render gate's `LEAK_PATTERNS`:
/// `(class, pattern)` pairs in the same order.
static LEAK_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "home-path",
            Regex::new(r"(?:~/(?:[^\s]+)|/(?:Users|home)/[^\s/]+(?:/[^\s]+)?)").expect("static regex"),
        ),
        (
            "session-id",
            Regex::new(
                r"(?i)\b(?:[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}|ses[-_][A-Za-z0-9_-]+)\b",
            )
            .expect("static regex"),
        ),
    ]
});

/// The shared `(class, pattern)` vocabulary, for callers that mask what the
/// gate would refuse (the upstream-issue draft).
pub(crate) fn leak_patterns() -> &'static [(&'static str, Regex)] {
    &LEAK_PATTERNS
}

/// Every leak class the title matches, in vocabulary order.
pub fn title_leak_classes(title: &str) -> Vec<&'static str> {
    LEAK_PATTERNS
        .iter()
        .filter(|(_, pattern)| pattern.is_match(title))
        .map(|(class, _)| *class)
        .collect()
}

fn title_of(row: &Value) -> &str {
    row.get("title").and_then(Value::as_str).unwrap_or("")
}

/// First matched token, for the refusal text. Home paths can be long; the
/// message stays one line.
fn offending_token(title: &str) -> String {
    for (_, pattern) in LEAK_PATTERNS.iter() {
        if let Some(hit) = pattern.find(title) {
            let token = hit.as_str();
            return if token.chars().count() > 60 {
                let cut: String = token.chars().take(57).collect();
                format!("{cut}...")
            } else {
                token.to_string()
            };
        }
    }
    String::new()
}

/// The write-time refusal, mirroring `node_state::budget_message`: names the
/// class, the offending token and the rule (titles publish; details do not).
pub fn refusal_message(row_id: &str, title: &str) -> Option<String> {
    let classes = title_leak_classes(title);
    if let Some(class) = classes.first() {
        let token = offending_token(title);
        return Some(format!(
            "public title gate refused {row_id}: title carries a {class} (\"{token}\"). \
Titles publish to the public roadmap; put paths and session ids in --details."
        ));
    }
    let len = title.chars().count();
    if len > TITLE_MAX_CHARS {
        return Some(format!(
            "public title gate refused {row_id}: title is {len} chars (limit {TITLE_MAX_CHARS}). \
Titles render on one public line; put the rest in --details."
        ));
    }
    None
}

/// The publication-seam title policy, shaped like
/// [`super::node_state`]'s prose budget: a title the pre-image already holds
/// passes untouched (99 stored titles predate this gate; an unrelated status
/// or claim update on such a row must keep succeeding), while a NEW or
/// CHANGED title the render gate would refuse is refused here, at write time.
pub fn enforce_title_gate(
    pre: &[Value],
    entries: &[Value],
) -> Result<(), crate::graph_store::StoreError> {
    // One borrowed index over the pre-image, built once: a linear find per
    // entry is quadratic over the whole graph, and every publish-seam write
    // runs this gate.
    let pre_by_id = crate::graph_store::index_by_id(pre);
    for cand in entries {
        let Some(id) = crate::graph_store::entry_id(cand) else {
            continue;
        };
        let title = title_of(cand);
        if title.is_empty() {
            continue;
        }
        let pre_title = pre_by_id.get(id).copied().map(title_of);
        if pre_title == Some(title) {
            continue;
        }
        if let Some(message) = refusal_message(id, title) {
            return Err(crate::graph_store::StoreError::Invalid(message));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, title: &str) -> Value {
        json!({"id": id, "slug": id, "title": title, "type": "feature", "status": "idea"})
    }

    #[test]
    fn every_leak_class_refuses() {
        for (title, class) in [
            ("see https://x/~/notes/secrets.md", "home-path"),
            (
                "crashed for session 0f1e2d3c-4b5a-6978-8796-a5b4c3d2e1f0",
                "session-id",
            ),
        ] {
            let err = enforce_title_gate(&[], &[node("x-new", title)])
                .expect_err(&format!("{class} must refuse: {title}"));
            assert!(err.to_string().contains(class), "{class}: {err}");
        }
    }

    #[test]
    fn an_unchanged_legacy_leaky_title_passes_an_unrelated_update() {
        let pre = [node("x-old", "legacy title about /Users/bb16/notes")];
        let mut updated = node("x-old", "legacy title about /Users/bb16/notes");
        updated["status"] = json!("in_progress");
        enforce_title_gate(&pre, &[updated]).expect("unchanged stored title must pass");
    }
}
