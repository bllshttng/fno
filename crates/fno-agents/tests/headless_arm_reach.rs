//! `spawn --substrate headless` reaches the harness's own headless arm for pi
//! and footnote. Before the pi arm existed the pi call fell through to the
//! daemon and died on "daemon PTY hosting was retired ... use --substrate
//! bg|headless", a refusal that named the substrate the caller had asked for.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Command, Output};

fn temp_dir(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "fno-headless-reach-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn client_bin() -> Option<PathBuf> {
    let bin = match option_env!("CARGO_BIN_EXE_fno-agents") {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("target/debug/fno-agents"),
    };
    bin.exists().then_some(bin)
}

fn spawn_headless(bin: &PathBuf, harness: &str, path: String) -> Output {
    Command::new(bin)
        .envs(fno_agents::test_run::self_owner_env())
        .args(["spawn", "reach-worker", "hello", "--harness", harness])
        .args(["--substrate", "headless"])
        .arg("--cwd")
        .arg(temp_dir("cwd"))
        .env("FNO_SPAWN_GATE", "0")
        .env("FNO_E2E", "1")
        .env("FNO_AGENTS_HOME", temp_dir("home"))
        .env("HOME", temp_dir("user-home"))
        .env_remove("ANTHROPIC_BASE_URL")
        .env("PATH", path)
        .output()
        .expect("run fno-agents spawn")
}

#[test]
fn pi_headless_runs_one_print_turn_and_returns_its_reply() {
    let Some(bin) = client_bin() else {
        eprintln!("skipping: fno-agents binary unavailable");
        return;
    };
    let base = temp_dir("pi");
    let bin_dir = base.join("bin");
    fs::create_dir_all(&bin_dir).unwrap();
    let argv_log = base.join("pi-argv");
    let pi = bin_dir.join("pi");
    let stream = r#"{"type":"agent_end","messages":[{"role":"assistant","content":[{"type":"text","text":"pong"}],"stopReason":"stop"}]}"#;
    fs::write(
        &pi,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv_log:?}\nprintf '%s\\n' '{stream}'\n"),
    )
    .unwrap();
    fs::set_permissions(&pi, fs::Permissions::from_mode(0o755)).unwrap();

    let output = spawn_headless(&bin, "pi", format!("{}:/usr/bin:/bin", bin_dir.display()));
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert_eq!(output.status.code(), Some(0), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "pong");
    let argv = fs::read_to_string(&argv_log).expect("the pi arm ran the fake pi");
    let argv: Vec<&str> = argv.lines().collect();
    assert_eq!(
        &argv[..5],
        ["--print", "--mode", "json", "--no-session", "--provider"]
    );
}

#[test]
fn footnote_headless_reaches_its_own_turn_loop() {
    let Some(bin) = client_bin() else {
        eprintln!("skipping: fno-agents binary unavailable");
        return;
    };
    let output = spawn_headless(&bin, "footnote", "/usr/bin:/bin".to_string());
    let stderr = String::from_utf8_lossy(&output.stderr);

    // With no model endpoint configured, footnote's loop refuses by name. A
    // gate or a daemon fall-through would print a thread-lane or PTY-hosting
    // refusal instead.
    assert!(stderr.contains("no model endpoint"), "{stderr}");
    assert!(!stderr.contains("thread lane"), "{stderr}");
    assert!(!stderr.contains("PTY hosting"), "{stderr}");
}
