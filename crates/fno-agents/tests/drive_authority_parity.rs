//! parity-stage: characterization
//! parity-oracle: cli/src/fno/drive_authority.py
//!
//! Characterization for the native `drive-authority` command, frozen against
//! goldens captured from the Python detector (`active_drive_sessions` in
//! cli/src/fno/drive_authority.py) BEFORE that module deleted. Each case
//! builds an agents home of `state.json` fixtures, runs the binary's
//! `drive-authority --json` against it under `FNO_AGENTS_HOME`, and asserts
//! the frozen (exit, stdout) pair. The JSON line is the byte the Python leg's
//! own `json.dumps` shape produces, so the frozen golden pins the detector's
//! decisions, its field order, and its truthiness rules at once.
//!
//! Capture mode (FNO_CAPTURE_GOLDEN=1, only meaningful while the Python leg
//! still exists) shells `active_drive_sessions` on the same fixture and
//! asserts Rust==Python at freeze time. Once the module deletes, capture
//! fails loudly instead of silently re-freezing, because a golden can only
//! be captured from a live leg.
//!
//! The command's human lines and refusals have no Python counterpart (the
//! Python leg was a library function), so the last test pins them directly.

mod common;

use common::{assert_golden, capture_mode, slug, Golden};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repo root: crates/fno-agents -> repo.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// One agent dir under the home: a name plus the `state.json` body. `None`
/// writes the dir with no state file; `FILE` writes a plain file instead of
/// a dir.
struct AgentFixture {
    name: &'static str,
    body: Option<String>,
}

fn agent(name: &'static str, body: impl Into<String>) -> AgentFixture {
    AgentFixture {
        name,
        body: Some(body.into()),
    }
}

struct Case {
    label: &'static str,
    agents: Vec<AgentFixture>,
    /// The home dir itself is not created when false (the absent-home case).
    make_home: bool,
}

fn active_body(mode: &str) -> String {
    format!(
        r#"{{"short_id": "ag1", "pty": {{"drive_active": true, "drive_mode": "{mode}", "drive_session_id": "sess-1"}}}}"#
    )
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "active interactive window",
            agents: vec![agent("ag1", active_body("interactive"))],
            make_home: true,
        },
        Case {
            label: "active step window",
            agents: vec![agent("ag1", active_body("step"))],
            make_home: true,
        },
        Case {
            label: "active paranoid window",
            agents: vec![agent("ag1", active_body("paranoid"))],
            make_home: true,
        },
        Case {
            label: "watch window is excluded",
            agents: vec![agent("ag1", active_body("watch"))],
            make_home: true,
        },
        Case {
            label: "false drive flag is excluded",
            agents: vec![agent(
                "ag1",
                r#"{"short_id": "ag1", "pty": {"drive_active": false, "drive_mode": "interactive", "drive_session_id": "sess-1"}}"#,
            )],
            make_home: true,
        },
        Case {
            label: "missing drive flag is excluded",
            agents: vec![agent(
                "ag1",
                r#"{"short_id": "ag1", "pty": {"drive_mode": "interactive"}}"#,
            )],
            make_home: true,
        },
        // Python truthiness: a non-empty string is truthy, the number zero
        // is not. The Rust twin mirrors `not pty.get("drive_active")`.
        Case {
            label: "string drive flag is truthy",
            agents: vec![agent(
                "ag1",
                r#"{"short_id": "ag1", "pty": {"drive_active": "yes", "drive_mode": "interactive", "drive_session_id": "sess-1"}}"#,
            )],
            make_home: true,
        },
        Case {
            label: "zero drive flag is falsy",
            agents: vec![agent(
                "ag1",
                r#"{"short_id": "ag1", "pty": {"drive_active": 0, "drive_mode": "interactive"}}"#,
            )],
            make_home: true,
        },
        Case {
            label: "short id falls back to the dir name",
            agents: vec![agent(
                "ag1",
                r#"{"pty": {"drive_active": true, "drive_mode": "interactive", "drive_session_id": "sess-1"}}"#,
            )],
            make_home: true,
        },
        Case {
            label: "missing drive session id reads null",
            agents: vec![agent(
                "ag1",
                r#"{"short_id": "ag1", "pty": {"drive_active": true, "drive_mode": "step"}}"#,
            )],
            make_home: true,
        },
        Case {
            label: "corrupt state json is skipped",
            agents: vec![agent("ag1", "not json at all")],
            make_home: true,
        },
        Case {
            label: "missing state json is skipped",
            agents: vec![AgentFixture {
                name: "ag1",
                body: None,
            }],
            make_home: true,
        },
        Case {
            label: "dot dir is skipped",
            agents: vec![agent(".hidden", active_body("interactive"))],
            make_home: true,
        },
        Case {
            label: "plain file entry is skipped",
            agents: vec![AgentFixture {
                name: "loose",
                body: Some(active_body("interactive")),
            }],
            make_home: true,
        },
        Case {
            label: "agents sort by dir name",
            agents: vec![
                agent("zeta", active_body("interactive")),
                agent("alpha", active_body("step")),
            ],
            make_home: true,
        },
        Case {
            label: "empty agents home reads empty",
            agents: vec![],
            make_home: true,
        },
        Case {
            label: "absent agents home reads empty",
            agents: vec![],
            make_home: false,
        },
    ]
}

fn build_home(root: &Path, case: &Case) -> PathBuf {
    let home = root.join(slug(case.label));
    if case.make_home {
        fs::create_dir_all(&home).unwrap();
    }
    for a in &case.agents {
        let path = home.join(a.name);
        if a.name == "loose" {
            // The plain-file case: a FILE holding a state body, not a dir.
            fs::write(&path, a.body.as_deref().unwrap_or_default()).unwrap();
        } else {
            fs::create_dir_all(&path).unwrap();
            if let Some(text) = a.body.as_deref() {
                fs::write(path.join("state.json"), text).unwrap();
            }
        }
    }
    home
}

fn run_binary(home: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .args(["drive-authority"].iter().chain(args.iter()))
        .env("FNO_AGENTS_HOME", home)
        .output()
        .expect("fno-agents binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The OLD leg on the same fixture: the Python detector's answer, serialized
/// the way the binary prints it (Python `json.dumps` default separators and
/// key order). `FNO_PYTHON` pins the interpreter (the project venv that has
/// the fno package deps); bare python3 is the fallback.
fn python_oracle(home: &Path) -> Golden {
    let root = repo_root();
    let interpreter = std::env::var("FNO_PYTHON").unwrap_or_else(|_| "python3".to_string());
    let script = "\
import json, sys
sys.path.insert(0, sys.argv[2])
from fno.drive_authority import active_drive_sessions
s = active_drive_sessions(sys.argv[1])
print(json.dumps({'active': bool(s), 'sessions': s}))
";
    let out = Command::new(&interpreter)
        .arg("-c")
        .arg(script)
        .arg(home)
        .arg(root.join("cli/src"))
        .output()
        .unwrap_or_else(|e| {
            panic!("capture needs the Python leg's interpreter (FNO_PYTHON or python3): {e}")
        });
    if !out.status.success() {
        panic!(
            "capture: the Python leg failed to run: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // The frozen exit is the binary's contract derived from the OLD leg's
    // answer: 0 when a window is active, else 1. The interpreter's own status
    // says nothing about that contract.
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let active = text.contains("\"active\": true");
    Golden {
        exit: Some(if active { 0 } else { 1 }),
        streams: vec![text],
    }
}

#[test]
fn drive_authority_json_matches_the_captured_goldens() {
    let root = std::env::temp_dir().join(format!("drive-auth-parity-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    for case in cases() {
        let home = build_home(&root, &case);
        let (code, out, _err) = run_binary(&home, &["--json"]);
        let rust = Golden {
            exit: Some(code),
            streams: vec![out],
        };
        let oracle = capture_mode().then(|| python_oracle(&home));
        assert_golden("drive_authority", case.label, &rust, oracle);
    }
    let _ = fs::remove_dir_all(&root);
}

/// The command's own surface: human lines, the quiet-home line, and the
/// argument refusals. No Python counterpart existed for these bytes, so
/// they pin directly.
#[test]
fn the_command_contract_holds_without_json() {
    let root = std::env::temp_dir().join(format!("drive-auth-cli-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    fs::create_dir_all(home.join("ag1")).unwrap();
    fs::write(
        home.join("ag1/state.json"),
        r#"{"short_id": "ag1", "pty": {"drive_active": true, "drive_mode": "interactive", "drive_session_id": "sess-1"}}"#,
    )
    .unwrap();

    // Human line order: short_id, mode, session_id.
    let (code, out, _err) = run_binary(&home, &[]);
    assert_eq!(code, 0);
    assert_eq!(out, "ag1 interactive sess-1\n");

    let empty = root.join("empty");
    fs::create_dir_all(&empty).unwrap();
    let (code, out, _err) = run_binary(&empty, &[]);
    assert_eq!(code, 1);
    assert_eq!(out, "no active drive authority\n");

    let (code, _out, err) = run_binary(&home, &["--wat"]);
    assert_eq!(code, 2);
    assert!(err.contains("unknown drive-authority flag: --wat"), "{err}");

    let (code, _out, err) = run_binary(&home, &["extra"]);
    assert_eq!(code, 2);
    assert!(
        err.contains("drive-authority takes no positional arguments"),
        "{err}"
    );

    let _ = fs::remove_dir_all(&root);
}
