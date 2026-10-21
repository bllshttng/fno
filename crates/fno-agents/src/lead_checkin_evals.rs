//! The `evals` reading: the latest bank verdict as one check-in line, read
//! from the same native fold triage health prints. The verdict used to live
//! only in `fno doctor evals trend`, so a user who never ran that command
//! never learned the bank existed; the lead beat now names it.

use serde_json::{json, Value};

use crate::lead_checkin::Reading;

pub(crate) fn reading() -> Result<Value, String> {
    match crate::backlog::triage_health::evals_summary() {
        Some(payload) => Ok(payload),
        None => Ok(unmeasured_json("no bank rows in the eval history")),
    }
}

fn unmeasured_json(reason: &str) -> Value {
    json!({ "unmeasured": reason })
}

/// Exactly one line: the verdict a lead can act on. A stale window, an
/// alarm and a regressed set each earn a token; a silent `ok` is what the
/// bank already died of.
pub(crate) fn lines(readings: &[Reading]) -> Vec<String> {
    let Some(reading) = readings.iter().find(|r| r.name == "evals") else {
        return Vec::new();
    };
    if !reading.ok {
        return vec![format!("READER FAILED evals: {}", reading.error)];
    }
    if let Some(reason) = reading.value.get("unmeasured").and_then(Value::as_str) {
        return vec![format!("evals: unmeasured ({reason})")];
    }
    if reading.value.get("never_ran").and_then(Value::as_bool) == Some(true) {
        return vec!["evals: regression bank never ran".to_string()];
    }
    let mut line = String::from("evals:");
    if let Some(rate) = reading
        .value
        .get("regression_pass_rate")
        .and_then(Value::as_f64)
    {
        line.push_str(&format!(" regression pass {:.0}%,", rate * 100.0));
    }
    line.push_str(&format!(
        " flakes {},",
        reading
            .value
            .get("flake_count")
            .and_then(Value::as_i64)
            .unwrap_or(0)
    ));
    match reading.value.get("age_days").and_then(Value::as_f64) {
        Some(age) => line.push_str(&format!(" last run {age:.0}d ago,")),
        // Rows exist but carry no readable timestamp: that is unmeasured,
        // never a clean "never" - the bank may be stale past every window
        // and the line must not hide it.
        None => line.push_str(" last run unmeasured,"),
    }
    if let Some(window) = reading.value.get("window_days").and_then(Value::as_i64) {
        line.push_str(&format!(" window {window}d,"));
    }
    if reading.value.get("stale").and_then(Value::as_bool) == Some(true) {
        line.push_str(" STALE");
    }
    if let Some(alarm) = reading
        .value
        .get("regression_alarm")
        .and_then(Value::as_array)
    {
        if !alarm.is_empty() {
            line.push_str(&format!(" ALARM {}", alarm.len()));
        }
    }
    if let Some(regressed) = reading.value.get("regressed").and_then(Value::as_array) {
        if !regressed.is_empty() {
            line.push_str(&format!(" REGRESSED {}", regressed.len()));
        }
    }
    // The trailing comma the segments above accumulated never reaches the
    // lead: trim it once, after every optional token had its chance.
    while line.ends_with(',') || line.ends_with(' ') {
        line.pop();
    }
    vec![line]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verdict_renders_as_one_line_with_its_tokens() {
        let payload = json!({
            "regression_pass_rate": 0.875,
            "flake_count": 1,
            "regression_alarm": ["task-a"],
            "regressed": ["task-a", "task-b"],
            "window_days": 7,
            "age_days": 6.2,
            "stale": false,
            "never_ran": false,
        });
        assert_eq!(
            lines(&[Reading::took("evals", payload)]),
            vec![
                "evals: regression pass 88%, flakes 1, last run 6d ago, window 7d, ALARM 1 REGRESSED 2"
                    .to_string()
            ]
        );
    }

    #[test]
    fn a_stale_bank_names_the_window_and_a_rateless_fold_still_reads() {
        let stale = json!({
            "regression_pass_rate": null,
            "flake_count": 0,
            "regression_alarm": [],
            "regressed": [],
            "window_days": 7,
            "age_days": 9.0,
            "stale": true,
            "never_ran": false,
        });
        assert_eq!(
            lines(&[Reading::took("evals", stale)]),
            vec!["evals: flakes 0, last run 9d ago, window 7d, STALE".to_string()]
        );

        let no_age = json!({
            "regression_pass_rate": 1.0,
            "flake_count": 0,
            "regression_alarm": [],
            "regressed": [],
            "window_days": 7,
            "age_days": null,
            "stale": false,
            "never_ran": false,
        });
        assert_eq!(
            lines(&[Reading::took("evals", no_age)]),
            vec![
                "evals: regression pass 100%, flakes 0, last run unmeasured, window 7d".to_string()
            ]
        );
    }

    #[test]
    fn unmeasured_never_ran_failed_and_absent_read_distinctly() {
        assert_eq!(
            lines(&[Reading::took(
                "evals",
                unmeasured_json("no bank rows in the eval history")
            )]),
            vec!["evals: unmeasured (no bank rows in the eval history)".to_string()]
        );
        assert_eq!(
            lines(&[Reading::took("evals", json!({ "never_ran": true }))]),
            vec!["evals: regression bank never ran".to_string()]
        );
        assert_eq!(
            lines(&[Reading::failed("evals", "history unreadable".into())]),
            vec!["READER FAILED evals: history unreadable".to_string()]
        );
        assert!(lines(&[]).is_empty());
    }
}
