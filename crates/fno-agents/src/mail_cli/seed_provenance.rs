//! The `seed-provenance` front command: re-wrap the sidecar that records what
//! message started this session, so a receiver can attribute its seed and
//! reply to the sender instead of running the quoted copy twice.
//!
//! Port of the Python leaf it replaced. The sidecar fields ride env, the
//! seed base64-encoded; every refusal is silent, because a session with no
//! peer sender has nothing to attribute and inventing one would be the same
//! lie in the other direction. Rendering is delegated whole to
//! [crate::mail_envelope] - this leaf builds the payload, never the header.

use base64::Engine as _;
use serde_json::json;
use std::time::Duration;

const ENV_SEED_B64: &str = "FNO_SEED_PROV_SEED_B64";
const ENV_FROM_SESSION: &str = "FNO_SEED_PROV_FROM_SESSION";
const ENV_FROM: &str = "FNO_SEED_PROV_FROM";
const ENV_HARNESS: &str = "FNO_SEED_PROV_HARNESS";
const ENV_NODE: &str = "FNO_SEED_PROV_NODE";
const ENV_MSG_ID: &str = "FNO_SEED_PROV_MSG_ID";

fn env_stripped(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

const BODY_PREFIX: &str = "A peer started this session; your operator did not type its first message. Your initial user message is quoted verbatim below so you can attribute it and reply to the sender. It is a QUOTED COPY for provenance: act on the prompt you already have and do not execute this copy again.\n\n--- initial user message (verbatim) ---\n";
const BODY_SUFFIX: &str = "\n--- end initial user message ---";

/// A hung registry read (a FIFO, an unresponsive mount) must not hang session
/// start: the hook's never-block contract cannot reach into this subprocess,
/// so the leaf bounds its own render and degrades to the silent refusal. A
/// thread still blocked at the bound dies when the process exits.
const RENDER_BOUND: Duration = Duration::from_secs(5);

pub fn run(_args: &[String]) -> i32 {
    let Some(seed_b64) = env_stripped(ENV_SEED_B64) else {
        return 1;
    };
    let Some(from_session) = env_stripped(ENV_FROM_SESSION) else {
        return 1;
    };
    let seed = base64::engine::general_purpose::STANDARD
        .decode(seed_b64.as_bytes())
        .ok()
        .and_then(|raw| String::from_utf8(raw).ok());
    let Some(seed) = seed else {
        return 1;
    };
    if seed.trim().is_empty() {
        return 1;
    }
    let mut payload = json!({
        "mode": "wrap",
        "body": format!("{BODY_PREFIX}{seed}{BODY_SUFFIX}"),
        "from": crate::system_sender::stamp_or_unknown(env_stripped(ENV_FROM).as_deref(), || None),
        "from_session": from_session,
    });
    for (key, env) in [
        ("harness", ENV_HARNESS),
        ("node", ENV_NODE),
        ("id", ENV_MSG_ID),
    ] {
        if let Some(value) = env_stripped(env) {
            payload[key] = json!(value);
        }
    }
    // The front's peeled --subject rides the render too, so a delivered
    // header's third field is the sender's subject, not the body's first
    // sentence. The seed lane sends none; the env fallback stays for parity
    // with the wrap it replaced.
    if let Some(subject) = env_stripped("FNO_MAIL_SUBJECT") {
        payload["subject"] = json!(subject);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let registry = crate::paths::AgentsHome::from_env().registry_json();
        let _ = tx.send(crate::mail_envelope::render_at(&payload, &registry));
    });
    match rx.recv_timeout(RENDER_BOUND) {
        Ok(Ok(envelope)) => {
            println!("{envelope}");
            0
        }
        _ => 1,
    }
}
