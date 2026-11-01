//! parity-stage: characterization
//! parity-oracle: fno.agents.cli.cmd_spawn
//!
//! Characterization for `spawn_bare`: the bare spawn's thread seat and fence
//! refusal, the `--wait` grammar, the gate's waitable reasons and the sandbox
//! write policy refusal. Each `tests/golden/bare_spawn/*.json` case holds what
//! `cmd_spawn`'s leg answered, captured before the seam delete removed it.

use fno_agents::duration::parse_wait_secs;
use fno_agents::harness_capabilities::HarnessContract;
use fno_agents::spawn_bare::{sandbox_block, thread_refusal, WAITABLE_REFUSAL_REASONS};
use serde_json::Value;
use std::path::PathBuf;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/bare_spawn")
}

fn golden(name: &str) -> Value {
    let path = golden_dir().join(format!("{name}.json"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn thread_refusals_match_the_python_door() {
    let contract = HarnessContract::packaged().expect("packaged contract");
    let mut seen = 0;
    for entry in std::fs::read_dir(golden_dir()).expect("golden dir") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        let Some(case) = name.strip_suffix(".json") else {
            continue;
        };
        if !(case.starts_with("thread_") || case.starts_with("fence_")) {
            continue;
        }
        let golden = golden(case);
        let harness = golden["harness"].as_str().expect("harness");
        let want = golden["line"].as_str().map(str::to_string);
        assert_eq!(
            thread_refusal(&contract, harness, &strings(&golden["fenced"])),
            want,
            "{case}"
        );
        seen += 1;
    }
    assert!(seen >= 9, "only {seen} thread cases replayed");
}

#[test]
fn wait_grammar_and_waitable_set_match_the_python_door() {
    let golden_waits = golden("wait_grammar");
    for (raw, want) in golden_waits["waits"].as_object().expect("waits") {
        assert_eq!(parse_wait_secs(raw), want.as_f64(), "{raw:?}");
    }
    let mut ours: Vec<String> = WAITABLE_REFUSAL_REASONS
        .iter()
        .map(|s| s.to_string())
        .collect();
    ours.sort();
    assert_eq!(ours, strings(&golden("waitable_reasons")["reasons"]));
}

#[test]
fn sandbox_refusals_match_the_python_door() {
    let dir = std::env::temp_dir().join(format!("bare-spawn-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("tmp");
    for case in ["sandbox_disabled", "sandbox_missing"] {
        let golden = golden(case);
        let path = dir.join(format!("{case}.json"));
        std::fs::write(&path, golden["policy"].to_string()).expect("policy");
        let path = path.to_string_lossy().into_owned();
        let (line, exit) = sandbox_block(&path, "thread", "claude").expect_err(case);
        assert_eq!(
            exit as i64,
            golden["exit"].as_i64().expect("exit"),
            "{case}"
        );
        let want = golden["stderr"]
            .as_str()
            .expect("stderr")
            .replace("<path>", &path);
        assert_eq!(format!("{line}\n"), want, "{case}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
