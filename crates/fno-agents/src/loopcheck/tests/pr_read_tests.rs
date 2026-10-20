use super::*;

/// AC5-EDGE: an unexpected gh state string maps to PrState::None
/// (fail-closed), never panics.
#[test]
fn pr_state_unknown_string_fails_closed() {
    assert_eq!(PrState::from_gh_str("DRAFT"), PrState::None);
    assert_eq!(PrState::from_gh_str(""), PrState::None);
    assert_eq!(PrState::from_gh_str("open"), PrState::None);
}

/// AC5-UI: as_str/render reproduce the exact legacy fingerprint vocabulary.
#[test]
fn enum_rendering_byte_identical_to_legacy_strings() {
    assert_eq!(PrState::Open.as_str(), "OPEN");
    assert_eq!(PrState::Merged.as_str(), "MERGED");
    assert_eq!(PrState::Closed.as_str(), "CLOSED");
    assert_eq!(PrState::None.as_str(), "none");
    assert_eq!(CiConclusion::Success.render(), "SUCCESS");
    assert_eq!(
        CiConclusion::Failure(Some("lint".into())).render(),
        "FAILURE:lint"
    );
    assert_eq!(CiConclusion::Failure(None).render(), "FAILURE");
    assert_eq!(CiConclusion::Pending.render(), "PENDING");
    assert_eq!(CiConclusion::Skipped.render(), "skipped");
    assert_eq!(CiConclusion::None.render(), "none");
}

/// The manifest node's primary PR pins the terminal read's selector, and
/// every unreadable shape falls open to the branch-resolved read (None) -
/// the branch read is the only correct answer before the PR exists at all.
#[test]
fn manifest_node_selector_reads_the_primary_pr_and_fails_open() {
    let _guard = crate::claims::test_env_lock();
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    std::env::set_var("FNO_STATE_DIR", dir.path().join("state"));
    let graph = crate::graph_get::graph_path_for(&cwd);
    std::fs::write(&graph, b"{\"entries\": []}").unwrap();
    let connection = crate::backlog::open(&graph).unwrap();
    crate::backlog::save_aggregate(
        &connection,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-sel", "slug": "sel", "title": "T", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 930,
            "pr_url": "https://github.com/o/r/pull/930",
        }))
        .unwrap(),
    )
    .unwrap();
    // A second node carrying no PR: the branch-resolved read stays.
    crate::backlog::save_aggregate(
        &connection,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-bare", "slug": "bare", "title": "B", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        manifest_node_pr_selector(&cwd, Some("x-sel")).as_deref(),
        Some("930")
    );
    assert_eq!(manifest_node_pr_selector(&cwd, Some("x-bare")), None);
    // An unknown node and a missing manifest node both fail open.
    assert_eq!(manifest_node_pr_selector(&cwd, Some("x-absent")), None);
    assert_eq!(manifest_node_pr_selector(&cwd, None), None);
    std::env::remove_var("FNO_STATE_DIR");

    // A node whose attached rows disagree: a merged-stamped primary with a
    // later attached row picks the later PR, and an all-terminal node picks
    // the newest number. attach writes merge_status None, so an attached row
    // always outranks a stamped dead one. The second store lives under its
    // own state root, so the function under test reads it via the same
    // FNO_STATE_DIR door as the first.
    std::env::set_var("FNO_STATE_DIR", dir.path().join("state2"));
    let graph2 = crate::graph_get::graph_path_for(&cwd);
    std::fs::create_dir_all(graph2.parent().expect("graph path always has a parent")).unwrap();
    std::fs::write(&graph2, b"{\"entries\": []}").unwrap();
    let conn2 = crate::backlog::open(&graph2).unwrap();
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-multi", "slug": "multi", "title": "M", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "MERGED",
            "additional_prs": [
                {"number": 940, "url": "https://github.com/o/r/pull/940"},
                {"number": 930, "url": "https://github.com/o/r/pull/930", "merge_status": "MERGED"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();
    crate::backlog::save_aggregate(
        &conn2,
        &crate::backlog::model::Node::from_json(&serde_json::json!({
            "id": "x-reopen", "slug": "reopen", "title": "R", "type": "bug",
            "status": "in_progress", "priority": "p2", "domain": "code",
            "created_at": "2026-10-20T00:00:00+00:00",
            "pr_number": 900,
            "pr_url": "https://github.com/o/r/pull/900",
            "merge_status": "MERGED",
            "additional_prs": [
                {"number": 930, "url": "https://github.com/o/r/pull/930", "merge_status": "MERGED"},
            ],
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(
        manifest_node_pr_selector(&cwd, Some("x-multi")).as_deref(),
        Some("940")
    );
    assert_eq!(
        manifest_node_pr_selector(&cwd, Some("x-reopen")).as_deref(),
        Some("930")
    );
    std::env::remove_var("FNO_STATE_DIR");
}
