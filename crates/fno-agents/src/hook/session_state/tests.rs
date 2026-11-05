//! The session-state contracts, ported from the three shell suites the
//! entry replaces (blocked/report, markers/pin, picker): every case guards
//! the same contract here, against the implementation itself.

use super::*;
use crate::hook::adapter;
use serde_json::json;
use std::path::PathBuf;

/// The env-pinned tests share one process env with every other env-using
/// test in the binary, so they serialize on the crate's shared lock, not a
/// private one (a private lock let these mutations race hook::stop's).
fn env_lock() -> std::sync::MutexGuard<'static, ()> {
    crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The wiring a row's session_state audit declares; every call site here
/// tests a supported row, so a missing wiring is the failure.
fn job(name: &str) -> crate::harness_capabilities::HookWiring {
    crate::harness_capabilities::HarnessContract::packaged()
        .unwrap()
        .hook_job(name, "session_state")
        .unwrap()
        .wiring
        .clone()
        .unwrap()
}

fn claude_job() -> crate::harness_capabilities::HookWiring {
    job("claude")
}

fn ev(event: &str, extra: serde_json::Value) -> adapter::HookEvent {
    let mut payload = json!({"session_id": "sess-1", "hook_event_name": event});
    let obj = payload.as_object_mut().unwrap();
    if let serde_json::Value::Object(extra) = extra {
        for (k, v) in extra {
            obj.insert(k, v);
        }
    }
    adapter::normalize("claude", event, &payload)
}

// --- the state decision (the blocked/report side) ---

/// The row map is the data: claude wires its four events onto Notification;
/// codex wires three plus PermissionRequest as its blocked producer, with
/// the screen backstop kept for `request_user_input`; a goal-leg PreToolUse decides
/// working (a continuation is a user-role row, so UserPromptSubmit never
/// fires - the tool calls are the only signal).
#[test]
fn the_row_map_wires_claude_and_codex() {
    let claude = claude_job();
    assert_eq!(claude.blocked, "Notification");
    for (event, word) in [
        ("UserPromptSubmit", "working"),
        ("PreToolUse", "working"),
        ("Stop", "done"),
        ("PostModelSwitch", "model"),
    ] {
        assert_eq!(claude.events.get(event).map(String::as_str), Some(word));
    }
    let codex = job("codex");
    assert_eq!(codex.blocked, "PermissionRequest");
    assert!(codex.screen_backstop);
    assert!(crate::harness_capabilities::session_state_names_no_blocked_producer("codex"));
    assert_eq!(
        codex.events.get("PreToolUse").map(String::as_str),
        Some("working")
    );
    assert_eq!(codex.events.get("Stop").map(String::as_str), Some("done"));
    assert_eq!(codex.events.get("PostModelSwitch"), None);
    let goal = decide(
        &codex,
        &adapter::normalize(
            "codex",
            "PreToolUse",
            &json!({"session_id": "0f0e1d2c-3b4a-4958-8675-3092f4c1b2a3", "tool_name": "Bash"}),
        ),
    )
    .unwrap();
    assert_eq!(goal.state, "working");
    // An unwired event decides nothing.
    assert_eq!(decide(&claude, &ev("PreCompact", json!({}))), None);
}

// --- the transition gate (the 120 s record) ---

fn runtime_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fno-ss-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// --- the marker identity gate (ancestry verdict + v1 first-writer fallback) ---

fn set_pane_env(pane: &str, epoch: &str, server: &str, rt: &std::path::Path) {
    std::env::set_var("FNO_PANE", pane);
    std::env::set_var("FNO_PANE_EPOCH", epoch);
    std::env::set_var("FNO_SERVER", server);
    std::env::set_var("XDG_RUNTIME_DIR", rt);
}

fn clear_pane_env() {
    for k in [
        "FNO_PANE",
        "FNO_PANE_EPOCH",
        "FNO_SERVER",
        "FNO_SESSION",
        "XDG_RUNTIME_DIR",
    ] {
        std::env::remove_var(k);
    }
}

/// mT9: a path-traversal FNO_SESSION is sanitized so the pin (and the gate
/// record) never escapes the rendezvous dir.
#[test]
fn a_traversal_server_name_stays_in_the_pin_dir() {
    let _g = env_lock();
    let rt = runtime_dir("traversal");
    set_pane_env("9", "9000", "", &rt);
    // An empty census sends the gate to the v1 latch, whose sanitization
    // this test pins; the ancestry path needs a census to answer at all.
    crate::spawn_context::set_test_ancestry_table(Some(std::collections::BTreeMap::new()));
    std::env::remove_var("FNO_SERVER");
    std::env::set_var("FNO_SESSION", "../../escape");
    assert!(is_pane_host("h"));
    let escaped = std::fs::read_dir(&rt)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with("escape-")
                || n.contains("-escape")
                || n.contains("fno-turn-pins-") && n.contains("..")
        });
    assert!(!escaped, "a pin escaped the rendezvous dir: {escaped}");
    crate::spawn_context::set_test_ancestry_table(None);
    clear_pane_env();
    let _ = std::fs::remove_dir_all(&rt);
}

// --- the stop gate's block report ---

/// A blocked stop reports working for the session and marks the gate record,
/// so the leg's first PreToolUse working fire collapses as the duplicate it
/// is. A footnote-harness session stays out of the row's way (the shim's one
/// policy line), and an empty id reports nothing.
#[test]
fn the_stop_gate_block_report_sends_working_and_marks_the_record() {
    let _g = env_lock();
    let rt = runtime_dir("stopgate-report");
    let home = rt.join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    std::env::set_var("XDG_RUNTIME_DIR", &rt);
    // No daemon socket under this root: the send answers DaemonNotRunning,
    // which the fire-and-forget contract treats as delivered and marks.
    let saved_agents_home = std::env::var_os("FNO_AGENTS_HOME");
    std::env::set_var("FNO_AGENTS_HOME", &home);
    let saved_harness_name = std::env::var_os("FNO_HARNESS_NAME");
    std::env::remove_var("FNO_HARNESS_NAME");
    report_working("sess-blocked");
    let record =
        std::fs::read_to_string(runtime_pin_dir().unwrap().join("state-sess-blocked")).unwrap();
    assert!(record.starts_with("working "), "{record}");
    std::env::set_var("FNO_HARNESS_NAME", "footnote");
    report_working("sess-footnote");
    assert!(!runtime_pin_dir()
        .unwrap()
        .join("state-sess-footnote")
        .exists());
    report_working("");
    match saved_agents_home {
        Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
        None => std::env::remove_var("FNO_AGENTS_HOME"),
    }
    match saved_harness_name {
        Some(v) => std::env::set_var("FNO_HARNESS_NAME", v),
        None => std::env::remove_var("FNO_HARNESS_NAME"),
    }
    std::env::remove_var("XDG_RUNTIME_DIR");
    let _ = std::fs::remove_dir_all(&rt);
}
