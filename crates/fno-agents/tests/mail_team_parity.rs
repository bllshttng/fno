//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_team
//!
//! Characterization harness for the `mail team` front port. The goldens were captured from the Python team leaf while it still ran its own body: the two success receipts (scope `all` reaching the two non-self registry rows, scope `leads` reaching the one lead), the style refusal on a 90-word body, and the missing-message usage line. Captured with `uv run python` driving `cmd_team` through CliRunner under the freeze-capture env block while the oracle existed; in normal mode the frozen bytes are the contract and the Python leg never runs.
//!
//! Every case runs the client binary on its own temp root under the freeze capture's env block. Normalization mirrors the freeze pipeline (drop setup noise, rstrip, a sorted-compact JSON re-dump, then the time / fmail-id / root scrubs) so a matching leg freezes to the same bytes the oracle froze to.

mod common;

use common::{assert_golden, Golden};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";

const BODY: &str =
    "fleet note: the bus writer moved to crates fno-agents. run fno doctor after pulling.";

/// The 90-word body the style gate refuses: `["alpha","beta","gamma","delta",
/// "epsilon"] * 20` truncated to 90 words, i.e. 18 full cycles.
const STYLE_BODY: &str = concat!(
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon ",
    "alpha beta gamma delta epsilon",
);

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

/// The freeze capture's env block, applied over the inherited environment (the pins win; the blanks silence the other harness markers). The broadcast re-enters `current_exe()` for its `announce send` child, so no PATH or interpreter is needed. The launcher stamp pair (`FNO_SESSION_HARNESS` + a live `FNO_SESSION_PID`) pins the harness half ahead of the host's process ancestry; `FNO_AGENT_SELF` must stay unset so the self still resolves through the harness markers.
fn team_env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.envs(fno_agents::test_run::self_owner_env())
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_BUS_DIR", root.join("bus"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_HARNESS_NAME", "claude")
        .env("FNO_HARNESS_SESSION_ID", SID)
        .env("CLAUDE_CODE_SESSION_ID", SID)
        .env("CODEX_THREAD_ID", "")
        .env("CODEX_SESSION_ID", "")
        .env("GEMINI_SESSION_ID", "")
        .env("OPENCODE_SESSION_ID", "")
        .env("CLAUDE_SESSION_ID", "")
        // The launcher stamp pair answers before the ancestry walk: FNO_SESSION_HARNESS names a known harness, FNO_SESSION_PID is the live test process the child can kill(pid,0), so the fixture identity holds on any host harness. FNO_AGENT_SELF stays unset, so the self still resolves through the harness markers.
        .env("FNO_SESSION_HARNESS", "claude")
        .env("FNO_SESSION_PID", std::process::id().to_string())
        .env("FNO_AGENTS_BIN", binary_path())
        .env_remove("FNO_AGENT_SELF")
}

fn run_client(root: &Path, args: &[&str]) -> Leg {
    let mut cmd = Command::new(binary_path());
    let out = team_env(&mut cmd, root)
        .args(["mail"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail team");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

const DROP_PREFIXES: [&str; 6] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
    "mail-hold: release timer did not start",
];

/// The freeze pipeline, in order: drop setup noise (the events.emit warning opens a multi-line skip that a top-level `Error: ` closes), rstrip, re-dump a JSON object sorted and compact (serde_json keeps insertion order here), then scrub the volatile fields.
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
    let text = match serde_json::from_str::<Value>(text.trim()) {
        Ok(v @ Value::Object(_)) => sorted_compact(&v),
        _ => text.to_string(),
    };
    scrub(&text, root)
}

fn sorted_compact(v: &Value) -> String {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let inner = keys
                .iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap_or_default(),
                        sorted_compact(&map[*k])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{inner}}}")
        }
        Value::Array(items) => {
            let inner = items
                .iter()
                .map(sorted_compact)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{inner}]")
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

fn scrub(text: &str, root: &Path) -> String {
    static RE_TIME: OnceLock<regex::Regex> = OnceLock::new();
    static RE_FMAIL: OnceLock<regex::Regex> = OnceLock::new();
    let text = RE_TIME
        .get_or_init(|| regex::Regex::new(r"\d{2}:\d{2}:\d{2}").unwrap())
        .replace_all(text, "HH:MM:SS");
    let text = RE_FMAIL
        .get_or_init(|| regex::Regex::new(r"fmail-[0-9a-f]{12}").unwrap())
        .replace_all(&text, "fmail-SCRUB");
    // os.path.realpath order: the symlink-resolved form first, then the
    // literal path, so both spellings scrub on macOS /var temp roots.
    let real = std::fs::canonicalize(root)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let text = if real.is_empty() {
        text.into_owned()
    } else {
        text.replace(&real, "ROOT")
    };
    text.replace(&root.display().to_string(), "ROOT")
}

/// The planted registry, byte-for-byte the capture's fixture: sasha (this
/// session, so the audience snapshot excludes it), nova (the one other lead),
/// and rook (codex, no role).
const REGISTRY: &str = r#"{"schema_version": 1, "agents": [
  {"name": "sasha", "short_id": "sasha-id", "harness": "claude",
   "harness_session_id": "a1b2c3d4-1111-2222-3333-444455556666",
   "cwd": "/tmp/x-62f6/sasha", "log_path": "/tmp/x-62f6/sasha.log",
   "created_at": "2026-10-12T09:00:00Z", "status": "live", "role_level": "lead"},
  {"name": "nova", "short_id": "nova-id", "harness": "claude",
   "harness_session_id": "b2c3d4e5-1111-2222-3333-444455556666",
   "cwd": "/tmp/x-62f6/nova", "log_path": "/tmp/x-62f6/nova.log",
   "created_at": "2026-10-12T09:00:00Z", "status": "live", "role_level": "lead"},
  {"name": "rook", "short_id": "rook-id", "harness": "codex",
   "harness_session_id": "c3d4e5f6-1111-2222-3333-444455556666",
   "cwd": "/tmp/x-62f6/rook", "log_path": "/tmp/x-62f6/rook.log",
   "created_at": "2026-10-12T09:00:00Z", "status": "live"}
]}"#;

enum Setup {
    Registry,
}

fn apply_setup(root: &Path, setup: &Setup) {
    match setup {
        Setup::Registry => {
            let home = root.join("agents-home");
            std::fs::create_dir_all(&home).unwrap();
            std::fs::write(home.join("registry.json"), REGISTRY).unwrap();
            std::fs::create_dir_all(root.join("bus")).unwrap();
        }
    }
}

struct Case {
    label: &'static str,
    args: &'static [&'static str],
    setups: &'static [Setup],
}

const CASES: &[Case] = &[
    Case {
        label: "scope all success",
        args: &["team", "--scope", "all", BODY],
        setups: &[Setup::Registry],
    },
    Case {
        label: "scope leads success",
        args: &["team", "--scope", "leads", BODY],
        setups: &[Setup::Registry],
    },
    Case {
        label: "style refusal long body",
        args: &["team", "--scope", "all", STYLE_BODY],
        setups: &[Setup::Registry],
    },
    Case {
        label: "usage missing message",
        args: &["team", "--scope", "all"],
        setups: &[Setup::Registry],
    },
];

#[test]
fn team_front_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for setup in case.setups {
            apply_setup(root, setup);
        }
        let leg = run_client(root, case.args);
        assert_golden("mail_team", case.label, &leg.normalized_golden(root), None);
    }
}

/// Writer flags relay as flag-value pairs in either form: the space form no
/// longer leaks its value into the message slot, and the `=` form reaches a
/// writer that parses only the space form.
#[test]
fn team_relays_writer_flags_with_their_values() {
    for args in [
        &[
            "team",
            "--scope",
            "all",
            "--subject",
            "S",
            "--expires",
            "12h",
            "--urgent",
            BODY,
        ][..],
        &[
            "team",
            "--scope",
            "all",
            "--subject=S",
            "--expires=12h",
            "--urgent",
            BODY,
        ][..],
    ] {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        apply_setup(root, &Setup::Registry);
        let leg = run_client(root, args);
        // The writer refuses an unknown flag non-zero, so exit 0 is the proof
        // that every flag reached it paired with its value.
        assert_eq!(leg.exit, 0, "{args:?}: {}", leg.stderr);
    }
}
