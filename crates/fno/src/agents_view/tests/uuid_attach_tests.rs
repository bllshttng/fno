//! A claude row that lost its cached short id after a mux restart still
//! reaches Drive: its job id derives from the session uuid.
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
            "harness_session_id":"not-a-uuid"}"#,
    );
    let rows = derive_rows(&raw, 0).unwrap();
    let lost = rows.iter().find(|r| r.name == "lost").unwrap();
    assert_eq!(lost.attach_id.as_deref(), Some("346f5d0d"));
    assert_eq!(
        thread_reach(lost.harness.as_deref(), lost.attach_id.as_deref()),
        Reach::Drive
    );
    let odd = rows.iter().find(|r| r.name == "odd").unwrap();
    assert_eq!(odd.attach_id, None, "a non-uuid id derives no job");
}
