//! The worker registry append op, ported from the
//! `cli/src/fno/runtime/registry.py`'s `register_worker` (the module's other
//! two functions had no production caller and the deletion retires them).
//! Characterized by the `register_worker_*` goldens.

use super::sorted_json;
use chrono::Utc;
use serde_json::{json, Value};
use std::os::fd::AsRawFd;
use std::path::PathBuf;

/// Append one started row to the workers JSONL under an exclusive lock.
pub fn register_worker(
    worker_id: &str,
    task: &str,
    campaign: &str,
    workers_file: Option<PathBuf>,
) -> std::io::Result<Value> {
    let workers_file = workers_file.unwrap_or_else(default_workers_file);
    if let Some(dir) = workers_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let now = Utc::now();
    let now_text = now.to_rfc3339_opts(chrono::SecondsFormat::Micros, false);
    let entry = json!({
        "worker_id": worker_id,
        "task": task,
        "campaign": campaign,
        "session_id": "",
        "status": "started",
        "started_at": now_text,
        "last_heartbeat": now_text,
    });
    let lock_path = lock_path_for(&workers_file);
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    let rc = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
    }
    let append_result = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&workers_file)
        .and_then(|mut f| {
            use std::io::Write;
            f.write_all(sorted_json(&entry).as_bytes())
                .and_then(|_| f.write_all(b"\n"))
        });
    let _ = libc::flock(lock.as_raw_fd(), libc::LOCK_UN);
    drop(lock);
    append_result.map(|_| entry)
}

fn lock_path_for(workers_file: &std::path::Path) -> PathBuf {
    let mut s = workers_file.as_os_str().to_os_string();
    s.push(".lock");
    PathBuf::from(s)
}

fn default_workers_file() -> PathBuf {
    // `<repo>/.fno/workers.jsonl`, anchored to the git root, never the cwd.
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = super::git_out(&cwd, &["rev-parse", "--show-toplevel"])
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
        .unwrap_or(cwd);
    root.join(".fno").join("workers.jsonl")
}

/// The `workspace-register-worker` op: `--id ID [--task T] [--campaign C]
/// [--workers-file F]`, printing the written entry.
pub fn run_client(args: &[String]) -> i32 {
    let mut id: Option<String> = None;
    let mut task = String::new();
    let mut campaign = String::new();
    let mut workers_file: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--id" if i + 1 < args.len() => {
                id = Some(args[i + 1].clone());
                i += 2;
            }
            "--task" if i + 1 < args.len() => {
                task = args[i + 1].clone();
                i += 2;
            }
            "--campaign" if i + 1 < args.len() => {
                campaign = args[i + 1].clone();
                i += 2;
            }
            "--workers-file" if i + 1 < args.len() => {
                workers_file = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--json" | "-J" => i += 1,
            other => {
                eprintln!("fno-agents workspace-register-worker: unexpected argument {other}");
                return 2;
            }
        }
    }
    let Some(worker_id) = id else {
        eprintln!("fno-agents workspace-register-worker: --id is required");
        return 2;
    };
    match register_worker(&worker_id, &task, &campaign, workers_file) {
        Ok(entry) => {
            println!("{}", sorted_json(&entry));
            0
        }
        Err(e) => {
            eprintln!("fno-agents workspace-register-worker: {e}");
            1
        }
    }
}
