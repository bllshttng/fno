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
fn resolve_harness_single_family_wins_disagreement_is_unknown() {
    // Two DISAGREEING families are ambiguous: precedence must not pick codex
    // and tag the claim with a harness this process cannot prove it owns.
    let both = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("cx".to_string()),
        "CLAUDE_CODE_SESSION_ID" => Some("cl".to_string()),
        _ => None,
    };
    assert_eq!(resolve_harness_from(both).as_deref(), None);
    // Two markers of ONE family carrying the SAME id -> that family.
    let same_family = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("cx".to_string()),
        "CODEX_SESSION_ID" => Some("CX".to_string()),
        _ => None,
    };
    assert_eq!(resolve_harness_from(same_family).as_deref(), Some("codex"));
    // Two DIFFERENT ids of ONE family disagree: attribute nothing, never
    // the table-first value (x-0992) - without proof either id could be
    // the stranger's.
    let same_family_disagree = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("cx".to_string()),
        "CODEX_SESSION_ID" => Some("cx2".to_string()),
        _ => None,
    };
    assert_eq!(resolve_harness_from(same_family_disagree).as_deref(), None);
    // A blank higher-precedence marker is UNSET; a lower real one still wins.
    let blank_hi = |k: &str| match k {
        "CODEX_THREAD_ID" => Some("   ".to_string()),
        "CLAUDE_CODE_SESSION_ID" => Some("cl".to_string()),
        _ => None,
    };
    assert_eq!(resolve_harness_from(blank_hi).as_deref(), Some("claude"));
    assert_eq!(
        resolve_harness_from(|k| (k == "OPENCODE_SESSION_ID").then(|| "ses_1".to_string()))
            .as_deref(),
        Some("opencode")
    );
    // No markers -> None (unknown), never a panic.
    assert_eq!(resolve_harness_from(|_| None), None);
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
fn command_stamp_scrubs_all_identity_and_sets_only_bound_session() {
    let mut command = std::process::Command::new("sh");
    command.env("CODEX_THREAD_ID", "parent");
    command.env("FNO_AGENT_SELF", "parent");
    command.env("FNO_AGENT_SUBSTRATE", "pane");
    command.env(FNO_HARNESS_NAME, "parent");
    stamp_command_env(
        &mut command,
        Some("child"),
        "claude",
        Some(" 11111111-1111-4111-8111-111111111111 "),
    );
    let values: std::collections::HashMap<_, _> = command
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();

    assert_eq!(
        values.get("FNO_AGENT_SELF").map(String::as_str),
        Some("child")
    );
    assert_eq!(
        values.get("FNO_AGENT_HARNESS").map(String::as_str),
        Some("claude")
    );
    assert_eq!(
        values.get(FNO_HARNESS_NAME).map(String::as_str),
        Some("claude")
    );
    assert_eq!(
        values.get(FNO_HARNESS_SESSION_ID).map(String::as_str),
        Some("11111111-1111-4111-8111-111111111111")
    );
    assert!(!values.contains_key("CODEX_THREAD_ID"));
    // These ask lanes stamp no substrate of their own, so the parent's must
    // go: a child that keeps `pane` reads as attended for life (x-be78).
    assert!(!values.contains_key("FNO_AGENT_SUBSTRATE"));

    let mut unowned = std::process::Command::new("sh");
    unowned.env("FNO_AGENT_SELF", "parent");
    stamp_command_env(&mut unowned, None, "codex", None);
    let values: std::collections::HashMap<_, _> = unowned
        .get_envs()
        .filter_map(|(key, value)| {
            value.map(|value| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
        })
        .collect();
    assert!(!values.contains_key("FNO_AGENT_SELF"));
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
