//! parity-stage: characterization
//! parity-oracle: fno.adapters.providers.benchmarks
//!
//! Characterization for `fno-agents spawn-overlay accounts benchmarks`, the
//! port of `fno.adapters.providers.benchmarks` and the Python `benchmarks`
//! group, which are gone. Each `tests/golden/accounts_benchmarks/*.json` case
//! with `args` holds the snapshot (under `cache/`, or `legacy` at the state
//! root), any config, and what the Python command left: exit code, stdout and
//! stderr. `{NOW}` and `{OLD}` are filled with now and now minus 20 days, so
//! the staleness verdict holds on any date. `tiers.json` is what the Python
//! `unreachable_tier_ids()` and `empty_bands_for_harness()` answered over the
//! shipped tables. Recorded deltas: a legacy root snapshot is read in place,
//! not renamed into `cache/`; a refresh writes the snapshot mode 0600
//! and gives the request curl's 10 s ceiling, not Python's 30 s.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_benchmarks")
}

fn iso(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

fn fno_agents(args: &[&str], tmp: &Path) -> std::process::Output {
    let home = tmp.join("home");
    let repo = tmp.join("repo");
    Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["spawn-overlay", "accounts", "benchmarks"])
        .args(args)
        .current_dir(&repo)
        .env_clear()
        .envs(fno_agents::test_run::self_owner_env())
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", &home)
        .env("PWD", &repo)
        .env("FNO_REPO_ROOT", &repo)
        .env("FNO_STATE_DIR", tmp.join("state"))
        .env("USER", "tester")
        .output()
        .expect("fno-agents starts")
}

fn run_case(case: &Value, tmp: &Path, now: &str, old: &str) -> Value {
    let state = tmp.join("state");
    for dir in [
        tmp.join("home/.fno"),
        tmp.join("repo/.fno"),
        state.join("cache"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let fill = |s: &str| {
        s.replace("{TMP}", tmp.to_str().unwrap())
            .replace("{NOW}", now)
            .replace("{OLD}", old)
    };
    let config = format!(
        "state_dir = \"{}\"\n{}",
        state.display(),
        fill(case["config"].as_str().unwrap_or(""))
    );
    std::fs::write(tmp.join("home/.fno/config.toml"), config).unwrap();
    for (key, path) in [
        ("snapshot", state.join("cache/benchmarks.json")),
        ("legacy", state.join("benchmarks.json")),
    ] {
        if let Some(text) = case[key].as_str() {
            std::fs::write(path, fill(text)).unwrap();
        }
    }
    let args: Vec<&str> = case["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let out = fno_agents(&args, tmp);
    let got = json!({
        "exit": out.status.code(),
        "stdout": String::from_utf8_lossy(&out.stdout),
        "stderr": String::from_utf8_lossy(&out.stderr),
    });
    let text = serde_json::to_string(&got)
        .unwrap()
        .replace(tmp.to_str().unwrap(), "<TMP>")
        .replace(now, "{NOW}")
        .replace(old, "{OLD}");
    serde_json::from_str(&text).unwrap()
}

#[test]
fn accounts_benchmarks_matches_the_python_command() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(golden_dir())
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .filter(|p| p.file_name().is_some_and(|n| n != "tiers.json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 6, "golden cases missing: {}", cases.len());
    let now_t = chrono::Utc::now();
    let (now, old) = (iso(now_t), iso(now_t - chrono::Duration::days(20)));
    let mut failures = Vec::new();
    for path in &cases {
        let case: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let got = run_case(&case, &root, &now, &old);
        if got != case["expect"] {
            failures.push(format!(
                "{}\n  expect: {}\n  rust:   {}",
                path.file_name().unwrap().to_string_lossy(),
                case["expect"],
                got
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} case(s) differ:\n{}",
        failures.len(),
        failures.join("\n")
    );

    let tmp = tempfile::tempdir().unwrap();
    for dir in ["home", "repo"] {
        std::fs::create_dir_all(tmp.path().join(dir)).unwrap();
    }
    let out = fno_agents(&["tiers"], tmp.path());
    assert!(out.status.success());
    let got: Value = serde_json::from_slice(&out.stdout).unwrap();
    let want: Value =
        serde_json::from_str(&std::fs::read_to_string(golden_dir().join("tiers.json")).unwrap())
            .unwrap();
    assert_eq!(got, want);
}
