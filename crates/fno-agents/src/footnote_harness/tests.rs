use super::*;

/// A Claude.ai login never reaches api.anthropic.com; a route env does. A
/// spawn whose footnote binary is missing refuses with the install command
/// and leaves no registry row. A spawn hands the binary a v2 spec that
/// carries the node and the permission mode.
#[test]
fn the_launcher_refuses_a_login_endpoint_and_a_missing_binary() {
    let cwd = Path::new("/");
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
    };
    let err = endpoint::resolve_endpoint(
        cwd,
        &env(&[
            ("ANTHROPIC_BASE_URL", "https://api.anthropic.com"),
            ("ANTHROPIC_AUTH_TOKEN", "oauth"),
        ]),
        None,
    )
    .unwrap_err();
    assert!(err.contains("Claude Code lane"), "{err}");
    let ep = endpoint::resolve_endpoint(
        cwd,
        &env(&[
            ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
            ("ANTHROPIC_AUTH_TOKEN", "k"),
            ("FNO_ROUTE_PROVIDER", "zai"),
        ]),
        None,
    )
    .unwrap();
    assert_eq!(
        (ep.wire.as_str(), ep.provider_id.as_deref(), ep.bearer),
        ("anthropic", Some("zai"), true)
    );
    assert_eq!(ep.route, "env");
    // A model id names its provider as a prefix. A prefix with no provider
    // record is part of the id. An env endpoint keeps the id whole unless its
    // own stamp is the prefix. No endpoint at all names the legal form.
    assert_eq!(
        endpoint::split_model(cwd, &env(&[]), "zai/glm-x"),
        (Some("zai".to_string()), "glm-x")
    );
    for whole in ["glm-test", "no-such-provider/model-x", "zai/"] {
        assert_eq!(endpoint::split_model(cwd, &env(&[]), whole), (None, whole));
    }
    let proxy = env(&[("ANTHROPIC_BASE_URL", "https://proxy.example")]);
    assert_eq!(
        endpoint::split_model(cwd, &proxy, "zai/glm-x"),
        (None, "zai/glm-x")
    );
    let stamped = env(&[
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("FNO_ROUTE_PROVIDER", "zai"),
    ]);
    assert_eq!(
        endpoint::split_model(cwd, &stamped, "zai/glm-x"),
        (None, "glm-x")
    );
    let err = endpoint::resolve_endpoint(cwd, &env(&[]), None).unwrap_err();
    assert!(err.contains("-m <provider>/<model>"), "{err}");
    assert!(!err.contains("-P"), "{err}");

    let tmp = tempfile::tempdir().unwrap();
    // The guard holds the crate's env lock for the rest of the test, so the
    // three launch vars below cannot race another env-reading test.
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let home = AgentsHome::from_env();
    let vars = [
        ("FNO_FOOTNOTE_BIN", "/nonexistent/footnote"),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("ZAI_API_KEY", "zk"),
    ];
    let saved: Vec<_> = vars
        .iter()
        .map(|(k, _)| (*k, std::env::var_os(k)))
        .collect();
    for (k, v) in vars {
        std::env::set_var(k, v);
    }
    let o = dispatch_once(
        &home,
        "fx-missing",
        "hi",
        "tester",
        tmp.path(),
        Some("glm-test"),
        None,
        &serde_json::json!({}),
    );
    let missing = o;
    // A stand-in binary keeps the spec it read and refuses, as a stale
    // binary would.
    let seen = tmp.path().join("spec.json");
    let bin = crate::write_exec_stub(
        tmp.path(),
        "footnote",
        &format!("#!/bin/sh\ncat > '{}'\nexit 2\n", seen.display()),
    );
    std::env::set_var("FNO_FOOTNOTE_BIN", &bin);
    // With no env endpoint, the model id's prefix picks the provider record.
    std::env::remove_var("ANTHROPIC_BASE_URL");
    let refused = dispatch_once(
        &home,
        "fx-plan",
        "hi",
        "tester",
        tmp.path(),
        Some("zai/glm-test"),
        None,
        &serde_json::json!({"node": "x-1", "permission_mode": "plan"}),
    );
    for (k, v) in saved {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
    assert_eq!(missing.exit_code, 2, "{}", missing.stderr);
    assert!(
        missing
            .stderr
            .contains("cargo install --locked --path ~/code/footnote/fnh"),
        "{}",
        missing.stderr
    );
    assert_eq!(refused.exit_code, 2, "{}", refused.stderr);
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
    assert_eq!(spec["v"], 2);
    assert_eq!(spec["node"], "x-1");
    assert_eq!(spec["permission_mode"], "plan");
    assert_eq!(spec["model"], "glm-test");
    assert_eq!(spec["endpoint"]["provider_id"], "zai");
    assert_eq!(spec["endpoint"]["route"], "config");
    let reg = load_registry(&home.registry_json()).unwrap();
    assert!(reg.find("fx-missing").is_none());
    assert!(reg.find("fx-plan").is_none());
}
