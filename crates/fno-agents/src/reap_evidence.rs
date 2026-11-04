//! What a row's transcript says about it, read at the two ends the reaper
//! needs. The head names the skill: a `/fno:<skill>` call marks a skill
//! session, and a transcript with none is a brainstorm (law d-defa43c7). The
//! tail names the end: a `<promise>` with no newer user turn marks the work
//! done.

use std::path::{Path, PathBuf};

use crate::session_truth::{injected_record_text, record_parser, tail_records, Signal};

/// How many trailing records the tail read folds.
const TAIL_RECORDS: usize = 40;

/// Did the session run an fno skill? `None` when the store holds no
/// transcript for it, so there is nothing to read.
pub(crate) fn fno_skill(paths: Option<&[PathBuf]>) -> Option<bool> {
    let paths = paths.filter(|p| !p.is_empty())?;
    Some(
        paths
            .iter()
            .any(|p| crate::session_backfill::bg_fno_invocation_in(p)),
    )
}

/// Does the newest transcript end in a promise with no user turn after it?
pub(crate) fn tail_done(paths: Option<&[PathBuf]>, harness: &str) -> bool {
    let Some(parse) = record_parser(harness) else {
        return false;
    };
    let Some(path) = paths.and_then(newest) else {
        return false;
    };
    tail_records(&path, TAIL_RECORDS, parse).is_ok_and(|records| {
        records
            .iter()
            .rev()
            .filter(|r| !(r.role == "user" && injected_record_text(&r.text)))
            .find_map(|r| match r.role.as_str() {
                "user" => Some(false),
                "assistant" if Signal::of(&r.text) == Signal::Promise => Some(true),
                _ => None,
            })
            .unwrap_or(false)
    })
}

fn newest(paths: &[PathBuf]) -> Option<PathBuf> {
    paths.iter().max_by_key(|p| mtime(p)).cloned()
}

fn mtime(p: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript(tag: &str, lines: &[&str]) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fno-reap-evidence-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.jsonl");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();
        path
    }

    const TARGET: &str = r#"{"type":"user","message":{"role":"user","content":"<command-message>fno:target</command-message>\n<command-name>/fno:target</command-name>\n<command-args>x-1</command-args>"}}"#;
    const CHAT: &str = r#"{"type":"user","message":{"role":"user","content":"what if the reaper kept brainstorms?"}}"#;
    const PROMISE: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"<promise>MISSION COMPLETE: shipped</promise>"}]}}"#;
    const PLAIN: &str = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"working on it"}]}}"#;

    #[test]
    fn the_head_names_the_skill_and_the_tail_names_the_end() {
        let done = transcript("done", &[TARGET, PLAIN, PROMISE]);
        let reopened = transcript("reopened", &[TARGET, PROMISE, CHAT]);
        let brainstorm = transcript("brainstorm", &[CHAT, PLAIN]);
        let one = |p: &PathBuf| Some(vec![p.clone()]);

        assert_eq!(fno_skill(one(&done).as_deref()), Some(true));
        assert_eq!(fno_skill(one(&brainstorm).as_deref()), Some(false));
        assert_eq!(fno_skill(Some(&[])), None);
        assert_eq!(fno_skill(None), None);

        assert!(tail_done(one(&done).as_deref(), "claude"));
        // A user turn after the promise reopens the session.
        assert!(!tail_done(one(&reopened).as_deref(), "claude"));
        assert!(!tail_done(one(&brainstorm).as_deref(), "claude"));
        assert!(!tail_done(one(&done).as_deref(), "opencode"));
    }
}
