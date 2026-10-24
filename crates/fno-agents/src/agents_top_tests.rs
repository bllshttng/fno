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

/// A joined worker carries its seat facts from the registry row and the
/// truth reading; a worker with neither carries the same keys as null, and
/// the row still serializes.
#[test]
fn worker_rows_carry_seat_facts_and_null_when_unread() {
    let checkout = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(checkout.path().join(".fno")).unwrap();
    std::fs::write(
        checkout.path().join(".fno/config.toml"),
        "[project]\nid = \"fno\"\n",
    )
    .unwrap();
    let sid = "aaaa1111-2222-3333-4444-555566667777";
    let mut entry = row("sorrel", AgentStatus::Live);
    entry.model = Some("opus".into());
    entry.cwd = checkout.path().display().to_string();
    entry.harness_session_id = Some(sid.into());
    let worker = |name: &str, entry: Option<usize>, session_id: Option<&str>| Worker {
        source: "fno",
        name: name.into(),
        harness: "claude".into(),
        substrate: "pane".into(),
        pid: None,
        session_pid: None,
        stored_status: "live".into(),
        status_basis: None,
        session_id: session_id.map(str::to_string),
        spawned_by: None,
        entry,
    };
    let c = Census {
        workers: vec![
            worker("sorrel", Some(0), Some(sid)),
            worker("stray", None, None),
        ],
        ..Default::default()
    };
    let rows = vec![entry];
    let mut truth = HashMap::new();
    truth.insert(
        truth_handle(&c.workers[0], Some(&rows[0])),
        crate::truth_probe::TruthProbe {
            state: "working".into(),
            reachability: Some("reachable".into()),
            basis: Some("transcript".into()),
            last_activity_age_s: Some(53.0),
            last_activity_basis: None,
            last_event_at: Some("2026-10-23T10:00:00Z".into()),
            last_message: Some("x".repeat(300)),
            observed_model: json!({"kind": "observed", "model": "sonnet"}),
            provider_refusal: None,
            harness_title: None,
        },
    );
    let out = worker_rows(
        &c,
        &rows,
        &truth,
        crate::truth_probe::BatchOutcome::Measured,
        &Map::new(),
        &BTreeMap::new(),
    );
    let seat = out.iter().find(|r| r["name"] == "sorrel").unwrap();
    assert_eq!(seat["model"], "opus");
    assert_eq!(seat["model_basis"], "registry");
    assert_eq!(seat["project"], "fno");
    assert_eq!(seat["session_id"], sid);
    assert_eq!(seat["started_at"], "2026-01-01T00:00:00Z");
    assert_eq!(seat["last_message_at"], "2026-10-23T10:00:00Z");
    assert_eq!(seat["last_message"].as_str().unwrap().chars().count(), 240);

    let stray = out.iter().find(|r| r["name"] == "stray").unwrap();
    for key in [
        "session_id",
        "model",
        "started_at",
        "last_message",
        "cwd",
        "project",
        "context_pct",
    ] {
        assert!(stray[key].is_null(), "{key}: {}", stray[key]);
    }
    assert!(serde_json::to_string(stray).is_ok());
}
