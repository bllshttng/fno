//! x-26bd wave 1: the findings aggregate and its API, store-level.

use fno_agents::backlog::api::{self, FindingInput, Store};
use serde_json::json;
use std::path::PathBuf;

fn fixture(id: &str) -> serde_json::Value {
    json!({
        "id": id,
        "slug": format!("slug-{id}"),
        "title": format!("Node {id}"),
        "type": "feature",
        "status": "ready",
        "priority": "p1",
        "details": "premise",
    })
}

fn temp_graph(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let graph = dir.path().join(format!("{tag}-graph.json"));
    fno_agents::graph_store::seed_rows(&graph, &[fixture("x-f1"), fixture("x-f2")]).unwrap();
    (dir, graph)
}

fn input(body: &str) -> FindingInput {
    FindingInput {
        body: body.to_string(),
        ..Default::default()
    }
}

#[test]
fn finding_create_readback_and_list_roundtrip() {
    let (_dir, graph) = temp_graph("create");
    let store = Store::new(&graph);
    let receipt = api::finding_create(&store, "x-f1", input("gate holds")).unwrap();
    assert_eq!(receipt.finding_id.len(), 8);
    assert_eq!(receipt.node_id, "x-f1");

    // A second store instance reads the same id as open (AC1 readback).
    let later = api::findings(&Store::new(&graph), Some("x-f1"), true).unwrap();
    assert_eq!(later.len(), 1);
    assert_eq!(later[0].finding_id, receipt.finding_id);
    assert_eq!(later[0].body, "gate holds");
    assert!(later[0].resolved_at.is_none());

    // Resolve clears it from the open list, keeps it in the full list.
    let resolve = api::finding_resolve(&store, &receipt.finding_id, Some("sess-1")).unwrap();
    assert_eq!(resolve.status, "resolved");
    let open = api::findings(&store, Some("x-f1"), true).unwrap();
    assert!(open.is_empty());
    let all = api::findings(&store, Some("x-f1"), false).unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].resolved_by_session_id.as_deref(), Some("sess-1"));
}

#[test]
fn finding_create_refuses_empty_over_limit_and_unknown_node() {
    let (_dir, graph) = temp_graph("refuse");
    let store = Store::new(&graph);

    let empty = api::finding_create(&store, "x-f1", input("   \n  "));
    assert!(empty.is_err());

    let long = "x".repeat(5001);
    let over = api::finding_create(&store, "x-f1", input(&long));
    assert!(over.is_err(), "5001 chars must refuse");
    let at_limit = api::finding_create(&store, "x-f1", input(&"x".repeat(5000)));
    assert!(at_limit.is_ok(), "5000 chars must pass");

    let unknown = api::finding_create(&store, "x-none", input("ghost"));
    assert!(unknown.is_err());
}

#[test]
fn second_resolve_reports_already_resolved() {
    let (_dir, graph) = temp_graph("already");
    let store = Store::new(&graph);
    let receipt = api::finding_create(&store, "x-f1", input("first")).unwrap();
    let first = api::finding_resolve(&store, &receipt.finding_id, Some("s1")).unwrap();
    let second = api::finding_resolve(&store, &receipt.finding_id, Some("s2")).unwrap();
    assert_eq!(second.status, "already_resolved");
    assert_eq!(second.resolved_at, first.resolved_at);
}
