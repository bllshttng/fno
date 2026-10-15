//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr_watch/cli.py:heal
//!
//! Characterization for the pr-watch heal port: the Rust verb
//! (`fno-agents pr-watch heal`) is pinned by the frozen goldens under
//! tests/golden/pr-watch-heal/. The goldens freeze the live Python leaf's
//! bytes; this change replaced that body with a forward, so capture mode
//! replays the fixture oracle (`tests/fixtures/pr_watch_heal/oracle.py`)
//! rather than a surviving leg, and the frozen files are the contract.
//!
//! Seven fixture states: the watcher disabled, a bounce pending its first
//! tick, a held single-flight, the plain cure, the missing-plist cure (the
//! re-render installs it), a kickstart failure, and a tick in flight
//! (bounce deferred). The probe and the flight are env pins on the oracle
//! and fixture state on the Rust side: a fresh plist with no tick row reads
//! `healthy-pending` on the verdict tree both legs share. The bounce cases
//! never touch the real launchctl: `FNO_TEST_PR_WATCH_LAUNCHCTL` scripts
//! the steps on both legs, the way the load-state pin does for status.

use std::path::{Path, PathBuf};
use std::process::Command;

use common::{assert_golden, capture_mode, python_executable, slug, Golden};

mod common;

/// Repo `cli/src` so Python can import the real `fno` package.
fn pythonpath() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/src")
}

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
    /// Write a plist at all; the missing-plist cure case leaves it absent.
    plist: bool,
    /// The watcher's agent reads as loaded in `launchctl list`.
    loaded: bool,
    /// Append a fresh `pr_watch_tick` row, so the plist's age reads as an
    /// already-ticking watcher rather than a pending bounce.
    fresh_tick: bool,
    /// Pre-seed the `pr-watch:heal` flight under the case's claims root.
    flight_held: bool,
    /// The probe answer the oracle replays; None reads as a probe that
    /// raised. The Rust side never reads this pin: the fixture state above
    /// drives its own verdict to the same answer.
    probe: Option<&'static str>,
    tick_pid: Option<u32>,
    /// Scripted launchctl steps as `[rc, timed]` pairs, in invocation order.
    launchctl: Vec<(i32, bool)>,
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

    if case.plist {
        std::fs::write(agents.join("sh.fno.pr-watcher.plist"), "# fixture plist\n").unwrap();
    }

    let mut events = String::new();
    if case.fresh_tick {
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.6fZ");
        events.push_str(&format!(
            "{{\"ts\":\"{ts}\",\"type\":\"pr_watch_tick\",\"data\":{{}}}}\n"
        ));
    }
    std::fs::write(state.join("events.jsonl"), events).unwrap();
    dir
}

/// The env pins both legs inherit: one config file, one state root, one
/// claims root, the load-state answer, the injected tick-in-flight answer,
/// and the scripted launchctl steps.
fn pin_env(dir: &Path, case: &Case) -> Vec<(String, String)> {
    let mut pins = vec![
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
        (
            "FNO_CLAIMS_ROOT".into(),
            dir.join("claims").display().to_string(),
        ),
        (
            "FNO_TEST_PR_WATCH_LOADED".into(),
            if case.loaded { "1" } else { "0" }.into(),
        ),
    ];
    if case.flight_held {
        pins.push(("FNO_TEST_HEAL_FLIGHT".into(), "held".into()));
    }
    if let Some(probe) = case.probe {
        pins.push(("FNO_TEST_HEAL_PROBE".into(), probe.into()));
    }
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

/// Seed the held flight through the binary's own gate, so the heal verb's
/// in-process acquire meets the same claim layout the CLI verb writes.
fn seed_flight(dir: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args([
            "claim",
            "flight-acquire",
            "pr-watch:heal",
            "--holder",
            "sessionstart-other",
            "--scope",
            "pr-watch SessionStart self-heal",
            "--ttl-ms",
            "300000",
            "--claims-root",
            &dir.join("claims").display().to_string(),
        ])
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("flight-acquire seeds");
    assert!(
        out.status.success(),
        "the flight seed must acquire: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The Rust leg through the built binary: the exact surface the Python leaf
/// forwards to.
fn rust_leg(dir: &Path, case: &Case) -> Golden {
    if case.flight_held {
        seed_flight(dir);
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args([
        "pr-watch",
        "heal",
        "--fno-binary",
        "/usr/local/bin/fno",
        "--caller",
        "heal",
    ]);
    for (k, v) in pin_env(dir, case) {
        cmd.env(k, v);
    }
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.current_dir(dir);
    let out = cmd.output().expect("pr-watch heal runs");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Golden {
        exit: Some(out.status.code().unwrap_or(-1)),
        streams: vec![stdout],
    }
}

/// The old leg through its replay oracle, capture mode only.
fn python_oracle(dir: &Path, case: &Case) -> Golden {
    let oracle_script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pr_watch_heal/oracle.py");
    let mut cmd = Command::new(python_executable());
    cmd.arg(&oracle_script);
    cmd.arg("--case").arg(dir);
    for (k, v) in pin_env(dir, case) {
        cmd.env(k, v);
    }
    cmd.env("PYTHONPATH", pythonpath());
    cmd.current_dir(dir);
    let out = cmd.output().expect("oracle runs");
    // stdout + exit are the pinned contract: interpreter stderr is this
    // machine's noise.
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    Golden {
        exit: Some(out.status.code().unwrap_or(-1)),
        streams: vec![stdout],
    }
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "disabled watcher skips the heal",
            enabled: false,
            plist: true,
            loaded: false,
            fresh_tick: false,
            flight_held: false,
            probe: None,
            tick_pid: None,
            launchctl: vec![],
        },
        Case {
            label: "bounce pending skips the heal",
            enabled: true,
            plist: true,
            loaded: true,
            fresh_tick: false,
            flight_held: false,
            probe: Some(
                "{\"bounce_pending\": true, \"detail\": \"installed recently; awaiting first tick\"}",
            ),
            tick_pid: None,
            launchctl: vec![],
        },
        Case {
            label: "held flight skips the heal",
            enabled: true,
            plist: true,
            loaded: true,
            fresh_tick: true,
            flight_held: true,
            probe: None,
            tick_pid: None,
            launchctl: vec![],
        },
        Case {
            label: "plain cure bounces the watcher",
            enabled: true,
            plist: true,
            loaded: true,
            fresh_tick: true,
            flight_held: false,
            probe: None,
            tick_pid: Some(0),
            launchctl: vec![(0, false), (0, false), (0, false)],
        },
        Case {
            label: "missing plist cure renders and bounces",
            enabled: true,
            plist: false,
            loaded: true,
            fresh_tick: true,
            flight_held: false,
            probe: None,
            tick_pid: Some(0),
            launchctl: vec![(0, false), (0, false), (0, false)],
        },
        Case {
            label: "kickstart failure exits nonzero",
            enabled: true,
            plist: true,
            loaded: true,
            fresh_tick: true,
            flight_held: false,
            probe: None,
            tick_pid: Some(0),
            launchctl: vec![(0, false), (0, false), (1, false)],
        },
        Case {
            label: "tick in flight defers the bounce",
            enabled: true,
            plist: true,
            loaded: true,
            fresh_tick: true,
            flight_held: false,
            probe: None,
            tick_pid: Some(4242),
            launchctl: vec![],
        },
    ]
}

#[test]
fn pr_watch_heal_goldens() {
    let tmp = tempfile::TempDir::new().unwrap();
    for case in cases() {
        let dir = write_case(tmp.path(), &case);
        let rust = rust_leg(&dir, &case);
        let rust = Golden {
            exit: rust.exit,
            streams: rust.streams.into_iter().map(|s| masked(&s, &dir)).collect(),
        };
        let oracle = capture_mode().then(|| {
            let golden = python_oracle(&dir, &case);
            Golden {
                exit: golden.exit,
                streams: golden
                    .streams
                    .into_iter()
                    .map(|s| masked(&s, &dir))
                    .collect(),
            }
        });
        assert_golden("pr-watch-heal", case.label, &rust, oracle);
    }
}
