//! `fno-agents pr-merge-result-check <pr>`: the standalone merge-result
//! verb, over the same walk the merge gate probes in process
//! ([`crate::gate_probes::merge_result_verdict`]). Ported from
//! `cli/src/fno/pr/_merge_result.py`, which this verb replaces.

use crate::authorized_merge::PrFacts;
use crate::gate_probes::{merge_result_verdict, run_probe};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

/// One `gh pr view` read fills the base/head the probe needs. Output is the
/// Python verb's contract: stdout `merge-result: ok - <reason>` at 0, stderr
/// `REFUSED` plus the rebase remedy at 3, `unknown - <reason>` at 4. Dead
/// probes answer unknown, never a crash: the Python `_probe` contract.
pub fn run_merge_result_verb(argv: &[String]) -> i32 {
    const USAGE: &str = "usage: fno-agents pr-merge-result-check <pr-number>";
    let number = match argv.first().map(String::as_str).map(str::parse::<u64>) {
        Some(Ok(number)) => number,
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let run = |args: &[&str], dir: &Path| {
        let mut cmd = Command::new(args[0]);
        cmd.args(&args[1..]).current_dir(dir);
        run_probe(cmd)
    };
    let payload = run(
        &[
            "gh",
            "pr",
            "view",
            &number.to_string(),
            "--json",
            "baseRefName,headRefOid",
        ],
        &cwd,
    )
    .filter(|r| r.ok)
    .and_then(|r| serde_json::from_str::<Value>(&r.stdout).ok());
    let (base_ref, head_sha) = match payload {
        Some(v) => (
            v.get("baseRefName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            v.get("headRefOid")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        ),
        None => (String::new(), String::new()),
    };
    if base_ref.is_empty() || head_sha.is_empty() {
        eprintln!(
            "merge-result: unknown - could not read base/head of PR #{number} (gh pr view failed)"
        );
        return 4;
    }
    let facts = PrFacts {
        number,
        base_ref,
        head_sha,
        ..Default::default()
    };
    let (verdict, reason) = merge_result_verdict(&cwd, &facts, &run);
    match verdict.as_str() {
        "ok" => {
            println!("merge-result: ok - {reason}");
            0
        }
        "red" => {
            eprintln!("merge-result: REFUSED - {reason}");
            eprintln!("  remedy: rebase, fix, push, retry");
            3
        }
        _ => {
            eprintln!("merge-result: unknown - {reason}");
            4
        }
    }
}
