//! The Messages tab's model: the thread projection gathered from the hidden
//! `fno-agents mail-threads --format json` command, with org_model's bounded,
//! fail-open shape. A timeout, a non-zero exit or invalid JSON lands in
//! [`MessagesSnapshot::error`], never a paint refusal; the view renders its
//! error line and keeps the last good projection.

use serde_json::Value;
use std::sync::Arc;

/// The last good projection for the process's lifetime: a reopened Messages
/// tab paints it at once instead of an empty screen (item 1). One row, in a
/// mutex; the first gather replaces it. Shared, never deep-copied: a live
/// projection runs to tens of megabytes, and a copy on the input thread
/// stalls keys.
static CACHE: std::sync::LazyLock<std::sync::Mutex<Option<Arc<Value>>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// The cached projection, if a read ever landed in this process.
pub fn cached() -> Option<Arc<Value>> {
    CACHE.lock().ok().and_then(|guard| guard.clone())
}

/// Cache a landed projection for the next open (item 1). Called by the
/// gather task, never by the snapshot's own apply - tests apply fixtures
/// freely without polluting the process-global cache. The replaced value
/// drops here, off the input thread.
pub(crate) fn remember(projection: &Arc<Value>) {
    let old = CACHE
        .lock()
        .ok()
        .and_then(|mut guard| guard.replace(projection.clone()));
    drop(old);
}

/// The fingerprint the stat poll compares: the chats dir's entry count, the
/// total length and the newest mtime.
pub type Fingerprint = (usize, u64, u64);

static NEXT_VERSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// The mail store's change signal (item 1): the chats dir's entry count and
/// its messages.jsonl files' total length and newest mtime, read every 5s by
/// the open board. No change, no re-read. The claude transcripts the You
/// threads read ride in the same terms: the projects dir's `*.jsonl` files
/// fold their count, total length and newest mtime in, so a growing session
/// re-gathers the tab even when no chat moved.
///
/// The dir resolves through the same ladder `fno-agents chats::chats_dir`
/// applies (a `paths.chats` config override, else `<state>/chats`); the
/// mirror lives here because the client crate cannot call the agent crate.
pub fn store_fingerprint() -> Option<Fingerprint> {
    let (mut count, mut total_len, mut newest) = chats_facts(&chats_dir()?)?;
    if let Some(transcripts) = transcript_facts() {
        count += transcripts.0;
        total_len += transcripts.1;
        newest = newest.max(transcripts.2);
    }
    Some((count, total_len, newest))
}

/// The chats store's facts, plus the mail carrier's stamp beside it: the
/// daemon rewrites `../carrier/stamp` when another machine's mail moves, the
/// one change on the primary a stat walk can see.
fn chats_facts(dir: &std::path::Path) -> Option<Fingerprint> {
    let rd = std::fs::read_dir(dir).ok()?;
    let stamp = dir.parent().map(|mail| mail.join("carrier").join("stamp"));
    let mut count = 0usize;
    let mut total_len = 0u64;
    let mut newest = 0u64;
    let files = rd
        .flatten()
        .map(|entry| Some(entry.path().join("messages.jsonl")));
    for path in files.chain([stamp]) {
        count += 1;
        let Some(meta) = path.and_then(|p| p.metadata().ok()) else {
            continue;
        };
        total_len += meta.len();
        newest = newest.max(mtime_secs(&meta));
    }
    Some((count, total_len, newest))
}

fn mtime_secs(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The claude projects dir's transcript facts as `(count, total len, newest
/// mtime)`: a depth-2 walk, one level of project slugs and their `*.jsonl`
/// session files. `None` when the dir does not resolve. The base dir comes
/// from `transcript_tail`'s accessor, the crate's one mirror of the
/// fno-agents ladder.
fn transcript_facts() -> Option<(usize, u64, u64)> {
    let root = crate::transcript_tail::claude_projects_dir();
    let rd = std::fs::read_dir(&root).ok()?;
    let mut count = 0usize;
    let mut total_len = 0u64;
    let mut newest = 0u64;
    for slug in rd.flatten() {
        let Ok(files) = std::fs::read_dir(slug.path()) else {
            continue;
        };
        for f in files.flatten() {
            if f.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(meta) = f.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            count += 1;
            total_len += meta.len();
            newest = newest.max(mtime_secs(&meta));
        }
    }
    Some((count, total_len, newest))
}

/// `chats_dir` as the agent crate resolves it: the `paths.chats` override
/// from `<cwd>/.fno/config.toml` then `~/.fno/config.toml`, else
/// `<state_dir>/chats`.
fn chats_dir() -> Option<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join(".fno/config.toml"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        candidates.push(std::path::PathBuf::from(home).join(".fno/config.toml"));
    }
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = text.parse::<toml::Table>() else {
            continue;
        };
        if let Some(raw) = value
            .get("paths")
            .and_then(|p| p.get("chats"))
            .and_then(toml::Value::as_str)
        {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                let expanded = trimmed
                    .strip_prefix("~/")
                    .map(|rest| {
                        std::env::var_os("HOME")
                            .map(std::path::PathBuf::from)
                            .unwrap_or_default()
                            .join(rest)
                    })
                    .unwrap_or_else(|| std::path::PathBuf::from(trimmed));
                return Some(expanded);
            }
        }
    }
    crate::state_layout::place_dir(&crate::model_catalog::state_dir(), "chats")
}

#[derive(Debug, Clone, Default)]
pub struct MessagesSnapshot {
    pub projection: Option<Arc<Value>>,
    /// Bumps on every landed projection. Rows derived from the projection
    /// are built once per version, never once per paint.
    pub version: u64,
    error: Option<String>,
    error_at: Option<u64>,
}

impl MessagesSnapshot {
    /// A snapshot opening onto the cached last read, if any (item 1).
    pub fn with_cache() -> Self {
        Self {
            projection: cached(),
            version: next_version(),
            error: None,
            error_at: None,
        }
    }
    pub fn apply(&mut self, value: impl Into<Arc<Value>>) {
        let old = self.projection.replace(value.into());
        self.version = next_version();
        self.error = None;
        self.error_at = None;
        // The last reference to a large projection takes tens of ms to free.
        if let Some(old) = old.filter(|old| Arc::strong_count(old) == 1) {
            std::thread::spawn(move || drop(old));
        }
    }
    pub fn fail(&mut self, reason: String, now: u64) {
        self.error = Some(reason);
        self.error_at = Some(now)
    }
    pub fn error_line(&self, now: u64) -> String {
        match (&self.projection, &self.error) {
            (None, Some(reason)) => format!(
                "{reason} (failed {}s ago)",
                now.saturating_sub(self.error_at.unwrap_or(now))
            ),
            (None, None) => "not read".into(),
            (Some(_), Some(reason)) => format!("stale: {reason}"),
            (Some(_), None) => String::new(),
        }
    }
}

fn next_version() -> u64 {
    NEXT_VERSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Gather the projection: `fno-agents mail-threads --format json` with
/// org_model's 30s bound and fail-open shape. mail-threads has no public
/// `fno` command, so this calls the internal door on the paired fno-agents.
pub async fn gather() -> Result<Value, String> {
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new(crate::digest_overlay::fno_agents_bin())
            .args(["mail-threads", "--format", "json"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    match output {
        Err(_) => Err("mail-threads timed out after 30s".into()),
        Ok(Err(error)) => Err(format!("mail-threads could not run: {error}")),
        Ok(Ok(output)) if !output.status.success() => Err(format!(
            "mail-threads exited {}: {}",
            output
                .status
                .code()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "by signal".into()),
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Ok(Ok(output)) => serde_json::from_slice(&output.stdout)
            .map_err(|e| format!("mail-threads invalid JSON: {e}")),
    }
}

#[cfg(test)]
mod tests {
    /// Another machine's mail never touches the local chats files; the
    /// carrier's stamp beside them is what moves the fingerprint.
    #[test]
    fn the_carrier_stamp_moves_the_fingerprint() {
        let root = tempfile::tempdir().unwrap();
        let chats = root.path().join("mail").join("chats");
        std::fs::create_dir_all(chats.join("chat-1")).unwrap();
        std::fs::write(chats.join("chat-1").join("messages.jsonl"), "{}\n").unwrap();
        let before = super::chats_facts(&chats).unwrap();
        let carrier = root.path().join("mail").join("carrier");
        std::fs::create_dir_all(&carrier).unwrap();
        std::fs::write(carrier.join("stamp"), "1 1790000000 7").unwrap();
        assert_ne!(super::chats_facts(&chats).unwrap(), before);
    }
}
