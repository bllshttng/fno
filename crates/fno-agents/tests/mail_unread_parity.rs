//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_unread (the deleted Python unread body,
//! goldens captured from the front command before the forward swap)
//!
//! Characterization harness for the `mail unread` port. The goldens were
//! captured in-process from the Python leaf (via `uv run python`, redirected
//! typer streams) while its body still ran: the cursor-bounded listing, the
//! full rescan with no cursor, the empty inbox, the corrupt-cursor warning
//! followed by its full rescan, and the rotated-out cursor (a parseable
//! cursor naming an id the retained log no longer holds) taking the same
//! full rescan with no warning. The frozen bytes are the contract
//! with no normalization: the fixture ids, bodies and timestamps are fixed
//! strings, so the output carries no clock or path, and the byte-parity JSON
//! writer in mail_cli::unread mirrors the oracle's
//! `json.dumps(..., ensure_ascii=False)`.
//!
//! Sanctioned divergences, uncharacterized by the goldens: an invalid
//! `--name` refuses cleanly (exit 2) where the oracle raised a traceback;
//! corrupt bus lines or segments skip silently where the oracle warned on
//! stderr (the shared reader's posture); and the oracle's `str()` coercion
//! of odd field types (a null thread, a numeric ts) has no golden, so the
//! port renders those with its id/empty fallbacks instead.
//!
//! Every case runs the client binary on a fresh temp root under the capture
//! env block. Only stderr is filtered, dropping the first-run setup noise;
//! stdout stays raw, so a stray banner would fail the byte compare loudly.

mod common;

use common::{assert_golden, Golden};
use std::path::Path;
use std::process::Command;

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

fn run_client(root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(binary_path())
        .envs(fno_agents::test_run::self_owner_env())
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_BUS_DIR", root.join("bus"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_AGENTS_BIN", binary_path())
        .env_remove("FNO_AGENT_SELF")
        .args(["mail"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail unread");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const DROP_PREFIXES: [&str; 5] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
];

/// Drop the setup noise on stderr only. The events.emit warning opens a
/// multi-line skip that a top-level `Error: ` line closes. Joining the kept
/// lines preserves the trailing newline the goldens end with.
fn filter_stderr(text: &str) -> String {
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
    kept.join("\n")
}

/// The 11-row bus fixture the goldens were captured against, verbatim.
const BUS_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"before cursor","ts":"2026-10-12T09:00:00Z"}
{"id":"m2","thread":"t1","from":"lead","to":"worker","kind":"send","body":"at cursor","ts":"2026-10-12T09:05:00Z"}
{"id":"m3","thread":"t1","from":"lead","to":"other","kind":"send","body":"not mine","ts":"2026-10-12T09:06:00Z"}
{"id":"m4","thread":"t1","from":"lead","to":"worker","kind":"withdraw","body":"","ts":"2026-10-12T09:07:00Z","meta":{"withdraws":"m5"}}
{"id":"m5","thread":"t1","from":"lead","to":"worker","kind":"send","body":"retracted","ts":"2026-10-12T09:08:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","body":"staged twin","ts":"2026-10-12T09:09:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"hosted","body":"hosted receipt","ts":"2026-10-12T09:10:00Z"}
{"id":"m7","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"typed","body":"typed delivery","ts":"2026-10-12T09:11:00Z"}
{"id":"m8","from":"lead","to":"worker","kind":"send","body":"fresh café"}
{"id":"m9","thread":"t1","from":"lead","to":"worker","kind":"send","body":"first copy wins","ts":"2026-10-12T09:12:00Z"}
{"id":"m9","thread":"t1","from":"lead","to":"worker","kind":"send","body":"second copy dropped","ts":"2026-10-12T09:13:00Z"}
"#;

const CURSOR_M2: &str = "{\"last_seen_id\": \"m2\", \"ts\": \"2026-10-12T09:14:00Z\"}\n";

const CURSOR_CORRUPT: &str = "{ invalid";

const CURSOR_ROTATED: &str = "{\"last_seen_id\": \"mGONE\", \"ts\": \"2026-10-12T09:15:00Z\"}\n";

struct Case {
    label: &'static str,
    args: &'static [&'static str],
    cursor: Option<&'static str>,
}

const CASES: &[Case] = &[
    Case {
        label: "unread after cursor",
        args: &["unread", "--name", "worker"],
        cursor: Some(CURSOR_M2),
    },
    Case {
        label: "unread full rescan",
        args: &["unread", "--name", "worker", "--json"],
        cursor: None,
    },
    Case {
        label: "unread empty inbox",
        args: &["unread", "--name", "nobody"],
        cursor: None,
    },
    Case {
        label: "unread corrupt cursor",
        args: &["unread", "--name", "worker"],
        cursor: Some(CURSOR_CORRUPT),
    },
    Case {
        label: "unread cursor rotated out",
        args: &["unread", "--name", "worker"],
        cursor: Some(CURSOR_ROTATED),
    },
];

#[test]
fn unread_listing_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let bus = root.join("bus");
        std::fs::create_dir_all(&bus).unwrap();
        std::fs::write(bus.join("messages.jsonl"), BUS_LOG).unwrap();
        if let Some(cursor) = case.cursor {
            std::fs::create_dir_all(bus.join("cursors")).unwrap();
            std::fs::write(bus.join("cursors").join("worker.json"), cursor).unwrap();
        }
        let (exit, stdout, stderr) = run_client(root, case.args);
        let golden = Golden {
            exit: Some(exit),
            streams: vec![stdout, filter_stderr(&stderr)],
        };
        assert_golden("mail_unread", case.label, &golden, None);
    }
}
