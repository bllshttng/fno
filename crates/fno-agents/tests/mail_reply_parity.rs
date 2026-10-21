//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_reply
//!
//! Characterization harness for the `mail reply` wrapped-verb port.
//! The goldens were captured from the Python leg (cmd_reply) before its body
//! was deleted, through tests/capture/mail_reply_capture.py; the frozen bytes
//! are the contract now that the front routes every reply to this leaf. The
//! hermetic root cannot offer a live recipient, so the deep live lanes
//! surface as their designed misses: the inject probe misses, the durable
//! floor writes, the landing report fails (exit 14), and the receipts name
//! why. That IS the contract the goldens freeze.
//!
//! Normalization mirrors the capture driver (tests/capture/mail_reply_capture.py):
//! the setup-noise drops (including the host-dependent escalation stderr
//! advisory), rstrip, sorted-compact JSON re-dump when the whole stream is
//! one object, then the TS/DATE/TIME, fmail-/msg- id, 8-hex, and ROOT scrubs.
//! Keep the two tables in lockstep when a case is added.
//!
//! The settle window is pinned to 0: the goldens were captured with the
//! settle pinned to 0 as well, which changes latency and never output,
//! because a hermetic root lands nothing.

mod common;

use common::{assert_golden, slug, Golden};
use std::path::{Path, PathBuf};
use std::process::Command;

const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";
const OTHER_SID: &str = "b2c3d4e5-1111-2222-3333-444455556666";
const CREATED: &str = "2026-10-12T09:00:00Z";

const DROP_PREFIXES: [&str; 6] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
    // The escalation stderr advisory names whether the OS notifier displayed,
    // a property of the host, not of the verb. The receipt is the contract.
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
        // Store-location overrides: an ambient value would point one or both
        // legs outside the fixture root.
        .env_remove("CLAUDE_CONFIG_DIR")
        .env_remove("FNO_CLAUDE_PROJECTS_DIR")
        .env_remove("FNO_CLAUDE_SESSIONS_DIR")
        .env_remove("FNO_CLAUDE_DAEMON_DIR")
        .env_remove("FNO_INBOX_ROOT")
        .env_remove("FNO_INBOX_KNOWN_PROJECTS")
        .env_remove("FNO_MAIL_BODY_WARN")
        .env_remove("FNO_MAIL_BODY_REFUSE")
        .env_remove("FNO_AGENT_SELF")
        .env_remove("FNO_MAIL_SUBJECT")
        .env_remove("FNO_MAIL_LOCAL_FORWARD")
}

fn run_case(
    root: &Path,
    style_on: bool,
    no_identity: bool,
    inbox_project: Option<&str>,
    argv: &[&str],
) -> Leg {
    let root_str = root.display().to_string();
    let argv: Vec<String> = argv
        .iter()
        .map(|a| a.replace("<ROOT>", &root_str))
        .collect();
    let mut cmd = Command::new(binary_path());
    let cmd = base_env(&mut cmd, root);
    // The capture driver pointed inbox_project cases at the fixture root;
    // without it the child's default inbox root lands under inbox/agents and
    // the seeded parent thread is never found (the reply orphans instead of
    // appending).
    let cmd = match inbox_project {
        Some(project) => cmd
            .env("FNO_INBOX_ROOT", root.join("inbox"))
            .env("FNO_INBOX_KNOWN_PROJECTS", project),
        None => cmd,
    };
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
        .args(["mail", "reply"])
        .args(&argv)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail reply");
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

fn bus_row(
    msg_id: &str,
    from_: &str,
    to: &str,
    kind: &str,
    to_kind: Option<&str>,
    from_session: Option<&str>,
) -> serde_json::Value {
    let mut r = serde_json::json!({
        "id": msg_id,
        "thread": msg_id,
        "from": from_,
        "to": to,
        "kind": kind,
        "body": "ping",
        "ts": "2026-10-12T09:00:00Z",
    });
    if let Some(k) = to_kind {
        r["to_kind"] = serde_json::json!(k);
    }
    if let Some(fs) = from_session {
        r["from_session"] = serde_json::json!(fs);
    }
    r
}

fn seed_bus(root: &Path, rows: &[serde_json::Value]) {
    let bus = root.join("bus");
    std::fs::create_dir_all(&bus).unwrap();
    let text = rows
        .iter()
        .map(|r| r.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(
        bus.join("messages.jsonl"),
        if rows.is_empty() {
            String::new()
        } else {
            format!("{text}\n")
        },
    )
    .unwrap();
}

fn seed_transcript_envelope(root: &Path, session_id: &str, from_: &str, msg_id: &str) {
    let enc = root.join(".claude").join("projects").join("-users-x-proj");
    std::fs::create_dir_all(&enc).unwrap();
    let envelope = format!(
        "<fno_mail from=\"{from_}\" harness=\"claude-code\" model=\"opus\" id=\"{msg_id}\">\nping\n</fno_mail>"
    );
    let line =
        serde_json::json!({"type": "user", "message": {"role": "user", "content": envelope}});
    std::fs::write(enc.join(format!("{session_id}.jsonl")), format!("{line}\n")).unwrap();
}

fn seed_torn_registry(root: &Path) {
    let home = root.join("agents-home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("registry.json"), "{ not json").unwrap();
}

/// One existing self-addressed thread under web's inbox plus its bus row:
/// the append path's parent. The frontmatter's `from:` names web, so the
/// reply's recipient re-finds the same file and appends.
fn seed_project_thread(root: &Path) -> &'static str {
    let thread = root
        .join("inbox")
        .join("web")
        .join("inbox")
        .join("2026-10-12-parent-ping.md");
    std::fs::create_dir_all(thread.parent().unwrap()).unwrap();
    std::fs::write(
        &thread,
        "---\nthread_id: msg-parent1\nfrom: web\nto: web\nkind: send\n---\n\n## msg-parent1 · 2026-10-12T09:00:00Z · from:web\n\nparent ping\n",
    )
    .unwrap();
    let row = bus_row("msg-parent1", "web", "web", "send", None, None);
    seed_bus(root, &[row.clone()]);
    // The real write path records the chats row before the bus write (the
    // record seam); the chats-recovery case wipes the bus afterward, leaving
    // the chats index as the only id -> from binding, exactly as the capture
    // left it. The dir name is the chat id slot; the resolver reads from_key
    // off the indexed row, never the dir name.
    let mut rec = row;
    if let serde_json::Value::Object(map) = &mut rec {
        map.insert("type".into(), serde_json::json!("message"));
        map.insert("chat_id".into(), serde_json::json!("chat-seedparent00000"));
    }
    let chat = root.join("chats").join("chat-seedparent00000");
    std::fs::create_dir_all(&chat).unwrap();
    let mut line = rec.to_string();
    line.push('\n');
    std::fs::write(chat.join("messages.jsonl"), line).unwrap();
    "msg-parent1"
}

/// One sentence, well over the 25-word cap: the length rule's cleanest
/// tripwire.
const STYLE_BODY: &str = "This single sentence deliberately pads itself with one filler word after another until it blows straight past the twenty five word length cap that the mail style gate enforces on every authored relay body it checks.";

struct Case {
    label: &'static str,
    argv: &'static [&'static str],
    rows: Vec<serde_json::Value>,
    bus: Vec<serde_json::Value>,
    transcript: Option<(&'static str, &'static str)>,
    torn_registry: bool,
    inbox_project: Option<&'static str>,
    parent_thread: bool,
    wipe_bus: bool,
    style_on: bool,
    json_out: bool,
}

fn plain(label: &'static str, argv: &'static [&'static str]) -> Case {
    Case {
        label,
        argv,
        rows: Vec::new(),
        bus: Vec::new(),
        transcript: None,
        torn_registry: false,
        inbox_project: None,
        parent_thread: false,
        wipe_bus: false,
        style_on: false,
        json_out: false,
    }
}

fn cases() -> Vec<Case> {
    vec![
        plain("usage no args", &[]),
        plain("missing to", &["hello there"]),
        plain("unknown msg id refused", &["--to", "msg-capnone", "ack"]),
        Case {
            bus: vec![bus_row(
                "msg-capn1",
                "9a063cd3",
                "claude-capture1",
                "send",
                Some("name"),
                None,
            )],
            ..plain("name lane durable floor", &["--to", "msg-capn1", "ack"])
        },
        Case {
            rows: vec![row_worker()],
            bus: vec![bus_row(
                "msg-capn2",
                "worker-1",
                "claude-capture1",
                "send",
                Some("name"),
                None,
            )],
            ..plain("name lane registry floor", &["--to", "msg-capn2", "ack"])
        },
        Case {
            bus: vec![bus_row(
                "msg-caps1",
                "9a063cd3",
                SID,
                "send",
                Some("session"),
                Some("11111111-2222-3333-4444-555566667777"),
            )],
            ..plain(
                "session lane full provenance floor",
                &["--to", "msg-caps1", "ack"],
            )
        },
        Case {
            bus: vec![bus_row(
                "msg-caps2",
                "9a063cd3",
                SID,
                "send",
                Some("session"),
                None,
            )],
            ..plain(
                "session lane requires resolution refused",
                &["--to", "msg-caps2", "ack"],
            )
        },
        Case {
            bus: vec![bus_row(
                "msg-capl1",
                "claude-deadbeef",
                "claude-capture1",
                "send",
                Some("name"),
                None,
            )],
            ..plain(
                "legacy sender migrated offline refused",
                &["--to", "msg-capl1", "ack"],
            )
        },
        Case {
            transcript: Some(("9a063cd3", "msg-live1")),
            ..plain(
                "us3 transcript recovery floors",
                &["--to", "msg-live1", "pong"],
            )
        },
        Case {
            bus: vec![bus_row(
                "msg-capf1",
                "9a063cd3",
                "claude-capture1",
                "send",
                Some("name"),
                None,
            )],
            torn_registry: true,
            ..plain("torn registry fail open", &["--to", "msg-capf1", "ack"])
        },
        plain(
            "two bodies refused",
            &["--to", "msg-capx1", "body-one", "--body", "body-two"],
        ),
        plain(
            "kind unknown refused",
            &["--to", "msg-capx2", "--kind", "bogus", "ack"],
        ),
        Case {
            style_on: true,
            ..plain("style refusal", &["--to", "msg-capx3", STYLE_BODY])
        },
        plain("body cap refuse", &["--to", "msg-capx4", "<CAPBODY>"]),
        Case {
            bus: vec![bus_row(
                "msg-capp1",
                "9a063cd3",
                "web",
                "send",
                Some("project"),
                None,
            )],
            inbox_project: Some("web"),
            ..plain("orphan self note", &["--to", "msg-capp1", "self note"])
        },
        Case {
            inbox_project: Some("web"),
            parent_thread: true,
            ..plain(
                "appended reply",
                &["--to", "<PARENT_MSG>", "appended answer"],
            )
        },
        Case {
            inbox_project: Some("web"),
            parent_thread: true,
            json_out: true,
            ..plain(
                "appended reply json",
                &["--to", "<PARENT_MSG>", "appended answer", "--json"],
            )
        },
        Case {
            inbox_project: Some("web"),
            parent_thread: true,
            wipe_bus: true,
            ..plain(
                "us3 chats index recovery floors",
                &["--to", "<PARENT_MSG>", "via chats"],
            )
        },
    ]
}

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("fnoreplyparity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn mail_reply_matches_captured_goldens() {
    for case in cases() {
        let root = temp_root(&slug(case.label));
        if !case.rows.is_empty() {
            seed_registry(&root, &case.rows);
        }
        if !case.bus.is_empty() {
            seed_bus(&root, &case.bus);
        }
        if let Some((from_, msg_id)) = case.transcript {
            seed_transcript_envelope(&root, SID, from_, msg_id);
        }
        if case.torn_registry {
            seed_torn_registry(&root);
        }
        if let Some(project) = case.inbox_project {
            let fno_dir = root.join(".fno");
            std::fs::create_dir_all(&fno_dir).unwrap();
            std::fs::write(
                fno_dir.join("settings.yaml"),
                format!("project: {project}\n"),
            )
            .unwrap();
            std::fs::create_dir_all(root.join("inbox")).unwrap();
        }
        // Placeholders the case table leaves for runtime values: the parent
        // thread's minted id (seeded only by the cases that carry it), and
        // the body-cap case's over-cap body.
        let parent = if case.parent_thread {
            Some(seed_project_thread(&root))
        } else {
            None
        };
        let mut argv: Vec<String> = case
            .argv
            .iter()
            .map(|a| {
                a.replace("<PARENT_MSG>", parent.as_deref().unwrap_or("<PARENT_MSG>"))
                    .replace("<CAPBODY>", &"x".repeat(5100))
            })
            .collect();
        if case.wipe_bus {
            std::fs::write(root.join("bus").join("messages.jsonl"), "").unwrap();
        }
        if case.json_out {
            argv.push("--json".to_string());
        }
        let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
        let leg = run_case(&root, case.style_on, false, case.inbox_project, &refs);
        // Characterization: the frozen bytes ARE the contract; the oracle
        // slot stays None (the capture driver owns the lineage).
        assert_golden("mail_reply", case.label, &leg.golden(&root), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
