//! The bus log: where it lives, how its segments are read, and how one line
//! is appended under the sidecar lock. Nothing here names a fleet concept.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

const LOCK_TIMEOUT: Duration = Duration::from_secs(5);

/// The bus log path, the one ladder every bus reader and writer shares:
/// `FNO_BUS_DIR`, then the `config.paths.bus_dir` override, else the table's
/// bus folder under `fno_dir` (`state_layout`, so a root the daemon has not
/// folded yet still answers its legacy spelling). The caller reads the
/// environment and the config and passes the two values in, so this module
/// stays free of the config cascade.
pub fn resolve_log_path(
    fno_dir: &Path,
    bus_dir_env: Option<&str>,
    configured_dir: Option<&Path>,
) -> PathBuf {
    if let Some(dir) = bus_dir_env.filter(|v| !v.is_empty()) {
        return PathBuf::from(dir).join("messages.jsonl");
    }
    if let Some(dir) = configured_dir {
        return dir.join("messages.jsonl");
    }
    crate::state_layout::place_in(fno_dir, "bus", "messages.jsonl")
}

/// The bus DIRECTORY for `log_path`: its parent, or the table's bus folder
/// under `fno_dir` when the path has none.
pub fn dir_of_log(log_path: &Path, fno_dir: &Path) -> PathBuf {
    log_path.parent().map(Path::to_path_buf).unwrap_or_else(|| {
        crate::state_layout::place_dir(fno_dir, "bus").unwrap_or_else(|| fno_dir.join("bus"))
    })
}

/// All retained bus lines oldest -> newest (log.py::_segment_paths_oldest_first
/// order), malformed lines skipped. Readers are lock-free by contract.
pub fn read_bus_segments(live: &Path) -> Vec<Value> {
    read_bus_segments_reported(live)
        .0
        .into_iter()
        .map(|r| r.row)
        .collect()
}

/// One parsed bus row and where it came from, so a validation warning can
/// name the segment and line the Python read leg named.
pub struct PositionedBusRow {
    pub row: Value,
    pub source: String,
    pub line: usize,
}

/// The same walk with one stderr warning per skipped line, the reader
/// contract `bus.log.iter_messages` froze: blank lines skip silently, a line
/// that fails to parse warns, and an unreadable segment warns whole.
pub fn read_bus_segments_reported(live: &Path) -> (Vec<PositionedBusRow>, Vec<String>) {
    let mut paths: Vec<(u64, PathBuf)> = Vec::new();
    if let Some(parent) = live.parent() {
        let prefix = format!(
            "{}.",
            live.file_name().unwrap_or_default().to_string_lossy()
        );
        if let Ok(rd) = std::fs::read_dir(parent) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if let Some(suffix) = name.strip_prefix(&prefix) {
                    if let Ok(n) = suffix.parse::<u64>() {
                        paths.push((n, entry.path()));
                    }
                }
            }
        }
    }
    paths.sort_by(|a, b| b.0.cmp(&a.0)); // oldest (highest N) first
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for p in paths
        .iter()
        .map(|(_, p)| p.as_path())
        .chain(std::iter::once(live))
    {
        let text = match std::fs::read_to_string(p) {
            Ok(text) => text,
            Err(e) => {
                warnings.push(format!("bus log: cannot read segment {}: {e}", p.display()));
                continue;
            }
        };
        for (lineno, line) in text.lines().enumerate() {
            match serde_json::from_str::<Value>(line) {
                Ok(v) if !super::envelope::is_readable(&v) => warnings.push(format!(
                    "bus log: skipping row {}:{} (v={} is newer than supported v={})",
                    p.file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default(),
                    lineno + 1,
                    super::envelope::row_version(&v),
                    super::envelope::ENVELOPE_VERSION
                )),
                Ok(v) => out.push(PositionedBusRow {
                    row: v,
                    source: p
                        .file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default()
                        .to_string(),
                    line: lineno + 1,
                }),
                Err(_) if line.trim().is_empty() => {}
                Err(_) => warnings.push(format!(
                    "bus log: skipping malformed line {}:{} (JSONDecodeError)",
                    p.file_name()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap_or_default(),
                    lineno + 1
                )),
            }
        }
    }
    (out, warnings)
}

/// Exclusive flock on the Python sidecar lockfile (`messages.jsonl + .lock`),
/// so the one-line writer serializes against every Python appender.
struct BusLock {
    /// Held for the lock's lifetime; the flock dies with this handle.
    _file: File,
}

impl BusLock {
    fn acquire(live: &Path) -> Result<BusLock, String> {
        let lock_path = PathBuf::from(format!("{}.lock", live.display()));
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("bus lock dir: {e}"))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| format!("bus lock open {}: {e}", lock_path.display()))?;
        // The Python tests' tuning rides env now that the door owns the
        // wait: a contended lock must time out inside the caller's own
        // subprocess budget (FNO_BUS_LOCK_TIMEOUT_SECS).
        let timeout = lock_timeout();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(BusLock { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if std::time::Instant::now() >= deadline {
                        return Err(format!(
                            "bus lock timeout after {timeout:?} at {}",
                            lock_path.display()
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => return Err(format!("bus lock: {e}")),
            }
        }
    }
}

fn lock_timeout() -> Duration {
    std::env::var("FNO_BUS_LOCK_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0)
        .map(Duration::from_secs_f64)
        .unwrap_or(LOCK_TIMEOUT)
}

/// The bus log's size cap and segment count, the Python appender's env
/// contract (`FNO_BUS_MAX_BYTES` / `FNO_BUS_RETAIN`).
fn bus_max_bytes() -> u64 {
    std::env::var("FNO_BUS_MAX_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v > 0)
        .unwrap_or(5 * 1024 * 1024)
}

fn bus_retain() -> usize {
    std::env::var("FNO_BUS_RETAIN")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v| *v >= 1)
        .unwrap_or(5)
}

/// Shift the rotated segments up one and start a fresh live log, the
/// Python appender's `_rotate_locked` port. Every step is best-effort: a
/// rotation failure must not lose the message, so the write follows anyway.
fn rotate_bus_locked(live: &Path) {
    let name = live
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let segment = |n: usize| live.with_file_name(format!("{name}.{n}"));
    let retain = bus_retain();
    let _ = std::fs::remove_file(segment(retain));
    for n in (1..retain).rev() {
        let _ = std::fs::rename(segment(n), segment(n + 1));
    }
    let _ = std::fs::rename(live, segment(1));
}

/// Append one row under the sidecar flock, with the Python appender's
/// rotation and owner-only mode. The caller owns every policy check (sender
/// guard, test fence, chat-store record) and runs it before this call.
pub fn append_raw(live: &Path, obj: &Value) -> Result<(), String> {
    let mut line = serde_json::to_string(obj).map_err(|e| format!("serialize: {e}"))?;
    line.push('\n');
    let _lock = BusLock::acquire(live)?;
    // A live log at or over the cap rotates before the write; a stat
    // failure must not lose the message.
    if let Ok(meta) = std::fs::metadata(live) {
        if meta.len() >= bus_max_bytes() {
            rotate_bus_locked(live);
        }
    }
    // The log holds message bodies: owner-only on creation and on an
    // existing segment, best-effort, like the Python appender's chmod.
    if live.exists() {
        let _ = std::fs::set_permissions(live, std::fs::Permissions::from_mode(0o600));
    }
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(live)
        .map_err(|e| format!("bus open {}: {e}", live.display()))?;
    f.write_all(line.as_bytes())
        .map_err(|e| format!("bus append: {e}"))?;
    Ok(())
}
