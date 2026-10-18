use super::super::posture::{harness_family, peer_family, route_provider};
use super::*;

#[test]
fn resolved_optional_bots_default_matches_the_shared_python_default() {
    // One oracle, two readers: this resolver and fno.config's
    // DEFAULT_OPTIONAL_APPS both answer these rows, so a drift on either
    // side fails its own test against the SAME file.
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../cli/tests/config/optional_apps_default.json");
    let text = std::fs::read_to_string(&golden)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", golden.display()));
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let unset = v["unset"].as_array().unwrap();
    let unset: Vec<String> = unset
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    // Unset (None) -> the built-in default.
    let settings = Settings::default();
    assert_eq!(resolved_optional_bots(&settings), unset);
    // An explicit [] is a real opt-out and wins over the default.
    let empty = Settings {
        optional_apps: Some(Vec::new()),
        ..Settings::default()
    };
    assert!(resolved_optional_bots(&empty).is_empty());
    // A partial list EXTENDS the built-ins (round-2 ruling): it never
    // silently drops a honored-if-present login.
    let partial = v["partial_union"].as_array().unwrap();
    let partial: Vec<String> = partial
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    let mine = Settings {
        optional_apps: Some(vec!["my-app".to_string()]),
        ..Settings::default()
    };
    assert_eq!(resolved_optional_bots(&mine), partial);
}

#[test]
fn is_bot_reviewer_known_patterns() {
    assert!(is_bot_reviewer("gemini-code-assist[bot]", &[]));
    assert!(is_bot_reviewer("chatgpt-codex-connector", &[]));
    assert!(is_bot_reviewer("some-bot[bot]", &[]));
    assert!(!is_bot_reviewer("human-reviewer", &[]));
}

#[test]
fn identity_free_peer_uses_local_attestation_not_a_login() {
    let s = Settings {
        github_apps: Some(Vec::new()),
        peers: vec![PeerEntry {
            provider: "gemini".into(),
            model: None,
            identity: None,
        }],
        peer_identity: None,
        ..Default::default()
    };
    assert!(resolved_required_bots_for_author(&s, Some("codex")).is_empty());
    assert_eq!(
        resolved_local_peer_reviewers_for_author(&s, Some("codex")),
        vec![LOCAL_PEER_REVIEWER.to_string()]
    );
}

#[test]
fn identity_free_same_model_peer_is_an_unsatisfiable_local_gate() {
    let s = Settings {
        peers: vec![PeerEntry {
            provider: "codex".into(),
            model: None,
            identity: None,
        }],
        ..Default::default()
    };
    assert_eq!(
        resolved_local_peer_reviewers_for_author(&s, Some("codex")),
        vec![SAME_MODEL_LOCAL_PEER_SENTINEL.to_string()]
    );
}

/// US5: effective model family resolution across bare providers, routes,
/// malformed routes (fall back to provider), and unknown providers (None).
#[test]
fn peer_family_mapping_table() {
    let bare = |p: &str| PeerEntry {
        provider: p.into(),
        model: None,
        identity: None,
    };
    let routed = |p: &str, m: &str| PeerEntry {
        provider: p.into(),
        model: Some(m.into()),
        identity: None,
    };
    // harness_family: names + aliases + case-insensitivity; unknown -> None.
    assert_eq!(harness_family("claude"), Some("anthropic"));
    assert_eq!(harness_family("ANTHROPIC"), Some("anthropic"));
    assert_eq!(harness_family("codex"), Some("openai"));
    assert_eq!(harness_family("gemini"), Some("google"));
    assert_eq!(harness_family("zai"), None);
    // route_provider: exactly two non-empty parts, else None (fall back).
    assert_eq!(route_provider("zai,glm-5.2"), Some("zai"));
    assert_eq!(route_provider(" openai , gpt-5 "), Some("openai"));
    assert_eq!(route_provider("gpt-5"), None); // no comma -> malformed
    assert_eq!(route_provider("zai,"), None); // empty model -> malformed
    assert_eq!(route_provider(",glm"), None); // empty provider -> malformed
    assert_eq!(route_provider("a,b,c"), None); // three parts -> malformed

    // peer_family: bare provider, valid route wins, malformed falls back.
    assert_eq!(peer_family(&bare("codex")), Some("openai"));
    assert_eq!(peer_family(&bare("grok")), None); // unknown -> never matches
    assert_eq!(peer_family(&routed("claude", "zai,glm-5.2")), None); // route wins
    assert_eq!(
        peer_family(&routed("codex", "openai,gpt-5")),
        Some("openai")
    );
    assert_eq!(peer_family(&routed("codex", "gpt-5")), Some("openai")); // malformed -> provider
}

/// AC1-HP: codex author + `peers: [codex]` -> the peer login is replaced by
/// the same-model sentinel so the gate cannot clear.
#[test]
fn same_model_peer_holds_gate() {
    let s = Settings {
        github_apps: Some(Vec::new()),
        peers: vec![PeerEntry {
            provider: "codex".into(),
            model: None,
            identity: None,
        }],
        peer_identity: Some("fno-peer-bot".into()),
        ..Default::default()
    };
    let logins = resolved_required_bots_for_author(&s, Some("codex"));
    assert!(logins.iter().any(|l| l == SAME_MODEL_PEER_SENTINEL));
    assert!(!logins.iter().any(|l| l == "fno-peer-bot"));
}

/// A same-model peer whose identity COLLIDES with a required App login is
/// fail-closed, not exempt (codex peer review on PR #375): the App login is
/// kept (its requirement is not loosened) AND the sentinel is added, so a
/// same-model review under the shared login cannot clear the gate.
#[test]
fn base_app_login_collision_is_fail_closed() {
    let s = Settings {
        github_apps: Some(vec!["fno-peer-bot".into()]),
        peers: vec![PeerEntry {
            provider: "codex".into(),
            model: None,
            identity: None,
        }],
        peer_identity: Some("fno-peer-bot".into()),
        ..Default::default()
    };
    let logins = resolved_required_bots_for_author(&s, Some("codex"));
    assert!(logins.iter().any(|l| l == "fno-peer-bot")); // App requirement kept
    assert!(logins.iter().any(|l| l == SAME_MODEL_PEER_SENTINEL)); // gate held
}

/// A codex/gemini peer's `model` route is NOT honored (only claude transport
/// executes a route; codex/gemini dispatch runs the bare provider). A codex
/// peer with a zai route stays openai-family -> same-model on a codex author,
/// closing the route-bypass codex flagged on PR #375.
#[test]
fn non_claude_route_is_ignored() {
    let routed_codex = PeerEntry {
        provider: "codex".into(),
        model: Some("zai,glm-5.2".into()),
        identity: None,
    };
    assert_eq!(peer_family(&routed_codex), Some("openai"));
    let s = Settings {
        github_apps: Some(Vec::new()),
        peers: vec![routed_codex],
        peer_identity: Some("fno-peer-bot".into()),
        ..Default::default()
    };
    let logins = resolved_required_bots_for_author(&s, Some("codex"));
    assert!(logins.iter().any(|l| l == SAME_MODEL_PEER_SENTINEL));
    assert!(!logins.iter().any(|l| l == "fno-peer-bot"));
}

#[test]
fn login_matches_bot_cases() {
    // Full login, [bot]-suffixed login, and short config names all match.
    assert!(login_matches_bot(
        "chatgpt-codex-connector",
        "chatgpt-codex-connector"
    ));
    assert!(login_matches_bot(
        "chatgpt-codex-connector[bot]",
        "chatgpt-codex-connector"
    ));
    assert!(login_matches_bot("chatgpt-codex-connector", "codex"));
    assert!(login_matches_bot("Gemini-Code-Assist[bot]", "gemini"));
    assert!(!login_matches_bot("alice-the-human", "codex"));
    // Empty config entry must never match every login.
    assert!(!login_matches_bot("anyone", ""));
}
