//! Bounded records for mux failures, shared by the client and server.
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const CAP: u64 = 2 * 1024 * 1024;
const REPEAT_WINDOW: Duration = Duration::from_secs(60);
static LOGS: OnceLock<Mutex<HashMap<PathBuf, Record>>> = OnceLock::new();
static SERVER: OnceLock<PathBuf> = OnceLock::new();

struct Record {
    level: String,
    message: String,
    since: Instant,
    repeats: u64,
}

fn logs() -> &'static Mutex<HashMap<PathBuf, Record>> {
    LOGS.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("mux-log".into())
            .spawn(|| loop {
                std::thread::sleep(Duration::from_secs(1));
                if let Some(logs) = LOGS.get() {
                    if let Ok(mut logs) = logs.lock() {
                        for (path, record) in logs.iter_mut() {
                            if record.since.elapsed() >= REPEAT_WINDOW {
                                record.flush(path);
                            }
                        }
                    }
                }
            });
        Mutex::new(HashMap::new())
    })
}

impl Record {
    fn flush(&mut self, path: &Path) {
        if self.repeats > 0 {
            let _ = write_record(
                path,
                &self.level,
                &format!("repeated {} times", self.repeats),
            );
            self.repeats = 0;
        }
        self.since = Instant::now();
    }
}

/// Server breadcrumbs share the writer and its timestamps, gated off in production.
pub(crate) fn server_breadcrumb(msg: std::fmt::Arguments<'_>) {
    static ON: OnceLock<bool> = OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("FNO_E2E").is_some()) {
        server(
            "info",
            &format!("fno mux e2e[pid {}]: {msg}", std::process::id()),
        );
    }
}

pub fn client_path() -> PathBuf {
    crate::proto::mux_dir().join("client.log")
}

pub fn client(level: &str, message: &str) {
    append(&client_path(), level, message);
}

pub fn server(level: &str, message: &str) {
    if let Some(path) = SERVER.get() {
        append(path, level, message);
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "{} {level} {message}",
            chrono::Utc::now().to_rfc3339()
        );
    }
}

pub fn server_init(path: &Path) -> std::io::Result<()> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let _ = SERVER.set(path.to_owned());
    redirect_stderr(path, &file)?;
    Ok(())
}

fn redirect_stderr(path: &Path, file: &File) -> std::io::Result<()> {
    if SERVER.get().is_some_and(|p| p == path) && unsafe { libc::dup2(file.as_raw_fd(), 2) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub fn append(path: &Path, level: &str, message: &str) {
    let Ok(mut logs) = logs().lock() else { return };
    if let Some(previous) = logs.get_mut(path) {
        if previous.level == level
            && previous.message == message
            && previous.since.elapsed() < REPEAT_WINDOW
        {
            previous.repeats = previous.repeats.saturating_add(1);
            return;
        }
        previous.flush(path);
    }
    if write_record(path, level, message).is_ok() {
        logs.insert(
            path.to_owned(),
            Record {
                level: level.into(),
                message: message.into(),
                since: Instant::now(),
                repeats: 0,
            },
        );
    }
}

pub fn flush(path: &Path) {
    if let Some(mut logs) = LOGS.get().and_then(|logs| logs.lock().ok()) {
        if let Some(record) = logs.get_mut(path) {
            record.flush(path);
        }
    }
}

struct FileLock(File);
impl Drop for FileLock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn write_record(path: &Path, level: &str, message: &str) -> std::io::Result<()> {
    let lock_path = path.with_file_name(format!(
        "{}.lock",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let lock = FileLock(
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(lock_path)?,
    );
    if unsafe { libc::flock(lock.0.as_raw_fd(), libc::LOCK_EX) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // Reopen under the shared lock: another client may have rotated this path.
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    for text in message
        .lines()
        .chain(if message.is_empty() { Some("") } else { None })
    {
        let text: String = text.chars().take(16_384).collect();
        let line = format!("{timestamp} {level} {text}\n");
        if file.metadata()?.len() + line.len() as u64 >= CAP {
            let rotated = path.with_file_name(format!(
                "{}.1",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            std::fs::rename(path, rotated)?;
            file = OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(path)?;
        }
        redirect_stderr(path, &file)?;
        file.write_all(line.as_bytes())?;
    }
    Ok(())
}

pub fn tail(path: &Path, lines: usize) -> String {
    flush(path);
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.metadata().map(|m| m.len()) else {
        return String::new();
    };
    let offset = size.saturating_sub(65_536);
    if file.seek(SeekFrom::Start(offset)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.take(65_536).read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = if offset > 0 {
        text.split_once('\n').map(|(_, rest)| rest).unwrap_or("")
    } else {
        &text
    };
    let mut selected: Vec<_> = text.lines().rev().take(lines).collect();
    selected.reverse();
    selected.join("\n")
}

/// The last `fno mux: shutting down (...)` line in the server's log, read
/// from the log tail so a long-lived log costs a bounded read.
pub(crate) fn last_shutdown_line(log: &std::path::Path) -> Option<String> {
    tail(log, usize::MAX)
        .lines()
        .rev()
        .find(|line| line.contains("fno mux: shutting down"))
        .map(str::to_string)
}

#[macro_export]
macro_rules! mux_log {
    ($level:ident; $($arg:tt)*) => { $crate::mux_log::server(stringify!($level), &format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mux_log_collapses_repeats_and_follows_another_writers_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.log");
        let shutdown = dir.path().join("shutdown.log");
        let mut unicode = format!("{}\nfno mux: shutting down (probe)\n", "é".repeat(40_000));
        while unicode.is_char_boundary(unicode.len() - 65_536) {
            unicode.push('\n');
        }
        std::fs::write(&shutdown, unicode).unwrap();
        assert!(
            last_shutdown_line(&shutdown)
                .is_some_and(|line| line.contains("shutting down (probe)")),
            "shutdown cause survives a UTF-8 tail boundary"
        );
        for _ in 0..10_000 {
            append(&path, "warn", "same failure");
        }
        flush(&path);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "one failure and its repeat count");
        assert!(text.contains("repeated 9999 times"));
        std::fs::rename(&path, dir.path().join("client.log.1")).unwrap();
        std::fs::write(&path, "").unwrap();
        append(&path, "error", "fresh inode");
        assert!(tail(&path, 40).contains("fresh inode"));
        assert!(!std::fs::read_to_string(dir.path().join("client.log.1"))
            .unwrap()
            .contains("fresh inode"));
        append(
            &dir.path().join("missing/client.log"),
            "error",
            "unwritable",
        );
        assert!(tail(&dir.path().join("missing/client.log"), 40).is_empty());
    }

    #[test]
    fn mux_log_bounds_distinct_records_and_timestamps_each_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("server.log");
        for n in 0..3200 {
            append(&path, "error", &format!("{n} {}", "x".repeat(1024)));
        }
        assert!(
            path.metadata().unwrap().len() < 2 * 1024 * 1024,
            "live log must stay below 2 MiB"
        );
        assert!(dir.path().join("server.log.1").exists());
        for line in std::fs::read_to_string(path).unwrap().lines() {
            let (stamp, rest) = line.split_once(' ').unwrap();
            assert!(chrono::DateTime::parse_from_rfc3339(stamp).is_ok());
            assert!(rest.starts_with("error "));
        }
    }
}
