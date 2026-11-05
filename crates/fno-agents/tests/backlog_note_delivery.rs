//! wave 2: the native note action (the grouped `backlog note` door), real-store tests.

use fno_agents::backlog::node_state;
use serde_json::json;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn fixture(id: &str, status: &str) -> serde_json::Value {
    json!({
        "id": id,
        "slug": format!("slug-{id}"),
        "title": format!("Node {id}"),
        "type": "feature",
        "status": status,
        "priority": "p1",
        "details": "premise",
    })
}

fn write_graph(path: &PathBuf, entries: &[serde_json::Value]) {
    fno_agents::graph_store::seed_rows(path, entries).unwrap();
}

/// The landed rows, read from the store: graph.json is a frozen mirror
/// under graph.db.
fn read_graph(path: &PathBuf) -> serde_json::Value {
    json!({ "entries": fno_agents::graph_store::read_rows(path).unwrap() })
}

/// Move the `--graph <path>` pair to the front: the grouped door's engine
/// contract opens with it (every real bridge speaks this shape), while the
/// engine parser itself accepts the pair anywhere.
fn door_first(args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--graph" && i + 1 < args.len() {
            out.insert(0, args[i + 1].to_string());
            out.insert(0, args[i].to_string());
            i += 2;
        } else {
            out.push(args[i].to_string());
            i += 1;
        }
    }
    out
}

/// Run the binary entry point with the body on stdin.
fn note(_graph: &std::path::Path, args: &[&str], body: &str) -> i32 {
    use std::io::Write;
    let argv = door_first(args);
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(
            std::iter::once("backlog")
                .chain(std::iter::once("note"))
                .chain(argv.iter().map(String::as_str)),
        )
        // The client lazy-starts a daemon that inherits this env, so the
        // daemon dies with this test run instead of idling an hour (x-5533).
        .envs(fno_agents::test_run::self_owner_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn fno-agents backlog note");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    std::io::stdout().write_all(&out.stdout).unwrap();
    out.status.code().unwrap_or(-1)
}

fn graph_arg(graph: &std::path::Path) -> [String; 2] {
    ["--graph".to_string(), graph.display().to_string()]
}

/// The note door with all three channels captured: the x-786d contract is
/// asserted on stderr, so it must not inherit the test's stderr.
fn note_captured(args: &[&str], body: &str) -> (i32, String, String) {
    use std::io::Write;
    let argv = door_first(args);
    let mut child = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(
            std::iter::once("backlog")
                .chain(std::iter::once("note"))
                .chain(argv.iter().map(String::as_str)),
        )
        .envs(fno_agents::test_run::self_owner_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fno-agents backlog note");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// x-786d: a corrupt store answers as a read failure (exit 5), never as an
/// absent node.
#[test]
fn corrupt_graph_names_the_read_failure_never_absence() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    let db = fno_agents::backlog::database_path(&graph);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    std::fs::write(&db, "{").unwrap();
    let g = graph_arg(&graph);
    let (code, stdout, stderr) = note_captured(
        &[g[0].as_str(), g[1].as_str(), "t-1", "body", "--quiet"],
        "",
    );
    assert_eq!(code, 5, "stdout: {stdout} stderr: {stderr}");
    assert!(stderr.contains("graph read failed"), "{stderr}");
    assert!(
        !stdout.contains("no node resolves") && !stderr.contains("no node resolves"),
        "a starved read must not read as an absent node: {stdout} {stderr}"
    );
}

#[test]
fn ac5_positional_stdin_and_file_bodies_all_append_to_the_thread() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    // l-row is a legacy minimal row (no slug/type/status): the store parks
    // it raw in the carry. The reader serves it with defaults applied, so
    // a note appends to its thread through the same upgrade instead of
    // refusing. Seeded beside t-1: a second write_graph call would run the
    // publish read-back against a non-empty store and refuse.
    write_graph(
        &graph,
        &[
            fixture("t-1", "ready"),
            json!({"id": "l-row", "title": "legacy"}),
        ],
    );
    let g = graph_arg(&graph);
    // stdin
    let code = note(
        &graph,
        &[
            "--stdin", "--json", "--node", "t-1", "--quiet", &g[0], &g[1],
        ],
        "body one",
    );
    assert_eq!(code, 0);
    // body file
    let body_file = dir.path().join("body.txt");
    std::fs::write(&body_file, "body two").unwrap();
    let bf = body_file.display().to_string();
    let code = note(
        &graph,
        &[
            "--body-file",
            &bf,
            "--json",
            "--node",
            "t-1",
            "--quiet",
            &g[0],
            &g[1],
        ],
        "",
    );
    assert_eq!(code, 0);
    // positional
    let code = note(
        &graph,
        &["t-1", "body three", "--json", "--quiet", &g[0], &g[1]],
        "",
    );
    assert_eq!(code, 0);
    let entries = read_graph(&graph)["entries"].as_array().unwrap().clone();
    let row = entries.iter().find(|r| r["id"] == json!("t-1")).unwrap();
    let notes = row["progress_notes"].as_array().unwrap();
    let bodies: Vec<&str> = notes
        .iter()
        .map(|n| n["text"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(bodies, vec!["body one", "body two", "body three"]);
    assert!(
        row.get(node_state::STATE_KEY).is_none(),
        "a note writes no state"
    );
    // --if-revision guards --clear only; on the note route it is usage.
    let code = note(
        &graph,
        &[
            "t-1",
            "stale",
            "--json",
            "--quiet",
            "--if-revision",
            "1",
            &g[0],
            &g[1],
        ],
        "",
    );
    assert_eq!(code, 2);
    let row = read_graph(&graph)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!("t-1"))
        .unwrap()
        .clone();
    assert_eq!(row["progress_notes"].as_array().unwrap().len(), 3);
    // The legacy row's own leg: a note appends to its thread through the
    // upgrade, never a refusal.
    let code = note(
        &graph,
        &[
            "l-row",
            "a note on a raw row",
            "--json",
            "--quiet",
            &g[0],
            &g[1],
        ],
        "",
    );
    assert_eq!(code, 0);
    let row = read_graph(&graph)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!("l-row"))
        .unwrap()
        .clone();
    let notes = row["progress_notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0]["text"], json!("a note on a raw row"));
}

#[test]
fn ac7_machine_and_wave_records_go_to_history_only() {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join("graph.json");
    write_graph(&graph, &[fixture("m-1", "in_progress")]);
    let g = graph_arg(&graph);
    // A human note first: it appends a thread row and writes no state, so
    // there is no state a machine record could clobber.
    note(
        &graph,
        &[
            "--stdin", "--json", "--node", "m-1", "--quiet", &g[0], &g[1],
        ],
        "human state",
    );
    let code = note(
        &graph,
        &[
            "--machine",
            "task_done",
            "--stdin",
            "--json",
            "--node",
            "m-1",
            "--quiet",
            &g[0],
            &g[1],
        ],
        "machine progress row",
    );
    assert_eq!(code, 0);
    let row = read_graph(&graph)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!("m-1"))
        .unwrap()
        .clone();
    assert!(
        row.get(node_state::STATE_KEY).is_none(),
        "a human note writes no state for a machine record to clobber"
    );
    let notes = row["progress_notes"].as_array().unwrap();
    assert_eq!(notes.len(), 1, "the machine record skips the thread");
    assert_eq!(notes[0]["text"], json!("human state"));
    let (_, total) = fno_agents::backlog::note_history::read(&graph, Some("m-1"), 0, 50).unwrap();
    assert_eq!(total, 1, "machine record landed in history");
    // Wave addition: history + the refresh marker.
    let code = note(
        &graph,
        &[
            "--wave", "--stdin", "--json", "--node", "m-1", "--quiet", &g[0], &g[1],
        ],
        "wave 2 done",
    );
    assert_eq!(code, 0);
    let row = read_graph(&graph)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == json!("m-1"))
        .unwrap()
        .clone();
    assert_eq!(row["state_needs_refresh"], json!(true));
}
