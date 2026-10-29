//! `fno inbox law restore <decision-id>`: re-record a retracted law under a
//! fresh id that supersedes it, so every reader sees a live successor. The
//! row passed the validator and evidence gate once; the copy runs neither.

use serde_json::{json, Value};

use crate::decision_index;
use crate::law_match::{
    all_decision_rows, append_decision, chat_may_retire, door_refuse, is_decision_id,
    is_retraction_row, mint_decision_id, now_iso, resolve_caller, row_id, row_subject,
    scope_filtered_rows, Caller,
};

const RESTORE_USAGE: &str = "usage: fno inbox law restore <decision-id>";

/// The fields a restore copies from the retracted row, verbatim.
const COPIED: &str =
    "decision subject scope paths graduation graduation_ref rationale reads options";

pub fn run_restore(argv: &[String]) -> i32 {
    let token = match argv {
        [one] if is_decision_id(one.trim()) => one.trim().to_string(),
        _ => {
            eprintln!("fno inbox law restore: restore takes one decision id\n{RESTORE_USAGE}");
            return 2;
        }
    };
    let caller = match resolve_caller() {
        Ok(c) => c,
        Err(message) => return door_refuse(&message),
    };
    let (rows, damaged) = match all_decision_rows() {
        Ok(pair) => pair,
        Err(reason) => {
            return door_refuse(&format!(
                "the decision index could not be read ({reason}); nothing restored"
            ))
        }
    };
    if damaged > 0 {
        eprintln!(
            "fno inbox law restore: {damaged} index row(s) could not be parsed; \
             the answer may be incomplete."
        );
    }
    let rows = scope_filtered_rows(rows);
    let data = match restored_data(&token, &rows, &caller) {
        Ok(d) => d,
        Err(message) => return door_refuse(&message),
    };
    let new_id = data["decision_id"].as_str().unwrap_or("").to_string();
    let envelope =
        json!({"ts": now_iso(), "type": "operator_decision", "source": "target", "data": data});
    if let Err(code) = append_decision(&envelope, &new_id) {
        return code;
    }
    println!("{new_id}");
    eprintln!("fno inbox law restore: restored {token} as {new_id}");
    0
}

/// The new row's data, pure over the rows so every refusal is testable
/// without env surgery. `Err` carries the refusal text.
fn restored_data(token: &str, rows: &[Value], caller: &Caller) -> Result<Value, String> {
    let want = token.to_lowercase();
    let old = rows
        .iter()
        .filter(|r| !is_retraction_row(r))
        .find(|r| row_id(r).to_lowercase() == want)
        .ok_or_else(|| format!("decision {token} is not recoverable from the decision index"))?;
    let retired = decision_index::retirement_map(rows);
    match retired.get(&want) {
        Some((_, "retracted")) => {}
        Some((_, "superseded")) => {
            let successor = newest(rows, |r| {
                field(r, "supersedes").eq_ignore_ascii_case(token) && !is_retraction_row(r)
            });
            let successor_id = successor.map(row_id).unwrap_or("");
            return Err(match successor {
                Some(s) if s.get("restores").is_some() => {
                    format!("{token} was already restored as {successor_id}")
                }
                _ => format!("{token} is superseded by {successor_id}; nothing to restore"),
            });
        }
        _ => return Err(format!("{token} is live; nothing to restore")),
    }
    if !decision_index::is_law(old) {
        return Err(format!(
            "{token} is not a law-lane decision; restore re-records laws only"
        ));
    }
    if caller.authority != "operator"
        && (caller.authority != "chat_attested" || !chat_may_retire(old))
    {
        return Err(format!(
            "restoring {token} needs superuser authority (got {}; it was recorded as {}); \
             only the user restores it, so ask them with `fno inbox outstanding ask`",
            caller.authority,
            field(old, "authority_source"),
        ));
    }
    // A second live law under one subject breaks the auto-supersede rule,
    // so a subject that gained a new law since the retraction refuses.
    if let Some(rival) = rows.iter().find(|r| {
        !is_retraction_row(r)
            && decision_index::is_law(r)
            && !retired.contains_key(&row_id(r).to_lowercase())
            && row_subject(r)
                .trim()
                .eq_ignore_ascii_case(row_subject(old).trim())
            && field(r, "scope") == field(old, "scope")
    }) {
        return Err(format!(
            "{} is live under the subject {:?}; retract or supersede it instead of \
             restoring {token}",
            row_id(rival),
            row_subject(old).trim()
        ));
    }
    let retraction = newest(rows, |r| {
        is_retraction_row(r) && field(r, "target_decision_id").eq_ignore_ascii_case(token)
    });
    let mut data = json!({
        "decision_id": mint_decision_id(),
        "authority_source": caller.authority,
        "decided_by": caller.decided_by,
        "supersedes": row_id(old),
        "restores": retraction.map(|r| field(r, "retraction_id")).unwrap_or(""),
    });
    for key in COPIED.split(' ') {
        if let Some(value) = old.get(key).filter(|v| !v.is_null()) {
            data[key] = value.clone();
        }
    }
    if let Some(a) = &caller.attested_by {
        data["attested_by"] = json!(a);
    }
    if let Some(r) = &caller.relayed_by {
        data["relayed_by"] = json!(r);
    }
    if let Some(map) = data.as_object_mut() {
        crate::identity::stamp_person(map);
    }
    Ok(data)
}

fn field<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

fn newest<'a>(rows: &'a [Value], keep: impl Fn(&Value) -> bool) -> Option<&'a Value> {
    rows.iter()
        .filter(|r| keep(r))
        .max_by(|a, b| field(a, "ts").cmp(field(b, "ts")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn law(id: &str, authority: &str, ts: &str) -> Value {
        json!({
            "_event_type": "operator_decision", "decision_id": id, "subject": "portal-test",
            "decision": "One pane is never a portal", "authority_source": authority,
            "scope": "project:demo", "reads": [{"cmd": "head -5 a.py"}], "ts": ts
        })
    }

    fn retraction(target: &str, ts: &str) -> Value {
        json!({
            "_event_type": "decision_retracted", "retraction_id": "r-1",
            "target_decision_id": target, "subject": "portal-test", "ts": ts
        })
    }

    #[test]
    fn law_restore_re_records_a_retracted_chat_law_and_refuses_the_rest() {
        let chat = Caller::as_authority("chat_attested");
        let mut rows = vec![
            law("d-aaaa0001", "chat_attested", "2026-09-01T00:00:00Z"),
            retraction("d-aaaa0001", "2026-09-02T00:00:00Z"),
        ];
        let data = restored_data("d-aaaa0001", &rows, &chat).expect("restores");
        assert_eq!(data["supersedes"], json!("d-aaaa0001"));
        assert_eq!(data["restores"], json!("r-1"));
        assert_eq!(data["authority_source"], json!("chat_attested"));
        assert_eq!(data["decision"], json!("One pane is never a portal"));
        assert_eq!(data["reads"], json!([{"cmd": "head -5 a.py"}]));

        // Once restored, the old id reads superseded by a restore.
        let mut restored = data.clone();
        restored["_event_type"] = json!("operator_decision");
        restored["ts"] = json!("2026-09-03T00:00:00Z");
        rows.push(restored.clone());
        let err = restored_data("d-aaaa0001", &rows, &chat).expect_err("twice");
        assert!(err.contains("was already restored as"), "{err}");

        // The restored row itself is live.
        let new_id = restored["decision_id"].as_str().unwrap().to_string();
        let err = restored_data(&new_id, &rows, &chat).expect_err("live");
        assert_eq!(err, format!("{new_id} is live; nothing to restore"));

        // A superuser law refuses the chat lane.
        let rows = vec![
            law("d-bbbb0002", "operator", "2026-09-01T00:00:00Z"),
            retraction("d-bbbb0002", "2026-09-02T00:00:00Z"),
        ];
        let err = restored_data("d-bbbb0002", &rows, &chat).expect_err("superuser");
        assert!(err.contains("needs superuser authority"), "{err}");
        assert!(restored_data("d-bbbb0002", &rows, &Caller::as_authority("operator")).is_ok());

        // A subject that gained a new live law since refuses.
        let rows = vec![
            law("d-cccc0003", "chat_attested", "2026-09-01T00:00:00Z"),
            retraction("d-cccc0003", "2026-09-02T00:00:00Z"),
            law("d-dddd0004", "chat_attested", "2026-09-03T00:00:00Z"),
        ];
        let err = restored_data("d-cccc0003", &rows, &chat).expect_err("rival");
        assert!(err.starts_with("d-dddd0004 is live"), "{err}");
    }
}
