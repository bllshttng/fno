//! parity-stage: characterization
//! parity-oracle: fno.agents.role.promote_existing_session
//!
//! The role leaf (`status --role`, the `fno agents org promote` door) over a
//! sandboxed home: one config with two projects and a registry the case
//! seeds. The refusal texts are the Python leg's, which the deleted
//! test_role.py pinned; the grant case checks the receipt and the stamped
//! row. The environment is cleared, so the caller is an attended human.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

struct World {
    root: PathBuf,
}

impl World {
    fn new(case: &str, rows: Value) -> Self {
        let root = std::env::temp_dir().join(format!("role-parity-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".fno")).unwrap();
        std::fs::create_dir_all(root.join("agents")).unwrap();
        std::fs::write(
            root.join(".fno/config.toml"),
            "[work.workspaces.w]\nprojects = [{ name = \"alpha\", path = \"/tmp/alpha\" }, \
             { name = \"beta\", path = \"/tmp/beta\" }]\n",
        )
        .unwrap();
        let doc = json!({"schema_version": 43, "agents": rows});
        std::fs::write(root.join("agents/registry.json"), doc.to_string()).unwrap();
        World { root }
    }

    fn run(&self, argv: &[&str]) -> (i32, String, String) {
        let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
            .args(["status", "--role"])
            .args(argv)
            .env_clear()
            .envs(fno_agents::test_run::self_owner_env())
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("FNO_HOME", self.root.join(".fno"))
            .env("FNO_STATE_DIR", self.root.join(".fno"))
            .env("FNO_SPACES_DIR", self.root.join("spaces"))
            .env("FNO_AGENTS_HOME", self.root.join("agents"))
            // The term-command mail is a real send; a grant case must not
            // deliver anywhere, so the sender is a binary that refuses.
            .env("FNO_AGENTS_BIN", "/usr/bin/false")
            .current_dir(&self.root)
            .output()
            .unwrap();
        (
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn row(&self, name: &str) -> Value {
        let text = std::fs::read_to_string(self.root.join("agents/registry.json")).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        doc["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name)
            .cloned()
            .unwrap()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(Path::new(&self.root));
    }
}

fn row(name: &str, status: &str, sid: &str) -> Value {
    json!({"name": name, "cwd": "/tmp/alpha", "log_path": "", "harness": "claude",
           "status": status, "harness_session_id": sid, "short_id": &sid[..4]})
}

#[test]
fn a_grant_needs_a_handle_and_a_scope() {
    let w = World::new("usage", json!([]));
    let (code, _, err) = w.run(&["t"]);
    assert_eq!(code, 2);
    assert_eq!(err, "role: HANDLE and at least one --scope are required\n");
}

#[test]
fn an_unknown_handle_names_the_list_command() {
    let w = World::new("unknown", json!([]));
    let (code, _, err) = w.run(&["zz", "--scope", "alpha"]);
    assert_eq!(code, 2);
    assert!(err.starts_with("role: no agent matching 'zz';"), "{err}");
    assert!(
        err.trim_end()
            .ends_with("`fno agents list` shows every handle you can role."),
        "{err}"
    );
}

#[test]
fn a_terminal_target_names_the_stale_snapshot() {
    let w = World::new("terminal", json!([row("t", "exited", "aaaa1111-0000")]));
    let (code, _, err) = w.run(&["t", "--scope", "alpha"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("its STORED status is 'exited', which is terminal"),
        "{err}"
    );
}

#[test]
fn half_a_role_is_refused_not_overwritten() {
    let mut half = row("t", "idle", "aaaa1111-0000");
    half["role_scope"] = json!("beta");
    let w = World::new("half", json!([half]));
    let (code, _, err) = w.run(&["t", "--scope", "alpha"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("it holds half a role (level=None, scope='beta')"),
        "{err}"
    );
}

#[test]
fn a_second_live_holder_is_refused_with_the_ways_out() {
    let mut holder = row("h", "idle", "bbbb2222-0000");
    holder["role_scope"] = json!("alpha");
    holder["role_level"] = json!(1);
    let w = World::new("holder", json!([holder, row("t", "idle", "aaaa1111-0000")]));
    let (code, _, err) = w.run(&["t", "--scope", "alpha"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("scope 'alpha' is already held by live row 'h'"),
        "{err}"
    );
    assert!(err.contains("fno agents org promote h --scope"), "{err}");
    assert!(w.row("t").get("role_scope").is_none_or(Value::is_null));
}

#[test]
fn an_attended_human_grants_a_project_role_in_place() {
    let w = World::new("grant", json!([row("t", "idle", "aaaa1111-0000")]));
    let (code, out, err) = w.run(&["t", "--scope", "alpha"]);
    assert_eq!(code, 0, "{err}");
    let receipt: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(receipt["promoted"], "t");
    assert_eq!(receipt["level"], 1);
    assert_eq!(receipt["scope"], "alpha");
    assert_eq!(receipt["grantor"], "human");
    assert!(receipt["vacated_scope"].is_null());
    let stamped = w.row("t");
    assert_eq!(stamped["role_scope"], "alpha");
    assert_eq!(stamped["role_level"], 1);
    assert_eq!(stamped["role_grantor"], "human");
}

#[test]
fn a_reclaim_from_a_human_shell_needs_the_holder() {
    let w = World::new("reclaim", json!([]));
    let (code, _, err) = w.run(&["--reclaim"]);
    assert_eq!(code, 2);
    assert!(
        err.starts_with("role reclaim: reclaim needs the current promoted holder"),
        "{err}"
    );
}
