//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_send
//!
//! Characterization harness for the `mail send` wrapped-command port.
//! The goldens were captured from the Python leg (cmd_send) before its body
//! was deleted, through tests/capture/mail_send_capture.py; the frozen bytes
//! are the contract now that the front routes every send to this leaf. The hermetic root cannot offer a live recipient, so
//! the deep live lanes surface as their designed misses: the inject probe
//! misses, the durable floor writes, and the receipts name why. That IS the
//! contract the goldens freeze.
//!
//! Normalization mirrors the capture driver (tests/capture/mail_send_capture.py):
//! the setup-noise drops (including the host-dependent escalation stderr
//! advisory), rstrip, sorted-compact JSON re-dump when the whole stream is
//! one object, then the TS/DATE/TIME, fmail-/msg- id, 8-hex, and ROOT scrubs.
//! Keep the two tables in lockstep when a case is added.
//!
//! The settle window is pinned to 0: the goldens were captured with the
//! default 2s sleep, which changes latency and never output, because a
//! hermetic root lands nothing.

mod common;

use common::{assert_golden, slug, Golden};
use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";
const OTHER_SID: &str = "b2c3d4e5-1111-2222-3333-444455556666";
const DND_SID: &str = "d3e4f5a6-1111-2222-3333-444455556666";
const CODEX_SID: &str = "04b5621d-1111-2222-3333-444455556666";
const CREATED: &str = "2026-10-12T09:00:00Z";

const DROP_PREFIXES: [&str; 6] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
    // The escalation stderr advisory names whether the OS notifier displayed,
    // a property of the host, not of the command. The receipt is the contract.
    "escalated to human (",
];

struct Leg {
    exit: i32,
    stdout: String,
    stderr: String,
}

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

fn base_env<'a>(cmd: &'a mut Command, root: &Path) -> &'a mut Command {
    cmd.envs(fno_agents::test_run::self_owner_env())
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_BUS_DIR", root.join("bus"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_HARNESS_NAME", "claude")
        .env("FNO_HARNESS_SESSION_ID", SID)
        .env("CLAUDE_CODE_SESSION_ID", SID)
        .env("CODEX_THREAD_ID", "")
        .env("CODEX_SESSION_ID", "")
        .env("GEMINI_SESSION_ID", "")
        .env("OPENCODE_SESSION_ID", "")
        .env("CLAUDE_SESSION_ID", "")
        .env("FNO_AGENTS_BIN", binary_path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        // The hermetic pin both legs share: it silences the OS notifier and
        // any registry spawn leg.
        .env("FNO_TEST_HERMETIC", "1")
        // The settle sleep changes latency and never output; pin it to 0.
        .env("FNO_MAIL_LANDED_SETTLE_S", "0")
        // The style gate runs off in the default block; the style case
        // removes this pin.
        .env("FNO_STYLE_ENFORCE", "0")
        .env_remove("FNO_AGENT_SELF")
        .env_remove("FNO_MAIL_SUBJECT")
        .env_remove("FNO_MAIL_LOCAL_FORWARD")
}

fn run_case(root: &Path, style_on: bool, no_identity: bool, argv: &[&str]) -> Leg {
    let root_str = root.display().to_string();
    let argv: Vec<String> = argv
        .iter()
        .map(|a| a.replace("<ROOT>", &root_str))
        .collect();
    let mut cmd = Command::new(binary_path());
    let cmd = base_env(&mut cmd, root);
    let cmd = if style_on {
        cmd.env_remove("FNO_STYLE_ENFORCE")
    } else {
        cmd
    };
    let cmd = if no_identity {
        cmd.env("FNO_HARNESS_SESSION_ID", "")
            .env("CLAUDE_CODE_SESSION_ID", "")
    } else {
        cmd
    };
    let out = cmd
        .args(["mail", "send"])
        .args(&argv)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail send");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

impl Leg {
    fn golden(&self, root: &Path) -> Golden {
        Golden {
            exit: Some(self.exit),
            streams: vec![normalize(&self.stdout, root), normalize(&self.stderr, root)],
        }
    }
}

fn json_sorted_compact(text: &str) -> Option<String> {
    let mut value: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    if !value.is_object() {
        return None;
    }
    // preserve_order keeps the re-dump at insertion order; the capture driver
    // sorted, so sort here at every nesting level to match it.
    sort_keys(&mut value);
    Some(value.to_string())
}

fn sort_keys(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (_, v) in map.iter_mut() {
                sort_keys(v);
            }
            let mut pairs: Vec<(String, serde_json::Value)> =
                std::mem::take(map).into_iter().collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            *map = pairs.into_iter().collect();
        }
        serde_json::Value::Array(items) => {
            for item in items {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

fn normalize(text: &str, root: &Path) -> String {
    static RE_TS: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_DATE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_TIME: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_FMAIL: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_MSG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    static RE_HEX8: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let kept: Vec<&str> = text
        .split('\n')
        .filter(|line| !DROP_PREFIXES.iter().any(|p| line.starts_with(p)))
        .collect();
    let mut out = kept.join("\n");
    while out.ends_with('\n') {
        out.pop();
    }
    if let Some(sorted) = json_sorted_compact(&out) {
        out = sorted;
    }
    let out = RE_TS
        .get_or_init(|| {
            regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z?").unwrap()
        })
        .replace_all(&out, "<TS>");
    let out = RE_DATE
        .get_or_init(|| regex::Regex::new(r"\d{4}-\d{2}-\d{2}").unwrap())
        .replace_all(&out, "<DATE>");
    let out = RE_TIME
        .get_or_init(|| regex::Regex::new(r"\d{2}:\d{2}:\d{2}").unwrap())
        .replace_all(&out, "<TIME>");
    let out = RE_FMAIL
        .get_or_init(|| regex::Regex::new(r"\bfmail-[0-9a-fA-F]{4,}").unwrap())
        .replace_all(&out, "fmail-HEX");
    let out = RE_MSG
        .get_or_init(|| regex::Regex::new(r"\bmsg-[0-9a-fA-F]{4,}").unwrap())
        .replace_all(&out, "msg-HEX");
    let out = RE_HEX8
        .get_or_init(|| regex::Regex::new(r"\b[0-9a-fA-F]{8}\b").unwrap())
        .replace_all(&out, "HEX8");
    let real = std::fs::canonicalize(root)
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let out = out.replace(&real, "<ROOT>");
    out.replace(&root.display().to_string(), "<ROOT>")
}

/// One registry row per capture fixture: same fields, same blanks.
fn row(name: &str, harness: &str, sid: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "name": name,
        "short_id": "",
        "harness": harness,
        "harness_session_id": sid,
        "cwd": "/tmp",
        "log_path": "/tmp/nope.log",
        "created_at": CREATED,
        "status": "live",
    })
}

fn seed_registry(root: &Path, rows: &[serde_json::Value]) {
    let home = root.join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    let doc = serde_json::json!({
        "schema_version": fno_agents::state::REGISTRY_SCHEMA_VERSION,
        "min_writer_version": fno_agents::state::REGISTRY_SCHEMA_VERSION,
        "agents": rows,
    });
    std::fs::write(home.join("registry.json"), doc.to_string()).unwrap();
}

fn row_worker() -> serde_json::Value {
    row("worker-1", "claude", Some(OTHER_SID))
}

fn row_dnd() -> serde_json::Value {
    let mut v = row("dnd-row-1", "claude", Some(DND_SID));
    v["delivery_policy"] = serde_json::json!("bus-only");
    v
}

fn row_self() -> serde_json::Value {
    row("self-row-1", "claude", Some(SID))
}

fn row_codex() -> serde_json::Value {
    row("codework-1", "codex", Some(CODEX_SID))
}

/// One sentence, well over the 25-word cap: the length rule's cleanest
/// tripwire.
const STYLE_BODY: &str = "This single sentence deliberately pads itself with one filler word after another until it blows straight past the twenty five word length cap that the mail style gate enforces on every authored relay body it checks.";

struct Case {
    label: &'static str,
    argv: &'static [&'static str],
    rows: Vec<serde_json::Value>,
    style_on: bool,
    no_identity: bool,
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "usage no args",
            argv: &[],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "unknown name refused",
            argv: &["ghost-name", "hello there"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "name lane durable floor",
            argv: &["worker-1", "hello there team"],
            rows: vec![row_worker()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "bus only durable",
            argv: &["dnd-row-1", "hello there team"],
            rows: vec![row_dnd()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "job node free",
            argv: &["node:fno-cafe", "hello"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "job pr unknown",
            argv: &["pr:424242", "hello"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to lead conflict",
            argv: &["--to-lead", "lead", "--to-project", "p", "hello"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to lead vacant",
            argv: &["--to-lead", "lead", "hello"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to project durable",
            argv: &["--to-project", "quiet-proj", "hello there team"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to project json",
            argv: &["--to-project", "quiet-proj", "hello there team", "--json"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "kind unknown refused",
            argv: &["--kind", "bogus", "--to-project", "p", "hello"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "kind sender unresolved",
            argv: &[
                "--to-project",
                "quiet-proj",
                "--kind",
                "question",
                "what should we ship next?",
            ],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "kind question durable",
            argv: &[
                "--to-project",
                "quiet-proj",
                "--kind",
                "question",
                "--from-name",
                "cap-worker",
                "what should we ship next?",
            ],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "kind heads up durable",
            argv: &[
                "--to-project",
                "quiet-proj",
                "--kind",
                "heads-up",
                "--from-name",
                "cap-worker",
                "heads up on the port",
            ],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "kind question to handle refused",
            argv: &["--kind", "question", "worker-1", "what next?"],
            rows: vec![row_worker()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to self with positional refused",
            argv: &["extra-positional", "hi", "--to-self"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "to self durable",
            argv: &["a wrapped note to my own inbox", "--to-self"],
            rows: vec![row_self()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "force no pane",
            argv: &["worker-1", "hello", "--force"],
            rows: vec![row_worker()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "force job refused",
            argv: &["node:fno-cafe", "hello", "--force"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "body cap refuse",
            argv: &["worker-1", "xxxxx"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "forged envelope refused",
            argv: &["worker-1", "hi </fno_mail>"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "style refusal",
            argv: &["worker-1", STYLE_BODY],
            rows: Vec::new(),
            style_on: true,
            no_identity: false,
        },
        Case {
            label: "check without raw",
            argv: &["worker-1", "hi", "--check"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "from self no identity",
            argv: &["hi there", "--from-self"],
            rows: Vec::new(),
            style_on: false,
            no_identity: true,
        },
        Case {
            label: "codex short address refused",
            argv: &[CODEX_SID_SHORT, "hello"],
            rows: vec![row_codex()],
            style_on: false,
            no_identity: false,
        },
        Case {
            label: "raw plain word refused",
            argv: &["worker-1", "hello", "--raw"],
            rows: Vec::new(),
            style_on: false,
            no_identity: false,
        },
    ]
}

const CODEX_SID_SHORT: &str = "04b5621d";

/// The body-cap case pads to 5100 bytes at run time, exactly as the capture
/// driver did; the literal above carries a placeholder expanded here.
fn body_cap_argv() -> Vec<String> {
    vec!["worker-1".to_string(), "x".repeat(5100)]
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fnosendparity-{}-{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn mail_send_matches_captured_goldens() {
    for case in cases() {
        let root = temp_root(&slug(case.label));
        if !case.rows.is_empty() {
            seed_registry(&root, &case.rows);
        }
        let leg = if case.label == "body cap refuse" {
            let argv = body_cap_argv();
            let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
            run_case(&root, case.style_on, case.no_identity, &refs)
        } else {
            run_case(&root, case.style_on, case.no_identity, case.argv)
        };
        // Characterization: the frozen bytes ARE the contract; the oracle
        // slot stays None (the capture driver owns the lineage).
        assert_golden("mail_send", case.label, &leg.golden(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}

// ---------------------------------------------------------------------------
// Live-lane contracts
// ---------------------------------------------------------------------------
//
// The goldens above freeze the durable floor. These rows drive the live
// lanes through a shell stub that stands in for every child the send leaf
// shells: `FNO_AGENTS_BIN` (the `mail-inject` lanes) and `FNO_BIN` (the
// `agents resume` wake and the `mux pane read|send` pane lane). The stub logs
// each call, snapshots the bus at inject time, answers from the row's env,
// and execs the real binary for every other argv (the receipt door).

const ASLEEP_SID: &str = "c4d5e6f7-1111-2222-3333-444455556666";
const LIVE_SID: &str = "e5f6a7b8-1111-2222-3333-444455556666";
const UNKNOWN_SID: &str = "f6a7b8c9-1111-2222-3333-444455556666";
const TWIN_A_SID: &str = "a7b8c9d0-1111-2222-3333-444455556666";
const TWIN_B_SID: &str = "a7b8c9d0-9999-8888-7777-666655554444";

const STUB: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$STUB_ROOT/calls.log"
if [ "$1" = "mail-inject" ]; then
  cat > /dev/null
  bus="$FNO_BUS_DIR/messages.jsonl"
  if [ -f "$bus" ]; then
    printf 'bus-at-inject=%s\n' "$(wc -l < "$bus" | tr -d ' ')" >> "$STUB_ROOT/calls.log"
  else
    printf 'bus-at-inject=0\n' >> "$STUB_ROOT/calls.log"
  fi
  miss='{"delivered": false, "reason": "not-injectable"}'
  if [ "$3" = "codex" ]; then
    printf '%s\n' "${STUB_INJECT_CODEX:-$miss}"
  else
    printf '%s\n' "${STUB_INJECT_CLAUDE:-$miss}"
  fi
  exit 0
fi
if [ "$1" = "agents" ] && [ "$2" = "resume" ]; then
  exit "${STUB_RESUME_EXIT:-1}"
fi
if [ "$1" = "mux" ] && [ "$2" = "pane" ] && [ "$3" = "read" ]; then
  printf '{"text": "ready"}\n'
  exit 0
fi
if [ "$1" = "mux" ] && [ "$2" = "pane" ] && [ "$3" = "send" ]; then
  exit "${STUB_PANE_EXIT:-0}"
fi
exec "$STUB_REAL" "$@"
"#;

/// One assistant turn. `API Error` reads stalled at any age; plain text
/// written now reads working.
fn transcript_line(text: &str) -> String {
    serde_json::json!({
        "type": "assistant",
        "timestamp": chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
        "message": {"role": "assistant", "model": "claude-test", "content": [{"type": "text", "text": text}]},
    })
    .to_string()
}

fn plant_transcript(root: &Path, sid: &str, text: &str) {
    let dir = root.join("projects").join("-repo");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{sid}.jsonl")),
        format!("{}\n", transcript_line(text)),
    )
    .unwrap();
}

fn plant_asleep(root: &Path, sid: &str) {
    plant_transcript(root, sid, "API Error: Request rejected (529) overloaded");
}

fn mux_row(name: &str, sid: &str, status: &str) -> serde_json::Value {
    let mut v = row(name, "claude", Some(sid));
    v["status"] = serde_json::json!(status);
    v["mux"] = serde_json::json!({"session": "s1", "pane_id": 7});
    v
}

struct LiveOut {
    exit: i32,
    stdout: String,
    stderr: String,
    calls: String,
    bus: Vec<serde_json::Value>,
    events: Vec<serde_json::Value>,
}

impl LiveOut {
    fn bus_rows_for(&self, msg_id: &str) -> usize {
        self.bus
            .iter()
            .filter(|r| r["msg_id"] == msg_id || r["id"] == msg_id)
            .count()
    }
    fn receipt_msg_id(&self) -> String {
        self.stdout
            .lines()
            .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .and_then(|v| v["msg_id"].as_str().map(str::to_string))
            .unwrap_or_default()
    }
    fn typed_rows(&self) -> Vec<&serde_json::Value> {
        self.bus
            .iter()
            .filter(|r| r["delivery"] == "typed")
            .collect()
    }
    fn inject_calls(&self) -> Vec<&str> {
        self.calls
            .lines()
            .filter(|l| l.starts_with("mail-inject"))
            .collect()
    }
}

fn read_jsonl(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// An event field at the top level or inside the emitter's payload object.
fn event_field<'a>(row: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    ["", "data", "payload", "fields"].iter().find_map(|nest| {
        let scope = if nest.is_empty() {
            row
        } else {
            row.get(*nest)?
        };
        scope.get(key).filter(|v| !v.is_null())
    })
}

fn events_of<'a>(out: &'a LiveOut, kind: &str) -> Vec<&'a serde_json::Value> {
    out.events
        .iter()
        .filter(|e| {
            ["type", "event", "kind"]
                .iter()
                .any(|k| e.get(*k).and_then(|v| v.as_str()) == Some(kind))
        })
        .collect()
}

fn live_root(label: &str, rows: &[serde_json::Value]) -> PathBuf {
    let root = temp_root(&format!("live-{}", slug(label)));
    seed_registry(&root, rows);
    let stub = root.join("stub.sh");
    std::fs::write(&stub, STUB).unwrap();
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    root
}

fn live_command(root: &Path, env: &[(&str, &str)], argv: &[&str]) -> Command {
    let stub = root.join("stub.sh");
    let mut cmd = Command::new(binary_path());
    base_env(&mut cmd, root);
    cmd.env("FNO_AGENTS_BIN", &stub)
        .env("FNO_BIN", &stub)
        .env("STUB_ROOT", root)
        .env("STUB_REAL", binary_path())
        .env("FNO_CLAUDE_PROJECTS_DIR", root.join("projects"));
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.args(["mail", "send"]).args(argv).current_dir(root);
    cmd
}

fn collect(root: &Path, out: std::process::Output) -> LiveOut {
    LiveOut {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        calls: std::fs::read_to_string(root.join("calls.log")).unwrap_or_default(),
        bus: read_jsonl(&root.join("bus").join("messages.jsonl")),
        events: read_jsonl(&root.join("events.jsonl")),
    }
}

struct LiveRow {
    contract: &'static str,
    rows: Vec<serde_json::Value>,
    setup: fn(&Path),
    env: &'static [(&'static str, &'static str)],
    argv: Vec<String>,
    check: fn(&LiveOut),
}

fn run_live_rows(rows: Vec<LiveRow>) {
    for row in rows {
        let root = live_root(row.contract, &row.rows);
        (row.setup)(&root);
        let argv: Vec<&str> = row.argv.iter().map(String::as_str).collect();
        let out = live_command(&root, row.env, &argv)
            .output()
            .expect("run mail send");
        let leg = collect(&root, out);
        let report = format!(
            "contract {}: exit={}\nstdout:\n{}\nstderr:\n{}\ncalls:\n{}",
            row.contract, leg.exit, leg.stdout, leg.stderr, leg.calls
        );
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (row.check)(&leg)));
        let _ = std::fs::remove_dir_all(&root);
        if outcome.is_err() {
            panic!("{report}");
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn no_setup(_: &Path) {}

const CLAUDE_DELIVERS: &[(&str, &str)] = &[(
    "STUB_INJECT_CLAUDE",
    r#"{"delivered": true, "reason": "ok"}"#,
)];
const CODEX_DELIVERS: &[(&str, &str)] = &[(
    "STUB_INJECT_CODEX",
    r#"{"delivered": true, "reason": "ok"}"#,
)];
const WAKE_OK: &[(&str, &str)] = &[("STUB_RESUME_EXIT", "0")];
const PANE_FAILS: &[(&str, &str)] = &[("STUB_PANE_EXIT", "1")];
const NOT_CONFIRMED: &[(&str, &str)] = &[(
    "STUB_INJECT_CLAUDE",
    r#"{"delivered": false, "reason": "not-confirmed"}"#,
)];

/// Delivery-matrix lanes below discovery: the inject probe, the asleep
/// resolution, and the wake rung (swept Python: test_mail_delivery_matrix).
#[test]
fn mail_send_delivery_matrix_lanes() {
    run_live_rows(vec![
        LiveRow {
            contract: "1a-discovery-miss-still-injects",
            rows: vec![],
            setup: no_setup,
            env: CLAUDE_DELIVERS,
            argv: args(&[UNKNOWN_SID, "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("delivered (hosted)"));
                assert!(o
                    .calls
                    .contains(&format!("--harness claude --session {UNKNOWN_SID}")));
            },
        },
        LiveRow {
            contract: "1b-inject-precedes-durable-write",
            rows: vec![],
            setup: |r| plant_asleep(r, ASLEEP_SID),
            env: &[],
            argv: args(&[ASLEEP_SID, "hello there team"]),
            check: |o| {
                // At inject time only the staged body row exists; the durable
                // envelope for the same msg id lands after the live miss.
                assert!(o.calls.contains("bus-at-inject=1"));
                assert!(o.stdout.contains("queued (durable)"));
                assert_eq!(o.bus_rows_for(&o.receipt_msg_id()), 2);
            },
        },
        LiveRow {
            contract: "1c-asleep-session-woken-not-queued",
            rows: vec![],
            setup: |r| plant_asleep(r, ASLEEP_SID),
            env: WAKE_OK,
            argv: args(&[ASLEEP_SID, "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("delivered (woken)"));
                assert!(!o.stdout.contains("queued"));
                assert!(o
                    .calls
                    .contains(&format!("agents resume {ASLEEP_SID} --message")));
            },
        },
        LiveRow {
            contract: "1d-wake-prompt-envelope-wrapped",
            rows: vec![],
            setup: |r| plant_asleep(r, ASLEEP_SID),
            env: WAKE_OK,
            argv: args(&[ASLEEP_SID, "hello there team"]),
            check: |o| {
                let wake = o.calls.split("agents resume").nth(1).unwrap_or_default();
                assert!(wake.contains("--message <fno_mail"));
                assert!(wake.contains("hello there team"));
            },
        },
        LiveRow {
            contract: "1e-failed-wake-demotes-with-lane-receipt",
            rows: vec![],
            setup: |r| plant_asleep(r, ASLEEP_SID),
            env: &[],
            argv: args(&[ASLEEP_SID, "hello there team"]),
            check: |o| {
                assert!(o
                    .stderr
                    .contains("lanes tried: inject=not-delivered, wake=resume-failed"));
                assert!(o.stdout.contains("queued (durable)"));
            },
        },
        LiveRow {
            contract: "1f-ambiguous-short-id-wakes-nothing",
            rows: vec![],
            setup: |r| {
                plant_asleep(r, TWIN_A_SID);
                plant_asleep(r, TWIN_B_SID);
            },
            env: WAKE_OK,
            argv: args(&["a7b8c9d0", "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 2);
                assert!(o.stderr.contains(TWIN_A_SID) && o.stderr.contains(TWIN_B_SID));
                assert!(o.calls.is_empty());
                assert!(o.bus.is_empty());
            },
        },
        LiveRow {
            contract: "1g-unreadable-store-never-injects",
            rows: vec![],
            setup: |r| {
                std::fs::write(r.join("agents-home").join("name-map.json"), "{torn").unwrap()
            },
            env: CLAUDE_DELIVERS,
            argv: args(&["b8c9d0e1", "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 2);
                assert!(o.stderr.contains("unreadable stores: registry"));
                assert!(o.calls.is_empty());
            },
        },
        LiveRow {
            contract: "1i-live-failure-receipt-positives-durable-leg",
            rows: vec![],
            setup: |r| plant_asleep(r, ASLEEP_SID),
            env: NOT_CONFIRMED,
            argv: args(&[ASLEEP_SID, "hello there team"]),
            check: |o| {
                assert!(o.stdout.contains("durable leg holds"));
                assert!(!o.stdout.contains("not-confirmed"));
            },
        },
        LiveRow {
            contract: "1j-codex-probed-when-nothing-resolved",
            rows: vec![],
            setup: no_setup,
            env: CODEX_DELIVERS,
            argv: args(&[UNKNOWN_SID, "hello there team"]),
            check: |o| {
                let calls = o.inject_calls();
                assert_eq!(calls.len(), 2);
                assert!(calls[0].contains("--harness claude"));
                assert!(calls[1].contains("--harness codex"));
                assert!(o.stdout.contains("delivered (hosted)"));
            },
        },
    ]);
}

/// The --force pane lane: transport changes, mail semantics stay (swept
/// Python: test_mail_force).
#[test]
fn mail_send_force_pane_lane() {
    run_live_rows(vec![
        LiveRow {
            contract: "2a-refused-pane-write-leaves-staged-row",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: no_setup,
            env: PANE_FAILS,
            argv: args(&["worker-1", "hello there team", "--force"]),
            check: |o| {
                assert_eq!(o.exit, 1);
                assert_eq!(o.bus.len(), 1);
                assert!(o.typed_rows().is_empty());
            },
        },
        LiveRow {
            contract: "2b-force-receipt-typed-never-delivered",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: no_setup,
            env: &[],
            argv: args(&["worker-1", "hello there team", "--force"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("typed (pane 7)"));
                assert!(!o.stdout.contains("delivered"));
            },
        },
        LiveRow {
            contract: "2c-force-refuses-non-live-row",
            rows: vec![mux_row("worker-1", OTHER_SID, "exited")],
            setup: no_setup,
            env: &[],
            argv: args(&["worker-1", "hello there team", "--force"]),
            check: |o| {
                assert_eq!(o.exit, 1);
                assert!(o.stderr.contains("is exited"));
                assert!(!o.calls.contains("mux pane send"));
            },
        },
        LiveRow {
            contract: "2d-force-refuses-this-session",
            rows: vec![mux_row("self-row-1", SID, "live")],
            setup: no_setup,
            env: &[],
            argv: args(&[SID, "hello there team", "--force"]),
            check: |o| {
                assert_eq!(o.exit, 2);
                assert!(o
                    .stderr
                    .contains("cannot type into this session's own prompt"));
                assert!(!o.calls.contains("mux pane send"));
            },
        },
        LiveRow {
            contract: "2e-outbox-row-names-the-pane",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: no_setup,
            env: &[],
            argv: args(&["worker-1", "hello there team", "--force"]),
            check: |o| {
                let typed = o.typed_rows();
                assert_eq!(typed.len(), 1);
                assert_eq!(typed[0]["meta"]["pane_id"], "7");
            },
        },
    ]);
}

/// Registered and discovered live lanes for codex and claude recipients
/// (swept Python: test_send, test_mail_cli).
#[test]
fn mail_send_codex_and_claude_live_lanes() {
    run_live_rows(vec![
        LiveRow {
            contract: "3a-codex-hosted-inject-short-circuits-durable",
            rows: vec![],
            setup: no_setup,
            env: CODEX_DELIVERS,
            argv: args(&[UNKNOWN_SID, "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("delivered (hosted)"));
                assert!(o.bus.iter().all(|r| r["delivery"] == "hosted"));
                assert!(!o.calls.contains("agents resume"));
            },
        },
        LiveRow {
            contract: "3b-pane-rung-delivers-on-socket-miss",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: no_setup,
            env: &[],
            argv: args(&["worker-1", "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("delivered (hosted)"));
                assert!(o.calls.contains("mux pane send 7"));
            },
        },
        LiveRow {
            contract: "3c-pane-rung-failure-falls-closed",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: no_setup,
            env: PANE_FAILS,
            argv: args(&["worker-1", "hello there team"]),
            check: |o| {
                assert!(o.stdout.contains("queued (durable)"));
                assert!(!o.stdout.contains("delivered"));
                assert_eq!(o.calls.matches("mux pane send 7").count(), 2);
            },
        },
        LiveRow {
            contract: "3d-transcript-veto-names-the-reading",
            rows: vec![mux_row("worker-1", OTHER_SID, "live")],
            setup: |r| plant_asleep(r, OTHER_SID),
            env: CLAUDE_DELIVERS,
            argv: args(&["worker-1", "hello there team"]),
            check: |o| {
                assert!(o.stdout.contains("[transcript-stalled"));
                assert!(!o.stdout.contains("live-miss"));
                assert!(o.inject_calls().is_empty());
                assert!(!o.calls.contains("mux pane send"));
            },
        },
        LiveRow {
            contract: "3e-registered-handle-colliding-with-discovery-sends-nothing",
            rows: vec![row_worker()],
            setup: |r| {
                let mut map = serde_json::Map::new();
                map.insert(LIVE_SID.to_string(), serde_json::json!("worker-1"));
                let map = serde_json::Value::Object(map).to_string();
                std::fs::write(r.join("agents-home").join("name-map.json"), map).unwrap();
                plant_transcript(r, LIVE_SID, "Running the tests now.");
            },
            env: CLAUDE_DELIVERS,
            argv: args(&["worker-1", "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 2);
                assert!(o.stderr.contains("ambiguous"));
                assert!(o.calls.is_empty());
                assert!(o.bus.is_empty());
            },
        },
        LiveRow {
            contract: "3f-discovered-handle-is-session-addressed",
            rows: vec![],
            setup: |r| plant_transcript(r, LIVE_SID, "Running the tests now."),
            env: CLAUDE_DELIVERS,
            argv: args(&[LIVE_SID, "hello there team"]),
            check: |o| {
                assert_eq!(o.exit, 0);
                assert!(o.stdout.contains("delivered (hosted) [live claude session"));
                assert!(o.calls.contains(&format!("--session {LIVE_SID}")));
                assert!(!o.calls.contains("agents resume"));
            },
        },
    ]);
}

/// Receipts and events on the failure paths (swept Python: test_send).
#[test]
fn mail_send_failure_receipts() {
    run_live_rows(vec![LiveRow {
        contract: "4a-send-failed-carries-msg-id",
        rows: vec![row_worker()],
        // A regular file where the bus dir belongs: every bus append fails
        // after the msg id is minted.
        setup: |r| std::fs::write(r.join("bus"), "not a dir").unwrap(),
        env: &[],
        argv: args(&["worker-1", "hello there team"]),
        check: |o| {
            assert_eq!(o.exit, 12);
            let started = events_of(o, "agent_send_started");
            let failed: Vec<_> = events_of(o, "agent_send_failed")
                .into_iter()
                .filter(|e| {
                    event_field(e, "stage").and_then(|v| v.as_str()) == Some("envelope-write")
                })
                .collect();
            assert_eq!(failed.len(), 1);
            assert!(event_field(failed[0], "msg_id").is_some());
            assert_eq!(
                event_field(failed[0], "msg_id"),
                event_field(started[0], "msg_id")
            );
        },
    }]);

    // contract: "4b-lock-timeout-names-grace-never-not-live"
    // It stages a held agent lock, so it runs outside the table: the test
    // holds the flock and releases it when the grace acquire starts waiting.
    let contract = "4b-lock-timeout-names-grace-never-not-live";
    let root = live_root(contract, &[row_worker()]);
    let lock_dir = root.join("agents-home").join("locks");
    std::fs::create_dir_all(&lock_dir).unwrap();
    let held = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(lock_dir.join("worker-1.lock"))
        .unwrap();
    use std::os::unix::io::AsRawFd as _;
    assert_eq!(
        unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    let mut child = live_command(
        &root,
        &[("_FNO_MACHINE_MAIL_LOCK_TIMEOUT", "0.2")],
        &["worker-1", "hello there team"],
    )
    .stdout(std::process::Stdio::piped())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .expect("spawn mail send");
    let stdout_pipe = child.stdout.take().unwrap();
    let stdout_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = std::io::Read::read_to_string(&mut { stdout_pipe }, &mut text);
        text
    });
    let mut stderr = String::new();
    let mut reader = std::io::BufReader::new(child.stderr.take().unwrap());
    let mut released = false;
    loop {
        let mut line = String::new();
        if std::io::BufRead::read_line(&mut reader, &mut line).unwrap_or(0) == 0 {
            break;
        }
        if !released && line.starts_with("Waiting for agent") {
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) };
            released = true;
        }
        stderr.push_str(&line);
    }
    let _ = child.wait();
    let stdout = stdout_reader.join().unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);
    let report = format!("contract {contract}:\nstdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(released, "{report}");
    assert!(
        stdout.contains("queued (durable)") && stdout.contains("agent-lock-timeout"),
        "{report}"
    );
    assert!(stderr.contains("lock was busy"), "{report}");
    assert!(
        !stderr.contains("is not live") && !stderr.contains("fno agents resume"),
        "{report}"
    );
}
