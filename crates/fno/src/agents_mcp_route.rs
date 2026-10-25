//! `fno agents mcp send`: the MCP channel family's verb, answered natively.
//!
//! The family ported whole to crates/fno-agents/src/mcp (the worker's
//! `--mcp-exec` lanes), so the verb keeps its spelling through the front
//! door: `fno agents mcp send` (and the menu spelling `fno agents mail
//! admin mcp send`, which the menu already rewrites here) claims and execs
//! `fno-agents-worker --mcp-exec send` with the tail byte-verbatim. The
//! deprecated root spelling `fno mcp send` prints its move notice and runs
//! the same door. One verb, one PR (d-450caaeb).

use std::ffi::OsString;
use std::process::Command;

/// Classify the verb: `fno agents mcp send ...` (the deprecated root
/// `fno mcp send` already printed its move notice in the Python move table
/// and re-entered as `agents mcp send`). Only `send` claims, one verb per
/// PR (d-450caaeb); an unknown sub-word forwards to Python, which refuses
/// it as unknown like any other dead leaf.
pub fn classify(args: &[OsString]) -> Option<Vec<OsString>> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    match words.as_slice() {
        ["agents", "mcp", "send", ..] => Some(args[2..].to_vec()),
        _ => None,
    }
}

/// Exec the worker's `--mcp-exec send` lane; stdout, stderr and the exit
/// code pass through. A missing worker is a refusal (exit 3), never an
/// empty answer, matching the paths door's posture.
pub fn run(args: &[OsString]) -> i32 {
    let Some(binary) = crate::store_client::worker_binary() else {
        eprintln!(
            "fno agents mcp: refused: the fno-agents-worker binary is unavailable \
             (set FNO_AGENTS_WORKER, or install the worker beside fno)."
        );
        return 3;
    };
    match Command::new(&binary)
        .arg("--mcp-exec")
        .arg("send")
        .args(args)
        .status()
    {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno agents mcp: refused: cannot spawn the mcp worker: {e}");
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
    fn the_front_claims_the_agents_mcp_send_spelling() {
        let rest = classify(&oss(&["agents", "mcp", "send", "--session-id", "s"])).unwrap();
        assert_eq!(
            rest,
            vec![OsString::from("--session-id"), OsString::from("s")]
        );
        assert!(
            classify(&oss(&["mcp", "send"])).is_none(),
            "the root spelling resolves through the move table into agents mcp"
        );
        assert!(classify(&oss(&["agents", "mcp", "status"])).is_none());
        assert!(classify(&oss(&["agents", "mcp"])).is_none());
    }
}
