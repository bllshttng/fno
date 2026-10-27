//! parity-stage: characterization
//! parity-oracle: fno.route_cli
//! Front render and init bytes complement the agents-door replay.

use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn route_front_replays_table_and_init_goldens() {
    let golden: Value = serde_json::from_str(include_str!(
        "../../fno-agents/tests/golden/config_route/cases.json"
    ))
    .unwrap();
    let front = PathBuf::from(env!("CARGO_BIN_EXE_fno"));
    let agents = std::env::var_os("FNO_AGENTS_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| front.with_file_name("fno-agents"));
    assert!(
        agents.is_file(),
        "build the sibling fno-agents binary for route parity"
    );
    for (index, case) in golden["cases"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, c)| c["args"][0] == "ls" || c["args"][0] == "init")
    {
        let tmp = tempfile::tempdir().unwrap();
        let home_name = if case["label"] == "init is idempotent" {
            "home-idem".into()
        } else {
            format!("home-{index}")
        };
        let home = tmp.path().join(&home_name);
        std::fs::create_dir_all(&home).unwrap();
        let config = tmp.path().join("read.toml");
        std::fs::write(&config, case["config"].as_str().unwrap()).unwrap();
        let invoke = || {
            let mut cmd = Command::new(&front);
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().starts_with("FNO_") {
                    cmd.env_remove(key);
                }
            }
            cmd.current_dir(tmp.path())
                .env("HOME", &home)
                .env("FNO_CONFIG", &config)
                .env("FNO_AGENTS_BIN", &agents)
                .env("FNO_GLOBAL_SETTINGS_PATH", home.join(".fno/absent.json"))
                .env("FNO_STATE_DIR", tmp.path().join("state"))
                .env("FNO_AGENTS_HOME", tmp.path().join("agents"))
                .env("FNO_NO_CANONICAL_CONFIG", "1")
                .env_remove("ZAI_API_KEY")
                .args(["config", "route"])
                .args(
                    case["args"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|v| v.as_str().unwrap()),
                );
            cmd.output().unwrap()
        };
        if home_name == "home-idem" {
            assert!(invoke().status.success());
        }
        let result = invoke();
        let stdout = String::from_utf8(result.stdout)
            .unwrap()
            .replace(tmp.path().to_str().unwrap(), "<CASE-TMP>");
        assert_eq!(
            result.status.code(),
            Some(case["exit"].as_i64().unwrap() as i32)
        );
        assert_eq!(stdout, case["out"].as_str().unwrap(), "{}", case["label"]);
        assert_eq!(
            String::from_utf8(result.stderr).unwrap(),
            case["err"].as_str().unwrap()
        );
        if case["args"][0] == "init" {
            let sample = std::fs::read_to_string(home.join(".fno/config.toml")).unwrap();
            assert_eq!(sample.matches("# SAMPLE routing inventory").count(), 1);
            assert!(sample
                .lines()
                .filter(|s| !s.is_empty())
                .all(|s| s.starts_with('#')));
        }
    }
}
