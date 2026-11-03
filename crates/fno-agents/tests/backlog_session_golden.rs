//! The native `fno backlog session` lifecycle: the lease contract the port
//! must keep (2h TTL, the passed identity, pid_unavailable when no durable
//! session pid resolves), the handover join ladder, and the close release.
//! Characterization for the deleted Python `graph/_session.py`.

use std::path::PathBuf;
use std::process::Command;

fn sandbox() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(&config, format!("state_dir = \"{}\"", dir.path().display()))
        .expect("write config");
    let graph = dir.path().join("graph.json");
    (dir, config, graph)
}

fn identity_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("FNO_HARNESS_NAME", "claude"),
        ("FNO_HARNESS_SESSION_ID", "sess-golden-0001"),
    ]
}

fn ambient_markers() -> &'static [&'static str] {
    &[
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CODEX_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
        "FNO_NODE_CLAIM_HOLDER",
    ]
}

fn session(
    config: &PathBuf,
    graph: &PathBuf,
    args: &[&str],
    extra_env: &[(&str, &str)],
    strip_markers: bool,
) -> (i32, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args([&["backlog", "session"], args].concat())
        .env("FNO_CONFIG", config)
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .env("FNO_TRACKER_BACKEND", "graph")
        .env(
            "FNO_CLAIMS_ROOT",
            graph.parent().unwrap().join("claims-root"),
        )
        .envs(fno_agents::test_run::self_owner_env());
    for (k, v) in identity_env() {
        cmd.env(k, v);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    if strip_markers {
        for marker in ambient_markers() {
            cmd.env_remove(marker);
        }
    }
    let out = cmd.output().expect("run fno-agents backlog session");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seed(graph: &PathBuf, entries: &[serde_json::Value]) {
    fno_agents::graph_store::seed_rows(graph, entries).expect("seed store");
}

/// The claim row for a node, as the YAML the old lockfile carried.
fn lockfile(graph: &PathBuf, node_id: &str) -> Option<String> {
    let root = graph.parent().unwrap().join("claims-root");
    let (_, record) = fno_agents::claims::status(&format!("node:{node_id}"), Some(&root));
    record.map(|rec| serde_yaml_ng::to_string(&rec).unwrap())
}

#[test]
fn open_writes_the_two_hour_lease_naming_the_passed_identity() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-aaaa1111", "title": "n", "status": "design",
            "project": "fno", "slug": "n",
        })],
    );
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-aaaa1111", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["status"], "opened");
    assert_eq!(receipt["holder"], "blueprint-session:sess-golden-0001");
    let raw = lockfile(&graph, "x-aaaa1111").expect("claim lockfile");
    let rec: serde_json::Value = serde_yaml_ng::from_str(&raw).unwrap();
    assert_eq!(rec["holder"], "blueprint-session:sess-golden-0001");
    assert_eq!(rec["harness"], "claude");
    assert_eq!(rec["session_id"], "sess-golden-0001");
    // Either the durable session pid was proven (liveness pid arm) or the
    // lease records pid_unavailable and lives by its TTL. A transient pid
    // is the one forbidden answer.
    let acquired = rec["acquired_at"].as_i64().unwrap();
    assert_eq!(rec["expires_at"].as_i64().unwrap(), acquired + 7_200_000);
    if rec["pid_unavailable"] == true {
        assert!(rec["pid"].is_null(), "{}", raw);
    } else {
        assert!(rec["pid"].is_i64(), "{}", raw);
    }
}

#[test]
fn a_second_open_by_the_same_session_refuses_and_keeps_the_acquire_time() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-bbbb2222", "title": "n", "status": "design",
            "project": "fno", "slug": "n",
        })],
    );
    let first = session(
        &config,
        &graph,
        &["open", "x-bbbb2222", "--json"],
        &[],
        false,
    );
    assert_eq!(first.0, 0, "{}{}", first.1, first.2);
    let before = lockfile(&graph, "x-bbbb2222").unwrap();
    let r = session(&config, &graph, &["open", "x-bbbb2222"], &[], false);
    assert_eq!(r.0, 1, "{}{}", r.1, r.2);
    assert!(r.2.contains("is already open for this session"), "{}", r.2);
    let after = lockfile(&graph, "x-bbbb2222").unwrap();
    let t =
        |s: &str| serde_yaml_ng::from_str::<serde_json::Value>(s).unwrap()["acquired_at"].clone();
    assert_eq!(t(&before), t(&after));
}

#[test]
fn open_joins_a_spawn_handover_or_target_claim_this_session_owns() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[
            serde_json::json!({
                "id": "x-cccc3333", "title": "n", "status": "design",
                "project": "fno", "slug": "n",
            }),
            serde_json::json!({
                "id": "x-cccc3334", "title": "m", "status": "design",
                "project": "fno", "slug": "m",
            }),
        ],
    );
    let root = graph.parent().unwrap().join("claims-root");
    let outcome = fno_agents::claims::acquire(
        "node:x-cccc3333",
        "spawn-handover:worker-a",
        fno_agents::claims::AcquireOpts {
            ttl_ms: Some(60_000),
            root: Some(root),
            ..Default::default()
        },
    );
    assert!(matches!(
        outcome,
        fno_agents::claims::AcquireOutcome::Acquired(_)
    ));
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-cccc3333", "--json"],
        &[("FNO_NODE_CLAIM_HOLDER", "spawn-handover:worker-a")],
        false,
    );
    assert_eq!(code, 0, "{}{}", stdout, stderr);
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["status"], "joined");
    assert_eq!(receipt["holder"], "spawn-handover:worker-a");

    // A target worker on an unplanned node plans under its own target claim.
    let outcome = fno_agents::claims::acquire(
        "node:x-cccc3334",
        "target-session:sess-golden-0001",
        fno_agents::claims::AcquireOpts {
            ttl_ms: Some(60_000),
            root: Some(graph.parent().unwrap().join("claims-root")),
            ..Default::default()
        },
    );
    assert!(matches!(
        outcome,
        fno_agents::claims::AcquireOutcome::Acquired(_)
    ));
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-cccc3334", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 0, "{}{}", stdout, stderr);
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["status"], "joined");
    assert_eq!(receipt["holder"], "target-session:sess-golden-0001");
}

#[test]
fn open_refuses_a_live_foreign_holder_and_leaves_it_intact() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-dddd4444", "title": "n", "status": "design",
            "project": "fno", "slug": "n",
        })],
    );
    let root = graph.parent().unwrap().join("claims-root");
    let outcome = fno_agents::claims::acquire(
        "node:x-dddd4444",
        "spawn-handover:worker-a",
        fno_agents::claims::AcquireOpts {
            ttl_ms: Some(60_000),
            root: Some(root),
            ..Default::default()
        },
    );
    assert!(matches!(
        outcome,
        fno_agents::claims::AcquireOutcome::Acquired(_)
    ));
    let (code, stdout, stderr) = session(&config, &graph, &["open", "x-dddd4444"], &[], false);
    assert_eq!(code, 1, "{}{}", stdout, stderr);
    assert!(
        stderr.contains("held by spawn-handover:worker-a"),
        "{}",
        stderr
    );
    assert!(stderr.contains("no planner started"), "{}", stderr);
    assert!(
        lockfile(&graph, "x-dddd4444").is_some(),
        "the foreign claim must stay"
    );
}

#[test]
fn open_without_any_identity_refuses_and_claims_nothing() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-eeee5555", "title": "n", "status": "design",
            "project": "fno", "slug": "n",
        })],
    );
    let (code, stdout, stderr) = session(&config, &graph, &["open", "x-eeee5555"], &[], true);
    assert_eq!(code, 2, "{}{}", stdout, stderr);
    assert!(stderr.contains("no ambient identity"), "{}", stderr);
    assert!(lockfile(&graph, "x-eeee5555").is_none());
}

#[test]
fn close_releases_the_blueprint_holder_and_stamps_the_row() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-ffff6666", "title": "n", "status": "design",
            "project": "fno", "slug": "n",
        })],
    );
    let opened = session(
        &config,
        &graph,
        &["open", "x-ffff6666", "--json"],
        &[],
        false,
    );
    assert_eq!(opened.0, 0, "{}{}", opened.1, opened.2);
    let receipt: serde_json::Value = serde_json::from_str(opened.1.trim()).unwrap();
    let acquired = receipt["acquired_at"].as_i64().unwrap();
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &[
            "close",
            "x-ffff6666",
            "--summary",
            "plan ready",
            "--launch",
            "claude /fno:target x-ffff6666",
        ],
        &[],
        false,
    );
    assert_eq!(code, 0, "{}{}", stdout, stderr);
    assert!(
        lockfile(&graph, "x-ffff6666").is_none(),
        "the close releases"
    );
    let rows = fno_agents::graph_store::read_rows(&graph).unwrap();
    let sessions = rows[0]["sessions"].as_array().cloned().unwrap_or_default();
    let blueprint: Vec<&serde_json::Value> = sessions
        .iter()
        .filter(|r| r["phase"] == "blueprint" && r["session_id"] == "sess-golden-0001")
        .collect();
    assert_eq!(
        blueprint.len(),
        1,
        "{}",
        serde_json::to_string(&sessions).unwrap()
    );
    assert!(blueprint[0]["ended_at"].is_string());
    // The claim's acquire time bounds the planning window.
    let started = blueprint[0]["started_at"].as_str().unwrap();
    let parsed = chrono::DateTime::parse_from_rfc3339(&started.replace("Z", "+00:00")).unwrap();
    // The row stamp is second-resolution; the claim's ms acquire time truncates.
    assert_eq!(parsed.timestamp_millis(), (acquired / 1000) * 1000);
}

#[test]
fn think_open_blocks_a_second_session_naming_the_target() {
    // AC1 + AC2: session A opens a think; session B's open exits 1 and its
    // stderr names A's holder and the target door.
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-think0001", "title": "n", "status": "idea",
            "project": "fno", "slug": "n",
            "plan_path": "/plans/20260901-somewhere-x-think0001.md",
        })],
    );
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-think0001", "--phase", "think", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["status"], "opened");

    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &[
            "open",
            "x-think0001",
            "--phase",
            "think",
            "--json",
            "--harness",
            "claude",
            "--session-id",
            "sess-second",
        ],
        &[],
        true,
    );
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(
        stderr.contains("blueprint-session:sess-golden-0001"),
        "{stderr}"
    );
    assert!(
        stderr.contains("plan=/plans/20260901-somewhere-x-think0001.md"),
        "{stderr}"
    );
    assert!(stderr.contains("plan_date=2026-09-01"), "{stderr}");
    assert!(stderr.contains("/fno:target x-think0001"), "{stderr}");
}

#[test]
fn think_close_writes_a_think_row_and_frees_the_claim() {
    // AC3: a think close writes one think session row with ended_at and
    // releases the node claim.
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-think0002", "title": "n", "status": "idea",
            "project": "fno", "slug": "n",
        })],
    );
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-think0002", "--phase", "think", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &[
            "close",
            "x-think0002",
            "--phase",
            "think",
            "--summary",
            "one line",
            "--launch",
            "/fno:blueprint x-think0002",
            "--json",
        ],
        &[],
        false,
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    let receipt: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(receipt["phase"], "think");
    assert_eq!(receipt["claim_released"], true);
    assert!(lockfile(&graph, "x-think0002").is_none());
    let rows = fno_agents::graph_store::read_rows_where_strict(
        &graph,
        &fno_agents::backlog::RowQuery::default(),
    )
    .unwrap();
    let node = rows
        .iter()
        .find(|r| r.get("id").and_then(serde_json::Value::as_str) == Some("x-think0002"))
        .unwrap();
    let sessions = node["sessions"].as_array().unwrap();
    let think_rows: Vec<_> = sessions.iter().filter(|s| s["phase"] == "think").collect();
    assert_eq!(think_rows.len(), 1);
    assert!(think_rows[0]["ended_at"].is_string());
}

#[test]
fn open_refuses_a_fresh_plan_doc_for_a_second_planner() {
    // AC5 at the open door: an inline planner never passes the spawn guard,
    // so open runs the plan-fresh rule. Think refuses a doc inside the
    // window; blueprint refuses a ready doc; a stale doc opens with a warn.
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("20261102-somewhere-x-think0003.md");
    let today = chrono::Utc::now().date_naive().to_string();
    let old = (chrono::Utc::now().date_naive() - chrono::Duration::days(30)).to_string();
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-think0003", "title": "n", "status": "idea",
            "project": "fno", "slug": "n",
            "plan_path": doc.display().to_string(),
        })],
    );
    std::fs::write(
        &doc,
        format!("---\nstatus: ready\ncreated: {today}\n---\n\n# n\n"),
    )
    .unwrap();

    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-think0003", "--phase", "think", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stderr.contains("reason=plan-fresh"), "{stderr}");
    assert!(stderr.contains("somewhere-x-think0003.md"), "{stderr}");

    let (code, _stdout, stderr) = session(
        &config,
        &graph,
        &[
            "open",
            "x-think0003",
            "--json",
            "--harness",
            "claude",
            "--session-id",
            "sess-second",
        ],
        &[],
        true,
    );
    assert_eq!(code, 1, "{stderr}");
    assert!(stderr.contains("reason=plan-fresh"), "{stderr}");

    std::fs::write(
        &doc,
        format!("---\nstatus: ready\ncreated: {old}\n---\n\n# n\n"),
    )
    .unwrap();
    let (code, stdout, stderr) = session(
        &config,
        &graph,
        &["open", "x-think0003", "--phase", "think", "--json"],
        &[],
        false,
    );
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert!(stderr.contains("think proceeds"), "{stderr}");
}
