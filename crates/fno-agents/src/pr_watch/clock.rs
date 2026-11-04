//! The tick's phase clock and its one subprocess door.
//!
//! Every phase body runs under a slice. All slow work is a child process, so
//! the door bounds each child by the phase deadline and kills its process
//! group on a cut; the cut then travels up as [`DoorError::Cut`], releasing
//! locks on the way. A hang inside this process is the wall backstop's job.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

const POLL: Duration = Duration::from_millis(50);
const DRAIN_GRACE: Duration = Duration::from_secs(5);

static PHASE: Mutex<String> = Mutex::new(String::new());
static DEADLINE: Mutex<Option<Instant>> = Mutex::new(None);
static ACTIVE_PGID: AtomicI32 = AtomicI32::new(0);
static PROGRESS: OnceLock<Mutex<std::collections::BTreeMap<String, String>>> = OnceLock::new();

#[derive(Debug, PartialEq)]
pub(crate) enum DoorError {
    /// The phase deadline fired; the caller stops and lets the runner report.
    Cut,
    Failed(String),
}

pub(crate) fn set_phase(name: &str) {
    if let Ok(mut phase) = PHASE.lock() {
        *phase = name.to_string();
    }
}

pub(crate) fn phase() -> String {
    PHASE.lock().map(|p| p.clone()).unwrap_or_default()
}

pub(crate) fn set_deadline(deadline: Option<Instant>) {
    if let Ok(mut slot) = DEADLINE.lock() {
        *slot = deadline;
    }
}

/// Seconds left in the armed phase; `None` when no phase is armed.
pub(crate) fn left() -> Option<f64> {
    let deadline = (*DEADLINE.lock().ok()?)?;
    Some(
        deadline
            .saturating_duration_since(Instant::now())
            .as_secs_f64(),
    )
}

pub(crate) fn check() -> Result<(), DoorError> {
    match left() {
        Some(s) if s <= 0.0 => Err(DoorError::Cut),
        _ => Ok(()),
    }
}

fn progress() -> &'static Mutex<std::collections::BTreeMap<String, String>> {
    PROGRESS.get_or_init(Default::default)
}

/// A phase's partial-work note, so a cut hands back what it did.
pub(crate) fn note(phase: &str, text: String) {
    if let Ok(mut map) = progress().lock() {
        map.insert(phase.to_string(), text);
    }
}

pub(crate) fn take_note(phase: &str) -> Option<String> {
    progress().lock().ok()?.remove(phase)
}

/// Kill the running child's group. The SIGTERM path uses it, so a booted-out
/// tick cannot leave a merge running in a group launchd no longer tracks.
pub(crate) fn kill_active_child() {
    let pgid = ACTIVE_PGID.swap(0, Ordering::SeqCst);
    if pgid > 0 {
        crate::bounded_spawn::killpg(pgid);
    }
}

pub(crate) struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub(crate) struct Call<'a> {
    pub program: &'a std::ffi::OsStr,
    pub args: &'a [String],
    pub stdin: Option<String>,
    pub cwd: Option<&'a Path>,
    pub env: &'a [(&'a str, String)],
    pub timeout_s: f64,
}

/// Run one child under `min(timeout, phase left)`. An overrun of the phase is
/// a cut. An overrun of the call's own bound is a plain failure, spelled
/// `timed out after`, which the merge phase maps to `grant_queue_timeout`.
pub(crate) fn run(call: Call<'_>) -> Result<Output, DoorError> {
    check()?;
    let own = Duration::from_secs_f64(call.timeout_s.max(0.05));
    let started = Instant::now();
    let mut command = Command::new(call.program);
    command
        .args(call.args)
        .stdin(if call.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = call.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in call.env {
        command.env(key, value);
    }
    crate::github_app::inject(&mut command, std::ffi::OsStr::new(call.program));
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let display = call.program.to_string_lossy().to_string();
    let mut child = command
        .spawn()
        .map_err(|e| DoorError::Failed(format!("{display}: {e}")))?;
    ACTIVE_PGID.store(child.id() as i32, Ordering::SeqCst);
    if let (Some(text), Some(mut pipe)) = (call.stdin, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut out = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut out);
            }
            let _ = tx.send(String::from_utf8_lossy(&out).into_owned());
        });
        rx
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|p| Box::new(p) as Box<dyn Read + Send>),
    );
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                ACTIVE_PGID.store(0, Ordering::SeqCst);
                return Err(DoorError::Failed(format!("{display}: {e}")));
            }
        }
        let cut = matches!(left(), Some(s) if s <= 0.0);
        if cut || started.elapsed() >= own {
            crate::bounded_spawn::kill_process_group(&mut child);
            let _ = child.wait();
            ACTIVE_PGID.store(0, Ordering::SeqCst);
            return Err(if cut {
                DoorError::Cut
            } else {
                DoorError::Failed(format!("{display} timed out after {:.0}s", call.timeout_s))
            });
        }
        std::thread::sleep(POLL);
    };
    ACTIVE_PGID.store(0, Ordering::SeqCst);
    use std::os::unix::process::ExitStatusExt;
    Ok(Output {
        code: status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
        // A process the child left behind can hold these pipes open past its
        // exit; no phase cut fires here, so the read needs its own bound.
        stdout: out.recv_timeout(DRAIN_GRACE).unwrap_or_default(),
        stderr: err.recv_timeout(DRAIN_GRACE).unwrap_or_default(),
    })
}

fn self_exe() -> Result<std::path::PathBuf, DoorError> {
    std::env::current_exe().map_err(|e| DoorError::Failed(format!("fno-agents binary: {e}")))
}

/// One round trip with this binary's own door; JSON out, optional JSON in.
pub(crate) fn door(
    args: &[&str],
    stdin: Option<&Value>,
    timeout_s: f64,
) -> Result<Value, DoorError> {
    let exe = self_exe()?;
    let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    let output = run(Call {
        program: exe.as_os_str(),
        args: &argv,
        stdin: stdin.map(Value::to_string),
        cwd: None,
        env: &[],
        timeout_s,
    })?;
    let name = args.join(" ");
    if output.code != 0 {
        let tail: String = output.stderr.trim().chars().take(200).collect();
        return Err(DoorError::Failed(format!(
            "fno-agents {name} exited {}: {tail}",
            output.code
        )));
    }
    serde_json::from_str(&output.stdout)
        .map_err(|e| DoorError::Failed(format!("fno-agents {name} bad output: {e}")))
}

/// Run this binary's own door with raw args, for a command that speaks exit
/// codes rather than one JSON answer.
pub(crate) fn door_raw(args: &[String], timeout_s: f64) -> Result<Output, DoorError> {
    let exe = self_exe()?;
    run(Call {
        program: exe.as_os_str(),
        args,
        stdin: None,
        cwd: None,
        env: &[],
        timeout_s,
    })
}

/// Run a read this process cannot kill on a helper thread, abandoning it at
/// the bound. Only for reads: an abandoned one writes nothing the tick needs.
pub(crate) fn bounded_read<T: Send + 'static>(
    timeout_s: f64,
    read: impl FnOnce() -> T + Send + 'static,
) -> Result<T, DoorError> {
    check()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(read());
    });
    let phase_left = left().unwrap_or(f64::MAX);
    let bound = timeout_s.min(phase_left).max(0.05);
    rx.recv_timeout(Duration::from_secs_f64(bound))
        .map_err(|_| {
            if phase_left <= timeout_s {
                DoorError::Cut
            } else {
                DoorError::Failed(format!("read timed out after {timeout_s:.0}s"))
            }
        })
}
