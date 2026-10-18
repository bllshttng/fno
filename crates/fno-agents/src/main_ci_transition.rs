//! The durable `main_ci_changed` row: one append per red/green flip of main's
//! CI verdict, never one per probe. The live reading (`main_ci_reading`)
//! calls [`record`] with the verdict it just judged; the writer compares it
//! with the newest stored row, so the store alone holds the "previous" state
//! and no second cache can drift from it.

use serde_json::{json, Value};
use std::path::Path;

pub(crate) const EVENT_TYPE: &str = "main_ci_changed";

fn state_of(token: &Value) -> Option<&'static str> {
    match token.get("verdict").and_then(Value::as_str) {
        Some("red") => Some("red"),
        Some("green") => Some("green"),
        _ => None,
    }
}

/// The `data` payload owed for this reading, or `None` when nothing flipped.
/// A pending verdict is neither state. With no earlier row, a green main has
/// nothing to report and a red main reports with `previous_state: unknown`.
fn owed_data(repo: &str, sha: &str, token: &Value, previous_line: Option<&str>) -> Option<Value> {
    let state = state_of(token)?;
    let previous = previous_line
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .and_then(|row| {
            row.pointer("/data/state")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if previous.as_deref() == Some(state) || (previous.is_none() && state == "green") {
        return None;
    }
    let failing: Vec<&str> = token
        .get("workflow")
        .and_then(Value::as_str)
        .filter(|_| state == "red")
        .into_iter()
        .collect();
    Some(json!({
        "repo": repo,
        "sha": sha,
        "state": state,
        "previous_state": previous.as_deref().unwrap_or("unknown"),
        "failing_checks": failing,
    }))
}

/// Append the transition row when this reading flipped main's state. The
/// idempotency key names the repo and the row it follows, so two readers racing on the
/// same flip commit one row while a later flip back always commits its own.
/// Returns whether a row was inserted; a store fault is the caller's to
/// ignore, since a missing feed row must never fail a CI read.
pub(crate) fn record(journal: &Path, repo: &str, sha: &str, token: &Value) -> Result<bool, String> {
    if state_of(token).is_none() {
        return Ok(false);
    }
    let previous = crate::event_store::newest_of_type(journal, EVENT_TYPE, repo)?;
    let Some(data) = owed_data(
        repo,
        sha,
        token,
        previous.as_ref().map(|(_, line)| line.as_str()),
    ) else {
        return Ok(false);
    };
    let key = format!(
        "{EVENT_TYPE}:{repo}:{}:{}",
        previous.as_ref().map_or("none", |(id, _)| id.as_str()),
        data["state"].as_str().unwrap_or_default()
    );
    let envelope = json!({
        "ts": crate::events::now_rfc3339(),
        "type": EVENT_TYPE,
        "source": "agents",
        "data": data,
    });
    let receipt = crate::event_store::append_envelope(journal, &envelope.to_string(), Some(&key))?;
    Ok(receipt.inserted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: &str) -> String {
        json!({"type": EVENT_TYPE, "data": {"state": state}}).to_string()
    }

    fn red() -> Value {
        json!({"verdict": "red", "workflow": "cli-ci", "sha": "abc"})
    }

    #[test]
    fn a_flip_owes_a_row_and_a_repeat_or_pending_reading_owes_none() {
        let flip =
            owed_data("o/r", "abc", &red(), Some(&row("green"))).expect("green to red flips");
        assert_eq!(
            flip,
            json!({"repo": "o/r", "sha": "abc", "state": "red", "previous_state": "green", "failing_checks": ["cli-ci"]})
        );
        let back = owed_data(
            "o/r",
            "def",
            &json!({"verdict": "green"}),
            Some(&row("red")),
        )
        .expect("red to green flips");
        assert_eq!(back["failing_checks"], json!([]));
        assert_eq!(owed_data("o/r", "abc", &red(), Some(&row("red"))), None);
        assert_eq!(
            owed_data(
                "o/r",
                "abc",
                &json!({"verdict": "pending"}),
                Some(&row("red"))
            ),
            None
        );
    }

    #[test]
    fn the_first_reading_reports_red_as_unknown_and_stays_silent_on_green() {
        let first = owed_data("o/r", "abc", &red(), None).expect("a first red is news");
        assert_eq!(first["previous_state"], "unknown");
        assert_eq!(
            owed_data("o/r", "abc", &json!({"verdict": "green"}), None),
            None
        );
    }

    #[test]
    fn record_commits_one_row_per_flip_through_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        assert!(record(&journal, "o/r", "abc", &red()).unwrap());
        assert!(
            !record(&journal, "o/r", "abc", &red()).unwrap(),
            "same state, no row"
        );
        assert!(record(&journal, "o/r", "def", &json!({"verdict": "green"})).unwrap());
        assert!(
            record(&journal, "o/r", "ghi", &red()).unwrap(),
            "a second red is its own flip"
        );
        let rows = crate::event_store::query_events(
            &journal,
            &crate::event_store::EventQuery::of_types(&[EVENT_TYPE]),
        )
        .unwrap();
        let states: Vec<String> = rows
            .iter()
            .filter(|r| r.r#type == EVENT_TYPE)
            .map(|r| {
                let v: Value = serde_json::from_str(&r.line).unwrap();
                v["data"]["state"].as_str().unwrap().to_string()
            })
            .collect();
        assert_eq!(states, ["red", "green", "red"]);
        assert!(
            record(&journal, "o/other", "abc", &red()).unwrap(),
            "another repo's last row never decides this repo's flip"
        );
        assert!(rows.iter().all(|r| r.retention_class == "durable"));
    }
}
