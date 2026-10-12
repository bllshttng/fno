//! The pi transcript source: `~/.pi/agent/sessions/<cwd-slug>/*.jsonl`
//! rendered as claude-shaped JSONL, so the shared classifier reads it with
//! no new rule.
//!
//! pi rows carry no marker separating operator-typed text from text the mux
//! typed into the pane, so every turn reads the classifier's residual
//! (`unknown`), never `operator`. The header row's `cwd` decides the
//! project. The store is only ever read.

use crate::opencode_transcript::{push_row, push_user_row, rfc3339_ms};
use crate::provenance::{
    claude_shaped_tool_uses, claude_shaped_turns, first_cwd_row, in_roots, within_window,
    SessionFile, TranscriptSource, Turn,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

/// `~/.pi/agent/sessions`, pi's per-project session dirs.
fn default_sessions_dir() -> PathBuf {
    match std::env::var_os("HOME") {
        Some(home) => PathBuf::from(home)
            .join(".pi")
            .join("agent")
            .join("sessions"),
        None => PathBuf::from("."),
    }
}

/// The pi transcript store: one JSONL file per session under one level of
/// per-cwd dirs. `sessions_dir` is `None` for the default store (tests
/// inject a fixture dir); `roots` scopes the fold to the requested
/// projects, `None` reads every project.
pub(crate) struct PiSource {
    pub(crate) sessions_dir: Option<PathBuf>,
    pub(crate) roots: Option<Vec<PathBuf>>,
}

impl PiSource {
    fn dir(&self) -> PathBuf {
        self.sessions_dir
            .clone()
            .unwrap_or_else(default_sessions_dir)
    }

    /// The header row's session id; the file stem when the header is
    /// missing or malformed.
    fn session_id(path: &Path) -> String {
        let from_header = std::fs::read_to_string(path)
            .ok()
            .and_then(|raw| raw.lines().next().map(str::to_string))
            .and_then(|first| serde_json::from_str::<Value>(&first).ok())
            .and_then(|row| row.get("id").and_then(|v| v.as_str()).map(str::to_string));
        from_header.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string()
        })
    }

    /// The store must exist; an empty dir is fine (no sessions yet).
    pub(crate) fn probe(&self) -> Result<(), String> {
        let dir = self.dir();
        if dir.is_dir() {
            Ok(())
        } else {
            Err(format!("no pi session dir at {}", dir.display()))
        }
    }
}

impl TranscriptSource for PiSource {
    fn harness(&self) -> &'static str {
        "pi"
    }

    fn sessions(&self, days: u64) -> Vec<SessionFile> {
        let now = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = Vec::new();
        let base = self.dir();
        let projects = std::fs::read_dir(&base).into_iter().flatten().flatten();
        for project in projects {
            if !project.path().is_dir() {
                continue;
            }
            let files = std::fs::read_dir(project.path())
                .into_iter()
                .flatten()
                .flatten();
            for file in files {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let Ok(meta) = file.metadata() else {
                    continue;
                };
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if !within_window(mtime, days, now) {
                    continue;
                }
                let cwd = first_cwd_row(&path);
                if !in_roots(cwd.as_deref().map(Path::new), self.roots.as_deref()) {
                    continue;
                }
                let session_id = Self::session_id(&path);
                out.push(SessionFile {
                    path,
                    session_id,
                    mtime,
                    size: meta.len(),
                });
            }
        }
        out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        out
    }

    /// One session's transcript as claude-shaped JSONL, re-read from the
    /// file the listing named.
    fn read(&self, file: &SessionFile) -> String {
        let Ok(raw) = std::fs::read_to_string(&file.path) else {
            return String::new();
        };
        render(&raw)
    }

    fn turns(&self, raw: &str) -> Vec<Turn> {
        claude_shaped_turns(raw)
    }

    fn tool_uses(&self, raw: &str) -> usize {
        claude_shaped_tool_uses(raw)
    }
}

/// pi JSONL rendered as claude-shaped JSONL: each user message becomes one
/// user row (the session format permits `content` as a plain string or as
/// an array of text blocks), each assistant `toolCall` block becomes one
/// assistant row carrying a `tool_use` block so the shared counter sees it.
/// Every other row shape (`session`, `model_change`,
/// `thinking_level_change`, `bashExecution`) renders nothing.
fn render(raw: &str) -> String {
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if row.get("type").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        let Some(msg) = row.get("message") else {
            continue;
        };
        let ts = msg.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
        match msg.get("role").and_then(|v| v.as_str()) {
            Some("user") => {
                let text = match msg.get("content") {
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(Value::Array(parts)) => {
                        let text: Vec<&str> = parts
                            .iter()
                            .filter(|p| p.get("type").and_then(|v| v.as_str()) == Some("text"))
                            .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                            .collect();
                        (!text.is_empty()).then(|| text.join("\n"))
                    }
                    _ => None,
                };
                if let Some(text) = text {
                    push_user_row(&mut out, ts, &text);
                }
            }
            Some("assistant") => {
                let Some(parts) = msg.get("content").and_then(|v| v.as_array()) else {
                    continue;
                };
                for block in parts {
                    if block.get("type").and_then(|v| v.as_str()) != Some("toolCall") {
                        continue;
                    }
                    push_row(
                        &mut out,
                        &serde_json::json!({
                            "type": "assistant", "timestamp": rfc3339_ms(ts),
                            "message": {"role": "assistant", "content": [{"type": "tool_use"}]},
                        }),
                    );
                }
            }
            _ => {}
        }
    }
    out
}
