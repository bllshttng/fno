//! A session's user turns, read off its transcript: how many, the text of
//! the newest, and the newest reply's text with its paragraphs. The fleet
//! beacon publishes both each beat, so the read
//! resumes at the byte where the last one stopped instead of rescanning a
//! long transcript. Claude and codex have readers. Other harnesses answer
//! nothing, and their seats read null.
//!
//! A user turn is a person's or a sender's message: claude's `user` event
//! that is not meta and carries text rather than a tool result, and codex's
//! user `response_item`. Text that opens with `<` is harness framing (a
//! command frame, an environment block, injected instructions) and counts
//! as no turn. A reply is the text of an assistant message. A message that
//! only calls tools leaves the last reply standing.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::Value;

/// The newest prompt is cut to this many characters.
pub const PROMPT_CHARS: usize = 600;
/// The newest reply is cut to this many characters.
pub const REPLY_CHARS: usize = 1200;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Turns {
    pub count: usize,
    pub last: Option<String>,
    pub reply: Option<String>,
}

#[derive(Default)]
struct Cursor {
    path: PathBuf,
    offset: u64,
    turns: Turns,
}

static CURSORS: Mutex<Option<HashMap<String, Cursor>>> = Mutex::new(None);

/// The codex rollout index. Building it walks the whole session store, so
/// one build serves every cold cursor; a session missing from it rebuilds
/// at most once per `REINDEX`.
static ROLLOUTS: Mutex<Option<(Instant, HashMap<String, PathBuf>)>> = Mutex::new(None);
const REINDEX: Duration = Duration::from_secs(30);

fn codex_rollout(sid: &str) -> Option<PathBuf> {
    let sid = sid.to_ascii_lowercase();
    let mut guard = ROLLOUTS.lock().unwrap_or_else(|e| e.into_inner());
    let rebuild = guard
        .as_ref()
        .is_none_or(|(at, map)| !map.contains_key(&sid) && at.elapsed() > REINDEX);
    if rebuild {
        *guard = Some((Instant::now(), crate::session_backfill::codex_rollouts()));
    }
    guard.as_ref()?.1.get(&sid).cloned()
}

fn transcript(harness: &str, sid: &str) -> Option<PathBuf> {
    match harness {
        // A worker on an isolated account keeps its transcript under that
        // account's config dir, not the ambient one.
        "claude" => crate::claude_drive::find_transcript(sid).or_else(|| {
            crate::claude_roster::isolated_account_dirs()
                .into_iter()
                .find_map(|(_, dir)| {
                    crate::claude_drive::find_transcript_in(&dir.join("projects"), sid)
                })
        }),
        "codex" => codex_rollout(sid),
        _ => None,
    }
}

/// The text of a user turn, or None when the event is not one.
pub fn turn_text(event: &Value) -> Option<String> {
    let text = match event.get("type").and_then(Value::as_str)? {
        "user" if event.get("isMeta") != Some(&Value::Bool(true)) => {
            let content = event.pointer("/message/content")?;
            match content {
                Value::String(s) => s.clone(),
                Value::Array(items) => {
                    if items
                        .iter()
                        .any(|i| i.get("type").and_then(Value::as_str) == Some("tool_result"))
                    {
                        return None;
                    }
                    items
                        .iter()
                        .filter_map(|i| i.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                _ => return None,
            }
        }
        "response_item" => {
            let payload = event.get("payload")?;
            if payload.get("role").and_then(Value::as_str) != Some("user") {
                return None;
            }
            payload
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|c| c.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.chars().take(PROMPT_CHARS).collect())
}

/// The text of an assistant reply, or None when the event carries none.
pub fn reply_text(event: &Value) -> Option<String> {
    let items = match event.get("type").and_then(Value::as_str)? {
        "assistant" => event.pointer("/message/content")?.as_array()?,
        "response_item"
            if event.pointer("/payload/role").and_then(Value::as_str) == Some("assistant") =>
        {
            event.pointer("/payload/content")?.as_array()?
        }
        _ => return None,
    };
    let text = items
        .iter()
        .filter_map(|i| i.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.chars().take(REPLY_CHARS).collect())
}

/// Fold the complete lines past `cursor.offset` into its turns. A last line
/// with no newline yet is still being written, so the cursor stops before it.
fn advance(cursor: &mut Cursor) -> std::io::Result<()> {
    let mut file = std::fs::File::open(&cursor.path)?;
    if file.metadata()?.len() < cursor.offset {
        // The file was replaced or cut: read it again from the top.
        cursor.offset = 0;
        cursor.turns = Turns::default();
    }
    file.seek(SeekFrom::Start(cursor.offset))?;
    let mut reader = std::io::BufReader::new(file);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = reader.read_until(b'\n', &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            return Ok(());
        }
        cursor.offset += n as u64;
        let Ok(event) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if let Some(text) = turn_text(&event) {
            cursor.turns.count += 1;
            cursor.turns.last = Some(text);
        } else if let Some(text) = reply_text(&event) {
            cursor.turns.reply = Some(text);
        }
    }
}

/// The session's user turns, or None when no transcript is on disk.
pub fn user_turns(harness: &str, sid: &str) -> Option<Turns> {
    let key = format!("{harness}:{sid}");
    let mut guard = CURSORS.lock().unwrap_or_else(|e| e.into_inner());
    let cursors = guard.get_or_insert_with(HashMap::new);
    if !cursors.contains_key(&key) {
        let path = transcript(harness, sid)?;
        cursors.insert(
            key.clone(),
            Cursor {
                path,
                ..Cursor::default()
            },
        );
    }
    let cursor = cursors.get_mut(&key)?;
    if advance(cursor).is_err() {
        // A transcript that moved or vanished resolves again next beat.
        cursors.remove(&key);
        return None;
    }
    Some(cursor.turns.clone())
}

/// Keep only the cursors of the sessions in `live`, so a long-running
/// daemon holds one cursor per live session.
pub fn retain_live(live: &[(&str, &str)]) {
    let keep: HashSet<String> = live.iter().map(|(h, s)| format!("{h}:{s}")).collect();
    if let Some(cursors) = CURSORS.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        cursors.retain(|key, _| keep.contains(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_turn_is_text_a_person_or_sender_wrote() {
        let typed = json!({"type": "user", "message": {"content": "fix the drawer"}});
        let blocks = json!({"type": "user", "message": {"content": [{"type": "text", "text": "Stop hook feedback:\nretry"}]}});
        let tool = json!({"type": "user", "message": {"content": [{"type": "tool_result", "content": "ok"}]}});
        let meta = json!({"type": "user", "isMeta": true, "message": {"content": "preamble"}});
        let frame = json!({"type": "user", "message": {"content": "<command-name>/fno:target</command-name>"}});
        let codex = json!({"type": "response_item", "payload": {"role": "user", "content": [{"text": "ship it"}]}});
        let env = json!({"type": "response_item", "payload": {"role": "user", "content": [{"text": "<environment_context>"}]}});
        assert_eq!(turn_text(&typed).as_deref(), Some("fix the drawer"));
        assert_eq!(
            turn_text(&blocks).as_deref(),
            Some("Stop hook feedback:\nretry")
        );
        assert_eq!(turn_text(&codex).as_deref(), Some("ship it"));
        for none in [tool, meta, frame, env] {
            assert_eq!(turn_text(&none), None);
        }
        let reply = json!({"type": "assistant", "message": {"content": [{"type": "text", "text": "Done."}, {"type": "text", "text": "Next: ship."}]}});
        let tools = json!({"type": "assistant", "message": {"content": [{"type": "tool_use", "name": "Bash"}]}});
        let codex_reply = json!({"type": "response_item", "payload": {"role": "assistant", "content": [{"text": "merged"}]}});
        assert_eq!(reply_text(&reply).as_deref(), Some("Done.\n\nNext: ship."));
        assert_eq!(reply_text(&tools), None);
        assert_eq!(reply_text(&codex_reply).as_deref(), Some("merged"));
    }

    #[test]
    fn the_cursor_resumes_and_skips_a_line_still_being_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let user = |t: &str| format!("{}\n", json!({"type": "user", "message": {"content": t}}));
        std::fs::write(
            &path,
            format!("{}{}{{\"type\":\"user\"", user("one"), user("two")),
        )
        .unwrap();
        let mut cursor = Cursor {
            path: path.clone(),
            ..Cursor::default()
        };
        advance(&mut cursor).unwrap();
        assert_eq!(
            cursor.turns,
            Turns {
                count: 2,
                last: Some("two".into()),
                reply: None
            }
        );
        std::fs::write(
            &path,
            format!("{}{}{}", user("one"), user("two"), user("three")),
        )
        .unwrap();
        advance(&mut cursor).unwrap();
        assert_eq!(
            cursor.turns,
            Turns {
                count: 3,
                last: Some("three".into()),
                reply: None
            }
        );
    }
}
