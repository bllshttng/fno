//! parity-stage: characterization
//! parity-oracle: fno.dispatch_flags
//! Normalize the JSON door envelope to the old function return/exception surface.

use serde_json::{json, Value};
use std::process::Command;

#[test]
fn dispatch_harness_goldens_replay_every_payload() {
    let golden: Value =
        serde_json::from_str(include_str!("golden/dispatch_harness/cases.json")).unwrap();
    for case in golden["cases"].as_array().unwrap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("config.toml"), case["config"].as_str().unwrap()).unwrap();
        let mut payload = case["payload"].clone();
        payload["cwd"] = json!(root);
        let input = root.join("payload.json");
        std::fs::write(&input, payload.to_string()).unwrap();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("FNO_") {
                cmd.env_remove(key);
            }
        }
        let result = cmd
            .current_dir(root)
            .env("HOME", root)
            .env("FNO_CONFIG", root.join("config.toml"))
            .env("FNO_GLOBAL_SETTINGS_PATH", root.join("absent.json"))
            .env("FNO_NO_CANONICAL_CONFIG", "1")
            .env("FNO_STATE_DIR", root.join("state"))
            .env("FNO_AGENTS_HOME", root.join("agents"))
            .args(["route-slot"])
            .arg(input)
            .output()
            .unwrap();
        let label = case["label"].as_str().unwrap();
        assert!(
            result.status.success(),
            "{label}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(result.stderr.is_empty(), "{label}: unexpected stderr");
        let answer: Value = serde_json::from_slice(&result.stdout).unwrap();
        if let Some(refusal) = answer.get("refusal").and_then(Value::as_str) {
            assert_eq!(case["exit"], 1, "{label}");
            assert_eq!(
                format!("DispatchFlagError: {refusal}"),
                case["err"].as_str().unwrap(),
                "{label}"
            );
            assert_eq!(case["out"], "");
            continue;
        }
        assert_eq!(case["exit"], 0, "{label}: missing refusal");
        assert_eq!(case["err"], "");
        match payload["mode"].as_str().unwrap() {
            "configured" => {
                let expected: Value = serde_json::from_str(case["out"].as_str().unwrap()).unwrap();
                assert_eq!(answer, expected, "{label}");
            }
            "validate-model" => {
                let expected = &payload["model"];
                assert_eq!(&answer["model"], expected, "{label}");
                if expected.is_null() {
                    assert_eq!(case["out"], "null");
                } else {
                    assert_eq!(&case["out"], expected);
                }
            }
            _ if label == "infer returns none on contradiction" => {
                assert_eq!(case["out"], "null");
                assert_eq!(answer["source"], "builtin-default");
                assert_eq!(answer["harness"], "claude");
            }
            _ => {
                let expected: Value = serde_json::from_str(case["out"].as_str().unwrap()).unwrap();
                assert_eq!(
                    json!([answer["harness"], answer["source"]]),
                    expected,
                    "{label}"
                );
            }
        }
    }
}
