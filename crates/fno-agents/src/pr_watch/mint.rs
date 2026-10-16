//! The `pr-watch merge-cleanup-mint` arm: JSON in (the mint's keyword set
//! plus an optional `candidate_row_names`), JSON out ({request_id}). The
//! merge-cleanup mint's transport door; the logic is
//! `crate::merge_reap::emit_merge_cleanup_requested`.

use serde_json::{json, Value};

pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("pr-watch merge-cleanup-mint: no flags; the payload rides stdin");
        return 2;
    }
    let mut buf = String::new();
    if std::io::Read::read_to_string(&mut std::io::stdin(), &mut buf).is_err() {
        eprintln!("pr-watch merge-cleanup-mint: cannot read stdin");
        return 2;
    }
    let payload: Value = match serde_json::from_str(&buf) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-watch merge-cleanup-mint: bad payload: {e}");
            return 2;
        }
    };
    let str_field = |key: &str| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
    };
    let Some(pr) = payload.get("pr").and_then(Value::as_i64) else {
        eprintln!("pr-watch merge-cleanup-mint: pr (int) is required");
        return 2;
    };
    let Some(branch) = str_field("branch") else {
        eprintln!("pr-watch merge-cleanup-mint: branch is required");
        return 2;
    };
    let Some(repo) = str_field("repo") else {
        eprintln!("pr-watch merge-cleanup-mint: repo is required");
        return 2;
    };
    let project = str_field("project").unwrap_or("");
    let home = crate::paths::AgentsHome::from_env();
    let node_ids: Vec<String> = payload
        .get("node_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let candidates: Option<Vec<String>> = payload
        .get("candidate_row_names")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect::<Vec<String>>()
        });
    let request_id = crate::merge_reap::emit_merge_cleanup_requested(
        &home,
        repo,
        project,
        pr,
        branch,
        payload
            .get("worktree")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty()),
        &node_ids,
        payload.get("session_id").and_then(Value::as_str),
        payload.get("harness").and_then(Value::as_str),
        payload.get("merged_at").and_then(Value::as_str),
        candidates,
        payload.get("repo_slug").and_then(Value::as_str),
    );
    let out = json!({"request_id": request_id});
    println!("{out}");
    0
}
