//! The hold front verbs: `hold` (alias `dnd`) and `hold-release`.
//!
//! Port of the Python hold front onto the native mail-hold clock and the
//! registry delivery stamp. The receipt and help name do-not-disturb (DND);
//! the verb stays `hold` with `dnd` accepted as an alias everywhere the
//! front dispatches.

use crate::mail_header::{render_held_release, HeldMessage, HeldRelease};
use crate::mail_hold::{
    clock_description, hold_sidecar_path, identity_key, read_clock, row_matches_token,
};
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::Write as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The identity refusal shared by both verbs: hold stamps a delivery policy
/// on a registry row, so an unprovable self is a hard stop, not a default.
/// The ambiguous case prints its own detail; the generic line is the
/// missing-identity message only, matching the Python leg's per-case output.
fn self_identity_or_exit() -> Option<(String, String)> {
    let home = AgentsHome::from_env();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!("cannot decide which session is 'self': multiple harness markers present (inherited env?)");
        eprintln!("a hold stamped on the wrong row holds another agent's mail");
        return None;
    }
    match (ident.session_id.clone(), ident.harness.clone()) {
        (Some(session_id), Some(harness)) => Some((session_id, harness)),
        _ => {
            eprintln!("no provable harness identity - there is no session to hold mail for");
            None
        }
    }
}

const HOLD_USAGE: &str =
    "usage: fno agents mail admin hold (--for <minutes> | --minutes <minutes> | --off | --status)
  --for N      do-not-disturb for N wall-clock minutes
  --minutes N  do-not-disturb until N quiet minutes pass
  --off        lift do-not-disturb and deliver what it held
  --status     say whether do-not-disturb is on";

/// `hold` (alias `dnd`): stamp bus-only on the self row and arm the clock.
/// A bare call prints usage and writes nothing: an arm is always a choice
/// the caller names, never a default a stray call falls into.
pub fn run_hold(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("{HOLD_USAGE}");
        return 2;
    }
    let Some((session_id, harness)) = self_identity_or_exit() else {
        return 3;
    };
    let handle = crate::identity::canonical_handle(&session_id);
    let clock_key = identity_key(&session_id);

    let mut minutes: Option<i64> = None;
    let mut for_minutes: Option<i64> = None;
    let mut status = false;
    let mut off = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--minutes" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => minutes = Some(v),
                None => {
                    eprintln!("error: --minutes wants an integer");
                    return 2;
                }
            },
            "--for" => match it.next().and_then(|v| v.parse::<i64>().ok()) {
                Some(v) => for_minutes = Some(v),
                None => {
                    eprintln!("error: --for wants an integer");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("{HOLD_USAGE}");
                return 0;
            }
            "--status" => status = true,
            "--off" => off = true,
            other => {
                eprintln!("hold: unknown argument {other:?}");
                return 2;
            }
        }
    }
    if minutes.is_some() && for_minutes.is_some() {
        eprintln!("error: --minutes and --for are mutually exclusive");
        return 2;
    }

    if status {
        let policy = entry_for(&session_id)
            .and_then(|e| e.delivery_policy)
            .unwrap_or_default();
        return crate::mail_hold::run_mail_hold(&[
            "--status-line".to_string(),
            handle,
            "--policy".to_string(),
            policy,
        ]);
    }

    if off {
        let was_on = entry_for(&session_id)
            .and_then(|e| e.delivery_policy)
            .as_deref()
            == Some("bus-only")
            || read_clock(&clock_key).is_some();
        let result = release(&clock_key, 0);
        if result.get("policy_cleared").and_then(Value::as_bool) != Some(true) {
            eprintln!("hold NOT off: the registry write failed, so {handle} still reads bus-only and mail is still held. Retry, or check `fno agents list` for the row.");
            return 1;
        }
        let held = result
            .get("held_count")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if held > 0 {
            println!(
                "hold off: delivered {held} held message(s) ({} deduped) - {}",
                result
                    .get("deduped_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
                result.get("outcome").and_then(Value::as_str).unwrap_or(""),
            );
        } else if was_on {
            println!("hold off: do-not-disturb cleared for {handle}; no mail was waiting");
        } else {
            println!("hold off: do-not-disturb was not on for {handle}; nothing to clear");
        }
        return 0;
    }

    let wall = for_minutes.is_some();
    let Some(window) = for_minutes.or(minutes) else {
        eprintln!("{HOLD_USAGE}");
        return 2;
    };
    if window < 1 {
        eprintln!(
            "error: {} must be at least 1",
            if wall { "--for" } else { "--minutes" }
        );
        return 2;
    }
    if let Err(exc) = register_existing_session(&harness, &session_id) {
        eprintln!("hold: {exc}");
        return 1;
    }

    let mut arm = vec![
        "--arm".to_string(),
        clock_key.clone(),
        "--minutes".to_string(),
        window.to_string(),
    ];
    if wall {
        arm.push("--kind".to_string());
        arm.push("wall".to_string());
    }
    let code = crate::mail_hold::run_mail_hold(&arm);
    if code != 0 {
        return code;
    }
    let clock = read_clock(&clock_key);
    let clock_text = match &clock {
        Some(c) => clock_description(c),
        None => "no expiry".to_string(),
    };
    let holds_until = clock
        .as_ref()
        .and_then(|c| c.until)
        .map(|u| u.format("%H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "an unknown time".to_string());
    println!("do-not-disturb on for {handle}: {clock_text}, holds until {holds_until} ({window}m), then delivers itself.");
    0
}

/// `hold-release`: wait out the clock on `--handle`, then drain what it held.
pub fn run_hold_release(args: &[String]) -> i32 {
    let mut handle: Option<String> = None;
    let mut poll_s: f64 = 15.0;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            // A flag-shaped value is a typo'd invocation; the old typer
            // surface refused it, so a quiet no-clock exit would hide the miss.
            "--handle" => match it.next() {
                Some(v) if !v.starts_with("--") => handle = Some(v.clone()),
                _ => {
                    eprintln!("error: --handle is required");
                    return 2;
                }
            },
            "--poll-s" => match it.next().and_then(|v| v.parse::<f64>().ok()) {
                Some(v) => poll_s = v,
                None => {
                    eprintln!("error: --poll-s wants a number");
                    return 2;
                }
            },
            other => {
                eprintln!("hold-release: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let Some(handle) = handle else {
        eprintln!("error: --handle is required");
        return 2;
    };

    let started = Instant::now();
    loop {
        let until = read_clock(&handle).and_then(|c| c.until);
        let Some(until) = until else {
            // No clock, or a clock with no deadline (permanent): nothing to
            // wait out, so leave quietly without touching the registry.
            return 0;
        };
        // Whole-second truncation releases early: a 0.4s remainder read as
        // 0 and the loop broke before the deadline. Milliseconds keep the
        // sub-second slice, and nap covers it when under the poll interval.
        let remaining_ms = (until - chrono::Utc::now()).num_milliseconds();
        if remaining_ms <= 0 {
            break;
        }
        let nap = Duration::from_millis(remaining_ms as u64)
            .min(Duration::from_secs_f64(poll_s.max(1.0)));
        std::thread::sleep(nap);
    }
    let held_for_s = started.elapsed().as_secs() as i64;
    let result = release(&handle, held_for_s);
    println!("{}", serde_json::to_string(&result).unwrap_or_default());
    0
}

/// Drain the held bus for `handle` and clear the delivery stamp. Port of the
/// Python release primitive: clock text is read BEFORE any mutation, the
/// stamp clears first (a failed stamp leaves everything as it was), and the
/// entry resolves after the clear so lanes re-check policy on what we hand over.
pub(crate) fn release(handle: &str, held_for_s: i64) -> Value {
    let clock_text = match read_clock(handle) {
        Some(c) => clock_description(&c),
        None => "no expiry".to_string(),
    };
    let policy_cleared = clear_policy(handle);
    if policy_cleared {
        let _ = std::fs::remove_file(hold_sidecar_path(handle));
    }

    let home = AgentsHome::from_env();
    let entry = entry_for(handle);
    let sid = entry.as_ref().and_then(|e| e.harness_session_id.clone());
    let forms: Vec<String> = match &sid {
        Some(sid) => dedupe_keep_order(vec![
            crate::spawn_context::session_identity_key(sid),
            crate::identity::canonical_handle(sid),
            handle.to_string(),
        ]),
        None => vec![handle.to_string()],
    };

    let bus = bus_dir(&home);
    let msgs = read_bus(&bus);
    let mut merged: Vec<Value> = Vec::new();
    for form in &forms {
        for m in crate::mail_control_drain::unread_messages(&bus, &msgs, form) {
            merged.push(m.clone());
        }
    }
    let merged = dedupe_keep_order_by_id(merged);
    let held_count = merged.len();

    let mut miss_reason: Option<String> = None;
    let mut outcome = "empty".to_string();
    if held_count > 0 {
        match (&entry, &sid) {
            (Some(entry), Some(sid)) => {
                let messages: Vec<HeldMessage> = merged
                    .iter()
                    .map(|m| HeldMessage {
                        sender: str_field(m, "from", "?"),
                        sent_at: str_field(m, "ts", ""),
                        id: str_field(m, "id", "?"),
                        body: str_field(m, "body", ""),
                    })
                    .collect();
                let digest = render_held_release(&HeldRelease {
                    held_for_s,
                    harness: entry.harness.clone(),
                    messages,
                });
                match crate::mail_envelope::render_at(
                    &json!({"mode": "held-release", "body": digest}),
                    &AgentsHome::shared_registry_json(),
                ) {
                    Ok(framed) => {
                        let (delivered, reason) = deliver_live(sid, &framed);
                        if delivered {
                            outcome = "delivered".to_string();
                            for form in &forms {
                                for m in &merged {
                                    crate::mail_notify_self::advance_main_cursor(
                                        &bus,
                                        &msgs,
                                        form,
                                        &str_field(m, "id", "?"),
                                    );
                                }
                            }
                            for m in &merged {
                                crate::mail_notify_self::emit_marker(handle, m, "printed");
                            }
                        } else {
                            outcome = "inject-missed".to_string();
                            miss_reason = reason;
                        }
                    }
                    Err(exc) => {
                        outcome = "inject-missed".to_string();
                        miss_reason =
                            Some(truncate_160(format!("deliver-raised:ValueError: {exc}")));
                    }
                }
            }
            _ => {
                outcome = "inject-missed".to_string();
                miss_reason = Some("no-registry-row".to_string());
            }
        }
    }

    emit_release_marker(handle, held_count, &outcome, held_for_s);

    json!({
        "handle": handle,
        "clock": clock_text,
        "held_count": held_count,
        "deduped_count": 0,
        "held_for_s": held_for_s,
        "outcome": outcome,
        "miss_reason": miss_reason,
        "policy_cleared": policy_cleared,
    })
}

/// The release marker, in the hybrid row shape the deleted Python emitter
/// wrote: flat kind-keyed fields beside the envelope keys, committed to the
/// state-root store (`paths.state_dir() / "events.jsonl"`), never the
/// agents-home journal. A failed commit warns to stderr and swallows, as
/// Python's emitter did.
fn emit_release_marker(handle: &str, held_count: usize, outcome: &str, held_for_s: i64) {
    let data = json!({
        "handle": handle,
        "held_count": held_count,
        "outcome": outcome,
        "held_for_s": held_for_s,
    });
    let mut row = data.clone();
    row["ts"] = json!(chrono::Utc::now().to_rfc3339());
    row["kind"] = json!("mail_hold_released");
    row["type"] = json!("mail_hold_released");
    row["source"] = json!("agents");
    row["data"] = data;
    let journal = match crate::agents_config::state_dir(std::path::Path::new(".")) {
        Some(root) => root.join("events.jsonl"),
        None => {
            eprintln!("warning: no state dir; mail_hold_released not committed");
            return;
        }
    };
    if let Err(error) = crate::event_store::append_envelope(&journal, &row.to_string(), None) {
        eprintln!("warning: could not commit mail_hold_released: {error}");
    }
}

/// The registry stamp, as a tri-state: true when the desired state holds
/// (no row to strand counts as success), false only on a store failure.
fn clear_policy(session_id: &str) -> bool {
    let path = AgentsHome::shared_registry_json();
    crate::state::update_registry(&path, |registry| {
        if let Some(i) = registry
            .entries
            .iter()
            .position(|e| row_matches_token(e, session_id))
        {
            registry.entries[i].delivery_policy = None;
        }
    })
    .is_ok()
}

fn entry_for(session_id: &str) -> Option<crate::state::RegistryEntry> {
    let registry = crate::state::load_registry(&AgentsHome::shared_registry_json()).ok()?;
    registry
        .entries
        .into_iter()
        .find(|e| row_matches_token(e, session_id))
}

/// Stamp bus-only on this session's row, minting one when absent. Port of
/// the Python register_existing_session: refusal inside a self-spawn, a
/// known-harness check, idempotent refresh on (harness, harness_session_id).
pub(crate) fn register_existing_session(harness: &str, session_id: &str) -> Result<(), String> {
    if std::env::var_os("FNO_AGENT_SELF").is_some_and(|v| !v.is_empty()) {
        return Err(
            "refusing to register from inside a session that has FNO_AGENT_SELF set".to_string(),
        );
    }
    if !crate::provider::KNOWN_HARNESSES.contains(&harness) {
        return Err(format!("unknown harness for registration: {harness}"));
    }
    if session_id.is_empty() {
        return Err("no session id to register".to_string());
    }
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let path = AgentsHome::shared_registry_json();
    crate::state::update_registry(&path, |registry| {
        let existing = registry.entries.iter_mut().find(|e| {
            e.harness.as_deref() == Some(harness)
                && e.harness_session_id.as_deref() == Some(session_id)
        });
        if let Some(row) = existing {
            row.delivery_policy = Some("bus-only".to_string());
            if row.cwd.is_empty() {
                row.cwd = cwd.clone();
            }
            return;
        }
        let base = crate::identity::canonical_handle(session_id);
        let mut name = base.clone();
        let mut n = 1;
        while registry.entries.iter().any(|e| e.name == name) {
            n += 1;
            name = format!("{base}-{n}");
        }
        registry.entries.push(crate::state::RegistryEntry {
            name,
            harness: Some(harness.to_string()),
            cwd,
            delivery_policy: Some("bus-only".to_string()),
            status: crate::AgentStatus::Idle,
            ..crate::state::RegistryEntry::new(
                Some(session_id.to_string()),
                crate::state::Lineage::unproven(
                    "mail-hold register: self-registration carries no parent edge",
                ),
            )
        });
    })
    .map_err(|e| format!("registry write failed: {e}"))?;
    Ok(())
}

/// One delivery attempt over the inject path: the current binary re-entered
/// as `mail-inject` with the framed payload on stdin. Success is exit 0; a
/// clean failure surfaces the inject outcome's own reason when it prints one.
fn deliver_live(session_id: &str, framed: &str) -> (bool, Option<String>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return (
                false,
                Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
            )
        }
    };
    let mut child = match Command::new(exe)
        .args(["mail-inject", "--session", session_id])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                false,
                Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
            )
        }
    };
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let _ = stdin.write_all(framed.as_bytes());
    }
    match child.wait_with_output() {
        Ok(out) if out.status.success() => (true, None),
        Ok(out) => {
            let reason = std::str::from_utf8(&out.stdout)
                .ok()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
                .and_then(|v| {
                    v.get("outcome")
                        .and_then(|o| o.get("reason"))
                        .and_then(Value::as_str)
                        .or_else(|| v.get("reason").and_then(Value::as_str))
                        .map(str::to_string)
                });
            (false, reason)
        }
        Err(e) => (
            false,
            Some(truncate_160(format!("deliver-raised:OSError: {e}"))),
        ),
    }
}

fn bus_dir(home: &AgentsHome) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| home.root().to_path_buf());
    crate::intel::bus_dir_of(&root)
}

fn read_bus(bus: &PathBuf) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(bus.join("messages.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .collect()
}

fn str_field(m: &Value, key: &str, blank: &str) -> String {
    m.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(blank)
        .to_string()
}

fn dedupe_keep_order(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for item in items {
        if seen.insert(item.clone()) {
            out.push(item);
        }
    }
    out
}

fn dedupe_keep_order_by_id(messages: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in messages {
        if seen.insert(str_field(&m, "id", "?")) {
            out.push(m);
        }
    }
    out
}

fn truncate_160(detail: String) -> String {
    detail.chars().take(160).collect()
}
