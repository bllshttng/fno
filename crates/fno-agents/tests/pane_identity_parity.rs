//! parity-stage: characterization
//! parity-oracle: fno.agents.reachability.pane_identity_crosscheck
//!
//! The pane-identity command, frozen against goldens captured from its Python
//! leg (tests/golden/pane_identity/capture.py) before that leg was deleted.
//! Each case replays the pane listing and registry rows the capture recorded
//! beside its goldens, through the same stub `fno`, and compares exit code,
//! stdout and stderr byte for byte.

use std::path::{Path, PathBuf};
use std::process::Command;

const STUB: &str = "#!/bin/sh\n\
if [ -n \"$PANES_FAIL\" ]; then echo \"$PANES_FAIL\" >&2; exit 1; fi\n\
cat \"$PANES_FILE\"\n";

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/pane_identity")
}

fn golden(case: &str, ext: &str) -> String {
    let path = golden_dir().join(format!("{case}.{ext}"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn run(root: &Path, case: &str, argv: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let home = root.join(case);
    std::fs::create_dir_all(&home).unwrap();
    let rows: serde_json::Value = serde_json::from_str(&golden(case, "rows")).unwrap();
    let doc = serde_json::json!({"schema_version": 43, "agents": rows});
    std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
    let panes_file = home.join("panes.json");
    std::fs::write(&panes_file, golden(case, "panes")).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.env("FNO_AGENTS_HOME", &home)
        .env("FNO_HOME", root.join("state"))
        .env("FNO_STATE_DIR", root.join("state"))
        .env("FNO_BIN", root.join("fno"))
        .env("PANES_FILE", &panes_file)
        .env_remove("FNO_SERVER")
        .env_remove("FNO_SESSION")
        .env_remove("PANES_FAIL")
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(root);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let out = cmd
        .args(["status", "--pane-identity"])
        .args(argv)
        .output()
        .expect("command ran");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn fixture_root() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("state")).unwrap();
    std::fs::write(dir.path().join("state/.path-migration-done"), "\n").unwrap();
    let stub = dir.path().join("fno");
    std::fs::write(&stub, STUB).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

#[test]
fn pane_identity_matches_the_python_goldens() {
    let dir = fixture_root();
    let cases: [(&str, &[&str], &[(&str, &str)]); 11] = [
        ("clean-text", &["--server", "main"], &[]),
        ("clean-json", &["--server", "main", "--json"], &[]),
        ("empty-listing", &["--server", "main"], &[]),
        ("mixed-text", &["--server", "main"], &[]),
        ("mixed-json", &["--server", "main", "-J"], &[]),
        ("env-server", &[], &[("FNO_SERVER", "main")]),
        ("alias-session-id", &["--session-id", "main"], &[]),
        ("alias-session", &["--session", "main"], &[]),
        (
            "listing-fails",
            &["--server", "main"],
            &[("PANES_FAIL", "no server main")],
        ),
        ("listing-empty", &["--server", "main"], &[]),
        ("listing-not-list", &["--server", "main"], &[]),
    ];
    for (case, argv, env) in cases {
        let (code, out, err) = run(dir.path(), case, argv, env);
        let want_code: i32 = golden(case, "exit").trim().parse().unwrap();
        assert_eq!(code, want_code, "{case}: exit\nstderr={err}");
        assert_eq!(out, golden(case, "out"), "{case}: stdout");
        assert_eq!(err, golden(case, "err"), "{case}: stderr");
    }
}

#[test]
fn pane_identity_refuses_the_server_flag_with_an_alias() {
    let dir = fixture_root();
    for alias in ["--session-id", "--session"] {
        let (code, _, err) = run(
            dir.path(),
            "clean-text",
            &["--server", "a", alias, "b"],
            &[],
        );
        assert_eq!(code, 2, "{alias}: {err}");
        assert!(err.contains("not both"), "{alias}: {err}");
    }
}
