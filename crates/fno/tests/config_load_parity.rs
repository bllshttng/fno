//! parity-stage: differential
//! parity-oracle: fno.config._loader._load_settings_at
//!
//! The config loader port, held against goldens captured from the Python
//! loader through `config_load_driver.py --capture` (the committed oracle).
//! The Python loader stays live until the config family deletes it; this
//! file then flips to characterization with the oracle gone.
//! Every pydantic validator on the settings model tree has a row in
//! `validators.tsv`; `cases.jsonl` holds the probes the driver kept for it,
//! plus one bad value per schema leaf type and the no-file defaults. Each
//! case is one pinned `settings.yaml`, so the native side replays it as one
//! layer named `<FIXTURE>/settings.yaml`: the same alias pass and unwrap the
//! reader runs, then `validate_layer_list`. A loaded case compares the
//! validated value at the probed loc; a refused case compares the refusal
//! text byte for byte; both compare the stderr lines. The host owns one
//! fold: Python prints a warning's file:line before its category, and the
//! driver drops it.

use fno::config_cli::model::{drop_retired_dispatch_keys, validate_layer_list};
use fno::config_cli::{alias_legacy_keys, parse_config_file, unwrap_config_dict, yaml_to_json};
use serde_json::Value;
use std::path::{Path, PathBuf};

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/config_load");

fn rows(name: &str) -> Vec<Value> {
    let text = std::fs::read_to_string(Path::new(GOLDEN).join(name)).expect("golden readable");
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| serde_json::from_str(l).expect("golden row parses"))
        .collect()
}

fn at<'a>(value: &'a Value, loc: &[Value]) -> Option<&'a Value> {
    let mut node = value;
    for part in loc {
        node = match part {
            Value::Number(n) => node.as_array()?.get(n.as_u64()? as usize)?,
            Value::String(key) => node.as_object()?.get(key)?,
            _ => return None,
        };
    }
    Some(node)
}

fn replay(case: &Value) -> (i64, String, Value) {
    let text = case["yaml"].as_str().unwrap_or_default();
    let parsed: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).expect("case yaml parses");
    let mut raw = yaml_to_json(parsed);
    alias_legacy_keys(&mut raw);
    let layers = vec![(
        PathBuf::from("<FIXTURE>/settings.yaml"),
        unwrap_config_dict(&raw),
    )];
    let outcome = validate_layer_list(&layers, false);
    let stderr: String = outcome
        .notes
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    match outcome.result {
        Ok(value) => {
            let loc = case["loc"].as_array().cloned().unwrap_or_default();
            let here = at(&value, &loc).cloned().unwrap_or(Value::Null);
            (0, stderr, here)
        }
        Err(refusal) => (1, stderr, Value::String(refusal)),
    }
}

#[test]
fn every_validator_has_a_frozen_case() {
    let tsv = std::fs::read_to_string(Path::new(GOLDEN).join("validators.tsv"))
        .expect("manifest readable");
    let validators: Vec<&str> = tsv.lines().filter(|l| !l.is_empty()).collect();
    assert!(
        validators.len() >= 152,
        "{} validators, the base had 152",
        validators.len()
    );
    let cases = rows("cases.jsonl");
    let missing: Vec<&&str> = validators
        .iter()
        .filter(|row| {
            !cases
                .iter()
                .any(|case| case["validator"].as_str() == Some(**row))
        })
        .collect();
    assert!(missing.is_empty(), "validators with no case: {missing:?}");
}

#[test]
fn native_model_answers_every_golden_case() {
    let mut diffs = Vec::new();
    let cases = rows("cases.jsonl");
    for case in &cases {
        let (exit, stderr, value) = replay(case);
        let want_exit = case["exit"].as_i64().unwrap_or(-1);
        let want_stderr = case["stderr"].as_str().unwrap_or_default();
        if exit != want_exit || stderr != want_stderr || value != case["value"] {
            diffs.push(format!(
                "{}\n  yaml: {:?}\n  want exit {want_exit} stderr {want_stderr:?} value {}\n  got  exit {exit} stderr {stderr:?} value {}",
                case["name"].as_str().unwrap_or_default(),
                case["yaml"].as_str().unwrap_or_default(),
                case["value"],
                value
            ));
        }
    }
    let shown: Vec<&String> = diffs.iter().take(25).collect();
    assert!(
        diffs.is_empty(),
        "{} of {} cases diverge:\n{}",
        diffs.len(),
        cases.len(),
        shown
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn retired_dispatch_keys_leave_disk_with_the_same_receipt() {
    for case in rows("retired.jsonl") {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = case["name"].as_str().unwrap_or_default();
        let path = dir
            .path()
            .join(name)
            .join(case["file"].as_str().unwrap_or_default());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, case["before"].as_str().unwrap_or_default()).expect("write");
        let receipts: Vec<Value> = drop_retired_dispatch_keys(&[path.clone()])
            .into_iter()
            .map(|line| Value::String(fold(&line, dir.path())))
            .collect();
        assert_eq!(Value::Array(receipts), case["receipts"], "{name}: receipts");
        let after = parse_config_file(&path).unwrap_or(Value::Object(Default::default()));
        assert_eq!(after, case["after"], "{name}: file after the drop");
    }
}

fn fold(text: &str, root: &Path) -> String {
    let resolved = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let mut out = text.to_string();
    for token in [resolved.display().to_string(), root.display().to_string()] {
        out = out.replace(&token, "<FIXTURE>");
    }
    out
}
