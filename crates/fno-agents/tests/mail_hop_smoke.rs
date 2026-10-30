//! One real mail send through both built binaries: the `fno` front door and
//! `fno-agents mail-remote`. Each half has unit tests of its own, and the
//! plain-name recursion still shipped because no test ran a send through
//! both. Wrapper scripts stand in front of each binary and log every exec,
//! and the native `mail send` is the local command, so the test reads the real
//! process chain a send builds.
//!
//! The chain is bounded twice: the wrappers refuse past a fixed line count,
//! so a regression fails here instead of filling the process table, and the
//! relay's own hop counter refuses at depth 3.
//!
//! The front is built from this checkout by the test itself, never taken
//! from PATH or a stale target dir: a front from another commit would pass
//! without exercising this one. Where CI already built it, the build is a
//! no-op.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

const RUNAWAY: usize = 40;

fn real_fno() -> &'static Path {
    static FRONT: OnceLock<PathBuf> = OnceLock::new();
    FRONT.get_or_init(|| {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../fno/Cargo.toml");
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let out = Command::new(cargo)
            .args(["build", "--quiet", "--bin", "fno", "--message-format=json"])
            .arg("--manifest-path")
            .arg(&manifest)
            .output()
            .expect("run cargo build for the fno front");
        assert!(
            out.status.success(),
            "building the fno front failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
            .find(|p| p.file_name().is_some_and(|n| n == "fno"))
            .expect("cargo reported no fno executable")
    })
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// A logging wrapper: one line per exec, refuse past the runaway bound, and
/// (when `DROP_MARKER` is set) lose the relay's re-entry marker the way a
/// new route that forgets it would.
fn wrapper(label: &str, real: &str) -> String {
    format!(
        r#"#!/bin/sh
printf '{label} %s\n' "$*" >> "$HOP_LOG"
lines=$(wc -l < "$HOP_LOG")
if [ $lines -gt {RUNAWAY} ]; then echo "runaway relay" >&2; exit 97; fi
if [ -n "$DROP_MARKER" ]; then unset FNO_MAIL_LOCAL_FORWARD; fi
exec "{real}" "$@"
"#
    )
}

struct Chain {
    exit: i32,
    stderr: String,
    log: Vec<String>,
}

impl Chain {
    fn count(&self, prefix: &str) -> usize {
        self.log.iter().filter(|l| l.starts_with(prefix)).count()
    }
}

fn send(root: &Path, args: &[&str], drop_marker: bool) -> Chain {
    let log = root.join("hops.log");
    let _ = std::fs::remove_file(&log);
    std::fs::write(&log, "").unwrap();
    let mut cmd = Command::new(real_fno());
    cmd.args(args)
        .env_clear()
        .envs(fno_agents::test_run::self_owner_env())
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_BUS_DIR", root.join("bus"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_TEST_HERMETIC", "1")
        .env("FNO_MAIL_LANDED_SETTLE_S", "0")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("FNO_BIN", root.join("bin/fno"))
        .env("FNO_AGENTS_BIN", root.join("bin/fno-agents"))
        .env("REAL_FNO", real_fno())
        .env("REAL_AGENTS", env!("CARGO_BIN_EXE_fno-agents"))
        .env("HOP_LOG", &log);
    if drop_marker {
        cmd.env("DROP_MARKER", "1");
    }
    let out = cmd.output().unwrap();
    Chain {
        exit: out.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        log: std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(String::from)
            .collect(),
    }
}

#[test]
fn a_real_send_crosses_both_binaries_in_at_most_two_hops() {
    let root = std::env::temp_dir().join(format!("fno-mail-hop-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bin")).unwrap();
    script(&root.join("bin/fno"), &wrapper("fno", "$REAL_FNO"));
    script(
        &root.join("bin/fno-agents"),
        &wrapper("agents", "$REAL_AGENTS"),
    );
    let host = Command::new("hostname").output().unwrap();
    let host = fno_agents::mail_bus::normalize_host(&String::from_utf8_lossy(&host.stdout));
    let at_self = format!("quinlan@{host}");

    // A plain name and a name@self each take the relay once and re-enter the
    // front once (two hops), then reach the local command exactly once.
    for target in ["quinlan", at_self.as_str()] {
        let c = send(&root, &["agents", "mail", "send", target, "hello"], false);
        let why = format!(
            "{target}: exit {} log {:?} stderr {}",
            c.exit, c.log, c.stderr
        );
        assert_eq!(c.count("agents mail-remote"), 1, "{why}");
        assert_eq!(c.count("fno "), 1, "{why}");
        assert_eq!(c.count("agents mail send "), 1, "{why}");
        assert!(
            c.log.iter().any(|l| l == "agents mail send quinlan hello"),
            "{why}"
        );
    }

    // The daemon's flag-led machine mail never enters the relay.
    let c = send(
        &root,
        &[
            "agents",
            "mail",
            "send",
            "--from-name",
            "fno/quiet-recovery",
            "--to-lead",
            "scope-a",
            "body",
        ],
        false,
    );
    let why = format!(
        "flag-led: exit {} log {:?} stderr {}",
        c.exit, c.log, c.stderr
    );
    assert_eq!(c.count("agents mail-remote"), 0, "{why}");

    // A route that loses the re-entry marker loops; the hop counter stops it
    // at depth 3 with a named error, and the local command never runs.
    let c = send(&root, &["agents", "mail", "send", "quinlan", "hello"], true);
    let why = format!(
        "looped: exit {} log {:?} stderr {}",
        c.exit, c.log, c.stderr
    );
    assert_eq!(c.count("agents mail-remote"), 3, "{why}");
    assert_eq!(c.count("agents mail send "), 0, "{why}");
    assert_eq!(c.exit, 2, "{why}");
    assert!(c.stderr.contains("relay hop limit"), "{why}");

    let _ = std::fs::remove_dir_all(&root);
}
