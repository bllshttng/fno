//! `fno inbox law renew <subject-or-id> --until <date>`: move a live law's
//! end date by re-recording it under a fresh id that supersedes it. Like
//! restore, the copy passed the validator and evidence gate once and runs
//! neither.

use serde_json::{json, Value};

use crate::decision_index;
use crate::law_match::{
    all_decision_rows, append_decision, autonomy_ask_gate, chat_may_retire, door_refuse,
    is_decision_id, is_retraction_row, mint_decision_id, now_iso, resolve_caller, row_id,
    row_subject, scope_filtered_rows, Caller,
};

const RENEW_USAGE: &str =
    "usage: fno inbox law renew <subject-or-id> --until YYYY-MM-DD [--quote <the user's words>]";

/// The fields a renewal copies from the live row, verbatim.
const COPIED: &str = "decision subject scope paths strength when graduation graduation_ref \
                      rationale reads options";

/// `--until YYYY-MM-DD`: the last day the law binds.
pub fn parse_until(spec: &str) -> Result<String, String> {
    chrono::NaiveDate::parse_from_str(spec.trim(), "%Y-%m-%d")
        .map(|d| d.to_string())
        .map_err(|_| format!("--until wants YYYY-MM-DD, got {spec:?}"))
}

pub fn run_renew(argv: &[String]) -> i32 {
    let mut token = None;
    let mut until = None;
    let mut quote = None;
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--until" => until = it.next().cloned(),
            "--quote" => quote = it.next().cloned(),
            a if a.starts_with('-') => {
                eprintln!("fno inbox law renew: no such option: {a}\n{RENEW_USAGE}");
                return 2;
            }
            a if token.is_none() => token = Some(a.trim().to_string()),
            _ => {
                eprintln!("fno inbox law renew: too many arguments\n{RENEW_USAGE}");
                return 2;
            }
        }
    }
    let (Some(token), Some(until)) = (token, until) else {
        eprintln!("fno inbox law renew: a law and --until are required\n{RENEW_USAGE}");
        return 2;
    };
    let until = match parse_until(&until) {
        Ok(d) => d,
        Err(message) => {
            eprintln!("fno inbox law renew: {message}\n{RENEW_USAGE}");
            return 2;
        }
    };
    let caller = match resolve_caller() {
        Ok(c) => c,
        Err(message) => return door_refuse(&message),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    if let Err(message) = autonomy_ask_gate(&caller, quote.as_deref(), &cwd) {
        return door_refuse(&message);
    }
    let (rows, _damaged) = match all_decision_rows() {
        Ok(pair) => pair,
        Err(reason) => {
            return door_refuse(&format!(
                "the decision index could not be read ({reason}); nothing renewed"
            ))
        }
    };
    let rows = scope_filtered_rows(rows);
    let data = match renewed_data(&token, &until, &rows, &caller) {
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
    eprintln!("fno inbox law renew: {token} now ends {until} as {new_id}");
    0
}

/// The new row's data, pure over the rows so every refusal is testable.
fn renewed_data(
    token: &str,
    until: &str,
    rows: &[Value],
    caller: &Caller,
) -> Result<Value, String> {
    let retired = decision_index::retirement_map(rows);
    let live: Vec<&Value> = rows
        .iter()
        .filter(|r| {
            !is_retraction_row(r)
                && decision_index::is_law(r)
                && !retired.contains_key(&row_id(r).to_lowercase())
        })
        .collect();
    let hits: Vec<&Value> = if is_decision_id(token) {
        live.into_iter()
            .filter(|r| row_id(r).eq_ignore_ascii_case(token))
            .collect()
    } else {
        live.into_iter()
            .filter(|r| row_subject(r).trim().eq_ignore_ascii_case(token))
            .collect()
    };
    let old = match hits.as_slice() {
        [one] => *one,
        [] => return Err(format!("no live law matches {token:?}; nothing to renew")),
        many => {
            let ids: Vec<&str> = many.iter().map(|r| row_id(r)).collect();
            return Err(format!(
                "{} live laws match {token:?} ({}); renew one by id",
                many.len(),
                ids.join(", ")
            ));
        }
    };
    if caller.authority != "operator"
        && (caller.authority != "chat_attested" || !chat_may_retire(old))
    {
        return Err(format!(
            "renewing {} needs superuser authority (got {}; it was recorded as {}); \
             only the user renews it, so ask them with `fno inbox outstanding ask`",
            row_id(old),
            caller.authority,
            old.get("authority_source")
                .and_then(Value::as_str)
                .unwrap_or(""),
        ));
    }
    let mut data = json!({
        "decision_id": mint_decision_id(),
        "authority_source": caller.authority,
        "decided_by": caller.decided_by,
        "supersedes": row_id(old),
        "until": until,
    });
    for key in COPIED.split_whitespace() {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn law(id: &str, authority: &str) -> Value {
        json!({
            "_event_type": "operator_decision", "decision_id": id, "subject": "quiet-hours",
            "decision": "No pushes at night", "authority_source": authority,
            "scope": "project:demo", "strength": "warn", "ts": "2026-09-01T00:00:00Z"
        })
    }

    #[test]
    fn renew_supersedes_the_live_law_with_a_new_end_and_refuses_the_rest() {
        let chat = Caller::as_authority("chat_attested");
        let rows = vec![law("d-aaaa0001", "chat_attested")];
        for token in ["quiet-hours", "d-aaaa0001"] {
            let data = renewed_data(token, "2026-12-01", &rows, &chat).expect("renews");
            assert_eq!(data["supersedes"], json!("d-aaaa0001"));
            assert_eq!(data["until"], json!("2026-12-01"));
            assert_eq!(data["strength"], json!("warn"));
            assert_eq!(data["decision"], json!("No pushes at night"));
        }

        let err = renewed_data("nope", "2026-12-01", &rows, &chat).expect_err("none");
        assert!(err.contains("no live law matches"), "{err}");

        let rows = vec![law("d-bbbb0002", "operator")];
        let err = renewed_data("quiet-hours", "2026-12-01", &rows, &chat).expect_err("superuser");
        assert!(err.contains("needs superuser authority"), "{err}");

        assert_eq!(parse_until("2026-12-01").as_deref(), Ok("2026-12-01"));
        assert!(parse_until("soon").is_err());
    }
}
