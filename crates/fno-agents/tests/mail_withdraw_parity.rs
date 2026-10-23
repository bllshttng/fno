//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_withdraw (the deleted Python body, goldens captured from the live leaf)
//!
//! Characterization harness for the `mail withdraw` leaf port. The goldens were captured from the Python leaf while it still ran its own body: the success receipt, the ownership / cursor-claim / hosted / typed refusals, the idempotent re-withdraw, the unknown id, and the explicit `--from-name` lane. Captured through the parity env block with the venv python invoked directly while the oracle existed; in normal mode the frozen bytes are the contract and the Python leg is gone (the command is a forward to this leaf).
//!
//! Normalization mirrors the capture pipeline exactly: the five path-migration drops, the events.emit multi-line skip, rstrip, then the ROOT scrub. No rename translate, no JSON re-dump, no time scrubs - the oracle is static prose, so anything beyond the capture's pipeline would mask a real divergence instead of freezing it.
//!
//! Two silent-reader divergences from the Python leg are part of the contract: malformed bus lines and corrupt cursor files are skipped without a warning, and cursor filenames use the raw address rather than the Python `_safe_name` spelling.

mod common;

use common::{assert_golden, Golden};
use std::path::Path;
use std::process::Command;

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

/// The freeze capture's env block, applied over the inherited environment (the pins win; the blanks silence the other harness markers). `FNO_AGENT_SELF` must stay unset for the resolver to behave exactly as it did under the capture; the test-owner env rides along so the keeper attributes the client to this test process.
fn withdraw_env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
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
    let out = withdraw_env(&mut cmd, root)
        .args(["mail"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail withdraw");
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
/// ROOT scrub (realpath first, then the literal path). Nothing else is
/// scrubbed: the oracle is static prose, so extra translate / re-dump / time
/// passes would mask a real divergence instead of freezing it.
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

/// The capture's `send` fixture row, spelled exactly as the freeze script
/// wrote it (`delivery` appended last when present).
fn send_msg(id: &str, from: &str, delivery: Option<&str>) -> String {
    let delivery = delivery
        .map(|d| format!(r#","delivery":"{d}""#))
        .unwrap_or_default();
    format!(
        r#"{{"id":"{id}","thread":"t1","from":"{from}","to":"lead","kind":"send","body":"body {id}","ts":"2026-10-12T09:00:00Z"{delivery}}}"#
    )
}

/// The capture's planted tombstone (withdrawing wmsg-1 from a1b2c3d4).
const TOMB: &str = r#"{"id":"wdg-1","thread":"t1","from":"a1b2c3d4","to":"lead","kind":"withdraw","body":"withdrawn: wmsg-1","meta":{"withdraws":"wmsg-1"},"ts":"2026-10-12T09:05:00Z"}"#;

enum Row {
    Msg {
        id: &'static str,
        from: &'static str,
        delivery: Option<&'static str>,
    },
    Tomb,
}

enum Setup {
    /// The messages.jsonl the capture planted, rows in capture order.
    Bus(&'static [Row]),
    /// A delivery cursor: `{"last_seen_id": id}` at `bus/cursors/{name}.json`
    /// (the filename IS the raw address).
    Cursor(&'static str, &'static str),
}

fn apply_setup(root: &Path, setup: &Setup) {
    match setup {
        Setup::Bus(rows) => {
            let dir = root.join("bus");
            std::fs::create_dir_all(&dir).unwrap();
            let content: String = rows
                .iter()
                .map(|row| match row {
                    Row::Msg { id, from, delivery } => send_msg(id, from, *delivery),
                    Row::Tomb => TOMB.to_string(),
                })
                .map(|line| line + "\n")
                .collect();
            std::fs::write(dir.join("messages.jsonl"), content).unwrap();
        }
        Setup::Cursor(name, last_seen) => {
            let dir = root.join("bus").join("cursors");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("{name}.json")),
                format!("{{\"last_seen_id\": \"{last_seen}\"}}\n"),
            )
            .unwrap();
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
        label: "unclaimed withdraw success",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[Row::Msg {
            id: "wmsg-1",
            from: "a1b2c3d4",
            delivery: None,
        }])],
    },
    Case {
        label: "already claimed refusal",
        args: &["withdraw", "wmsg-1"],
        setups: &[
            Setup::Bus(&[Row::Msg {
                id: "wmsg-1",
                from: "a1b2c3d4",
                delivery: None,
            }]),
            Setup::Cursor("lead", "wmsg-1"),
        ],
    },
    Case {
        label: "cursor before target success",
        args: &["withdraw", "wmsg-2"],
        setups: &[
            Setup::Bus(&[
                Row::Msg {
                    id: "wmsg-1",
                    from: "a1b2c3d4",
                    delivery: None,
                },
                Row::Msg {
                    id: "wmsg-2",
                    from: "a1b2c3d4",
                    delivery: None,
                },
            ]),
            Setup::Cursor("lead", "wmsg-1"),
        ],
    },
    Case {
        label: "not own refusal",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[Row::Msg {
            id: "wmsg-1",
            from: "peer",
            delivery: None,
        }])],
    },
    Case {
        label: "no such message",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[])],
    },
    Case {
        label: "already withdrawn",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[
            Row::Msg {
                id: "wmsg-1",
                from: "a1b2c3d4",
                delivery: None,
            },
            Row::Tomb,
        ])],
    },
    Case {
        label: "hosted refusal",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[Row::Msg {
            id: "wmsg-1",
            from: "a1b2c3d4",
            delivery: Some("hosted"),
        }])],
    },
    Case {
        label: "typed refusal",
        args: &["withdraw", "wmsg-1"],
        setups: &[Setup::Bus(&[Row::Msg {
            id: "wmsg-1",
            from: "a1b2c3d4",
            delivery: Some("typed"),
        }])],
    },
    Case {
        label: "explicit from name",
        args: &["withdraw", "wmsg-1", "--from-name", "fleet"],
        setups: &[Setup::Bus(&[Row::Msg {
            id: "wmsg-1",
            from: "fleet",
            delivery: None,
        }])],
    },
];

#[test]
fn withdraw_leaf_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for setup in case.setups {
            apply_setup(root, setup);
        }
        let leg = run_client(root, case.args);
        assert_golden(
            "mail_withdraw",
            case.label,
            &leg.normalized_golden(root),
            None,
        );
    }
}
