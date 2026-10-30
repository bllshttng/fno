//! `fno backlog carveout`, driven through the binary on a throwaway checkout.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

struct Env {
    tmp: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("repo/.git")).unwrap();
        Self { tmp }
    }

    fn repo(&self) -> PathBuf {
        self.tmp.path().join("repo")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        cmd.envs(fno_agents::test_run::self_owner_env())
            .args(["backlog"])
            .args(args)
            .current_dir(self.repo());
        for (name, _) in std::env::vars() {
            if name.contains("SESSION_ID") || name == "CODEX_THREAD_ID" {
                cmd.env_remove(name);
            }
        }
        cmd.env("FNO_REPO_ROOT", self.repo())
            .env("FNO_SPACES_DIR", self.tmp.path().join("spaces"))
            .env("FNO_STATE_DIR", self.tmp.path().join("state"))
            .env("FNO_CLAIMS_ROOT", self.tmp.path().join("claims"))
            .env("FNO_AGENTS_HOME", self.tmp.path().join("agents-home"));
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).stdin(Stdio::null()).output().unwrap()
    }

    fn carveout(&self, args: &[&str]) -> Output {
        let mut full = vec!["carveout"];
        full.extend_from_slice(args);
        self.run(&full)
    }

    fn add(&self, kind: &str, description: &str) -> String {
        let out = self.carveout(&["add", "-k", kind, description]);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out).trim().to_string()
    }

    fn manifest(&self, body: &str) {
        let dir = self.repo().join(".fno");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("target-state.md"), body).unwrap();
    }

    fn rows(&self) -> Vec<Value> {
        let out = self.carveout(&["list", "--all", "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        stdout(&out)
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|n| n == name) {
            return Some(path);
        }
        if path.is_dir() {
            if let Some(hit) = find_file(&path, name) {
                return Some(hit);
            }
        }
    }
    None
}

#[test]
fn add_records_the_session_prints_only_the_id_and_refuses_bad_values() {
    let env = Env::new();
    env.manifest("---\nstatus: IN_PROGRESS\nsession_id: run-1\n---\nbody\n");
    let id = env.add("deferred", "skipped SSO wiring");
    assert!(id.starts_with("cv-") && id.len() == 11, "{id}");
    let rows = env.rows();
    assert_eq!(rows[0]["session_id"], "run-1");
    assert_eq!(rows[0]["description"], "skipped SSO wiring");
    assert!(rows[0]["ts"].as_str().unwrap().ends_with('Z'));

    for bad in [
        vec!["add", "-k", "bogus", "x"],
        vec!["add", "-k", "deferred", "--priority", "p9", "x"],
        vec!["add", "-k", "deferred", "--severity", "huge", "x"],
        vec!["add", "x"],
    ] {
        assert_eq!(env.carveout(&bad).status.code(), Some(2), "{bad:?}");
    }
    assert_eq!(env.rows().len(), 1);
}

#[test]
fn a_row_with_no_session_is_still_filed_and_the_warning_says_so() {
    let env = Env::new();
    let out = env.carveout(&["add", "--kind=oos-bug", "found a leak"]);
    assert!(out.status.success());
    assert!(stderr(&out).contains("no active session"));
    assert_eq!(env.rows()[0]["session_id"], Value::Null);
}

#[test]
fn list_scopes_to_the_session_and_always_banners_the_scope() {
    let env = Env::new();
    let orphan = env.add("oos-bug", "ownerless");
    let none = env.carveout(&["list"]);
    assert!(stderr(&none).contains("no active session; listing all 1 row(s)"));
    assert!(stdout(&none).contains(&orphan));

    env.manifest("---\nfno_id: mine\n---\n");
    let mine = env.add("deferred", "mine");
    env.manifest("---\nfno_id: theirs\n---\n");
    let theirs = env.add("deferred", "theirs");
    env.manifest("---\nfno_id: mine\n---\n");

    let scoped = env.carveout(&["list"]);
    assert!(stderr(&scoped).contains("scoped to session mine; showing 2 of 3 row(s)"));
    assert!(stdout(&scoped).contains(&mine) && stdout(&scoped).contains(&orphan));
    assert!(!stdout(&scoped).contains(&theirs));

    // An explicit filter asks a precise ownership question: no ownerless rows.
    let exact = env.carveout(&["list", "--session-id", "theirs"]);
    assert!(stdout(&exact).contains(&theirs) && !stdout(&exact).contains(&orphan));
    let both = env.carveout(&["list", "--session-id", "a", "--pr-number", "1"]);
    assert_eq!(both.status.code(), Some(2));
}

#[test]
fn update_keeps_the_id_and_refuses_what_it_cannot_apply() {
    let env = Env::new();
    let id = env.add("deferred", "worded badly");
    let other = env.add("deferred", "untouched");

    let out = env.carveout(&["update", &id, "-d", "worded well", "-k", "backfill"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), id);
    assert!(stderr(&out).contains("/fno:ship pr merged"));
    let rows = env.rows();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], id.as_str());
    assert_eq!(rows[0]["description"], "worded well");
    assert_eq!(rows[0]["kind"], "backfill");
    assert_eq!(rows[1]["id"], other.as_str());

    assert_eq!(env.carveout(&["update", &id]).status.code(), Some(2));
    assert_eq!(
        env.carveout(&["update", &id, "-d", "  "]).status.code(),
        Some(2)
    );
    let absent = env.carveout(&["update", "cv-deadbeef", "-d", "new"]);
    assert_eq!(absent.status.code(), Some(1));
    assert!(stderr(&absent).contains("not on the ledger"));
    assert_eq!(env.rows().len(), 2);
}

#[test]
fn resolve_counts_what_went_and_records_only_those_ids_for_a_reason() {
    let env = Env::new();
    let id = env.add("deferred", "residue");
    let out = env.carveout(&["resolve", &id, &id, "cv-absent", "-R", "test residue"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out).trim(), "resolved 1 carve-out(s)");
    assert!(stderr(&out).contains("resolved 1 of 2 requested"));
    assert!(env.rows().is_empty());

    let store = find_file(&env.tmp.path().join("spaces"), "events.db").expect("event store");
    let text = fno_agents::event_store::journal_text(&store.with_file_name("events.jsonl"), &[]);
    let event: Value = text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|e| e["type"] == "carveout_resolved")
        .expect("carveout_resolved event");
    assert_eq!(event["data"]["carveout_ids"], id.as_str());
    assert_eq!(event["data"]["reason"], "test residue");
}

#[test]
fn the_manifest_proves_the_owner_only_for_its_own_session() {
    let env = Env::new();
    env.manifest("---\nclaude_session_id: sess-1\n---\ngraph_node_id: x-own\n");
    let mut foreign = env.command(&["carveout", "add", "-k", "deferred", "foreign"]);
    foreign.env("CLAUDE_CODE_SESSION_ID", "sess-other");
    assert!(foreign.output().unwrap().status.success());
    let mut own = env.command(&["carveout", "add", "-k", "deferred", "own"]);
    own.env("CLAUDE_CODE_SESSION_ID", "sess-1");
    assert!(own.output().unwrap().status.success());
    let rows = env.rows();
    assert_eq!(rows[0]["node"], Value::Null);
    assert_eq!(rows[1]["node"], "x-own");
}

#[test]
fn the_python_read_door_filters_and_names_a_failed_read() {
    use std::io::Write;
    let env = Env::new();
    env.manifest("---\nfno_id: mine\n---\n");
    let id = env.add("deferred", "mine");
    env.add("oos-bug", "mine, other kind");

    let mut child = env
        .command(&["carveout", "_read"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let payload = serde_json::json!({
        "root": env.repo(), "kind": "deferred", "session_ids": ["mine"],
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(answer["carveouts"].as_array().unwrap().len(), 1);
    assert_eq!(answer["carveouts"][0]["id"], id.as_str());

    let session = env.carveout(&["_session"]);
    let answer: Value = serde_json::from_slice(&session.stdout).unwrap();
    assert_eq!(answer["session_id"], "mine");
}
