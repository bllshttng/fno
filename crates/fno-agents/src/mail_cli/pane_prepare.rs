//! `fno-agents mail pane-prepare`: gate one pane payload and frame it as
//! attributed agent mail, printing the exact bytes to type on stdout.
//!
//! Ported from the Python leaf (`fno agents mail pane-prepare`). The mux's
//! pane-send crossing in the `fno` front binary is the production caller; the
//! golden suite under `tests/golden/mail_pane_prepare/` pins every refusal
//! text byte-exact against the Python leg that used to own them.

use std::collections::HashSet;
use std::io::{Read as _, Write as _};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::backlog::style_check;
use crate::harness_capabilities::HarnessContract;
use crate::identity::canonical_handle;
use crate::mail_bus::mint_msg_id;
use crate::mail_control_drain::is_control;
use crate::mail_envelope::render_at;
use crate::mail_header::{classify, Framing};
use crate::mail_inject::{cap_env_int, contains_fno_mail_tag_anywhere, enforce_body_cap};
use crate::manifest::evaluate_screen_json;
use crate::paths::AgentsHome;
use crate::spawn_context::{resolve_self_identity, OwnedDisposition};
use crate::state::{load_registry, Registry, RegistryEntry};

/// Frames the identity receipt reads with, matching the Python gate's
/// `GATE_FRAME_LINES`.
const GATE_FRAME_LINES: &str = "40";

/// How long one nested `mux pane read` may take. The Python renderer bounded
/// this hop at 30s (`_MUX_SUBPROCESS_TIMEOUT_S`); the parent keeps its own
/// 45s envelope budget, which is only honest while this stays the smaller
/// number.
const MUX_READ_TIMEOUT_S: u64 = 30;

fn fail(code: i32, message: &str) -> i32 {
    eprintln!("{message}");
    code
}

/// One captured `mux` subprocess, translating spawn failure and timeout the
/// way the Python `_run_mux` helper did, so refusal texts stay identical. The
/// binary is `FNO_BIN` when set (the parent crossing always sets it, because
/// `fno` itself may not be on PATH) and bare `fno` otherwise.
fn run_mux_captured(args: &[String], timeout_s: u64) -> Result<Output, String> {
    let bin = std::env::var("FNO_BIN").unwrap_or_else(|_| "fno".to_string());
    let mut command = Command::new(&bin);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!(
                "the '{bin}' binary was not found on PATH; the pane substrate is hosted by the fno mux (set FNO_BIN or install fno)"
            ));
        }
        Err(err) => return Err(format!("could not run '{bin}': {err}")),
    };
    // Both pipes drain on their own threads for the whole wait: a refusal
    // answer larger than the pipe buffer must not deadlock against a parent
    // still polling `try_wait`.
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stdout_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr_pipe.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + Duration::from_secs(timeout_s);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(err) => return Err(format!("could not run '{bin}': {err}")),
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let head = args.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
            return Err(format!(
                "fno mux did not answer within {timeout_s}s ({head}...)"
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

/// The `(stderr or stdout or "").strip() or "no detail"` shape the Python
/// gate used to fill its "failed" refusals.
fn run_detail(output: &Output) -> String {
    let raw = if output.stderr.is_empty() {
        &output.stdout
    } else {
        &output.stderr
    };
    let text = String::from_utf8_lossy(raw).trim().to_string();
    if text.is_empty() {
        "no detail".to_string()
    } else {
        text
    }
}

/// Python's `UnicodeDecodeError` text, rebuilt from `Utf8Error` plus the raw
/// bytes, so the refusal names the same byte, position, and phrase on both
/// legs. A byte failing to continue a started sequence is a "continuation"
/// complaint; anything else is a bad start.
fn python_utf8_error(raw: &[u8], err: std::str::Utf8Error) -> String {
    let pos = err.valid_up_to();
    let (byte, reason) = match err.error_len() {
        None => (raw[pos], "unexpected end of data".to_string()),
        Some(_) if pos > 0 && (0xC2..=0xF4).contains(&raw[pos - 1]) => {
            (raw[pos], "invalid continuation byte".to_string())
        }
        Some(_) => (raw[pos], "invalid start byte".to_string()),
    };
    format!("'utf-8' codec can't decode byte 0x{byte:02x} in position {pos}: {reason}")
}

/// The registry row occupying `session:pane`, mirroring `_pane_entry`: an
/// unreadable registry resolves to no row, duplicate rows collapse on
/// `(name, identity)`, and anything that is not exactly one distinct
/// identity resolves to `None`, which the caller refuses on rather than
/// attributing to a guess.
fn pane_entry<'a>(registry: &'a Registry, session: &str, pane: u64) -> Option<&'a RegistryEntry> {
    let mut matches: Vec<&RegistryEntry> = Vec::new();
    let mut seen: HashSet<(String, Option<String>)> = HashSet::new();
    for row in &registry.entries {
        let Some(mux) = &row.mux else { continue };
        if mux.session != session || mux.pane_id != pane {
            continue;
        }
        let identity = row
            .fno_id
            .as_deref()
            .or(row.harness_session_id.as_deref())
            .or(row.session_id.as_deref())
            .map(str::to_string);
        if seen.insert((row.name.clone(), identity)) {
            matches.push(row);
        }
    }
    if matches.len() == 1 {
        matches.pop()
    } else {
        None
    }
}

/// The undeclared-harness note prints once per harness per process, matching
/// the Python `_UNDECLARED_NOTED` module state.
fn note_undeclared_once(harness: &str, pane: u64) {
    static NOTED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut noted = match NOTED.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if noted.iter().any(|seen| seen == harness) {
        return;
    }
    noted.push(harness.to_string());
    eprintln!(
        "note: pane {pane} hosts undeclared harness '{harness}'; no prompt model exists for it, so sends to it are not prompt-gated"
    );
}

fn unavailable_refusal(error: &str) -> String {
    format!(
        "prompt detector unavailable ({error}); refusing to type blind. \
         An instrument that never ran is not an idle pane. The message still \
         reaches the durable bus; --raw types it at the pane unattributed. \
         A source checkout hits this because the detector resolves an \
         INSTALLED fno-agents and skips the cargo dev target."
    )
}

/// The identity receipt: the pane frame read as JSON while pinning that the
/// pane still hosts the identity the registry named a moment ago. `Ok`
/// carries the frame text; `Err` is the refusal, in the Python gate's order.
fn identity_receipt_frame(
    session: &str,
    pane: u64,
    expected_name: &str,
    expected_fno_id: &str,
) -> Result<String, String> {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "read".into(),
        "--server".into(),
        session.to_string(),
        pane.to_string(),
        "--lines".into(),
        GATE_FRAME_LINES.into(),
        "--json".into(),
    ];
    let output = run_mux_captured(&args, MUX_READ_TIMEOUT_S)
        .map_err(|err| format!("pane {pane} identity receipt unreadable ({err})"))?;
    if !output.status.success() {
        return Err(format!(
            "pane {pane} identity receipt failed ({})",
            run_detail(&output)
        ));
    }
    // Python's json.loads error shape, not serde's Display: the detail text is
    // part of the frozen refusal, and the parity normalize translates the
    // "Expecting value: line L column C (char N)" family. char is Python's
    // 0-based offset; a receipt is single-line, so column-1 recovers it.
    let payload: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        format!(
            "pane {pane} identity receipt is invalid JSON (Expecting value: \
                 line {} column {} (char {}))",
            err.line(),
            err.column(),
            err.column().saturating_sub(1),
        )
    })?;
    if !payload.is_object() || payload.get("pane_id").and_then(Value::as_u64) != Some(pane) {
        return Err(format!("pane {pane} identity receipt names the wrong pane"));
    }
    let actual_name = payload.get("pane_name").and_then(Value::as_str);
    let actual_fno_id = payload.get("registry_fno_id").and_then(Value::as_str);
    // The uuid is the identity; the label may lag a rename.
    if actual_fno_id != Some(expected_fno_id) {
        return Err(format!(
            "pane {pane} identity mismatch: addressed {expected_name} ({expected_fno_id}) pane hosts {} ({})",
            actual_name.unwrap_or("<unknown>"),
            actual_fno_id.unwrap_or("<unknown>"),
        ));
    }
    payload
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("pane {pane} identity receipt has invalid text"))
}

/// The plain frame read backing the gate when the registry could not pin an
/// identity: an unattributed pane is not proof the pane is idle, so an
/// unreadable frame still refuses.
fn plain_frame(session: &str, pane: u64) -> Result<String, String> {
    let args: Vec<String> = vec![
        "mux".into(),
        "pane".into(),
        "read".into(),
        "--server".into(),
        session.to_string(),
        pane.to_string(),
        "--lines".into(),
        GATE_FRAME_LINES.into(),
    ];
    let output = run_mux_captured(&args, MUX_READ_TIMEOUT_S)
        .map_err(|err| format!("pane {pane} frame unreadable ({err}); refusing to type blind"))?;
    if !output.status.success() {
        return Err(format!(
            "pane {pane} read failed ({}); refusing to type blind",
            run_detail(&output)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// The pane's manifest verdict as one state word, for callers that gate on
/// busy or blocked and act otherwise: `working`/`blocked` when a rule
/// matched, `unknown` when the frame matched no rule, `Err` when the pane
/// or the prompt model is unreadable.
pub(crate) fn screen_state(session: &str, pane: u64, harness: &str) -> Result<String, String> {
    let frame = plain_frame(session, pane)?;
    let verdict = evaluate_screen_json(harness, &frame)?;
    if verdict.get("matched").and_then(Value::as_bool) == Some(true) {
        // A rule that holds its state update (the transcript pager) leaves
        // the pane unjudgeable: the caller must not type into it.
        if verdict.get("skip_state_update").and_then(Value::as_bool) == Some(true) {
            return Err("pane state is held by a skip_state_update rule".to_string());
        }
        Ok(verdict
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    } else {
        Ok("unknown".to_string())
    }
}

/// The prompt gate, in the Python `prompt_refusal` order: the identity
/// receipt when the registry pinned one, then the declared-harness check (an
/// undeclared harness has no prompt model by definition, so the gate skips
/// with a note instead of refusing the only delivery mechanism such a
/// harness has), then the plain frame read when no receipt already fetched
/// one, then the packaged prompt model in-process. `Some` is the refusal.
fn prompt_refusal(
    session: &str,
    pane: u64,
    harness: &str,
    expected_name: Option<&str>,
    expected_fno_id: Option<&str>,
) -> Option<String> {
    let receipt_text = match (expected_name, expected_fno_id) {
        (Some(name), Some(fno_id)) => match identity_receipt_frame(session, pane, name, fno_id) {
            Ok(text) => Some(text),
            Err(refusal) => return Some(refusal),
        },
        _ => None,
    };
    let contract = match HarnessContract::packaged() {
        Ok(contract) => contract,
        Err(err) => return Some(unavailable_refusal(&err.to_string())),
    };
    if contract.capabilities(harness).is_err() {
        note_undeclared_once(harness, pane);
        return None;
    }
    let frame = match receipt_text {
        Some(text) => text,
        None => match plain_frame(session, pane) {
            Ok(text) => text,
            Err(refusal) => return Some(refusal),
        },
    };
    let verdict = match evaluate_screen_json(harness, &frame) {
        Ok(verdict) => verdict,
        Err(err) => return Some(unavailable_refusal(&err)),
    };
    if verdict.get("matched").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    // `answerable` carries the extracted answer payload (a non-empty object),
    // never a bool; the Python oracle reads its truthiness.
    let answerable = verdict
        .get("answerable")
        .and_then(Value::as_object)
        .is_some_and(|options| !options.is_empty());
    let blocked = verdict.get("state").and_then(Value::as_str) == Some("blocked");
    if !answerable && !blocked {
        return None;
    }
    let rule = verdict
        .get("rule_id")
        .and_then(Value::as_str)
        .filter(|rule| !rule.is_empty())
        .unwrap_or("unnamed rule");
    let what = if answerable {
        "an option prompt"
    } else {
        "a blocking prompt"
    };
    Some(format!(
        "pane {pane} is showing {what} ({rule}); a submit would dismiss this payload and select the highlighted default. Answer the prompt with --raw, or wait for it to clear."
    ))
}

pub fn run(args: &[String]) -> i32 {
    let mut session_id_flag: Option<String> = None;
    let mut legacy_session: Option<String> = None;
    let mut pane_flag: Option<u64> = None;
    let mut harness_flag: Option<String> = None;
    let mut allow_reason: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--session-id" => session_id_flag = it.next().cloned(),
            "--session" => legacy_session = it.next().cloned(),
            "--pane" => {
                let Some(value) = it.next() else {
                    return fail(2, "error: --pane wants an integer");
                };
                match value.parse::<u64>() {
                    Ok(pane) => pane_flag = Some(pane),
                    Err(_) => return fail(2, "error: --pane wants an integer"),
                }
            }
            "--harness" => harness_flag = it.next().cloned(),
            "--style-exception" => allow_reason = it.next().cloned(),
            other => {
                return fail(2, &format!("error: unknown argument {other:?}"));
            }
        }
    }
    let session = match (session_id_flag, legacy_session) {
        (Some(_), Some(_)) => {
            return fail(
                2,
                "error: pass either --session-id or --session (deprecated), not both",
            );
        }
        (Some(session), None) => session,
        (None, Some(session)) => {
            eprintln!(
                "warning: --session is deprecated; use --session-id instead. The alias will be removed in a future release."
            );
            session
        }
        (None, None) => return fail(2, "error: --session-id is required"),
    };
    let Some(pane) = pane_flag else {
        return fail(2, "error: --pane is required");
    };

    // Read BYTES and decode once, explicitly: the caller pipes a payload it
    // never re-encodes, and a non-UTF-8 byte must surface as the bad input it
    // is rather than as a renderer fault.
    let mut raw = Vec::new();
    if std::io::stdin().read_to_end(&mut raw).is_err() {
        return fail(3, "error: could not read the pane payload from stdin");
    }
    let body = match String::from_utf8(raw) {
        Ok(body) => body,
        Err(err) => {
            let detail = python_utf8_error(err.as_bytes(), err.utf8_error());
            return fail(
                3,
                &format!(
                    "error: pane payload is not valid UTF-8 ({detail}); the envelope quotes text and cannot frame arbitrary bytes."
                ),
            );
        }
    };
    // The UNTRUSTED boundary: the wrap passthrough below is a PREFIX test, so
    // a handcrafted tag anywhere in the body would be typed at a pane and
    // read as peer mail from whoever it names. The Python leaf checked the
    // tag ANYWHERE, not just as a prefix; so does this.
    if contains_fno_mail_tag_anywhere(&body) {
        return fail(
            1,
            "error: mail body contains an <fno_mail> tag. The envelope frames peer mail; a body cannot contain one.",
        );
    }
    // BOTH attribution containers: `_already_wrapped` passes the
    // cross-session container too, so refusing only the envelope tag would
    // let a handcrafted attributed turn ride the passthrough.
    let lowered = body.to_lowercase();
    if lowered.contains("<cross-session-message") || lowered.contains("</cross-session-message") {
        return fail(
            2,
            "error: body carries a <cross-session-message> container. That tag marks a peer's attributed turn, and a body cannot supply its own. Send it as mail, or use --raw to type it as keystrokes.",
        );
    }
    // The mail commands' two body gates, at the one choke point every non-raw
    // pane send passes through: a sender refused by mail must not deliver the
    // identical prose here instead.
    if let Some(code) = enforce_body_cap(
        body.len(),
        cap_env_int("FNO_MAIL_BODY_WARN", 3000),
        cap_env_int("FNO_MAIL_BODY_REFUSE", 5000),
    ) {
        return code;
    }
    if std::env::var("FNO_STYLE_ENFORCE").ok().as_deref() != Some("0")
        && !body.is_empty()
        && allow_reason
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
        && !is_control(&body)
    {
        let violations = style_check::check(&body, "mail", None);
        if !violations.is_empty() {
            eprintln!("{}", style_check::format_violations(&violations, "mail"));
            return 1;
        }
    }

    // ONE registry snapshot drives everything downstream: the envelope's
    // `to`, the identity the prompt gate pins, and the recipient handle.
    // Resolving the recipient here and re-resolving it in the gate was a
    // TOCTOU on the Python leg; the pins make a reassigned pane refuse
    // instead of rendering an envelope to the old occupant.
    let home = AgentsHome::from_env();
    let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
    let self_session = match ident.disposition {
        OwnedDisposition::Single
        | OwnedDisposition::Proven
        | OwnedDisposition::AsCanonical
        | OwnedDisposition::SpawnRecord => ident.session_id.clone(),
        _ => None,
    };
    let self_harness = ident.harness.clone();
    let sender = crate::system_sender::stamp_or_unknown(None, || {
        self_session.as_deref().map(canonical_handle)
    });

    let registry_path = AgentsHome::shared_registry_json();
    let registry = load_registry(&registry_path).ok();
    let entry = registry
        .as_ref()
        .and_then(|r| pane_entry(r, &session, pane));
    let resolved = match harness_flag.or_else(|| entry.and_then(|e| e.harness.clone())) {
        Some(resolved) => resolved,
        None => {
            return fail(
                3,
                &format!(
                    "pane send refused: pane {pane} hosts no registered agent, so there is no peer to attribute this to. Use --raw to type keystrokes at it."
                ),
            );
        }
    };
    let to = entry.and_then(|e| {
        e.harness_session_id
            .as_deref()
            .or(e.session_id.as_deref())
            .map(canonical_handle)
    });
    let expected_name = entry.map(|e| e.name.as_str());
    let expected_fno_id = entry.and_then(|e| {
        e.fno_id
            .as_deref()
            .or(e.harness_session_id.as_deref())
            .or(e.session_id.as_deref())
    });

    let already_wrapped = classify(&body) != Framing::Bare;
    if !already_wrapped && body.trim().is_empty() {
        // Hoisted ABOVE the gate and the wrap so the advice can name the
        // target harness: a bare CR does not submit every harness's
        // composer. The posture answers "enter" for an undeclared harness,
        // so this stays the ordinary attribution refusal rather than a
        // capability-table raise - mail by pane-send stays reachable.
        let unsupported = HarnessContract::packaged().ok().is_some_and(|contract| {
            contract
                .capabilities(&resolved)
                .ok()
                .is_some_and(|caps| caps.submit_keys == ["unsupported"])
        });
        if unsupported {
            return fail(
                3,
                &format!(
                    "pane send refused: empty payload: there is nothing to attribute, and {resolved} has no programmable submit key. Type the payload with --raw and submit it in that pane's own UI."
                ),
            );
        }
        return fail(
            3,
            "pane send refused: empty payload: there is nothing to attribute. Use --raw --submit for a bare submit keystroke; its 'submitted' marker or exit 22 reports whether the pane took it.",
        );
    }

    if let Some(refusal) = prompt_refusal(&session, pane, &resolved, expected_name, expected_fno_id)
    {
        return fail(3, &format!("pane send refused: {refusal}"));
    }

    let rendered = if already_wrapped {
        body
    } else {
        // The collision-safe reply address rides the typed envelope too: a
        // pane drive is exactly the message a recipient most needs to answer.
        let mut payload = json!({
            "mode": "wrap",
            "body": body,
            "from": sender,
            "id": mint_msg_id(),
        });
        if let Some(harness) = &self_harness {
            payload["harness"] = json!(harness);
        }
        if let Some(to) = &to {
            payload["to"] = json!(to);
        }
        if let Some(session) = &self_session {
            payload["from_session"] = json!(session);
        }
        match render_at(&payload, &registry_path) {
            Ok(framed) => framed,
            Err(err) => return fail(3, &format!("pane send refused: {err}")),
        }
    };
    // The control lane's budget reservation (the Python leaf's
    // `_reserve_budget`, fed from the control-body reserve path) is the one
    // piece of the Python command NOT ported: it is control-lane bookkeeping,
    // not rendering, and it moves with the control drain port. A pane send
    // through this leaf currently charges nothing.
    print!("{rendered}");
    let _ = std::io::stdout().flush();
    0
}
