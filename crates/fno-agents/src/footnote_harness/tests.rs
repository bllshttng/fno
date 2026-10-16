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
    // A resumed row names its provider, and that record outranks whatever
    // endpoint the asking session exports.
    let ep = endpoint::resolve_endpoint(tmp.path(), &proxy, Some("zai")).unwrap();
    assert_eq!(
        (ep.route.as_str(), ep.provider_id.as_deref()),
        ("config", Some("zai"))
    );
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

/// A running headless child carries its start token, and the stop helper ends
/// it. Before, a stop marked the row stopped and left the pid running, and a
/// row with no start token wedged the fleet-wide footprint check.
#[test]
fn a_running_child_carries_a_start_token_and_a_stop_ends_it() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let home = AgentsHome::from_env();
    let bin = crate::write_exec_stub(
        tmp.path(),
        "footnote",
        "#!/bin/sh\ncat >/dev/null\nsleep 60\n",
    );
    let saved = set_env(&[
        ("FNO_FOOTNOTE_BIN", bin.to_str().unwrap()),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
    ]);
    let (h, cwd) = (home.clone(), tmp.path().to_path_buf());
    let run = std::thread::spawn(move || {
        let params = serde_json::json!({});
        dispatch_once(
            &h,
            "fx-run",
            "hi",
            "tester",
            &cwd,
            Some("glm-test"),
            None,
            &params,
        )
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let row = loop {
        let row = load_registry(&home.registry_json())
            .ok()
            .and_then(|r| r.find("fx-run").cloned())
            .filter(|e| e.pid.is_some());
        if let Some(row) = row {
            break row;
        }
        assert!(std::time::Instant::now() < deadline, "no running row");
        std::thread::sleep(Duration::from_millis(50));
    };
    let (pid, started) = (row.pid.unwrap(), row.pid_start_time);
    let stopped = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap()
        .block_on(stop_confirmed(pid, started));
    run.join().unwrap();
    restore_env(saved);
    assert!(started.is_some(), "the row carries no pid start token");
    assert!(stopped, "the child outlived the stop");
}

/// The pane spec is the launch spec in a file only its owner reads, because
/// it carries the endpoint key. A missing model refuses before any write.
#[test]
fn the_pane_spec_is_a_private_file_holding_the_launch_spec() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let saved = set_env(&[
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
    ]);
    let refused = write_pane_spec(tmp.path(), "", None, None);
    let path = write_pane_spec(tmp.path(), "glm-test", Some("x-1"), Some("plan"));
    restore_env(saved);
    assert!(refused.unwrap_err().contains("needs -m"));
    let path = path.unwrap();
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(spec["v"], 2);
    assert_eq!(spec["model"], "glm-test");
    assert_eq!(spec["node"], "x-1");
    assert_eq!(spec["permission_mode"], "plan");
    assert!(spec["endpoint"]["key"]
        .as_str()
        .is_some_and(|k| !k.is_empty()));
}

fn set_env(vars: &[(&'static str, &str)]) -> Vec<(&'static str, Option<std::ffi::OsString>)> {
    let saved = vars
        .iter()
        .map(|(k, _)| (*k, std::env::var_os(k)))
        .collect();
    for (k, v) in vars {
        std::env::set_var(k, v);
    }
    saved
}

fn restore_env(saved: Vec<(&'static str, Option<std::ffi::OsString>)>) {
    for (k, v) in saved {
        match v {
            Some(v) => std::env::set_var(k, v),
            None => std::env::remove_var(k),
        }
    }
}

/// A `/loop` seed creates once, then resumes once per tick until the row is
/// removed: the binary saw one create spec and two resume specs with the
/// seed's prompt, and the driver exits 0 naming `row removed` and 3 ticks.
/// The fake binary also writes the transcript file the real one writes, so
/// the ask lane can find the session again.
#[test]
fn a_loop_seed_creates_once_then_resumes_each_tick_until_the_row_is_removed() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let home = AgentsHome::from_env();
    let scratch = tmp.path().join("scratch");
    std::fs::create_dir_all(&scratch).unwrap();
    let script = [
        "#!/bin/sh",
        "n=$(cat '__D__/count' 2>/dev/null || echo 0)",
        "n=$((n+1))",
        "echo \"$n\" > '__D__/count'",
        "cat > \"__D__/spec-$n.json\"",
        "spec=\"__D__/spec-$n.json\"",
        "d=$(sed -n 's/.*\"session_dir\":\"\\([^\"]*\\)\".*/\\1/p' \"$spec\" | head -1)",
        "i=$(sed -n 's/.*\"fno_id\":\"\\([^\"]*\\)\".*/\\1/p' \"$spec\" | head -1)",
        "mkdir -p \"$d\" && : > \"$d/$i.jsonl\"",
        "exit 0",
        "",
    ]
    .join("\n")
    .replace("__D__", scratch.to_str().unwrap());
    let bin = crate::write_exec_stub(tmp.path(), "footnote", &script);
    let saved = set_env(&[
        ("FNO_FOOTNOTE_BIN", bin.to_str().unwrap()),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
        ("ZAI_API_KEY", "zk"),
    ]);
    let seed = super::loop_seed::parse("/loop 5m Reply TICK")
        .unwrap()
        .unwrap();
    let (h, cwd) = (home.clone(), tmp.path().to_path_buf());
    let first = |prompt: &str| {
        create_turn(
            &h,
            "fx-loop",
            prompt,
            "tester",
            &cwd,
            "glm-test",
            None,
            &serde_json::json!({}),
        )
    };
    let tick = |prompt: &str| {
        let code =
            maybe_run_ask(&h, &serde_json::json!({ "message": prompt }), "fx-loop").unwrap_or(12);
        outcome(code, String::new(), String::new())
    };
    let row_alive = || {
        load_registry(&h.registry_json())
            .ok()
            .is_some_and(|reg| reg.find_name_or_full_session_id("fx-loop").is_some())
    };
    let mut sleeps = 0u32;
    let out = super::loop_seed::drive(
        &seed,
        first,
        tick,
        row_alive,
        |_| {
            sleeps += 1;
            if sleeps >= 3 {
                let _ = update_registry(&h.registry_json(), |reg| {
                    let before = reg.entries.len();
                    reg.entries.retain(|e| e.name != "fx-loop");
                    reg.entries.len() != before
                });
            }
        },
        std::time::SystemTime::now,
    );
    restore_env(saved);
    assert_eq!(out.exit_code, 0, "{}", out.stderr);
    assert!(out.stderr.contains("row removed"), "{}", out.stderr);
    assert!(out.stderr.contains("3 ticks"), "{}", out.stderr);
    let spec = |n: &str| {
        serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(scratch.join(format!("spec-{n}.json"))).unwrap(),
        )
        .unwrap()
    };
    let created = spec("1");
    assert_eq!(created["mode"], "create");
    assert_eq!(created["message"], "Reply TICK");
    for n in ["2", "3"] {
        let resumed = spec(n);
        assert_eq!(resumed["mode"], "resume", "{n}");
        assert_eq!(resumed["message"], "Reply TICK", "{n}");
    }
}

/// A seed with a missing, sub-minute or unterminated interval refuses with
/// the usage line before any child or registry write; a `/loopy` prefix or
/// a plain prompt is not a seed and keeps the one-turn path.
#[test]
fn a_loop_seed_refuses_bad_intervals_and_leaves_plain_prompts_alone() {
    for bad in ["/loop Reply TICK", "/loop 30s x", "/loop 5m", "/loop"] {
        let err = super::loop_seed::parse(bad).unwrap().unwrap_err();
        assert!(err.contains("/loop <N>s|m|h|d <prompt>"), "{bad}: {err}");
    }
    assert!(super::loop_seed::parse("/loopy 5m x").is_none());
    assert!(super::loop_seed::parse("Reply TICK").is_none());
    assert!(super::loop_seed::parse("/loop 90s Reply TICK")
        .unwrap()
        .is_ok());
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let home = AgentsHome::from_env();
    let saved = set_env(&[
        ("FNO_FOOTNOTE_BIN", "/nonexistent/footnote"),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
    ]);
    let o = dispatch_once(
        &home,
        "fx-bad",
        "/loop Reply TICK",
        "tester",
        tmp.path(),
        Some("glm-test"),
        None,
        &serde_json::json!({}),
    );
    restore_env(saved);
    assert_eq!(o.exit_code, 2, "{}", o.stderr);
    assert!(
        o.stderr.contains("/loop <N>s|m|h|d <prompt>"),
        "{}",
        o.stderr
    );
    let reg = load_registry(&home.registry_json()).unwrap();
    assert!(reg.find("fx-bad").is_none());
}

/// A refused tick (exit 2) stops the loop at once; three failed ticks in a
/// row stop it carrying the last code.
#[test]
fn a_loop_stops_on_a_refusal_and_after_three_failures_in_a_row() {
    let seed = super::loop_seed::LoopSeed {
        interval: Duration::from_secs(60),
        prompt: "x".into(),
    };
    let out = super::loop_seed::drive(
        &seed,
        |_| outcome(0, String::new(), String::new()),
        |_| outcome(2, String::new(), String::new()),
        || true,
        |_| {},
        || std::time::SystemTime::UNIX_EPOCH,
    );
    assert_eq!(out.exit_code, 2);
    assert!(out.stderr.contains("refused"), "{}", out.stderr);
    let mut ticks = 0u32;
    let out = super::loop_seed::drive(
        &seed,
        |_| outcome(0, String::new(), String::new()),
        |_| {
            ticks += 1;
            outcome(1, String::new(), String::new())
        },
        || true,
        |_| {},
        || std::time::SystemTime::UNIX_EPOCH,
    );
    assert_eq!(ticks, 3);
    assert_eq!(out.exit_code, 1);
    assert!(
        out.stderr.contains("3 failed ticks in a row"),
        "{}",
        out.stderr
    );
}
