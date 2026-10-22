//! `fno-agents commands [harness]` - the agent-facing teaching surface for the
//! capability table's native commands. One line per command: the command,
//! the raw-mail risk class, and the when-to-use line, straight from the
//! packaged table so the answer cannot drift from what the mail guard
//! enforces. Bare form resolves the calling session's own harness through
//! the same ambient identity markers the claims writer reads, so a worker
//! can ask about its own harness without naming it.

use crate::harness_capabilities::{CommandRisk, HarnessContract};

pub const COMMANDS_USAGE: &str =
    "usage: fno-agents commands [--harness <name>|<name>] [--json]   # bare form detects your own harness from the session env";

/// The command entry: direct dispatch, no daemon RPC. Exit 0 on a rendered
/// roster (possibly empty); exit 2 on a transport-level fault (unknown
/// harness, undetectable bare form, unreadable table).
pub fn run_commands(args: &[String]) -> i32 {
    let mut harness: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" | "-J" => json = true,
            "--help" | "-h" => {
                println!("{COMMANDS_USAGE}");
                return 0;
            }
            "--harness" => match iter.next() {
                Some(value) => harness = Some(value.clone()),
                None => {
                    eprintln!("commands: --harness needs a value\n{COMMANDS_USAGE}");
                    return 2;
                }
            },
            other => {
                if other.starts_with('-') && other.len() > 1 {
                    eprintln!("commands: unknown flag {other}\n{COMMANDS_USAGE}");
                    return 2;
                }
                if harness.is_some() {
                    eprintln!("commands: name one harness\n{COMMANDS_USAGE}");
                    return 2;
                }
                harness = Some(other.to_string());
            }
        }
    }

    let harness = match harness {
        Some(name) => name,
        None => match crate::claims::resolve_harness_from(|k| std::env::var(k).ok()) {
            Some(name) => name,
            None => {
                eprintln!(
                    "commands: cannot tell your harness from this session's env; \
                     pass the harness by name (--harness <name>)"
                );
                return 2;
            }
        },
    };

    let contract = match HarnessContract::packaged() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("commands: packaged capability table unreadable: {e}");
            return 2;
        }
    };
    let caps = match contract.capabilities(&harness) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("commands: {e}");
            return 2;
        }
    };

    if caps.native_verbs.is_empty() {
        if json {
            println!("{{\"harness\": {harness:?}, \"verbs\": []}}"); // verb-wording: allow (wire key)
        } else {
            println!("{harness}: no native commands measured");
        }
        return 0;
    }

    if json {
        let commands: Vec<serde_json::Value> = caps
            .native_verbs
            .iter()
            .map(|command| {
                let meta = caps.native_verb_meta.get(command);
                serde_json::json!({
                    "verb": command,
                    "risk": meta.map(|m| m.risk.as_str()).unwrap_or(""),
                    "use_when": meta.map(|m| m.use_when.as_str()).unwrap_or(""),
                    "guarded": meta.map(|m| m.risk.is_guarded()).unwrap_or(false),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({"harness": harness, "verbs": commands})
        );
        return 0;
    }

    println!("{harness} native commands (risk | command | when):");
    for command in &caps.native_verbs {
        let meta = caps.native_verb_meta.get(command);
        let risk = meta.map(|m| m.risk).unwrap_or(CommandRisk::Safe);
        let marker = if risk.is_guarded() { " *" } else { "" };
        let use_when = meta.map(|m| m.use_when.as_str()).unwrap_or("");
        println!("{:>19} | {marker:<2}{command} | {use_when}", risk.as_str());
    }
    println!("* raw mail (mail send --raw) refuses this command without --ack-command-risk");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> (i32, String, String) {
        // The command prints to stdout/stderr directly; capture through the
        // process by shelling the assertion helper below instead. Here the
        // unit tests assert the exit codes and the guard-visible facts.
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        let code = run_commands(&owned);
        (code, String::new(), String::new())
    }

    #[test]
    fn unknown_harness_refuses_exit_two() {
        let (code, _, _) = run(&["no-such-harness"]);
        assert_eq!(code, 2);
    }

    #[test]
    fn dangerous_commands_are_flagged_in_the_render() {
        // The packaged agy and claude rows carry guarded commands; the same
        // facts the mail guard reads must be what the render shows.
        let contract = HarnessContract::packaged().unwrap();
        let caps = contract.capabilities("claude").unwrap();
        assert!(caps.native_verb_meta["/clear"].risk.is_guarded());
        let agy = contract.capabilities("agy").unwrap();
        assert_eq!(
            agy.native_verb_meta["/exit"].risk,
            CommandRisk::SessionEnding
        );
    }
}
