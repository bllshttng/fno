//! parity-stage: characterization
//! parity-oracle: cli/src/fno/tracker/__init__.py
//!
//! The goldens under tests/golden/tracker/ were captured from the Python seam
//! package by tests/fixtures/tracker/capture_driver.py before the package was
//! deleted. This test is the Rust half of the protocol: it rebuilds the
//! identical fixture tree (the driver's `--build-only` writes cases.json with
//! no leg run; the test seeds the store through `graph_store::seed_rows` and
//! writes the same per-id sidecar files), drives the tracker door
//! (`tracker::run_door`, the same entry `fno-agents graph-get`'s stdin form
//! serves), and compares against the frozen envelopes. The driver's capture
//! mode can no longer re-freeze a golden after the deletion: a golden can
//! only be captured from a live leg.
//!
//! Envelope rules, all driver-side (see the driver docstring): the case root
//! normalizes to `<ROOT>`; a sidecar map is SPARSE (null fields dropped, the
//! three list fields present at `[]`); `row_after` masks the write stamp
//! (`touched_at` value) and the `_reading` annotation the strict read-back
//! attaches, neither of which this test can or should reproduce. Everything
//! else must match byte for byte.

mod common;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const SUBJECT: &str = "tracker";

/// The fields the store stamps on a write, which the write envelope masks:
/// the mutation clock (`touched_at`) and the reading annotation the strict
/// read-back attaches (`_reading`). Neither is the sidecar write's contract.
const MASKED_ROW_KEYS: [&str; 2] = ["touched_at", "_reading"];

fn python() -> PathBuf {
    common::python_executable()
}

/// The driver's case table, built import-free (`--build-only`).
fn build_cases() -> (tempfile::TempDir, BTreeMap<String, Value>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(python())
        .arg(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tracker/capture_driver.py"),
        )
        .arg("--build-only")
        .arg(dir.path())
        .output()
        .expect("run capture driver --build-only");
    assert!(
        out.status.success(),
        "build-only failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let cases: BTreeMap<String, Value> = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join("cases.json")).expect("cases.json"),
    )
    .expect("parse cases.json");
    (dir, cases)
}

/// The stamped seed shape the driver's `_seeded_rows` applies (the typed
/// store drops a row the model cannot parse, so seeds carry the stamped
/// fields).
fn seeded(case: &Value) -> Vec<Value> {
    let Some(rows) = case.get("rows").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .map(|e| {
            let mut row = json!({"type": "feature", "priority": "p2", "status": "idea"});
            let obj = row.as_object_mut().unwrap();
            for (k, v) in e.as_object().expect("row object") {
                obj.insert(k.clone(), v.clone());
            }
            let fallback = e
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("node")
                .to_string();
            obj.entry("title".to_string())
                .or_insert_with(|| json!(fallback));
            obj.entry("slug".to_string())
                .or_insert_with(|| json!(fallback));
            row
        })
        .collect()
}

/// One throwaway state root per case: the store seeded, the per-id sidecar
/// files written, env pinned. Returns the case root (whose `state/` subtree
/// the env pins).
fn setup_case(case: &Value) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let state = root.path().join("state");
    let graph = state.join("db").join("graph.json");
    if case.get("corrupt").and_then(Value::as_bool) == Some(true) {
        std::fs::create_dir_all(graph.parent().unwrap()).expect("mkdir db");
        let bytes = b"this fixture store is deliberately corrupt\n";
        std::fs::write(&graph, bytes).expect("write corrupt anchor");
        std::fs::write(graph.with_extension("db"), bytes).expect("write corrupt store");
    } else if !case
        .get("rows")
        .and_then(Value::as_array)
        .unwrap_or(&Vec::new())
        .is_empty()
    {
        fno_agents::graph_store::seed_rows(&graph, &seeded(case)).expect("seed store");
    } else {
        std::fs::create_dir_all(graph.parent().unwrap()).expect("mkdir db");
    }
    if let Some(files) = case.get("sidecar_files").and_then(Value::as_object) {
        let dir = state.join("sidecar");
        std::fs::create_dir_all(&dir).expect("mkdir sidecar");
        for (name, body) in files {
            let text = match body {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            std::fs::write(dir.join(name), text).expect("write sidecar file");
        }
    }
    (root, state)
}

/// Pin the hermetic env for one door call: the state root the fixture lives
/// under plus the case's backend selection. Returns the guard for the shared
/// env lock, since the door's backend resolution reads process env.
struct PinnedEnv {
    _guard: std::sync::MutexGuard<'static, ()>,
}

fn pin_env(state: &std::path::Path, case: &Value) -> PinnedEnv {
    let guard = fno_agents::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    std::env::set_var("FNO_STATE_DIR", state);
    // The door reads the store through the same resolver the Python side
    // pins with FNO_STATE_DIR; FNO_HOME would point it at another root.
    std::env::remove_var("FNO_HOME");
    match case.get("env").and_then(Value::as_object) {
        Some(env) => {
            for (k, v) in env {
                std::env::set_var(k, v.as_str().unwrap_or_default());
            }
        }
        None => {
            std::env::remove_var("FNO_TRACKER_BACKEND");
        }
    }
    PinnedEnv { _guard: guard }
}

/// Replace every occurrence of the case root (and its realpath spelling)
/// with `<ROOT>` in a JSON tree - the driver's own normalization rule.
fn normalize(value: &mut Value, root: &std::path::Path) {
    match value {
        Value::String(s) => {
            let markers = [root.to_string_lossy().to_string(), {
                let p = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
                p.to_string_lossy().to_string()
            }];
            for m in markers {
                if !m.is_empty() {
                    *s = s.replace(&m, "<ROOT>");
                }
            }
        }
        Value::Array(a) => {
            for v in a {
                normalize(v, root);
            }
        }
        Value::Object(o) => {
            for (_, v) in o.iter_mut() {
                normalize(v, root);
            }
        }
        _ => {}
    }
}

/// The Rust answer for one case, as the envelope Value.
fn run_case(key: &str, case: &Value) -> Value {
    let script = case.get("script").and_then(Value::as_str).unwrap_or("");
    let args = case.get("args").cloned().unwrap_or_else(|| json!({}));
    match script {
        "backend" => {
            let explicit = args.get("explicit").and_then(Value::as_str);
            let name = fno_agents::tracker::backend_name(explicit);
            json!({"backend": name})
        }
        "sidecar_get" => {
            let id = args.get("id").and_then(Value::as_str).expect("case id");
            let out = fno_agents::tracker::run_door(&json!({"tracker": "sidecar-get", "id": id}));
            json!({"sidecar": out.get("sidecar").cloned().unwrap_or(Value::Null)})
        }
        "sidecar_all" => {
            let out = fno_agents::tracker::run_door(&json!({"tracker": "sidecar-all"}));
            json!({"sidecars": out.get("sidecars").cloned().unwrap_or(Value::Null)})
        }
        "sidecar_set_merge" => {
            let id = args.get("id").and_then(Value::as_str).expect("case id");
            // The load-mutate-save roundtrip the Python case ran, expressed
            // as the fields the loaded map carried after mutation.
            let fields = json!({"cwd": "/new", "plan_path": "/p.md", "pr_number": 42});
            let out = fno_agents::tracker::run_door(&json!({
                "tracker": "sidecar-set", "id": id, "fields": fields
            }));
            let saved = out.get("saved").cloned().unwrap_or(Value::Null);
            // The strict read-back (the same shape the driver froze: the row
            // as the store serves it).
            let graph = std::env::var("FNO_STATE_DIR")
                .map(|d| std::path::PathBuf::from(d).join("db").join("graph.json"))
                .unwrap_or_default();
            let rows = fno_agents::graph_store::read_rows_where(
                &graph,
                &fno_agents::backlog::RowQuery::default(),
            )
            .expect("read back rows");
            let row = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
                .unwrap_or(Value::Null);
            json!({"saved": saved, "row_after": row})
        }
        "sidecar_set_missing" => {
            let out = fno_agents::tracker::run_door(&json!({
                "tracker": "sidecar-set", "id": "ab-ghost", "fields": {"cwd": "/nowhere"}
            }));
            if out.get("not_found").and_then(Value::as_bool) == Some(true) {
                json!({"not_found": "ab-ghost"})
            } else {
                json!({"unexpected": out})
            }
        }
        "sidecar_set_external" => {
            let id = args.get("id").and_then(Value::as_str).expect("case id");
            let fields = json!({"cwd": "/one", "plan_path": "/p.md", "pr_number": 5});
            let out = fno_agents::tracker::run_door(&json!({
                "tracker": "sidecar-set", "id": id, "fields": fields
            }));
            let saved = out
                .get("saved")
                .and_then(Value::as_str)
                .expect("saved path")
                .to_string();
            let file_after: Value =
                serde_json::from_str(&std::fs::read_to_string(&saved).expect("read sidecar file"))
                    .expect("parse sidecar file");
            json!({"saved": saved, "file_after": file_after})
        }
        other => panic!("unknown case script {other} in {key}"),
    }
}

/// Mask the write stamp and the reading annotation on a frozen row_after so
/// the compare sees the write's contract, not the clock.
fn mask_row(value: &mut Value) {
    if let Some(obj) = value.get_mut("row_after").and_then(Value::as_object_mut) {
        for k in MASKED_ROW_KEYS {
            obj.remove(k);
        }
    }
}

#[test]
fn tracker_door_matches_the_frozen_python_seam() {
    let (_build, cases) = build_cases();
    assert!(
        cases.contains_key("sidecar_get_hit"),
        "the case table must carry the door cases"
    );
    for (key, case) in &cases {
        let (root, state) = setup_case(case);
        {
            let _env = pin_env(&state, case);
            let mut rust = run_case(key, case);
            normalize(&mut rust, root.path());
            if key.starts_with("sidecar_set") {
                mask_row(&mut rust);
            }
            let frozen =
                std::fs::read_to_string(common::golden_dir(SUBJECT).join(format!("{key}.out")))
                    .unwrap_or_else(|e| panic!("golden {key}.out: {e}"));
            // The compare is structural: serde_json object equality is
            // order-insensitive, and key order is not the contract - the
            // VALUES are.
            let mut golden_value: Value = serde_json::from_str(frozen.trim())
                .unwrap_or_else(|e| panic!("parse golden {key}: {e}"));
            mask_row(&mut golden_value);
            assert_eq!(
                rust, golden_value,
                "case {key}: the door's answer diverged from the frozen Python seam"
            );
        }
    }
}
