//! The denominator-ratio door's load-bearing contracts at the native surface:
//! the deliverables:1 bypass verdict and the `--since-days` window. The
//! Python leg pinned these and its CliRunner tests died with the wheel; the
//! parity replay carries only the empty-window shape, so these two pin the
//! arithmetic and the filter against the real binary.

use std::path::Path;
use std::process::Command;

const AGENTS_BIN: &str = env!("CARGO_BIN_EXE_fno-agents");

fn event(ts: &str, denominator: &str, count: Option<i64>) -> String {
    let count_part = count
        .map(|c| format!(r#", "count": {c}"#))
        .unwrap_or_default();
    format!(
        r#"{{"ts": "{ts}", "type": "target_denominator", "source": "target", "data": {{"denominator": "{denominator}"{count_part}}}}}"#
    )
}

fn fmt(t: chrono::NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

/// The hermetic pins both sides resolve identically: the test process writes
/// the events file through `space_dir` and the child reads it through the
/// same resolution, so the FNO_SPACES_DIR/HOME/FNO_REPO_ROOT pins must match.
fn spawn_env(root: &Path, spaces: &Path) -> Vec<(String, String)> {
    let s = |p: &Path| p.to_string_lossy().to_string();
    vec![
        ("HOME".into(), s(spaces)),
        ("FNO_SPACES_DIR".into(), s(spaces)),
        ("FNO_REPO_ROOT".into(), s(root)),
        ("FNO_TEST_HERMETIC".into(), "1".into()),
        ("FNO_STYLE_ENFORCE".into(), "0".into()),
        ("PATH".into(), std::env::var("PATH").unwrap_or_default()),
    ]
}

fn write_events(root: &Path, spaces: &Path, lines: &[String]) {
    // The resolution must see the same pins the child sees, and both tests
    // in this binary share one process env: serialize the pin window.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let guard = ENV_LOCK.lock().unwrap();
    std::env::set_var("FNO_SPACES_DIR", spaces);
    std::env::set_var("FNO_TEST_HERMETIC", "1");
    let dir = fno_agents::paths::space_dir(root);
    std::env::remove_var("FNO_SPACES_DIR");
    std::env::remove_var("FNO_TEST_HERMETIC");
    drop(guard);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn run_ratio(root: &Path, spaces: &Path, extra: &[&str]) -> (i32, String) {
    let out = Command::new(AGENTS_BIN)
        .args(["target-gates", "denominator-ratio", "--json"])
        .args(extra)
        .env_clear()
        .envs(
            spawn_env(root, spaces)
                .iter()
                .map(|(k, v)| (k.as_str(), v.as_str())),
        )
        .output()
        .expect("the fno-agents binary spawns");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn deliverables_1_dominance_flags_the_bypass() {
    let root = tempfile::tempdir().unwrap();
    let spaces = tempfile::tempdir().unwrap();
    let now = chrono::Local::now().naive_local();
    let lines = [
        event(&fmt(now), "plan", None),
        event(&fmt(now), "plan", None),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
        event(&fmt(now), "deliverables", Some(1)),
    ];
    write_events(root.path(), spaces.path(), &lines);
    let (code, out) = run_ratio(root.path(), spaces.path(), &[]);
    assert_eq!(code, 0, "{out}");
    let obj: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(obj["deliverables_1_ratio_pct"], 80.0);
    assert_eq!(obj["deliverables_1"], 8);
    assert_eq!(obj["plan_backed"], 2);
    assert!(obj["verdict"].as_str().unwrap().starts_with("bypass"));
}

#[test]
fn stale_events_fall_outside_the_window() {
    let root = tempfile::tempdir().unwrap();
    let spaces = tempfile::tempdir().unwrap();
    let now = chrono::Local::now().naive_local();
    let stale = now - chrono::Duration::days(40);
    let lines = [
        event(&fmt(stale), "deliverables", Some(1)),
        event(&fmt(now), "plan", None),
    ];
    write_events(root.path(), spaces.path(), &lines);
    let (code, out) = run_ratio(root.path(), spaces.path(), &["--since-days", "28"]);
    assert_eq!(code, 0, "{out}");
    let obj: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(obj["plan_backed"], 1);
    assert_eq!(obj["deliverables_declared"], 0);
}
