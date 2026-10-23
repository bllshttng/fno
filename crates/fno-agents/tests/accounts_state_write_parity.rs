//! parity-stage: differential
//! parity-oracle: fno.adapters.providers.runtime_state
//!
//! Differential parity for the write side of `accounts::state`, the one
//! writer of the runtime-state file. Each
//! `tests/golden/accounts_state_write/*.json` case carries the file it was
//! captured against, the writer and its inputs, and the Python writer's
//! answer plus the file it left under `expect`, frozen at `NOW` by
//! `accounts_state_write_golden_driver.py` (the case table lives there). A
//! `locked` case holds the update lock first, so the writer takes its timeout
//! branch. This test runs the Rust writer on the same file and asserts equal
//! JSON for both.

use fno_agents::accounts::state::{self, Rule};
use serde_json::{json, Value};
use std::fs::{File, FileTimes, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

const NOW: f64 = 1_789_400_000.0;

fn hold_lock(path: &Path) -> File {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".update.lock");
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(lock_path)
        .unwrap();
    // SAFETY: flock on a descriptor this test owns.
    assert_eq!(unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) }, 0);
    file
}

fn set_mtime(path: &Path, secs: f64) {
    let at = UNIX_EPOCH + Duration::from_secs_f64(secs);
    File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(FileTimes::new().set_modified(at).set_accessed(at))
        .unwrap();
}

fn run_op(case: &Value, path: &Path, timeout: Duration) -> Value {
    let id = case["id"].as_str().unwrap_or_default();
    let span = case["window_minutes"].as_f64().map(|m| m * 60.0);
    match case["op"].as_str().unwrap() {
        "update-health" => {
            let rule = Rule {
                backoff: case["rule"]["backoff"].as_bool().unwrap(),
                cooldown_ms: case["rule"]["cooldown_ms"].as_i64(),
            };
            let h = state::update_health(
                path,
                id,
                rule,
                case["model"].as_str(),
                case["resets_at"].as_f64(),
                span,
                NOW,
                timeout,
            )
            .unwrap();
            state::keyed(h.to_json(), "provider_id", id)
        }
        "write-usage" => {
            let snap = state::parse_snapshot(&case["snapshot"]).unwrap();
            json!(state::write_usage(path, id, snap, NOW, timeout).unwrap())
        }
        "advance-cursor" => {
            let combo = case["combo"].as_str().unwrap();
            let c = state::advance_cursor(
                path,
                combo,
                case["sticky"].as_i64().unwrap(),
                case["hash"].as_str().unwrap(),
                case["count"].as_i64().unwrap(),
                NOW,
                timeout,
            )
            .unwrap();
            state::keyed(c.to_json(), "combo_name", combo)
        }
        "stamp-window" => json!(state::stamp_window(
            path,
            id,
            case["opened_at"].as_f64().unwrap(),
            case["label"].as_str().unwrap_or("window"),
            span,
            NOW,
            timeout,
        )
        .unwrap()),
        "mark-warned" => json!(state::mark_warned(path, id, NOW, timeout).unwrap()),
        "fold" => {
            let legacy = path.with_file_name("legacy.json");
            std::fs::write(&legacy, case["legacy"].to_string()).unwrap();
            let newer = case["legacy_newer"].as_bool().unwrap();
            let (older, now) = (NOW - 50.0, NOW);
            if path.exists() {
                set_mtime(path, if newer { older } else { now });
            }
            set_mtime(&legacy, if newer { now } else { older });
            state::fold_legacy(path, &legacy, NOW, timeout).unwrap();
            Value::Null
        }
        other => panic!("unknown op {other}"),
    }
}

#[test]
fn accounts_state_write_matches_the_python_leg() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/accounts_state_write");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 45, "golden cases missing: {}", cases.len());
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|golden| {
            let case: Value =
                serde_json::from_str(&std::fs::read_to_string(golden).unwrap()).unwrap();
            let tmp = tempfile::tempdir().unwrap();
            let path = tmp.path().join("provider-runtime-state.json");
            match &case["state"] {
                Value::Null => {}
                Value::String(text) => std::fs::write(&path, text).unwrap(),
                other => std::fs::write(&path, other.to_string()).unwrap(),
            }
            let locked = case["locked"].as_bool().unwrap_or(false);
            let _held = locked.then(|| hold_lock(&path));
            let timeout = if locked {
                Duration::from_millis(200)
            } else {
                state::LOCK_TIMEOUT
            };
            let result = run_op(&case, &path, timeout);
            let file = std::fs::read_to_string(&path)
                .ok()
                .map(|text| serde_json::from_str(&text).unwrap_or(Value::String(text)))
                .unwrap_or(Value::Null);
            let got = json!({"result": result, "file": file});
            (got != case["expect"]).then(|| {
                format!(
                    "{}\n  expect: {}\n  rust:   {}",
                    golden.file_name().unwrap().to_string_lossy(),
                    case["expect"],
                    got
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
    // The serialized form is Python's sort_keys + indent=2 shape.
    let probe_dir = tempfile::tempdir().unwrap();
    let probe = probe_dir.path().join("probe.json");
    state::write_usage(
        &probe,
        "a1",
        state::parse_snapshot(&json!({"windows": [], "probed_at": NOW})).unwrap(),
        NOW,
        state::LOCK_TIMEOUT,
    )
    .unwrap();
    let text = std::fs::read_to_string(&probe).unwrap();
    assert!(
        text.starts_with("{\n  \"combo_cursors\": {},\n  \"provider_health\": {},"),
        "{text}"
    );
}
