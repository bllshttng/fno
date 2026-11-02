//! `fno-agents backlog script-door <op>`: the small facts the shell scripts and
//! the remaining Python callers read. Not a catalog command, so it adds nothing
//! to the menu. Every failed read exits nonzero: a caller never gets a clean
//! empty answer from a store it could not read.

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::collision::{
    has_file_surface, match_shared_root, partition, HIDDEN_SHARED_OUTPUT_ROOTS,
};
use super::node_match::{resolve_id, MatchKind};
use super::workflows::{active_backend_name, text_at};
use crate::graph_store;

const USAGE: &str = "usage: fno backlog script-door <op>
  rows [--graph PATH] [--include-archived]   every row, as one JSON array
  resolve-plan <query> [--graph PATH]        print a node's plan_path
  match <query> [--branch B]                 one loose-match answer as JSON
  has-file-surface <plan>                    exit 0 when the plan names files, else 7
  partition                                  stdin {items:[[id,[path...]]]} to the collision partition
  root-patterns <doc>                        the state-root inventory patterns, one per line";

fn flag_value<'a>(tail: &'a [String], flag: &str) -> Option<&'a str> {
    let at = tail.iter().position(|a| a == flag)?;
    tail.get(at + 1).map(String::as_str)
}

fn graph_of(tail: &[String]) -> PathBuf {
    flag_value(tail, "--graph")
        .filter(|g| !g.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(super::settings::graph_path)
}

fn live(row: &Value) -> bool {
    row.get("archived_at").is_none_or(Value::is_null)
}

fn run_rows(tail: &[String]) -> i32 {
    let graph = graph_of(tail);
    match graph_store::read_rows(&graph) {
        Ok(rows) => {
            let all = tail.iter().any(|a| a == "--include-archived");
            let rows: Vec<Value> = rows.into_iter().filter(|r| all || live(r)).collect();
            println!("{}", Value::Array(rows));
            0
        }
        Err(e) => {
            eprintln!("fno backlog script-door rows: the graph could not be read: {e}");
            1
        }
    }
}

/// The shell resolver's contract: 0 plan_path printed, 1 no match or unreadable
/// store, 3 no plan_path, 4 ambiguous, 6 external backend (the id is opaque, so
/// the caller passes it through). An explicit --graph is a sandbox and never
/// asks the backend.
fn run_resolve_plan(tail: &[String]) -> i32 {
    let Some(query) = tail.first().filter(|q| q.as_str() != "--graph") else {
        eprintln!("{USAGE}");
        return 2;
    };
    let pinned = flag_value(tail, "--graph").is_some_and(|g| !g.is_empty());
    if !pinned && active_backend_name() != "graph" {
        return 6;
    }
    let Ok(rows) = graph_store::read_rows(&graph_of(tail)) else {
        return 1;
    };
    let rows: Vec<Value> = rows.into_iter().filter(live).collect();
    let found = resolve_id(Some(query), &rows, None);
    match found.kind {
        MatchKind::Ambiguous => 4,
        MatchKind::None => 1,
        _ => {
            let plan = found
                .candidates
                .first()
                .and_then(|c| text_at(c, "plan_path"))
                .filter(|p| !p.is_empty());
            match plan {
                Some(plan) => {
                    print!("{plan}");
                    0
                }
                None => 3,
            }
        }
    }
}

fn run_partition() -> i32 {
    let mut raw = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw) {
        eprintln!("fno backlog script-door partition: stdin read failed: {e}");
        return 1;
    }
    let payload: Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fno backlog script-door partition: bad JSON: {e}");
            return 2;
        }
    };
    let mut items: Vec<(String, BTreeSet<String>)> = Vec::new();
    for item in payload["items"].as_array().map_or(&[][..], Vec::as_slice) {
        let id = item[0].as_str().unwrap_or_default().to_string();
        let paths = item[1]
            .as_array()
            .map(|p| {
                p.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        items.push((id, paths));
    }
    let (groups, unevaluated) = partition(&items, HIDDEN_SHARED_OUTPUT_ROOTS);
    let mut shared = serde_json::Map::new();
    for path in items.iter().flat_map(|(_, paths)| paths) {
        if let Some(root) = match_shared_root(path, HIDDEN_SHARED_OUTPUT_ROOTS) {
            shared.insert(path.clone(), json!(root));
        }
    }
    println!(
        "{}",
        json!({"groups": groups, "unevaluated": unevaluated, "shared_roots": shared})
    );
    0
}

pub fn run(tail: &[String]) -> i32 {
    let Some((op, rest)) = tail.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    match op.as_str() {
        "rows" => run_rows(rest),
        "resolve-plan" => run_resolve_plan(rest),
        "match" => super::node_match::run(rest),
        "has-file-surface" => match rest.first() {
            Some(plan) if has_file_surface(Path::new(plan)) => 0,
            Some(_) => 7,
            None => {
                eprintln!("{USAGE}");
                2
            }
        },
        "partition" => run_partition(),
        "root-patterns" => {
            let Some(doc) = rest.first() else {
                eprintln!("{USAGE}");
                return 2;
            };
            match std::fs::read_to_string(doc) {
                Ok(text) => {
                    for pattern in crate::state_root_drift::top_level_patterns(&text) {
                        println!("{pattern}");
                    }
                    0
                }
                Err(e) => {
                    eprintln!("fno backlog script-door root-patterns: cannot read {doc}: {e}");
                    2
                }
            }
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}
