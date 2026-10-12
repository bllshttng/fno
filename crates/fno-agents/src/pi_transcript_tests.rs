//! The pi source against a fixture store: the header cwd scopes the
//! listing, and the render turns pi user messages into claude-shaped rows.

use crate::pi_transcript::*;
use crate::provenance::TranscriptSource;
use std::fs;
use std::path::{Path, PathBuf};

fn fixture_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fno-pi-fixture-{tag}"));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_session(base: &Path, project: &str, id: &str, cwd: &str, body: &str) -> PathBuf {
    let dir = base.join(project);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("2026-01-01T00-00-00-000Z_{id}.jsonl"));
    let header = format!(
        r#"{{"type":"session","version":3,"id":"{id}","timestamp":"2026-01-01T00:00:00.000Z","cwd":"{cwd}"}}"#
    );
    fs::write(&path, format!("{header}\n{body}")).unwrap();
    path
}

/// The plan fixture: one session under the requested root carrying two user
/// messages plus non-user rows, and one session under a foreign root.
#[test]
fn lists_by_header_cwd_and_renders_user_turns() {
    let base = fixture_dir("lists");
    let id = "11111111-1111-4111-8111-111111111111";
    write_session(
        &base,
        "proj-a",
        id,
        "/fixture/pi-a",
        concat!(
            r#"{"type":"model_change","id":"m1"}"#,
            "\n",
            r#"{"type":"message","id":"t1","message":{"role":"user","content":[{"type":"text","text":"first typed"}],"timestamp":1785000000000}}"#,
            "\n",
            r#"{"type":"message","id":"t2","message":{"role":"user","content":[{"type":"text","text":"second typed"}],"timestamp":1785000001000}}"#,
            "\n",
            r#"{"type":"message","id":"a1","message":{"role":"assistant","content":[],"stopReason":"end_turn"}}"#,
            "\n",
            r#"{"type":"message","id":"b1","message":{"role":"bashExecution","command":"ls","output":"","timestamp":1785000002000}}"#,
            "\n",
            r#"{"type":"message","id":"t4","message":{"role":"user","content":"string typed","timestamp":1785000003000}}"#,
            "\n",
            r#"{"type":"message","id":"a2","message":{"role":"assistant","content":[{"type":"toolCall","name":"bash"}],"timestamp":1785000004000}}"#,
        ),
    );
    write_session(
        &base,
        "proj-b",
        "22222222-2222-4222-8222-222222222222",
        "/elsewhere/pi-b",
        r#"{"type":"message","id":"t3","message":{"role":"user","content":[{"type":"text","text":"foreign"}],"timestamp":1785000003000}}"#,
    );
    let source = PiSource {
        sessions_dir: Some(base),
        roots: Some(vec!["/fixture/pi-a".into()]),
    };
    assert!(source.probe().is_ok());
    let sessions = source.sessions(0);
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].session_id, id);
    let raw = source.read(&sessions[0]);
    let turns = source.turns(&raw);
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0].text, "first typed");
    assert_eq!(turns[1].text, "second typed");
    assert_eq!(turns[2].text, "string typed");
    assert_eq!(source.tool_uses(&raw), 1);
}
