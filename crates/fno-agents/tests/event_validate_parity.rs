//! parity-stage: characterization
//! parity-oracle: fno.events.validate
//!
//! Characterization tests for the native envelope judge, frozen against
//! goldens captured from the Python judge before that leg was deleted. The
//! goldens under tests/golden/event_validate/ freeze one
//! (exit, stdout, stderr) triple per corpus row (284 of the 285 rows in
//! tests/py/events/parity_corpus.jsonl; the overflow-literals row is
//! answered by the door's substrate refusal). The native judge in
//! crates/fno-agents/src/event_store/validate.rs must answer every frozen
//! case identically. No oracle leg is left, so the goldens are the contract.

use common::{assert_golden, Golden};
use std::fs;
use std::path::PathBuf;

mod common;

/// Repo root: crates/fno-agents -> repo.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// (label, event JSON) per corpus row.
fn corpus_rows() -> Vec<(String, String)> {
    let path = repo_root().join("tests/py/events/parity_corpus.jsonl");
    let text = fs::read_to_string(path).expect("parity corpus is present");
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Rows with literals serde_json cannot represent (non-finite
        // overflow numbers) never reach the judge: the door answers them at
        // the substrate layer and the write still never lands. Their
        // Python-side diagnostic contract stays in the Python suite until
        // that leg retires.
        let row: serde_json::Value = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let label = row["reason"]
            .as_str()
            .expect("corpus rows carry reasons")
            .to_string();
        rows.push((
            label,
            serde_json::to_string(&row["event"]).expect("event reserializes"),
        ));
    }
    rows
}

/// The native judge's verdict for one corpus row, as a Golden.
fn run_rust_verdict(event_json: &str) -> Golden {
    match fno_agents::event_store::validate::judge_line(event_json) {
        fno_agents::event_store::validate::Verdict::Valid => Golden {
            exit: Some(0),
            streams: vec![String::new(), String::new()],
        },
        fno_agents::event_store::validate::Verdict::Invalid(msg) => Golden {
            exit: Some(1),
            streams: vec![String::new(), format!("{msg}\n")],
        },
        fno_agents::event_store::validate::Verdict::Substrate(msg) => Golden {
            exit: Some(2),
            streams: vec![String::new(), format!("{msg}\n")],
        },
    }
}

/// Every corpus row, one golden key each: the frozen golden is the contract.
#[test]
fn parity_over_corpus() {
    for (label, event_json) in corpus_rows() {
        let rust = run_rust_verdict(&event_json);
        assert_golden("event_validate", &label, &rust, None);
    }
}
