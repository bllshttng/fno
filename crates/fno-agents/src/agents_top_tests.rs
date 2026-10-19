use super::*;
use crate::state::MuxRef;
use crate::AgentStatus;

fn roster_worker(session_id: &str, pid: u32, repl_pid: u32) -> crate::claude_roster::RosterWorker {
    serde_json::from_value(json!({"sessionId": session_id, "pid": pid, "replPid": repl_pid}))
        .unwrap()
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
    let c = census(&roster, &rows, &state, &claim, chrono::Utc::now());

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

/// Deltas within the journal-latest session; a counter that went down is a
/// server restart, reported born-and-gone, never a negative delta.
#[test]
fn pane_counters_difference_the_latest_session_and_report_resets() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("events.jsonl");
    let sample = |ts: &str, session: &str, panes: Value| {
        json!({"type": "mux_pane_counters", "ts": ts, "data": {"session": session, "panes": panes}})
            .to_string()
    };
    let pane = |id: i64, bytes: i64| {
        json!({"pane_id": id, "name": "w", "bytes_in": bytes, "grid_updates": 1,
               "frames_composited": 1, "frames_emitted": 1, "cpu_ns": 2_000_000})
    };
    let lines = [
        sample("2026-10-09T10:00:00Z", "other", json!([pane(9, 1)])),
        sample(
            "2026-10-09T10:00:30Z",
            "main",
            json!([pane(1, 100), pane(2, 500), pane(3, 7)]),
        ),
        sample(
            "2026-10-09T10:01:00Z",
            "main",
            json!([pane(1, 160), pane(2, 20), pane(4, 1)]),
        ),
    ];
    std::fs::write(&path, lines.join("\n") + "\n").unwrap();
    let out = pane_counter_rows(&path);
    assert_eq!(out["status"], "ok");
    assert_eq!(out["session"], "main");
    assert_eq!(out["window_s"], 30.0);
    assert_eq!(out["rows"].as_array().unwrap().len(), 1);
    assert_eq!(out["rows"][0]["bytes_in"], 60);
    assert_eq!(out["born"], json!([2, 4]));
    assert_eq!(out["gone"], json!([2, 3]));
    assert_eq!(
        pane_counter_rows(&dir.path().join("none.jsonl"))["status"],
        "insufficient-samples"
    );

    // The rotated sibling is read after the live journal but holds the
    // oldest sample: time order, not file order, picks the pair.
    let live = dir.path().join("live.jsonl");
    let rotated = format!("{}{}.1", live.display(), crate::events::EPHEMERAL_SUFFIX);
    std::fs::write(&live, lines[1..].join("\n") + "\n").unwrap();
    std::fs::write(
        &rotated,
        sample("2026-10-09T09:59:00Z", "main", json!([pane(1, 1)])) + "\n",
    )
    .unwrap();
    let out = pane_counter_rows(&live);
    assert_eq!(out["window_s"], 30.0);
    assert_eq!(out["rows"][0]["bytes_in"], 60);
}
