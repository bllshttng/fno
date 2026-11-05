//! State-layer unit tests. Moved verbatim out of `state.rs`, which is over
//! the 5,000-line budget and therefore shrink-only; test motion is the
//! sanctioned shrink. The `#[path]` child below keeps loading from
//! `src/state/tests/`, exactly as the inline module did.

use super::*;

fn tmpdir(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-state-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn sample_entry(name: &str) -> RegistryEntry {
    RegistryEntry {
        substrate: None,
        node: None,
        spawned_by_session: None,
        spawned_by_harness: None,
        spawned_by_cwd: None,
        launch_account: None,
        related_session_id: None,
        name: name.into(),
        short_id: format!("{name}-id"),
        legacy_provider: String::new(),
        provider: None,
        model: None,
        model_basis: None,
        effort: None,
        harness: Some("codex".into()),
        harness_session_id: None,
        predecessor_session_ids: Vec::new(),
        forked_from_session_id: None,
        route_provider_id: None,
        model_name: None,
        account_record_id: None,
        cwd: "/tmp/x".into(),
        project_root: "/tmp/x".into(),
        session_id: Some("uuid-1".into()),
        claude_session_uuid: None,
        messaging_socket_path: None,
        codex_session_id: Some("uuid-1".into()),
        gemini_session_id: None,
        mcp_channel_id: None,
        host_mode: None,
        cc_session_id: None,
        status: AgentStatus::Live,
        last_message_at: None,
        created_at: "2026-05-24T00:00:00Z".into(),
        pid: Some(1234),
        pid_start_time: None,
        keeper_child_pid: None,
        log_path: None,
        last_reconciled_at: None,
        inside_leg: None,
        exited_at: None,
        mux: None,
        screen_state: None,
        role_level: None,
        role_scope: None,
        role_grantor: None,
        route_settings_path: None,
        fno_id: None,
        delivery_policy: None,
        sandbox_posture: None,
        git_grant: None,
        origin: None,
        spawn_trigger: None,
        legacy_claude_short_id: None,
        ..Default::default()
    }
}

#[test]
fn state_update_registry_enforces_one_live_ref() {
    // Write-time invariant (brief Locked 7): a mux ref alongside a worker
    // short_id (or a bg claude_short_id) is refused; the store is left
    // untouched and the lock released (a later clean write succeeds).
    let dir = tmpdir("one-ref");
    let path = dir.join("registry.json");
    let res = update_registry(&path, |r| {
        let mut e = sample_entry("double"); // sample has short_id set
        e.mux = Some(MuxRef {
            session: "main".into(),
            pane_id: 1,
        });
        r.entries.push(e);
    });
    assert!(
        matches!(res, Err(StateError::InvariantViolation(_))),
        "double-ref row must be refused: {res:?}"
    );
    assert!(
        load_registry(&path).unwrap().entries.is_empty(),
        "refused write must not persist"
    );
    // bg-thread ref (jobId in short_id, v9) + mux is refused the same way.
    let res = update_registry(&path, |r| {
        let mut e = sample_entry("bg-double");
        e.short_id = "abcd1234".into();
        e.mux = Some(MuxRef {
            session: "main".into(),
            pane_id: 2,
        });
        r.entries.push(e);
    });
    assert!(matches!(res, Err(StateError::InvariantViolation(_))));
    // A clean mux-only row persists (lock was released by the refusals).
    update_registry(&path, |r| {
        let mut e = sample_entry("clean");
        e.short_id = String::new();
        e.mux = Some(MuxRef {
            session: "main".into(),
            pane_id: 3,
        });
        r.entries.push(e);
    })
    .unwrap();
    let reg = load_registry(&path).unwrap();
    assert_eq!(reg.entries.len(), 1);
    assert_eq!(reg.schema_version, REGISTRY_SCHEMA_VERSION);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn succession_preserves_thread_id_and_branch_keeps_two_rows() {
    let mut predecessor = sample_entry("worker");
    predecessor.fno_id = Some("thread-a".into());
    predecessor.harness_session_id = Some("session-a".into());
    predecessor.short_id = "transport-a".into();
    predecessor.session_id = Some("session-a".into());
    predecessor.codex_session_id = Some("session-a".into());
    predecessor.log_path = Some("/tmp/session-a.log".into());
    predecessor.role_level = Some(2);
    predecessor.role_scope = Some("scope-a".into());
    predecessor.role_grantor = Some("human".into());
    predecessor.mux = Some(MuxRef {
        session: "main".into(),
        pane_id: 4,
    });

    assert!(predecessor.apply_succession("session-a", "session-b"));
    assert_eq!(predecessor.fno_id.as_deref(), Some("thread-a"));
    assert_eq!(predecessor.harness_session_id.as_deref(), Some("session-b"));
    assert_eq!(predecessor.predecessor_session_ids, vec!["session-a"]);
    assert!(!predecessor.apply_succession("session-a", "session-c"));

    let branch = predecessor.fork_for_session("worker-branch", "session-c", "session-b");
    assert_eq!(predecessor.harness_session_id.as_deref(), Some("session-b"));
    assert_eq!(branch.harness_session_id.as_deref(), Some("session-c"));
    assert_eq!(branch.forked_from_session_id.as_deref(), Some("session-b"));
    // The branch carries no id of its own; the registry write mints it one.
    assert_eq!(branch.fno_id, None);
    assert!(branch.role_level.is_none());
    assert!(branch.role_scope.is_none());
    assert!(branch.role_grantor.is_none());
    assert!(branch.short_id.is_empty());
    assert!(branch.session_id.is_none());
    assert!(branch.codex_session_id.is_none());
    assert!(branch.log_path.is_none());
    assert!(branch.mux.is_none());
    assert!(branch.pid.is_none());
}

// rename_agent: label mutation with identity lock + alias carry.

#[test]
fn rename_agent_renames_and_carries_the_old_label_as_alias() {
    let dir = tmpdir("rename-happy");
    let path = dir.join("registry.json");
    update_registry(&path, |registry| {
        let mut e = sample_entry("worker-a");
        e.harness = Some("claude".into());
        e.harness_session_id = Some("aaaaaaaa-0000-0000-0000-111111111111".into());
        registry.entries.push(e);
    })
    .unwrap();

    let (old, new) = rename_agent(&path, "worker-a", "worker-b", None).unwrap();
    assert_eq!(old, "worker-a");
    assert_eq!(new, "worker-b");

    let reg = load_registry(&path).unwrap();
    let row = reg
        .find("worker-b")
        .expect("renamed row under the new label");
    assert_eq!(row.aliases, vec!["worker-a".to_string()]);
    assert_eq!(
        row.harness_session_id.as_deref(),
        Some("aaaaaaaa-0000-0000-0000-111111111111")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn rename_agent_refuses_duplicate_and_unknown_and_grammar() {
    let dir = tmpdir("rename-refusals");
    let path = dir.join("registry.json");
    update_registry(&path, |registry| {
        let mut a = sample_entry("worker-a");
        a.harness_session_id = Some("aaaaaaaa-0000-0000-0000-111111111111".into());
        registry.entries.push(a);
        registry.entries.push(sample_entry("worker-b"));
    })
    .unwrap();

    let dup = rename_agent(&path, "worker-a", "worker-b", None).unwrap_err();
    assert!(dup.contains("already names another worker"), "{dup}");
    // Renaming onto a label another row ANSWERS to (its alias) refuses the
    // same way: that label must not be made ambiguous at resolve time.
    rename_agent(&path, "worker-b", "worker-x", None).unwrap();
    let alias_dup = rename_agent(&path, "worker-b", "worker-a", None).unwrap_err();
    assert!(
        alias_dup.contains("already names another worker"),
        "{alias_dup}"
    );
    let unknown = rename_agent(&path, "no-such-row", "worker-c", None).unwrap_err();
    assert!(unknown.contains("no such agent"), "{unknown}");
    let grammar = rename_agent(&path, "worker-a", "bad label!", None).unwrap_err();
    assert!(grammar.contains("1-64 letters"), "{grammar}");
    // Nothing was written by any refused call (the b->x rename above DID
    // land: worker-x holds it, and "worker-b" answers only as that row's
    // old-label alias, never as a name of its own).
    let reg = load_registry(&path).unwrap();
    assert!(reg.find("worker-a").is_some() && reg.find("worker-x").is_some());
    assert_eq!(
        reg.find("worker-b").map(|e| e.name.as_str()),
        Some("worker-x")
    );
    assert!(reg.find("worker-c").is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_registry_refuses_to_wipe_a_corrupt_registry() {
    // The data-loss path Gemini flagged: update_registry reads, mutates,
    // writes. If the read silently defaulted on a corrupt file, the write
    // would publish an (almost) empty registry. It must instead propagate the
    // parse error and leave the file byte-for-byte intact.
    let dir = tmpdir("no-wipe");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("registry.json");
    let corrupt = "{\"schema_version\": 3, \"agents\": [ BROKEN";
    crate::registry_store::seed_raw(&path, corrupt);

    let result = update_registry(&path, |r| r.entries.push(sample_entry("new-A")));
    assert!(result.is_err(), "update over corrupt registry must error");
    assert_eq!(
        crate::registry_store::read_raw(&path),
        corrupt,
        "corrupt registry must be left untouched, not overwritten"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A row-shaped raw registry body, the shape the x-a879 loss took from
/// disk: real fields, no test-side struct construction.
fn seeded_registry_body(rows: &[String]) -> String {
    format!(
        r#"{{"schema_version":{},"agents":[{}]}}"#,
        REGISTRY_SCHEMA_VERSION,
        rows.join(",")
    )
}

fn loss_shaped_row(name: &str, harness: &str, sid: Option<&str>) -> String {
    let sid_json = match sid {
        Some(s) => format!(r#""{s}""#),
        None => "null".to_string(),
    };
    format!(
        r#"{{"name":"{name}","short_id":"{name}-id","harness":"{harness}","harness_session_id":{sid_json},"cwd":"/tmp/{name}","created_at":"2026-09-01T10:00:00Z","status":"live"}}"#
    )
}

#[test]
fn update_registry_accounts_for_a_removed_row() {
    // x-a879: a closure that drops a row leaves a receipt and an event,
    // whatever door the closure belongs to.
    let dir = tmpdir("removal-accounting");
    let home = dir.join("agents");
    std::fs::create_dir_all(&home).unwrap();
    let path = home.join("registry.json");
    std::fs::write(
        &path,
        seeded_registry_body(&[
            loss_shaped_row("kept-a", "claude", Some("a-s")),
            loss_shaped_row("dropped", "claude", Some("dropped-s")),
            loss_shaped_row("kept-b", "codex", Some("b-s")),
        ]),
    )
    .unwrap();

    update_registry(&path, |r| {
        r.entries.retain(|e| e.name != "dropped");
    })
    .unwrap();

    // The write persisted the two survivors.
    let reg = load_registry(&path).unwrap();
    assert_eq!(reg.entries.len(), 2);

    // One per-row event on the agent-lifecycle log, naming the row, the
    // remover, and a staged receipt (the grouped registry_rows_lost line
    // lands after it; the tests for that event assert its own shape).
    let events = crate::events::committed_journal_text(&home.join("events.jsonl"));
    let removals: Vec<serde_json::Value> = events
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .filter(|e| e["type"] == "registry_row_removed")
        .collect();
    assert_eq!(removals.len(), 1, "exactly one removal event: {events}");
    let event: serde_json::Value = removals[0].clone();
    assert_eq!(event["type"], "registry_row_removed");
    assert_eq!(event["source"], "daemon");
    assert_eq!(event["data"]["name"], "dropped");
    assert_eq!(event["data"]["harness"], "claude");
    assert_eq!(event["data"]["harness_session_id"], "dropped-s");
    assert_eq!(event["data"]["receipt_staged"], true);
    assert_eq!(event["data"]["pid"], std::process::id());
    assert!(
        event["data"]["remover"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the remover is named, not blank: {event}"
    );

    // The receipt sits beside the reap-path ones and says who took the row.
    let receipt_path = home.join("reap-receipts").join("claude-dropped-s.json");
    let receipt: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["row_name"], "dropped");
    // The receipt names its writer (the argv command); the event's
    // `remover` answers the separate process-name question it always did.
    assert!(
        receipt["removed_by"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "the removal receipt names its writer: {receipt}"
    );
    assert_eq!(receipt["removal_trigger"], "session");
    assert!(receipt["resume"].as_str().is_some_and(|s| !s.is_empty()));
    // The door that dropped the row also removes the harness side, and its
    // receipt records the attempt as a typed active-surface effect - whatever
    // outcome the probe reached, the attempt is never silent.
    let effects = receipt["effects"].as_array().expect("effects recorded");
    assert_eq!(effects.len(), 1, "one active-surface effect: {receipt}");
    assert_eq!(effects[0]["op"], "active-surface");
    assert!(
        effects[0]["outcome"].is_string(),
        "typed outcome recorded: {receipt}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn load_registry_reads_a_newer_schema_forward() {
    // This assertion was inverted deliberately. The old contract (Codex P2,
    // ab-a171ceb2) refused a future version so a stale reader could not
    // silently drop a field. Refusing turned out to be the worse failure:
    // registry.json is shared by every agent on the machine, so one process
    // ahead of the deployment took the whole fleet's reads down at once.
    // Dropping a field is made safe by refusing to WRITE and by announcing
    // the degrade, not by refusing to look.
    let dir = tmpdir("version-guard");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("registry.json");
    let future = REGISTRY_SCHEMA_VERSION + 1;
    std::fs::write(
        &path,
        format!(r#"{{"schema_version":{future},"agents":[]}}"#),
    )
    .unwrap();
    assert!(
        load_registry(&path).is_ok(),
        "a newer writer must not brick this reader"
    );
    crate::registry_store::seed_raw(&path, r#"{"schema_version":1,"agents":[]}"#);
    assert!(
        load_registry(&path).is_ok(),
        "v1 must still read (back-compat)"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn update_registry_respects_writer_floor_and_preserves_unknown_fields() {
    // An additive future schema is writable when this build meets its floor.
    let dir = tmpdir("version-write-guard");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("registry.json");
    let newer_version = REGISTRY_SCHEMA_VERSION + 1;
    let additive = format!(
        r#"{{"schema_version":{newer_version},"min_writer_version":{},"writer_rev":"writer-ahead","future_top":"kept","agents":[{{"name":"worker","cwd":"/x","log_path":"/l","harness":"claude","status":"live","created_at":"2026-01-01T00:00:00Z","future_row":"kept"}}]}}"#,
        REGISTRY_SCHEMA_VERSION
    );
    crate::registry_store::seed_raw(&path, additive);

    assert!(
        update_registry(&path, |reg| reg.entries[0].status = AgentStatus::Idle).is_ok(),
        "an additive schema ahead of this writer's floor must remain writable"
    );
    let written: serde_json::Value =
        serde_json::from_slice(&crate::registry_store::read_raw(&path).into_bytes()).unwrap();
    assert_eq!(written["schema_version"], newer_version);
    assert_eq!(written["min_writer_version"], REGISTRY_SCHEMA_VERSION);
    assert_eq!(written["writer_rev"], env!("FNO_AGENTS_GIT_REV"));
    assert_eq!(written["future_top"], "kept");
    assert_eq!(written["agents"][0]["future_row"], "kept");
    assert_eq!(written["agents"][0]["status"], "idle");

    let breaking = format!(
        r#"{{"schema_version":{},"min_writer_version":{},"writer_rev":"future-writer","agents":[]}}"#,
        newer_version + 1,
        REGISTRY_SCHEMA_VERSION + 1
    );
    crate::registry_store::seed_raw(&path, &breaking);
    let error = update_registry(&path, |_| ()).unwrap_err();
    match &error {
        StateError::WriterTooOld {
            min_writer,
            understood,
            writer_rev,
            reader_rev,
            ..
        } => {
            assert_eq!(*min_writer, REGISTRY_SCHEMA_VERSION + 1);
            assert_eq!(*understood, REGISTRY_SCHEMA_VERSION);
            assert_eq!(writer_rev.as_deref(), Some("future-writer"));
            assert_eq!(reader_rev, env!("FNO_AGENTS_GIT_REV"));
        }
        other => panic!("expected WriterTooOld, got {other:?}"),
    }
    assert!(error.to_string().contains("fno doctor update"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&crate::registry_store::read_raw(&path)).unwrap(),
        serde_json::from_str::<serde_json::Value>(&breaking).unwrap()
    );

    let incomplete = format!(
        r#"{{"schema_version":{newer_version},"min_writer_version":{},"writer_rev":"writer-ahead","agents":[{{"name":"future","cwd":"/x","log_path":"/l","harness":"claude","status":"hibernating","created_at":"2026-01-01T00:00:00Z"}},{{"name":"readable","cwd":"/x","log_path":"/l","harness":"claude","status":"live","created_at":"2026-01-01T00:00:00Z"}}]}}"#,
        REGISTRY_SCHEMA_VERSION
    );
    crate::registry_store::seed_raw(&path, &incomplete);
    // The table mints an fno_id per row on seed; the refusal must leave the
    // stored document exactly as seeded.
    let seeded = crate::registry_store::read_raw(&path);
    assert!(matches!(
        update_registry(&path, |_| ()),
        Err(StateError::WriterTooOld { .. })
    ));
    assert_eq!(crate::registry_store::read_raw(&path), seeded);
    std::fs::remove_dir_all(&dir).ok();
}

/// A row with none of the three legs: no pid identity, no legacy provider
/// (so `backfill_harness_aliases` cannot promote one), no log_path, no
/// harness. Unlike `sample_entry`, which carries a `legacy_provider` that
/// backfill resolves into a real `harness`/`harness_session_id` pair
/// before the write-choke-point guard ever runs.
fn handleless_entry(name: &str) -> RegistryEntry {
    let mut e = sample_entry(name);
    e.legacy_provider = String::new();
    e.codex_session_id = None;
    e.session_id = None;
    e.pid = None;
    e.pid_start_time = None;
    e.log_path = None;
    e.harness = None;
    e.harness_session_id = None;
    e
}

// ── x-665d: refuse a source-ahead bump of the SHARED registry ───────────

#[test]
fn update_registry_refuses_a_source_ahead_bump_of_the_shared_registry() {
    // AC8-CON: `registry.json` has writers in two languages, so a guard on
    // one leaves the daemon, mux, and every client command still able to
    // poison the file. This test binary genuinely runs from
    // `<checkout>/target/...`, so it IS the source-run case; pointing
    // FNO_AGENTS_HOME at a tmp dir outside the checkout makes that dir the
    // process-global registry the guard protects.
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let home = tmpdir("source-ahead-shared");
    let saved = std::env::var_os("FNO_AGENTS_HOME");
    std::env::set_var("FNO_AGENTS_HOME", &home);
    let path = crate::paths::AgentsHome::from_env().registry_json();
    let older = REGISTRY_SCHEMA_VERSION - 1;
    let body = format!(r#"{{"schema_version":{older},"agents":[]}}"#);
    crate::registry_store::seed_raw(&path, &body);

    let result = update_registry(&path, |reg| reg.entries.clear());

    match saved {
        Some(v) => std::env::set_var("FNO_AGENTS_HOME", v),
        None => std::env::remove_var("FNO_AGENTS_HOME"),
    }
    match result {
        Err(StateError::SourceAheadSchemaBump { found, current, .. }) => {
            assert_eq!(found, older);
            assert_eq!(current, REGISTRY_SCHEMA_VERSION);
        }
        other => panic!("expected SourceAheadSchemaBump, got {other:?}"),
    }
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&crate::registry_store::read_raw(&path)).unwrap(),
        serde_json::from_str::<serde_json::Value>(&body).unwrap(),
        "the refused write must leave the document unchanged"
    );
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn update_registry_refuses_a_new_handleless_row() {
    let dir = tmpdir("refuse-handleless");
    let path = dir.join("registry.json");
    // Seed with one legitimate row so the "unchanged on refusal" assertion
    // has real content to check.
    update_registry(&path, |r| r.entries.push(sample_entry("seed"))).unwrap();
    let before = crate::registry_store::read_raw(&path);

    let result = update_registry(&path, |r| r.entries.push(handleless_entry("ghost")));
    match result {
        Err(StateError::InvariantViolation(msg)) => {
            assert!(msg.contains("ghost"), "error must name the row: {msg}");
            assert!(msg.contains("resolvable handle"));
        }
        other => panic!("expected InvariantViolation, got {other:?}"),
    }
    assert_eq!(
        crate::registry_store::read_raw(&path),
        before,
        "a refused write must leave the registry unchanged"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn pty_state_wire_shape_is_flat_and_stable() {
    // The Option<DriveWindow> in-memory shape must still serialize to the
    // flat state.json schema (Wave 7 cross-language parity).
    let no_drive = PtyState {
        active: true,
        drive: None,
    };
    assert_eq!(
        serde_json::to_value(&no_drive).unwrap(),
        serde_json::json!({"active": true, "drive_active": false})
    );

    let with_drive = PtyState {
        active: true,
        drive: Some(DriveWindow {
            session_id: Some("d-1".into()),
            mode: Some("interactive".into()),
            last_heartbeat_at_monotonic_ns: Some(99),
        }),
    };
    assert_eq!(
        serde_json::to_value(&with_drive).unwrap(),
        serde_json::json!({
            "active": true,
            "drive_active": true,
            "drive_session_id": "d-1",
            "drive_mode": "interactive",
            "last_heartbeat_at_monotonic_ns": 99
        })
    );
    // Roundtrips back to the same typed value.
    let back: PtyState =
        serde_json::from_value(serde_json::to_value(&with_drive).unwrap()).unwrap();
    assert_eq!(back, with_drive);
}

// (x-4c87 raw-versus-decoded row count family) moved verbatim into its own module: this file is over the
// shrink-only line, and test motion is the sanctioned shrink.
#[path = "x4c87_row_counts.rs"]
mod row_count_tests;

#[path = "x43ce_provenance_repair.rs"]
mod provenance_repair_tests;

#[path = "registry_lock_queue.rs"]
mod registry_lock_queue_tests;

/// The equal-version trap (see `registry_schema.toml`): PR 2090 minted v33
/// and PR 2091 then added
/// `spawn_id`/`spawn_provenance` AT v33 without a bump, so an fno built
/// between the two merges called itself v33, read the new rows as its own
/// version, and Python's strict reader refused the whole registry. This test
/// pins the struct's serde field set to `registry_schema.toml`'s `fields`
/// array. A complete struct literal catches newly added fields at compile time;
/// its serialized keys catch wire drift, while read-only legacy aliases are
/// accounted for explicitly. The schema-bump guard makes field changes require
/// a version bump.
#[test]
fn registry_schema_fields() {
    let raw = include_str!("../../registry_schema.toml");
    let parsed: toml::Value = toml::from_str(raw).expect("registry_schema.toml must parse as TOML");
    let toml_names: Vec<String> = parsed
        .get("fields")
        .and_then(|f| f.as_array())
        .expect("registry_schema.toml must carry a `fields` array")
        .iter()
        .map(|v| {
            v.as_str()
                .expect("fields entries must be strings")
                .to_string()
        })
        .collect();

    // This exhaustive literal makes a newly added named field a compile error
    // here. Populating skipped optionals makes serialization expose the full
    // wire-key inventory; three legacy aliases are deliberately read-only.
    let entry = RegistryEntry {
        name: "inventory".into(),
        aliases: vec!["alias".into()],
        short_id: "short".into(),
        legacy_provider: "legacy".into(),
        provider: Some("value".into()),
        model: Some("value".into()),
        model_basis: Some("value".into()),
        effort: Some("value".into()),
        liveness: Some("value".into()),
        liveness_measured_at: Some("value".into()),
        last_report_at: Some("value".into()),
        context_used_pct: Some(1),
        context_used_tokens: Some(1),
        context_window_tokens: Some(1),
        tool_calls: Some(1),
        tool_errors: Some(1),
        context_measured_at: Some("value".into()),
        mail_unread: Some(1),
        harness_title: Some("value".into()),
        cwd: "/tmp".into(),
        project_root: "/tmp".into(),
        session_id: Some("value".into()),
        claude_session_uuid: Some("legacy".into()),
        harness: Some("value".into()),
        harness_session_id: Some("value".into()),
        predecessor_session_ids: vec!["value".into()],
        forked_from_session_id: Some("value".into()),
        launch_account: Some("value".into()),
        launch_account_source: Some("value".into()),
        related_session_id: Some("value".into()),
        transcript_path: Some("value".into()),
        start_source: Some("value".into()),
        node: Some("value".into()),
        node_basis: Some("value".into()),
        node_reason: Some("value".into()),
        pending_session_row: Some(serde_json::json!({})),
        requested_model: Some("value".into()),
        requested_provider: Some("value".into()),
        requested_effort: Some("value".into()),
        spawn_payload: Some("value".into()),
        harness_args: vec!["value".into()],
        route_provider_id: Some("value".into()),
        model_name: Some("value".into()),
        account_record_id: Some("value".into()),
        messaging_socket_path: Some("value".into()),
        codex_session_id: Some("value".into()),
        gemini_session_id: Some("value".into()),
        mcp_channel_id: Some("value".into()),
        host_mode: Some("value".into()),
        cc_session_id: Some("value".into()),
        status: AgentStatus::Live,
        last_message_at: Some("value".into()),
        created_at: "value".into(),
        pid: Some(1),
        pid_start_time: Some(1),
        keeper_child_pid: Some(1),
        substrate: Some("value".into()),
        log_path: Some("value".into()),
        last_reconciled_at: Some("value".into()),
        inside_leg: Some(InsideLegReport::default()),
        exited_at: Some("value".into()),
        stop: Some(StopRecord {
            by: "test".into(),
            at: "value".into(),
            reason: None,
        }),
        mux: Some(MuxRef {
            session: "value".into(),
            pane_id: 1,
        }),
        screen_state: Some(ScreenStateReport {
            state: "idle".into(),
            rule: "test".into(),
            seq: 1,
            at: "value".into(),
            ttl_ms: None,
            answerable: None,
        }),
        role_level: Some(1),
        role_scope: Some("value".into()),
        role_grantor: Some("value".into()),
        route_settings_path: Some("value".into()),
        fno_id: Some("value".into()),
        delivery_policy: Some("value".into()),
        sandbox_posture: Some("value".into()),
        git_grant: Some("value".into()),
        origin: Some("value".into()),
        spawn_trigger: Some("value".into()),
        spawned_by_session: Some("value".into()),
        spawned_by_harness: Some("value".into()),
        spawned_by_cwd: Some("value".into()),
        lineage_kind: Some("value".into()),
        lineage_reason: Some("value".into()),
        adopted_by_session: Some("value".into()),
        resolved_sandbox: Some("value".into()),
        granted_writable_roots: vec!["value".into()],
        requested_permission_mode: Some("value".into()),
        turn_policy_source: Some("value".into()),
        legacy_claude_short_id: Some("legacy".into()),
        spawn_id: Some("value".into()),
        spawn_provenance: Some(crate::spawn_contract::SpawnProvenance {
            origin: crate::spawn_contract::SpawnOrigin::NonSession {
                source: crate::spawn_contract::NonSessionSource::TestScript {
                    path: "test".into(),
                    run_id: "test".into(),
                },
            },
            owner: crate::spawn_contract::SpawnOwner::TestRun {
                script: "test".into(),
                run_id: "test".into(),
            },
        }),
        extra: Default::default(),
    };
    let serialized =
        serde_json::to_value(&entry).expect("full registry entry fixture must serialize");
    let mut struct_names: Vec<String> = serialized
        .as_object()
        .expect("registry entry serializes as an object")
        .keys()
        .cloned()
        .collect();
    struct_names.sort();

    let read_only = [
        "claude_session_uuid",
        "claude_short_id",
        "codex_session_id",
        "gemini_session_id",
        "legacy_provider",
    ];
    let mut pinned: Vec<String> = toml_names
        .into_iter()
        .filter(|name| !read_only.contains(&name.as_str()))
        .collect();
    pinned.sort();
    assert_eq!(
        struct_names, pinned,
        "serialized RegistryEntry keys must match schema `fields` (minus read-only legacy aliases)"
    );
}

#[test]
fn heal_rewrites_only_a_full_uuid_copy_and_keeps_the_row_identity() {
    let dir = tmpdir("heal-short");
    let path = dir.join("registry.json");
    let uuid = "49a80492-388e-44a3-bd91-017be26bcaa0";
    let mut warden = sample_entry("warden");
    warden.harness = Some("claude".into());
    warden.harness_session_id = Some(uuid.into());
    warden.short_id = uuid.into();
    warden.aliases = vec!["footnote-49a80492".into()];
    warden.role_level = Some(1);
    warden.role_scope = Some("fleet".into());
    let mut spawned = sample_entry("spawned");
    spawned.harness = Some("claude".into());
    spawned.harness_session_id = Some("abcd1234-1111-2222-3333-444444444444".into());
    spawned.short_id = "abcd1234".into();
    let mut independent = sample_entry("independent");
    independent.harness = Some("claude".into());
    independent.harness_session_id = Some("11111111-2222-3333-4444-555555555555".into());
    independent.short_id = "99999999-388e-44a3-bd91-017be26bcaa0".into();
    // The pane row is a legal pure-mux row: the one-live-ref invariant bars a
    // mux row from any short_id, so the heal's population never includes it.
    let pane_uuid = "7c9e6679-7425-40de-944b-e07fc1f90ae7";
    let mut pane = sample_entry("pane");
    pane.harness = Some("claude".into());
    pane.harness_session_id = Some(pane_uuid.into());
    pane.short_id = String::new();
    pane.pid = None;
    pane.session_id = None;
    pane.codex_session_id = None;
    pane.mux = Some(MuxRef {
        session: "work".into(),
        pane_id: 7,
    });
    update_registry(&path, |r| {
        r.entries = vec![warden, spawned, independent, pane];
    })
    .unwrap();

    let healed = heal_full_uuid_short_ids(&path).unwrap();
    assert_eq!(healed, 1, "only the byte-copy bg row heals");
    let reg = load_registry(&path).unwrap();
    let by_name = |n: &str| {
        reg.entries
            .iter()
            .find(|e| e.name == n)
            .unwrap_or_else(|| panic!("row {n} missing"))
    };
    let warden = by_name("warden");
    assert_eq!(warden.short_id, "49a80492");
    assert_eq!(
        warden.harness_session_id.as_deref(),
        Some(uuid),
        "the session id is untouched"
    );
    assert_eq!(warden.name, "warden", "the name survives");
    assert_eq!(warden.aliases, vec!["footnote-49a80492"]);
    assert_eq!(warden.role_level, Some(1), "the team fields survive");
    assert_eq!(warden.role_scope.as_deref(), Some("fleet"));
    assert_eq!(by_name("spawned").short_id, "abcd1234");
    assert_eq!(
        by_name("independent").short_id,
        "99999999-388e-44a3-bd91-017be26bcaa0",
        "an independent transport key is not fno's to rewrite"
    );
    assert_eq!(
        by_name("pane").short_id,
        "",
        "a mux row is outside the heal's population"
    );

    // Idempotent: a second pass finds nothing.
    assert_eq!(heal_full_uuid_short_ids(&path).unwrap(), 0);
    std::fs::remove_dir_all(&dir).ok();
}

// ---------------------------------------------------------------------------
// Rolling registry snapshots (task 1.2)
// ---------------------------------------------------------------------------

#[test]
fn registry_snapshot_pins_last_good_copy_on_collapse() {
    let dir = tmpdir("snapshot-collapse");
    let path = dir.join("registry.json");
    update_registry(&path, |registry| {
        for i in 0..20 {
            let mut e = sample_entry(&format!("w{i}"));
            e.harness_session_id = Some(format!("aaaaaaaa-0000-0000-0000-{i:012}"));
            registry.entries.push(e);
        }
    })
    .unwrap();
    // Materialize the first snapshot of the 20-row file.
    update_registry(&path, |_| {}).unwrap();
    // Another writer cuts the file to 1 row, bypassing update_registry.
    update_registry(&path, |registry| {
        registry.entries.retain(|e| e.name == "w0");
    })
    .unwrap();
    // The next write reads a collapsed file: snapshot at once, and the
    // 20-row copy leaves rotation as a pre-shrink pin.
    update_registry(&path, |_| {}).unwrap();

    let snapshots = dir.join("registry-snapshots");
    let pins: Vec<PathBuf> = std::fs::read_dir(&snapshots)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("pre-shrink.registry.json."))
                .unwrap_or(false)
        })
        .collect();
    assert_eq!(pins.len(), 1, "the collapsed predecessor is pinned");
    let pinned = std::fs::read_to_string(&pins[0]).unwrap();
    assert!(pinned.contains("\"w19\""), "the pin holds 20 rows");
    // Rotation still keeps a live newest copy.
    let has_newest = std::fs::read_dir(&snapshots)
        .unwrap()
        .filter_map(|e| e.ok())
        .any(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("registry.json.")
        });
    assert!(
        has_newest,
        "the newest snapshot dir entry stays in rotation"
    );
    std::fs::remove_dir_all(&dir).ok();
}
