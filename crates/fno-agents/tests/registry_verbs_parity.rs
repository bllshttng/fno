//! parity-stage: characterization
//! parity-oracle: fno.agents.whoami.resolve_self
//!
//! The name, whoami, registry-repair and heal-token verbs, frozen against
//! goldens captured from their Python legs (tests/golden/registry_verbs/
//! capture.py) before those legs were deleted. The fixtures here rebuild the
//! capture's homes and stores case for case. Exit codes and stderr compare
//! byte for byte everywhere; stdout does too, except heal-token's row, which
//! compares by field (the Python leg printed every dataclass field, the
//! native verb prints the row as stored).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

const SESSION_ID: &str = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d";
const OTHER_ID: &str = "7c1d4e9b-2a6f-4b8c-9d0e-5f1a2b3c4d5e";
const TWIN_ID: &str = "3f2b8c1a-0000-4000-8000-000000000001";
const CODEX_ID: &str = "019a2b3c-4d5e-7f60-8123-456789abcdef";

fn golden(case: &str, ext: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/registry_verbs")
        .join(format!("{case}.{ext}"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn scrub(bytes: &[u8], root: &Path) -> String {
    let text = String::from_utf8_lossy(bytes).into_owned();
    let canonical = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let text = text
        .replace(canonical.to_string_lossy().as_ref(), "<ROOT>")
        .replace(root.to_string_lossy().as_ref(), "<ROOT>");
    let stamp = regex::Regex::new(r"schema-repair-\d{8}T\d{6}Z").unwrap();
    stamp
        .replace_all(&text, "schema-repair-<STAMP>")
        .into_owned()
}

fn row(name: &str, harness: &str, sid: &str, root: &Path, extra: Value) -> Value {
    let mut base = json!({
        "name": name, "cwd": root.to_string_lossy(), "log_path": "",
        "harness": harness, "harness_session_id": sid, "status": "idle",
    });
    for (k, v) in extra.as_object().unwrap() {
        base[k] = v.clone();
    }
    base
}

fn seed(home: &Path, rows: Vec<Value>, version: u64) {
    std::fs::create_dir_all(home).unwrap();
    let doc = json!({"schema_version": version, "agents": rows});
    std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
}

fn claude_transcript(root: &Path, sid: &str, cwd: &str) {
    let project = root.join("claude-projects/-work-repo");
    std::fs::create_dir_all(&project).unwrap();
    let lines = [
        json!({"type": "summary"}),
        json!({"type": "user", "cwd": cwd, "timestamp": "2026-10-20T08:00:00.000Z"}),
        json!({"type": "assistant", "timestamp": "2026-10-20T08:05:30.000Z"}),
    ];
    let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(project.join(format!("{sid}.jsonl")), body).unwrap();
}

fn codex_rollout(root: &Path, sid: &str, cwd: &str) {
    let day = root.join("codex-sessions/2026/10/20");
    std::fs::create_dir_all(&day).unwrap();
    let meta = json!({"type": "session_meta", "timestamp": "2026-10-20T09:00:00.000Z",
                      "payload": {"id": sid, "cwd": cwd}});
    std::fs::write(
        day.join(format!("rollout-2026-10-20T09-00-00-{sid}.jsonl")),
        format!("{meta}\n"),
    )
    .unwrap();
}

struct Run<'a> {
    root: &'a Path,
    home: PathBuf,
    env: Vec<(&'a str, String)>,
    cwd: Option<PathBuf>,
}

impl Run<'_> {
    fn exec(&self, door: &str, args: &[&str]) -> (i32, String, String) {
        let root = self.root;
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
        cmd.env("FNO_AGENTS_HOME", &self.home)
            .env("FNO_HOME", root.join("state"))
            .env("FNO_STATE_DIR", root.join("state"))
            .env("FNO_CLAUDE_PROJECTS_DIR", root.join("claude-projects"))
            .env("FNO_CODEX_SESSIONS_DIR", root.join("codex-sessions"))
            .env("FNO_OPENCODE_STORAGE_DIR", root.join("opencode-storage"))
            .env("FNO_MUX_DIR", root.join("mux"))
            // The launcher stamp pair pins the harness ahead of the host's
            // process ancestry, so a run inside a harness and a CI run agree.
            .env("FNO_SESSION_HARNESS", "claude")
            .env("FNO_SESSION_PID", std::process::id().to_string())
            .envs(fno_agents::test_run::self_owner_env())
            .current_dir(self.cwd.clone().unwrap_or_else(|| root.to_path_buf()));
        for key in [
            "CLAUDE_CODE_SESSION_ID",
            "CODEX_THREAD_ID",
            "FNO_AGENT_SELF",
            "FNO_AGENT_SESSION",
            "FNO_AGENT_ROW_PENDING",
            "FNO_AGENT_HARNESS",
            "FNO_AGENT_PROVIDER",
            "FNO_AGENTS_NAME_MODEL",
            "CLAUDE_CONFIG_DIR",
        ] {
            cmd.env_remove(key);
        }
        for (key, value) in &self.env {
            cmd.env(key, value);
        }
        let out = cmd
            .args(["status", door])
            .args(args)
            .output()
            .expect("verb ran");
        (
            out.status.code().unwrap_or(-1),
            scrub(&out.stdout, root),
            scrub(&out.stderr, root),
        )
    }
}

fn fixture_root() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    for d in [
        "claude-projects",
        "codex-sessions",
        "opencode-storage",
        "mux",
        "state",
    ] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    (dir, root)
}

fn assert_case(case: &str, got: (i32, String, String)) {
    let (code, out, err) = got;
    assert_eq!(
        code.to_string(),
        golden(case, "exit").trim(),
        "{case} exit; stderr: {err}"
    );
    assert_eq!(out, golden(case, "out"), "{case} stdout");
    assert_eq!(err, golden(case, "err"), "{case} stderr");
}

#[test]
fn name_matches_the_python_goldens() {
    let (_dir, root) = fixture_root();
    let cases: &[(&str, &[&str], Option<&str>)] = &[
        (
            "name-legacy",
            &["target", "x-245d", "--slug", "port-verbs"],
            None,
        ),
        (
            "name-dispatch",
            &[
                "--source",
                "kl",
                "--command",
                "th",
                "x-245d",
                "--discriminator",
                "ab12",
            ],
            None,
        ),
        (
            "name-command-word",
            &["--command", "target", "x-1234"],
            None,
        ),
        ("name-model-env", &["--command", "t", "x-1"], Some("opus")),
        ("name-missing-node", &[], None),
        (
            "name-unknown-source",
            &["--source", "zz", "--command", "t", "x-1"],
            None,
        ),
        (
            "name-both-forms",
            &["target", "x-1", "--command", "t"],
            None,
        ),
        ("name-no-command", &["--source", "kl", "x-1"], None),
    ];
    for (case, args, model) in cases {
        let run = Run {
            root: &root,
            home: root.join("home-name"),
            env: model
                .map(|m| vec![("FNO_AGENTS_NAME_MODEL", m.to_string())])
                .unwrap_or_default(),
            cwd: None,
        };
        assert_case(case, run.exec("--name", args));
    }
}

#[test]
fn whoami_matches_the_python_goldens() {
    let (_dir, root) = fixture_root();
    let home = root.join("home-whoami");
    seed(
        &home,
        vec![
            row(
                "calm-otter",
                "codex",
                CODEX_ID,
                &root,
                json!({"status": "live"}),
            ),
            row(
                "lead-heron",
                "claude",
                SESSION_ID,
                &root,
                json!({"role_level": 1, "role_scope": "x-245d", "role_grantor": "Gavin"}),
            ),
        ],
        43,
    );
    let node_cwd = root.join("node-cwd");
    std::fs::create_dir_all(node_cwd.join(".fno")).unwrap();
    std::fs::write(
        node_cwd.join(".fno/target-state.md"),
        format!("---\nclaude_session_id: \"{SESSION_ID}\"\ngraph_node_id: x-245d\n---\n"),
    )
    .unwrap();
    let claude = ("CLAUDE_CODE_SESSION_ID", SESSION_ID.to_string());
    let cases: [(&str, Vec<(&str, String)>, &[&str], Option<PathBuf>); 5] = [
        ("whoami-unregistered", vec![], &[], None),
        (
            "whoami-env",
            vec![
                ("FNO_AGENT_SESSION", CODEX_ID.to_string()),
                ("FNO_AGENT_SELF", "calm-otter".to_string()),
            ],
            &["--json"],
            None,
        ),
        (
            "whoami-stale-env",
            vec![("FNO_AGENT_SESSION", OTHER_ID.to_string()), claude.clone()],
            &["--json"],
            None,
        ),
        ("whoami-fallback-role", vec![claude.clone()], &[], None),
        (
            "whoami-node",
            vec![claude.clone()],
            &["--json"],
            Some(node_cwd.clone()),
        ),
    ];
    for (case, env, args, cwd) in cases {
        let run = Run {
            root: &root,
            home: home.clone(),
            env,
            cwd,
        };
        assert_case(case, run.exec("--whoami", args));
    }
}

#[test]
fn registry_repair_matches_the_python_goldens() {
    let (_dir, root) = fixture_root();
    let poisoned = |root: &Path| {
        vec![
            row(
                "calm-otter",
                "codex",
                CODEX_ID,
                root,
                json!({"future_field": null, "future_list": []}),
            ),
            row("swift-heron", "claude", SESSION_ID, root, json!({})),
        ]
    };
    let clean = |root: &Path| vec![row("swift-heron", "claude", SESSION_ID, root, json!({}))];
    let cases: [(&str, &str, Vec<Value>, u64, &[&str]); 5] = [
        (
            "repair-dry",
            "home-repair-dry",
            poisoned(&root),
            99,
            &["--to", "43"],
        ),
        (
            "repair-apply",
            "home-repair-apply",
            poisoned(&root),
            99,
            &["--to", "43", "--apply"],
        ),
        (
            "repair-clean",
            "home-repair-clean",
            clean(&root),
            99,
            &["--to", "43"],
        ),
        (
            "repair-not-above",
            "home-repair-low",
            clean(&root),
            43,
            &["--to", "43"],
        ),
        (
            "repair-carries-data",
            "home-repair-data",
            vec![row(
                "calm-otter",
                "codex",
                CODEX_ID,
                &root,
                json!({"future_field": 0}),
            )],
            99,
            &["--to", "43", "--apply"],
        ),
    ];
    for (case, home, rows, version, args) in cases {
        let home = root.join(home);
        seed(&home, rows, version);
        let run = Run {
            root: &root,
            home,
            env: vec![],
            cwd: None,
        };
        assert_case(case, run.exec("--registry-repair", args));
    }
    // A missing --to is a usage error; the Click usage text was a Python
    // artifact, not a contract.
    let run = Run {
        root: &root,
        home: root.join("home-repair-data"),
        env: vec![],
        cwd: None,
    };
    assert_eq!(run.exec("--registry-repair", &[]).0, 2);
}

/// The fields a healed row must carry the way the Python leg's did.
fn assert_row_case(case: &str, got: (i32, String, String), keys: &[&str]) {
    let (code, out, err) = got;
    assert_eq!(
        code.to_string(),
        golden(case, "exit").trim(),
        "{case} exit; stderr: {err}"
    );
    assert_eq!(err, golden(case, "err"), "{case} stderr");
    let want: Value = serde_json::from_str(golden(case, "out").trim()).unwrap();
    let have: Value =
        serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("{case}: {e}: {out}"));
    for key in keys {
        assert_eq!(have.get(*key), want.get(*key), "{case} field {key}");
    }
}

#[test]
fn heal_token_matches_the_python_goldens() {
    let (_dir, root) = fixture_root();
    claude_transcript(&root, SESSION_ID, "/work/repo");
    codex_rollout(&root, CODEX_ID, "/work/other");
    let home = root.join("home-heal");
    seed(
        &home,
        vec![row(
            "busy-otter",
            "codex",
            CODEX_ID,
            &root,
            json!({"status": "live"}),
        )],
        43,
    );
    let reg = home.join("registry.json").to_string_lossy().into_owned();
    let run = Run {
        root: &root,
        home: home.clone(),
        env: vec![],
        cwd: None,
    };
    assert_case(
        "heal-miss",
        run.exec("--heal-token", &["deadbeef", "--registry", &reg]),
    );
    assert_case(
        "heal-not-shaped",
        run.exec("--heal-token", &["calm-otter", "--registry", &reg]),
    );
    assert_case(
        "heal-all-name-miss",
        run.exec(
            "--heal-token",
            &["nobody-here", "--registry", &reg, "--all-sources"],
        ),
    );
    assert_row_case(
        "heal-all-registry-hit",
        run.exec(
            "--heal-token",
            &[CODEX_ID, "--registry", &reg, "--all-sources"],
        ),
        &["name", "harness", "harness_session_id", "status", "cwd"],
    );

    let adopt_home = root.join("home-heal-adopt");
    seed(&adopt_home, vec![], 43);
    let adopt_reg = adopt_home
        .join("registry.json")
        .to_string_lossy()
        .into_owned();
    let run = Run {
        root: &root,
        home: adopt_home.clone(),
        env: vec![],
        cwd: None,
    };
    assert_row_case(
        "heal-adopt-claude",
        run.exec(
            "--heal-token",
            &[&SESSION_ID[..8], "--registry", &adopt_reg],
        ),
        &[
            "name",
            "cwd",
            "harness",
            "status",
            "origin",
            "short_id",
            "harness_session_id",
            "last_message_at",
        ],
    );

    let tomb_home = root.join("home-heal-tomb");
    seed(&tomb_home, vec![], 43);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        tomb_home.join("rm_tombstones.json"),
        json!([{"harness": "claude", "session_id": SESSION_ID, "removed_at": now}]).to_string(),
    )
    .unwrap();
    let tomb_reg = tomb_home
        .join("registry.json")
        .to_string_lossy()
        .into_owned();
    let run = Run {
        root: &root,
        home: tomb_home,
        env: vec![],
        cwd: None,
    };
    assert_case(
        "heal-tombstone",
        run.exec("--heal-token", &[SESSION_ID, "--registry", &tomb_reg]),
    );

    // The twin transcript lands last: before it the prefix is unique.
    claude_transcript(&root, TWIN_ID, "/work/repo");
    let amb_home = root.join("home-heal-amb");
    seed(&amb_home, vec![], 43);
    let amb_reg = amb_home
        .join("registry.json")
        .to_string_lossy()
        .into_owned();
    let run = Run {
        root: &root,
        home: amb_home,
        env: vec![],
        cwd: None,
    };
    assert_case(
        "heal-ambiguous",
        run.exec("--heal-token", &[&SESSION_ID[..8], "--registry", &amb_reg]),
    );
}
