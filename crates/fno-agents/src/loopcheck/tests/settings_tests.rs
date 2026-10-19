use super::*;

#[test]
fn live_optional_apps_optout_is_honored() {
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = std::env::temp_dir().join(format!("fno-loop-optional-{}", std::process::id()));
    std::env::set_var("FNO_CLAIMS_ROOT", &root);
    let key = "config-optout:review.optional_apps";
    let acquired = crate::claims::acquire(
        key,
        "session-a",
        crate::claims::AcquireOpts {
            ttl_ms: Some(300_000),
            events_dir: Some(root.clone()),
            ..Default::default()
        },
    );
    assert!(matches!(
        acquired,
        crate::claims::AcquireOutcome::Acquired(_)
    ));

    let settings = parse_settings("[review]\noptional_apps = []\n");

    assert_eq!(settings.optional_apps, Some(Vec::new()));
    let _ = crate::claims::release(key, "session-a", None, Some(&root));
    match prior {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
}

#[test]
fn unbacked_self_review_optout_defaults_to_obligation_on() {
    let _guard = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let prior = std::env::var_os("FNO_CLAIMS_ROOT");
    let root = std::env::temp_dir().join(format!("fno-loop-optout-{}", std::process::id()));
    std::env::set_var("FNO_CLAIMS_ROOT", &root);

    let settings = parse_settings("[review]\nself_review_required = false\n");

    assert_eq!(settings.self_review_required, None);
    match prior {
        Some(value) => std::env::set_var("FNO_CLAIMS_ROOT", value),
        None => std::env::remove_var("FNO_CLAIMS_ROOT"),
    }
}

#[test]
fn github_approval_satisfies_parses_lax_bool_spellings_and_defaults_on() {
    // Absent -> None -> unwrap_or(true), the default-ON direction; the
    // pydantic string spellings parse the same way on every lax-bool
    // review leaf, so one config cannot load true on one gate and false
    // on the other.
    let absent = parse_settings("[review]\n");
    assert_eq!(absent.github_approval_satisfies, None);
    assert_eq!(absent.github_approval_satisfies.unwrap_or(true), true);

    let spelled = parse_settings("[review]\ngithub_approval_satisfies = \"yes\"\n");
    assert_eq!(spelled.github_approval_satisfies, Some(true));

    let off = parse_settings("[review]\ngithub_approval_satisfies = false\n");
    assert_eq!(off.github_approval_satisfies, Some(false));
}

#[test]
fn parse_manifest_harness_session_id_null_sentinel_is_none() {
    // init writes `harness_session_id: ${_HARNESS_SESSION:-null}`, so an
    // unresolvable session lands as the literal "null" and an empty value as
    // "". Both must read as None or a real attester compared against
    // Some("null") mislabels a self-attestation as other_session.
    for raw in ["null", ""] {
        let content = format!("---\nsession_id: abc\nharness_session_id: {raw}\n---\n");
        let m = parse_manifest(&content).unwrap();
        assert_eq!(
            m.harness_session_id, None,
            "harness_session_id: {raw:?} must parse as None"
        );
    }
    // A real id parses through unchanged.
    let m = parse_manifest(
        "---\nsession_id: abc\nharness_session_id: 3abddea3-ad19-481f-b0c1-af19043c95fe\n---\n",
    )
    .unwrap();
    assert_eq!(
        m.harness_session_id.as_deref(),
        Some("3abddea3-ad19-481f-b0c1-af19043c95fe")
    );
}

#[test]
fn scan_manifest_field_reads_claim_fields_after_frontmatter() {
    // x-aaaa regression: `fno do target init` APPENDS the node-claim fields
    // AFTER the closing `---`, so the frontmatter-bounded parse_manifest must
    // NOT be relied on for them - the whole-file scanner is what drives
    // renewal. Mirrors init's real manifest shape.
    let content = "---\nsession_id: s1\nattended: false\n---\n\
                       Immutable session manifest.\n\
                       target_claim_key: \"node:x-aaaa\"\n\
                       target_claim_holder: \"target-session:s1\"\n\
                       target_claim_ttl: \"2h\"\n";
    // parse_manifest (frontmatter-bounded) never sees the appended fields.
    let m = parse_manifest(content).unwrap();
    assert_eq!(m.session_id.as_deref(), Some("s1"));
    // The whole-file scanner does.
    assert_eq!(
        scan_manifest_field(content, "target_claim_key").as_deref(),
        Some("node:x-aaaa")
    );
    assert_eq!(
        scan_manifest_field(content, "target_claim_holder").as_deref(),
        Some("target-session:s1")
    );
    assert_eq!(
        scan_manifest_field(content, "target_claim_ttl")
            .as_deref()
            .and_then(crate::claims::parse_ttl_ms),
        Some(7_200_000)
    );
    assert_eq!(scan_manifest_field(content, "nonexistent_field"), None);
}

#[test]
fn parse_manifest_legacy_complete() {
    let content = "---\nsession_id: s\ncreated_at: 2026-06-05T00:00:00Z\nstatus: COMPLETE\n---\n";
    let m = parse_manifest(content).unwrap();
    assert_eq!(m.legacy_status.as_deref(), Some("COMPLETE"));
}

#[test]
fn parse_manifest_strips_quotes() {
    // gemini MEDIUM on #447: quoted YAML values must parse identically.
    let content = "---\nsession_id: \"s-quoted\"\ncreated_at: '2026-06-05T00:00:00Z'\n---\n";
    let m = parse_manifest(content).unwrap();
    assert_eq!(m.session_id.as_deref(), Some("s-quoted"));
    assert_eq!(m.created_at.as_deref(), Some("2026-06-05T00:00:00Z"));
}

#[test]
fn parse_manifest_attended_default_true() {
    let content = "---\nsession_id: s\ncreated_at: 2026-06-05T00:00:00Z\n---\n";
    let m = parse_manifest(content).unwrap();
    assert!(m.attended, "attended should default to true when absent");
}

#[test]
fn parse_settings_nested_budget() {
    let cfg = "[budget.attended]\nwall_clock_cap_minutes = 90\ncost_cap_usd = 10.0\n\n[budget.unattended]\nwall_clock_cap_minutes = 60\ncost_cap_usd = 5.0\n";
    let s = parse_settings(cfg);
    assert_eq!(s.attended_wall_cap_minutes, Some(Ok(90)));
    assert_eq!(s.attended_cost_cap_usd, Some(Ok(10.0)));
    assert_eq!(s.unattended_wall_cap_minutes, Some(Ok(60)));
    assert_eq!(s.unattended_cost_cap_usd, Some(Ok(5.0)));
}

#[test]
fn session_cost_from_ledger_sums_session_only() {
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("l.json");
    std::fs::write(&ledger, format!("{}\n", r#"{"entries":[{"session_id":"a","cost_usd":1.0},{"session_id":"b","cost_usd":0.5},{"session_id":"a","cost_usd":0.25}]}"#)).unwrap();
    let cost = session_cost_from_ledger(&ledger, "a");
    assert!((cost - 1.25).abs() < 0.001, "expected 1.25, got {cost}");
    // The bare-array spelling no writer emits reads as no history.
    let bare = tmp.path().join("bare.json");
    std::fs::write(&bare, format!("{}\n", r#"[{"session_id":"a","cost_usd":1.0},{"session_id":"b","cost_usd":0.5},{"session_id":"a","cost_usd":0.25}]"#)).unwrap();
    let cost = session_cost_from_ledger(&bare, "a");
    assert_eq!(cost, 0.0);
}

#[test]
fn parse_manifest_malformed_cost_cap_fail_closed() {
    // Fix 2: a present but unparseable cost cap must be Err (fail-closed)
    let content =
        "---\nsession_id: s\ncreated_at: 2026-06-05T00:00:00Z\nbudget_cost_cap_usd: 5.OO\n---\n";
    let m = parse_manifest(content).unwrap();
    assert!(
        matches!(m.budget_cost_cap_usd, Some(Err(_))),
        "malformed cost cap must be Some(Err(...))"
    );
}

#[test]
fn parse_settings_required_bots_inline_empty_is_declared_empty() {
    // The explicit [] form is the ONLY way to declare the no-review-gate
    // path (US3, locked decision 2).
    let cfg = "[review]\nrequired_bots = []\n";
    let s = parse_settings(cfg);
    assert_eq!(s.required_bots, Some(Vec::new()));
}

/// A bare scalar `required_bots = "gemini"` GATES on that one login (parity
/// with peers + Python), rather than failing OPEN to no-gate on a
/// bracket-less typo (codex P1 on #205).
#[test]
fn parse_settings_required_bots_scalar_is_singleton() {
    let cfg = "[review]\nrequired_bots = \"gemini\"\n";
    let s = parse_settings(cfg);
    assert_eq!(s.required_bots, Some(vec!["gemini".to_string()]));
    // github_apps behaves identically.
    let g = parse_settings("[review]\ngithub_apps = \"chatgpt-codex-connector\"\n");
    assert_eq!(
        g.github_apps,
        Some(vec!["chatgpt-codex-connector".to_string()])
    );
}

/// An ABSENT required_bots key resolves to the default (no gate), and a
/// following block still parses.
#[test]
fn parse_settings_absent_required_bots_defaults() {
    let cfg = "[review]\ngithub_apps = []\n\n[ci]\ndeclared_none = true\n";
    let s = parse_settings(cfg);
    assert_eq!(
        s.required_bots, None,
        "absent key resolves to the no-gate default"
    );
    assert!(s.ci_declared_none, "following blocks still parse");
}

#[test]
fn parse_settings_malformed_fails_closed_not_zeroed() {
    // A malformed config.toml must NOT silently zero the gate (the old
    // fail-open); it fails CLOSED with an unsatisfiable sentinel so the ship
    // gate blocks visibly. Here: an unclosed table header.
    let cfg = "[review\nrequired_bots = []\n";
    assert!(
        parse_settings_result(cfg).is_err(),
        "malformed TOML must be a parse error"
    );
    let s = parse_settings(cfg);
    assert_eq!(
        s.required_bots,
        Some(vec![UNPARSEABLE_SETTINGS_SENTINEL.to_string()]),
        "a malformed file must fail closed, not zero the gate"
    );
    // The sentinel can never be satisfied by a real bot login.
    assert!(!login_matches_bot(
        "chatgpt-codex-connector",
        UNPARSEABLE_SETTINGS_SENTINEL
    ));
}

#[test]
fn parse_settings_reviewers_forms() {
    // Inline, block-under, key-aligned (PyYAML), bare scalar all parse; a
    // leading '/' is normalized off (parity with the Python validator).
    let inline = parse_settings("[review]\nreviewers = [\"sigma\", \"/code-review\"]\n");
    assert_eq!(
        inline.reviewers,
        vec!["sigma".to_string(), "code-review".to_string()]
    );
    let block = parse_settings("[review]\nreviewers = [\n  \"sigma\",\n]\n");
    assert_eq!(block.reviewers, vec!["sigma".to_string()]);
    let scalar = parse_settings("[review]\nreviewers = \"/code-review\"\n");
    assert_eq!(scalar.reviewers, vec!["code-review".to_string()]);
    let absent = parse_settings("[review]\ngithub_apps = []\n");
    assert!(absent.reviewers.is_empty());
}

#[test]
fn parse_settings_reviewers_malformed_mapping_fails_closed() {
    // A `{...}` mapping value must NOT drop to no-gate (Python raises here);
    // Rust stores an unsatisfiable sentinel so the gate stays active but can
    // never clear (codex peer review P1).
    let s = parse_settings("[review]\nreviewers = {a = \"b\"}\n");
    assert_eq!(s.reviewers, vec![MALFORMED_REVIEWERS_SENTINEL.to_string()]);
    let tmp = tempfile::tempdir().unwrap();
    let p = write_events(tmp.path(), &[]);
    assert!(
        !reviewers_all_attested(&p, &s.reviewers, "h"),
        "a malformed-reviewers sentinel must never be satisfiable"
    );
}

#[test]
fn parse_settings_reviewers_seq_with_nonscalar_fails_closed() {
    // gemini medium: a non-scalar item INSIDE the reviewers list (Python
    // raises on it) must fail CLOSED with the sentinel, not silently drop
    // the entry and gate on the survivors.
    let bad = parse_settings("[review]\nreviewers = [\"sigma\", {a = \"b\"}]\n");
    assert_eq!(
        bad.reviewers,
        vec![MALFORMED_REVIEWERS_SENTINEL.to_string()]
    );
    // A clean all-scalar list still parses normally.
    let ok = parse_settings("[review]\nreviewers = [\"sigma\", \"declare\"]\n");
    assert_eq!(
        ok.reviewers,
        vec!["sigma".to_string(), "declare".to_string()]
    );
}

#[test]
fn parse_settings_peers_single_mapping_is_one_peer() {
    // codex peer review P1: a single top-level table for peers (what
    // Python's coerce_peers wraps as [dict]) must parse as ONE peer, not be
    // silently dropped - dropping it is a fail-open on a configured peer gate.
    let block =
        parse_settings("[review]\npeers = {provider = \"codex\", identity = \"fno-codex-bot\"}\n");
    assert_eq!(block.peers.len(), 1, "table peers must be one peer");
    assert_eq!(block.peers[0].provider, "codex");
    assert_eq!(block.peers[0].identity.as_deref(), Some("fno-codex-bot"));
    // A dotted-table form parses identically.
    let dotted =
        parse_settings("[review.peers]\nprovider = \"gemini\"\nidentity = \"fno-gemini-bot\"\n");
    assert_eq!(dotted.peers.len(), 1);
    assert_eq!(dotted.peers[0].provider, "gemini");
    assert_eq!(dotted.peers[0].identity.as_deref(), Some("fno-gemini-bot"));
}

#[test]
fn parse_settings_peers_bare_scalar_is_one_provider() {
    // `peers = "codex"` (scalar) matches Python's coerce_peers -> one peer,
    // NOT a silent drop (which would fail open + diverge from Python).
    let cfg = "[review]\npeers = \"codex\"\npeer_identity = \"fno-peer-bot\"\n";
    let s = parse_settings(cfg);
    assert_eq!(s.peers.len(), 1);
    assert_eq!(s.peers[0].provider, "codex");
    // The gate then resolves on the shared identity (fail-closed if unset).
    assert_eq!(resolved_required_bots(&s), vec!["fno-peer-bot".to_string()]);
}

#[test]
fn parse_settings_peers_map_identity_before_provider() {
    // The map parser is order-agnostic (gemini HIGH on #205): `identity`
    // before `provider` must still resolve both fields.
    let cfg = "[review]\npeers = [{identity = \"fno-codex-bot\", provider = \"codex\"}, {provider = \"gemini\", identity = \"fno-gemini-bot\"}]\n";
    let s = parse_settings(cfg);
    assert_eq!(s.peers.len(), 2);
    assert_eq!(s.peers[0].provider, "codex");
    assert_eq!(s.peers[0].identity.as_deref(), Some("fno-codex-bot"));
    assert_eq!(s.peers[1].provider, "gemini");
    assert_eq!(s.peers[1].identity.as_deref(), Some("fno-gemini-bot"));
}
