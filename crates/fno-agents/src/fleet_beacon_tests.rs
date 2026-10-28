use super::*;
use crate::store_remote::test_primary;

fn machine(id: &str, host: &str, beat_at: i64) -> Machine {
    Machine {
        machine_id: id.into(),
        host: host.into(),
        owner: "jason".into(),
        role: "worker".into(),
        beat_at,
        cpu: Some(0.25),
        takes_work: true,
        ..Machine::default()
    }
}

fn session(machine_id: &str, name: &str, id: &str, parent: Option<&str>) -> Session {
    Session {
        machine_id: machine_id.into(),
        name: name.into(),
        session: Some(id.into()),
        parent: parent.map(str::to_string),
        ..Session::default()
    }
}

#[test]
fn fleet_beacon_each_machine_replaces_only_its_own_rows() {
    let primary = test_primary::start();
    let a = machine("mac-a", "mbp", 1000);
    let b = machine("mac-b", "r2d2", 1000);
    publish(
        &primary.remote,
        &a,
        &[session("mac-a", "w1", "aaaa1111", None)],
    )
    .unwrap();
    publish(
        &primary.remote,
        &b,
        &[session("mac-b", "w2", "bbbb2222", None)],
    )
    .unwrap();
    // A's next beat drops w1 and adds w3; B's rows stay as B wrote them.
    let a2 = Machine {
        beat_at: 1060,
        ..a.clone()
    };
    publish(
        &primary.remote,
        &a2,
        &[session("mac-a", "w3", "cccc3333", None)],
    )
    .unwrap();

    let Fleet {
        machines, sessions, ..
    } = read(&primary.remote).unwrap();
    assert_eq!(machines.len(), 2);
    assert_eq!(
        machines
            .iter()
            .find(|m| m.machine_id == "mac-a")
            .unwrap()
            .beat_at,
        1060
    );
    let names: Vec<_> = sessions
        .iter()
        .map(|s| (s.machine_id.as_str(), s.name.as_str()))
        .collect();
    assert_eq!(names, vec![("mac-a", "w3"), ("mac-b", "w2")]);
}

/// A new daemon's seats, turns, display name and lanes round trip with
/// whole-second ages; an old daemon's rows read null; a seat or turn whose
/// session row is gone, or whose beat is stale, drops; and no lane carries
/// its holders' names.
#[test]
fn fleet_beacon_seats_and_turns_join_their_sessions() {
    let primary = test_primary::start();
    let top = json!({"workers": [{
        "name": "sorrel", "handle": "sorrel", "harness": "claude", "status": "writing",
        "status_age_s": 53.4, "role": "L1 fno", "session_id": "aaaa1111-2222",
        "model": "opus", "model_basis": "registry", "started_at": "2026-10-23T09:00:00Z",
        "last_message": "private turn text", "last_message_at": "2026-10-23T10:00:00Z",
        "cwd": "/w/fno", "project": "fno", "context_pct": 41, "progress": "advancing"
    }], "lanes": [
        {"provider": "anthropic", "cap": 4, "count": 6, "full": true, "holders": ["sorrel"]},
        {"provider": null, "unreadable": "gate probe unreadable"}
    ]});
    let mut new = sessions_from_top("mac-a", &top);
    assert_eq!(new[0].age_s, Some(53));
    new[0].last_prompt = Some("ship it".into());
    new[0].user_turns = Some(3);
    let mut a = machine("mac-a", "mbp", 1000);
    a.display_name = Some("MacBook".into());
    a.lanes = lanes_from_top(&top);
    publish(&primary.remote, &a, &new).unwrap();
    // An old daemon writes only its nine session columns.
    publish(
        &primary.remote,
        &machine("mac-b", "r2d2", 1000),
        &[session("mac-b", "w2", "bbbb2222", None)],
    )
    .unwrap();
    // A seat left behind by a session that has since ended, and one a newer
    // daemon wrote on an earlier beat for a name the old daemon reuses.
    primary
        .remote
        .execute(
            "INSERT INTO fleet_seat (machine_id, name, model, beat_at) \
             VALUES ('mac-b', 'gone', 'opus', 1000), ('mac-b', 'w2', 'opus', 940)",
            &[],
        )
        .unwrap();
    primary
        .remote
        .execute(
            "INSERT INTO fleet_turn (machine_id, name, last_prompt, beat_at) \
             VALUES ('mac-b', 'w2', 'stale prompt', 940)",
            &[],
        )
        .unwrap();

    let fleet = read(&primary.remote).unwrap();
    assert_eq!(fleet.seat_error, None);
    let names: Vec<_> = fleet.sessions.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["sorrel", "w2"]);
    let map = map_json(
        &fleet.machines,
        &fleet.sessions,
        "mac-a",
        1010,
        DEFAULT_FRESH_S,
    );
    let seat = &map["machines"][0]["sessions"][0];
    for (key, want) in [
        ("model", json!("opus")),
        ("started_at", json!("2026-10-23T09:00:00Z")),
        ("last_message_at", json!("2026-10-23T10:00:00Z")),
        ("cwd", json!("/w/fno")),
        ("project", json!("fno")),
        ("role", json!("L1 fno")),
        ("age_s", json!(53)),
        ("last_message", json!("private turn text")),
        ("last_prompt", json!("ship it")),
        ("user_turns", json!(3)),
        ("progress", json!("advancing")),
    ] {
        assert_eq!(seat[key], want, "{key}");
    }
    let mbp = &map["machines"][0];
    assert_eq!(mbp["display_name"], json!("MacBook"));
    assert_eq!(
        mbp["lanes"],
        json!([{"provider": "anthropic", "cap": 4, "count": 6, "full": true}])
    );
    let old = &map["machines"][1]["sessions"][0];
    assert!(old["model"].is_null() && old["cwd"].is_null(), "{old}");
    assert!(old["last_prompt"].is_null(), "{old}");
    assert!(map["machines"][1]["display_name"].is_null());
    assert_eq!(map["machines"][1]["lanes"], json!([]));
}

#[test]
fn fleet_beacon_stale_machine_reads_offline_and_child_names_cross_machine_parent() {
    let machines = vec![machine("mac-a", "mbp", 1000), machine("mac-b", "r2d2", 400)];
    let sessions = vec![
        session("mac-a", "lead", "abcdef123456", None),
        session("mac-b", "child", "99998888", Some("abcdef12")),
    ];
    let map = map_json(&machines, &sessions, "mac-a", 1010, DEFAULT_FRESH_S);
    let rows = map["machines"].as_array().unwrap();
    assert_eq!(rows[0]["fresh"], json!(true));
    assert_eq!(rows[0]["local"], json!(true));
    assert_eq!(rows[1]["fresh"], json!(false));
    assert_eq!(rows[1]["age_s"], json!(610));
    assert!(machine_line(&rows[1]).contains("offline 610s"));
    assert_eq!(rows[1]["sessions"][0]["parent_at"], json!("lead@mbp"));
    assert!(render(&map).contains("child - - - - <- lead@mbp"));
}

#[test]
fn fleet_beacon_unreachable_primary_fails_without_panic() {
    let error = publish(&test_primary::dead(), &machine("m", "h", 1), &[]).unwrap_err();
    assert!(crate::store_remote::is_unreachable(&error), "{error}");
}

#[test]
fn fleet_beacon_takes_work_flag_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let home = AgentsHome::at(dir.path());
    assert!(takes_work(&home));
    set_takes_work(&home, false).unwrap();
    assert!(!takes_work(&home));
    set_takes_work(&home, true).unwrap();
    set_takes_work(&home, true).unwrap();
    assert!(takes_work(&home));
}
