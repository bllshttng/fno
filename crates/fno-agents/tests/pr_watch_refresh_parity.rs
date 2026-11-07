//! parity-stage: characterization
//! parity-oracle: gone (cli/src/fno/pr_watch died with the Python runtime; the
//! committed goldens under tests/golden/pr-watch-refresh/ stay as the frozen
//! contract)
//!
//! Characterization for the pr-watch refresh port: the Rust command
//! (`fno-agents pr-watch refresh`, reached through `pr_watch::refresh`) is
//! pinned by the frozen goldens under tests/golden/pr-watch-refresh/. The
//! goldens froze from the live Python leg (`refresh_watcher` +
//! `heal_status_line`) while it lived; the frozen files are the contract.
//!
//! Four fixture states: the watcher disabled, a tick in flight (bounce
//! deferred), a plain bounce (bootout/bootstrap/kickstep scripted), and an
//! unchanged plist (no bounce). The bounce cases never touch the real
//! launchctl: `FNO_TEST_PR_WATCH_LAUNCHCTL` scripts the steps on both legs,
//! the way the load-state pin does for status.

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{assert_golden, slug, Golden};

mod common;

/// Repo `cli/src` so Python can import the real `fno` package.
/// Case directories and the bounce's gui uid are the only machine-dependent
/// parts of the readout; timestamps ride no stdout.
fn masked(text: &str, dir: &Path) -> String {
    let uid = regex::Regex::new(r"gui/\d+").unwrap();
    let rooted = text.replace(&dir.display().to_string(), "<CASE>");
    uid.replace_all(&rooted, "gui/<UID>").into_owned()
}

/// One fixture case: a config file, a state root with the events journal,
/// and a LaunchAgents dir holding (or not) the plist.
struct Case {
    label: &'static str,
    enabled: bool,
    /// Pre-render the plist with the Rust renderer and write it, so the
    /// run reads "unchanged". The Python oracle renders with its own
    /// renderer over the same inputs: a byte difference between the two
    /// renderers fails the capture instead of hiding.
    prerender: bool,
    tick_pid: Option<u32>,
    /// Scripted launchctl steps as `[rc, timed]` pairs, in invocation order.
    launchctl: Vec<(i32, bool)>,
    force_bounce: bool,
}

fn write_case(root: &Path, case: &Case) -> PathBuf {
    let dir = root.join(slug(case.label));
    let agents = dir.join("LaunchAgents");
    let state = dir.join("state");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::create_dir_all(&state).unwrap();

    // state_dir is a TOP-LEVEL key: under a [table] header the config loader
    // reads it as pr_watch.state_dir and warns it is unmodeled.
    let mut config = format!("state_dir = \"{}\"\n\n", state.display());
    config.push_str("[pr_watch]\n");
    config.push_str(&format!("enabled = {}\n", case.enabled));
    config.push_str("interval_seconds = 600\n");
    std::fs::write(dir.join("config.toml"), config).unwrap();

    if case.prerender {
        let plist_text =
            fno_agents::pr_watch::render_plist_for_test(&agents, "/usr/local/bin/fno", None, 600);
        std::fs::write(agents.join("sh.fno.pr-watcher.plist"), plist_text).unwrap();
    } else {
        std::fs::write(agents.join("sh.fno.pr-watcher.plist"), "# fixture plist\n").unwrap();
    }

    std::fs::write(state.join("events.jsonl"), "").unwrap();
    dir
}

/// The env pins both legs inherit: one config file, one state root, the
/// injected tick-in-flight answer, and the scripted launchctl steps.
fn pin_env(dir: &Path, case: &Case) -> Vec<(String, String)> {
    let mut pins = vec![
        // Pin the launchd-label suffix so receipts and rendered plists stay
        // byte-for-byte across machines (x-781d).
        (
            fno_agents::launchd_labels::FNO_TEST_LAUNCHD_SUFFIX.into(),
            "testhome".into(),
        ),
        (
            "FNO_CONFIG".into(),
            dir.join("config.toml").display().to_string(),
        ),
        (
            "FNO_STATE_DIR".into(),
            dir.join("state").display().to_string(),
        ),
        (
            "FNO_TEST_PR_WATCH_LAUNCH_AGENTS_DIR".into(),
            dir.join("LaunchAgents").display().to_string(),
        ),
    ];
    if let Some(pid) = case.tick_pid {
        pins.push(("FNO_TEST_PR_WATCH_TICK_PID".into(), pid.to_string()));
    }
    if !case.launchctl.is_empty() {
        let script: Vec<String> = case
            .launchctl
            .iter()
            .map(|(rc, timed)| format!("[{rc},{timed}]"))
            .collect();
        pins.push((
            "FNO_TEST_PR_WATCH_LAUNCHCTL".into(),
            format!("[{}]", script.join(",")),
        ));
    }
    pins
}

/// The Rust leg through the built binary: the exact surface the Python leaf
/// forwards to.
fn rust_leg(dir: &Path, case: &Case) -> Golden {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args(["pr-watch", "refresh", "--fno-binary", "/usr/local/bin/fno"]);
    if case.force_bounce {
        cmd.arg("--force-bounce");
    }
    for (k, v) in pin_env(dir, case) {
        cmd.env(k, v);
    }
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.current_dir(dir);
    let out = cmd.output().expect("pr-watch refresh runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Golden {
        exit: Some(out.status.code().unwrap_or(-1)),
        streams: vec![stdout],
    }
}

/// The old leg through its own Python surface, capture mode only.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "disabled watcher skips the refresh",
            enabled: false,
            prerender: false,
            tick_pid: None,
            launchctl: vec![],
            force_bounce: false,
        },
        Case {
            label: "tick in flight defers the bounce",
            enabled: true,
            prerender: false,
            tick_pid: Some(4242),
            launchctl: vec![],
            force_bounce: false,
        },
        Case {
            label: "plain bounce runs the three launchctl steps",
            enabled: true,
            prerender: false,
            tick_pid: Some(0),
            launchctl: vec![(0, false), (0, false), (0, false)],
            force_bounce: false,
        },
        Case {
            label: "unchanged plist skips the bounce",
            enabled: true,
            prerender: true,
            tick_pid: Some(0),
            launchctl: vec![],
            force_bounce: false,
        },
    ]
}

#[test]
fn pr_watch_refresh_goldens() {
    // The prerendered fixture plists render in THIS process, so the seam
    // must hold here too: without it the in-process label (real-HOME hash)
    // never matches the subprocess's pinned label and every plist reads
    // "changed".
    std::env::set_var(
        fno_agents::launchd_labels::FNO_TEST_LAUNCHD_SUFFIX,
        "testhome",
    );
    let tmp = tempfile::TempDir::new().unwrap();
    for case in cases() {
        let dir = write_case(tmp.path(), &case);
        let rust = rust_leg(&dir, &case);
        let rust = Golden {
            exit: rust.exit,
            streams: rust.streams.into_iter().map(|s| masked(&s, &dir)).collect(),
        };
        let oracle = None;
        assert_golden("pr-watch-refresh", case.label, &rust, oracle);
    }
}
