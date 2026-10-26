//! The native `backlog get` exit contract: 0 hit, 1 clean miss (naming the
//! served store, no unreadable diagnostics), 3 unreadable (naming the read
//! failure, never asserting absence). Ported off the Python surface's
//! exit-code tests when its graph ladder came down.

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

fn get(config: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args([&["backlog", "get"], args].concat())
        .env("FNO_CONFIG", config)
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .env("FNO_TRACKER_BACKEND", "graph")
        // A client spawned by a test dies with the test run, never idling
        // as a daemon orphan (x-5533's contract, guard-enforced).
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .expect("run fno-agents backlog get");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn seed(graph: &PathBuf, entries: &[serde_json::Value]) {
    fno_agents::graph_store::seed_rows(graph, entries).expect("seed store");
}

#[test]
fn a_present_node_resolves_and_field_prints_raw() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({
            "id": "x-aaaa1111", "title": "n", "status": "ready",
            "project": "fno", "slug": "n"
        })],
    );
    let (code, stdout, stderr) = get(&config, &["--strict", "x-aaaa1111", "--field", "id"]);
    assert_eq!(code, 0, "{stdout}{stderr}");
    assert_eq!(stdout.trim(), "x-aaaa1111");
}

#[test]
fn a_clean_miss_is_exit_1_with_one_error_document_naming_the_served_store() {
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({"id": "x-aaaa1111", "title": "n"})],
    );
    let (code, stdout, stderr) = get(&config, &["--strict", "x-zzzz9999"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    // The miss is one parseable document on stdout (the JSON-stdout
    // contract: a pipe consumer never json.loads an empty string), rc
    // stays 1, and stderr stays quiet.
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout parses");
    assert_eq!(
        doc.get("ok").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    let error = doc
        .get("error")
        .and_then(serde_json::Value::as_str)
        .expect("error field");
    assert!(
        error.contains("No node matching 'x-zzzz9999' (id/slug/bare-hex)"),
        "{error}"
    );
    let db = fno_agents::backlog::database_path(&graph);
    assert!(error.contains(&db.display().to_string()), "{error}");
    assert!(!stderr.to_lowercase().contains("unreadable"), "{stderr}");
    assert!(!stderr.to_lowercase().contains("corrupt"), "{stderr}");
}

#[test]
fn an_empty_store_is_a_clean_miss_not_an_unreadable_one() {
    let (_dir, config, graph) = sandbox();
    seed(&graph, &[]);
    let (code, stdout, stderr) = get(&config, &["x-d157"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    let doc: serde_json::Value = serde_json::from_str(stdout.trim()).expect("stdout parses");
    assert_eq!(
        doc.get("ok").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert!(
        doc.get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .contains("No node matching"),
        "{doc}"
    );
}

#[test]
fn a_field_render_miss_keeps_empty_stdout_for_scalar_consumers() {
    // The scalar contract: --field consumers read empty-or-null as not-set
    // (the ship flow reads --field pr_url exactly that way), so a miss keeps
    // the old shape: empty stdout, the message on stderr, rc 1.
    let (_dir, config, graph) = sandbox();
    seed(
        &graph,
        &[serde_json::json!({"id": "x-aaaa1111", "title": "n"})],
    );
    let (code, stdout, stderr) = get(&config, &["--strict", "x-zzzz9999", "--field", "id"]);
    assert_eq!(code, 1, "{stdout}{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("No node matching"), "{stderr}");
}

#[test]
fn an_unreadable_store_is_the_distinct_exit_3_naming_the_failure_never_absence() {
    let (_dir, config, graph) = sandbox();
    let db = fno_agents::backlog::database_path(&graph);
    seed(
        &graph,
        &[serde_json::json!({"id": "x-aaaa1111", "title": "n"})],
    );
    std::fs::write(&db, b"not a database at all").expect("corrupt db");
    let (code, stdout, stderr) = get(&config, &["x-d157"]);
    assert_eq!(
        code,
        fno_agents::backlog::get_cli::GRAPH_UNREADABLE_EXIT,
        "{stdout}{stderr}"
    );
    assert_ne!(code, 1);
    let combined = format!("{stdout}{stderr}");
    assert!(!combined.contains("No node matching"), "{combined}");
    assert!(
        combined.contains("Could not read the graph cleanly"),
        "{combined}"
    );
}
