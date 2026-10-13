//! parity-stage: characterization
//! parity-oracle: cli/src/fno/mail/ack_refusal.py
//!
//! Characterization harness for the `mail ack` front port. The goldens were captured from the Python ack body (`cmd_bus_ack`) while it still ran its own logic: the two advance receipts, the two idempotent no-ops, and the unknown-id / wrong-recipient / hosted / typed / withdraw / landed refusals. Captured through a `uv run python` driver while the oracle existed; in normal mode the frozen bytes are the contract and the Python leg never runs.
//!
//! One sanctioned divergence, invisible in the streams: the Rust acked-set dedupes first rows after the recipient filter where the oracle deduped inside `scan_unread`; over this fixture set the acked-set membership is identical.
//!
//! Every case runs the client binary on its own temp root under the freeze capture's env block. Normalization mirrors the freeze pipeline (drop setup noise, rstrip, a sorted-compact JSON re-dump, then the time / root scrubs) minus the hold port's rename translate and held_for_s scrub, so a matching leg freezes to the same bytes the oracle froze to.

mod common;

use common::{assert_golden, Golden};
use serde_json::Value;
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";

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

/// The freeze capture's env block, applied over the inherited environment (the pins win; the blanks silence the other harness markers). `FNO_AGENT_SELF` must stay unset for the registration to run at all. The test-owner env rides along so the keeper attributes the spawned client to this test process.
fn hold_env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
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
        .env("FNO_AGENTS_BIN", binary_path())
        .env_remove("FNO_AGENT_SELF")
}

fn run_client(root: &Path, args: &[&str]) -> Leg {
    let mut cmd = Command::new(binary_path());
    let out = hold_env(&mut cmd, root)
        .args(["mail"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail ack");
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

/// The freeze pipeline, in order: drop setup noise (the events.emit warning opens a multi-line skip that a top-level `Error: ` closes), rstrip, re-dump a JSON object sorted and compact (serde_json keeps insertion order here), then scrub the volatile fields. No rename translate and no held_for_s scrub: the ack leaf prints neither.
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
    let text = RE_TIME
        .get_or_init(|| regex::Regex::new(r"\d{2}:\d{2}:\d{2}").unwrap())
        .replace_all(text, "HH:MM:SS");
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

/// The nine-row fixture bus, one JSON object per line, the freeze driver's rows in its key order.
const BUS_LOG: &str = r#"{"id":"ackmsg-1","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"one","ts":"2026-10-12T09:00:00Z"}
{"id":"ackmsg-2","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"two","ts":"2026-10-12T09:05:00Z"}
{"id":"ackmsg-3","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"three","ts":"2026-10-12T09:10:00Z"}
{"id":"ackmsg-4","thread":"t","from":"lead","to":"other","kind":"send","body":"theirs","ts":"2026-10-12T09:15:00Z"}
{"id":"ackmsg-5","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"already hosted","ts":"2026-10-12T09:20:00Z","delivery":"hosted"}
{"id":"ackmsg-6","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"typed once","ts":"2026-10-12T09:25:00Z","delivery":"typed"}
{"id":"ackmsg-7","thread":"t","from":"lead","to":"a1b2c3d4","kind":"withdraw","body":"","ts":"2026-10-12T09:30:00Z"}
{"id":"ackmsg-8","thread":"t","from":"lead","to":"a1b2c3d4","kind":"landed","body":"","ts":"2026-10-12T09:35:00Z","meta":{"landed":"ackmsg-1"}}
{"id":"ackmsg-9","thread":"t","from":"lead","to":"fno","kind":"send","body":"for the default name","ts":"2026-10-12T09:40:00Z"}
"#;

/// The planted read cursor, byte-matching the freeze capture's json.dumps line.
const CURSOR_FILE: &str = "{\"last_seen_id\": \"ackmsg-2\", \"ts\": \"2026-10-12T09:00:00Z\"}\n";

enum Setup {
    Bus,
    /// The read cursor for `a1b2c3d4` sits on ackmsg-2, the two idempotent cases' precondition.
    Cursor,
}

fn apply_setup(root: &Path, setup: &Setup) {
    match setup {
        Setup::Bus => {
            let dir = root.join("bus");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("messages.jsonl"), BUS_LOG).unwrap();
        }
        Setup::Cursor => {
            let dir = root.join("bus").join("cursors");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("a1b2c3d4.json"), CURSOR_FILE).unwrap();
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
        label: "advance past unread",
        args: &["ack", "ackmsg-2", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "advance default name",
        args: &["ack", "ackmsg-9"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "already acked behind",
        args: &["ack", "ackmsg-1", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus, Setup::Cursor],
    },
    Case {
        label: "already acked equal",
        args: &["ack", "ackmsg-2", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus, Setup::Cursor],
    },
    Case {
        label: "unknown msg id",
        args: &["ack", "nope", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "addressed to other",
        args: &["ack", "ackmsg-4", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "refuse hosted",
        args: &["ack", "ackmsg-5", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "refuse typed",
        args: &["ack", "ackmsg-6", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "refuse withdraw",
        args: &["ack", "ackmsg-7", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
    Case {
        label: "refuse landed",
        args: &["ack", "ackmsg-8", "--name", "a1b2c3d4"],
        setups: &[Setup::Bus],
    },
];

#[test]
fn ack_front_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for setup in case.setups {
            apply_setup(root, setup);
        }
        let leg = run_client(root, case.args);
        assert_golden("mail_ack", case.label, &leg.normalized_golden(root), None);
    }
}
