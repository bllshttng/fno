use super::*;
use serde_json::json;

/// Two machines. bb16 (this one) holds 17 seats and a bare shell row: lead
/// mb-00 spawned mb-01 to mb-09, lead mb-10 spawned mb-11 to mb-16. iMac2D2
/// holds 9 seats: im-0 to im-3 report to mb-00 across machines, im-4 was
/// spawned by the worker mb-01, and im-5 to im-8 reach no lead.
pub(crate) fn fleet_json() -> Value {
    let sid = |i: usize, kind: &str| format!("{i:08x}-{kind}-session");
    let session = |name: String, parent: Option<String>, status: &str, sid: String| {
        json!({
            "name": name, "session": sid[..8], "harness": "claude", "node": null,
            "status": status, "age_s": 60, "session_id": sid, "model": "claude-opus-4",
            "model_basis": "registry", "started_at": "2026-10-24T10:00:00Z",
            "last_message_at": "2026-10-24T11:00:00Z", "cwd": "/code/footnote",
            "project": "fno", "role": null, "context_pct": 40,
            "parent": parent.map(|p| p[..8].to_string())
        })
    };
    let mut bb: Vec<Value> = (0..17)
        .map(|i| {
            let parent = match i {
                0 | 10 => None,
                1..=9 => Some(sid(0, "local")),
                _ => Some(sid(10, "local")),
            };
            session(format!("mb-{i:02}"), parent, "writing", sid(i, "local"))
        })
        .collect();
    bb[0]["role"] = json!("lead");
    bb[10]["role"] = json!("lead");
    bb.push(json!({"name": "zsh-pane", "harness": null, "status": "unknown"}));
    let im: Vec<Value> = (0..9)
        .map(|i| {
            let parent = match i {
                0..=3 => Some(sid(0, "local")),
                4 => Some(sid(1, "local")),
                6 => Some(sid(105, "remote")),
                _ => None,
            };
            session(format!("im-{i}"), parent, "quiet", sid(100 + i, "remote"))
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
fn fold_groups_each_seat_under_the_lead_it_reports_to() {
    let mut map = fleet_json();
    // A seat with no published project takes its node's graph project.
    map["machines"][0]["sessions"][0]["project"] = Value::Null;
    map["machines"][0]["sessions"][0]["node"] = json!("x-1");
    // The turn facts and the lanes ride through; a seat with none reads null.
    for (k, v) in [
        ("last_message", json!("Done.")),
        ("last_prompt", json!("ship it")),
        ("user_turns", json!(4)),
        ("progress", json!("advancing")),
        ("rss_mb", json!(918.0)),
    ] {
        map["machines"][0]["sessions"][0][k] = v;
    }
    map["machines"][0]["display_name"] = json!("iMac");
    map["machines"][0]["lanes"] =
        json!([{"provider": "anthropic", "cap": 4, "count": 6, "full": true}]);
    let backlog = crate::backlog_model::fixture(vec![
        json!({"id": "x-1", "status": "in_progress", "title": "Seat work", "project": "fno-books", "details": "Phase 1."}),
    ]);
    let fleet = fold(&map, &backlog, &[], 7);
    let hosts: Vec<&str> = fleet.machines.iter().map(|m| m.host.as_str()).collect();
    assert_eq!(hosts, ["bb16", "iMac2D2"], "this machine first");
    assert_eq!(fleet.seats().count(), 26, "the shell pane is not a seat");
    assert!(!fleet.seats().any(|s| s.name == "zsh-pane"));
    let leads = |i: usize| -> Vec<(&str, usize)> {
        fleet.machines[i]
            .leads
            .iter()
            .map(|l| (l.name.as_str(), l.seats.len()))
            .collect()
    };
    assert_eq!(leads(0), [("mb-00", 10), ("mb-10", 7)]);
    assert_eq!(
        leads(1),
        [("mb-00", 5), (NO_LEAD, 4)],
        "a lead on another machine names the group; a worker's spawn walks up to its lead"
    );
    assert_eq!(
        fleet.machines[0].leads[0].seats[0].name, "mb-00",
        "the lead sorts first in its own group"
    );
    let moved = fleet.machines[1].leads[0]
        .seats
        .iter()
        .find(|s| s.name == "im-0")
        .unwrap();
    assert_eq!(moved.project.as_deref(), Some("fno-books"));
    assert_eq!(
        moved.work.as_ref().unwrap().title.as_deref(),
        Some("Seat work")
    );
    assert_eq!(moved.short, "00000064");
    assert_eq!(
        moved.work.as_ref().unwrap().details.as_deref(),
        Some("Phase 1.")
    );
    assert_eq!(
        (
            moved.last_message.as_deref(),
            moved.last_prompt.as_deref(),
            moved.user_turns,
            moved.progress.as_deref(),
            moved.rss_mb
        ),
        (
            Some("Done."),
            Some("ship it"),
            Some(4),
            Some("advancing"),
            Some(918.0)
        )
    );
    let bare = fleet.seats().find(|s| s.name == "im-1").unwrap();
    assert_eq!(
        (bare.last_message.as_deref(), bare.user_turns),
        (None, None)
    );
    let imac = &fleet.machines[1];
    assert_eq!(imac.display_name.as_deref(), Some("iMac"));
    let lane = Lane {
        provider: "anthropic".into(),
        cap: Some(4),
        count: Some(6),
        full: true,
    };
    assert_eq!(imac.lanes, [lane]);
    assert!(fleet.machines[0].lanes.is_empty() && fleet.machines[0].display_name.is_none());
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
