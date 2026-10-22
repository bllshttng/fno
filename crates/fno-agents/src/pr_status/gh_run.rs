//! The captured external-command runner the quota broker and the internal gh
//! adapter share. Ported from `cli/src/fno/pr/_proc.py` (the pr port 7
//! retirement): run a tool, capture text output, and distinguish "tool not
//! installed" from "tool ran and failed", so the missing-`gh` exit codes
//! (127) survive
//! the port instead of surfacing as spawn panics.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// The captured outcome of an external command (text mode), the
/// `_proc.Result` shape every caller matches on.
#[derive(Debug, Clone)]
pub(crate) struct RunResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RunResult {
    pub(crate) fn ok(&self) -> bool {
        self.returncode == 0
    }
}

/// Why a spawn never produced a result. `ToolMissing` maps to the exit code
/// the bash scripts pinned (127 for merge); `Io` is a spawn failure with the
/// binary present, a different fact a caller may want to name.
#[derive(Debug)]
pub(crate) enum RunError {
    ToolMissing(String),
    Io(String),
}

/// The scrubbed environment the broker hands every gh child: the proxy dirs
/// and shim directories are gone from PATH, so a delegated gh cannot re-enter
/// the broker. `None` inherits.
pub(crate) type EnvOverride = Option<std::collections::BTreeMap<String, String>>;

/// Run `cmd` capturing stdout/stderr as text, under a wall-clock bound.
/// `ToolMissing` when the binary itself is absent (the bash `command -v`
/// guard). A timed-out child is killed with its group and answers 124, the
/// shape `_reconcile.rest_runner` already gave timeouts.
pub(crate) fn run(
    cmd: &[String],
    cwd: Option<&Path>,
    env: EnvOverride,
    timeout: Duration,
) -> Result<RunResult, RunError> {
    let (program, args) = cmd
        .split_first()
        .ok_or_else(|| RunError::Io("run needs at least a program name".to_string()))?;
    let mut command = crate::pr_status::quota::helper_command(std::ffi::OsStr::new(program));
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    if let Some(map) = &env {
        for (key, value) in map {
            command.env(key, value);
        }
        // Keys the map drops must not leak through: the broker's scrubbed
        // PATH is a replacement, not a patch, so an inherited FNO_REAL_GH
        // would send the child back through the proxy it just escaped.
        command.env_remove("FNO_REAL_GH");
        command.env_remove(crate::pr_status::quota::PROXY_DIR_ENV);
        command.env_remove(crate::pr_status::quota::PROXY_DEPTH_ENV);
    }
    command.stdin(Stdio::null());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    use std::os::unix::process::CommandExt;
    let mut child = match command.process_group(0).spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(RunError::ToolMissing(program.to_string()))
        }
        Err(e) => return Err(RunError::Io(e.to_string())),
    };
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_drain = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
                    let _ = child.wait();
                    break Err(timeout);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
                let _ = child.wait();
                return Err(RunError::Io(e.to_string()));
            }
        }
    };
    let stdout = String::from_utf8_lossy(&stdout_drain.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_drain.join().unwrap_or_default()).into_owned();
    match status {
        Ok(status) => Ok(RunResult {
            returncode: status.code().unwrap_or(1),
            stdout,
            stderr,
        }),
        Err(timeout) => Ok(RunResult {
            returncode: 124,
            stdout,
            stderr: format!("timed out after {:.0}s", timeout.as_secs_f64()),
        }),
    }
}

/// The cwd every helper treats as "the process cwd" when a caller passed
/// none, resolved once so a mid-call chdir cannot split a read in half.
pub(crate) fn cwd_or_dot(cwd: Option<&str>) -> PathBuf {
    cwd.map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
