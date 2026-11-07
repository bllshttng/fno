//! parity-stage: characterization
//! parity-oracle: gone (cli/src/fno/pr_watch died with the Python runtime; the
//! committed goldens under tests/golden/pr-watch-install/ stay as the frozen
//! contract)
//!
//! Characterization for the pr-watch install and uninstall ports: the Rust
//! commands (`fno-agents pr-watch install`, its `--ensure` coupling mode, and
//! `fno-agents pr-watch uninstall`, plus `--unload-only`) are pinned by the
//! frozen goldens under tests/golden/pr-watch-install/. The goldens froze
//! from the live Python leg while it lived, both implementations answering
//! the same fixture under the same env pins. The Python package is gone, so
//! the goldens stand as the contract.
//!
//! Seventeen fixture states. Wave 4's nine: the dry-run render, the
//! confirmed write + bounce + legacy retirement, the declined gate, the
//! no-activate escape, a loud activation failure, and the four
//! `ensure_activated` outcome words, plus the already-running short circuit
//! and a legacy retirement through the coupling path. Wave 5's eight: full
//! uninstall over a loaded agent, without a plist, not-loaded, a best-effort
//! unload failure, and the four `unload_only` outcome words. Every volatile
//! byte (the case dir, the launchd uid) goes through a mask first - the
//! golden describes the rule tree, not this machine.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::{make_script, slug, Golden};

mod common;

/// What the launchctl stub answers, exported as `FNO_STUB_*` env so both
/// legs' subprocesses see the same fake.
struct LaunchCtl {
    list: Option<&'static str>,
    load_rc: i32,
    bootstrap_fails: u32,
    bootout_rc: i32,
    kickstart_rc: i32,
    unload_rc: i32,
}

impl Default for LaunchCtl {
    fn default() -> Self {
        LaunchCtl {
            list: None,
            load_rc: 0,
            bootstrap_fails: 0,
            bootout_rc: 0,
            kickstart_rc: 0,
            unload_rc: 0,
        }
    }
}

struct Case {
    label: &'static str,
    mode: Mode,
    stdin: Option<&'static str>,
    legacy: bool,
    /// Replace the LaunchAgents dir with a file: the write-failed shape.
    agents_dir_is_file: bool,
    /// Pre-seed the canonical plist: the uninstall cases' input state.
    plist: bool,
    launchctl: LaunchCtl,
}

enum Mode {
    Ensure,
    Uninstall { unload_only: bool },
}

fn write_case(root: &Path, case: &Case) -> PathBuf {
    let dir = root.join(slug(case.label));
    let home = dir.join("home");
    let agents = home.join("Library").join("LaunchAgents");
    let state = dir.join("state");
    let stub = dir.join("stub");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&stub).unwrap();

    let mut config = format!("state_dir = \"{}\"\n\n", state.display());
    config.push_str("[pr_watch]\n");
    config.push_str("enabled = true\n");
    config.push_str("interval_seconds = 600\n");
    std::fs::write(dir.join("config.toml"), config).unwrap();

    if case.agents_dir_is_file {
        std::fs::create_dir_all(agents.parent().unwrap()).unwrap();
        std::fs::write(&agents, "not a directory").unwrap();
    } else {
        std::fs::create_dir_all(&agents).unwrap();
    }

    if case.legacy {
        std::fs::write(
            agents.join("com.fno.postmerge-old.plist"),
            "<plist><dict/></plist>",
        )
        .unwrap();
    }

    // The uninstall cases' input state: the canonical plist already on disk.
    if case.plist {
        std::fs::write(
            agents.join("sh.fno.pr-watcher.plist"),
            "<plist><dict/></plist>",
        )
        .unwrap();
    }

    // The launchctl stub both legs resolve through PATH. The bootstrap-fails
    // counter lives in the case dir so retries never leak between cases.
    make_script(
        &stub,
        "launchctl",
        r#"
case "$1" in
  list) [ -n "$FNO_STUB_LIST" ] && echo "$FNO_STUB_LIST" ; exit 0 ;;
  bootout) exit ${FNO_STUB_BOOTOUT_RC:-0} ;;
  bootstrap)
    if [ -n "$FNO_STUB_BOOTSTRAP_FAILS" ]; then
      c=$(cat "$FNO_STUB_STATE/bootstrap_count" 2>/dev/null || echo 0)
      c=$((c+1))
      echo "$c" > "$FNO_STUB_STATE/bootstrap_count"
      if [ "$c" -le "$FNO_STUB_BOOTSTRAP_FAILS" ]; then exit 5; fi
    fi
    exit ${FNO_STUB_BOOTSTRAP_RC:-0} ;;
  kickstart) exit ${FNO_STUB_KICKSTART_RC:-0} ;;
  load) exit ${FNO_STUB_LOAD_RC:-0} ;;
  unload) exit ${FNO_STUB_UNLOAD_RC:-0} ;;
  *) exit 0 ;;
esac
"#,
    );
    // The fno binary path the plist carries: a stub, so the rendered
    // ProgramArguments stay inside the masked case dir.
    make_script(&stub, "fno-py", "exit 0");

    dir
}

/// The env pins both legs inherit: one home, one state root, one config, the
/// stub PATH, and the launchctl answers.
fn pin_env(dir: &Path, case: &Case) -> Vec<(String, String)> {
    let mut env = vec![
        // Pin the launchd-label suffix so receipts and rendered plists stay
        // byte-for-byte across machines (x-781d).
        (
            fno_agents::launchd_labels::FNO_TEST_LAUNCHD_SUFFIX.into(),
            "testhome".into(),
        ),
        ("HOME".into(), dir.join("home").display().to_string()),
        (
            "FNO_CONFIG".into(),
            dir.join("config.toml").display().to_string(),
        ),
        (
            "FNO_STATE_DIR".into(),
            dir.join("state").display().to_string(),
        ),
        ("CARGO_HOME".into(), dir.join("cargo").display().to_string()),
        (
            "FNO_TEST_PR_WATCH_LAUNCH_AGENTS_DIR".into(),
            dir.join("home")
                .join("Library")
                .join("LaunchAgents")
                .display()
                .to_string(),
        ),
        (
            "FNO_TEST_FNO_BINARY".into(),
            dir.join("stub").join("fno-py").display().to_string(),
        ),
        // Keep the deployed fno and any real launchctl out of reach; the stub
        // dir answers both.
        (
            "PATH".into(),
            format!("{}:/usr/bin:/bin:/usr/sbin", dir.join("stub").display()),
        ),
        ("FNO_STUB_STATE".into(), dir.display().to_string()),
        (
            "FNO_STUB_LIST".into(),
            case.launchctl.list.unwrap_or("").to_string(),
        ),
        (
            "FNO_STUB_LOAD_RC".into(),
            case.launchctl.load_rc.to_string(),
        ),
        (
            "FNO_STUB_UNLOAD_RC".into(),
            case.launchctl.unload_rc.to_string(),
        ),
        (
            "FNO_STUB_BOOTSTRAP_FAILS".into(),
            case.launchctl.bootstrap_fails.to_string(),
        ),
        (
            "FNO_STUB_BOOTOUT_RC".into(),
            case.launchctl.bootout_rc.to_string(),
        ),
        (
            "FNO_STUB_KICKSTART_RC".into(),
            case.launchctl.kickstart_rc.to_string(),
        ),
        // The Python store client resolves the binary through this pin, so
        // the bounce event commits into the fixture's own store.
        (
            "FNO_AGENTS_BIN".into(),
            env!("CARGO_BIN_EXE_fno-agents").to_string(),
        ),
    ];
    if case.launchctl.bootstrap_fails > 0 {
        env.push(("FNO_STUB_BOOTSTRAP_RC".into(), "5".into()));
    }
    env
}

fn case_args(case: &Case) -> Vec<String> {
    match &case.mode {
        Mode::Uninstall { unload_only } => {
            let mut argv: Vec<String> = Vec::new();
            if *unload_only {
                argv.push("--unload-only".to_string());
            }
            argv
        }
        Mode::Ensure => vec!["--ensure".to_string()],
    }
}

/// The Rust leg through the built binary: the exact surface the Python leaf
/// forwards to.
fn rust_leg(dir: &Path, case: &Case) -> Golden {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    match &case.mode {
        Mode::Uninstall { .. } => cmd.args(["pr-watch", "uninstall"]),
        _ => cmd.args(["pr-watch", "install"]),
    };
    for arg in case_args(case) {
        cmd.arg(arg);
    }
    // The Python leaf always passes the resolved binary; the stub path is the
    // fixture's stand-in for it, the same value FNO_TEST_FNO_BINARY hands the
    // oracle. The uninstall command takes nothing else: no binary, no interval.
    if !matches!(case.mode, Mode::Uninstall { .. }) {
        cmd.args([
            "--fno-binary",
            &dir.join("stub").join("fno-py").display().to_string(),
        ]);
    }
    for (k, v) in pin_env(dir, case) {
        cmd.env(k, v);
    }
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.current_dir(dir);
    // All three handles piped: wait_with_output captures only piped handles,
    // and an inherited stdout would land the command's bytes on the test console
    // with an empty Output.stdout.
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    let mut child = cmd.spawn().expect("pr-watch install runs");
    if let Some(answer) = case.stdin {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(answer.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().expect("pr-watch install finishes");
    Golden {
        exit: Some(out.status.code().unwrap_or(-1)),
        streams: vec![String::from_utf8_lossy(&out.stdout).to_string()],
    }
}

/// The ensure coupling path retires legacy agents from disk even though its
/// only output is the outcome word: the receipt-less cleanup the Python leg
/// performed is preserved.
#[test]
fn ensure_retires_the_legacy_plist_from_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let case = Case {
        label: "ensure legacy disk check",
        mode: Mode::Ensure,
        stdin: None,
        legacy: true,
        agents_dir_is_file: false,
        plist: false,
        launchctl: LaunchCtl::default(),
    };
    let dir = write_case(tmp.path(), &case);
    let legacy = dir
        .join("home")
        .join("Library")
        .join("LaunchAgents")
        .join("com.fno.postmerge-old.plist");
    assert!(legacy.is_file());
    let golden = rust_leg(&dir, &case);
    assert_eq!(golden.exit, Some(0));
    assert_eq!(golden.streams[0].trim(), "activated");
    assert!(!legacy.exists(), "legacy plist must be retired");
}

/// Full uninstall: the plist leaves disk and the watermark store survives, so
/// a reinstall does not re-fire handled PRs.
#[test]
fn uninstall_removes_the_plist_from_disk() {
    let tmp = tempfile::TempDir::new().unwrap();
    let case = Case {
        label: "uninstall disk check",
        mode: Mode::Uninstall { unload_only: false },
        stdin: None,
        legacy: false,
        agents_dir_is_file: false,
        plist: true,
        launchctl: LaunchCtl {
            list: Some("sh.fno.pr-watcher"),
            ..LaunchCtl::default()
        },
    };
    let dir = write_case(tmp.path(), &case);
    let plist = dir
        .join("home")
        .join("Library")
        .join("LaunchAgents")
        .join("sh.fno.pr-watcher.plist");
    let store = dir.join("state").join("pr-watcher-state.json");
    std::fs::write(&store, r#"{"handled":["some/repo#1"]}"#).unwrap();
    assert!(plist.is_file());
    let golden = rust_leg(&dir, &case);
    assert_eq!(golden.exit, Some(0));
    assert!(golden.streams[0].contains("Watermark store preserved"));
    assert!(!plist.exists(), "the plist must leave disk");
    assert_eq!(
        std::fs::read_to_string(&store).unwrap(),
        r#"{"handled":["some/repo#1"]}"#,
        "the watermark store must survive"
    );
}

/// The --unload-only coupling word: the agent unloads, the plist stays so the
/// config forward can report without removing anything.
#[test]
fn unload_only_unloads_but_keeps_the_plist() {
    let tmp = tempfile::TempDir::new().unwrap();
    let case = Case {
        label: "unload only disk check",
        mode: Mode::Uninstall { unload_only: true },
        stdin: None,
        legacy: false,
        agents_dir_is_file: false,
        plist: true,
        launchctl: LaunchCtl {
            list: Some("sh.fno.pr-watcher"),
            ..LaunchCtl::default()
        },
    };
    let dir = write_case(tmp.path(), &case);
    let plist = dir
        .join("home")
        .join("Library")
        .join("LaunchAgents")
        .join("sh.fno.pr-watcher.plist");
    let golden = rust_leg(&dir, &case);
    assert_eq!(golden.exit, Some(0));
    assert_eq!(golden.streams[0].trim(), "unloaded");
    assert!(plist.is_file(), "--unload-only keeps the plist on disk");
}

/// A removal that fails must not read as success: the Python unlink let the
/// error propagate as a nonzero exit, so the port reports the failure and
/// skips the Removed receipt instead of printing it unconditionally.
#[test]
fn uninstall_reports_a_failed_removal() {
    let tmp = tempfile::TempDir::new().unwrap();
    let case = Case {
        label: "uninstall failed removal check",
        mode: Mode::Uninstall { unload_only: false },
        stdin: None,
        legacy: false,
        agents_dir_is_file: false,
        plist: true,
        launchctl: LaunchCtl {
            list: Some("sh.fno.pr-watcher"),
            ..LaunchCtl::default()
        },
    };
    let dir = write_case(tmp.path(), &case);
    let plist = dir
        .join("home")
        .join("Library")
        .join("LaunchAgents")
        .join("sh.fno.pr-watcher.plist");
    // remove_file cannot remove a directory, which makes the failure
    // deterministic on every platform the suite runs on.
    std::fs::remove_file(&plist).unwrap();
    std::fs::create_dir(&plist).unwrap();
    let golden = rust_leg(&dir, &case);
    assert_eq!(golden.exit, Some(1));
    assert!(
        !golden.streams[0].contains("Removed"),
        "a failed removal must not print the Removed receipt"
    );
    assert!(plist.is_dir(), "the blocker must survive for diagnosis");
}
