//! Characterization against `fno.agents.watchdog`, captured to
//! tests/golden/watchdog/verdicts.json before the Python leg was deleted.

use super::sweep::{digest_text, provider_outage_lines, union_signature, verdict_signature};
use super::*;

fn golden() -> Value {
    serde_json::from_str(include_str!("../../tests/golden/watchdog/verdicts.json"))
        .expect("golden parses")
}

fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn fixture_facts(spec: &Value, now: f64) -> Option<TailFacts> {
    if spec.is_null() {
        return None;
    }
    let epoch = spec.get("age").and_then(Value::as_f64).map(|age| now - age);
    let body = text(spec, "text").unwrap_or("").to_string();
    let records = epoch.map_or(Vec::new(), |e| vec![(Some(e), body.clone())]);
    Some(TailFacts {
        records,
        last_event_epoch: epoch,
        tail_text: if epoch.is_some() {
            body.clone()
        } else {
            String::new()
        },
        last_role: Some("assistant".to_string()),
        last_text: body,
        pr_polls: Vec::new(),
    })
}

fn fixture_row(id: &str, spec: &Value, name: &str) -> FleetRow {
    FleetRow {
        row_id: id.to_string(),
        name: name.to_string(),
        state: text(spec, "state").unwrap_or("").to_string(),
        node: text(spec, "node").map(str::to_string),
        cwd: text(spec, "cwd").unwrap_or("").to_string(),
        agent: text(spec, "agent").unwrap_or("claude").to_string(),
        stopped_at: None,
        pid: None,
        pid_start_time: None,
        mux: None,
        substrate: text(spec, "substrate").map(str::to_string),
    }
}

#[test]
fn every_verdict_equals_the_python_oracle() {
    let golden = golden();
    let now = golden["extra"]["now"].as_f64().expect("now");
    for case in golden["cases"].as_array().expect("cases") {
        let name = text(case, "name").expect("name");
        let env = &case["env"];
        let row = fixture_row(&format!("s-{name}"), &case["row"], &format!("n-{name}"));
        let peer = env.get("peer").is_some();
        let mut rows = vec![row.clone()];
        if peer {
            rows.push(fixture_row(
                "peer",
                &json!({"state": "working", "cwd": row.cwd}),
                "peer",
            ));
        }
        let own = fixture_facts(&case["facts"], now);
        let row_id = row.row_id.clone();
        let transcript_for = |sid: &str| -> Option<TailFacts> {
            if sid == row_id {
                own.clone()
            } else {
                fixture_facts(&json!({"age": 30, "text": "busy"}), now)
            }
        };
        let claim_for = |_node: &str| -> Result<ClaimView, String> {
            let claim = env.get("claim");
            Ok(ClaimView {
                state: claim
                    .and_then(|c| text(c, "state"))
                    .unwrap_or("live")
                    .to_string(),
                holder: Some(
                    claim
                        .and_then(|c| text(c, "holder"))
                        .unwrap_or("h")
                        .to_string(),
                ),
            })
        };
        let node_state_for =
            |_node: &str| -> Result<Option<Value>, String> { Ok(env.get("node").cloned()) };
        let worktree_check = |_cwd: &str| peer;
        let pr_state_for = |_cwd: &str, _n: i64| -> Option<String> { None };
        let commit_count = |_cwd: &str| env.get("commits").and_then(Value::as_i64);
        let seams = Seams {
            transcript_for: &transcript_for,
            claim_for: &claim_for,
            node_state_for: &node_state_for,
            worktree_check: &worktree_check,
            pr_state_for: &pr_state_for,
            commit_count: &commit_count,
        };
        let silence = env.get("silence").and_then(Value::as_f64);
        let out = verdicts(&rows, &seams, now, None, silence);
        let actual = serde_json::to_value(&out[0]).expect("verdict serializes");
        assert_eq!(actual, case["verdict"], "case {name}");
    }
}

#[test]
fn the_pure_helpers_equal_the_python_oracle() {
    let golden = golden();
    let extra = &golden["extra"];
    let now = extra["now"].as_f64().expect("now");
    for row in extra["sgt"].as_array().expect("sgt") {
        let part = |i: usize| row[i].as_u64().expect("clock part") as u32;
        assert_eq!(
            parse_sgt_stamp(part(0), part(1), part(2), now),
            row[3].as_f64(),
            "{row}"
        );
    }
    let signature = json!({
        "verdicts": [
            {"row_id": "b", "verdict": "wake"},
            {"row_id": "a", "verdict": "stale"},
            {"row_id": "c", "verdict": "leave"},
        ],
        "terminal_harness_rows": 2,
        "provider_outages": {"breakers": [{"provider": "p", "account": null, "outage_epoch": 5}]},
    });
    assert_eq!(
        verdict_signature(&signature),
        extra["signature"].as_str().expect("signature")
    );
    assert_eq!(
        union_signature(&["b;a", "c;a"]),
        extra["union"].as_str().expect("union")
    );
    let digest = json!({
        "verdicts": [
            {"verdict": "wake", "name": "w", "basis": "b1", "row_id": "1"},
            {"verdict": "leave", "name": "l", "basis": "b2", "row_id": "2"},
        ],
        "counts": {"wake": 1, "leave": 1},
        "terminal_harness_rows": 1,
    });
    assert_eq!(
        digest_text(&digest, 8),
        extra["digest"].as_str().expect("digest")
    );
    let lines = provider_outage_lines(Some(&json!({
        "breakers": [{"provider": "p", "account": "a", "kind": "k", "reset_at": null}],
        "refusals": [{"reason": "r"}, {"reason": "r"}, {"reason": "q"}],
    })));
    assert_eq!(json!(lines), extra["outage_lines"]);
}

#[test]
fn a_git_denial_needs_a_real_git_path_segment() {
    assert!(names_git_dir("cannot lock .git/index"));
    assert!(names_git_dir(r"open .git\HEAD"));
    assert!(!names_git_dir("cannot read foo.git/index"));
    assert!(!names_git_dir("a_.git/x"));
    assert!(!names_git_dir(".github/workflows"));
}
