//! The front's unit contracts, ported from the Python `test_retask.py`
//! contracts the native front replaced. The viewport fakes inject the run
//! seam the way the Python tests monkeypatched `subprocess.run`.

use super::*;
use serde_json::json;

fn output(code: i32, stdout: &str) -> std::process::Output {
    use std::os::unix::process::ExitStatusExt;
    std::process::Output {
        status: std::process::ExitStatus::from_raw(code << 8),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn flag_value_reads_space_and_eq_forms() {
    let argv = [
        "--harness".to_string(),
        "codex".to_string(),
        "--route=zai/glm".to_string(),
    ];
    assert_eq!(flag_value(&argv, &["--harness"]).as_deref(), Some("codex"));
    assert_eq!(flag_value(&argv, &["--route"]).as_deref(), Some("zai/glm"));
    assert_eq!(flag_value(&argv, &["--account"]), None);
}

#[test]
fn parse_args_requires_worker_and_node() {
    let usage = parse_args(&[]).unwrap_err();
    assert!(usage.contains("usage"));
    let no_node = parse_args(&["bp-x".to_string()]).unwrap_err();
    assert_eq!(no_node, "retask needs --node");
    let bad = parse_args(&[
        "bp-x".to_string(),
        "--node".to_string(),
        "x-1".to_string(),
        "--wat".to_string(),
    ])
    .unwrap_err();
    assert!(bad.contains("unknown flag"));
    let ok = parse_args(&["bp-x".to_string(), "--node".to_string(), "x-1".to_string()]).unwrap();
    assert_eq!((ok.worker.as_str(), ok.node.as_str()), ("bp-x", "x-1"));
}

#[test]
fn node_command_folds_the_legacy_key_and_strips_the_slash() {
    let declared =
        json!({"id": "x-1", "difficulty": "medium", "dispatch_command": "/fno:blueprint"});
    assert_eq!(node_command(&declared).unwrap(), "blueprint");
    let legacy = json!({"id": "x-1", "difficulty": "medium", "nb": "/fno:blueprint"});
    assert_eq!(node_command(&legacy).unwrap(), "blueprint");
    let stub = json!({"id": "x-1"});
    assert!(node_command(&stub).is_err());
}

fn viewport_row() -> ViewportRow {
    ViewportRow {
        name: "bp-x".to_string(),
        thread_id: Some("F".to_string()),
    }
}

fn fake_run(listing: &str) -> RunSeam {
    let listing = listing.to_string();
    Box::new(move |argv: &[String], _secs: u64| {
        if argv.iter().any(|t| t == "thread") {
            Ok(output(0, ""))
        } else {
            Ok(output(0, &listing))
        }
    })
}

#[test]
fn viewport_joins_the_opened_pane_by_name_and_thread_id() {
    let listing = json!([{"name": "bp-x", "fno_id": "F", "pane_id": 7}]).to_string();
    let mut resolver = ViewportResolver {
        session: "main".to_string(),
        run: fake_run(&listing),
    };
    assert_eq!(
        resolver.resolve(&viewport_row()).unwrap(),
        ("main".to_string(), 7)
    );
}

#[test]
fn viewport_refuses_the_row_without_a_thread_reference() {
    let listing = json!([{"name": "bp-x", "fno_id": "F", "pane_id": 7}]).to_string();
    let mut resolver = ViewportResolver {
        session: "main".to_string(),
        run: fake_run(&listing),
    };
    let row = ViewportRow {
        name: "bp-x".to_string(),
        thread_id: None,
    };
    let error = resolver.resolve(&row).unwrap_err();
    assert_eq!(error.word, "worker_has_no_thread_ref");
    assert!(error.detail.unwrap().contains("A retry cannot fix it"));
}

#[test]
fn viewport_join_misses_after_three_tries() {
    let listing = json!([{"name": "other", "fno_id": "F", "pane_id": 7}]).to_string();
    let mut resolver = ViewportResolver {
        session: "main".to_string(),
        run: fake_run(&listing),
    };
    let error = resolver.resolve(&viewport_row()).unwrap_err();
    assert_eq!(error.word, "thread_view_join_missed");
}

#[test]
fn viewport_door_failure_carries_the_last_error_line() {
    let mut called = 0;
    let run: RunSeam = Box::new(move |_argv: &[String], _secs: u64| {
        called += 1;
        let mut out = output(1, "");
        out.stderr = b"no such pane".to_vec();
        Ok(out)
    });
    let mut resolver = ViewportResolver {
        session: "main".to_string(),
        run,
    };
    let error = resolver.resolve(&viewport_row()).unwrap_err();
    assert_eq!(error.word, "thread_view_unavailable");
    assert_eq!(error.detail.as_deref(), Some("no such pane"));
}
