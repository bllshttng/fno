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

    let (machines, sessions) = read(&primary.remote).unwrap();
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
