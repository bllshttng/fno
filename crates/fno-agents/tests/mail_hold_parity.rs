//! parity-stage: characterization
//! parity-oracle: cli/src/fno/mail/hold_cli.py
//!
//! Characterization harness for the `mail hold` / `mail hold-release` front port. The goldens were captured from the Python hold front while it still ran its own body: the three arm receipts (idle default, explicit minutes, wall deadline), the argument refusals, both status lines, the three hold-off outcomes (nothing held, failed stamp, delivery missed), and both release endings (quiet without a clock, expired on a planted clock). Captured with `FNO_CAPTURE_GOLDEN=1` through an `fno` shim while the oracle existed; in normal mode the frozen bytes are the contract and the Python leg never runs.
//!
//! One sanctioned divergence: the Rust receipt names do-not-disturb where the oracle said "busy mode on for" (the port's rename). The arm cases assert the DND wording on the raw stdout first, then translate it back so the frozen oracle bytes stay the compare contract; the DND wording itself is pinned by that assertion and by the help text in mail_cli.
//!
//! Every case runs the client binary on its own temp root under the freeze capture's env block. Normalization mirrors the freeze pipeline (drop setup noise, rstrip, the rename translate, a sorted-compact JSON re-dump, then the time / held_for_s / root scrubs) so a matching leg freezes to the same bytes the oracle froze to.

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

/// The freeze capture's env block, applied over the inherited environment (the pins win; the blanks silence the other harness markers). Delivery re-enters `current_exe()`, so no PATH or interpreter is needed; `FNO_AGENT_SELF` must stay unset for the registration to run at all. The test-owner env rides along so the keeper attributes the spawned client and its detached release timer to this test process.
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
        .expect("run fno-agents mail hold");
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

/// The freeze pipeline, in order: drop setup noise (the events.emit warning opens a multi-line skip that a top-level `Error: ` closes), rstrip, translate the sanctioned rename back to the oracle wording, re-dump a JSON object sorted and compact (serde_json keeps insertion order here), then scrub the volatile fields.
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
    let text = text.replace("do-not-disturb on for", "busy mode on for");
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
    static RE_HELD: OnceLock<regex::Regex> = OnceLock::new();
    let text = RE_TIME
        .get_or_init(|| regex::Regex::new(r"\d{2}:\d{2}:\d{2}").unwrap())
        .replace_all(text, "HH:MM:SS");
    let text = RE_HELD
        .get_or_init(|| regex::Regex::new(r#""held_for_s": ?\d+"#).unwrap())
        .replace_all(&text, r#""held_for_s":HELD_FOR_S"#);
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

const BUS_LOG: &str = r#"{"id":"holdmsg-1","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"held one","ts":"2026-10-12T09:00:00Z"}
{"id":"holdmsg-2","thread":"t","from":"lead","to":"a1b2c3d4","kind":"send","body":"held two","ts":"2026-10-12T09:05:00Z"}
"#;

const CLOCK_FILE: &str =
    "{\"until\":\"2020-01-01T00:00:00Z\",\"window_s\":540,\"clock_kind\":\"wall\"}\n";

enum Setup {
    /// Arm a real hold through the same client binary, the way the freeze capture's `fno` shim did. The detached release timer it spawns holds no pipes and self-exits after its root drops.
    Shim(&'static [&'static str]),
    CorruptRegistry,
    Bus,
    Clock,
}

fn apply_setup(root: &Path, setup: &Setup) {
    match setup {
        Setup::Shim(args) => {
            let leg = run_client(root, args);
            assert_eq!(
                leg.exit, 0,
                "shim arm failed: {} / {}",
                leg.stdout, leg.stderr
            );
        }
        Setup::CorruptRegistry => {
            let dir = root.join("agents-home");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("registry.json"), "{ invalid").unwrap();
        }
        Setup::Bus => {
            let dir = root.join("bus");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("messages.jsonl"), BUS_LOG).unwrap();
        }
        Setup::Clock => {
            let dir = root.join("mail-hold");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(format!("{SID}.json")), CLOCK_FILE).unwrap();
        }
    }
}

struct Case {
    label: &'static str,
    args: &'static [&'static str],
    setups: &'static [Setup],
    /// The three arm-success cases: pin the sanctioned DND naming on the raw receipt before the translate hides it from the byte compare.
    dnd_receipt: bool,
}

const CASES: &[Case] = &[
    Case {
        label: "busy idle default",
        args: &["hold"],
        setups: &[],
        dnd_receipt: true,
    },
    Case {
        label: "idle minutes 12",
        args: &["hold", "--minutes", "12"],
        setups: &[],
        dnd_receipt: true,
    },
    Case {
        label: "wall for 30",
        args: &["hold", "--for", "30"],
        setups: &[],
        dnd_receipt: true,
    },
    Case {
        label: "minutes for conflict",
        args: &["hold", "--minutes", "5", "--for", "10"],
        setups: &[],
        dnd_receipt: false,
    },
    Case {
        label: "minutes zero",
        args: &["hold", "--minutes", "0"],
        setups: &[],
        dnd_receipt: false,
    },
    Case {
        label: "for zero",
        args: &["hold", "--for", "0"],
        setups: &[],
        dnd_receipt: false,
    },
    Case {
        label: "status after arm",
        args: &["hold", "--status"],
        setups: &[Setup::Shim(&["hold", "--minutes", "5"])],
        dnd_receipt: false,
    },
    Case {
        label: "status no row",
        args: &["hold", "--status"],
        setups: &[],
        dnd_receipt: false,
    },
    Case {
        label: "off nothing held",
        args: &["hold", "--off"],
        setups: &[Setup::Shim(&["hold", "--minutes", "5"])],
        dnd_receipt: false,
    },
    Case {
        label: "off failed policy",
        args: &["hold", "--off"],
        setups: &[Setup::CorruptRegistry],
        dnd_receipt: false,
    },
    Case {
        label: "off delivers inject missed",
        args: &["hold", "--off"],
        setups: &[Setup::Shim(&["hold", "--minutes", "5"]), Setup::Bus],
        dnd_receipt: false,
    },
    Case {
        label: "release quiet no clock",
        args: &["hold-release", "--handle", SID],
        setups: &[],
        dnd_receipt: false,
    },
    Case {
        label: "release expired empty",
        args: &["hold-release", "--handle", SID],
        setups: &[Setup::Clock],
        dnd_receipt: false,
    },
];

#[test]
fn hold_front_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for setup in case.setups {
            apply_setup(root, setup);
        }
        let leg = run_client(root, case.args);
        if case.dnd_receipt {
            assert!(
                leg.stdout.contains("do-not-disturb on for"),
                "[{}] the receipt must name do-not-disturb before the characterization translate hides it from the byte compare",
                case.label
            );
        }
        assert_golden("mail_hold", case.label, &leg.normalized_golden(root), None);
    }
}
