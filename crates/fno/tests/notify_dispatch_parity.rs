//! Parity for the deleted Python leg `cli/src/fno/notify/_impl.py` ported to
//! `fno::notify_dispatch`. The goldens freeze the Python decision records:
//! `{code, msg, title, message, pointer, local_tool, mux_clients,
//! journal_ok, sink_routes}` as sort-keys compact JSON (out = record,
//! err = msg, exit = code), plus the two front-door CLI bytes.
//!
//! parity-stage: characterization
//! parity-oracle: cli/src/fno/notify/_impl.py
//!
//! Capture deviation: the mission freeze barred local cargo runs, so the
//! goldens were captured by driving the Python leg directly (its module
//! functions monkeypatched per case, exactly the seams
//! `send_notification_seams` restores) instead of FNO_CAPTURE_GOLDEN=1.
//! The oracle file is gone, so capture mode refuses. These bytes are final.
//!
//! The `fno notify` deprecated-move case asserts the stderr line byte-equal
//! and the empty stdout; its exit code is the channel table's verdict on the
//! capture host (an OS toast existed there), so the code itself is covered
//! by the nine seam cases and not re-asserted here - a Linux runner without
//! a notifier would answer 1 honestly and still be correct.

use fno::notify_dispatch::{send_notification_seams, DEPRECATED_MOVE_LINE};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

/// Sort-keys compact JSON, the capture record's serializer. The records hold
/// only ASCII strings, small ints, and null, so serde's string escaping
/// matches Python's byte for byte on these values.
fn py_json(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => serde_json::to_string(s).unwrap(),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(py_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let keys: BTreeMap<&String, &Value> = map.iter().collect();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|(k, v)| format!("{}:{}", serde_json::to_string(k).unwrap(), py_json(v)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
    }
}

/// One decision-table case against its frozen golden.
fn case(
    label: &str,
    title: &str,
    message: &str,
    pointer: &str,
    local_tool: bool,
    mux_clients: Option<u64>,
    journal_ok: bool,
    sink_routes: bool,
) {
    let (code, msg) = send_notification_seams(
        title,
        message,
        pointer,
        &mut |_, _| local_tool,
        &mut |_, _| mux_clients,
        &mut |_, _, _| journal_ok,
        &mut |_| sink_routes,
    );
    let mut record = Map::new();
    record.insert("code".into(), json!(code));
    record.insert("msg".into(), json!(msg));
    record.insert("title".into(), json!(title));
    record.insert("message".into(), json!(message));
    record.insert("pointer".into(), json!(pointer));
    record.insert("local_tool".into(), json!(local_tool));
    record.insert("mux_clients".into(), json!(mux_clients));
    record.insert("journal_ok".into(), json!(journal_ok));
    record.insert("sink_routes".into(), json!(sink_routes));
    let out = py_json(&Value::Object(record));
    let golden_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fno-agents/tests/golden/notify_dispatch");
    let key = format!(
        "{}",
        label
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect::<String>()
    );
    let exit: i32 = std::fs::read_to_string(golden_dir.join(format!("{key}.exit")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(exit, code, "[{label}] exit differs from golden");
    let want_out = std::fs::read_to_string(golden_dir.join(format!("{key}.out"))).unwrap();
    assert_eq!(want_out, out, "[{label}] record differs from golden");
    let want_err = std::fs::read_to_string(golden_dir.join(format!("{key}.err"))).unwrap();
    assert_eq!(want_err, msg, "[{label}] msg differs from golden");
}

#[test]
fn notify_decision_table_matches_the_captured_goldens() {
    case(
        "notify mux first channel wins",
        "T",
        "M",
        "",
        true,
        Some(2),
        true,
        false,
    );
    case(
        "notify local tool only",
        "T",
        "M",
        "",
        true,
        None,
        true,
        false,
    );
    case(
        "notify sink lane only",
        "T",
        "M",
        "",
        false,
        None,
        true,
        true,
    );
    case(
        "notify sink configured but no row",
        "T",
        "M",
        "",
        false,
        None,
        false,
        true,
    );
    case(
        "notify journal row without a sink",
        "T",
        "M",
        "",
        false,
        None,
        true,
        false,
    );
    case(
        "notify no channel at all",
        "T",
        "M",
        "",
        false,
        None,
        false,
        false,
    );
    case(
        "notify mux zero falls through",
        "T",
        "M",
        "",
        true,
        Some(0),
        true,
        true,
    );
    case(
        "notify defaults on empty strings",
        "",
        "",
        "",
        false,
        None,
        false,
        false,
    );
    case(
        "notify pointer passes through",
        "T",
        "M",
        "fno inbox outstanding",
        false,
        None,
        true,
        true,
    );
}

#[test]
fn notify_cli_cases_match_the_captured_goldens() {
    let golden_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fno-agents/tests/golden/notify_dispatch");

    // `fno inbox notify` with no args: the typer-shaped usage refusal.
    let bin = env!("CARGO_BIN_EXE_fno");
    let out = std::process::Command::new(bin)
        .args(["inbox", "notify"])
        .env("FNO_TEST_HERMETIC", "1")
        .output()
        .unwrap();
    let key = "cli_inbox_notify_missing_args";
    let exit: i32 = std::fs::read_to_string(golden_dir.join(format!("{key}.exit")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(exit, out.status.code().unwrap(), "[{key}] exit differs");
    let want_err = std::fs::read_to_string(golden_dir.join(format!("{key}.err"))).unwrap();
    assert_eq!(
        want_err,
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "[{key}] usage bytes differ from golden"
    );

    // `fno notify T B`: the stderr move line is the whole parity interest.
    let want_err =
        std::fs::read_to_string(golden_dir.join("cli_root_notify_deprecated_move.err")).unwrap();
    assert_eq!(
        want_err, DEPRECATED_MOVE_LINE,
        "the deprecated-move line drifted from the frozen bytes"
    );
}

/// The `fno status-fanout` root spelling: the front door prints its move
/// line and execs the fno-agents binary (stubbed here), so the wiring is
/// byte-verified end to end without a second build.
#[test]
fn status_fanout_deprecated_move_wiring() {
    let golden_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../fno-agents/tests/golden/status_fanout");
    let sandbox = std::env::temp_dir().join(format!("fanout-move-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&sandbox);
    let home = sandbox.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let stub = sandbox.join("stub-fno-agents.sh");
    std::fs::write(
        &stub,
        "#!/bin/sh\nprintf 'status-fanout: no enabled sinks (no-op)\\n'\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&stub, perms).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(["status-fanout", "tick", "--dry-run"])
        .env("FNO_AGENTS_BIN", &stub)
        .env("HOME", &home)
        .env("FNO_TEST_HERMETIC", "1")
        .output()
        .unwrap();
    let key = "cli_root_status_fanout_deprecated_move";
    let exit: i32 = std::fs::read_to_string(golden_dir.join(format!("{key}.exit")))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(exit, out.status.code().unwrap(), "[{key}] exit differs");
    let want_out = std::fs::read_to_string(golden_dir.join(format!("{key}.out"))).unwrap();
    assert_eq!(
        want_out,
        String::from_utf8_lossy(&out.stdout),
        "[{key}] stdout differs from golden"
    );
    let want_err = std::fs::read_to_string(golden_dir.join(format!("{key}.err"))).unwrap();
    assert_eq!(
        want_err,
        String::from_utf8_lossy(&out.stderr).trim_end(),
        "[{key}] move line differs from golden"
    );
    let _ = std::fs::remove_dir_all(&sandbox);
}
