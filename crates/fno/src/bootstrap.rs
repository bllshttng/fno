//! The sibling exec: the front door hands a claimed argv to the `fno-agents`
//! binary installed beside it.
//!
//! The binary install puts the four binaries in one directory, so a missing
//! sibling is a broken install. It refuses with the install remedy and never
//! provisions anything: no Python CLI is left to provision.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn bootstrap_command(program: impl AsRef<OsStr>) -> Command {
    crate::process_admission::bootstrap_command(program)
}

/// The remedy for a missing or broken sibling binary.
const NO_SIBLING_REMEDY: &str = "run fno update, or set FNO_AGENTS_BIN.";

/// Forward the `fno backlog ...` argv to the sibling Rust binary's grouped
/// dispatcher. One exec, stdio inherited, signals and exit codes pass
/// through unchanged.
pub fn forward_backlog(args: &[OsString]) -> ! {
    forward_agents(args, "fno backlog")
}

/// Forward argv to the sibling Rust binary, printing `label` on the exec
/// refusal. One exec, stdio inherited, signals and exit codes pass through
/// unchanged. A missing binary refuses with the install remedy.
pub fn forward_agents(args: &[OsString], label: &str) -> ! {
    let bin = crate::digest_overlay::fno_agents_bin();
    let mut command = bootstrap_command(&bin);
    command.args(args);
    #[cfg(unix)]
    {
        let err = crate::process_admission::bootstrap_exec(&mut command);
        eprintln!(
            "{label}: the sibling Rust binary could not be exec'd: {err}\n       {NO_SIBLING_REMEDY}"
        );
        std::process::exit(2);
    }
    #[cfg(not(unix))]
    match command.status() {
        Ok(status) => std::process::exit(status.code().unwrap_or(1)),
        Err(err) => {
            eprintln!(
                "{label}: the sibling Rust binary could not be run: {err}\n       {NO_SIBLING_REMEDY}"
            );
            std::process::exit(2);
        }
    }
}

/// Locate a usable `uv`: prefer one on PATH, else the well-known install dirs
/// Astral's installer uses. Returns the command to invoke (`uv` when on PATH,
/// otherwise an absolute path). The wheel-to-binary move reads it to retire
/// an old uv tool install.
pub(crate) fn find_uv() -> Option<PathBuf> {
    let mut command = bootstrap_command("uv");
    command
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if crate::process_admission::bootstrap_status(&mut command)
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return Some(PathBuf::from("uv"));
    }
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())?;
    [".local/bin/uv", ".cargo/bin/uv"]
        .iter()
        .map(|rel| home.join(rel))
        .find(|cand| is_executable(cand))
}

fn is_executable(p: &Path) -> bool {
    fs::metadata(p)
        .map(|m| m.is_file() && (m.permissions().mode() & 0o111 != 0))
        .unwrap_or(false)
}
