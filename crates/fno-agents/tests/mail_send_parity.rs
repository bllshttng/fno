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

use common::{assert_golden, make_script, slug, Golden};
use fno_agents::state_layout::place;
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

// Live lanes: a stub stands in for every child the leaf shells (FNO_AGENTS_BIN,
// FNO_BIN), logs each call, and execs the real binary for any other argv.

const ASLEEP_SID: &str = "c4d5e6f7-1111-2222-3333-444455556666";
const LIVE_SID: &str = "e5f6a7b8-1111-2222-3333-444455556666";
const BODY: &str = "hello there team";
const OK: &str = r#"{"delivered": true, "reason": "ok"}"#;
const FORCE: &[&str] = &["worker-1", BODY, "--force"];

const STUB: &str = r#"printf '%s\n' "$*" >> "$STUB_ROOT/calls.log"
miss='{"delivered": false, "reason": "not-injectable"}'
case "$1 $2 $3" in
  mail-inject*)
    cat > /dev/null
    echo "bus-at-inject=$(cat "$FNO_BUS_DIR/messages.jsonl" 2>/dev/null | wc -l | tr -d ' ')" >> "$STUB_ROOT/calls.log"
    if [ "$3" = codex ]; then echo "${STUB_INJECT_CODEX:-$miss}"; else echo "${STUB_INJECT_CLAUDE:-$miss}"; fi
    exit 0 ;;
  "agents resume"*) exit "${STUB_RESUME_EXIT:-1}" ;;
  "mux pane read") echo '{"text": "ready"}'; exit 0 ;;
  "mux pane send") exit "${STUB_PANE_EXIT:-0}" ;;
esac
exec "$STUB_REAL" "$@""#;

/// One assistant turn: `API Error` reads stalled; fresh plain text, working.
fn plant_turn(root: &Path, sid: &str, text: &str) {
    let dir = root.join("projects").join("-repo");
    std::fs::create_dir_all(&dir).unwrap();
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let content = serde_json::json!([{"type": "text", "text": text}]);
    let msg = serde_json::json!({"role": "assistant", "model": "claude-test", "content": content});
    let turn = serde_json::json!({"type": "assistant", "timestamp": ts, "message": msg});
    std::fs::write(dir.join(format!("{sid}.jsonl")), format!("{turn}\n")).unwrap();
}

fn asleep(root: &Path, sid: &str) {
    plant_turn(root, sid, "API Error: Request rejected (529) overloaded");
}

fn pane_seed(root: &Path, name: &str, sid: &str, status: &str) {
    let mut v = row(name, "claude", Some(sid));
    v["status"] = serde_json::json!(status);
    v["mux"] = serde_json::json!({"session": "s1", "pane_id": 7});
    seed_registry(root, &[v]);
}

fn live_command(root: &Path, env: &[(&str, &str)], argv: &[&str]) -> Command {
    let stub = make_script(root, "stub.sh", STUB);
    let mut cmd = Command::new(binary_path());
    base_env(&mut cmd, root)
        .env("FNO_AGENTS_BIN", &stub)
        .env("FNO_BIN", &stub)
        .env("STUB_ROOT", root)
        .env("STUB_REAL", binary_path())
        .env("FNO_CLAUDE_PROJECTS_DIR", root.join("projects"))
        .envs(env.iter().copied())
        .args(["mail", "send"])
        .args(argv)
        .current_dir(root);
    cmd
}

#[derive(Clone, Copy, Debug)]
enum Stream {
    Stdout,
    Stderr,
    Log,
    Bus,
    Events,
}

/// `Lines`: n lines carry it (Bus, Events: compact JSON rows). `Joined`: failure msg id = start's.
#[derive(Debug)]
enum Want {
    Exit(i32),
    Has(Stream, &'static str),
    Lacks(Stream, &'static str),
    Lines(Stream, &'static str, usize),
    Joined,
}

struct Live {
    contract: &'static str,
    seed: fn(&Path),
    env: &'static [(&'static str, &'static str)],
    argv: &'static [&'static str],
    want: Vec<Want>,
}

/// Hand-laid so each case reads as one row; folded ids sit in comments above.
#[rustfmt::skip]
fn live_cases() -> Vec<Live> {
    use Stream::*;
    use Want::*;
    vec![
        // contract: "1j-codex-probed-when-nothing-resolved"
        // contract: "3a-codex-hosted-inject-short-circuits-durable"
        Live { contract: "1a-discovery-miss-still-injects", seed: |_| {}, env: &[("STUB_INJECT_CODEX", OK)],
            argv: &["f6a7b8c9-1111-2222-3333-444455556666", BODY],
            want: vec![Exit(0), Has(Stdout, "delivered (hosted)"), Lacks(Stdout, "queued"), Lacks(Log, "agents resume"),
                Has(Log, "mail-inject --harness claude --session f6a7b8c9-1111"), Lines(Log, "mail-inject --harness codex", 1)] },
        // contract: "1e-failed-wake-demotes-with-lane-receipt"
        Live { contract: "1b-inject-precedes-durable-write", seed: |r| asleep(r, ASLEEP_SID), env: &[],
            argv: &[ASLEEP_SID, BODY],
            want: vec![Has(Log, "bus-at-inject=1"), Lines(Bus, "", 2), Has(Stdout, "queued (durable)"),
                Has(Stderr, "lanes tried: inject=not-delivered, wake=resume-failed")] },
        // contract: "1d-wake-prompt-envelope-wrapped"
        Live { contract: "1c-asleep-session-woken-not-queued", seed: |r| asleep(r, ASLEEP_SID),
            env: &[("STUB_RESUME_EXIT", "0")], argv: &[ASLEEP_SID, BODY],
            want: vec![Exit(0), Has(Stdout, "delivered (woken)"), Lacks(Stdout, "queued"), Has(Log, BODY),
                Has(Log, "agents resume c4d5e6f7-1111-2222-3333-444455556666 --message `@a1b2c3d4 · fmail-")] },
        Live { contract: "1f-ambiguous-short-id-wakes-nothing",
            seed: |r| { asleep(r, "a7b8c9d0-1111-2222-3333-444455556666"); asleep(r, "a7b8c9d0-9999-8888-7777-666655554444") },
            env: &[("STUB_RESUME_EXIT", "0")], argv: &["a7b8c9d0", BODY],
            want: vec![Exit(2), Has(Stderr, "a7b8c9d0-1111-2222-3333-444455556666"),
                Has(Stderr, "a7b8c9d0-9999-8888-7777-666655554444"), Lacks(Log, "agents resume"), Lines(Bus, "", 0)] },
        Live { contract: "1g-unreadable-store-never-injects",
            seed: |r| std::fs::write(r.join("agents-home").join("name-map.json"), "{torn").unwrap(),
            env: &[("STUB_INJECT_CLAUDE", OK)], argv: &["b8c9d0e1", BODY],
            want: vec![Exit(2), Has(Stderr, "unreadable stores: registry"), Lacks(Log, "mail-inject")] },
        Live { contract: "1i-live-failure-receipt-positives-durable-leg", seed: |r| asleep(r, ASLEEP_SID),
            env: &[("STUB_INJECT_CLAUDE", r#"{"delivered": false, "reason": "not-confirmed"}"#)],
            argv: &[ASLEEP_SID, BODY], want: vec![Has(Stdout, "durable leg holds"), Lacks(Stdout, "not-confirmed")] },
        Live { contract: "2a-refused-pane-write-leaves-staged-row", seed: |r| pane_seed(r, "worker-1", OTHER_SID, "live"), env: &[("STUB_PANE_EXIT", "1")], argv: FORCE, want: vec![Exit(1), Lines(Bus, "", 1), Lacks(Bus, r#""delivery":"typed""#)] },
        // contract: "2e-outbox-row-names-the-pane"
        Live { contract: "2b-force-receipt-typed-never-delivered", seed: |r| pane_seed(r, "worker-1", OTHER_SID, "live"), env: &[], argv: FORCE,
            want: vec![Exit(0), Has(Stdout, "typed (pane 7)"), Lacks(Stdout, "delivered"),
                Lines(Bus, r#""delivery":"typed""#, 1), Has(Bus, r#""pane_id":"7""#)] },
        Live { contract: "2c-force-refuses-non-live-row", seed: |r| pane_seed(r, "worker-1", OTHER_SID, "exited"), env: &[], argv: FORCE, want: vec![Exit(1), Has(Stderr, "is exited"), Lacks(Log, "mux pane send")] },
        Live { contract: "2d-force-refuses-this-session", seed: |r| pane_seed(r, "self-row-1", SID, "live"),
            env: &[], argv: &[SID, BODY, "--force"],
            want: vec![Exit(2), Has(Stderr, "cannot type into this session's own prompt"), Lacks(Log, "mux pane send")] },
        Live { contract: "3b-pane-rung-delivers-on-socket-miss", seed: |r| pane_seed(r, "worker-1", OTHER_SID, "live"), env: &[], argv: &["worker-1", BODY], want: vec![Exit(0), Has(Stdout, "delivered (hosted)"), Has(Log, "mux pane send 7")] },
        Live { contract: "3c-pane-rung-failure-falls-closed", seed: |r| pane_seed(r, "worker-1", OTHER_SID, "live"), env: &[("STUB_PANE_EXIT", "1")],
            argv: &["worker-1", BODY],
            want: vec![Has(Stdout, "queued (durable)"), Lacks(Stdout, "delivered"), Lines(Log, "mux pane send 7", 2)] },
        Live { contract: "3d-transcript-veto-names-the-reading", seed: |r| { pane_seed(r, "worker-1", OTHER_SID, "live"); asleep(r, OTHER_SID) },
            env: &[("STUB_INJECT_CLAUDE", OK)], argv: &["worker-1", BODY],
            want: vec![Has(Stdout, "[transcript-stalled"), Lacks(Stdout, "live-miss"), Lines(Log, "mail-inject", 0),
                Lacks(Log, "mux pane send")] },
        Live { contract: "3e-registered-handle-colliding-with-discovery-sends-nothing",
            seed: |r| { seed_registry(r, &[row_worker()]); plant_turn(r, LIVE_SID, "Running the tests now.");
                std::fs::write(r.join("agents-home").join("name-map.json"), serde_json::json!({ LIVE_SID: "worker-1" }).to_string()).unwrap() },
            env: &[("STUB_INJECT_CLAUDE", OK)], argv: &["worker-1", BODY],
            want: vec![Exit(2), Has(Stderr, "ambiguous"), Lacks(Log, "mail-inject"), Lines(Bus, "", 0)] },
        Live { contract: "3f-discovered-handle-is-session-addressed", seed: |r| plant_turn(r, LIVE_SID, "Running the tests now."),
            env: &[("STUB_INJECT_CLAUDE", OK)], argv: &[LIVE_SID, BODY],
            want: vec![Exit(0), Has(Stdout, "delivered (hosted) [live claude session"), Has(Log, "--session e5f6a7b8-1111"),
                Lacks(Log, "agents resume")] },
        // G1: a regular file where the bus dir belongs fails every append
        // after the msg id is minted.
        Live { contract: "4a-send-failed-carries-msg-id",
            seed: |r| { seed_registry(r, &[row_worker()]); std::fs::write(r.join("bus"), "not a dir").unwrap() },
            env: &[], argv: &["worker-1", BODY],
            want: vec![Exit(12), Lines(Events, r#""stage":"envelope-write""#, 1), Joined] },
    ]
}

/// Swept Python: test_mail_delivery_matrix, test_mail_force, test_send, test_mail_cli.
#[test]
fn mail_send_live_lanes() {
    let parse = |l: &str| serde_json::from_str::<serde_json::Value>(l).ok();
    let dump = |text: String| -> String {
        text.lines()
            .filter_map(parse)
            .map(|v| format!("{v}\n"))
            .collect()
    };
    let count = |text: &str, needle: &str| text.lines().filter(|l| l.contains(needle)).count();
    let mut failed = Vec::new();
    for case in live_cases() {
        let id = case.contract;
        let root = temp_root(&format!("live-{}", slug(case.contract)));
        seed_registry(&root, &[]);
        (case.seed)(&root);
        let out = live_command(&root, case.env, case.argv).output().unwrap();
        let exit = out.status.code();
        let read = |p: PathBuf| std::fs::read_to_string(p).unwrap_or_default();
        // The child routes events into its own state root's store; read that store.
        let journal = place(&root, "events.db").with_extension("jsonl");
        let events = fno_agents::event_store::query_events(&journal, &Default::default());
        let events: Vec<String> = events
            .unwrap_or_default()
            .into_iter()
            .map(|r| r.line)
            .collect();
        let s = [
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
            read(root.join("calls.log")),
            dump(read(root.join("bus").join("messages.jsonl"))),
            dump(events.join("\n")),
        ];
        let _ = std::fs::remove_dir_all(&root);
        let ids = |kind: &str| -> Vec<serde_json::Value> {
            let rows = s[4].lines().filter_map(parse).filter(|e| e["type"] == kind);
            rows.map(|e| e["data"]["msg_id"].clone()).collect()
        };
        for want in &case.want {
            let held = match *want {
                Want::Exit(code) => exit == Some(code),
                Want::Has(st, needle) => s[st as usize].contains(needle),
                Want::Lacks(st, needle) => !s[st as usize].contains(needle),
                Want::Lines(st, needle, n) => count(&s[st as usize], needle) == n,
                Want::Joined => {
                    let started = ids("agent_send_started");
                    ids("agent_send_failed")
                        .iter()
                        .any(|i| i.is_string() && started.contains(i))
                }
            };
            if !held {
                failed.push(format!("{id}: {want:?}, exit {exit:?}\n{s:#?}"));
            }
        }
    }
    assert!(failed.is_empty(), "{}", failed.join("\n"));
}

/// A held lock queues durable and never claims the peer is not live (test_send).
#[test]
fn mail_send_lock_timeout_names_the_grace() {
    // contract: "4b-lock-timeout-names-grace-never-not-live"
    use std::io::BufRead as _;
    use std::os::unix::io::AsRawFd as _;
    let root = temp_root("live-4b");
    seed_registry(&root, &[row_worker()]);
    let locks = root.join("agents-home").join("locks");
    std::fs::create_dir_all(&locks).unwrap();
    let held = std::fs::File::create(locks.join("worker-1.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX) }, 0);
    let lock_wait = [("_FNO_MACHINE_MAIL_LOCK_TIMEOUT", "0.2")];
    let mut child = live_command(&root, &lock_wait, &["worker-1", BODY])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn mail send");
    // Only the grace acquire waits past the 1s notice: release on its line.
    let mut stderr = String::new();
    for line in std::io::BufReader::new(child.stderr.take().unwrap()).lines() {
        let line = line.unwrap_or_default();
        if line.starts_with("Waiting for agent") {
            unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_UN) };
        }
        stderr.push_str(&format!("{line}\n"));
    }
    let out = child.wait_with_output().expect("wait mail send");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&root);
    let queued = stdout.contains("queued (durable)") && stdout.contains("agent-lock-timeout");
    let named = stderr.contains("Waiting for agent") && stderr.contains("lock was busy");
    let claims_dead = stderr.contains("is not live") || stderr.contains("fno agents resume");
    let report = format!("stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(queued && named && !claims_dead, "{report}");
}
