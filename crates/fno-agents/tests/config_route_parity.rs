//! parity-stage: characterization
//! parity-oracle: fno.route_cli
//! Replay the frozen inputs through the command door, including scope-local plans.

use serde_json::{json, Value};
use std::process::{Command, Output};

fn command(tmp: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FNO_") {
            cmd.env_remove(key);
        }
    }
    cmd.current_dir(tmp)
        .env("HOME", tmp.join("home"))
        .env("FNO_CONFIG", tmp.join("config.toml"))
        .env("FNO_GLOBAL_SETTINGS_PATH", tmp.join("global/absent.json"))
        .env("FNO_NO_CANONICAL_CONFIG", "1")
        .env("FNO_AGENTS_HOME", tmp.join("agents-home"))
        .env("FNO_STATE_DIR", tmp.join("state"))
        .env_remove("ZAI_API_KEY");
    cmd
}

fn assert_capture(label: &str, result: &Output, case: &Value) {
    assert_eq!(
        result.status.code(),
        case["exit"].as_i64().map(|v| v as i32),
        "{label}"
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stdout),
        case["out"].as_str().unwrap(),
        "{label} stdout"
    );
    assert_eq!(
        String::from_utf8_lossy(&result.stderr),
        case["err"].as_str().unwrap(),
        "{label} stderr"
    );
}

#[test]
fn config_route_goldens_replay_reads_and_write_plans() {
    let golden: Value =
        serde_json::from_str(include_str!("golden/config_route/cases.json")).unwrap();
    for case in golden["cases"].as_array().unwrap() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("global")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(
            root.join("config.toml"),
            case["config"].as_str().unwrap_or(""),
        )
        .unwrap();
        let label = case["label"].as_str().unwrap();
        let args: Vec<_> = case["args"]
            .as_array()
            .map(|args| args.iter().filter_map(Value::as_str).collect())
            .unwrap_or_else(|| vec!["settings", "ls"]);
        let op = args[0];
        let mut cmd = command(root);
        if let Some(key) = case.pointer("/env/ZAI_API_KEY").and_then(Value::as_str) {
            cmd.env("ZAI_API_KEY", key);
        }
        if let Some(overlay) = case.get("overlay") {
            let dir = root.join("state/route-settings");
            std::fs::create_dir_all(&dir).unwrap();
            let path = dir.join("seeded.json");
            std::fs::write(&path, overlay.to_string()).unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            file.set_modified(
                std::time::SystemTime::now() - std::time::Duration::from_secs(20 * 86400),
            )
            .unwrap();
        }
        cmd.arg("config-route");
        match op {
            "ls" => {
                cmd.arg("table");
            }
            "set" | "unset" | "init" => {
                cmd.arg("plan-write").args(&args);
            }
            _ => {
                cmd.args(&args);
            }
        }
        let result = cmd.output().unwrap();
        if matches!(op, "set" | "unset" | "init") && case["exit"] == 0 {
            assert!(
                result.status.success(),
                "{label}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
            let plan: Value = serde_json::from_slice(&result.stdout).unwrap();
            if op == "init" {
                assert_eq!(plan["action"], "init");
                assert_eq!(plan["path"], json!(root.join("global/config.toml")));
            } else {
                assert_eq!(plan["out"], case["out"], "{label}");
                assert_eq!(plan["err"], case["err"], "{label}");
                if op == "set" {
                    assert_eq!(plan["action"], "set");
                    let (provider, model) = args[2]
                        .split_once(',')
                        .or_else(|| args[2].split_once('/'))
                        .unwrap();
                    assert_eq!(plan["value"][args[1]], format!("{provider}/{model}"));
                } else {
                    assert_eq!(plan["action"], "noop");
                }
            }
        } else if op == "ls" {
            assert!(result.status.success());
            let rows: Value = serde_json::from_slice(&result.stdout).unwrap();
            if args.contains(&"--json") {
                assert_capture(label, &result, case);
            } else {
                let json_case = golden["cases"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["label"] == "ls json emits the rows")
                    .unwrap();
                assert_eq!(
                    rows,
                    serde_json::from_str::<Value>(json_case["out"].as_str().unwrap()).unwrap()
                );
            }
        } else {
            assert_capture(label, &result, case);
        }
    }
}
