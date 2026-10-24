use super::*;
use serde_json::json;

/// Two machines: bb16 (this one) holds 17 seats in fno and fnh plus a bare
/// shell row; iMac2D2 holds 9 seats in fno and fno-books.
pub(crate) fn fleet_json() -> Value {
    let session = |name: String, project: &str, status: &str, sid: String| {
        json!({
            "name": name, "session": sid[..8], "harness": "claude", "node": null,
            "status": status, "age_s": 60, "session_id": sid, "model": "claude-opus-4",
            "model_basis": "registry", "started_at": "2026-10-24T10:00:00Z",
            "last_message_at": "2026-10-24T11:00:00Z", "cwd": "/code/footnote",
            "project": project, "role": null, "context_pct": 40
        })
    };
    let mut bb: Vec<Value> = (0..17)
        .map(|i| {
            session(
                format!("mb-{i:02}"),
                if i < 10 { "fno" } else { "fnh" },
                "writing",
                format!("{i:08x}-local-session"),
            )
        })
        .collect();
    bb.push(json!({"name": "zsh-pane", "harness": null, "status": "unknown"}));
    let im: Vec<Value> = (0..9)
        .map(|i| {
            session(
                format!("im-{i}"),
                if i < 5 { "fno" } else { "fno-books" },
                "quiet",
                format!("{:08x}-remote-session", 100 + i),
            )
        })
        .collect();
    json!({"fresh_s": 300, "machines": [
        {"machine_id": "m-imac", "host": "iMac2D2", "local": false, "fresh": true, "age_s": 12,
         "verdict": null, "cpu": null, "mem_used": 0.4, "cap": 12, "takes_work": true, "sessions": im},
        {"machine_id": "m-bb16", "host": "bb16", "local": true, "fresh": true, "age_s": 5,
         "verdict": null, "cpu": null, "mem_used": 0.5, "cap": 20, "takes_work": true, "sessions": bb}
    ]})
}

#[test]
fn fold_groups_seats_into_one_pod_per_project_per_machine() {
    let mut map = fleet_json();
    // A seat with no published project takes its node's graph project.
    map["machines"][0]["sessions"][0]["project"] = Value::Null;
    map["machines"][0]["sessions"][0]["node"] = json!("x-1");
    map["machines"][0]["sessions"][3]["role"] = json!("lead");
    let backlog = crate::backlog_model::fixture(vec![
        json!({"id": "x-1", "status": "in_progress", "title": "Seat work", "project": "fno-books"}),
    ]);
    let fleet = fold(&map, &backlog, &[], 7);
    let hosts: Vec<&str> = fleet.machines.iter().map(|m| m.host.as_str()).collect();
    assert_eq!(hosts, ["bb16", "iMac2D2"], "this machine first");
    assert_eq!(fleet.seats().count(), 26, "the shell pane is not a seat");
    assert!(!fleet.seats().any(|s| s.name == "zsh-pane"));
    let pods = |i: usize| -> Vec<(&str, usize)> {
        fleet.machines[i]
            .pods
            .iter()
            .map(|p| (p.project.as_str(), p.seats.len()))
            .collect()
    };
    assert_eq!(pods(0), [("fnh", 7), ("fno", 10)]);
    assert_eq!(pods(1), [("fno", 4), ("fno-books", 5)]);
    let books = &fleet.machines[1].pods[1];
    assert_eq!(
        fleet.machines[1].pods[0].seats[0].name, "im-3",
        "a seat with a role sorts first"
    );
    let moved = books.seats.iter().find(|s| s.name == "im-0").unwrap();
    let work = moved.work.as_ref().unwrap();
    assert_eq!(work.title.as_deref(), Some("Seat work"));
    assert_eq!(moved.short, "00000064");
}

#[test]
fn a_seat_joins_a_local_row_by_full_session_id_only() {
    let map = fleet_json();
    let row = |sid: &str| AgentRow {
        name: "local".into(),
        harness: Some("claude".into()),
        harness_session_id: Some(sid.into()),
        ..Default::default()
    };
    // The first row matches a bb16 seat in full. The second shares only
    // the iMac seat's short id.
    let agents = [row("00000003-local-session"), row("00000064-another")];
    let fleet = fold(&map, &Default::default(), &agents, 7);
    let joined: Vec<&str> = fleet
        .seats()
        .filter(|s| s.local.is_some())
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(joined, ["mb-03"]);
    let remote = fleet.seats().find(|s| s.short == "00000064").unwrap();
    assert!(remote.local.is_none(), "a short id match never joins");
}
