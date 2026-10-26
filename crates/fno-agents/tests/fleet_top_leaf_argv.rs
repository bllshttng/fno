//! The leaf half of the Fleet tab's door. The `fno` front execs
//! `fno-agents census --workers --machines --json` for
//! `fno agents fleet top --machines --json` (pinned in fno's
//! `explorer_tab_argv.rs`); this runs that argv through the real binary's
//! dispatch, where the generic flag parser once refused `--machines`.

#![cfg(unix)]

use std::process::Command;

#[test]
fn census_workers_machines_answers_the_fleet_map() {
    let home = std::env::temp_dir().join(format!("fno-fleet-top-leaf-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["census", "--workers", "--machines", "--json"])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let payload: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON payload");
    assert!(payload["machines"].is_array(), "{payload}");
}
