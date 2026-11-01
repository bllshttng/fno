//! The two read leaves the dispatch-command retirement left behind (
//! change 2): `capabilities` reads the packaged harness capability table and
//! `target-family` classifies a message against the merge-posture family
//! table. With `--harness`, `target-family` also answers the loop gate: the
//! third question a dispatch asks, whether a looping dispatch at that
//! harness closes its loop on THIS machine. Both answer a question scripts
//! ask through the Python router, which execs them as arguments of the
//! `status` action (d-fe66560a keeps the binary's action list shrink-only);
//! they never touch the daemon.

use crate::merge_posture::is_target_family;
use serde_json::Value;
use std::path::Path;

/// `capabilities <harness> [--json|-J]`: one harness's config-independent
/// capability contract, read straight from the packaged table. The successor
/// to the retired dispatch capabilities query leaf (d-496680aa): the
/// JSON shape
/// (map_version, harness, then the harness's table) matches what the Python
/// leaf printed. An unknown harness exits 2 naming the harness and the
/// declared list, with nothing on stdout.
pub fn run_capabilities(args: &[String]) -> i32 {
    let json = args.iter().any(|a| a == "--json" || a == "-J");
    // An unknown flag refuses (exit 2) rather than reading as a positional:
    // the retired Python leaf's Typer usage error did the same, and a typo
    // like `--jsn` must not read as a clean answer.
    let unknown = args
        .iter()
        .find(|a| a.starts_with('-') && *a != "--json" && *a != "-J");
    if let Some(flag) = unknown {
        eprintln!("fno agents capabilities: unknown flag {flag:?} (expected [--json|-J])");
        return 2;
    }
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    if positional.len() != 1 {
        eprintln!("fno agents capabilities: exactly one harness argument is required");
        return 2;
    }
    let harness = positional[0].as_str();
    let (code, payload) = match capabilities_json(harness) {
        Ok(payload) => (0, payload),
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    let text = if json {
        serde_json::to_string(&payload).expect("serialized JSON")
    } else {
        serde_json::to_string_pretty(&payload).expect("serialized JSON")
    };
    println!("{text}");
    code
}

/// The capabilities answer, split from the printer so tests can read it.
/// `Err` carries the stderr line (the harness named, plus the declared list).
fn capabilities_json(harness: &str) -> Result<Value, String> {
    let contract = crate::harness_capabilities::HarnessContract::packaged()
        .map_err(|e| format!("fno agents capabilities: capability contract error: {e}"))?;
    let caps = contract.capabilities(harness).map_err(|_| {
        let declared: Vec<&str> = contract.harness.keys().map(String::as_str).collect();
        format!(
            "fno agents capabilities: unknown harness {harness:?}; the \
             packaged capability contract knows: {}",
            declared.join(", ")
        )
    })?;
    let caps_value = serde_json::to_value(caps)
        .map_err(|e| format!("fno agents capabilities: serialization error: {e}"))?;
    // Header fields first, then the harness table flattened beside them: the
    // key order the Python leaf printed (`{"map_version": ..., "harness":
    // ..., **caps_table}`).
    let mut out = serde_json::Map::new();
    out.insert(
        "map_version".to_string(),
        Value::Number(serde_json::Number::from(contract.map_version)),
    );
    out.insert("harness".to_string(), Value::String(harness.to_string()));
    if let Value::Object(table) = caps_value {
        for (key, value) in table {
            out.insert(key, value);
        }
    }
    Ok(Value::Object(out))
}

/// `target-family --message <m>`: print `family` when the message's first
/// token is a /target-family spelling, `other` otherwise, exit 0 either way.
/// The successor to the retired dispatch family query leaf; the
/// merge_posture
/// table is the same one the Rust merge-posture reads use.
pub fn run_target_family(args: &[String]) -> i32 {
    let mut message: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut extension_src: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if let Some(v) = arg.strip_prefix("--message=") {
            if message.is_some() {
                eprintln!("fno agents target-family: --message given twice");
                return 2;
            }
            message = Some(v.to_string());
            i += 1;
        } else if let Some(v) = arg.strip_prefix("--harness=") {
            if harness.is_some() {
                eprintln!("fno agents target-family: --harness given twice");
                return 2;
            }
            harness = Some(v.to_string());
            i += 1;
        } else if let Some(v) = arg.strip_prefix("--extension-src=") {
            if extension_src.is_some() {
                eprintln!("fno agents target-family: --extension-src given twice");
                return 2;
            }
            extension_src = Some(v.to_string());
            i += 1;
        } else if arg == "--message" || arg == "-m" {
            if message.is_some() || i + 1 >= args.len() {
                eprintln!("fno agents target-family: --message takes exactly one value");
                return 2;
            }
            message = Some(args[i + 1].clone());
            i += 2;
        } else if arg == "--harness" {
            if harness.is_some() || i + 1 >= args.len() {
                eprintln!("fno agents target-family: --harness takes exactly one value");
                return 2;
            }
            harness = Some(args[i + 1].clone());
            i += 2;
        } else if arg == "--extension-src" {
            if extension_src.is_some() || i + 1 >= args.len() {
                eprintln!("fno agents target-family: --extension-src takes exactly one value");
                return 2;
            }
            extension_src = Some(args[i + 1].clone());
            i += 2;
        } else {
            eprintln!("fno agents target-family: unknown argument {arg:?}");
            return 2;
        }
    }
    let Some(message) = message else {
        eprintln!("fno agents target-family: --message is required");
        return 2;
    };
    let Some(harness) = harness else {
        let verdict = if is_target_family(&message) {
            "family"
        } else {
            "other"
        };
        println!("{verdict}");
        return 0;
    };
    let family = is_target_family(&message);
    let refusal = loop_refusal(&harness, &message, extension_src.as_deref().map(Path::new));
    let out = serde_json::json!({ "family": family, "refusal": refusal });
    println!("{out}");
    0
}

/// Whether the routing walk must pass over `harness` for `work_command`: its
/// capability row can never close the command's loop, so the spawn gate would
/// refuse the pick. Row facts only: an install probe is the gate's job, and an
/// unknown harness stays the gate's to name.
pub fn lane_cannot_loop(harness: &str, work_command: &str) -> bool {
    if !is_target_family(&format!("/fno:{work_command}")) {
        return false;
    }
    capabilities_json(harness).is_ok_and(|row| {
        match row["loop_participation"].as_str().unwrap_or_default() {
            "native" => false,
            "extension" => row["loop_extension"]
                .as_str()
                .unwrap_or_default()
                .is_empty(),
            _ => true,
        }
    })
}

/// The loop gate on this machine: `Some` refuses a looping `message` at
/// `harness`. Spawn admission calls it with no `extension_src`, so the pi
/// probe there checks presence, not byte equality.
pub fn loop_refusal(harness: &str, message: &str, extension_src: Option<&Path>) -> Option<String> {
    if !is_target_family(message) {
        return None;
    }
    match capabilities_json(harness) {
        Err(text) => Some(text),
        Ok(row) => {
            let participation = row["loop_participation"].as_str().unwrap_or_default();
            let loop_extension = row["loop_extension"].as_str().unwrap_or_default();
            let probe = || crate::plugin_install::loop_install_probe(harness, extension_src);
            loop_gate_refusal(harness, participation, loop_extension, message, probe)
        }
    }
}

/// The loop gate, ported from Python `check_loop_participation`: `Some`
/// refuses a looping dispatch at `harness`, `None` admits it. The probe
/// closure carries the machine's install facts so the decision stays pure:
/// a non-looping command and a plain native row never run it, and the grok
/// probe (a 30-second-bounded `grok inspect`) runs only for a looping
/// command at a native row whose install the gate must ask about.
fn loop_gate_refusal(
    harness: &str,
    participation: &str,
    loop_extension: &str,
    command: &str,
    probe: impl FnOnce() -> Option<Result<(), String>>,
) -> Option<String> {
    if !is_target_family(command) {
        return None;
    }
    match participation {
        "native" => {
            if let Some(Err(detail)) = probe() {
                return Some(format!(
                    "refused: harness '{harness}' closes its loop through \
                     footnote's plugin hooks, and {harness} will not run them \
                     on this machine ({detail}). Run 'fno config plugin \
                     install {harness}', then dispatch again - a loop whose \
                     stop gate never runs would take '{command}' and never \
                     stop."
                ));
            }
            crate::loop_readiness::pre_launch_refusal(harness, command)
        }
        "extension" if !loop_extension.is_empty() => {
            let detail = match probe() {
                Some(Ok(())) => return None,
                Some(Err(detail)) => detail,
                None => "no install probe answered".to_string(),
            };
            Some(format!(
                "refused: harness '{harness}' closes its loop through a \
                 fno-installed extension that is absent or stale on this \
                 machine ({detail}). Run 'fno config plugin install \
                 {harness}' to install it, then dispatch again - a loop \
                 whose stop gate is not installed would take '{command}' \
                 and never stop."
            ))
        }
        _ => {
            let why = if participation == "none" {
                "no lifecycle boundary invokes loop-check"
            } else {
                "its loop rides a harness-native extension fno has not \
                 written yet and nothing invokes loop-check"
            };
            Some(format!(
                "refused: harness '{harness}' declares loop_participation = \
                 '{participation}', so {why} and the looping command \
                 '{command}' would never stop. Dispatch a one-shot instead."
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walk passes over a lane the spawn gate would refuse, and only that
    /// lane. zcode declares no loop extension, codex closes its own loop.
    #[test]
    fn a_lane_that_cannot_close_the_loop_is_named_for_looping_commands_only() {
        assert!(lane_cannot_loop("zcode", "target"));
        assert!(!lane_cannot_loop("zcode", "think"));
        assert!(!lane_cannot_loop("codex", "target"));
        assert!(!lane_cannot_loop("opencode", "target"));
        assert!(!lane_cannot_loop("no-such-harness", "target"));
    }

    #[test]
    fn capabilities_json_carries_the_header_and_the_harness_table() {
        let out = capabilities_json("codex").expect("codex is a declared harness");
        assert_eq!(out["harness"], "codex");
        assert!(out["map_version"].is_u64());
        // AC2-HP: the four fields the agent scripts read survive the port.
        assert_eq!(out["command_surface"], "codex-skill");
        assert!(out["keeper"].is_null());
        assert_eq!(
            out["resume_strategy"]["forms"]["interactive_attach"]["tokens"],
            serde_json::json!(["codex", "resume", "{session_id}", "--remote", "unix://"])
        );
    }

    #[test]
    fn capabilities_unknown_harness_names_the_name_and_the_declared_list() {
        let err = capabilities_json("nosuch").unwrap_err();
        assert!(err.contains("nosuch"));
        assert!(err.contains("claude"));
        assert!(err.contains("knows:"));
    }

    #[test]
    fn target_family_answers_family_then_other_and_always_exits_zero() {
        // AC3-HP: a /target-family message reads family; prose reads other.
        let family = run_target_family(&["--message".to_string(), "/fno:target x-1".to_string()]);
        assert_eq!(family, 0);
        let other = run_target_family(&["--message".to_string(), "hello".to_string()]);
        assert_eq!(other, 0);
        assert!(is_target_family("/fno:target x-1"));
        assert!(!is_target_family("hello"));
    }

    #[test]
    fn grok_native_with_an_absent_probe_refuses_and_names_the_fix() {
        let refusal = loop_gate_refusal("grok", "native", "", "/fno:target x-1", || {
            Some(Err("absent: no enabled fno plugin with hooks".to_string()))
        })
        .expect("an absent plugin must refuse");
        assert!(refusal.contains("grok"));
        assert!(refusal.contains("fno config plugin install grok"));
        assert!(refusal.contains("absent"));
        assert!(refusal.contains("never stop"));
    }

    #[test]
    fn a_native_row_with_a_reachable_probe_admits() {
        // claude carries a native row in the packaged table, so the native
        // arm's readiness legs admit; the probe passing is the row-agnostic
        // half of the contract (grok's row will consume it when its hooks
        // are proven on the wire).
        assert_eq!(
            loop_gate_refusal("claude", "native", "", "/fno:target x-1", || Some(Ok(()))),
            None
        );
    }

    #[test]
    fn pi_with_a_failing_probe_refuses_naming_the_setup_fix() {
        let refusal = loop_gate_refusal(
            "pi",
            "extension",
            "cli/src/fno/setup/assets/pi/footnote.ts",
            "/fno:target x-1",
            || {
                Some(Err(
                    "pi extension /p/footnote.ts is absent or stale".to_string()
                ))
            },
        )
        .expect("a missing pi extension must refuse");
        assert!(refusal.contains("fno config plugin install pi"));
        assert!(refusal.contains("pi extension /p/footnote.ts is absent or stale"));
    }

    #[test]
    fn an_extension_row_with_an_empty_artifact_names_the_unwritten_extension() {
        let refusal = loop_gate_refusal("cursor-agent", "extension", "", "/fno:target x-1", || {
            panic!("no probe runs for an empty artifact")
        })
        .expect("an unwritten extension must refuse");
        assert!(refusal.contains("has not written yet"));
    }
}
