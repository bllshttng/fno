use super::*;
use crate::graph_store;

#[test]
fn reads_and_help_are_not_launches() {
    for cmd in [
        "claude --version",
        "claude --bg --help",
        "grep -n 'claude --bg' notes.md",
        "cat > notes.md <<'EOF'\nclaude --bg -n w run it\nEOF",
        "codex exec resume abc",
        "codex --help",
        "opencode",
        "echo pi",
        // AC6: a resume is a rejoin, not a launch; the subcommand table too.
        "codex resume 0197abcd-1234-7abc-9def-0123456789ab --remote unix://run.sock",
        // The resume builder emits global options before the subcommand; the
        // subcommand is still the first positional, past flags and values.
        "codex --dangerously-bypass-hook-trust --cd /w resume 0197abcd-1234-7abc-9def-0123456789ab",
        "codex app-server daemon start",
        "codex thread list",
        "codex login",
        "codex",
    ] {
        assert!(parse_command(cmd, "/repo").is_empty(), "{cmd}");
    }
}

/// A dispatcher with no Bash hook writes the same record a hook would, slot
/// included, and the child claims it.
#[test]
fn a_dispatcher_writes_a_record_the_child_claims() {
    let home = tempfile::tempdir().unwrap();
    let cwd = home.path().to_string_lossy().into_owned();
    write_record(
        home.path(),
        Launch {
            harness: "claude".into(),
            resume: None,
            name: Some("w9".into()),
            node: Some("x-ab12".into()),
            slot: Some("g2".into()),
            provider: None,
            cwd: cwd.clone(),
        },
        "lead-9",
        Some("claude"),
        &cwd,
        1_000_000,
    )
    .unwrap();
    let got = claim(
        home.path(),
        &child("c-9", "claude", &cwd, Some("w9"), 1_001_000),
    )
    .unwrap();
    assert_eq!(got.launch.slot.as_deref(), Some("g2"));
    assert_eq!(got.launch.node.as_deref(), Some("x-ab12"));
    assert_eq!(got.launcher_session, "lead-9");
}

fn child(sid: &str, harness: &str, cwd: &str, name: Option<&str>, at: i64) -> Child {
    Child {
        harness: harness.into(),
        session_id: sid.into(),
        cwd: cwd.into(),
        name: name.map(str::to_string),
        started_at_ms: at,
    }
}

#[test]
fn ambiguous_unattributed_and_stale_records_claim_nothing() {
    let home = tempfile::tempdir().unwrap();
    let cwd = home.path().to_string_lossy().into_owned();
    record_command(
        home.path(),
        "codex exec a",
        &cwd,
        Some("lead-1"),
        None,
        1_000_000,
    )
    .unwrap();
    record_command(
        home.path(),
        "codex exec b",
        &cwd,
        Some("lead-1"),
        None,
        1_000_001,
    )
    .unwrap();
    assert!(claim(home.path(), &child("c-1", "codex", &cwd, None, 1_002_000)).is_none());
    assert_eq!(
        std::fs::read_dir(home.path().join("launches/pending"))
            .unwrap()
            .count(),
        2
    );
    // No launcher session writes nothing; a record past the window is
    // never claimed.
    assert_eq!(
        record_command(home.path(), "claude --bg x", &cwd, None, None, 1).unwrap(),
        0
    );
    record_command(
        home.path(),
        "claude --bg -n w x",
        &cwd,
        Some("l"),
        None,
        1_000_000,
    )
    .unwrap();
    let late = 1_000_000 + WINDOW_MS + 1;
    assert!(claim(home.path(), &child("c", "claude", &cwd, Some("w"), late)).is_none());
}

#[test]
fn the_first_launch_edge_is_never_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    let rows = serde_json::json!([
        {"id": "x-ab12", "slug": "ab", "title": "AB", "type": "feature", "status": "idea",
         "priority": "p2", "domain": "code", "created_at": "2026-10-09T00:00:00+00:00"}
    ]);
    graph_store::seed_rows(&graph, rows.as_array().unwrap()).unwrap();
    assert_eq!(
        stamp_node(&graph, "x-ab12", "lead-1", Some("claude"), Some("/repo")).unwrap(),
        Stamp::Wrote
    );
    assert_eq!(
        stamp_node(&graph, "x-ab12", "lead-2", Some("codex"), None).unwrap(),
        Stamp::Kept("lead-1".into())
    );
    assert_eq!(
        stamp_node(&graph, "x-none", "lead-1", None, None).unwrap(),
        Stamp::NoNode
    );
    let back = graph_store::read_rows(&graph).unwrap();
    let row = back.iter().find(|r| r["id"] == "x-ab12").unwrap();
    assert_eq!(row["spawned_by_session"], "lead-1");
    assert_eq!(row["spawned_by_cwd"], "/repo");
}

/// The hand-typed revive that crossed two rows: an unnamed `--bg --resume`
/// of a session still running.
#[test]
fn an_unnamed_or_forking_claude_bg_line_is_refused() {
    let line = "cd /w/fb-9b9b && claude --bg --resume 58b9fbe4-b512 --model opus \"Lead Hope: go\"";
    let launches = parse_command(line, "/repo");
    assert_eq!(launches[0].resume.as_deref(), Some("58b9fbe4-b512"));
    let forks = claude_bg_refusal(&launches, |id| id == "58b9fbe4-b512").unwrap();
    assert!(forks.contains("still running"), "{forks}");
    let unnamed = claude_bg_refusal(&launches, |_| false).unwrap();
    assert!(unnamed.contains("--name"), "{unnamed}");
    // A named line passes, and so does a bare `-r` picker, which names no id.
    for ok in [
        "claude --bg --name t-fb-9b9b --resume 58b9fbe4-b512 go",
        "claude --bg -r -n w",
    ] {
        let launches = parse_command(ok, "/repo");
        assert!(claude_bg_refusal(&launches, |_| true).is_none(), "{ok}");
    }
}
