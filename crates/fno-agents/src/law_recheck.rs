//! `fno inbox law recheck`: re-measure the premise of every live law.
//!
//! Date guards retire in the index derivation. This covers the rest: event and
//! set guards read the graph, and a reading re-runs its command against the stored
//! output; a moved reading is stale. `--apply` retracts a fired law and leaves a receipt.

use serde_json::{json, Value};

use crate::decision_index;
use crate::law_match::{
    evidence_repo_root, row_id, row_subject, write_retraction_event, RetractOutcome, RetractWrite,
};

const USAGE: &str = "usage: fno inbox law recheck [--apply] [--json]";

#[derive(Debug, PartialEq)]
pub(crate) enum Verdict {
    Ok,
    /// A guard fired; the reason names the evidence.
    Expired(String),
    /// A reading moved; the reason names the command and both heads.
    Stale(String),
    /// The check could not run; the law is not called ok.
    Unknown(String),
}

fn node_closed(entries: &[Value], id: &str) -> Option<bool> {
    let node = entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(id))?;
    Some(!crate::backlog_ready::node_is_open(node))
}

/// The guard half. `entries` is the graph read, `None` when unreadable.
pub(crate) fn guard_verdict(row: &Value, entries: Option<&[Value]>, now: &str) -> Verdict {
    if decision_index::date_guard_fired(row, now) {
        let at = row
            .pointer("/guard/at")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Verdict::Expired(format!("date guard {at} passed"));
    }
    let Some(guard) = row.get("guard").filter(|g| g.is_object()) else {
        return Verdict::Ok;
    };
    let kind = guard.get("kind").and_then(Value::as_str).unwrap_or("");
    let ids: Vec<&str> = match kind {
        "event" => guard
            .get("node_id")
            .and_then(Value::as_str)
            .into_iter()
            .collect(),
        "set" => guard
            .get("node_ids")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default(),
        _ => return Verdict::Ok,
    };
    let Some(entries) = entries else {
        return Verdict::Unknown("the graph could not be read".into());
    };
    let mut open = Vec::new();
    for id in &ids {
        match node_closed(entries, id) {
            Some(true) => {}
            Some(false) => open.push(*id),
            None => return Verdict::Unknown(format!("guard node {id} is not in the graph")),
        }
    }
    if open.is_empty() && !ids.is_empty() {
        Verdict::Expired(format!("{kind} guard drained: {} closed", ids.join(", ")))
    } else {
        Verdict::Ok
    }
}

/// The reading half: re-run each stored command and compare exit and head.
pub(crate) fn reading_verdict(
    row: &Value,
    runner: &mut crate::evidence::Runner,
    root: &std::path::Path,
) -> Verdict {
    let Some(reads) = row
        .pointer("/expiry_ref/reads")
        .and_then(Value::as_array)
        .filter(|_| row.pointer("/expiry_ref/kind").and_then(Value::as_str) == Some("reading"))
    else {
        return Verdict::Ok;
    };
    for r in reads {
        let cmd = r.get("cmd").and_then(Value::as_str).unwrap_or("");
        let was_exit = r.get("exit").and_then(Value::as_i64).unwrap_or(0);
        let was_head = r.get("out_head").and_then(Value::as_str).unwrap_or("");
        match runner(cmd, root) {
            Err(why) => return Verdict::Unknown(format!("read '{cmd}' did not run ({why})")),
            Ok(out) => {
                let now_head = crate::evidence::out_head(&out.stdout);
                if i64::from(out.exit) != was_exit || now_head.trim() != was_head.trim() {
                    return Verdict::Stale(format!(
                        "read '{cmd}' moved: recorded exit {was_exit} {was_head:?}, now exit {} {now_head:?}",
                        out.exit
                    ));
                }
            }
        }
    }
    Verdict::Ok
}

/// One pass over the live laws. `apply` retracts a law whose guard fired.
pub(crate) fn collect(apply: bool) -> Result<Value, String> {
    let (rows, damaged) = match decision_index::read_store_rows(
        &crate::graph_get::default_graph_path(),
        &decision_index::default_state_path("decisions.jsonl"),
    ) {
        Ok(read) => read,
        // A project that never recorded a law has no store: nothing to recheck.
        Err(e) if e.starts_with("no decision store") => (Vec::new(), 0),
        Err(e) => return Err(format!("the decision index could not be read ({e})")),
    };
    let index = decision_index::derive_rows_unguarded(rows, damaged);
    let entries = crate::backlog::decisions_cli::read_graph_entries().ok();
    let root = evidence_repo_root();
    let mut runner = |cmd: &str, root: &std::path::Path| crate::evidence::shell_run(cmd, root, 20);
    let now = chrono::Utc::now().to_rfc3339();
    let mut report: Vec<Value> = Vec::new();
    let (mut stale, mut expired, mut unknown, mut retracted) = (0, 0, 0, 0);
    for row in index.rows.iter().filter(|r| decision_index::is_law(r)) {
        let guard = guard_verdict(row, entries.as_deref(), &now);
        let verdict = match guard {
            Verdict::Ok => reading_verdict(row, &mut runner, &root),
            other => other,
        };
        let (state, why) = match &verdict {
            Verdict::Ok => continue,
            Verdict::Expired(w) => ("expired", w.clone()),
            Verdict::Stale(w) => ("stale", w.clone()),
            Verdict::Unknown(w) => ("unknown", w.clone()),
        };
        match state {
            "expired" => expired += 1,
            "stale" => stale += 1,
            _ => unknown += 1,
        }
        let id = row_id(row).to_string();
        let mut done = false;
        if apply && state == "expired" {
            let outcome = write_retraction_event(&RetractWrite {
                target_decision_id: id.clone(),
                subject: row_subject(row).trim().to_string(),
                reason: format!("guard fired: {why}"),
                retracted_by: "law-recheck".into(),
                authority_source: "guard".into(),
                attested_by: None,
                relayed_by: None,
                quote: None,
            });
            done = matches!(outcome, RetractOutcome::Done);
            if done {
                retracted += 1;
            }
        }
        report.push(json!({
            "decision_id": id,
            "subject": row_subject(row),
            "state": state,
            "why": why,
            "retracted": done,
        }));
    }
    Ok(
        json!({"laws": report, "stale": stale, "expired": expired, "unknown": unknown, "retracted": retracted}),
    )
}

/// The check-in beat's line: empty when every law's premise holds.
pub(crate) fn beat_line(reading: &Value) -> String {
    let n = |k: &str| reading.get(k).and_then(Value::as_u64).unwrap_or(0);
    if n("stale") + n("expired") + n("unknown") == 0 {
        return String::new();
    }
    let mut line = format!(
        "laws: {} stale, {} expired ({} retracted), {} unknown",
        n("stale"),
        n("expired"),
        n("retracted"),
        n("unknown")
    );
    for l in reading
        .get("laws")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line.push_str(&format!(
            "\n  {} {} {}: {}",
            l["decision_id"].as_str().unwrap_or(""),
            l["state"].as_str().unwrap_or(""),
            l["subject"].as_str().unwrap_or(""),
            l["why"].as_str().unwrap_or("")
        ));
    }
    line
}

pub(crate) fn run_recheck(argv: &[String]) -> i32 {
    let mut apply = false;
    let mut as_json = false;
    for a in argv {
        match a.as_str() {
            "--apply" => apply = true,
            "--json" | "-J" => as_json = true,
            _ => {
                eprintln!("fno inbox law recheck: no such option: {a}\n{USAGE}");
                return 2;
            }
        }
    }
    let reading = match collect(apply) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno inbox law recheck: {e}");
            return 1;
        }
    };
    if as_json {
        println!("{reading}");
    } else {
        println!("{}", beat_line(&reading));
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(guard: Value) -> Value {
        json!({"decision_id": "d-test0001", "subject": "s", "guard": guard})
    }

    #[test]
    fn date_guard_expires_only_after_its_instant() {
        let r = row(json!({"kind": "date", "at": "2026-11-03T00:00:00+00:00"}));
        assert_eq!(
            guard_verdict(&r, None, "2026-11-02T00:00:00+00:00"),
            Verdict::Ok
        );
        assert!(matches!(
            guard_verdict(&r, None, "2026-11-04T00:00:00+00:00"),
            Verdict::Expired(_)
        ));
    }

    #[test]
    fn until_grammar_parses_each_kind_and_refuses_count() {
        use crate::law_match::parse_guard;
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-03T00:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(
            parse_guard("for:30m", now).unwrap()["at"],
            json!("2026-11-03T00:30:00+00:00")
        );
        assert_eq!(
            parse_guard("date:2026-11-04", now).unwrap()["kind"],
            json!("date")
        );
        assert_eq!(
            parse_guard("event:x-aaaa", now).unwrap()["node_id"],
            json!("x-aaaa")
        );
        assert_eq!(
            parse_guard("set:x-aaaa,x-bbbb", now).unwrap()["node_ids"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(parse_guard("count:10", now).is_err());
        assert!(parse_guard("soon", now).is_err());
    }
}
