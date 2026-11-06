use super::*;
use crate::state::MuxRef;
use crate::AgentStatus;

fn roster_worker(session_id: &str, pid: u32, repl_pid: u32) -> crate::claude_roster::RosterWorker {
    serde_json::from_value(json!({"sessionId": session_id, "pid": pid, "replPid": repl_pid}))
        .unwrap()
}

/// A home with no keep marker and no workspace file.
fn no_home() -> crate::paths::AgentsHome {
    crate::paths::AgentsHome::at("/nonexistent/marks-home")
}

fn row(name: &str, status: AgentStatus) -> RegistryEntry {
    RegistryEntry {
        name: name.into(),
        status,
        harness: Some("claude".into()),
        created_at: "2026-01-01T00:00:00Z".into(),
        ..Default::default()
    }
}

/// The union: a roster session shows once under its short id, and the fno
/// row it dedups into hands it the lead and the registry join. A pid-less
/// claude row lives through the roster, a pid-less codex row through its
/// slot claim, and a dead pid drops out. Every row reads its stored token.
#[test]
fn the_census_dedups_the_roster_and_keeps_each_liveness_arm() {
    let roster = vec![roster_worker(
        "aaaa1111-0000-0000-0000-000000000000",
        100,
        101,
    )];
    let mut adopted = row("sorrel", AgentStatus::Live);
    adopted.short_id = "aaaa1111".into();
    adopted.harness_session_id = Some("aaaa1111-0000-0000-0000-000000000000".into());
    adopted.spawned_by_session = Some("1eadbeef-lead".into());
    let mut thread = row("t-x-1234-sol", AgentStatus::Idle);
    thread.harness = Some("codex".into());
    let mut pane = row("t-x-5678-glm", AgentStatus::Spawning);
    pane.pid = Some(300);
    pane.mux = Some(MuxRef {
        session: "main".into(),
        pane_id: 4,
    });
    let mut dead = row("gone", AgentStatus::Busy);
    dead.pid = Some(400);
    let exited = row("done", AgentStatus::Exited);
    // Its start time cannot be read: counted, but no evidence for a rewrite.
    let mut unread = row("t-x-9abc-glm", AgentStatus::Spawning);
    unread.pid = Some(500);
    unread.pid_start_time = Some(1);
    let rows = vec![adopted, thread, pane, dead, exited, unread];

    let state = |pid: u32, _: Option<u64>| match pid {
        100 | 101 | 300 => Some(true),
        500 => None,
        _ => Some(false),
    };
    let claim = |name: &str| name == "t-x-1234-sol";
    let c = census(
        &roster,
        &rows,
        &state,
        &claim,
        &HashSet::new(),
        chrono::Utc::now(),
    );

    let names: Vec<&str> = c.workers.iter().map(|w| w.name.as_str()).collect();
    assert_eq!(
        names,
        ["aaaa1111", "t-x-1234-sol", "t-x-5678-glm", "t-x-9abc-glm"]
    );
    assert_eq!(c.workers[3].stored_status, "spawning");
    assert_eq!(c.workers[3].status_basis, None);
    assert!(c.warnings[0].contains("t-x-9abc-glm"), "{:?}", c.warnings);
    let shown = &c.workers[0];
    assert_eq!(
        shown.session_pid,
        Some(101),
        "cost reads the session process, not the host"
    );
    assert_eq!(shown.spawned_by.as_deref(), Some("1eadbeef-lead"));
    assert_eq!(shown.entry, Some(0));
    assert_eq!(c.workers[1].substrate, "worker");
    assert_eq!(c.workers[2].substrate, "pane");
    assert_eq!(c.workers[2].stored_status, "quiet");
    assert_eq!(
        c.workers[2].status_basis.as_deref(),
        Some("stale-spawning-live-pid")
    );
    assert!(c.live_registry_names.contains("sorrel"));
}

/// AC7-HP and AC8-ERR: a read worker shows its trailing refusals and its
/// last turn; a worker with no reading carries unmeasured keys, renders `-`,
/// and shifts no other column.
#[test]
fn receipt_columns_render_and_unmeasured_reads_dash() {
    let mut workers = vec![
        json!({"name": "sorrel", "harness": "claude", "session_id": "s1", "status": "busy"}),
        json!({"name": "stray", "harness": "kiro", "session_id": null, "status": "busy"}),
    ];
    let read = |_: &str, _: &str| {
        json!({"basis": "transcript",
               "denied": {"last_turn": {}, "trailing": {"window": 50,
                   "permission_denied": 2, "user_rejected": 1, "hook_blocked": 4}},
               "last_turn": {"age_s": 240, "provenance": "relay_fno_mail", "tool_calls": 12}})
    };
    attach_receipts(&mut workers, &read);
    assert!(workers[1]["denied"]["unmeasured"].is_string());
    assert!(workers[1]["last_turn"]["unmeasured"].is_string());
    let text = render(&json!({"workers": workers}));
    let line = |name: &str| text.lines().find(|l| l.contains(name)).unwrap().to_string();
    let (read_line, stray_line) = (line("sorrel"), line("stray"));
    assert!(read_line.contains("     3 4m relay 12c"), "{read_line}");
    assert!(stray_line.contains("     - -"), "{stray_line}");
    assert_eq!(read_line.find("busy"), stray_line.find("busy"));
}

/// The STATUS column says blocked for a harness-blocked row even when the
/// transcript age is fresh: the retry's limit error must not read as
/// writing. The stored-live word alone would render writing here.
#[test]
fn the_status_column_says_blocked_over_a_fresh_transcript_age() {
    let mut entry = row("sorrel", AgentStatus::Live);
    entry.short_id = "aaaa1111".into();
    let worker = Worker {
        source: "fno",
        name: "sorrel".into(),
        harness: "claude".into(),
        substrate: "pane".into(),
        pid: None,
        session_pid: None,
        stored_status: "live".into(),
        status_basis: None,
        session_id: Some("aaaa1111-0000-0000-0000-000000000000".into()),
        spawned_by: None,
        entry: Some(0),
    };
    let mut blocked = HashSet::new();
    blocked.insert("aaaa1111".to_string());
    let c = Census {
        workers: vec![worker],
        ..Default::default()
    };
    let out = worker_rows(
        &c,
        &[entry],
        &HashMap::new(),
        crate::truth_probe::BatchOutcome::Measured,
        &Map::new(),
        &BTreeMap::new(),
        &blocked,
        &Marks::read(&no_home()),
    );
    assert_eq!(out[0]["status"], "blocked");
    assert_eq!(
        out[0]["stored_status"], "live",
        "the stored token is untouched; the render arm carries the override"
    );
}
