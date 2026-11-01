//! parity-stage: characterization
//! parity-oracle: fno.agents.provider_outage

use fno_agents::watchdog::outage::{
    collect_pane_evidence, collect_transcript_evidence, fold_provider_outages, measure_and_persist,
    pane_status, EvidenceIdentity, OutageEvidence, OutagePolicy,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn golden(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/watchdog")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Compared as text: Value equality ignores key order, and the order is part of
/// the contract.
fn same(actual: &Value, expected: &Value, label: &str) {
    assert_eq!(actual.to_string(), expected.to_string(), "{label}");
}

fn records_of(value: &Value) -> Vec<OutageEvidence> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|item| OutageEvidence::from_json(item).unwrap())
        .collect()
}

#[test]
fn the_fold_answers_the_frozen_reports_and_journal_states() {
    let frozen = golden("outage_fold.json");
    let now_s = frozen["now_s"].as_f64().unwrap();
    for case in frozen["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let records = records_of(&case["records"]);
        let prior = Some(&case["prior"]).filter(|value| !value.is_null());
        let (report, state) =
            fold_provider_outages(&records, prior, now_s, &OutagePolicy::default());
        same(&report, &case["report"], name);
        same(&state, &case["state"], name);
    }
}

#[test]
fn the_transcript_collector_answers_the_frozen_records_and_refusals() {
    let frozen = golden("outage_transcript.json");
    let identities: Vec<EvidenceIdentity> = frozen["identities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| EvidenceIdentity {
            row_id: item["row_id"].as_str().unwrap().to_string(),
            harness: item["harness"].as_str().unwrap().to_string(),
            provider: item["provider"].as_str().map(str::to_string),
            account: item["account"].as_str().map(str::to_string),
            session_id: item["session_id"].as_str().unwrap().to_string(),
            cwd: item["cwd"].as_str().unwrap().to_string(),
        })
        .collect();
    let entries = frozen["entries"].clone();
    let (records, refusals) = collect_transcript_evidence(
        &identities,
        frozen["now_s"].as_f64().unwrap(),
        600.0,
        &mut |row: &str| entries.get(row).and_then(Value::as_array).cloned(),
        &|identity: &EvidenceIdentity| {
            Some(PathBuf::from(format!("/t/{}.jsonl", identity.session_id)))
        },
        &|_: &str| Some("Asia/Singapore".to_string()),
    );
    let records: Vec<Value> = records.iter().map(OutageEvidence::to_json).collect();
    same(&Value::Array(records), &frozen["records"], "records");
    same(&Value::Array(refusals), &frozen["refusals"], "refusals");
    for (text, status) in frozen["pane_status"].as_object().unwrap() {
        assert_eq!(pane_status(text), status.as_i64(), "{text}");
    }
}

fn pane_identity(row: &str) -> EvidenceIdentity {
    EvidenceIdentity {
        row_id: row.to_string(),
        harness: "codex".to_string(),
        provider: Some("openai".to_string()),
        account: Some("a1".to_string()),
        session_id: row.to_string(),
        cwd: "/w".to_string(),
    }
}

#[test]
fn a_pane_votes_only_after_its_screen_reads_back_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let snapshots = dir.path().join("snapshots");
    let identities = vec![
        pane_identity("voted"),
        pane_identity("no-mux"),
        pane_identity("read-fails"),
    ];
    let mux: HashMap<String, Value> = HashMap::from([
        ("voted".to_string(), json!({"session": "m", "pane_id": 3})),
        (
            "read-fails".to_string(),
            json!({"session": "m", "pane_id": "9"}),
        ),
    ]);
    let (records, refusals) = collect_pane_evidence(
        &identities,
        &mux,
        1_790_000_000.5,
        &snapshots,
        &mut |_: &str, pane: &str| {
            if pane == "3" {
                Ok("  API Error: 529 overloaded\n".to_string())
            } else {
                Err("it's gone".to_string())
            }
        },
        &|_: &str| None,
    );
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].raw_status, Some(529));
    assert_eq!(records[0].content, "API Error: 529 overloaded");
    assert_eq!(records[0].pane_id.as_deref(), Some("3"));
    let written = snapshots.join(format!("{}.json", records[0].fingerprint()));
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(written).unwrap()).unwrap();
    assert_eq!(persisted["mux_session"], "m");
    same(
        &Value::Array(refusals),
        &json!([
            {"row_id": "no-mux", "reason": "pane_identity_missing", "count": 1},
            {"row_id": "read-fails", "reason": "pane_read_failed",
             "detail": "RuntimeError(\"it's gone\")", "count": 1},
        ]),
        "refusals",
    );
}

#[test]
fn the_journal_keeps_breaker_epochs_and_a_damaged_one_reads_unknown() {
    let frozen = golden("outage_fold.json");
    let now_s = frozen["now_s"].as_f64().unwrap();
    let records = records_of(&frozen["cases"][0]["records"]);
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("recovery").join("provider-outages.json");
    let policy = OutagePolicy::default();

    let first = measure_and_persist(&records, now_s, Some(&journal), &policy);
    let opened = first["breakers"][0]["opened_at"].clone();
    assert_eq!(opened, json!(now_s));
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(&journal).unwrap()).unwrap();
    assert_eq!(on_disk["version"], 1);

    let later = measure_and_persist(&[], now_s + 60.0, Some(&journal), &policy);
    assert_eq!(later["breakers"][0]["opened_at"], opened);
    assert_eq!(later["instrument"], "measured");

    std::fs::write(&journal, "{ damaged").unwrap();
    let damaged = measure_and_persist(&records, now_s, Some(&journal), &policy);
    same(
        &damaged,
        &json!({
            "instrument": "unknown",
            "breakers": [],
            "counts": {"journal_unreadable": 1},
            "refusals": [{"reason": "journal_unreadable", "path": journal.display().to_string()}],
        }),
        "damaged",
    );
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), "{ damaged");
}
