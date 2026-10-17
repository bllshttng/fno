use super::*;

/// The spend in a session's newest `terminal` lands in the ledger as one
/// execution row, which a resume updates in place and never duplicates or
/// strips of another writer's stamp; a heartbeat under 90 s old reads
/// fresh, an older or absent one does not.
#[test]
fn the_record_feeds_the_ledger_and_the_heartbeat_reads_fresh() {
    use serde_json::json;
    let tmp = tempfile::tempdir().unwrap();
    let ledger = tmp.path().join("ledger.json");
    let rec = |seq: u64, ty: &str, data: Value| json!({"seq": seq, "ts": format!("2026-10-16T10:0{seq}:00.000Z"), "type": ty, "data": data});
    let spec: LaunchSpec = serde_json::from_value(json!({"v": 2, "mode": "resume", "fno_id": "s1",
        "session_dir": "/s1", "cwd": "/", "model": "glm-x", "message": "hi", "timeout_secs": null,
        "node": null, "plan_path": "/plans/p.md", "cost_cap_usd": null, "wall_cap_minutes": null,
        "plugin_root": null, "parent_session_id": null, "permission_mode": null, "price_cache": "/p",
        "context_window": 1, "endpoint": {"base_url": "u", "key": "k", "bearer": true, "wire": "anthropic",
            "provider_id": null, "route": "env"}}))
    .unwrap();
    let mut records = vec![rec(0, "header", json!({"node": "x-1"}))];
    assert!(execution_row(&records, &spec, None).is_none());
    let stamped = json!({"entries": [{"type": "execution", "fno_id": "s1", "pr_number": 7}]});
    std::fs::write(&ledger, stamped.to_string()).unwrap();
    for (seq, cost) in [(1, 0.25), (2, 0.42)] {
        records.push(rec(
            seq,
            "terminal",
            json!({"state": "done", "turns": seq, "cost_usd": cost}),
        ));
        let row = execution_row(&records, &spec, Some("glm-y")).unwrap();
        crate::ledger_axes::upsert_execution(&ledger, row).unwrap();
    }
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    let rows = data["entries"].as_array().unwrap();
    let row = &rows[0];
    assert_eq!((rows.len(), row["cost_usd"].as_f64()), (1, Some(0.42)));
    assert_eq!(row["pr_number"], 7);
    assert_eq!(
        (row["graph_node_id"].as_str(), row["model"].as_str()),
        (Some("x-1"), Some("glm-y"))
    );
    assert_eq!(row["termination_reason"], "Done");
    // A resume that lost the plan path keeps the known one; a budget stop
    // reads as non-delivery, so the merge backstop skips it.
    let lost = LaunchSpec {
        plan_path: None,
        ..spec.clone()
    };
    records.push(rec(
        3,
        "terminal",
        json!({"state": "budget", "cost_usd": 0.5}),
    ));
    let row = execution_row(&records, &lost, None).unwrap();
    crate::ledger_axes::upsert_execution(&ledger, row).unwrap();
    let data: Value = serde_json::from_str(&std::fs::read_to_string(&ledger).unwrap()).unwrap();
    let row = &data["entries"][0];
    assert_eq!(
        (
            row["plan_path"].as_str(),
            row["termination_reason"].as_str()
        ),
        (Some("/plans/p.md"), Some("Budget"))
    );
    std::fs::write(&ledger, "not json").unwrap();
    let row = execution_row(&records, &spec, None).unwrap();
    assert!(crate::ledger_axes::upsert_execution(&ledger, row).is_err());
    assert_eq!(std::fs::read_to_string(&ledger).unwrap(), "not json");

    // The freshness check reads the file's mtime, so the clock here is real.
    let path = tmp.path().join("s1.jsonl");
    let at = chrono::Utc::now().timestamp() as u64;
    let beat_at = |secs: u64| {
        let ts = crate::provider_cap::epoch_to_rfc3339(secs as i64);
        json!({"ts": ts, "type": "heartbeat", "data": {"state": "idle"}})
    };
    std::fs::write(&path, format!("{}\n{}\n", records[0], beat_at(at))).unwrap();
    assert!(transcript::heartbeat_fresh(&path, at + 30));
    assert!(!transcript::heartbeat_fresh(&path, at + 120));
    // An offset timestamp names the same instant as its UTC form.
    let west = chrono::FixedOffset::west_opt(7200).unwrap();
    let ts = chrono::DateTime::from_timestamp(at as i64, 0)
        .unwrap()
        .with_timezone(&west);
    let beat = json!({"ts": ts.to_rfc3339(), "type": "heartbeat", "data": {"state": "idle"}});
    std::fs::write(&path, format!("{}\n{beat}\n", records[0])).unwrap();
    assert!(transcript::heartbeat_fresh(&path, at + 30));
    std::fs::write(&path, format!("{}\n{}\n", records[0], beat_at(at - 120))).unwrap();
    assert!(!transcript::heartbeat_fresh(&path, at));
    std::fs::write(&path, format!("{}\n", records[0])).unwrap();
    assert!(!transcript::heartbeat_fresh(&path, at));
    assert!(!transcript::heartbeat_fresh(
        &tmp.path().join("gone.jsonl"),
        at
    ));
}

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
/// it carries the endpoint key. A missing model refuses before any write. A
/// caller-assigned id names the TUI's first session, so the spec carries it and
/// the session dir ends in it; a malformed id refuses before any write.
#[test]
fn the_pane_spec_is_a_private_file_holding_the_launch_spec() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let saved = set_env(&[
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
    ]);
    let id = "0b73a1c0-6bd0-4bf7-a30f-9b1fddac1e58";
    let named = write_pane_spec(tmp.path(), "glm-test", None, None, Some(id));
    let malformed: Vec<_> = ["../x", "0B73A1C0-6BD0-4BF7-A30F-9B1FDDAC1E58", ""]
        .into_iter()
        .map(|bad| write_pane_spec(tmp.path(), "glm-test", None, None, Some(bad)))
        .collect();
    let root = transcript::sessions_root();
    let files = std::fs::read_dir(&root).unwrap().count();
    let refused = write_pane_spec(tmp.path(), "", None, None, None);
    let written = write_pane_spec(tmp.path(), "glm-test", Some("x-1"), Some("plan"), None);
    restore_env(saved);
    let (named_path, named_sid) = named.unwrap();
    assert_eq!(named_sid, id);
    let named_spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&named_path).unwrap()).unwrap();
    assert_eq!(named_spec["fno_id"], id);
    assert!(named_spec["session_dir"].as_str().unwrap().ends_with(id));
    for bad in malformed {
        assert!(bad.unwrap_err().contains("malformed fno_id"));
    }
    assert_eq!(files, 1);
    assert!(refused.unwrap_err().contains("needs -m"));
    let (path, sid) = written.unwrap();
    assert!(crate::resume_wake::is_uuid_shaped(&sid));
    assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    let spec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(spec["v"], 2);
    assert_eq!(spec["fno_id"], sid.as_str());
    assert_eq!(spec["model"], "glm-test");
    assert_eq!(spec["node"], "x-1");
    assert_eq!(spec["permission_mode"], "plan");
    assert!(spec["endpoint"]["key"]
        .as_str()
        .is_some_and(|k| !k.is_empty()));
}

/// A pane row binds the TUI's session as `harness_session_id`, and its birth
/// `fno_id` names no transcript, so an ask resumes the bound session. A
/// headless row sets both ids to one value and resumes as before.
#[test]
fn an_ask_resumes_the_session_the_harness_id_names() {
    let tmp = tempfile::tempdir().unwrap();
    let _home = crate::AgentsHomeEnvGuard::set(&tmp.path().join("agents-home"));
    let home = AgentsHome::from_env();
    let birth = "0b73a1c0-6bd0-4bf7-a30f-9b1fddac1e58";
    let bound = "5a1c0f2e-9d34-4c7b-8e21-3f6a7b9c0d12";
    let plain = "c4e8d2a6-1f37-4a90-b5d3-72e0a9f14c68";
    for id in [bound, plain] {
        let dir = transcript::session_dir(&transcript::sessions_root(), tmp.path(), id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(transcript::transcript_file(&dir, id), "").unwrap();
    }
    let rows = [("fx-pane", birth, bound), ("fx-headless", plain, plain)];
    for (name, fno_id, harness_id) in rows {
        let mut row = RegistryEntry {
            name: name.to_string(),
            harness: Some("footnote".into()),
            harness_session_id: Some(harness_id.into()),
            requested_model: Some("glm-test".into()),
            cwd: tmp.path().to_string_lossy().to_string(),
            ..RegistryEntry::new(
                Some(harness_id.into()),
                crate::spawn_lineage::ambient_lineage(),
            )
        };
        row.fno_id = Some(fno_id.into());
        update_registry(&home.registry_json(), |reg| {
            reg.entries.push(row.clone());
            true
        })
        .unwrap();
    }
    let seen = tmp.path().join("spec.json");
    let bin = crate::write_exec_stub(
        tmp.path(),
        "footnote",
        &format!("#!/bin/sh\ncat > '{}'\nexit 2\n", seen.display()),
    );
    let saved = set_env(&[
        ("FNO_FOOTNOTE_BIN", bin.to_str().unwrap()),
        ("ANTHROPIC_BASE_URL", "https://api.z.ai/api/anthropic"),
        ("ANTHROPIC_AUTH_TOKEN", "k"),
    ]);
    let specs: Vec<_> = rows
        .into_iter()
        .map(|(name, _, want)| {
            let _ = std::fs::remove_file(&seen);
            let code = maybe_run_ask(&home, &serde_json::json!({"message": "hi"}), name);
            let body = std::fs::read_to_string(&seen).unwrap_or_default();
            (code, serde_json::from_str::<serde_json::Value>(&body), want)
        })
        .collect();
    restore_env(saved);
    for (code, spec, want) in specs {
        let spec = spec.expect("the stand-in binary never read a spec");
        assert_eq!(code, Some(2));
        assert_eq!(spec["mode"], "resume");
        assert_eq!(spec["fno_id"], want);
        assert!(spec["session_dir"].as_str().unwrap().ends_with(want));
    }
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
