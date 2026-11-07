//! `spawn-remote`: the cross-host spawn relay command. One ssh hop runs the
//! spawn ON the target host: the remote machine's own `fno agents spawn`
//! answers with its own gate, registry, config, and lanes, and its output
//! and exit code pass through byte-verbatim. A target whose host normalizes
//! to this machine strips the `@host` and execs the local front door, so
//! the local command's refusals stay the only voice. No store lane and no
//! queue: a spawn is a command execution, not a payload; an unreachable
//! host fails loudly and the caller retries.
//!
//! The sending machine's spawn gate admits the caller before the claim. A
//! `--cwd` or `--add-dir` value names a path ON THE TARGET HOST: the relay
//! forwards byte-verbatim and the remote command validates.

use crate::mail::address::normalize_host;
use crate::mail::address::{split_target, target_error};
use crate::mail_remote::fno_binary;
use std::ffi::OsString;
use std::process::Command;

/// What the relay execs, decided without touching the network. Pure, so the
/// same-host and cross-host shapes are testable without ssh.
pub struct SpawnPlan {
    /// The target host, normalized.
    pub host: String,
    /// True when the target is this machine: exec the stripped argv locally.
    pub same_host: bool,
    /// The remote (or local-forward) command words, beginning `agents spawn`.
    pub words: Vec<String>,
}

/// Build the plan from the relay tail (everything after `spawn-remote`).
/// `local_host` arrives pre-resolved, so tests pin the shapes without env
/// reads. The address rides in one of two slots: a `--name` value with an
/// `@host` part, or a bare `--machine <host>` pin, which keeps the
/// default name. `--machine` beside a plain `--name` is legal: the name
/// rides and the pin carries the host. Every other word rides in order.
/// `--name @<host>` drops the flag from the exec'd words, so the remote slug
/// mint answers and no recursion survives the strip.
pub fn spawn_plan(args: &[String], local_host: &str) -> Result<SpawnPlan, String> {
    let mut name_at: Option<(usize, String, usize)> = None;
    let mut machine_at: Option<(usize, String, usize)> = None;
    let mut i = 0;
    while i < args.len() {
        let word = args[i].as_str();
        // `taken` is the flag's argv span: the bare form is two slots
        // (flag, value), the typer equals spelling is one.
        let mut hit = |v: &str, taken: usize| -> Result<(), String> {
            if v.contains('@') && name_at.is_some() {
                return Err("spawn takes one cross-host --name".to_string());
            }
            if v.contains('@') {
                name_at = Some((i, v.to_string(), taken));
            }
            Ok(())
        };
        let mut pin = |v: &str, taken: usize| -> Result<(), String> {
            if machine_at.is_some() {
                return Err("spawn takes one --machine".to_string());
            }
            machine_at = Some((i, v.to_string(), taken));
            Ok(())
        };
        if word == "--name" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| "--name needs a value".to_string())?;
            hit(&value, 2)?;
            i += 2;
        } else if let Some(v) = word.strip_prefix("--name=") {
            hit(v, 1)?;
            i += 1;
        } else if word == "--machine" {
            let value = args
                .get(i + 1)
                .ok_or_else(|| "--machine needs a host".to_string())?;
            pin(&value, 2)?;
            i += 2;
        } else if let Some(v) = word.strip_prefix("--machine=") {
            pin(v, 1)?;
            i += 1;
        } else {
            i += 1;
        }
    }
    if name_at.is_some() && machine_at.is_some() {
        return Err("spawn takes one cross-host address: --machine <host> or \
             --name <name>@<host>, not both"
            .to_string());
    }
    let machine_pin = machine_at.is_some();
    let Some((idx, value, taken)) = name_at.or(machine_at) else {
        return Err(
            "usage: fno agents spawn --machine <host> | --name <name>@<host> \
             [args...] <prompt>"
                .to_string(),
        );
    };
    let (name, host) = if machine_pin {
        if value.is_empty() || value.contains('@') {
            return Err(format!("--machine takes a bare host, got {value:?}"));
        }
        (String::new(), value)
    } else if let Some(rest) = value.strip_prefix('@') {
        if rest.is_empty() {
            return Err(target_error(&value));
        }
        (String::new(), rest.to_string())
    } else {
        split_target(&value)?
    };
    let host = normalize_host(&host);
    let same_host = host == normalize_host(local_host);
    // The --name value loses its @host part; the flag disappears when the
    // name is empty. Every other word keeps its order byte-verbatim.
    let mut words: Vec<String> = vec!["agents".to_string(), "spawn".to_string()];
    for (i, word) in args.iter().enumerate() {
        if i == idx {
            if !name.is_empty() {
                words.push("--name".to_string());
                words.push(name.clone());
            }
        } else if !(taken == 2 && i == idx + 1) {
            words.push(word.clone());
        }
    }
    Ok(SpawnPlan {
        host,
        same_host,
        words,
    })
}

/// The relay words that carry a parent session across the ssh hop, empty
/// when no edge resolved. One flag: the child row reads the session id; the
/// parent's harness and cwd stay local facts.
fn parent_edge_args(session: Option<&str>) -> Vec<String> {
    match session.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => vec!["--spawned-by".to_string(), s.to_string()],
        None => Vec::new(),
    }
}

/// Binary-direct entry from `fno-agents spawn-remote <tail...>`, where the
/// tail is the spawn argv after the command word.
pub fn run(args: &[String]) -> i32 {
    let local_host = crate::name_ledger::local_bus_host();
    let plan = match spawn_plan(args, &local_host) {
        Ok(plan) => plan,
        Err(e) => {
            eprintln!("fno agents spawn: {e}");
            return 2;
        }
    };
    if plan.same_host {
        let mut argv: Vec<OsString> = vec![fno_binary()];
        for word in &plan.words {
            argv.push(OsString::from(word));
        }
        // No @ name rides, so the front door forwards, never re-enters.
        return crate::mail_remote::exec_forward(&argv, "fno agents spawn", false);
    }
    // An ssh hop forwards no caller environment, so the far side's ambient
    // parent capture resolves nothing and its row records no edge. Carry the
    // spawner's session id across as an explicit argument instead; a local
    // forward keeps the inherited environment and needs no flag.
    let (session, _harness, _cwd) = crate::claims::ambient_parent_edge();
    let mut words = plan.words.clone();
    words.extend(parent_edge_args(session.as_deref()));
    let argv = crate::mail_remote::ssh_argv(&plan.host, &words);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("fno agents spawn: ssh to {} failed: {e}", plan.host);
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // One table-style declaration: the CI delta caps new test declarations.
    #[test]
    fn spawn_relay_plans_its_shapes() {
        // Cross-host: the ssh argv runs the remote machine's own front door
        // with the @host part stripped and every other word in order.
        let plan = spawn_plan(
            &[
                "--name".to_string(),
                "quinlan@mbp3".to_string(),
                "fix".to_string(),
                "it".to_string(),
            ],
            "imac2d2",
        )
        .expect("plans");
        assert!(!plan.same_host);
        assert_eq!(plan.host, "mbp3");
        let line = crate::mail_remote::ssh_argv(&plan.host, &plan.words).join(" ");
        assert!(line.contains("'agents' 'spawn'"));
        assert!(line.contains("'--name' 'quinlan'"));
        assert!(
            !line.contains("'quinlan@mbp3'"),
            "the host part never rides"
        );
        // `@host` alone mints remotely: the flag disappears, no @ rides.
        let plan = spawn_plan(
            &["--name".to_string(), "@mbp3".to_string(), "hi".to_string()],
            "imac2d2",
        )
        .expect("plans");
        assert!(!plan.words.contains(&"--name".to_string()));
        assert!(!plan.words.iter().any(|w| w.contains('@')));
        // Same-host: the stripped words carry no @, so the forward cannot
        // re-enter this relay.
        let plan = spawn_plan(
            &[
                "--name".to_string(),
                "q@MBP3.local".to_string(),
                "hi".to_string(),
            ],
            "mbp3",
        )
        .expect("plans");
        assert!(plan.same_host);
        assert!(!plan.words.iter().any(|w| w.contains('@')));
        assert!(plan.words.contains(&"q".to_string()));
        // A second plain --name rides untouched beside the cross-host one.
        let plan = spawn_plan(
            &[
                "--name".to_string(),
                "q@mbp3".to_string(),
                "--name".to_string(),
                "plain".to_string(),
            ],
            "imac2d2",
        )
        .expect("plans");
        assert!(
            !plan.same_host,
            "the @-name carries the address; the plain one rides"
        );
        assert!(plan.words.contains(&"q".to_string()));
        assert!(plan.words.contains(&"plain".to_string()));
        // The typer equals spelling parses the same as the bare form.
        let plan =
            spawn_plan(&["--name=q@mbp3".to_string(), "hi".to_string()], "imac2d2").expect("plans");
        assert!(!plan.same_host);
        assert!(plan.words.contains(&"q".to_string()));
        assert!(!plan.words.iter().any(|w| w.contains('@')));
        // Each refusal shape: no @ name, a double @, empty host, bare @,
        // missing value.
        assert!(spawn_plan(&["hi".to_string()], "x").is_err());
        assert!(spawn_plan(&["--name".to_string(), "q@a@b".to_string()], "x").is_err());
        assert!(spawn_plan(&["--name".to_string(), "q@".to_string()], "x").is_err());
        assert!(spawn_plan(&["--name".to_string(), "@".to_string()], "x").is_err());
        assert!(spawn_plan(&["--name".to_string()], "x").is_err());
        // --machine pins the host and keeps the default name: the flag and
        // its value are stripped and the relayed words carry no name, so the
        // remote slug mint answers.
        let plan = spawn_plan(
            &[
                "--machine".to_string(),
                "mbp3".to_string(),
                "fix".to_string(),
                "it".to_string(),
            ],
            "imac2d2",
        )
        .expect("plans");
        assert!(!plan.same_host);
        assert_eq!(plan.host, "mbp3");
        assert!(plan.words.contains(&"fix".to_string()));
        assert!(!plan.words.contains(&"--machine".to_string()));
        assert!(!plan.words.iter().any(|w| w.contains('@')));
        // --machine beside a plain --name is legal: the name rides, the pin
        // carries the host.
        let plan = spawn_plan(
            &[
                "--machine".to_string(),
                "mbp3".to_string(),
                "--name".to_string(),
                "quinlan".to_string(),
                "hi".to_string(),
            ],
            "imac2d2",
        )
        .expect("plans");
        assert!(!plan.same_host);
        assert_eq!(plan.host, "mbp3");
        assert!(plan.words.contains(&"quinlan".to_string()));
        assert!(plan.words.contains(&"--name".to_string()));
        assert!(!plan.words.contains(&"--machine".to_string()));
        // The typer equals spelling pins the same way.
        let plan = spawn_plan(&["--machine=mbp3".to_string(), "hi".to_string()], "imac2d2")
            .expect("plans");
        assert_eq!(plan.host, "mbp3");
        // Each --machine refusal shape: duplicate pin, @-bearing or empty
        // value, missing value, and --machine beside an @-name.
        assert!(spawn_plan(&["--machine".to_string()], "x").is_err());
        assert!(spawn_plan(&["--machine".to_string(), "a@b".to_string()], "x").is_err());
        assert!(spawn_plan(
            &[
                "--machine".to_string(),
                "mbp3".to_string(),
                "--name".to_string(),
                "q@mbp3".to_string()
            ],
            "x"
        )
        .is_err());
    }

    // One table-style declaration: the CI delta caps new test declarations.
    #[test]
    fn the_relay_carries_a_resolved_parent_edge_over_the_hop() {
        // A resolved session rides as the explicit argument, sh-quoted into
        // the remote line so the far front door parses it.
        let mut words = spawn_plan(
            &[
                "--name".to_string(),
                "quinlan@mbp3".to_string(),
                "hi".to_string(),
            ],
            "imac2d2",
        )
        .expect("plans")
        .words;
        words.extend(parent_edge_args(Some(" s-parent ")));
        let line = crate::mail_remote::ssh_argv("mbp3", &words).join(" ");
        assert!(line.contains("'--spawned-by' 's-parent'"));
        // No resolved session adds no words: the relay stays byte-verbatim
        // and the remote row keeps its refusal reason.
        let mut plain = spawn_plan(
            &["--name".to_string(), "@mbp3".to_string(), "hi".to_string()],
            "imac2d2",
        )
        .expect("plans")
        .words;
        let before = plain.clone();
        plain.extend(parent_edge_args(None));
        assert_eq!(plain, before);
        // An empty or whitespace session is no edge.
        assert!(parent_edge_args(Some("  ")).is_empty());
    }
}
