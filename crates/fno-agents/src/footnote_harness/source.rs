//! Intel's view of footnote sessions: each record file read as session
//! records, the format the claude source reaches through its adapter.

use crate::footnote_harness::transcript::header_cwd;
use crate::provenance::{
    in_roots, record_tool_uses, record_turns, within_window, SessionFile, TranscriptSource, Turn,
};
use serde_json::Value;
use std::path::PathBuf;

pub(crate) struct FootnoteSource {
    pub(crate) sessions_root: PathBuf,
    pub(crate) roots: Option<Vec<PathBuf>>,
}

fn parsed(raw: &str) -> impl Iterator<Item = Value> + '_ {
    raw.lines().filter_map(|l| serde_json::from_str(l).ok())
}

impl TranscriptSource for FootnoteSource {
    fn harness(&self) -> &'static str {
        "footnote"
    }

    fn sessions(&self, days: u64) -> Vec<SessionFile> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut out = Vec::new();
        let projects = std::fs::read_dir(&self.sessions_root)
            .into_iter()
            .flatten()
            .flatten();
        for entry in
            projects.flat_map(|p| std::fs::read_dir(p.path()).into_iter().flatten().flatten())
        {
            let path = entry.path();
            if !crate::footnote_harness::transcript::is_record_file(&path) {
                continue;
            }
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if !within_window(mtime, days, now)
                || !in_roots(header_cwd(&path).as_deref(), self.roots.as_deref())
            {
                continue;
            }
            out.push(SessionFile {
                session_id: path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                path,
                mtime,
                size: meta.len(),
            });
        }
        out.sort_by(|a, b| b.mtime.cmp(&a.mtime));
        out
    }

    fn turns(&self, raw: &str) -> Vec<Turn> {
        record_turns(parsed(raw))
    }

    fn tool_uses(&self, raw: &str) -> usize {
        record_tool_uses(parsed(raw))
    }
}
