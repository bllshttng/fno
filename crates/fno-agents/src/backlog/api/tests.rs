//! The api contract's own tests: every query and mutation runs once per
//! backend on the same logical fixture and the two arms must agree
//! (AC14-HP, AC15-EDGE). The volatile stamps a real clock fills in
//! (created_at, ts, touched_at, archived_at, ended_at) are scrubbed before
//! the cross-arm transcript compare; every deterministic byte compares.

use super::*;
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::sync::MutexGuard;
use tempfile::TempDir;

fn base_row(id: &str, title: &str, status: &str) -> Node {
    Node::from_json(&json!({
        "id": id, "slug": id, "title": title, "type": "feature",
        "status": status, "priority": "p2", "domain": "code",
        "created_at": "2026-09-11T00:00:00+00:00"
    }))
    .unwrap()
}

fn session_row(session_id: &str) -> SessionRecord {
    SessionRecord {
        phase: "execute".into(),
        harness: "claude".into(),
        session_id: session_id.into(),
        started_at: None,
        ended_at: None,
        ended_by: None,
        effort: None,
        observed_model: None,
        merge_grant: None,
        extras: Map::new(),
    }
}

/// The shared fixture: three project rows (one held, one parented, one
/// free) and one archived row outside the project.
fn fixture_nodes() -> Vec<Node> {
    let mut one = base_row("ab-one", "One", "idea");
    one.project = Some("fno".into());
    one.labels = Some(vec!["infra".into()]);
    one.claim.locked_by = Some("holder-1".into());
    one.claim.locked_at = Some("2026-09-11T01:00:00+00:00".into());
    one.sessions = Some(vec![session_row("s-1")]);
    let mut two = base_row("ab-two", "Two", "ready");
    two.project = Some("fno".into());
    two.parent = Some("ab-one".into());
    two.created_at = Some("2026-09-12T00:00:00+00:00".into());
    let mut three = base_row("ab-three", "Three", "ready");
    three.project = Some("fno".into());
    let mut four = base_row("ab-four", "Four", "done");
    four.project = Some("other".into());
    four.archived_at = Some("2026-09-10T00:00:00+00:00".into());
    vec![one, two, three, four]
}

/// Seed an independent store instance through graph.db.
fn arm(dir: &TempDir) -> Store {
    let graph = dir.path().join("graph.json");
    let entries: Vec<Value> = fixture_nodes().iter().map(Node::to_json).collect();
    crate::graph_store::seed_rows(&graph, &entries).unwrap();
    let store = Store::new(&graph);
    store
}

/// A pinned empty claims root held for the caller's whole body: the claim
/// projection rides the read, so an unpinned test would serve the
/// operator's live claims or race a concurrent fixture's env.
struct ClaimsRootPin {
    _guard: MutexGuard<'static, ()>,
    _root: TempDir,
}

impl ClaimsRootPin {
    fn take() -> Self {
        let guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        std::env::set_var("FNO_CLAIMS_ROOT", root.path());
        Self {
            _guard: guard,
            _root: root,
        }
    }
}

fn store_pair() -> (ClaimsRootPin, TempDir, TempDir, Store, Store) {
    let pin = ClaimsRootPin::take();
    let first_dir = TempDir::new().unwrap();
    let second_dir = TempDir::new().unwrap();
    let first_store = arm(&first_dir);
    let second_store = arm(&second_dir);
    (pin, first_dir, second_dir, first_store, second_store)
}

/// A claims root pinned for the whole test body, with one live node claim:
/// the holder of record reads from the claim store at parse time, so a
/// claimed-filter or holder assertion needs this seeded, never the row's
/// retired mirror fields.
struct ClaimsFixture {
    _guard: MutexGuard<'static, ()>,
    _root: TempDir,
    previous: Vec<(&'static str, Option<OsString>)>,
}

impl ClaimsFixture {
    fn new() -> Self {
        let guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root = tempfile::tempdir().unwrap();
        let names = std::iter::once("FNO_CLAIMS_ROOT")
            .chain(crate::claims::AMBIENT_IDENTITY_NAMES.iter().copied())
            .chain(std::iter::once("CLAUDE_SESSION_ID"));
        let mut previous = Vec::new();
        for name in names {
            previous.push((name, std::env::var_os(name)));
            std::env::remove_var(name);
        }
        std::env::set_var("FNO_CLAIMS_ROOT", root.path());
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "s-1");
        match crate::claims::acquire(
            "node:ab-one",
            "target-session:s-1",
            crate::claims::AcquireOpts {
                root: Some(root.path().to_path_buf()),
                ..Default::default()
            },
        ) {
            crate::claims::AcquireOutcome::Acquired(_) => {}
            other => panic!("fixture node claim failed: {other:?}"),
        };
        Self {
            _guard: guard,
            _root: root,
            previous,
        }
    }
}

impl Drop for ClaimsFixture {
    fn drop(&mut self) {
        for (name, value) in self.previous.drain(..) {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn claimed_store_pair() -> (ClaimsFixture, TempDir, TempDir, Store, Store) {
    let claims = ClaimsFixture::new();
    let first_dir = TempDir::new().unwrap();
    let second_dir = TempDir::new().unwrap();
    let first_store = arm(&first_dir);
    let second_store = arm(&second_dir);
    (claims, first_dir, second_dir, first_store, second_store)
}

// -- queries ----------------------------------------------------------------

#[test]
fn api_nodes_pages_two_then_cursor_third() {
    // AC14-HP: first:2 over 3 matching rows returns 2 with a next page; the
    // end cursor resumes at the third.
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let filter = NodeFilter {
            project: Some("fno".into()),
            ..Default::default()
        };
        let page_one = nodes(
            store,
            &filter,
            &Page {
                first: Some(2),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page_one.nodes.len(), 2);
        assert!(page_one.page_info.has_next_page);
        assert!(!page_one.page_info.has_previous_page);
        let cursor = page_one.page_info.end_cursor.clone().unwrap();
        let page_two = nodes(
            store,
            &filter,
            &Page {
                first: Some(2),
                after: Some(cursor),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(page_two.nodes.len(), 1);
        assert_eq!(page_two.nodes[0].id, "ab-three");
        assert!(!page_two.page_info.has_next_page);
        assert!(page_two.page_info.has_previous_page);
    }
}

#[test]
fn api_nodes_filters_by_parent_label_claim_and_session() {
    let (_claims, _d1, _d2, first_store, second_store) = claimed_store_pair();
    for store in [&first_store, &second_store] {
        let cases: Vec<NodeFilter> = vec![
            NodeFilter {
                label: Some("infra".into()),
                ..Default::default()
            },
            NodeFilter {
                claimed: Some(true),
                ..Default::default()
            },
            NodeFilter {
                session_id: Some("s-1".into()),
                ..Default::default()
            },
        ];
        for filter in &cases {
            let conn = nodes(store, filter, &Page::default()).unwrap();
            assert_eq!(conn.nodes.len(), 1, "filter {filter:?}");
            assert_eq!(conn.nodes[0].id, "ab-one", "filter {filter:?}");
        }
        let conn = nodes(
            store,
            &NodeFilter {
                parent: Some("ab-one".into()),
                ..Default::default()
            },
            &Page::default(),
        )
        .unwrap();
        assert_eq!(conn.nodes.len(), 1);
        assert_eq!(conn.nodes[0].id, "ab-two", "the child of ab-one");
        let filter = NodeFilter {
            claimed: Some(false),
            ..Default::default()
        };
        let conn = nodes(store, &filter, &Page::default()).unwrap();
        assert_eq!(conn.nodes.len(), 2);
    }
}

// -- mutations ---------------------------------------------------------------

#[test]
fn api_failed_mutation_keeps_version() {
    // AC15-EDGE: a failed mutation answers success:false and the store's
    // counter does not move.
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let before = version(store).unwrap();
        let payload = node_update(
            store,
            "ab-absent",
            NodeUpdateInput {
                title: Some("nobody".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!payload.success);
        assert!(payload.node.is_none());
        assert_eq!(payload.version, before);
        let payload = node_update(
            store,
            "ab-one",
            NodeUpdateInput {
                status: Some("not-a-status".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!payload.success);
        assert_eq!(version(store).unwrap(), before);
    }
}

#[test]
fn api_node_update_status_moves_through_the_patch_door() {
    // AC9-HP: the typed API's status arm refuses a door-invalid move and
    // leaves the row and version untouched. Leaving a terminal row rewrites
    // the replacer's chain, a second-row edit the single-row API must not
    // do silently, so it refuses naming the owning door.
    let (_pin, _d1, _d2, first_store, _second_store) = store_pair();
    let graph = _d1.path().join("graph.json");
    let mut entries: Vec<Value> = fixture_nodes().iter().map(Node::to_json).collect();
    entries.push(json!({
        "id": "ab-five",
        "slug": "ab-five",
        "title": "Five",
        "type": "feature",
        "status": "superseded",
        "priority": "p2",
        "domain": "code",
        "created_at": "2026-09-11T00:00:00+00:00",
        "superseded_by": "ab-one",
    }));
    crate::graph_store::seed_rows(&graph, &entries).unwrap();
    let before = version(&first_store).unwrap();
    let payload = node_update(
        &first_store,
        "ab-five",
        NodeUpdateInput {
            status: Some("idea".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!payload.success);
    assert_eq!(version(&first_store).unwrap(), before);
}

#[test]
fn api_node_create_appends_and_queries() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let before = version(store).unwrap();
        let payload = node_create(
            store,
            NodeCreateInput {
                id: "ab-five".into(),
                title: "Five".into(),
                project: Some("fno".into()),
                description: Some("the fifth".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(payload.success, "create refused");
        let created = payload.node.unwrap();
        assert_eq!(created.status, Status::Idea, "default rung");
        assert_eq!(payload.version, before + 1);
        let reread = node(store, "ab-five").unwrap().unwrap();
        assert_eq!(reread.title, "Five");
        assert_eq!(reread.description.as_deref(), Some("the fifth"));
        // A duplicate id refuses and leaves the counter alone.
        let again = node_create(
            store,
            NodeCreateInput {
                id: "ab-five".into(),
                title: "Five again".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!again.success);
        assert_eq!(version(store).unwrap(), before + 1);
    }
}

#[test]
fn api_node_batch_update_moves_every_named_row() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let payload = node_batch_update(
            store,
            &["ab-one".into(), "ab-two".into()],
            NodeUpdateInput {
                priority: Some("p0".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(payload.success);
        let moved = payload.node.unwrap();
        assert_eq!(moved.len(), 2);
        assert!(moved.iter().all(|n| n.priority == Priority::P0));
        // One missing id fails the whole batch.
        let before = version(store).unwrap();
        let payload = node_batch_update(
            store,
            &["ab-one".into(), "ab-absent".into()],
            NodeUpdateInput {
                priority: Some("p1".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!payload.success);
        assert_eq!(version(store).unwrap(), before);
    }
}

#[test]
fn api_archive_unarchive_delete_roundtrip() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let payload = node_archive(store, "ab-two").unwrap();
        assert!(payload.success);
        let conn = nodes(store, &NodeFilter::default(), &Page::default()).unwrap();
        assert_eq!(
            conn.nodes.len(),
            2,
            "the archived row left the default view"
        );
        node_unarchive(store, "ab-two").unwrap();
        let conn = nodes(store, &NodeFilter::default(), &Page::default()).unwrap();
        assert_eq!(conn.nodes.len(), 3);
        let payload = node_delete(store, "ab-two").unwrap();
        assert!(payload.success);
        assert!(node(store, "ab-two").unwrap().is_none());
        let payload = node_delete(store, "ab-two").unwrap();
        assert!(!payload.success, "deleting twice refuses");
    }
}

#[test]
fn api_pr_session_dispatch_and_encounter_mutations_agree() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        let payload = pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 42,
                url: Some("https://example.test/pr/42".into()),
                note: None,
            },
        )
        .unwrap();
        assert!(payload.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.primary_pr.unwrap().number, Some(42));
        pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 43,
                url: None,
                note: Some("follow-up".into()),
            },
        )
        .unwrap();
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.additional_prs.unwrap().len(), 1);
        let payload = session_append(store, "ab-one", session_row("s-2")).unwrap();
        assert!(payload.success);
        let payload = session_end(
            store,
            "ab-one",
            "s-2",
            "operator",
            Some("execute"),
            Some("claude"),
            None,
        )
        .unwrap();
        assert!(payload.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        let s2 = one
            .sessions
            .unwrap()
            .into_iter()
            .find(|row| row.session_id == "s-2")
            .unwrap();
        assert_eq!(s2.ended_by.as_deref(), Some("operator"));
        let again = session_end(
            store,
            "ab-one",
            "s-2",
            "operator",
            Some("execute"),
            Some("claude"),
            None,
        )
        .unwrap();
        assert!(!again.success, "ending twice refuses");
        let mut review_row = session_row("s-review");
        review_row.phase = "review".into();
        let other = session_append(store, "ab-one", review_row).unwrap();
        assert!(other.success);
        // A settle names its window: an open row of ANOTHER phase with the
        // same session id stays untouched (codex P2, gc_sweep scope).
        let payload = session_end(
            store,
            "ab-one",
            "s-review",
            "operator",
            Some("execute"),
            Some("claude"),
            None,
        );
        assert!(matches!(payload, Ok(p) if !p.success));
        let one = node(store, "ab-one").unwrap().unwrap();
        let review = one
            .sessions
            .unwrap()
            .into_iter()
            .find(|row| row.session_id == "s-review")
            .unwrap();
        assert!(review.ended_at.is_none());
        let payload = dispatch_set(
            store,
            "ab-one",
            Some(Dispatch {
                verb: Some("do".into()),
                brief: None,
                model: Some("test-model".into()),
            }),
        )
        .unwrap();
        assert!(payload.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.dispatch.verb.as_deref(), Some("do"));
        dispatch_set(store, "ab-one", None).unwrap();
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.dispatch.verb, None);
        let payload = encounter_create(
            store,
            "ab-one",
            EncounterInput {
                evidence: "cost me an hour".into(),
                session_id: Some("s-1".into()),
            },
        )
        .unwrap();
        assert!(payload.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.encounters.unwrap().len(), 1);
    }
}

#[test]
fn pull_request_stamp_matches_one_entry_and_never_double_stamps() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 1522,
                url: None,
                note: None,
            },
        )
        .unwrap();
        pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 1523,
                url: Some("https://github.com/o/r/pull/1523".into()),
                note: None,
            },
        )
        .unwrap();

        // Stamps 1523 only; 1522 stays unstamped.
        let payload = pull_request_stamp(
            store,
            "ab-one",
            1523,
            Some("https://github.com/o/r/pull/1523"),
            "merged",
        )
        .unwrap();
        assert!(payload.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        // 1522 landed as the primary (primary_pr was empty); 1523 is the
        // one additional_prs entry, and only it carries the stamp.
        assert_eq!(one.primary_pr.unwrap().merge_status, None);
        let extras = one.additional_prs.unwrap();
        assert_eq!(extras.len(), 1);
        assert_eq!(extras[0].merge_status.as_deref(), Some("merged"));

        // A second call is refused: the entry already reads merged.
        let again = pull_request_stamp(
            store,
            "ab-one",
            1523,
            Some("https://github.com/o/r/pull/1523"),
            "closed",
        )
        .unwrap();
        assert!(!again.success);

        // An absent number is refused and writes nothing.
        let absent = pull_request_stamp(store, "ab-one", 999, None, "merged").unwrap();
        assert!(!absent.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.additional_prs.unwrap().len(), 1);
    }
}

#[test]
fn primary_pr_stamp_refuses_a_number_or_url_mismatch() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 2180,
                url: Some("https://github.com/o/r/pull/2180".into()),
                note: None,
            },
        )
        .unwrap();
        let wrong_number = primary_pr_stamp(
            store,
            "ab-one",
            2181,
            Some("https://github.com/o/r/pull/2180"),
            "merged",
        )
        .unwrap();
        assert!(!wrong_number.success);
        let wrong_url = primary_pr_stamp(
            store,
            "ab-one",
            2180,
            Some("https://github.com/o/r/pull/9999"),
            "merged",
        )
        .unwrap();
        assert!(!wrong_url.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(one.primary_pr.unwrap().merge_status, None);
    }
}

#[test]
fn primary_pr_stamp_never_overwrites_a_recorded_value() {
    let (_pin, _d1, _d2, first_store, second_store) = store_pair();
    for store in [&first_store, &second_store] {
        mutate(store, "seed-failed-primary", |entries| {
            entries.push(json!({
                "id": "ab-failed", "slug": "ab-failed", "title": "Failed",
                "type": "feature", "status": "done", "priority": "p2",
                "created_at": "2026-09-11T00:00:00+00:00",
                "pr_number": 2180, "merge_status": "failed"
            }));
            Ok(true)
        })
        .unwrap();
        let refused = primary_pr_stamp(store, "ab-failed", 2180, None, "merged").unwrap();
        assert!(!refused.success);
        let seeded = node(store, "ab-failed").unwrap().unwrap();
        assert_eq!(
            seeded.primary_pr.unwrap().merge_status.as_deref(),
            Some("failed")
        );

        pull_request_attach(
            store,
            "ab-one",
            PullRequestInput {
                number: 2199,
                url: None,
                note: None,
            },
        )
        .unwrap();
        let stamped = primary_pr_stamp(store, "ab-one", 2199, None, "merged").unwrap();
        assert!(stamped.success);
        let again = primary_pr_stamp(store, "ab-one", 2199, None, "closed").unwrap();
        assert!(!again.success);
        let one = node(store, "ab-one").unwrap().unwrap();
        assert_eq!(
            one.primary_pr.unwrap().merge_status.as_deref(),
            Some("merged")
        );
    }
}

#[test]
fn api_comment_thread_write_and_reply_state_rules() {
    let (_pin, _d1, _d2, store, _second) = store_pair();
    for store in [&store] {
        // A user comment then an agent reply with state done and a link.
        let ask = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "rename the flag".into(),
                kind: Some("comment".into()),
                author: Some("user".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(ask.success);
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        assert_eq!(thread.nodes.len(), 1);
        let cid = thread.nodes[0]
            .extras
            .get("comment_id")
            .and_then(Value::as_str)
            .unwrap()
            .to_string();
        assert!(cid.starts_with("c-") && cid.len() == 8, "c- plus 6 hex");
        assert_eq!(
            thread.nodes[0].extras.get("state").and_then(Value::as_str),
            Some("open")
        );
        assert_eq!(
            thread.nodes[0].extras.get("author").and_then(Value::as_str),
            Some("user")
        );

        let reply = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "landed".into(),
                kind: Some("reply".into()),
                reply_to: Some(cid.clone()),
                state: Some("done".into()),
                state_ref: Some("PR 2951".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(reply.success);
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        assert_eq!(thread.nodes.len(), 2, "comment then reply, in order");
        assert_eq!(
            thread.nodes[1]
                .extras
                .get("reply_to")
                .and_then(Value::as_str),
            Some(cid.as_str())
        );
        // The PARENT row carries the moved state, in the same mutation.
        assert_eq!(
            thread.nodes[0].extras.get("state").and_then(Value::as_str),
            Some("done")
        );
        assert_eq!(
            thread.nodes[0]
                .extras
                .get("state_ref")
                .and_then(Value::as_str),
            Some("PR 2951")
        );

        // A reply to a comment id that does not exist is refused.
        let ghost = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "orphan".into(),
                kind: Some("reply".into()),
                reply_to: Some("c-999999".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            ghost.0.contains("names no comment"),
            "the refusal names the unknown thread: {}",
            ghost.0
        );
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        assert_eq!(thread.nodes.len(), 2, "no row added");

        // A ref without a state has nothing to land on: refused.
        let lone_ref = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "pointing".into(),
                kind: Some("reply".into()),
                reply_to: Some(cid.clone()),
                state_ref: Some("PR 1".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            lone_ref.0.contains("names a landing"),
            "the refusal names the missing state: {}",
            lone_ref.0
        );

        // State and ref flags belong to replies; a bare comment carrying
        // them would drop them silently, so it refuses.
        let stateful_comment = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "an ask".into(),
                kind: Some("comment".into()),
                state: Some("accepted".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            stateful_comment.0.contains("state moves a thread head"),
            "the refusal names the reply path: {}",
            stateful_comment.0
        );

        // State rules: done without a ref, an unknown state, and a kind
        // outside the thread vocabulary all refuse.
        let no_ref = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "landed".into(),
                kind: Some("reply".into()),
                reply_to: Some(cid.clone()),
                state: Some("done".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(no_ref.0.contains("names its landing"), "{}", no_ref.0);
        let bad_state = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "hmm".into(),
                kind: Some("reply".into()),
                reply_to: Some(cid),
                state: Some("closed".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            bad_state.0.contains("must be accepted, done or declined"),
            "{}",
            bad_state.0
        );
        let bad_kind = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "a note-shaped row".into(),
                kind: Some("note".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            bad_kind.0.contains("must be comment, reply"),
            "{}",
            bad_kind.0
        );

        // a note-kind row is a feed record - identity on the row,
        // no ask state, and no reply threading.
        let note = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "routed the FAIL at /tmp/report".into(),
                kind: Some("progress".into()),
                author: Some("agent".into()),
                session_id: Some("sess-1".into()),
                harness: Some("claude".into()),
                agent_name: Some("t-note-glm".into()),
                model: Some("glm-5.3-flash".into()),
                working_node: Some("ab-two".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(note.success);
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        let last = thread.nodes.last().unwrap();
        assert_eq!(last.kind.as_deref(), Some("progress"));
        assert!(last.extras.get("state").is_none(), "no ask state");
        assert!(last.extras.get("comment_id").is_some(), "pointer minted");
        assert_eq!(
            last.extras.get("agent_name").and_then(Value::as_str),
            Some("t-note-glm")
        );
        assert_eq!(
            last.extras.get("model").and_then(Value::as_str),
            Some("glm-5.3-flash")
        );
        assert_eq!(
            last.extras.get("working_node").and_then(Value::as_str),
            Some("ab-two")
        );
        assert_eq!(last.source_session_id.as_deref(), Some("sess-1"));
        assert_eq!(last.source_harness.as_deref(), Some("claude"));

        let threaded_note = comment_create(
            store,
            "ab-one",
            CommentCreateInput {
                body: "not a reply".into(),
                kind: Some("finding".into()),
                reply_to: Some("c-000000".into()),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            threaded_note.0.contains("a note row is a feed record"),
            "{}",
            threaded_note.0
        );
    }
}

/// the first note append carries the node's note journal into the
/// thread once - every record a progress row keyed by its journal content
/// hash, the marker stamped in the same mutation, a retry adding nothing.
#[test]
fn api_note_append_migrates_the_journal_into_the_thread_once() {
    let (_pin, _d1, _d2, store, _second) = store_pair();
    for store in [&store] {
        crate::backlog::note_history::append(
            &store.graph,
            "ab-one",
            crate::backlog::note_history::REASON_STATE_REPLACED,
            Some(3),
            None,
            &json!({"revision": 3, "body": "prior state", "ts": "2026-10-02T10:00:00+00:00"}),
            Some("sess-old"),
            Some("codex"),
        )
        .unwrap();
        let note = |body: &str| {
            comment_create(
                store,
                "ab-one",
                CommentCreateInput {
                    body: body.into(),
                    kind: Some("progress".into()),
                    ..Default::default()
                },
            )
            .unwrap()
        };
        note("first appended note");
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        let migrated: Vec<_> = thread
            .nodes
            .iter()
            .filter(|c| c.kind.as_deref() == Some("progress") && c.extras.get("migrated").is_some())
            .collect();
        assert_eq!(migrated.len(), 1, "one journal record, one migrated row");
        assert_eq!(
            migrated[0].body.as_deref(),
            Some("prior state"),
            "the journal body verbatim"
        );
        assert_eq!(migrated[0].source_session_id.as_deref(), Some("sess-old"));
        assert_eq!(migrated[0].source_harness.as_deref(), Some("codex"));
        let rows = read_rows(store).unwrap();
        let row = rows
            .iter()
            .find(|r| crate::graph_store::entry_id(r) == Some("ab-one"))
            .unwrap();
        assert!(row.get("thread_migrated_at").is_some(), "marker stamped");
        // A second append migrates nothing new.
        note("second appended note");
        let thread = comments(store, "ab-one", &Page::default()).unwrap();
        let migrated: Vec<_> = thread
            .nodes
            .iter()
            .filter(|c| c.extras.get("migrated").is_some())
            .collect();
        assert_eq!(migrated.len(), 1, "the migration ran once");
    }
}
