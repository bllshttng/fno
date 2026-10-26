//! A claude row that lost its cached short id after a mux restart still
//! reaches Drive: its job id derives from the session uuid, unless a roster
//! worker answers the row and holds the real job id.
use super::*;
use crate::proto::Reach;

fn reg(rows: &str) -> String {
    format!(r#"{{"schema_version": 6, "agents": [{rows}]}}"#)
}

#[test]
fn a_claude_row_without_short_id_derives_its_job_from_the_session_uuid() {
    let raw = reg(
        r#"{"name":"lost","cwd":"/w","status":"exited","harness":"claude",
            "harness_session_id":"346F5D0D-9840-473c-af21-eaf100ca9ec2"},
           {"name":"odd","cwd":"/w","status":"live","harness":"claude",
            "harness_session_id":"not-a-uuid"},
           {"name":"rostered","cwd":"/w","status":"live","harness":"claude",
            "harness_session_id":"01a0f1ce-0000-4c1e-8a1c-2d3e4f5a6b7c"}"#,
    );
    let roster = [RosterWorker {
        short_id: "9a1b2c3d".into(),
        name: "rostered".into(),
        cwd: "/w".into(),
        account: None,
    }];
    let rows = merge_rows(derive_rows(&raw, 0).unwrap(), &roster);
    let get = |n: &str| rows.iter().find(|r| r.name == n && !r.external).unwrap();
    let lost = get("lost");
    assert_eq!(lost.attach_id.as_deref(), Some("346f5d0d"));
    assert_eq!(
        thread_reach(lost.harness.as_deref(), lost.attach_id.as_deref()),
        Reach::Drive
    );
    assert_eq!(get("odd").attach_id, None, "a non-uuid id derives no job");
    assert_eq!(
        get("rostered").attach_id,
        None,
        "the roster's own job id wins over a derived one"
    );
}
