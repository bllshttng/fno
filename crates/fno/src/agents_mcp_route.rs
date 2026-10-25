//! `fno agents mcp send`: the MCP channel family's verb, answered natively.
//!
//! The family ported whole to crates/fno-agents/src/mcp (the worker's
//! `--mcp-exec` lanes), so the verb keeps its spelling through the front
//! door: `fno agents mcp send` (and the menu spelling `fno agents mail
//! admin mcp send`, which the menu already rewrites here) claims and execs
//! `fno-agents-worker --mcp-exec send` with the tail byte-verbatim. The
//! deprecated root spelling `fno mcp send` claims here too, printing its
//! move notice and running the same door; nothing routes through the
//! deleted Python fold. One verb, one PR (d-450caaeb).

use std::ffi::OsString;
use std::process::Command;

/// Classify the verb: `fno agents mcp send ...` or the deprecated root
/// `fno mcp send ...` (the bool is the root spelling; its move notice
/// prints before the same exec). Only `send` claims, one verb per PR
/// (d-450caaeb); an unknown sub-word forwards, which refuses as unknown
/// like any other dead leaf.
pub fn classify(args: &[OsString]) -> Option<(bool, Vec<OsString>)> {
    let words: Vec<&str> = args.iter().map(|a| a.to_str()).collect::<Option<_>>()?;
    match words.as_slice() {
        ["mcp", "send", ..] => Some((true, args[2..].to_vec())),
        ["agents", "mcp", "send", ..] => Some((false, args[3..].to_vec())),
        _ => None,
    }
}

/// Exec the worker's `--mcp-exec send` lane; stdout, stderr and the exit
/// code pass through. The root spelling prints the move notice the Python
/// move table used to print, byte-identical, on stderr before the same
/// exec. A missing worker is a refusal (exit 3), never an empty answer,
/// matching the paths door's posture.
pub fn run(root_alias: bool, args: &[OsString]) -> i32 {
    if root_alias {
        eprintln!("fno mcp is now fno agents mcp");
    }
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
    fn the_front_claims_both_mcp_send_spellings() {
        let (root, rest) = classify(&oss(&["agents", "mcp", "send", "--session-id", "s"])).unwrap();
        assert!(!root);
        assert_eq!(
            rest,
            vec![OsString::from("--session-id"), OsString::from("s")]
        );
        let (root, rest) = classify(&oss(&["mcp", "send", "--session-id", "s"])).unwrap();
        assert!(root, "the root spelling claims here, notice and all");
        assert_eq!(
            rest,
            vec![OsString::from("--session-id"), OsString::from("s")]
        );
        assert!(classify(&oss(&["agents", "mcp", "status"])).is_none());
        assert!(classify(&oss(&["agents", "mcp"])).is_none());
    }
}
