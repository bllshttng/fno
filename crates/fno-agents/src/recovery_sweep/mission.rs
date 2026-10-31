//! Did a worker's mission finish, per the node's external artifacts? Transcript
//! truth only says the stream is terminal; a `<promise>` reads done whether or
//! not anything shipped.

use super::{node_id_from_worktree, truthy, Candidate};
use crate::naming::parse_dispatch_agent_name;
use serde_json::Value;
use std::path::Path;

/// Think reasons a node's artifacts cannot certify: they predate the worker.
const NON_BIRTH_THINK_REASONS: [&str; 3] = ["work-start", "retro", "conversational"];

fn mission_kind(command: &str) -> Option<&'static str> {
    match command {
        "t" | "f" => Some("target"),
        "bp" | "r" | "th" => Some("think"),
        _ => None,
    }
}

/// A minted name carries the node hex without its prefix; only the graph knows
/// the prefix. A hex with no unique full id stays bare.
fn full_node(node: String, hex_node: &dyn Fn(&str) -> Option<String>) -> String {
    if !node.is_empty() && node.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        return hex_node(&node).unwrap_or(node);
    }
    node
}

/// `(node_id, kind)` for a convention-named worker. Names are a convention, so
/// this only backs up the manifest read, and a miss never guesses.
pub fn parse_worker_mission(
    name: Option<&str>,
    hex_node: &dyn Fn(&str) -> Option<String>,
) -> Option<(String, &'static str)> {
    let parsed = parse_dispatch_agent_name(name)?;
    let node = parsed.node.filter(|n| !n.is_empty())?;
    let kind = mission_kind(&parsed.command)?;
    Some((full_node(node, hex_node), kind))
}

fn think_tail(name: &str, node_id: &str, hex_node: &dyn Fn(&str) -> Option<String>) -> String {
    if let Some(tail) = name.strip_prefix(&format!("think-{node_id}-")) {
        return tail.to_string();
    }
    match parse_dispatch_agent_name(Some(name)) {
        Some(parsed)
            if parsed
                .node
                .clone()
                .map(|n| full_node(n, hex_node))
                .as_deref()
                == Some(node_id) =>
        {
            parsed.tail
        }
        _ => String::new(),
    }
}

/// `None` is unverifiable, and the caller keeps the terminal skip. The manifest
/// answers first because the runtime wrote it; the exception is a think-named
/// worker, which spawns on the node's canonical root where an unrelated
/// session's manifest can sit.
pub fn mission_complete_with(
    candidate: &Candidate,
    row_of: &dyn Fn(&str) -> Option<Value>,
    hex_node: &dyn Fn(&str) -> Option<String>,
) -> Option<bool> {
    let parsed = parse_worker_mission(candidate.name.as_deref(), hex_node);
    let reads_manifest = parsed.as_ref().map_or(true, |(_, kind)| *kind != "think");
    let manifest_node = candidate
        .cwd
        .as_deref()
        .filter(|_| reads_manifest)
        .and_then(node_id_from_worktree);
    let (node_id, kind) = match manifest_node {
        Some(node) => (node, "target"),
        None => parsed?,
    };
    let entry = row_of(&node_id)?;
    let field = |key: &str| entry.get(key).unwrap_or(&Value::Null);
    if truthy(field("archived_at")) {
        return None;
    }
    if kind == "think" {
        let tail = think_tail(candidate.name.as_deref().unwrap_or(""), &node_id, hex_node);
        if NON_BIRTH_THINK_REASONS
            .iter()
            .any(|reason| tail == *reason || tail.starts_with(&format!("{reason}-")))
        {
            return None;
        }
    }
    if field("status").as_str() == Some("done") {
        return Some(true);
    }
    if kind == "think" {
        let plan = field("plan_path");
        return Some(truthy(plan) && plan.as_str().map_or(true, |p| !p.trim().is_empty()));
    }
    // A PR existing is the completion floor; red CI and pending bots belong to
    // the PR watch, not this sweep.
    Some(truthy(field("pr_number")) || truthy(field("pr_url")))
}

/// One graph row by exact id, archived rows included.
pub(crate) fn graph_row(graph: &Path, node_id: &str) -> Option<Value> {
    let query = crate::backlog::RowQuery {
        filter: crate::backlog::api::NodeFilter {
            id_in: Some(vec![node_id.to_string()]),
            ..Default::default()
        },
        ..Default::default()
    };
    crate::graph_store::read_rows_where(graph, &query)
        .ok()?
        .into_iter()
        .find(|row| row.get("id").and_then(Value::as_str) == Some(node_id))
}

/// The unique `<prefix>-<hex>` id for a bare hex, or `None`.
pub(crate) fn graph_hex_node(graph: &Path, hex: &str) -> Option<String> {
    let suffix = format!("-{hex}");
    let rows = crate::graph_store::read_rows(graph).ok()?;
    let mut hits = rows
        .iter()
        .filter_map(|row| row.get("id").and_then(Value::as_str))
        .filter(|id| id.ends_with(&suffix));
    let first = hits.next()?;
    hits.next().is_none().then(|| first.to_string())
}
