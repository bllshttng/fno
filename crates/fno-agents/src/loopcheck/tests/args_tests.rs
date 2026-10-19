use super::*;

/// AC5-ERR: required flags validated in parse_args, which returns Err.
#[test]
fn parse_args_missing_required_flags_err() {
    let no_state: Vec<String> = vec![
        "loop-check".into(),
        "--transcript".into(),
        "/t".into(),
        "--cwd".into(),
        "/c".into(),
    ];
    assert_eq!(
        parse_args(&no_state).unwrap_err(),
        "--state is required".to_string()
    );

    let no_transcript: Vec<String> = vec!["loop-check".into(), "--state".into(), "/s".into()];
    assert_eq!(
        parse_args(&no_transcript).unwrap_err(),
        "--transcript is required".to_string()
    );

    let no_cwd: Vec<String> = vec![
        "loop-check".into(),
        "--state".into(),
        "/s".into(),
        "--transcript".into(),
        "/t".into(),
    ];
    assert_eq!(
        parse_args(&no_cwd).unwrap_err(),
        "--cwd is required".to_string()
    );
}

#[test]
fn budget_flat_key_enforces_cost_cap_ab41b13d9d() {
    // Prove the flat budget_cap key enforces as cost cap for BOTH attended and
    // unattended - this is the fold-in test.
    let settings_cfg = "budget_cap = 0.10\n";
    let settings = parse_settings(settings_cfg);
    assert_eq!(settings.flat_budget_cap, Some(Ok(0.10)));
    // No nested blocks configured
    assert!(settings.attended_cost_cap_usd.is_none());
    assert!(settings.unattended_cost_cap_usd.is_none());
    // The budget resolver picks flat_budget_cap as cost cap fallback
    // for both attended=true and attended=false (tested in check_budget)

    let manifest_att = Manifest {
        session_id: Some("s1".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        attended: true,
        ..Default::default()
    };
    let manifest_unatt = Manifest {
        session_id: Some("s1".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        attended: false,
        ..Default::default()
    };

    // Ledger with cost > 0.10
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"session_id":"s1","cost_usd":0.50}]}"#,
    )
    .unwrap();

    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();

    assert_eq!(
        check_budget(&manifest_att, &settings, &now, &ledger),
        Some(BudgetTrip::Cost {
            cap_usd: Some(0.10),
            spent_usd: 0.50
        }),
        "flat budget_cap must enforce for attended"
    );
    assert_eq!(
        check_budget(&manifest_unatt, &settings, &now, &ledger),
        Some(BudgetTrip::Cost {
            cap_usd: Some(0.10),
            spent_usd: 0.50
        }),
        "flat budget_cap must enforce for unattended"
    );
}

#[test]
fn check_budget_malformed_cost_cap_trips_budget() {
    // Fix 2: malformed cap in manifest -> Budget termination (fail-closed)
    let m = Manifest {
        session_id: Some("s".into()),
        created_at: Some("2026-06-05T00:00:00Z".into()),
        budget_cost_cap_usd: Some(Err("5.OO".into())),
        ..Default::default()
    };
    let s = Settings::default();
    let now: DateTime<Utc> = "2026-06-05T01:00:00Z".parse().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    std::fs::write(
        &ledger,
        r#"{"entries":[{"session_id":"s","cost_usd":0.0}]}"#,
    )
    .unwrap();
    assert_eq!(
        check_budget(&m, &s, &now, &ledger),
        Some(BudgetTrip::Cost {
            cap_usd: None,
            spent_usd: 0.0
        }),
        "malformed cost cap must fail closed"
    );
}
