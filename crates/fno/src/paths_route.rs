//! `fno config paths emit-shell`, `shell-stub`, `verify` and `handoff`: the
//! paths commands answered natively.
//!
//! One command per PR (law d-450caaeb): all four landed across four PRs; the
//! Python paths group is gone. The route execs the worker's `--paths-exec`
//! lane (worker_binary resolution like the law door), argv and exit code
//! pass through unchanged.

use std::ffi::OsString;
use std::process::Command;

/// All four paths commands, one per PR (law d-450caaeb): emit-shell,
/// shell-stub, verify, handoff. The Python group is retired whole.
pub const NATIVE_PATHS_COMMANDS: &[&str] = &["emit-shell", "shell-stub", "verify", "handoff"];

/// Classify the paths command for the front door: `fno config paths <v> ...`
/// and its deprecated top-level spelling `fno paths <v> ...` both route
/// here when the command is served natively.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let w0 = args.first().and_then(|a| a.to_str());
    let start = match w0 {
        Some("config") if args.get(1).and_then(|a| a.to_str()) == Some("paths") => 2,
        Some("paths") => 1,
        _ => return None,
    };
    let command = args.get(start).and_then(|a| a.to_str())?;
    if !NATIVE_PATHS_COMMANDS.contains(&command) {
        return None;
    }
    Some(args[start..].to_vec())
}

/// Exec the worker's `--paths-exec` lane; stdout, stderr and the exit code
/// pass through. A missing worker is a refusal (exit 3), never an empty
/// answer, matching the law door's posture.
pub fn run(args: &[OsString]) -> i32 {
    let Some(binary) = crate::store_client::worker_binary() else {
        eprintln!("fno config paths: refused: the fno-agents-worker binary is unavailable (set FNO_AGENTS_WORKER, or install the worker beside fno).");
        return 3;
    };
    match Command::new(&binary)
        .arg("--paths-exec")
        .args(args)
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno config paths: refused: cannot spawn the paths worker: {e}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn oss(pieces: &[&str]) -> Vec<OsString> {
        pieces.iter().map(OsString::from).collect()
    }
    #[test]
    fn classify_leaves_other_commands_to_python() {
        for command in ["bogus"] {
            assert!(classify(&oss(&["config", "paths", command])).is_none());
            assert!(classify(&oss(&["paths", command])).is_none());
        }
        assert!(classify(&oss(&["config", "setup", "auto-wire"])).is_none());
        assert!(classify(&oss(&["paths"])).is_none());
        assert!(classify(&oss(&["config", "paths"])).is_none());
    }
}
