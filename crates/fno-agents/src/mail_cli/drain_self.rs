//! `mail drain-self`: drain every unread message addressed to any form of the
//! caller's identity, then ack per address form.
//!
//! Port of the Python drain-self verb. Forward-only + inject-before-ack: the
//! bodies render and flush BEFORE any cursor advances, so a crash re-surfaces
//! the message next SessionStart (a harmless repeat), never a loss. No harness
//! identity in env is a silent no-op (`[]` under `--json`), never an error, so
//! the hook is safe on any surface. A failed stdout delivery exits 1 before
//! any cursor advances, so the never-a-loss invariant holds on that path too.
//!
//! Two documented divergences from the Python leg, both output-only:
//! - The ambiguous-identity case prints one fixed sentence instead of the
//!   dynamic multi-line ambiguity report (hold-verb wording, hook-safe exit 0).
//! - The held-job manifest reader rejects the literal `"null"` placeholder at
//!   the call site, because the shared `scan_manifest_field` only rejects
//!   empty values where the Python field reader rejects both.

use crate::mail_control_drain::{present_mail_ids, unread_messages};
use crate::mail_notify_self::{advance_main_cursor, read_bus};
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// `drain-self`: print this session's unread mail, then ack it per form.
pub fn run_drain_self(args: &[String]) -> i32 {
    let mut json_out = false;
    for a in args {
        match a.as_str() {
            "--json" | "-J" => json_out = true,
            other => {
                eprintln!("drain-self: unknown argument {other:?}");
                return 2;
            }
        }
    }
    let home = AgentsHome::from_env();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
    if matches!(ident.disposition, OwnedDisposition::Ambiguous) {
        eprintln!(
            "error: drain-self: cannot decide which session is 'self': multiple harness markers present (inherited env?)"
        );
        return 0;
    }
    let sid = ident.session_id.clone().filter(|s| !s.is_empty());
    let harness = ident.harness.clone().filter(|s| !s.is_empty());
    let (Some(sid), Some(_harness)) = (sid, harness) else {
        if json_out {
            println!("[]");
        }
        return 0;
    };

    let handle = crate::identity::canonical_handle(&sid);
    // Drain every address form this session owns: the canonical first-eight
    // (new mail), the full id (the collision-escape send path), and the legacy
    // last-eight (pre-flip mail still on the bus). Each address has its own
    // cursor; a message has one `to`, so it matches exactly one form.
    let forms = dedupe_keep_order(vec![
        handle.clone(),
        crate::spawn_context::session_identity_key(&sid),
        crate::identity::legacy_suffix_handle(&sid),
    ]);

    let bus = bus_dir(&home);
    let msgs = read_bus(&bus);

    let mut last_by_form: Vec<(String, String)> = Vec::new();
    let mut form_by_id: HashMap<String, String> = HashMap::new();
    let mut all: Vec<Value> = Vec::new();
    for form in &forms {
        let got = unread_messages(&bus, &msgs, form);
        if !got.is_empty() {
            last_by_form.push((form.clone(), str_field(got.last().unwrap(), "id", "?")));
        }
        for m in &got {
            form_by_id
                .entry(str_field(m, "id", "?"))
                .or_insert_with(|| form.clone());
            // `m` is `&&Value`; `.clone()` on it would clone the reference.
            all.push((*m).clone());
        }
    }
    let mut seen = HashSet::new();
    let mut drained: Vec<Value> = Vec::new();
    for m in all {
        if seen.insert(str_field(&m, "id", "?")) {
            drained.push(m);
        }
    }
    // Render chronologically: the forms are scanned in address-order, not
    // log-order, so without this a legacy (pre-flip) message would render
    // after a newer canonical one. Cursor advance below is per-form and
    // unaffected. `sort_by_key` is stable, matching the Python leg.
    drained.sort_by_key(|m| str_field(m, "ts", ""));

    // Job mail: also drain mail addressed to the node THIS session holds.
    let (job_addr, job_msgs) = scan_held_job_mail(&sid, &bus, &msgs);

    // Cross-delivery dedup: an id already present in THIS session's transcript
    // is not surfaced again. `None` means the transcript could not be read; a
    // read failure is not evidence of absence, so print everything.
    let present = present_mail_ids(&crate::claude_drive::claude_projects_dir(), &sid);
    let landed = |m: &Value| match &present {
        Some(ids) => ids.contains(&str_field(m, "id", "")),
        None => false,
    };
    // The filter closure sees `&&Value`; `landed` takes `&Value`, and closure
    // calls do not deref-coerce, so every hand-off goes through `*m`.
    let to_print: Vec<&Value> = drained.iter().filter(|m| !landed(*m)).collect();
    let skipped: Vec<&Value> = drained.iter().filter(|m| landed(*m)).collect();
    let job_to_print: Vec<&Value> = job_msgs.iter().filter(|m| !landed(*m)).collect();
    let job_skipped: Vec<&Value> = job_msgs.iter().filter(|m| landed(*m)).collect();

    let mut out = String::new();
    if json_out {
        let mut rows: Vec<String> = to_print.iter().map(|m| json_row(m, None)).collect();
        rows.extend(
            job_to_print
                .iter()
                .map(|m| json_row(m, job_addr.as_deref())),
        );
        // Python `json.dumps` default separators: ", " between items and
        // between key and value, assembled by hand for byte parity.
        out.push_str(&format!("[{}]\n", rows.join(", ")));
    } else {
        if !to_print.is_empty() {
            out.push_str(&format!(
                "[fno agents mail] {} message(s) for {handle}:\n",
                to_print.len()
            ));
            for m in &to_print {
                out.push_str(&header_line(m));
                out.push('\n');
                out.push_str(&str_field(m, "body", ""));
                out.push('\n');
            }
        }
        if !job_to_print.is_empty() {
            out.push_str(&format!(
                "\n[fno agents mail] {} job message(s) for {}:\n",
                job_to_print.len(),
                job_addr.as_deref().unwrap_or("")
            ));
            for m in &job_to_print {
                out.push_str(&header_line(m));
                out.push('\n');
                out.push_str(&str_field(m, "body", ""));
                out.push('\n');
            }
        }
        if !to_print.is_empty() || !job_to_print.is_empty() {
            out.push_str(
                "\n[fno agents mail] to answer one: fno agents mail reply --to <id> --body \"...\"\n",
            );
        }
    }
    // Delivery gates the ack: a failed write or flush leaves every cursor
    // where it is, so the mail re-surfaces next SessionStart. The Python leg
    // got this from its unguarded flush raising BrokenPipeError before
    // advance; exit 1 mirrors that crash exit.
    let mut delivered = true;
    if !out.is_empty() {
        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        delivered = lock.write_all(out.as_bytes()).is_ok() && lock.flush().is_ok();
    }
    if !delivered {
        eprintln!(
            "error: drain-self: stdout write failed; leaving the cursor so the mail re-surfaces"
        );
        return 1;
    }

    // Inject-before-ack: advance the cursor to the last drained id only after
    // the bodies are out, so a crash re-surfaces rather than drops. One
    // address form at a time: advance, then emit that form's markers, so a
    // later form's cursor failure cannot strand an already-acked form's
    // markers. A skipped duplicate still advances and still gets a marker
    // (the reason distinguishes it). Best-effort throughout: the message is
    // already acked, so an observability gap must never become a failure.
    if !drained.is_empty() || !job_msgs.is_empty() {
        for (form, last_id) in &last_by_form {
            advance_main_cursor(&bus, &msgs, form, last_id);
            for m in to_print
                .iter()
                .chain(skipped.iter())
                .filter(|m| form_of(&form_by_id, m, &handle) == form.as_str())
            {
                let reason = if landed(*m) {
                    "skipped-duplicate"
                } else {
                    "printed"
                };
                emit_drain_marker(
                    &str_field(*m, "id", "?"),
                    &handle,
                    form,
                    &str_field(*m, "from", ""),
                    reason,
                );
            }
        }
        if let (Some(job), true) = (&job_addr, !job_msgs.is_empty()) {
            advance_main_cursor(
                &bus,
                &msgs,
                job,
                &str_field(job_msgs.last().unwrap(), "id", "?"),
            );
            let recipient = job_addr.as_deref().unwrap_or(&handle);
            for m in job_to_print.iter().chain(job_skipped.iter()) {
                let reason = if landed(*m) {
                    "skipped-duplicate"
                } else {
                    "printed"
                };
                emit_drain_marker(
                    &str_field(*m, "id", "?"),
                    recipient,
                    recipient,
                    &str_field(*m, "from", ""),
                    reason,
                );
            }
        }
    }
    0
}

/// The held-job scan: mail addressed to the node this session's manifest
/// names, gated on a live claim the manifest's holder still holds AND a
/// session join that names THIS session. Every rejection is the empty scan.
fn scan_held_job_mail(sid: &str, bus: &Path, msgs: &[Value]) -> (Option<String>, Vec<Value>) {
    let empty = || (None, Vec::new());
    let Ok(cwd) = std::env::current_dir() else {
        return empty();
    };
    let Ok(content) = std::fs::read_to_string(cwd.join(".fno").join("target-state.md")) else {
        return empty();
    };
    let field =
        |name: &str| crate::loopcheck::scan_manifest_field(&content, name).filter(|v| v != "null");
    let Some(key) = field("target_claim_key") else {
        return empty();
    };
    if !key.starts_with("node:") {
        return empty();
    }
    let (state, claim) = crate::claims::status(&key, None);
    if !matches!(
        state,
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
    ) {
        return empty();
    }
    let Some(claim) = claim else {
        return empty();
    };
    let holder = claim.holder;
    let manifest_sid = field("session_id");
    // Manifest join first (the renewal path records the holder explicitly),
    // then the legacy `target-session:<sid>` holder-shape fallback.
    let joined = match field("target_claim_holder") {
        Some(recorded) => {
            let same_key = field("target_claim_key").as_deref() == Some(key.as_str());
            if recorded == holder && same_key {
                manifest_sid.clone()
            } else {
                None
            }
        }
        None => match manifest_sid.as_deref() {
            Some(s) if holder == format!("target-session:{s}") => manifest_sid.clone(),
            _ => None,
        },
    };
    let derived = joined
        .or_else(|| holder.strip_prefix("target-session:").map(str::to_string))
        .filter(|s| !s.is_empty());
    if derived.as_deref() != Some(sid) {
        return empty();
    }
    let got = unread_messages(bus, msgs, &key)
        .into_iter()
        .cloned()
        .collect();
    (Some(key), got)
}

fn form_of<'a>(form_by_id: &'a HashMap<String, String>, m: &Value, handle: &'a str) -> &'a str {
    form_by_id
        .get(&str_field(m, "id", "?"))
        .map(String::as_str)
        .unwrap_or(handle)
}

/// Python: `getattr(m, "origin", None) not in (None, "peer")` gates both the
/// header label and the JSON key; a missing or non-string origin reads as
/// absent.
fn origin_label(m: &Value) -> Option<&str> {
    m.get("origin")
        .and_then(Value::as_str)
        .filter(|o| *o != "peer")
}

fn header_line(m: &Value) -> String {
    let label = match origin_label(m) {
        Some(o) => format!("  origin:{o}"),
        None => String::new(),
    };
    format!(
        "\n--- from {} ({})  id:{}{} ---",
        str_field(m, "from", ""),
        str_field(m, "ts", ""),
        str_field(m, "id", ""),
        label
    )
}

/// One `"key": value` fragment with the Python `json.dumps` default `": "`.
fn pair(key: &str, value: &str) -> String {
    format!(
        "{}: {}",
        serde_json::to_string(key).unwrap_or_default(),
        serde_json::to_string(value).unwrap_or_default()
    )
}

/// Key order id, from, to, kind, ts, body, then `job` for job rows, then a
/// non-peer origin last - the Python dict insertion order, byte-for-byte.
fn json_row(m: &Value, job: Option<&str>) -> String {
    let mut parts = vec![
        pair("id", &str_field(m, "id", "")),
        pair("from", &str_field(m, "from", "")),
        pair("to", &str_field(m, "to", "")),
        pair("kind", &str_field(m, "kind", "")),
        pair("ts", &str_field(m, "ts", "")),
        pair("body", &str_field(m, "body", "")),
    ];
    if let Some(job) = job {
        parts.push(pair("job", job));
    }
    if let Some(origin) = origin_label(m) {
        parts.push(pair("origin", origin));
    }
    format!("{{{}}}", parts.join(", "))
}

fn emit_drain_marker(
    msg_id: &str,
    recipient: &str,
    address_form: &str,
    sender: &str,
    reason: &str,
) {
    let emitter = crate::events::EventEmitter::new(
        crate::paths::AgentsHome::from_env().events_jsonl(),
        "agents",
    );
    let _ = emitter.emit(
        "agent_mail_drained",
        &json!({
            "msg_id": msg_id,
            "recipient": recipient,
            "address_form": address_form,
            "sender": sender,
            "reason": reason,
        }),
    );
}

fn bus_dir(home: &AgentsHome) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| home.root().to_path_buf());
    crate::intel::bus_dir_of(&root)
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
