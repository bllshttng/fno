use super::super::intent::detect_intent_full;
use super::*;

#[test]
fn detect_intent_tool_result_ignored() {
    // Tool result content with promise-like text should not trigger
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let user_line = serde_json::json!({
        "message": {"role": "user", "content": "<promise>fake</promise>"}
    });
    std::fs::write(&path, serde_json::to_string(&user_line).unwrap() + "\n").unwrap();
    assert_eq!(detect_intent_full(&path), Intent::None);
}

#[test]
fn extract_last_assistant_message_degrades_to_none() {
    // Missing field, malformed JSON, non-string value, and empty/blank
    // strings all degrade to None (transcript fallback), never an error.
    assert_eq!(
        extract_last_assistant_message(r#"{"transcript_path":"/t.jsonl"}"#),
        None
    );
    assert_eq!(extract_last_assistant_message("not json {"), None);
    assert_eq!(
        extract_last_assistant_message(r#"{"last_assistant_message":{"text":"obj"}}"#),
        None
    );
    assert_eq!(
        extract_last_assistant_message(r#"{"last_assistant_message":"   "}"#),
        None
    );
}

#[test]
fn detect_intent_payload_promise_wins_over_stale_transcript() {
    // AC2-HP: at the promise turn's own fire the transcript does NOT yet
    // contain the final message; the payload alone must carry the intent.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": "still working on it"}
    });
    std::fs::write(&path, serde_json::to_string(&line).unwrap() + "\n").unwrap();
    let (intent, source) = detect_intent(Some("<promise>MISSION COMPLETE: done</promise>"), &path);
    assert_eq!(intent, Intent::Promise);
    assert_eq!(source, "payload");
}

#[test]
fn detect_intent_payload_no_tag_is_authoritative() {
    // A tag-less payload is the stopping turn's final text; it must NOT
    // fall through to the transcript (stale-promise containment).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": "<promise>old stale promise</promise>"}
    });
    std::fs::write(&path, serde_json::to_string(&line).unwrap() + "\n").unwrap();
    let (intent, source) = detect_intent(Some("moving on to other work"), &path);
    assert_eq!(intent, Intent::None);
    assert_eq!(source, "payload");
}

#[test]
fn detect_intent_payload_aborted_beats_promise() {
    let (intent, source) = detect_intent(
        Some("<promise>done</promise> <aborted reason=\"kill\">stop</aborted>"),
        Path::new("/nonexistent"),
    );
    assert!(matches!(intent, Intent::Aborted { ref reason } if reason == "kill"));
    assert_eq!(source, "payload");
}

#[test]
fn watching_intent_parses_all_attrs() {
    let (intent, source) = detect_intent(
        Some("waiting <watching reason=\"ci\" pr=\"404\" timeout=\"30m\" task_id=\"task-123\">"),
        Path::new("/nonexistent"),
    );
    assert_eq!(source, "payload");
    assert_eq!(
        intent,
        Intent::Watching {
            reason: "ci".into(),
            pr: Some("404".into()),
            timeout: Some("30m".into()),
            task_id: Some("task-123".into()),
        }
    );
}

#[test]
fn watching_intent_stale_transcript_not_honored() {
    // AC3-EDGE: a watching tag 2 entries back with a tag-less newest entry
    // must NOT resurrect as Watching (payload-or-newest-entry rule).
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let mut content = String::new();
    for text in [
        "<watching reason=\"ci\" pr=\"3\">", // oldest
        "still going",
        "moving on to unrelated work", // newest
    ] {
        let line = serde_json::json!({"message": {"role": "assistant", "content": text}});
        content.push_str(&serde_json::to_string(&line).unwrap());
        content.push('\n');
    }
    std::fs::write(&path, content).unwrap();
    assert_eq!(detect_intent_full(&path), Intent::None);
}

#[test]
fn detect_intent_lookback_bound_holds() {
    // AC2-EDGE ("grill the stale-promise edge"): a promise older than
    // INTENT_LOOKBACK_ENTRIES assistant text entries must NOT ride the
    // window.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("t.jsonl");
    let mut content = String::new();
    let line = serde_json::json!({
        "message": {"role": "assistant", "content": "<promise>stale</promise>"}
    });
    content.push_str(&serde_json::to_string(&line).unwrap());
    content.push('\n');
    for i in 0..INTENT_LOOKBACK_ENTRIES {
        let line = serde_json::json!({
            "message": {"role": "assistant", "content": format!("pivoted work step {i}")}
        });
        content.push_str(&serde_json::to_string(&line).unwrap());
        content.push('\n');
    }
    std::fs::write(&path, content).unwrap();
    assert_eq!(detect_intent_full(&path), Intent::None);
}

#[test]
fn quoted_and_bare_tags_are_prose_not_intent() {
    // Same contract as the distress extractor: a tag quoted in backticks or
    // fenced is a worker talking about the protocol, not emitting it, and a
    // bare attribute-less tag is a mention. The real machine tags always
    // carry attributes.
    use super::super::intent::detect_intent_from_text;
    assert_eq!(
        detect_intent_from_text("not `<watching>` yet"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("see `<promise>fake</promise>` in the docs"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("```text\n<promise>x</promise>\n```"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("bare <watching> mention"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("bare <aborted> mention"),
        Intent::None
    );
    assert_eq!(
        detect_intent_from_text("I still owe the <promise>MISSION COMPLETE summary"),
        Intent::None
    );
    assert!(matches!(
        detect_intent_from_text("<watching reason=\"ci\" timeout=\"15m\">"),
        Intent::Watching { .. }
    ));
    assert!(matches!(
        detect_intent_from_text("<aborted reason=\"operator stop\">"),
        Intent::Aborted { .. }
    ));
    assert_eq!(
        detect_intent_from_text("<promise>MISSION COMPLETE: shipped</promise>"),
        Intent::Promise
    );
}

#[test]
fn declared_help_hold_needs_a_real_distress() {
    // The hold reads the real extractor, not a raw substring: the specimen
    // prose that once mailed the lead an unclassified help holds nothing.
    use crate::loopcheck::declared_recovery_hold;
    assert_eq!(
        declared_recovery_hold("Not `<help>`-worthy: this is the wait class with a live watcher"),
        None
    );
    assert_eq!(
        declared_recovery_hold(
            "stuck <help reason=\"missing dependency\" evidence=\"plan 4.2\">body</help>"
        ),
        Some("declared_help".into())
    );
}
