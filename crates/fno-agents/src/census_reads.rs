//! The sweep and gate reads, nested under the census action: the fno-agents root
//! is at its ceiling, so each read is an argument, not a new root command.

use serde_json::{json, Value};

/// The read `args` names, or None when the census action owns them.
pub fn dispatch(args: &[String]) -> Option<i32> {
    let rest = args.get(1..).unwrap_or(&[]);
    Some(match args.first()?.as_str() {
        "watchdog" => crate::watchdog::cli::run(rest),
        "watchdog-tick" => crate::watchdog::tick::run(rest),
        "watchdog-tail" => crate::watchdog::tick::run_tail(rest),
        "watchdog-arming" => crate::watchdog::tick::run_arming(),
        "sweep" => crate::silence_sweep::run(rest),
        "stale-escalate" => crate::stale_lane::run_stale(rest),
        "friction-escalate" => crate::stale_lane::run_friction(rest),
        "recovery-sweep" => crate::recovery_sweep::run_cli(rest),
        "distress-verdicts" => run_distress_verdicts(rest),
        "gate-status" => run_gate_status(),
        "codex-session-for-pid" => run_codex_session_for_pid(rest),
        "spawn-guard" => run_spawn_guard(rest),
        _ => return None,
    })
}

fn run_distress_verdicts(args: &[String]) -> i32 {
    let raw = match args {
        [flag, value] if flag == "--sessions" => value,
        _ => {
            eprintln!("usage: fno agents distress-verdicts --sessions JSON");
            return 2;
        }
    };
    let Ok(parsed) = serde_json::from_str::<Value>(raw) else {
        eprintln!("fno agents distress-verdicts: --sessions must be JSON");
        return 2;
    };
    let Some(ids) = parsed.as_array() else {
        eprintln!("fno agents distress-verdicts: --sessions must be a JSON array");
        return 2;
    };
    let sessions: Vec<String> = ids
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    println!("{}", crate::watchdog::cli::distress_verdicts(&sessions));
    0
}

/// The spawn gate's read-only capacity verdict. An unanswered gate is `unknown`,
/// never saturation.
fn run_gate_status() -> i32 {
    let home = crate::paths::AgentsHome::from_env();
    let get = |key: &str| std::env::var(key).ok();
    let caller = crate::spawn_context::resolve_self_identity(&get, None, None, &home).session_id;
    let answer = crate::spawn_gate_command::probe_answer(&json!({
        "mode": "probe",
        "caller_session": caller,
        "only": Value::Null,
    }));
    println!("{answer}");
    0
}

/// Exits 13 with no stdout when the pid is gone, no rollout is open yet, or the
/// tree holds more than one distinct session.
fn run_codex_session_for_pid(args: &[String]) -> i32 {
    let Some(pid) = args.first().and_then(|p| p.parse::<u32>().ok()) else {
        eprintln!("usage: fno agents codex-session-for-pid PID");
        return 2;
    };
    match crate::pane_spawn::codex::rollout_session_id_for_tree(pid) {
        Some(session_id) => {
            println!("session_id={session_id}");
            0
        }
        None => 13,
    }
}

struct GuardArgs {
    node_id: String,
    holder: String,
    ttl: String,
    no_reserve: bool,
    cwd: Option<String>,
    json_output: bool,
}

fn parse_guard_args(args: &[String]) -> Result<GuardArgs, String> {
    let mut node_id = None;
    let mut holder = None;
    let mut ttl = "3m".to_string();
    let mut no_reserve = false;
    let mut cwd = None;
    let mut json_output = false;
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].as_str();
        let mut value = || -> Result<String, String> {
            index += 1;
            args.get(index)
                .cloned()
                .ok_or_else(|| format!("{arg} needs a value"))
        };
        match arg {
            "--holder" => holder = Some(value()?),
            "--ttl" => ttl = value()?,
            "--cwd" => cwd = Some(value()?),
            "--no-reserve" => no_reserve = true,
            "--json" | "-J" => json_output = true,
            flag if flag.starts_with('-') => return Err(format!("unknown flag {flag}")),
            positional if node_id.is_none() => node_id = Some(positional.to_string()),
            extra => return Err(format!("unexpected argument {extra}")),
        }
        index += 1;
    }
    Ok(GuardArgs {
        node_id: node_id.ok_or("missing node id")?,
        holder: holder.ok_or("missing --holder")?,
        ttl,
        no_reserve,
        cwd,
        json_output,
    })
}

/// The verdict line shell callers parse with sed. Booleans render lowercase: a
/// capitalised token would be a second spelling of the same fact.
pub(crate) fn guard_line(verdict: &Value) -> String {
    let mut parts = vec![format!(
        "verdict={}",
        verdict.get("verdict").and_then(Value::as_str).unwrap_or("")
    )];
    for (key, value) in verdict.as_object().into_iter().flatten() {
        if key == "verdict" {
            continue;
        }
        let rendered = match value {
            Value::String(text) if key == "detail" => format!("\"{text}\""),
            Value::String(text) => text.clone(),
            Value::Null => "None".to_string(),
            other => other.to_string(),
        };
        parts.push(format!("{key}={rendered}"));
    }
    parts.join(" ")
}

/// The shared dispatch mutex: the one source of truth for `/target bg` and
/// `/agent spawn`. Exit 0 for every answered verdict; non-zero only for a usage
/// error or a fail-closed guard error.
fn run_spawn_guard(args: &[String]) -> i32 {
    let parsed = match parse_guard_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("fno agents spawn-guard: {message}");
            return 2;
        }
    };
    let (verdict, exit) = match crate::claims::parse_ttl_ms(&parsed.ttl) {
        None => (
            json!({
                "verdict": "error",
                "detail": format!("malformed --ttl {:?}; not dispatching", parsed.ttl),
            }),
            3,
        ),
        Some(ttl_ms) => crate::spawn_guard::decide(&crate::spawn_guard::GuardRequest {
            node_id: &parsed.node_id,
            holder: &parsed.holder,
            ttl_ms,
            no_reserve: parsed.no_reserve,
            cwd: parsed.cwd.as_deref(),
            handover_holder: None,
            root: None,
            pid: Some(std::process::id()),
        }),
    };
    if parsed.json_output {
        println!("{verdict}");
    } else {
        println!("{}", guard_line(&verdict));
    }
    exit
}
