//! parity-stage: characterization
//! parity-oracle: fno.agents.read.read_logs
//!
//! Characterization harness for the `logs` verb port. The goldens were
//! captured from the Python leg (cli/src/fno/agents/read.py) while it still
//! ran its own body, through the recipe in this directory (`capture.py` plus
//! the README): one throwaway root per case, a registry seed, the empty
//! harness stores, a PATH shim with the fake `claude` the case needs, and
//! `FNO_AGENTS_RUNTIME=python python -m fno.cli agents logs ...`.
//!
//! Normalization mirrors the capture pipeline exactly: the five setup-noise
//! drops, the events.emit multi-line skip, rstrip, then the ROOT scrub
//! (realpath first, then the literal path).
//!
//! One golden carries the SURVIVOR's bytes rather than the deleted leg's:
//! `unknown_name`. The registry resolver became session-id-first and its
//! not-found message changed shape, so the deleted Python leg printed
//! "agent not found: ghost" where the surviving resolver prints the
//! accepted-forms line below. capture.py's SURVIVOR_BYTES records the same
//! override, so re-running the recipe reproduces the frozen files exactly.

mod common;

use common::{assert_golden, make_script, Golden};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One run's observable outcome, still raw; normalization happens at compare time.
struct Leg {
    exit: i32,
    stdout: String,
    stderr: String,
}

impl Leg {
    fn normalized_golden(&self, root: &Path) -> Golden {
        Golden {
            exit: Some(self.exit),
            streams: vec![normalize(&self.stdout, root), normalize(&self.stderr, root)],
        }
    }
}

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

/// The capture's env block, applied over the inherited environment. Identity
/// markers are blanked so the resolver scans only the fixture stores, and
/// `FNO_AGENTS_RUNTIME` is REMOVED so the client never re-execs a Python leg
/// (the capture set it to run the other side of this pair).
fn logs_env<'a>(cmd: &'a mut Command, root: &Path, shimmed: bool) -> &'a mut Command {
    // The test-owner env rides along so the client's attribution rows name
    // this test process, not the operator's environment.
    let cmd = cmd
        .envs(fno_agents::test_run::self_owner_env())
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env(
            "FNO_CLAUDE_PROJECTS_DIR",
            root.join("empty-claude-projects"),
        )
        .env("FNO_CODEX_SESSIONS_DIR", root.join("empty-codex-sessions"))
        .env(
            "FNO_OPENCODE_STORAGE_DIR",
            root.join("empty-opencode-storage"),
        )
        .env("FNO_MUX_DIR", root.join("empty-mux"))
        .env("CODEX_THREAD_ID", "")
        .env("CODEX_SESSION_ID", "")
        .env("GEMINI_SESSION_ID", "")
        .env("OPENCODE_SESSION_ID", "")
        .env("CLAUDE_SESSION_ID", "")
        .env("CLAUDE_CODE_SESSION_ID", "")
        .env_remove("FNO_AGENT_SELF")
        .env_remove("FNO_RUST_FRONT")
        .env_remove("FNO_AGENTS_RUNTIME");
    if shimmed {
        let path = std::env::var("PATH").unwrap_or_default();
        let shim = root.join("shim");
        let prepended = if path.is_empty() {
            shim.display().to_string()
        } else {
            format!("{}:{}", shim.display(), path)
        };
        cmd.env("PATH", prepended)
    } else {
        cmd
    }
}

fn run_client(root: &Path, shimmed: bool, args: &[&str]) -> Leg {
    let mut cmd = Command::new(binary_path());
    let out = logs_env(&mut cmd, root, shimmed)
        .args(["logs"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents logs");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

const DROP_PREFIXES: [&str; 5] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
];

/// The capture pipeline, in order: drop setup noise, skip the events.emit
/// multi-line warning (a top-level `Error: ` closes it), rstrip, then the
/// ROOT scrub (realpath first, then the literal path).
fn normalize(text: &str, root: &Path) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.split('\n') {
        if DROP_PREFIXES.iter().any(|p| line.starts_with(p)) {
            continue;
        }
        if line.starts_with("fno agents: warning: events.emit(") {
            skipping = true;
            continue;
        }
        if skipping {
            if line.starts_with("Error: ") {
                skipping = false;
            }
            continue;
        }
        kept.push(line);
    }
    let text = kept.join("\n");
    let text = text.trim_end_matches('\n');
    // os.path.realpath order: the symlink-resolved form first, then the
    // literal path, so both spellings scrub on macOS /var temp roots.
    let real = std::fs::canonicalize(root)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let text = if real.is_empty() {
        text.to_string()
    } else {
        text.replace(&real, "ROOT")
    };
    text.replace(&root.display().to_string(), "ROOT")
}

const CLAUDE_ROW: &str = r#"{"name":"alpha","short_id":"abc12345","harness":"claude","harness_session_id":"sess-alpha-0001","cwd":"/repo","project_root":"/repo","log_path":"/repo/alpha/output.jsonl","status":"live","created_at":"2026-05-20T17:00:00Z"}"#;

fn codex_row(log_path: Option<&str>) -> String {
    let log_field = match log_path {
        Some(p) => format!(r#""log_path":"{p}","#),
        None => String::new(),
    };
    format!(
        r#"{{"name":"cx","short_id":"cx","harness":"codex","harness_session_id":"019fb417-1111-7222-8333-4444deadbeef","cwd":"/tmp/x","project_root":"/tmp/x",{log_field}"status":"live","created_at":"2026-05-26T09:00:00Z"}}"#
    )
}

const CODEX_LOG: &str = r#"{"line":1}
{"line":2}
{"line":3}
{"line":4}"#;

enum Row {
    Empty,
    Claude,
    ClaudeNoShortId,
    Codex(&'static str),
}

enum Shim {
    None,
    /// The shim body runs instead of a real `claude` binary.
    Claude(&'static str),
}

struct Case {
    label: &'static str,
    rows: Row,
    shim: Shim,
    args: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        label: "unknown name",
        rows: Row::Empty,
        shim: Shim::None,
        args: &["ghost"],
    },
    Case {
        label: "claude passthrough",
        rows: Row::Claude,
        shim: Shim::Claude("echo \"raw line 1\"; echo \"raw line 2\""),
        args: &["alpha"],
    },
    Case {
        label: "claude tail 3",
        rows: Row::Claude,
        shim: Shim::Claude("i=1; while [ $i -le 20 ]; do echo \"line$i\"; i=$((i+1)); done"),
        args: &["alpha", "--tail", "3"],
    },
    Case {
        label: "tail zero",
        rows: Row::Claude,
        shim: Shim::Claude("i=1; while [ $i -le 20 ]; do echo \"line$i\"; i=$((i+1)); done"),
        args: &["alpha", "--tail", "0"],
    },
    Case {
        label: "negative tail",
        rows: Row::Claude,
        shim: Shim::Claude("echo \"raw line 1\"; echo \"raw line 2\""),
        args: &["alpha", "--tail", "-5"],
    },
    Case {
        label: "claude non-zero exit propagates",
        rows: Row::Claude,
        shim: Shim::Claude("echo \"claude: not found\" >&2; exit 17"),
        args: &["alpha"],
    },
    Case {
        label: "claude json warning",
        rows: Row::Claude,
        shim: Shim::Claude("echo \"claude raw output\""),
        args: &["alpha", "--json"],
    },
    Case {
        label: "claude row with no short id",
        rows: Row::ClaudeNoShortId,
        shim: Shim::Claude("echo \"raw line 1\"; echo \"raw line 2\""),
        args: &["alpha"],
    },
    Case {
        label: "codex one-shot tail 2 by name",
        rows: Row::Codex("cx.log.jsonl"),
        shim: Shim::None,
        args: &["cx", "--tail", "2"],
    },
    Case {
        label: "codex one-shot tail 2 by session head",
        rows: Row::Codex("cx.log.jsonl"),
        shim: Shim::None,
        args: &["deadbeef", "--tail", "2"],
    },
    Case {
        label: "codex row with no log file",
        rows: Row::Codex("absent.jsonl"),
        shim: Shim::None,
        args: &["cx"],
    },
    Case {
        label: "codex no log_path recorded",
        rows: Row::Codex(""),
        shim: Shim::None,
        args: &["cx"],
    },
];

/// Seed the fixture the capture recipe built: registry, the codex tee file,
/// the empty harness stores, and the PATH shim.
fn build_fixture(root: &Path, case: &Case) -> Option<PathBuf> {
    let agents = root.join("agents-home");
    std::fs::create_dir_all(&agents).unwrap();
    let rows_json = match case.rows {
        Row::Empty => "[]".to_string(),
        Row::Claude => format!("[{CLAUDE_ROW}]"),
        Row::ClaudeNoShortId => {
            format!(
                "[{}]",
                CLAUDE_ROW.replace(r#""short_id":"abc12345","#, r#""short_id":"","#)
            )
        }
        Row::Codex(name) => {
            // An empty name seeds the capture recipe's `"log_path":""` key
            // (present but empty), not a missing key: the deleted leg's
            // registry treated those two rows differently, and the fixture
            // mirrors the shape the golden froze.
            let log_path = if name.is_empty() {
                String::new()
            } else {
                let p = root.join("agents-home").join(name);
                if name != "absent.jsonl" {
                    std::fs::write(&p, CODEX_LOG).unwrap();
                }
                p.display().to_string()
            };
            format!("[{}]", codex_row(Some(&log_path)))
        }
    };
    std::fs::write(
        agents.join("registry.json"),
        format!(r#"{{"schema_version":3,"agents":{rows_json}}}"#),
    )
    .unwrap();
    for dir in [
        "empty-claude-projects",
        "empty-codex-sessions",
        "empty-opencode-storage",
        "empty-mux",
    ] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    match case.shim {
        Shim::None => None,
        Shim::Claude(body) => {
            let dir = root.join("shim");
            std::fs::create_dir_all(&dir).unwrap();
            Some(make_script(&dir, "claude", body))
        }
    }
}

#[test]
fn logs_client_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let _shim = build_fixture(root, case);
        let shimmed = matches!(case.shim, Shim::Claude(_));
        let leg = run_client(root, shimmed, case.args);
        assert_golden("logs", case.label, &leg.normalized_golden(root), None);
    }
}
