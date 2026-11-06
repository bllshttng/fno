// Identity-tag tests, moved verbatim from claims.rs (shrink-only budget).
use super::*;

// ---- harness tag (x-3e70) ---------------------------------------------

// ---- session_id tag ----------------------------------------------------

// AC1: resolve_identity resolves session_id and harness from one call, so
// make_claim can never stamp a record naming a session of a different
// harness than the one it names.
#[test]
fn resolve_identity_resolves_session_and_harness_together() {
    let get = |key: &str| match key {
        "FNO_HARNESS_NAME" => Some("claude".to_string()),
        "FNO_HARNESS_SESSION_ID" => Some("abc123".to_string()),
        _ => None,
    };
    assert_eq!(
        resolve_identity_from(get),
        (Some("abc123".to_string()), Some("claude".to_string()))
    );
}

#[test]
fn canonical_partial_and_vendor_contradictions_refuse() {
    assert_eq!(
        resolve_harness_from(|key| {
            (key == "FNO_HARNESS_SESSION_ID").then(|| "session-only".to_string())
        }),
        None
    );
    assert_eq!(
        resolve_harness_from(|key| match key {
            "FNO_HARNESS_NAME" => Some("claude".to_string()),
            "FNO_HARNESS_SESSION_ID" => Some("claude-session".to_string()),
            "CODEX_THREAD_ID" => Some("codex-session".to_string()),
            _ => None,
        }),
        None
    );
}

#[test]
fn scrub_identity_env_removes_every_identity_name_and_keeps_routing() {
    // A host booted for the caller (supervisor, serve, daemon) outlives it:
    // it may keep PATH and the auth/router vars, never the caller's identity.
    let mut command = std::process::Command::new("sh");
    command.env("CODEX_THREAD_ID", "parent");
    for key in TERMINAL_HOST_ENV_KEYS {
        command.env(key, "user-session");
    }
    command.env("PATH", "/usr/bin:/bin");
    // The family is pinned literally: every test above iterates the const,
    // so only this line catches a narrowed or renamed key set.
    assert_eq!(
        TERMINAL_HOST_ENV_KEYS,
        ["REX_SERVER", "REX_SESSION", "REX_BLOCK"]
    );
    scrub_identity_env(&mut command);
    let envs: Vec<(String, Option<String>)> = command
        .get_envs()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.map(|value| value.to_string_lossy().into_owned()),
            )
        })
        .collect();

    for name in AMBIENT_IDENTITY_NAMES {
        assert!(
            envs.iter()
                .any(|(key, value)| key.as_str() == *name && value.is_none()),
            "{name} was not scrubbed"
        );
    }
    for key in TERMINAL_HOST_ENV_KEYS {
        assert!(
            envs.iter()
                .any(|(name, value)| name.as_str() == key && value.is_none()),
            "{key} was not scrubbed"
        );
    }
    assert_eq!(
        envs.iter()
            .find(|(key, _)| key.as_str() == "PATH")
            .and_then(|(_, value)| value.as_deref()),
        Some("/usr/bin:/bin")
    );
}

#[test]
fn ambient_parent_edge_resolves_id_harness_and_refuses_mixing() {
    // Single claude marker: the VALUE is the parent session id.
    let claude = |k: &str| match k {
        "CLAUDE_CODE_SESSION_ID" => Some("7420e8f7-eeba".to_string()),
        _ => None,
    };
    assert_eq!(
        ambient_parent_edge_from(claude),
        (
            Some("7420e8f7-eeba".to_string()),
            Some("claude".to_string()),
            ambient_parent_edge_from(|_| None).2
        )
    );
    // Within the codex family one id under both names resolves to it.
    let codex = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("t-1".to_string()),
        "CODEX_SESSION_ID" => Some("t-1".to_string()),
        _ => None,
    };
    assert_eq!(
        ambient_parent_edge_from(codex),
        (
            Some("t-1".to_string()),
            Some("codex".to_string()),
            ambient_parent_edge_from(|_| None).2
        )
    );
    // Two DIFFERENT ids of the codex family are the measured thread shape,
    // not a conflict: CODEX_SESSION_ID names the ROOT session while
    // CODEX_THREAD_ID names this thread, so the thread id is the parent
    // edge - the same measured rule resolve_owned_identity applies to a
    // proven codex family.
    let codex_disagree = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("t-1".to_string()),
        "CODEX_SESSION_ID" => Some("legacy-1".to_string()),
        _ => None,
    };
    assert_eq!(
        ambient_parent_edge_from(codex_disagree),
        (
            Some("t-1".to_string()),
            Some("codex".to_string()),
            ambient_parent_edge_from(|_| None).2
        )
    );
    // A canonical stamp that refused is never overridden by the exception:
    // a claude session's own stamp with an inherited codex marker stays
    // refused (the leak the disagreement rule was written for).
    let stamped = |k: &str| match k {
        "FNO_HARNESS_NAME" => Some("claude".to_string()),
        "FNO_HARNESS_SESSION_ID" => Some("11111111-1111-4111-8111-111111111111".to_string()),
        "CODEX_THREAD_ID" => Some("cx".to_string()),
        _ => None,
    };
    let (s, h, _) = ambient_parent_edge_from(stamped);
    assert_eq!((s, h), (None, None));
    // Two DISAGREEING families attribute NOTHING: no laundered lineage.
    let mixed = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("cx".to_string()),
        "CLAUDE_CODE_SESSION_ID" => Some("cl".to_string()),
        _ => None,
    };
    let (s, h, c) = ambient_parent_edge_from(mixed);
    assert_eq!((s, h), (None, None));
    assert!(c.is_some(), "cwd is captured even when identity is refused");
    // No markers: identity absent, cwd still present, never a panic.
    let (s, h, _) = ambient_parent_edge_from(|_| None);
    assert_eq!((s, h), (None, None));
}
