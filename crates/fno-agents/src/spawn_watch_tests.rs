use super::*;
use crate::event_store::{query_events, EventQuery};
use crate::launch_record::Launch;
use crate::paths::AgentsHome;
use crate::state::{self, Lineage, RegistryEntry};
use serde_json::{json, Value};

fn stamp(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap()
        .to_rfc3339()
}

#[test]
fn a_spawn_reports_its_stuck_stage_once_and_its_first_turn_quietly() {
    let _lock = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("FNO_CLAIMS_ROOT");
    let temp = tempfile::tempdir().unwrap();
    std::env::set_var("FNO_CLAIMS_ROOT", temp.path());
    let home = AgentsHome::at(temp.path().join("agents"));
    let cwd = temp.path().join("repo");
    std::fs::create_dir_all(&cwd).unwrap();
    let cwd = cwd.to_string_lossy().into_owned();
    let caller = "lead-1";
    let t0 = crate::claims::now_ms();
    for name in ["quick", "stuck", "refused"] {
        let launch = Launch {
            harness: "claude".into(),
            name: Some(name.into()),
            node: None,
            slot: None,
            provider: None,
            cwd: cwd.clone(),
        };
        crate::launch_record::write_record(home.root(), launch, caller, Some("claude"), &cwd, t0)
            .unwrap();
    }

    // "quick" booted and answered 70s after the call.
    let transcript = temp.path().join("quick.jsonl");
    std::fs::write(
        &transcript,
        format!(
            "{}\n{}\n",
            json!({"type": "user", "timestamp": stamp(t0 + 20_000)}),
            json!({"type": "assistant", "timestamp": stamp(t0 + 70_000)}),
        ),
    )
    .unwrap();
    let mut row = RegistryEntry::new(
        Some("quick-session".into()),
        Lineage::captured((
            Some(caller.into()),
            Some("claude".into()),
            Some(cwd.clone()),
        )),
    );
    row.name = "quick".into();
    row.harness = Some("claude".into());
    row.substrate = Some("thread".into());
    row.created_at = stamp(t0 + 10_000);
    row.transcript_path = Some(transcript.to_string_lossy().into_owned());
    state::update_registry(&home.registry_json(), |r| r.entries.push(row)).unwrap();

    // The caller already holds the refusal: its launch leaves the claim pool.
    settle_refused(&home, Some(caller), "refused", "no_wait");
    assert!(
        !crate::launch_record::records(home.root())
            .iter()
            .any(|(_, r)| r.launch.name.as_deref() == Some("refused")),
        "a refused launch is no longer claimable"
    );

    let mut notices: Vec<String> = Vec::new();
    let mut deliver = |argv: &[String], _: &str| {
        assert_eq!(argv[8], caller);
        notices.push(argv[9].clone());
        (0, "delivered (hosted)".to_string(), String::new())
    };
    run_pass(&home, t0 + 60_000, &mut deliver).unwrap();

    std::fs::write(watch_dir(&home).join("corrupt.json"), "{broken").unwrap();
    assert!(
        run_pass(&home, t0 + DEADLINE_MS, &mut deliver).is_err(),
        "an unreadable watch is reported"
    );
    std::fs::remove_file(watch_dir(&home).join("corrupt.json")).unwrap();
    run_pass(&home, t0 + DEADLINE_MS + 60_000, &mut deliver).unwrap();

    assert_eq!(notices.len(), 1, "{notices:?}");
    assert!(
        notices[0].starts_with("spawn stuck: no first turn 4m"),
        "{}",
        notices[0]
    );
    assert!(notices[0].contains("Stuck at gate"), "{}", notices[0]);

    let turns: Vec<Value> = query_events(
        &home.events_jsonl(),
        &EventQuery {
            include_rejected: true,
            ..Default::default()
        },
    )
    .unwrap_or_default()
    .iter()
    .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
    .filter(|l| l["type"] == "spawn_first_turn")
    .collect();
    assert_eq!(turns.len(), 1, "{turns:?}");
    assert_eq!(turns[0]["data"]["name"], "quick");
    assert_eq!(turns[0]["data"]["latency_ms"], 70_000);
    assert_eq!(turns[0]["data"]["row_ms"], 10_000);
    assert_eq!(turns[0]["data"]["substrate"], "thread");
    assert!(turns[0]["data"].get("machine").is_some());

    match prior {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
}
