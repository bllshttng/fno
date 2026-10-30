//! parity-stage: differential
//! parity-oracle: fno.paths
//!
//! Differential parity for the path hub: the eleven accessors `fno config
//! doctor` prints, resolved by the Python hub and by the Rust owners on the
//! same fixture. Each case in `tests/golden/paths_hub/cases.json` builds a
//! HOME, a project dir and an optional settings file under one fixture root.
//! The Rust answers come from the live shell stub (`emit_paths_sh_live`,
//! sourced by bash), `backlog::settings::graph_path`, `paths::space_dir` and
//! the fno crate's `agents_view::registry_path` (the mux's registry reader,
//! pinned to the hub's `agents_registry_path`).
//! `<case>.out` holds the Python answers, captured by
//! `paths_hub_golden_driver.py`; with Python present the live hub is checked
//! against them too. A case's `divergent` list names the accessors where the
//! Rust owners still disagree; each one must keep disagreeing, so a fix fails
//! the test until its entry goes. The stage flips to characterization when
//! the hub is deleted.

use common::python_executable;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const SUBJECT: &str = "paths_hub";

fn driver() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/paths_hub_golden_driver.py")
}

fn python_available() -> bool {
    let cli_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/src");
    let probe = Command::new(python_executable())
        .args(["-c", "import fno.paths"])
        .env("PYTHONPATH", cli_src)
        .output();
    matches!(probe, Ok(o) if o.status.success())
}

/// The Python hub's answers for one case, built by the driver under `root`.
fn python_leg(name: &str, root: &Path) -> String {
    let out = Command::new(python_executable())
        .arg(driver())
        .arg(name)
        .arg(root)
        .output()
        .expect("run paths_hub_golden_driver.py");
    assert!(
        out.status.success(),
        "[{name}] python leg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// Same folding the driver applies: the fixture root and its space slug.
fn normalize(text: &str, root: &Path) -> String {
    let root = root.to_string_lossy();
    text.replace(&root.replace('/', "-"), "<FIXTURE_SLUG>")
        .replace(&*root, "<FIXTURE>")
}

/// Lay out one case under `root` and pin this process's env to it. Every
/// ambient `FNO_*` var is cleared first, so a CI sandbox pin cannot leak in.
fn build(case: &serde_json::Value, root: &Path) -> PathBuf {
    let home = root.join("home");
    let proj = root.join("proj");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&proj).unwrap();
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("FNO_") {
            std::env::remove_var(key);
        }
    }
    std::env::set_var("HOME", &home);
    std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
    let fixture = root.to_string_lossy();
    if let Some(config) = case["config"].as_str() {
        let cfg = root.join("settings.yaml");
        std::fs::write(&cfg, config.replace("<FIXTURE>", &fixture)).unwrap();
        std::env::set_var("FNO_CONFIG", &cfg);
    }
    if let Some(env) = case["env"].as_object() {
        for (key, value) in env {
            let value = value.as_str().unwrap().replace("<FIXTURE>", &fixture);
            std::env::set_var(key, value);
        }
    }
    std::env::set_current_dir(&proj).unwrap();
    proj
}

/// The Rust owners' answers, in the driver's accessor order.
fn rust_leg(root: &Path, proj: &Path) -> String {
    let stub = fno_agents::paths_cli::emit_paths_sh_live(proj).expect("emit_paths_sh_live");
    let stub_path = root.join("paths.sh");
    std::fs::write(&stub_path, stub).unwrap();
    let out = Command::new("bash")
        .arg("-c")
        .arg(
            "source \"$1\"; printf '%s\\n' \"$STATE_DIR\" \"$LEDGER_JSON_PATH\" \"$BRIEFS_DIR\" \
             \"$FLEET_DIR\" \"$POSTMORTEMS_DIR\" \"$WORKTREES_BASE\" \"$MEMORY_DIR\" \"$HOOK_LOGS_DIR\"",
        )
        .arg("paths_hub")
        .arg(&stub_path)
        .current_dir(proj)
        .output()
        .expect("source the shell stub");
    assert!(
        out.status.success(),
        "sourcing the stub failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stub_values: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(stub_values.len(), 8, "stub printed {stub_values:?}");
    let graph = fno_agents::backlog::settings::graph_path();
    let space = fno_agents::paths::space_dir(proj);
    let registry = fno::agents_view::registry_path();
    let rows = [
        ("state_dir", stub_values[0].clone()),
        ("graph_json", graph.to_string_lossy().into_owned()),
        ("ledger_json", stub_values[1].clone()),
        ("briefs_dir", stub_values[2].clone()),
        ("fleet_dir", stub_values[3].clone()),
        ("postmortems_dir", stub_values[4].clone()),
        ("worktrees_base", stub_values[5].clone()),
        ("memory_dir", stub_values[6].clone()),
        ("hook_logs_dir", stub_values[7].clone()),
        ("space_dir", space.to_string_lossy().into_owned()),
        (
            "agents_registry_path",
            registry.to_string_lossy().into_owned(),
        ),
    ];
    rows.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

#[test]
fn the_doctor_accessors_resolve_alike_in_both_legs() {
    let _env = common::env_mutex();
    let saved_cwd = std::env::current_dir().unwrap();
    let cases: Vec<serde_json::Value> = serde_json::from_str(
        &std::fs::read_to_string(common::golden_dir(SUBJECT).join("cases.json")).unwrap(),
    )
    .unwrap();
    let live = python_available();
    for case in &cases {
        let name = case["name"].as_str().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let proj = build(case, &root);
        let rust = normalize(&rust_leg(&root, &proj), &root);
        let golden =
            std::fs::read_to_string(common::golden_dir(SUBJECT).join(format!("{name}.out")))
                .unwrap();
        if live {
            assert_eq!(
                python_leg(name, &root),
                golden,
                "[{name}] the Python hub drifted from its golden; recapture with \
                 paths_hub_golden_driver.py"
            );
        }
        let divergent: Vec<&str> = case["divergent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(
            golden.lines().count(),
            rust.lines().count(),
            "[{name}] accessor count differs:\n{rust}"
        );
        for (want, got) in golden.lines().zip(rust.lines()) {
            let accessor = want.split('=').next().unwrap();
            if divergent.contains(&accessor) {
                assert_ne!(
                    want, got,
                    "[{name}] {accessor} now agrees with Python; drop it from the case's divergent list"
                );
            } else {
                assert_eq!(want, got, "[{name}] {accessor}: Python and Rust disagree");
            }
        }
    }
    std::env::set_current_dir(saved_cwd).unwrap();
}
