//! parity-stage: characterization
//! parity-oracle: fno.agents.watchdog
//!
//! The recoverable lane against goldens frozen from the Python leg: the since
//! and cwd refusals, the strict scan over a fixture store, the payload keys in
//! order, the refusal rows, and one real adoption.

use fno_agents::watchdog::recoverable::{
    apply_recoverable_with, is_canonical_full_uuid, parse_recovery_since, recovery_result_counts,
    resolve_recovery_cwd, scan_recoverable_codex_rollouts, sweep_from_scan, RecoverableRow, Scan,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, UNIX_EPOCH};

const PURE: &str = include_str!("golden/watchdog/recoverable/pure.json");
const SCAN: &str = include_str!("golden/watchdog/recoverable/scan.json");

/// The adoption writes through the real store, so the state roots stay pinned
/// to a scratch directory for the whole test process.
fn pin_state_roots() {
    static PIN: Once = Once::new();
    PIN.call_once(|| {
        let scratch = tempfile::tempdir().unwrap().keep();
        std::env::set_var("FNO_STATE_DIR", scratch.join("state"));
        std::env::set_var("FNO_AGENTS_HOME", scratch.join("state").join("agents"));
        std::env::remove_var("FNO_AGENT_SELF");
    });
}

fn scrubbed(value: &Value, root: &Path) -> Value {
    let text = value
        .to_string()
        .replace(&root.display().to_string(), "<ROOT>");
    serde_json::from_str(&text).unwrap()
}

/// Same bytes, so key order is part of the comparison.
fn assert_same(actual: &Value, expected: &Value, label: &str) {
    assert_eq!(actual.to_string(), expected.to_string(), "{label}");
}

struct Fixture {
    _guard: tempfile::TempDir,
    root: PathBuf,
    golden: Value,
}

impl Fixture {
    fn new(file_sets: &[&str]) -> Self {
        pin_state_roots();
        let golden: Value = serde_json::from_str(SCAN).unwrap();
        let guard = tempfile::tempdir().unwrap();
        let root = guard.path().canonicalize().unwrap();
        let now = golden["now"].as_f64().unwrap();
        for set in file_sets {
            for file in golden[*set].as_array().unwrap() {
                let path = root.join("sessions").join(file[0].as_str().unwrap());
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(&path, file[1].as_str().unwrap()).unwrap();
                let mtime = UNIX_EPOCH + Duration::from_secs_f64(now - file[2].as_f64().unwrap());
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .unwrap()
                    .set_modified(mtime)
                    .unwrap();
            }
        }
        std::fs::write(
            root.join("registry.json"),
            golden["registry"].as_str().unwrap(),
        )
        .unwrap();
        Fixture {
            _guard: guard,
            root,
            golden,
        }
    }

    fn registry(&self) -> PathBuf {
        self.root.join("registry.json")
    }

    fn sessions(&self) -> PathBuf {
        self.root.join("sessions")
    }

    fn target(&self) -> PathBuf {
        PathBuf::from(self.golden["target"].as_str().unwrap())
    }

    fn scan(&self) -> Scan {
        scan_recoverable_codex_rollouts(
            &self.target(),
            self.golden["recency"].as_f64().unwrap(),
            Some(&self.sessions()),
            Some(&self.registry()),
            self.golden["now"].as_f64(),
        )
    }

    fn sweep(&self, session_id: Option<&str>) -> (Value, Vec<RecoverableRow>, Scan) {
        let now = self.golden["now"].as_f64().unwrap();
        sweep_from_scan(&self.target(), self.scan(), now, session_id)
    }

    fn case(&self, name: &str) -> &Value {
        &self.golden["cases"][name]
    }

    fn assert_sweep(&self, name: &str, session_id: Option<&str>) -> Scan {
        let (payload, rows, scan) = self.sweep(session_id);
        let rows: Vec<Value> = rows.iter().map(|r| json!([r.row_id, r.cwd])).collect();
        let actual = json!({"payload": payload, "rows": rows});
        assert_same(&scrubbed(&actual, &self.root), self.case(name), name);
        scan
    }
}

#[test]
fn since_uuid_and_cwd_answers_match_python() {
    let golden: Value = serde_json::from_str(PURE).unwrap();
    for (raw, expected) in golden["since"].as_object().unwrap() {
        let actual = match parse_recovery_since(raw) {
            Ok(seconds) => json!({"ok": seconds}),
            Err(message) => json!({"err": message}),
        };
        assert_same(&actual, expected, raw);
    }
    for (raw, expected) in golden["uuids"].as_object().unwrap() {
        assert_eq!(
            is_canonical_full_uuid(raw),
            expected.as_bool().unwrap(),
            "{raw}"
        );
    }
    let guard = tempfile::tempdir().unwrap();
    let root = guard.path().canonicalize().unwrap();
    std::fs::write(root.join("plain-file"), "x").unwrap();
    for (label, path) in [
        ("missing", root.join("absent").join("deeper")),
        ("file", root.join("plain-file")),
        ("under_file", root.join("plain-file").join("x")),
    ] {
        let message = resolve_recovery_cwd(Some(&path.display().to_string())).unwrap_err();
        let actual = json!({"err": message.replace(&root.display().to_string(), "<ROOT>")});
        assert_same(&actual, &golden["cwds"][label], label);
    }
    assert!(resolve_recovery_cwd(Some(&root.display().to_string())).is_ok());
}

#[test]
fn scan_payload_and_refusals_match_python() {
    let fixture = Fixture::new(&["complete_files"]);
    fixture.assert_sweep("complete", None);
    let selected = fixture.golden["selected"].as_str().unwrap().to_string();
    fixture.assert_sweep("selected", Some(&selected));

    let damaged = Fixture::new(&["complete_files", "damaged_files"]);
    let scan = damaged.assert_sweep("damaged", None);
    assert!(!scan.complete);
    let results = apply_recoverable_with(&scan, &damaged.registry(), None);
    assert_same(
        &scrubbed(&json!(results), &damaged.root),
        damaged.case("damaged_apply"),
        "damaged_apply",
    );

    let absent = Fixture::new(&[]);
    let root_gone = absent.root.join("absent-root");
    let scan = scan_recoverable_codex_rollouts(
        &absent.target(),
        3600.0,
        Some(&root_gone),
        Some(&absent.root.join("absent-registry.json")),
        absent.golden["now"].as_f64(),
    );
    let now = absent.golden["now"].as_f64().unwrap();
    let (payload, _, _) = sweep_from_scan(&absent.target(), scan, now, None);
    assert_same(
        &scrubbed(&payload, &absent.root),
        absent.case("absent_root"),
        "absent_root",
    );

    // The transcript moved after the scan and before the apply: nothing adopts.
    let moved = Fixture::new(&["complete_files"]);
    let scan = moved.scan();
    std::fs::remove_file(moved.sessions().join("b/rollout-offset-ts.jsonl")).unwrap();
    let usable = moved.sessions().join("a/rollout-usable.jsonl");
    let mut text = std::fs::read_to_string(&usable).unwrap();
    let newer = json!({
        "type": "response_item",
        "timestamp": "2026-10-30T01:02:03.500Z",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "newer"}],
        },
    });
    text.push_str(&format!("{newer}\n"));
    std::fs::write(&usable, text).unwrap();
    let results = apply_recoverable_with(&scan, &moved.registry(), None);
    assert_same(
        &scrubbed(&json!(results), &moved.root),
        moved.case("moved_apply"),
        "moved_apply",
    );
    assert_same(
        &recovery_result_counts(&results),
        moved.case("moved_counts"),
        "moved_counts",
    );

    let spent = Fixture::new(&["complete_files"]);
    let deny = || false;
    let results = apply_recoverable_with(&spent.scan(), &spent.registry(), Some(&deny));
    assert_same(
        &scrubbed(&json!(results), &spent.root),
        spent.case("deferred_apply"),
        "deferred_apply",
    );
    assert_same(
        &recovery_result_counts(&results),
        spent.case("deferred_counts"),
        "deferred_counts",
    );
}

#[test]
fn apply_adopts_with_the_follow_up_path_on_the_row() {
    let fixture = Fixture::new(&["complete_files"]);
    let selected = fixture.golden["selected"].as_str().unwrap().to_string();
    let (_, _, scan) = fixture.sweep(Some(&selected));
    let results = apply_recoverable_with(&scan, &fixture.registry(), None);
    assert_same(&json!(results), fixture.case("applied"), "applied");

    let registry = fno_agents::state::load_registry(&fixture.registry()).unwrap();
    let row = registry
        .entries
        .iter()
        .find(|e| e.harness_session_id.as_deref() == Some(selected.as_str()))
        .expect("adopted row");
    let expected = fixture.case("applied_row");
    assert_eq!(row.harness.as_deref(), expected["harness"].as_str());
    assert_eq!(row.origin.as_deref(), expected["origin"].as_str());
    assert_eq!(Some(row.cwd.as_str()), expected["cwd"].as_str());
    assert_eq!(Some(row.short_id.as_str()), expected["short_id"].as_str());
    let tail = format!("agents/{selected}/output.jsonl");
    assert!(
        row.log_path.as_deref().is_some_and(|p| p.ends_with(&tail)),
        "{:?}",
        row.log_path
    );
}
