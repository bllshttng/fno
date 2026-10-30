//! parity-stage: characterization
//! parity-oracle: fno.agents.discover

use fno_agents::discovered_sessions::aliases::{
    default_alias, disambiguate, fit_sid_alias, is_accreted, is_legacy_handle,
};
use fno_agents::discovered_sessions::sources::{candidate_dir_names, is_pid_file};
use fno_agents::discovered_sessions::{
    ambient_falsifier, discover_live_sessions, to_row, Candidate, Sources, TruthReading,
};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, SystemTime};

fn golden() -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/discovered/discover.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn text(value: &Value) -> String {
    value.as_str().unwrap().to_string()
}

fn strings(value: &Value) -> HashSet<String> {
    value.as_array().unwrap().iter().map(text).collect()
}

fn pairs(value: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in value.as_array().unwrap() {
        out.push((text(&pair[0]), text(&pair[1])));
    }
    out
}

fn lay(root: &Path, files: &Value, ages: &Value) {
    for (rel, body) in files.as_object().unwrap() {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body.as_str().unwrap()).unwrap();
        if let Some(age) = ages.get(rel).and_then(Value::as_u64) {
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(SystemTime::now() - Duration::from_secs(age))
                .unwrap();
        }
    }
}

fn run_case(root: &Path, tree: &Value, case: &Value, truth: &Value) {
    let empty = json!([]);
    let sources_in = Sources {
        sessions_dirs: vec![root.join("sessions")],
        projects_dir: root.join("projects"),
        codex_sessions_dir: root.join("codex"),
        opencode_storage_dir: root.join("opencode/storage"),
        name_map_path: root.join("state/mux/session-names.json"),
        registry_path: root.join("agents/registry.json"),
        roster_path: root.join("daemon/roster.json"),
        recency_s: 600.0,
        live_procs: case
            .get("live_procs")
            .unwrap_or(&empty)
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| (pair[0].as_u64().unwrap() as u32, text(&pair[1])))
            .collect(),
        loaded_threads: pairs(case.get("loaded_threads").unwrap_or(&empty)),
        bg_socket_pids: tree["bg_socket_pids"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(key, pid)| (key.clone(), pid.as_u64().unwrap() as u32))
            .collect(),
    };
    let projects = tree["projects"].clone();
    let project_of = |cwd: &str| {
        projects
            .get(cwd)
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let mut truth_of = |row: &Candidate, _handle: &str| {
        let fallback = json!({"state": "unknown"});
        TruthReading::from_payload(truth.get(&row.session_id).unwrap_or(&fallback))
    };
    let sessions = discover_live_sessions(
        &sources_in,
        &strings(&tree["exclude_short"]),
        &strings(&tree["exclude_ids"]),
        &project_of,
        &mut truth_of,
    )
    .unwrap();
    let expected = &case["expected"];
    let handles: Vec<Value> = sessions
        .iter()
        .map(|session| json!([session.candidate.session_id, session.handle]))
        .collect();
    assert_eq!(Value::Array(handles), expected["handles"]);
    let rows: Vec<Value> = sessions
        .iter()
        .filter(|session| session.truth.is_alive())
        .map(|session| to_row(session, ambient_falsifier(&session.candidate)))
        .collect();
    let want = expected["rows"].as_array().unwrap();
    assert_eq!(rows.len(), want.len());
    for (row, wanted) in rows.iter().zip(want) {
        assert_eq!(
            serde_json::to_string(row).unwrap(),
            serde_json::to_string(wanted).unwrap()
        );
    }
    assert_eq!(
        std::fs::read_to_string(&sources_in.name_map_path).unwrap(),
        expected["name_map"].as_str().unwrap()
    );
}

#[test]
fn enumeration_aliases_and_rows_match_over_the_legacy_opencode_tree() {
    let golden = golden();
    let tree = &golden["tree"];
    let dir = tempfile::tempdir().unwrap();
    lay(dir.path(), &tree["files"], &tree["ages"]);
    run_case(dir.path(), tree, tree, &tree["truth"]);
}

#[test]
fn the_opencode_database_retires_the_legacy_tree_and_a_non_text_directory_reads_empty() {
    let golden = golden();
    let tree = &golden["tree"];
    let case = &golden["db"];
    let dir = tempfile::tempdir().unwrap();
    lay(dir.path(), &case["files"], &json!({}));
    let conn = rusqlite::Connection::open(dir.path().join("opencode/opencode.db")).unwrap();
    conn.execute(
        "CREATE TABLE session (id TEXT, directory, time_updated INTEGER)",
        [],
    )
    .unwrap();
    for row in case["db_rows"].as_array().unwrap() {
        let directory: rusqlite::types::Value = match &row[1] {
            Value::String(cwd) => rusqlite::types::Value::Text(cwd.clone()),
            other => rusqlite::types::Value::Integer(other.as_i64().unwrap()),
        };
        conn.execute(
            "INSERT INTO session VALUES (?1, ?2, ?3)",
            rusqlite::params![text(&row[0]), directory, row[2].as_i64().unwrap()],
        )
        .unwrap();
    }
    drop(conn);
    let mut truth = tree["truth"].clone();
    let working = json!({
        "state": "working",
        "last_activity_age_s": 1,
        "observed_model": {"kind": "no-transcript"},
    });
    for row in case["db_rows"].as_array().unwrap() {
        truth[text(&row[0])] = working.clone();
    }
    run_case(dir.path(), tree, case, &truth);
}

#[test]
fn the_pure_alias_and_path_rules_match() {
    let golden = golden();
    let pure = &golden["pure"];
    for item in pure["accreted"].as_array().unwrap() {
        assert_eq!(
            json!(is_accreted(item[0].as_str().unwrap())),
            item[1],
            "{item}"
        );
    }
    for item in pure["legacy"].as_array().unwrap() {
        assert_eq!(
            json!(is_legacy_handle(item[0].as_str().unwrap())),
            item[1],
            "{item}"
        );
    }
    for item in pure["dir_names"].as_array().unwrap() {
        assert_eq!(
            json!(candidate_dir_names(item[0].as_str().unwrap())),
            item[1],
            "{item}"
        );
    }
    for item in pure["default_alias"].as_array().unwrap() {
        let got = default_alias(item[0].as_str(), item[1].as_str().unwrap());
        assert_eq!(json!(got), item[2], "{item}");
    }
    for item in pure["fit"].as_array().unwrap() {
        let got = fit_sid_alias(item[0].as_str().unwrap(), item[1].as_str().unwrap());
        assert_eq!(json!(got), item[2], "{item}");
    }
    for item in pure["pid_files"].as_array().unwrap() {
        assert_eq!(
            json!(is_pid_file(item[0].as_str().unwrap())),
            item[1],
            "{item}"
        );
    }
    for case in pure["disambiguate"].as_array().unwrap() {
        let aliases: BTreeMap<String, String> = case["aliases"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(sid, name)| (sid.clone(), text(name)))
            .collect();
        let live: Vec<Candidate> = case["live"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| Candidate {
                session_id: text(&row["session_id"]),
                short_id: text(&row["short_id"]),
                project: row["project"].as_str().map(str::to_string),
                ..Candidate::default()
            })
            .collect();
        assert_eq!(json!(disambiguate(&aliases, &live)), case["out"], "{case}");
    }
}
