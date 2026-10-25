//! Recorded-response tests for the tracker seam (mounted by `mod.rs`).

use super::github::GitHubTracker;
use super::linear::LinearTracker;
use super::*;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A hermetic env for tests that touch HOME-derived paths: FNO_CONFIG names an
/// empty file, so no real config resolves; HOME lands in the temp dir. Hold
/// the crate's env lock first so sibling tests do not read the overrides.
/// The returned guard restores the previous values on drop, so the overrides
/// never leak into sibling tests under parallel execution.
fn hermetic_env(tmp: &std::path::Path) -> EnvGuard {
    // Take the env lock before the first global pin: the save and the set
    // below are process-global writes, and a sibling test's config read must
    // not observe them mid-flight (the territory EnvGuard discipline).
    let lock = crate::claims::test_env_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let empty = tmp.join("empty.toml");
    std::fs::write(&empty, "").unwrap();
    let saved = [
        ("FNO_CONFIG", std::env::var("FNO_CONFIG").ok()),
        ("HOME", std::env::var("HOME").ok()),
        ("FNO_HOME", std::env::var("FNO_HOME").ok()),
        (
            "FNO_NO_CANONICAL_CONFIG",
            std::env::var("FNO_NO_CANONICAL_CONFIG").ok(),
        ),
    ];
    std::env::set_var("FNO_CONFIG", &empty);
    std::env::set_var("HOME", tmp);
    std::env::set_var("FNO_HOME", tmp);
    std::env::set_var("FNO_NO_CANONICAL_CONFIG", "1");
    EnvGuard { _lock: lock, saved }
}

struct EnvGuard {
    // Underscore-prefixed: held for the drop ordering, never read.
    _lock: std::sync::MutexGuard<'static, ()>,
    saved: [(&'static str, Option<String>); 4],
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.saved.iter() {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

struct FakeGh {
    view: (i32, String, String),
    list_open: (i32, String, String),
    list_closed: (i32, String, String),
}

impl github::GhRun for FakeGh {
    fn run(&self, args: &[String]) -> Result<(i32, String, String), String> {
        Ok(match (args[0].as_str(), args[1].as_str()) {
            ("issue", "view") => self.view.clone(),
            ("issue", "list") => {
                let closed = args
                    .iter()
                    .zip(args.iter().skip(1))
                    .any(|(a, v)| a == "--state" && v == "closed");
                if closed {
                    self.list_closed.clone()
                } else {
                    self.list_open.clone()
                }
            }
            _ => (1, String::new(), "unexpected gh call".into()),
        })
    }
}

/// A full in-memory tracker: candidates plus their sidecar maps.
struct FakeTracker {
    cands: Vec<Candidate>,
    sidecars: HashMap<String, serde_json::Map<String, Value>>,
    fail_list_open: bool,
}

impl Tracker for FakeTracker {
    fn name(&self) -> &str {
        "fake"
    }

    fn read(&self, id: &str) -> Result<TrackerNode, TrackerError> {
        self.cands
            .iter()
            .find(|c| c.node.id == id)
            .map(|c| c.node.clone())
            .ok_or_else(|| TrackerError::NotFound(id.to_string()))
    }

    fn list_open(&self) -> Result<Vec<Candidate>, TrackerError> {
        if self.fail_list_open {
            return Err(TrackerError::Backend(
                "gh issue list failed for o/r: network down".into(),
            ));
        }
        Ok(self.cands.clone())
    }

    fn list_closed_since(&self, _days: u32) -> Option<Result<Vec<Candidate>, TrackerError>> {
        Some(Ok(Vec::new()))
    }

    fn sidecar(&self, id: &str) -> Result<serde_json::Map<String, Value>, TrackerError> {
        Ok(self.sidecars.get(id).cloned().unwrap_or_default())
    }

    fn close(&self, _id: &str) -> Result<(), TrackerError> {
        Ok(())
    }
}

fn cand(id: &str, title: &str, blocked_by: &[&str]) -> Candidate {
    Candidate {
        node: TrackerNode {
            id: id.into(),
            title: Some(title.into()),
            state: State::Open,
            parent: None,
            blocked_by: blocked_by.iter().map(|s| (*s).into()).collect(),
            details: Some(format!("body of {id}")),
            url: Some(format!("https://x/{id}")),
            size: None,
        },
        priority: "p2".into(),
        rank: None,
        created_at: Some("2026-01-01T00:00:00Z".into()),
        closed_at: None,
    }
}

#[test]
fn ac1_snapshot_joins_sidecar_over_a_recorded_gh() {
    let dir = tempfile::tempdir().unwrap();
    let _env = hermetic_env(dir.path());
    // Sidecar file for o/r#1 with plan_path, cwd and two sessions.
    let sidecar_dir = dir.path().join(".fno/sidecar");
    std::fs::create_dir_all(&sidecar_dir).unwrap();
    let sc = json!({
        "id": "o/r#1",
        "plan_path": "/plans/one.md",
        "cwd": "/repo",
        "sessions": [{"phase": "execute", "session_id": "s1"}, {"phase": "execute", "session_id": "s2"}],
    });
    std::fs::write(
        sidecar_dir.join(format!("{}.json", crate::claims::encode_key("o/r#1"))),
        sc.to_string(),
    )
    .unwrap();
    let list_payload = json!([
        {"number": 1, "title": "First", "state": "OPEN",
         "createdAt": "2026-01-02T00:00:00Z", "body": "b1", "url": "u1"},
        {"number": 2, "title": "Second", "state": "OPEN",
         "createdAt": "2026-01-03T00:00:00Z", "body": "b2", "url": "u2"},
    ]);
    let gh = FakeGh {
        view: (0, String::new(), String::new()),
        list_open: (0, list_payload.to_string(), String::new()),
        list_closed: (0, "[]".into(), String::new()),
    };
    let t = GitHubTracker::new(Some("o/r".into()), Box::new(gh));
    let doc = snapshot::build(&t, "github").unwrap();
    let entries = doc.get("entries").and_then(Value::as_array).unwrap();
    assert_eq!(entries.len(), 2);
    let e1 = entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some("o/r#1"))
        .unwrap();
    assert_eq!(e1["title"], "First");
    assert_eq!(e1["state"], "open");
    assert_eq!(e1["created_at"], "2026-01-02T00:00:00Z");
    assert_eq!(e1["details"], "b1");
    assert_eq!(e1["url"], "u1");
    assert_eq!(e1["plan_path"], "/plans/one.md");
    assert_eq!(e1["cwd"], "/repo");
    assert_eq!(e1["sessions"].as_array().unwrap().len(), 2);
    let e2 = entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some("o/r#2"))
        .unwrap();
    assert_eq!(e2["title"], "Second");
    assert_eq!(e2["status"], "idea");
}

fn graph_fixture(dir: &std::path::Path, entries: Value) -> std::path::PathBuf {
    let path = dir.join("graph.json");
    crate::graph_store::seed_rows(&path, entries.as_array().expect("rows array")).unwrap();
    path
}

#[test]
fn ac4_graph_list_open_excludes_terminal_and_carries_ordering_inputs() {
    let dir = tempfile::tempdir().unwrap();
    // The readiness overlay resolves the global state root transitively; go
    // hermetic and declare the root so a sibling's legal env restore can never
    // land a real $HOME under this read (the p1 flake).
    let _env = hermetic_env(dir.path());
    crate::paths::pin_test_claims_root(dir.path());
    let path = graph_fixture(
        dir.path(),
        json!([
            {"id": "ab-done", "status": "done", "completed_at": "2026-01-01T00:00:00Z"},
            {"id": "ab-sup", "status": "superseded"},
            {"id": "ab-1", "status": "ready", "priority": "p0", "rank": 2.0,
             "created_at": "2026-01-02T00:00:00Z"},
            {"id": "ab-2", "status": "idea"},
        ]),
    );
    let t = graph::GraphTracker::with_path(path);
    let cands = t.list_open().unwrap();
    let ids: Vec<_> = cands.iter().map(|c| c.node.id.as_str()).collect();
    assert_eq!(ids, ["ab-1", "ab-2"]);
    let c1 = &cands[0];
    assert_eq!(c1.priority, "p0");
    assert_eq!(c1.rank, Some(2.0));
    assert_eq!(c1.created_at.as_deref(), Some("2026-01-02T00:00:00Z"));
    let c2 = &cands[1];
    assert_eq!(c2.priority, "p2");
    assert_eq!(c2.rank, None);
}

#[test]
fn ac2_stale_cache_serves_the_last_good_read_on_failure() {
    let dir = tempfile::tempdir().unwrap();
    let _env = hermetic_env(dir.path());
    std::env::set_var("FNO_TRACKER_GITHUB_REPO", "owner/a");
    let good = FakeTracker {
        cands: vec![cand("E-1", "One", &[])],
        sidecars: HashMap::new(),
        fail_list_open: false,
    };
    let bad = FakeTracker {
        cands: vec![],
        sidecars: HashMap::new(),
        fail_list_open: true,
    };
    // Prime the cache with one success, then fail with stale_ok: the cached
    // entries answer with stale_since and the failure rides in errors.
    let primed = snapshot::door_snapshot(&good, "github", false);
    assert!(primed.get("error").is_none());
    let doc = snapshot::door_snapshot(&bad, "github", true);
    assert!(doc.get("error").is_none(), "stale answer expected: {doc}");
    assert_eq!(
        doc.get("entries").unwrap().as_array().unwrap().len(),
        1,
        "the primed entry answers"
    );
    let taken = doc
        .get("stale_since")
        .and_then(Value::as_str)
        .expect("stale_since present");
    assert!(!taken.is_empty());
    assert!(doc
        .get("errors")
        .unwrap()
        .to_string()
        .contains("gh issue list failed"));
    // stale_ok=false refuses the cache.
    let refused = snapshot::door_snapshot(&bad, "github", false);
    assert!(refused.get("error").is_some());
}

#[test]
fn a_scope_switch_never_serves_another_scope_cache() {
    let dir = tempfile::tempdir().unwrap();
    let _env = hermetic_env(dir.path());
    std::env::set_var("FNO_TRACKER_GITHUB_REPO", "owner/a");
    let good = FakeTracker {
        cands: vec![cand("E-1", "One", &[])],
        sidecars: HashMap::new(),
        fail_list_open: false,
    };
    let bad = FakeTracker {
        cands: vec![],
        sidecars: HashMap::new(),
        fail_list_open: true,
    };
    snapshot::door_snapshot(&good, "github", false);
    // Switch scope to owner/b: no cache exists there, so the failure answers
    // the error, never owner/a's cached issues.
    std::env::set_var("FNO_TRACKER_GITHUB_REPO", "owner/b");
    let doc = snapshot::door_snapshot(&bad, "github", true);
    assert!(doc.get("error").is_some(), "expected error, got {doc}");
}

#[test]
fn door_ops_answer_refusals_in_the_payload() {
    // Unknown backend: exit-0 payload error naming the available set (AC7).
    let doc = run_door(&json!({"tracker": "read", "backend": "jira", "id": "X-1"}));
    assert_eq!(
        doc.get("error"),
        Some(&json!(
            "unknown tracker backend: jira. Available: graph, github, linear"
        ))
    );
    // Unknown op.
    let doc = run_door(&json!({"tracker": "nope", "backend": "graph"}));
    assert!(doc
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("")
        .starts_with("unknown tracker op"));
    // The graph backend refuses close: no second close path.
    let doc = run_door(&json!({"tracker": "close", "backend": "graph", "id": "ab-1"}));
    assert!(doc
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("")
        .contains("fno backlog done"));
    // An id carrying the claim-key partition character is refused.
    let doc = run_door(&json!({"tracker": "read", "backend": "graph", "id": "EX:1"}));
    assert!(doc
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or("")
        .contains("partition"));
}

/// A recorded Linear API: each POST is answered by the marker its query
/// document names. Pages pop in order, so a paginated list walks them.
struct FakeLinear {
    issue: (i32, String, String),
    open_pages: Mutex<Vec<String>>,
    states: (i32, String, String),
    mutation: (i32, String, String),
    sent: Arc<Mutex<Vec<String>>>,
}

impl linear::LinearHttp for FakeLinear {
    fn post(&self, body: &str) -> Result<(i32, String, String), String> {
        self.sent.lock().unwrap().push(body.to_string());
        if body.contains("issueUpdate") {
            return Ok(self.mutation.clone());
        }
        if body.contains("workflowStates") {
            return Ok(self.states.clone());
        }
        if body.contains("TeamOpenIssues") {
            let mut pages = self.open_pages.lock().unwrap();
            return match pages.is_empty() {
                true => Ok((0, String::new(), "no pages recorded".into())),
                false => Ok((0, pages.remove(0), String::new())),
            };
        }
        Ok(self.issue.clone())
    }
}

#[test]
fn a_linear_read_maps_the_node_spec_fields() {
    let issue = r#"{"data": {"issues": {"nodes": [{
        "identifier": "ENG-7", "title": "Ship it", "priority": 1,
        "createdAt": "2026-09-01T00:00:00Z",
        "description": "the issue body",
        "url": "https://linear.app/example/issue/ENG-7",
        "state": {"type": "started"},
        "parent": {"identifier": "ENG-1"},
        "estimate": {"value": 5.0},
        "blockedBy": {"nodes": [
            {"issue": {"identifier": "ENG-7"}, "relatedIssue": {"identifier": "ENG-2"}},
            {"issue": {"identifier": "ENG-3"}, "relatedIssue": {"identifier": "ENG-7"}}]}}]}}}"#;
    let t = LinearTracker::new(
        Some("key".into()),
        Some("ENG".into()),
        Box::new(FakeLinear {
            issue: (0, issue.into(), String::new()),
            open_pages: Mutex::new(vec![]),
            states: (0, "{}".into(), String::new()),
            mutation: (0, "{}".into(), String::new()),
            sent: Arc::new(Mutex::new(vec![])),
        }),
    );
    let node = t.read("ENG-7").unwrap();
    assert_eq!(node.id, "ENG-7");
    assert_eq!(node.title.as_deref(), Some("Ship it"));
    assert_eq!(node.state, State::Open);
    assert_eq!(node.parent.as_deref(), Some("ENG-1"));
    assert_eq!(node.blocked_by, ["ENG-2", "ENG-3"]);
    assert_eq!(node.details.as_deref(), Some("the issue body"));
    assert_eq!(
        node.url.as_deref(),
        Some("https://linear.app/example/issue/ENG-7")
    );
    assert_eq!(node.size.as_deref(), Some("M"));
}

#[test]
fn a_linear_tracker_without_the_api_key_refuses_naming_the_env_var() {
    let t = LinearTracker::new(
        None,
        Some("ENG".into()),
        Box::new(FakeLinear {
            issue: (0, "{}".into(), String::new()),
            open_pages: Mutex::new(vec![]),
            states: (0, "{}".into(), String::new()),
            mutation: (0, "{}".into(), String::new()),
            sent: Arc::new(Mutex::new(vec![])),
        }),
    );
    match t.read("ENG-7") {
        Err(TrackerError::Refused(msg)) => {
            assert!(msg.contains("FNO_TRACKER_LINEAR_API_KEY"), "{msg}")
        }
        other => panic!("expected Refused, got {:?}", other),
    }
}

/// Records argv and answers every `issue list` with one canned payload, so a
/// test can prove the Rust-side label check never leans on `gh` filtering.
struct RecordingGh {
    answer: (i32, String, String),
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl github::GhRun for RecordingGh {
    fn run(&self, args: &[String]) -> Result<(i32, String, String), String> {
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(args.to_vec());
        Ok(self.answer.clone())
    }
}

#[test]
fn list_labeled_keeps_only_both_label_issues_and_refuses_before_gh() {
    let issues = json!([
        {"number": 7, "title": "Add dark mode", "body": "please", "author": {"login": "visitor-1"},
         "labels": [{"name": "fno-request"}, {"name": "FNO-Approved"}]},
        {"number": 8, "title": "Case variant", "body": "b", "author": {"login": "visitor-2"},
         "labels": [{"name": "fno-request"}, {"name": "Fno-Approved"}]},
        {"number": 9, "title": "Approved only", "body": "c", "author": {"login": "visitor-3"},
         "labels": [{"name": "fno-approved"}]}
    ]);
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tracker = GitHubTracker::new(
        None,
        Box::new(RecordingGh {
            answer: (0, issues.to_string(), String::new()),
            calls: calls.clone(),
        }),
    );
    let got = tracker
        .list_labeled("o/r", "fno-request", "fno-approved", 100)
        .unwrap();
    assert_eq!(got.len(), 2, "only issues carrying both labels import");
    assert_eq!(got[0].number, 7);
    assert_eq!(got[0].login, "visitor-1");
    assert_eq!(got[1].number, 8, "label match ignores case");
    let argv = calls.lock().unwrap_or_else(|e| e.into_inner())[0].clone();
    let joined = argv.join(" ");
    assert!(joined.contains("--label fno-request"), "{joined}");
    assert!(joined.contains("--label fno-approved"), "{joined}");
    assert!(joined.contains("--state open"), "{joined}");
    assert!(
        joined.contains("--json number,title,body,author,labels"),
        "{joined}"
    );
    // A bad repo and equal labels refuse before any gh call.
    assert!(tracker
        .list_labeled("o/r --web", "fno-request", "fno-approved", 100)
        .is_err());
    assert!(tracker
        .list_labeled("o/r", "fno-request", "FNO-REQUEST", 100)
        .is_err());
    assert_eq!(
        calls.lock().unwrap_or_else(|e| e.into_inner()).len(),
        1,
        "the refusals must not reach gh"
    );
    let failing = GitHubTracker::new(
        None,
        Box::new(RecordingGh {
            answer: (1, String::new(), "boom".into()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }),
    );
    assert!(failing
        .list_labeled("o/r", "fno-request", "fno-approved", 100)
        .is_err());
}

#[test]
fn sidecar_save_round_trips_and_merges() {
    let tmp = tempfile::tempdir().unwrap();
    let mut first = serde_json::Map::new();
    first.insert("cwd".into(), json!("/tmp/x"));
    let path = sidecar::save(tmp.path(), "o/r#1", &first).unwrap();
    assert!(path.starts_with(tmp.path()));
    let mut second = serde_json::Map::new();
    second.insert("plan_path".into(), json!("/tmp/p.md"));
    sidecar::save(tmp.path(), "o/r#1", &second).unwrap();
    let got = sidecar::load(tmp.path(), "o/r#1").unwrap();
    assert_eq!(got.len(), 2);
    assert_eq!(got["cwd"], json!("/tmp/x"));
    assert_eq!(got["plan_path"], json!("/tmp/p.md"));
}

#[test]
fn sidecar_save_refuses_a_tracker_owned_key_before_any_write() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fields = serde_json::Map::new();
    fields.insert("title".into(), json!("x"));
    let err = sidecar::save(tmp.path(), "o/r#1", &fields).unwrap_err();
    assert!(err.to_string().contains("\"title\""), "{err}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

#[test]
fn sidecar_load_coerces_a_numeric_string_like_the_old_model() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fields = serde_json::Map::new();
    fields.insert("pr_number".into(), json!("7"));
    fields.insert("cost_usd".into(), json!("1.5"));
    sidecar::save(tmp.path(), "o/r#1", &fields).unwrap();
    let got = sidecar::load(tmp.path(), "o/r#1").unwrap();
    assert_eq!(got["pr_number"], json!(7));
    assert_eq!(got["cost_usd"], json!(1.5));
}

#[test]
fn sidecar_load_refuses_a_wrong_typed_field_loud() {
    let tmp = tempfile::tempdir().unwrap();
    let mut fields = serde_json::Map::new();
    fields.insert("pr_number".into(), json!("not-a-number"));
    sidecar::save(tmp.path(), "o/r#1", &fields).unwrap();
    let err = sidecar::load(tmp.path(), "o/r#1").unwrap_err();
    assert!(err.to_string().contains("sidecar invalid"), "{err}");
}

#[test]
fn sidecar_load_refuses_a_tracker_owned_key() {
    let tmp = tempfile::tempdir().unwrap();
    let name = format!("{}.json", crate::claims::encode_key("o/r#1"));
    std::fs::write(
        tmp.path().join(name),
        r#"{"id":"o/r#1","title":"tracker-owned"}"#,
    )
    .unwrap();
    let err = sidecar::load(tmp.path(), "o/r#1").unwrap_err();
    assert!(err.to_string().contains("title"), "{err}");
}
