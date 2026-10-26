//! Parity for the deleted Python leg `cli/src/fno/wake/signal.py`'s one
//! surviving read (`read_signals`), ported to
//! `fno_agents::wake_read::read_signals`. The goldens carry the Python
//! `json.dumps` output verbatim, `_path` scrubbed to `<ROOT>`/`<SLUG>`.
//!
//! parity-stage: characterization
//! parity-oracle: cli/src/fno/wake/signal.py
//!
//! Capture deviation: goldens captured by a direct Python driver under the
//! mission freeze, not the FNO_CAPTURE_GOLDEN=1 harness; the oracle file is
//! gone, so capture mode refuses. These bytes are final.

mod common;

use common::{assert_golden, Golden};
use fno_agents::wake_read::{python_json_default, read_signals};
use serde_json::{json, Value};

/// The capture scrub: realpath first, then the raw root, then the slug.
fn scrub(text: String, sandbox: &std::path::Path) -> String {
    let real = std::fs::canonicalize(sandbox).unwrap_or_else(|_| sandbox.to_path_buf());
    let real_str = real.display().to_string();
    let slug = real_str.replace('/', "-");
    text.replace(&real_str, "<ROOT>")
        .replace(&sandbox.display().to_string(), "<ROOT>")
        .replace(&slug, "<SLUG>")
}

fn seed(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).unwrap();
    let a = json!({
        "source": "inbox-drain", "kind": "question", "msg_id": "m-1",
        "from_project": "p1", "summary": "first",
        "ts": "2026-01-01T00:00:01+00:00", "signal_id": "wake-aaaa",
    });
    std::fs::write(dir.join("wake-a.json"), format!("{a}")).unwrap();
    let b = json!({
        "source": "daily-brief", "kind": "brief", "msg_id": "",
        "from_project": "p2", "summary": "second",
        "ts": "2026-01-02T00:00:00+00:00", "signal_id": "wake-bbbb",
    });
    std::fs::write(dir.join("wake-b.json"), format!("{b}")).unwrap();
    std::fs::write(dir.join("wake-c.json"), "{not json").unwrap();
    std::fs::write(dir.join("other.txt"), "ignored").unwrap();
}

#[test]
fn wake_read_cases_match_the_captured_goldens() {
    let sandbox = std::env::temp_dir().join(format!("wake-parity-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    let home = sandbox.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Pin HOME so paths::space_dir lands inside the sandbox, matching the
    // capture environment. Rust tests share one process, so the env pin and
    // the cleanup run under the suite-wide test mutex.
    let _guard = common::env_mutex();
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    std::env::remove_var("FNO_SPACES_DIR");

    let root = sandbox.join("wakeproj");
    let dir = fno_agents::paths::space_dir(&root).join("wake-signals");
    seed(&dir);

    let golden = |v: Value| -> Golden {
        Golden {
            exit: Some(0),
            streams: vec![python_json_default(&v)],
        }
    };

    let all = read_signals(&root, None);
    let all_text = scrub(python_json_default(&json!(all)), &sandbox);
    assert_golden(
        "wake_signals",
        "wake read all sorted",
        &Golden {
            exit: Some(0),
            streams: vec![all_text],
        },
        None,
    );

    let filtered = read_signals(&root, Some("question"));
    let filtered_text = scrub(python_json_default(&json!(filtered)), &sandbox);
    assert_golden(
        "wake_signals",
        "wake read kind filter",
        &Golden {
            exit: Some(0),
            streams: vec![filtered_text],
        },
        None,
    );

    let empty = read_signals(&sandbox.join("wake-absent"), None);
    assert_golden(
        "wake_signals",
        "wake read empty dir",
        &golden(json!(empty)),
        None,
    );
    let _ = std::fs::remove_dir_all(&sandbox);
}
