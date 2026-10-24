//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.cli
//!
//! Differential parity for `accounts::pick`, the port of `pick_account` and
//! the `pick` command. Each `tests/golden/accounts_pick/*.json` case carries
//! the files it set up and the Python command's stdout, stderr and exit
//! code under `expect`, captured at `NOW` by `accounts_pick_golden_driver.py`
//! (the case table lives there) with the usage probe failing and the window
//! stamp a no-op. The replay pins the same env, so it runs in one test.

use fno_agents::accounts::pick::{self, World};
use fno_agents::accounts::records::Record;
use serde_json::Value;
use std::path::{Path, PathBuf};

const NOW: f64 = 1_789_400_000.0;

fn pin_env(dir: &Path) {
    for (key, value) in [
        ("HOME", dir.to_path_buf()),
        ("PWD", dir.to_path_buf()),
        ("FNO_CONFIG", dir.join("config.toml")),
        ("FNO_STATE_DIR", dir.join("state")),
        ("FNO_AGENTS_HOME", dir.join("agents")),
        ("FNO_RUNTIME_STATE_PATH", dir.join("runtime-state.json")),
        ("FNO_GLOBAL_SETTINGS_PATH", dir.join("absent-global.json")),
        ("FNO_NO_CANONICAL_CONFIG", PathBuf::from("1")),
    ] {
        std::env::set_var(key, value);
    }
}

fn run_case(case: &Value, dir: &Path) -> Value {
    let shown = dir.display().to_string();
    for f in case["files"].as_array().unwrap() {
        let path = dir.join(f["rel"].as_str().unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, f["text"].as_str().unwrap().replace("{dir}", &shown)).unwrap();
    }
    pin_env(dir);
    let args: Vec<String> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    let mut combo = None;
    let mut exclude = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.split_once('=') {
            Some(("--exclude", v)) => exclude.push(v.to_string()),
            _ if arg == "--exclude" => exclude.push(it.next().unwrap().clone()),
            _ if arg == "--combo" => combo = it.next().cloned(),
            _ => {}
        }
    }
    let has = |flag: &str| args.iter().any(|a| a == flag);
    let as_json = has("--json") || has("-J");
    let refresh = |_: &Record| {};
    let stamp = |_: &str| {};
    let verdict = pick::pick(
        &World {
            cwd: dir,
            now: NOW,
            refresh: &refresh,
            stamp: &stamp,
        },
        combo.as_deref(),
        &exclude,
        has("--if-armed"),
    )
    .unwrap();
    let env = match (&verdict.account, has("--print-env") && !as_json) {
        (Some(id), true) => Some(pick::env_lines(id, dir).unwrap()),
        _ => None,
    };
    let (err, out) = pick::render(&verdict, as_json, env.as_deref());
    let joined = |lines: Vec<String>| -> String {
        lines
            .into_iter()
            .map(|l| format!("{}\n", l.replace(&shown, "{dir}")))
            .collect()
    };
    serde_json::json!({
        "stdout": joined(out),
        "stderr": joined(err),
        "exit": verdict.exit_code,
    })
}

#[test]
fn accounts_pick_matches_the_python_command() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_pick");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 19, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|golden| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            // The capture resolved its scratch dir, so the replay does too.
            let root = tmp.path().canonicalize().unwrap();
            let got = run_case(&case, &root);
            (got != case["expect"]).then(|| {
                format!(
                    "{}\n  expect: {}\n  rust:   {got}",
                    golden.file_name().unwrap().to_string_lossy(),
                    case["expect"]
                )
            })
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
