use super::*;
use serde_json::json;

#[test]
fn malformed_caps_and_policy_name_the_value_in_python_spelling() {
    let caps = json!({"kanban": {"wip_caps": {"now": 0, "next": true, "later": "five", "ok": 3}}});
    assert_eq!(
        checks::wip_cap_problems(&caps),
        vec![
            "wip_caps['now'] = 0 is not a positive integer; column left uncapped",
            "wip_caps['next'] = True is a boolean, not a cap; column left uncapped",
            "wip_caps['later'] = 'five' is not a positive integer; column left uncapped",
        ]
    );
    assert_eq!(
        checks::wip_cap_problems(&json!({"kanban": {"wip_caps": [1]}})),
        vec!["config.kanban.wip_caps is list, not a mapping; all columns left uncapped"]
    );
    assert_eq!(
        checks::worktree_policy_problems(&json!({"worktree": {"policy": "sometimes"}})),
        vec!["config.worktree.policy = 'sometimes' is not one of never | harness-native | external; worktree creation will refuse"]
    );
    assert!(checks::worktree_policy_problems(&json!({"worktree": {"policy": "never"}})).is_empty());
}

#[test]
fn the_unknown_key_walk_reads_map_keys_as_data_and_names_near_misses() {
    let table = crate::config_schema::schema_table();
    let schema = &table["json_schema"];
    let leaves: Vec<String> = table["leaves"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["path"].as_str().unwrap().to_string())
        .collect();
    let layer = |body: Value| vec![("/cfg/config.toml".to_string(), body)];
    // A dict-keyed block's keys are operator names, not typos.
    let profiles = json!({"agents": {"profiles": {"blueprint": {"model": "opus"}}}});
    assert!(schema_walk::unknown_key_problems(schema, &leaves, &layer(profiles)).is_empty());
    // The wrong section names the key the operator meant.
    let moved = json!({"agents": {"max_lanes": 4}});
    assert_eq!(
        schema_walk::unknown_key_problems(schema, &leaves, &layer(moved)),
        vec!["agents.max_lanes (set in /cfg/config.toml) is not a modeled config key; did you mean parallel.max_lanes?"]
    );
    // A legacy spelling the loader lifts is honored, never unknown.
    let honored = json!({"recovery": {"retire_grace_s": 60}});
    assert!(schema_walk::unknown_key_problems(schema, &leaves, &layer(honored)).is_empty());
}

#[test]
fn an_out_of_enum_value_refuses_naming_file_key_value_and_legal_set() {
    let table = crate::config_schema::schema_table();
    let flat = json!({"recovery": {"watchdog": {"mode": "on"}}});
    let refused = schema_walk::refusals(&table["json_schema"], &flat);
    let text = schema_walk::describe(&refused, &[("/cfg/config.toml".to_string(), flat.clone())]);
    assert_eq!(
        text,
        "refusing to load settings:\n  /cfg/config.toml: recovery.watchdog.mode = 'on'; 'report', 'wake' or 'handoff'"
    );
    let ok = json!({"recovery": {"watchdog": {"mode": "wake"}}});
    assert!(schema_walk::refusals(&table["json_schema"], &ok).is_empty());
}

#[test]
fn the_post_merge_verdict_reads_the_repo_file_only() {
    let root = tempfile::TempDir::new().unwrap();
    let fno = root.path().join(".fno");
    std::fs::create_dir_all(&fno).unwrap();
    std::fs::write(fno.join("config.toml"), "[post_merge]\nenabled = false\n").unwrap();
    let verdict = post_merge::readiness(root.path());
    assert_eq!(
        verdict.summary_line(),
        "[doctor] post-merge: opted_out (config.post_merge.enabled=false)"
    );
    assert_eq!(
        verdict.to_json(),
        r#"{"status": "opted_out", "enabled": false, "activity": false, "parking_lot_path": null, "project_id": null, "cause": null, "note": null}"#
    );
    std::fs::write(fno.join("config.toml"), "[post_merge\n").unwrap();
    let broken = post_merge::readiness(root.path());
    assert_eq!(broken.status, "error");
    assert!(broken
        .cause
        .unwrap_or_default()
        .starts_with("TOMLDecodeError: "));
}

#[test]
fn python_spellings_match_repr_and_difflib() {
    assert_eq!(py_repr_str("it's"), "\"it's\"");
    assert_eq!(py_repr_str("a\"b"), "'a\"b'");
    assert_eq!(
        py_repr(&json!([1, 2.0, null, true])),
        "[1, 2.0, None, True]"
    );
    assert_eq!(
        schema_walk::close_matches("appel", &["ape", "apple", "peach", "puppy"], 5, 0.6),
        vec!["apple", "ape"]
    );
}
