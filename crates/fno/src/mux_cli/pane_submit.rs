//! Identity-aware pane submission primitives shared by pane send and command.

use super::*;

// Keep these aligned with fno-agents mail_inject: the same PTY paste needs the
// same settle and retry cadence whether it arrived through mail or pane send.
const CR_SETTLE_MS: u64 = 800;
const CR_RESUBMIT_EVERY: u32 = 8;
const SUBMIT_CONFIRM_ATTEMPTS: u32 = 40;
const SUBMIT_CONFIRM_INTERVAL_MS: u64 = 250;

pub(super) fn positive_post_submit_marker(before_cr: &str, after_cr: &str) -> bool {
    !after_cr.trim().is_empty() && after_cr != before_cr
}

pub(super) fn pane_text(sock: &Path, session: &str, pane: u64) -> Result<String, ControlError> {
    match control_roundtrip(
        sock,
        session,
        ControlCommand::PaneRead {
            pane,
            lines: None,
            block: None,
        },
    )? {
        ServerMsg::PaneText { text, .. } => Ok(text),
        ServerMsg::Err { msg, .. } => Err(ControlError::Fatal(msg)),
        other => Err(ControlError::Fatal(format!(
            "unexpected pane read reply while confirming submit: {other:?}"
        ))),
    }
}

pub(super) fn send_pane_bytes(
    sock: &Path,
    session: &str,
    pane: u64,
    bytes: Vec<u8>,
    guarded: bool,
    expected_identity: Option<&str>,
    hold_pass: bool,
) -> Result<(), ControlError> {
    match control_roundtrip(
        sock,
        session,
        ControlCommand::PaneSend {
            pane,
            bytes,
            guarded,
            expected_identity: expected_identity.map(str::to_string),
            hold_pass,
        },
    )? {
        ServerMsg::Ok => Ok(()),
        ServerMsg::Err { code, msg }
            if code == err_code::TARGET_IDENTITY_MISMATCH || code == err_code::TARGET_DND =>
        {
            Err(ControlError::FatalCode { code, msg })
        }
        ServerMsg::Err { msg, .. } => Err(ControlError::Fatal(msg)),
        other => Err(ControlError::Fatal(format!(
            "unexpected pane send reply while submitting: {other:?}"
        ))),
    }
}

pub(super) fn submit_pane(
    sock: &Path,
    session: &str,
    pane: u64,
    bytes: Vec<u8>,
    guarded: bool,
    expected_identity: Option<&str>,
    json: bool,
) -> i32 {
    if let Err(e) = send_pane_bytes(
        sock,
        session,
        pane,
        bytes,
        guarded,
        expected_identity,
        false,
    ) {
        eprintln!("fno mux pane: {e}");
        return match e {
            ControlError::Unanswered(_) => EXIT_CONTROL_UNANSWERED,
            ControlError::Fatal(_) => EXIT_ERROR,
            ControlError::FatalCode { code, .. } if code == err_code::TARGET_IDENTITY_MISMATCH => {
                EXIT_TARGET_IDENTITY_MISMATCH
            }
            ControlError::FatalCode { code, .. } if code == err_code::TARGET_DND => EXIT_TARGET_DND,
            ControlError::FatalCode { .. } => EXIT_ERROR,
        };
    }
    std::thread::sleep(Duration::from_millis(CR_SETTLE_MS));
    let baseline = pane_text(sock, session, pane).ok();
    if let Err(e) = send_pane_bytes(
        sock,
        session,
        pane,
        vec![b'\r'],
        false,
        expected_identity,
        false,
    ) {
        eprintln!("fno mux pane: text delivered, submission unconfirmed: {e}");
        if let ControlError::FatalCode { code, .. } = e {
            if code == err_code::TARGET_IDENTITY_MISMATCH {
                return EXIT_TARGET_IDENTITY_MISMATCH;
            }
            if code == err_code::TARGET_DND {
                return EXIT_TARGET_DND;
            }
        }
        return EXIT_SUBMIT_UNCONFIRMED;
    }
    for attempt in 0..SUBMIT_CONFIRM_ATTEMPTS {
        if let (Some(before), Ok(after)) = (baseline.as_deref(), pane_text(sock, session, pane)) {
            if positive_post_submit_marker(before, &after) {
                if !json {
                    println!("submitted");
                }
                return render_reply(ServerMsg::Ok, json, false, None);
            }
        }
        std::thread::sleep(Duration::from_millis(SUBMIT_CONFIRM_INTERVAL_MS));
        if (attempt + 1) % CR_RESUBMIT_EVERY == 0 {
            let _ = send_pane_bytes(
                sock,
                session,
                pane,
                vec![b'\r'],
                false,
                expected_identity,
                false,
            );
        }
    }
    eprintln!("fno mux pane: text delivered, submission unconfirmed");
    EXIT_SUBMIT_UNCONFIRMED
}

// ---------------------------------------------------------------------------
// Codex composer read-back (pane send --harness codex)
// ---------------------------------------------------------------------------

/// The per-harness composer facts the read-back and the clear path need,
/// read off the canonical `harness_capabilities.toml` row. Absent keys leave
/// the arms off: an undeclared harness keeps the plain screen-changed
/// verdict.
pub(super) struct ComposerCaps {
    queue_affordance_marker: String,
    queued_markers: Vec<String>,
    composer_clear_keys: Vec<u8>,
    composer_empty_marker: String,
}

/// Read the canonical toml row for `harness` and lift the composer fields.
pub(super) fn composer_caps(harness: &str) -> Option<ComposerCaps> {
    let root: toml::Value = toml::from_str(include_str!("../harness_capabilities.toml")).ok()?;
    let row = root.get("harness")?.get(harness)?;
    let str_field = |name: &str| row.get(name)?.as_str().map(str::to_string);
    let byte_list = |name: &str| -> Option<Vec<u8>> {
        Some(
            row.get(name)?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().and_then(|s| s.chars().next()).map(|c| c as u8))
                .collect(),
        )
    };
    let str_list = |name: &str| -> Option<Vec<String>> {
        Some(
            row.get(name)?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        )
    };
    Some(ComposerCaps {
        queue_affordance_marker: str_field("queue_affordance_marker")?,
        queued_markers: str_list("queued_markers")?,
        composer_clear_keys: byte_list("composer_clear_keys")?,
        composer_empty_marker: str_field("composer_empty_marker")?,
    })
}

/// True when `marker` sits within a couple of lines of the line carrying
/// `sig`: the composer and its status render adjacent, and proximity is what
/// separates an affordance from scrollback noise.
fn marker_near_payload(frame: &str, marker: &str, sig: &str) -> bool {
    let lines: Vec<String> = frame
        .lines()
        .map(|ln| ln.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    for (i, ln) in lines.iter().enumerate() {
        if !ln.to_lowercase().contains(&marker.to_lowercase()) {
            continue;
        }
        let near = lines.iter().skip(i.saturating_sub(2)).take(5);
        if near.clone().any(|other| other.contains(sig)) {
            return true;
        }
    }
    false
}

/// Codex resends its queue affordance at most twice before the read-back
/// calls the envelope stranded (dispatch.py `_CODEX_POST_SUBMIT_RESENDS`).
const SUBMIT_RESENDS: u32 = 2;

/// A paste that has not rendered after this many polls gets its one
/// deciding key (dispatch.py `_CODEX_ABSORB_POLLS`), leaving the rest of
/// the confirm budget for the resend to land and settle.
const SUBMIT_ABSORB_POLLS: u32 = 12;

/// One observation of the post-submit frame -> the next read-back move.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Readback {
    /// A `queued` marker beside the payload: delivered to the one queue slot.
    Queued,
    /// The queue affordance beside the payload: one Tab queues it.
    SendTab,
    /// No affordance and not yet hedged: one deciding key (the payload's
    /// first byte); a no-op when the paste already landed.
    Hedge,
    /// The payload left the composer: delivered.
    Settled,
    /// Payload still resident after the deciding key: the honest
    /// bytes-written verdict, never a claimed submit.
    Submitted,
    /// Still rendering: poll again.
    Poll,
}

/// The ported read-back state machine (dispatch.py
/// `_codex_post_submit_outcome`): the queue affordance beside the payload
/// names the composer as still holding it; not-yet-rendered polls come
/// first; an affordance-less frame gets one deciding key.
pub(crate) fn readback_step(
    frame: &str,
    sig: &str,
    affordance: &str,
    queued_markers: &[String],
    hedged: bool,
    seen_payload: bool,
    polls: u32,
    absorb_polls: u32,
) -> Readback {
    if frame.contains(sig) {
        if queued_markers
            .iter()
            .any(|m| marker_near_payload(frame, m, sig))
        {
            return Readback::Queued;
        }
        if marker_near_payload(frame, affordance, sig) {
            return Readback::SendTab;
        }
        return if hedged {
            Readback::Submitted
        } else {
            Readback::Hedge
        };
    }
    if seen_payload {
        return Readback::Settled;
    }
    if polls >= absorb_polls {
        return Readback::Hedge;
    }
    Readback::Poll
}

/// The payload signature the read-back looks for: the first non-empty
/// payload line, whitespace-folded and lowercased - the shape the composer
/// renders it with.
fn payload_signature(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .map(str::trim)
        .find(|ln| !ln.is_empty())
        .map(|ln| ln.split_whitespace().collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
        .to_lowercase()
}

/// Send the harness's composer-clear keys and verify the empty marker.
/// A miss reads unconfirmed, never cleared.
fn clear_composer(
    sock: &Path,
    session: &str,
    pane: u64,
    expected_identity: Option<&str>,
    caps: &ComposerCaps,
    harness: &str,
) -> i32 {
    if caps.composer_clear_keys.is_empty() {
        eprintln!(
            "fno mux pane: --clear-composer unsupported on {harness}: no composer_clear_keys declared"
        );
        return EXIT_ERROR;
    }
    if let Err(e) = send_pane_bytes(
        sock,
        session,
        pane,
        caps.composer_clear_keys.clone(),
        false,
        expected_identity,
        false,
    ) {
        eprintln!("fno mux pane: clear keys refused: {e}");
        return EXIT_ERROR;
    }
    std::thread::sleep(Duration::from_millis(CR_SETTLE_MS));
    match pane_text(sock, session, pane) {
        Ok(frame) if frame.contains(&caps.composer_empty_marker) => EXIT_OK,
        _ => {
            eprintln!("fno mux pane: composer clear unconfirmed (empty marker absent)");
            EXIT_SUBMIT_UNCONFIRMED
        }
    }
}

/// The `pane send --submit` path with its receipts and tails: clear (when
/// asked), submit, confirm, audit, review-invocation record. Harness rows
/// without the read-back keys keep the plain screen-changed confirm.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_submit(
    sock: &Path,
    session: &str,
    pane: u64,
    bytes: Vec<u8>,
    guarded: bool,
    expected_identity: Option<&str>,
    json: bool,
    harness: Option<&str>,
    clear_composer_wanted: bool,
    audit: Option<PaneSendAudit>,
    review_command: Option<(String, String)>,
) -> i32 {
    let caps = harness.and_then(composer_caps);
    if harness.is_some() && caps.is_none() {
        eprintln!(
            "fno mux pane: no composer read-back keys declared for {}; plain confirm",
            harness.unwrap_or("unknown")
        );
    }
    if clear_composer_wanted {
        let Some((caps, name)) = caps.as_ref().zip(harness) else {
            eprintln!(
                "fno mux pane: --clear-composer unsupported on {}: no declared composer keys",
                harness.unwrap_or("unknown")
            );
            return EXIT_ERROR;
        };
        if clear_composer(sock, session, pane, expected_identity, caps, name) != EXIT_OK {
            return EXIT_SUBMIT_UNCONFIRMED;
        }
        if !json {
            println!("composer clear");
        }
    }
    let code = match caps.as_ref() {
        None => submit_pane(sock, session, pane, bytes, guarded, expected_identity, json),
        Some(caps) => submit_readback(
            sock,
            session,
            pane,
            bytes,
            guarded,
            expected_identity,
            json,
            caps,
        ),
    };
    if let Some(audit) = audit {
        audit.emit(session, code);
    }
    if let Some(review_command) = review_command {
        let receipt = match code {
            EXIT_OK => "submitted",
            EXIT_SUBMIT_UNCONFIRMED => "text delivered, submission unconfirmed",
            _ => "text delivery failed",
        };
        append_review_invocation(
            session,
            Some(review_command),
            true,
            code == EXIT_OK,
            receipt,
        );
    }
    code
}

/// The read-back confirm: the same paste + settle + submit-key prefix, then
/// the affordance loop instead of the plain screen-changed poll.
#[allow(clippy::too_many_arguments)]
fn submit_readback(
    sock: &Path,
    session: &str,
    pane: u64,
    bytes: Vec<u8>,
    guarded: bool,
    expected_identity: Option<&str>,
    json: bool,
    caps: &ComposerCaps,
) -> i32 {
    let sig = payload_signature(&bytes);
    let hedge_key = bytes.first().copied().unwrap_or(b'\r');
    if let Err(e) = send_pane_bytes(
        sock,
        session,
        pane,
        bytes,
        guarded,
        expected_identity,
        false,
    ) {
        eprintln!("fno mux pane: {e}");
        return match e {
            ControlError::Unanswered(_) => EXIT_CONTROL_UNANSWERED,
            ControlError::Fatal(_) => EXIT_ERROR,
            ControlError::FatalCode { code, .. } if code == err_code::TARGET_IDENTITY_MISMATCH => {
                EXIT_TARGET_IDENTITY_MISMATCH
            }
            ControlError::FatalCode { code, .. } if code == err_code::TARGET_DND => EXIT_TARGET_DND,
            ControlError::FatalCode { .. } => EXIT_ERROR,
        };
    }
    std::thread::sleep(Duration::from_millis(CR_SETTLE_MS));
    let _ = pane_text(sock, session, pane);
    if let Err(e) = send_pane_bytes(
        sock,
        session,
        pane,
        vec![b'\r'],
        false,
        expected_identity,
        false,
    ) {
        eprintln!("fno mux pane: text delivered, submission unconfirmed: {e}");
        return EXIT_SUBMIT_UNCONFIRMED;
    }
    let mut resends = 0u32;
    let mut hedged = false;
    let mut seen_payload = false;
    for polls in 1..=SUBMIT_CONFIRM_ATTEMPTS {
        std::thread::sleep(Duration::from_millis(SUBMIT_CONFIRM_INTERVAL_MS));
        let Ok(frame) = pane_text(sock, session, pane) else {
            // An unreadable frame keeps the bytes-written verdict.
            if !json {
                println!("submitted");
            }
            return render_reply(ServerMsg::Ok, json, false, None);
        };
        if frame.to_lowercase().contains(&sig) {
            seen_payload = true;
        }
        let step = readback_step(
            &frame,
            &sig,
            &caps.queue_affordance_marker,
            &caps.queued_markers,
            hedged,
            seen_payload,
            polls,
            SUBMIT_ABSORB_POLLS,
        );
        match step {
            Readback::Queued => {
                if !json {
                    println!("queued (one queue slot)");
                }
                return render_reply(ServerMsg::Ok, json, false, None);
            }
            Readback::Settled | Readback::Submitted => {
                if !json {
                    println!("submitted");
                }
                return render_reply(ServerMsg::Ok, json, false, None);
            }
            Readback::SendTab | Readback::Hedge => {
                if resends >= SUBMIT_RESENDS {
                    eprintln!(
                        "fno mux pane: envelope still composer-resident after {resends} recovery keys"
                    );
                    return EXIT_SUBMIT_UNCONFIRMED;
                }
                let key: Vec<u8> = if matches!(step, Readback::SendTab) {
                    vec![b'\t']
                } else {
                    vec![hedge_key]
                };
                let _ = send_pane_bytes(sock, session, pane, key, false, expected_identity, false);
                if matches!(step, Readback::Hedge) {
                    hedged = true;
                }
                resends += 1;
            }
            Readback::Poll => {}
        }
    }
    eprintln!("fno mux pane: text delivered, submission unconfirmed");
    EXIT_SUBMIT_UNCONFIRMED
}

#[cfg(test)]
mod tests {
    use super::*;

    const AFFORDANCE: &str = "tab to queue message";

    fn frame(payload_line: &str, affordance: bool) -> String {
        let mut s = String::from("| codex\n");
        s.push_str(payload_line);
        s.push('\n');
        if affordance {
            s.push_str("  tab to queue message\n");
        }
        s.push_str("Ready\n");
        s
    }

    // The affordance counts only beside the payload: near, it fires the Tab
    // recovery; the same words in scrollback are noise and the deciding
    // key runs instead.
    #[test]
    fn affordance_proximity_decides_tab_or_hedge() {
        let near = frame("> $fno:target x-1", true);
        assert_eq!(
            readback_step(
                &near,
                "$fno:target",
                AFFORDANCE,
                &["queued".into()],
                false,
                true,
                1,
                12
            ),
            Readback::SendTab
        );
        let mut far = String::from("tab to queue message\n\n\n\n\n\n\n");
        far.push_str(&frame("> hello", false));
        assert_eq!(
            readback_step(&far, "hello", AFFORDANCE, &[], false, true, 1, 12),
            Readback::Hedge
        );
    }

    #[test]
    fn queued_marker_reports_queued() {
        let mut f = frame("> review it", false);
        f.push_str("queued review it\n");
        assert_eq!(
            readback_step(
                &f,
                "review it",
                AFFORDANCE,
                &["queued".into()],
                false,
                true,
                1,
                12
            ),
            Readback::Queued
        );
    }

    // The hedge lifecycle: the deciding key fires once after the absorb
    // polls, and its second look reads the honest bytes-written verdict.
    #[test]
    fn hedge_fires_once_then_reads_honestly() {
        let f = frame("> hello", false);
        assert_eq!(
            readback_step(&f, "hello", AFFORDANCE, &[], true, true, 3, 12),
            Readback::Submitted
        );
        // A paste that never rendered: quiet frame, payload absent.
        let quiet = frame("Ready", false);
        assert_eq!(
            readback_step(&quiet, "hello", AFFORDANCE, &[], false, false, 12, 12),
            Readback::Hedge
        );
        assert_eq!(
            readback_step(&quiet, "hello", AFFORDANCE, &[], false, false, 11, 12),
            Readback::Poll
        );
    }

    #[test]
    fn payload_gone_is_settled() {
        let f = frame("Ready", false);
        assert_eq!(
            readback_step(&f, "hello", AFFORDANCE, &[], false, true, 2, 12),
            Readback::Settled
        );
    }

    #[test]
    fn signature_folds_whitespace_and_case() {
        assert_eq!(payload_signature(b"\n  Hello   World \n"), "hello world");
        assert_eq!(payload_signature(b""), "");
    }
}
