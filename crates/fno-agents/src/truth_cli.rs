//! `fno agents truth`: classify a worker's supervision state from its
//! transcript tail. A read leaf with no action of its own: the router rewrites
//! it into `status --truth`, because the binary's action list is shrink-only.
//!
//! The reader is [`crate::session_truth::resolve_payload_with`], the same one
//! the daemon's truth probe answers from, so the CLI and the probe can never
//! read two different transcripts. This module adds only the CLI shape: the
//! single and `--handles` batch forms, the human line, and the exit codes.

use crate::session_truth::{cursor, resolve_payload_with, ReadMode, Stores};
use crate::wake_read::python_json_default;
use serde_json::{Map, Value};

const USAGE: &str = "pass exactly one of: a positional handle, or --handles a,b,c";

const HELP: &str = "\
Usage: fno agents truth [OPTIONS] [HANDLE]

  Classify a worker's supervision state from its transcript TAIL.

  done | watching | your-move | working | stalled | unknown. Read-only; exits
  13 on an unresolvable handle, 0 otherwise. --handles a,b,c answers many in
  one process and always exits 0: every entry carries its own state.

Options:
  --handles TEXT  Comma-separated handles, answered in ONE process.
  -J, --json      Emit a single JSON object instead of a line.
  --help          Show this message and exit.
";

/// The bases a resume can relaunch from: the human line says `exited`
/// (resumable) for these and `dead` for every other falsifier.
const RESUMABLE_BASES: [&str; 3] = ["exit-recorded", "process-gone", "pane-gone"];

/// The payload keys the CLI emits, in wire order. The reader also carries
/// `suggestions`, which the CLI payload never did.
const WIRE_KEYS: [&str; 14] = [
    "handle",
    "state",
    "reason",
    "last_activity_age_s",
    "last_event_at",
    "last_activity_basis",
    "last_message",
    "provider_refusal",
    "session_id",
    "observed_model",
    "harness_title",
    "reachability",
    "basis",
    "falsifier_error",
];

fn now_epoch_s() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// One truth payload per handle, in order, read in this process from the
/// ambient registry and stores. In-process callers use this instead of
/// spawning `fno agents truth`.
pub fn truth_payloads(handles: &[String]) -> Vec<(String, Value)> {
    truth_payloads_at(handles, now_epoch_s())
}

fn truth_payloads_at(handles: &[String], now_s: f64) -> Vec<(String, Value)> {
    let rows = crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .ok()
        .map(|r| r.entries);
    let stores = Stores::ambient();
    let cursors = cursor::global();
    handles
        .iter()
        .map(|h| {
            let payload =
                resolve_payload_with(rows.as_deref(), h, now_s, &stores, cursors, ReadMode::Build);
            (h.clone(), cli_payload(&payload))
        })
        .collect()
}

/// True when the handle resolves: the single form's exit 0.
pub fn resolves(handle: &str) -> bool {
    let (_, payload) = truth_payloads(&[handle.to_string()]).remove(0);
    !unresolved(&payload)
}

fn unresolved(payload: &Value) -> bool {
    payload["state"] == "unknown"
        && matches!(
            payload["reason"].as_str(),
            Some("not-found" | "resolver-error")
        )
}

/// The reader payload in the CLI wire shape and key order.
pub fn cli_payload(payload: &Value) -> Value {
    let mut out = Map::new();
    for key in WIRE_KEYS {
        out.insert(key.into(), payload.get(key).cloned().unwrap_or(Value::Null));
    }
    Value::Object(out)
}

/// Fixed width 4, so a column never reflows; an unread age renders `?`.
fn humanize_age(seconds: Option<i64>) -> String {
    let Some(s) = seconds else {
        return "   ?".to_string();
    };
    let (n, unit) = if s < 60 {
        (s, "s")
    } else if s < 3600 {
        (s / 60, "m")
    } else if s < 86400 {
        (s / 3600, "h")
    } else {
        ((s / 86400).min(999), "d")
    };
    format!("{:>4}", format!("{n}{unit}"))
}

fn model_clause(observed: &Value) -> String {
    match observed.get("kind").and_then(Value::as_str) {
        Some("observed") => format!(" on {}", py_str(observed.get("model"))),
        Some("no-model-yet") => ", no model yet".to_string(),
        Some("unreadable") => ", model unreadable".to_string(),
        _ => String::new(),
    }
}

fn evidence(state: &str) -> &'static str {
    match state {
        "done" => "promise emitted",
        "watching" => "watching external",
        "your-move" => "awaiting your reply",
        "working" => "active",
        "stalled" => "silent",
        "dead" => "falsified",
        "exited" => "resumable",
        _ => "",
    }
}

/// Python `str()` of an optional JSON scalar.
fn py_str(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => "None".to_string(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn nonempty(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// The human truth line: the state, the model, the evidence, and the
/// `[reachability: basis]` suffix. A falsified row reads `exited` when a
/// resume can relaunch it, else `dead`.
pub fn truth_line(payload: &Value) -> String {
    let reachability = py_str(payload.get("reachability"));
    let basis = py_str(payload.get("basis"));
    let mut state = payload["state"].as_str().unwrap_or("").to_string();
    if reachability == "unreachable" {
        let resumable =
            RESUMABLE_BASES.contains(&basis.as_str()) && nonempty(&payload["session_id"]).is_some();
        state = if resumable { "exited" } else { "dead" }.to_string();
    }
    let handle = payload
        .get("handle")
        .map(|v| py_str(Some(v)))
        .unwrap_or_else(|| "?".to_string());
    let body = if state == "unknown" {
        let reason = nonempty(&payload["reason"]).unwrap_or_else(|| "unresolved".to_string());
        format!("truth {handle}: unknown ({reason})")
    } else {
        let age = humanize_age(payload["last_activity_age_s"].as_i64());
        let model = model_clause(&payload["observed_model"]);
        let suffix = nonempty(&payload["last_activity_basis"])
            .map(|b| format!(" by {b}"))
            .unwrap_or_default();
        let tag = nonempty(&payload["reason"])
            .map(|r| format!(" [{r}]"))
            .unwrap_or_default();
        format!(
            "truth {handle}: {state}{model}{tag} ({}, last activity {age} ago{suffix})",
            evidence(&state)
        )
    };
    format!("{body} [{reachability}: {basis}]")
}

struct Args {
    handle: Option<String>,
    handles: Option<String>,
    json: bool,
}

fn parse(args: &[String]) -> Result<Args, i32> {
    let args = crate::client_commands::expand_eq(args);
    let mut out = Args {
        handle: None,
        handles: None,
        json: false,
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Err(0);
            }
            "--json" | "-J" => out.json = true,
            "--handles" => match it.next() {
                Some(v) => out.handles = Some(v),
                None => {
                    eprintln!("fno agents truth: --handles requires a value");
                    return Err(2);
                }
            },
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("fno agents truth: no such option: {s}");
                return Err(2);
            }
            _ if out.handle.is_none() => out.handle = Some(arg),
            s => {
                eprintln!("fno agents truth: got unexpected extra argument ({s})");
                return Err(2);
            }
        }
    }
    Ok(out)
}

/// `status --truth [HANDLE] [--handles a,b] [--json]`.
pub fn run_truth(args: &[String]) -> i32 {
    run_truth_at(args, now_epoch_s())
}

fn run_truth_at(args: &[String], now_s: f64) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let out = match (args.handle, args.handles) {
        (None, Some(list)) => {
            let names: Vec<String> = list
                .split(',')
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(str::to_string)
                .collect();
            let answers = truth_payloads_at(&names, now_s);
            if args.json {
                let map: Map<String, Value> = answers.into_iter().collect();
                format!("{}\n", python_json_default(&Value::Object(map)))
            } else {
                answers
                    .iter()
                    .map(|(name, payload)| format!("{name}: {}\n", truth_line(payload)))
                    .collect()
            }
        }
        (Some(handle), None) => {
            let (_, payload) = truth_payloads_at(&[handle], now_s).remove(0);
            let text = if args.json {
                python_json_default(&payload)
            } else {
                truth_line(&payload)
            };
            println!("{text}");
            return if unresolved(&payload) { 13 } else { 0 };
        }
        _ => {
            eprintln!("{USAGE}");
            return 2;
        }
    };
    print!("{out}");
    // Always 0: an unresolvable handle is reported in its own entry, never
    // in an exit code the whole batch would have to share.
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_falsified_row_reads_exited_only_when_resumable() {
        let mut p = json!({
            "handle": "w", "state": "stalled", "reason": null,
            "last_activity_age_s": 4000, "last_activity_basis": "last-entry",
            "observed_model": {"kind": "observed", "model": "m", "samples": 1},
            "session_id": "sid", "reachability": "unreachable", "basis": "process-gone",
        });
        assert_eq!(
            truth_line(&p),
            "truth w: exited on m (resumable, last activity   1h ago by last-entry) \
             [unreachable: process-gone]"
        );
        p["session_id"] = Value::Null;
        assert!(truth_line(&p).starts_with("truth w: dead on m (falsified,"));
    }

    #[test]
    fn age_is_fixed_width_and_capped() {
        assert_eq!(humanize_age(None), "   ?");
        assert_eq!(humanize_age(Some(59)), " 59s");
        assert_eq!(humanize_age(Some(86400 * 5000)), "999d");
    }
}
