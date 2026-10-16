//! The `pr-watch pr-sessions` arm: JSON in, JSON out. The ledger join's
//! transport door; the logic lives in `crate::ledger_axes::pr_sessions`.

use serde_json::{json, Value};
use std::path::PathBuf;

pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("pr-watch pr-sessions: no flags; the payload rides stdin");
        return 2;
    }
    let mut buf = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_err() {
        eprintln!("pr-watch pr-sessions: cannot read stdin");
        return 2;
    }
    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-watch pr-sessions: bad payload: {e}");
            return 2;
        }
    };
    let Some(pr) = payload.get("pr").and_then(Value::as_i64) else {
        eprintln!("pr-watch pr-sessions: pr (int) is required");
        return 2;
    };
    let ledger = payload
        .get("ledger")
        .and_then(Value::as_str)
        .map(PathBuf::from);
    let repo_slug = payload
        .get("repo_slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let (sessions, reason) = crate::ledger_axes::pr_sessions(ledger.as_deref(), pr, repo_slug);
    let out = json!({
        "infra": crate::ledger_axes::reason_is_infra_failure(reason.as_deref()),
        "reason": reason,
        "sessions": sessions,
    });
    println!("{out}");
    0
}
