//! The Explorer tabs exec the public `fno` commands in `org_model`. This runs
//! that exact argv through the real front binary and reads the argv a stub
//! fno-agents leaf receives, so a command rename on either side fails here
//! instead of on the tab ("fleet not read: unknown flag --machines").
//!
//! The front runs as a subprocess with `env_clear`, like
//! mail_send_leaf_argv.rs, so `FNO_AGENTS_BIN` points the paired-binary
//! lookup at the stub without a cross-test env race.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::process::Command;

fn leaf_argv(label: &str, args: &[&str]) -> (Vec<String>, String) {
    let dir = std::env::temp_dir().join(format!(
        "fno-explorer-tab-argv-{label}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let stub = dir.join("fno-agents-stub");
    std::fs::write(
        &stub,
        "#!/bin/sh\n: > \"$LEAF_ARGV_RECORD\"\nfor arg in \"$@\"; do\n  printf '%s\\n' \"$arg\" >> \"$LEAF_ARGV_RECORD\"\ndone\n",
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    let record = dir.join("argv.txt");
    let out = Command::new(env!("CARGO_BIN_EXE_fno"))
        .args(args)
        .env_clear()
        .env("HOME", &dir)
        .env("FNO_AGENTS_BIN", &stub)
        .env("LEAF_ARGV_RECORD", &record)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let recorded = std::fs::read_to_string(&record)
        .unwrap_or_else(|_| format!("<stub never ran> front stderr: {stderr}"));
    let _ = std::fs::remove_dir_all(&dir);
    (recorded.lines().map(String::from).collect(), stderr)
}

/// The Fleet tab's argv reaches the worker census, which routes
/// `--machines` to the fleet map. The leaf side of that door is pinned in
/// fno-agents' `fleet_top_leaf_argv.rs`.
#[test]
fn fleet_tab_argv_reaches_the_census_machines_door() {
    let (argv, stderr) = leaf_argv("fleet", fno::org_model::FLEET_TOP_ARGV);
    assert_eq!(
        argv,
        vec!["census", "--workers", "--machines", "--json"],
        "{stderr}"
    );
}

/// The org fold's argv reaches the org-fold door with its flags verbatim.
#[test]
fn org_fold_argv_reaches_the_org_fold_door() {
    let mut args = fno::org_model::ORG_FOLD_ARGV.to_vec();
    args.extend(["--graph", "/nonexistent/graph.db", "--teams-json", "[]"]);
    let (argv, stderr) = leaf_argv("fold", &args);
    assert_eq!(
        argv,
        vec![
            "org-fold",
            "--graph",
            "/nonexistent/graph.db",
            "--teams-json",
            "[]"
        ],
        "{stderr}"
    );
}
