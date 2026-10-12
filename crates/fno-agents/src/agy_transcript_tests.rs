//! The agy source against a fixture store: the summaries row scopes the
//! listing through its workspace URIs and its conversation db file, and the
//! render turns that conversation's typed-input rows into claude-shaped
//! user rows.

use crate::agy_transcript::*;
use crate::intel::selected_harnesses;
use crate::provenance::TranscriptSource;
use std::fs;
use std::path::PathBuf;

/// One conversation ("1111...") under /fixture/agy-a with two typed rows; a
/// summaries row with no db file never lists; one history row for a foreign
/// conversation.
fn fixture_store(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("fno-agy-fixture-{tag}"));
    let _ = fs::remove_dir_all(&root);
    let conv_dir = root.join("conversations");
    fs::create_dir_all(&conv_dir).unwrap();
    let summaries = root.join("conversation_summaries.db");
    let conn = rusqlite::Connection::open(&summaries).unwrap();
    conn.execute_batch(
        "CREATE TABLE conversation_summaries (conversation_id TEXT, workspace_uris TEXT);",
    )
    .unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    conn.execute(
        "INSERT INTO conversation_summaries VALUES (?1, ?2)",
        rusqlite::params![id, r#"["file:///fixture/agy-a"]"#],
    )
    .unwrap();
    // A second summaries row with no db file never lists.
    conn.execute(
        "INSERT INTO conversation_summaries VALUES (?1, ?2)",
        rusqlite::params!["22222222-2222-4222-8222-222222222222", "[]"],
    )
    .unwrap();
    fs::write(conv_dir.join(format!("{id}.db")), b"").unwrap();
    let row = |text: &str, ts: i64, conv: &str| {
        format!(
            r#"{{"display":"{text}","timestamp":{ts},"workspace":"/fixture/agy-a","conversationId":"{conv}"}}"#
        )
    };
    let rows = [
        row("first typed", 1_785_000_000_000, id),
        row("second typed", 1_785_000_001_000, id),
        row(
            "foreign",
            1_785_000_002_000,
            "33333333-3333-4333-8333-333333333333",
        ),
    ];
    fs::write(root.join("history.jsonl"), rows.join("\n") + "\n").unwrap();
    (summaries, root.join("history.jsonl"), conv_dir)
}

/// The plan fixture: the conversation lists once, its two typed rows render
/// as two turns, and a broken store probes as an error naming the path.
#[test]
fn lists_the_conversation_and_renders_its_typed_rows() {
    let (summaries, history, conv_dir) = fixture_store("lists");
    let source = AgySource {
        summaries_db: summaries.clone(),
        history,
        conversations_dir: conv_dir,
        roots: Some(vec!["/fixture/agy-a".into()]),
    };
    assert!(source.probe().is_ok());
    let sessions = source.sessions(0);
    assert_eq!(sessions.len(), 1);
    assert_eq!(
        sessions[0].session_id,
        "11111111-1111-4111-8111-111111111111"
    );
    let raw = source.read(&sessions[0]);
    let turns = source.turns(&raw);
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[0].text, "first typed");
    assert_eq!(turns[turns.len() - 1].text, "second typed");
    let broken = AgySource {
        summaries_db: summaries.with_file_name("missing.db"),
        history: PathBuf::from("/nonexistent/history.jsonl"),
        conversations_dir: PathBuf::from("/nonexistent"),
        roots: None,
    };
    assert!(broken.probe().is_err());
}

/// The accept set grows with the harness list: `all` expands to six.
#[test]
fn the_harness_flag_knows_six_harnesses() {
    assert_eq!(selected_harnesses(&[]).len(), 6);
    assert_eq!(
        selected_harnesses(&["pi".to_string(), "agy".to_string()]),
        vec!["pi", "agy"]
    );
}
