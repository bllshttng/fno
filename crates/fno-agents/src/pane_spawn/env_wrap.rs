//! The mesh env wrapper (`_mesh_env_pairs` + `_mesh_env_wrapper`,
//! `cli/src/fno/agents/mux_spawn.py:1126/:1301`): the `env(1)` prefix every
//! pane shell launches under. `pane run` transports argv only, so identity,
//! substrate and seed attribution ride the wrapper as assignments and the
//! parent's own identity rides out as `-u` unsets.
//!
//! Scope, recorded: the role/provenance/account legs of the Python pairs are
//! NOT ported here. Every shape that carries them is seam-carved to Python
//! (`_is_role_bearing_spawn`, `_is_provenance_bearing_spawn`) and never
//! reaches an arm. The route overlay IS: the claude arm (row 8, PR B) hands
//! its resolved route env to this wrapper through the `overlay` param, which
//! the pairs append after the claude extras and the unsets answer with the
//! account_env.py scrub floor when the harness is claude.

use base64::Engine as _;

use super::PaneContext;

/// Python `qos_wrap`'s demotion prefix rides OUTSIDE the provider argv and
/// INSIDE the env wrapper (`env ... taskpolicy -c utility -- provider ...`).
/// Re-exported here so the arms compose one wrapped argv.
pub(crate) fn qos_wrap_for(ctx: &PaneContext, argv: Vec<String>) -> Vec<String> {
    crate::spawn_gate::qos_wrap(&ctx.cwd, argv)
}

/// `AMBIENT_IDENTITY_ENV` (`cli/src/fno/harness_identity.py:191`): the names
/// a spawned child never inherits. Each harness re-mints its own identity at
/// startup, so the strip is lossless; an inherited marker is how a codex
/// worker spawned from a claude parent resolves as the wrong harness for
/// life. Order is the Python tuple's order - the goldens pin it byte-equal.
pub const AMBIENT_IDENTITY_ENV: [&str; 19] = [
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDECODE_SESSION_ID",
    "HERMES_SESSION_ID",
    "TARGET_SESSION_ID",
    "CODEX_CI",
    "CODEX_INTERNAL_ORIGINATOR_OVERRIDE",
    "CODEX_SHELL",
    "CODEX_COMPANION_SESSION_ID",
    "CODEX_COMPANION_TRANSCRIPT_PATH",
    "FNO_AGENT_SUBSTRATE",
    "FNO_NODE_REASON",
];

/// The seed provenance keys, set-or-clear as a whole group: a pane worker
/// spawning a child passes its whole environment down, so leaving these
/// behind would hand the child the PARENT's seed and sender.
pub const SEED_PROVENANCE_KEYS: [&str; 6] = [
    "FNO_SEED_PROV_NODE",
    "FNO_SEED_PROV_SEED_B64",
    "FNO_SEED_PROV_FROM",
    "FNO_SEED_PROV_FROM_SESSION",
    "FNO_SEED_PROV_MSG_ID",
    "FNO_SEED_PROV_HARNESS",
];

/// account_env.py's `SCRUB_AUTH_VARS` (account_env.py:72-87): the inherited
/// auth vars a routed claude worker must never keep. The tier-alias remaps
/// (`MODEL_ENV_KEYS`' tail) ride along because endpoint, auth and model are
/// ONE provider route; the tuple order is the Python tuple's order - the
/// goldens pin it byte-equal.
pub const SCRUB_AUTH_VARS: [&str; 10] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "FNO_ROUTE_PROVIDER",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
];

/// `_mesh_env_pairs` for the shapes an arm answers: no role, no provenance,
/// no account overlay (seam-carved), so the pairs reduce to identity,
/// substrate, the pinned session, the claude extras, the caller's route
/// overlay and the seed sidecar. Returns `(unset, pairs)` in the Python
/// construction order.
pub fn mesh_env_pairs(
    name: &str,
    harness: &str,
    session_id: Option<&str>,
    seed_prov: &[(String, String)],
    overlay: &[(String, String)],
) -> (Vec<String>, Vec<String>) {
    let mut pairs = vec![
        format!("FNO_AGENT_SELF={name}"),
        format!("FNO_AGENT_HARNESS={harness}"),
        format!("FNO_HARNESS_NAME={harness}"),
        format!("FNO_AGENT_ROW_PENDING={name}"),
        "FNO_AGENT_SUBSTRATE=pane".to_string(),
    ];
    if let Some(sid) = session_id.filter(|s| !s.trim().is_empty()) {
        pairs.push(format!("FNO_HARNESS_SESSION_ID={}", sid.trim()));
    }
    // Worker parity with the gh delegate: the PATH pair rides only when the
    // delegating wrapper published one (`worker_environment`, PATH differs
    // only under FNO_REAL_GH).
    if let Ok(real) = std::env::var("FNO_REAL_GH") {
        if let Some(dir) = std::path::Path::new(&real).parent() {
            if let Some(bin) = dir.to_str() {
                if let Ok(path) = std::env::var("PATH") {
                    if !path.split(':').any(|p| p == bin) {
                        pairs.push(format!("PATH={}:{}", bin, path));
                    }
                }
            }
        }
    }
    if harness == "claude" {
        // Worker parity: transcripts must persist for resume/adoption.
        pairs.push("CLAUDE_CODE_FORCE_SESSION_PERSISTENCE=1".to_string());
        // Raise the Stop-hook block cap so fno's repeated-block loop is not
        // force-ended at the claude default; an operator-set env wins.
        let cap = std::env::var("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "50".to_string());
        pairs.push(format!("CLAUDE_CODE_STOP_HOOK_BLOCK_CAP={cap}"));
    }
    // The caller's route/account overlay, appended after the claude extras
    // the way Python's composed block appends after them (mux_spawn.py:1242):
    // env(1) is left-to-right last-wins, so the overlay wins over anything an
    // inherited assignment would say.
    pairs.extend(overlay.iter().map(|(k, v)| format!("{k}={v}")));
    let mut unset: Vec<String> = AMBIENT_IDENTITY_ENV
        .iter()
        .flat_map(|name| ["-u".to_string(), name.to_string()])
        .collect();
    unset.extend([
        "-u".to_string(),
        crate::spawn_route::ROUTE_PROVIDER_ENV.to_string(),
    ]);
    // The account_env.py scrub floor, riding with an overlay the way the
    // account/route block rides in Python (mux_spawn.py:1228-1242): claude
    // only, and only when the caller carries an overlay to apply, so a plain
    // pane's unset list stays the golden's. `env -u` on an unset var is a
    // harmless no-op; the tuple order is the Python tuple's order.
    if harness == "claude" && !overlay.is_empty() {
        for key in SCRUB_AUTH_VARS {
            unset.extend(["-u".to_string(), key.to_string()]);
        }
    }
    // Node provenance, set-or-clear (never reached with a value through an
    // arm, so it is always the clear half here).
    for key in [
        "FNO_NODE",
        "FNO_SLUG",
        "FNO_PLAN",
        "FNO_NODE_CLAIM_HOLDER",
        "FNO_WORKTREE_POLICY",
    ] {
        unset.extend(["-u".to_string(), key.to_string()]);
    }
    let resolved: std::collections::HashSet<&str> =
        seed_prov.iter().map(|(k, _)| k.as_str()).collect();
    for key in SEED_PROVENANCE_KEYS {
        if !resolved.contains(key) {
            unset.extend(["-u".to_string(), key.to_string()]);
        }
    }
    pairs.extend(seed_prov.iter().map(|(k, v)| format!("{k}={v}")));
    (unset, pairs)
}

/// `_mesh_env_wrapper`: the pairs as one `env(1)` prefix on the (already
/// qos-wrapped) provider argv.
pub fn mesh_env_wrapper(
    name: &str,
    harness: &str,
    session_id: Option<&str>,
    seed_prov: &[(String, String)],
    overlay: &[(String, String)],
    qos_wrapped: Vec<String>,
) -> Vec<String> {
    let (unset, pairs) = mesh_env_pairs(name, harness, session_id, seed_prov, overlay);
    let mut out = Vec::with_capacity(1 + unset.len() + pairs.len() + qos_wrapped.len());
    out.push("env".to_string());
    out.extend(unset);
    out.extend(pairs);
    out.extend(qos_wrapped);
    out
}

/// `MAX_SEED_BYTES` (`mail/seed_provenance.py:63`).
const MAX_SEED_BYTES: usize = 16 * 1024;

/// `mail.seed_provenance.build_env` (seed_provenance.py:74): the child-env
/// fields carrying this seed's attribution, or empty. Three honest empties:
/// a seed that already carries an envelope (self-attributing), an invoker
/// with no provable harness identity (the operator case), and a seed too
/// long to quote in full. Never refuses a spawn.
pub fn seed_provenance_env(seed: &str) -> Vec<(String, String)> {
    if seed.contains("<fno_mail") {
        return Vec::new();
    }
    let home = crate::paths::AgentsHome::from_env();
    let ident = crate::spawn_context::resolve_self_identity(
        &|name| std::env::var(name).ok(),
        None,
        None,
        &home,
    );
    let Some(from_session) = ident.session_id.filter(|s| !s.is_empty()) else {
        return Vec::new();
    };
    let raw = seed.as_bytes();
    if raw.len() > MAX_SEED_BYTES {
        eprintln!(
            "note: seed is {0} bytes, over the {1}-byte provenance cap; spawning without a \
             provenance sidecar. A sidecar that quotes the seed must quote all of it.",
            raw.len(),
            MAX_SEED_BYTES
        );
        return Vec::new();
    }
    let mut env = vec![
        (
            "FNO_SEED_PROV_SEED_B64".to_string(),
            base64::engine::general_purpose::STANDARD.encode(raw),
        ),
        (
            "FNO_SEED_PROV_FROM".to_string(),
            canonical_handle(&from_session),
        ),
        (
            "FNO_SEED_PROV_FROM_SESSION".to_string(),
            from_session.clone(),
        ),
        ("FNO_SEED_PROV_MSG_ID".to_string(), generate_msg_id()),
    ];
    if let Some(harness) = ident.harness.filter(|h| !h.is_empty()) {
        env.push(("FNO_SEED_PROV_HARNESS".to_string(), harness));
    }
    env
}

/// `harness_identity.canonical_handle` (harness_identity.py:467): the
/// mailbox address is the first eight characters of the session id.
fn canonical_handle(session_id: &str) -> String {
    session_id.chars().take(8).collect()
}

/// `inbox.store.generate_msg_id`: `fmail-` plus 12 hex characters.
fn generate_msg_id() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("OS CSPRNG unavailable");
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("fmail-{hex}")
}

/// `enrich_spawn_payload` (`spawn_payload.py:21`): the one payload entry -
/// first-party brevity guidance appended once to a non-empty seed.
pub fn enrich_spawn_payload(message: &str) -> String {
    const BREVITY_BLOCK: &str = "<fno_relay_compression>\nKeep reports and handoffs at 80 words or fewer unless this payload requires a longer artifact or exact output schema. Think fully; write only the requested result, essential reason, and next action. Drop filler, pleasantries, hedges, repeated context, and articles where clear. Fragments work. Keep technical terms, commands, errors, numbers, units, negation, and code blocks exact. Put long detail in durable artifacts when available; return a path or link.\n</fno_relay_compression>";
    if message.is_empty() || message.contains(BREVITY_BLOCK) {
        return message.to_string();
    }
    format!("{message}\n\n{BREVITY_BLOCK}")
}

/// `is_verb_seed` (`config/_dispatch_verbs.py:58`): position 0 is
/// load-bearing - only a leading command token may be rewritten.
pub fn is_verb_seed(seed: &str) -> bool {
    seed.split_whitespace()
        .next()
        .is_some_and(|first| crate::provider::parse_verb_token(first).is_some())
}

/// `normalize_command`'s slash-surface leg (harness_map.py:537): the
/// plugin-namespace prefix swap only, never re-tokenized. `codex-skill`
/// (the `$fno:verb` swap) and the `refused` surface belong to the codex and
/// gemini arms; a codex-shaped seed reaching this arm keeps its spelling.
/// An internal-slash first token (`/usr/bin/script`) is nobody's footnote
/// verb and passes verbatim, like Python's guard.
pub fn normalize_command(command: &str, harness: &str) -> String {
    let surface_ok = matches!(
        harness,
        "claude" | "agy" | "opencode" | "pi" | "grok" | "cursor-agent" | "footnote"
    );
    if !surface_ok {
        return command.to_string();
    }
    let cmd = command.trim();
    let Some(first_word) = cmd.split_whitespace().next() else {
        return command.to_string();
    };
    let Some((verb, namespaced)) = crate::provider::parse_verb_token(first_word) else {
        return command.to_string();
    };
    let tail = &cmd[first_word.len()..];
    let prefix = if harness == "opencode" { "fno:" } else { "" };
    if namespaced || first_word.starts_with('/') {
        return format!("/{prefix}{verb}{tail}");
    }
    command.to_string()
}
