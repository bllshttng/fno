//! parity-stage: characterization
//! parity-oracle: fno.mail.cli.cmd_drain_self
//!
//! The drain-self leaf is a finished port: the python command body is gone and
//! the python surface forwards to this rust front. These tests hold the rust
//! behavior against goldens captured from the python leg BEFORE the leaf
//! landed, so capture mode is a refusal here: the oracle symbol
//! `fno.mail.cli.cmd_drain_self` is no longer defined, and a golden can only
//! be captured while the old leg runs.
//!
//! Nothing in drain output is volatile, so the compare is byte-exact apart
//! from setup-noise lines the fno-agents binary may print to stderr under a
//! fresh HOME (path migration, settings, sentinel) that the in-process
//! python oracle never printed. Those lines are dropped, never rewritten:
//! the goldens are raw process bytes, so no rstrip, no timestamp scrub, and
//! no JSON re-dump.

mod common;

use common::{assert_golden, Golden};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

const SID: &str = "a1b2c3d4-1111-2222-3333-444455556666";
const HANDLE: &str = "a1b2c3d4";
const LEGACY: &str = "55556666";
const JOB: &str = "node:goldenjob";
const HOLDER: &str = "holder-goldenjob";

struct Leg {
    exit: i32,
    stdout: String,
    stderr: String,
}

impl Leg {
    fn golden(&self) -> Golden {
        Golden {
            exit: Some(self.exit),
            streams: vec![drop_setup(&self.stdout), drop_setup(&self.stderr)],
        }
    }
}

/// Stderr noise the binary prints under a fresh HOME that the in-process
/// python oracle never printed. Single-line prefixes only: a multi-line
/// version of any of these fails the compare loudly instead of being
/// silently eaten.
const DROP_PREFIXES: &[&str] = &[
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
];

fn drop_setup(text: &str) -> String {
    text.lines()
        .filter(|line| !DROP_PREFIXES.iter().any(|p| line.starts_with(p)))
        .map(|line| format!("{line}\n"))
        .collect()
}

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

static CASE: AtomicU32 = AtomicU32::new(0);

fn case_root() -> PathBuf {
    let n = CASE.fetch_add(1, Ordering::SeqCst);
    let dir =
        std::env::temp_dir().join(format!("fno-drain-self-golden-{}-{n}", std::process::id()));
    fs::create_dir_all(&dir).expect("case root created");
    dir
}

/// The env the goldens were captured under: everything state-shaped pinned
/// into the case root, harness identity claude + SID, every other harness
/// marker blanked, and the ambient overrides popped so the transcript store
/// resolves to HOME/.claude/projects.
fn drain_env(cmd: &mut Command, root: &Path) {
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.env("HOME", root);
    cmd.env("FNO_STATE_DIR", root);
    cmd.env("FNO_HOME", root);
    cmd.env("FNO_AGENTS_HOME", root.join("agents-home"));
    cmd.env("FNO_BUS_DIR", root.join("bus"));
    cmd.env("FNO_EVENTS_PATH", root.join("events.jsonl"));
    cmd.env("FNO_CLAIMS_ROOT", root);
    cmd.env("FNO_HARNESS_NAME", "claude");
    cmd.env("FNO_HARNESS_SESSION_ID", SID);
    cmd.env("CLAUDE_CODE_SESSION_ID", SID);
    for (key, val) in [
        ("CODEX_THREAD_ID", ""),
        ("CODEX_SESSION_ID", ""),
        ("GEMINI_SESSION_ID", ""),
        ("OPENCODE_SESSION_ID", ""),
        ("CLAUDE_SESSION_ID", ""),
    ] {
        cmd.env(key, val);
    }
    cmd.env_remove("FNO_AGENT_SELF");
    cmd.env_remove("FNO_CLAUDE_PROJECTS_DIR");
    cmd.env_remove("CLAUDE_CONFIG_DIR");
    cmd.env_remove("PYTHONPATH");
    cmd.current_dir(root);
}

fn run_drain(root: &Path, args: &[&str]) -> Leg {
    let mut cmd = Command::new(binary_path());
    drain_env(&mut cmd, root);
    cmd.args(["mail"]).args(args);
    let out = cmd.output().expect("fno-agents mail drain-self ran");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn row(id: &str, to: &str, ts: &str, body: &str) -> serde_json::Value {
    json!({
        "id": id,
        "thread": "t",
        "from": "lead",
        "to": to,
        "kind": "send",
        "body": body,
        "ts": ts,
    })
}

fn row_with_origin(mut r: serde_json::Value, origin: &str) -> serde_json::Value {
    r["origin"] = json!(origin);
    r
}

fn seed_bus(root: &Path, rows: &[serde_json::Value]) {
    let bus = root.join("bus");
    fs::create_dir_all(&bus).expect("bus dir created");
    let mut text = String::new();
    for r in rows {
        text.push_str(&r.to_string());
        text.push('\n');
    }
    fs::write(bus.join("messages.jsonl"), text).expect("bus seeded");
}

fn seed_manifest(root: &Path) {
    let dot = root.join(".fno");
    fs::create_dir_all(&dot).expect(".fno created");
    fs::write(
        dot.join("target-state.md"),
        format!("session_id: {SID}\ntarget_claim_key: \"{JOB}\"\ntarget_claim_holder: {HOLDER}\n"),
    )
    .expect("manifest seeded");
}

fn seed_claim(root: &Path) {
    // The pid is THIS test process: alive while the drain subprocess runs, so
    // the claim reads Live and the job gate passes. Same shape the python
    // capture seeded (reason, ttl, identity).
    let outcome = fno_agents::claims::acquire(
        JOB,
        HOLDER,
        fno_agents::claims::AcquireOpts {
            pid: Some(std::process::id()),
            ttl_ms: Some(3_600_000),
            reason: Some("golden fixture".into()),
            root: Some(root.to_path_buf()),
            identity: Some((SID.to_string(), "claude".into())),
            ..Default::default()
        },
    );
    assert!(
        matches!(outcome, fno_agents::claims::AcquireOutcome::Acquired(_)),
        "fixture claim not acquired: {outcome:?}"
    );
}

fn seed_transcript(root: &Path, ids: &[&str]) {
    let proj = root.join(".claude").join("projects").join("someproj");
    fs::create_dir_all(&proj).expect("projects dir created");
    let mut text = String::from("fake transcript line one\n");
    for id in ids {
        text.push_str(&format!(
            "<fno_mail id=\"{id}\">already delivered</fno_mail>\n"
        ));
    }
    fs::write(proj.join(format!("{SID}.jsonl")), text).expect("transcript seeded");
}

/// The store cutover made the sqlite store the write boundary on both
/// legs, so the raw JSONL journal is never written. Read the drain
/// receipts back through the store the emitter committed to, resolved
/// from the same journal path (it lands at agents-home/events.db).
fn marker_lines(root: &Path) -> Vec<String> {
    let journal = root.join("agents-home").join("events.jsonl");
    let _ = fno_agents::event_store::import_all(&journal);
    fno_agents::event_store::query_events(
        &journal,
        &fno_agents::event_store::EventQuery {
            types: vec!["agent_mail_drained".to_string()],
            ..Default::default()
        },
    )
    .expect("drain-self marker store read")
    .into_iter()
    .map(|row| row.line)
    .collect()
}

fn seed_unread(root: &Path) {
    seed_bus(
        root,
        &[
            row("m1", HANDLE, "2026-10-12T09:00:00Z", "first unread"),
            row_with_origin(
                row("m2", HANDLE, "2026-10-12T09:05:00Z", "second unread"),
                "pane",
            ),
            row_with_origin(
                row("m3", HANDLE, "2026-10-12T09:07:00Z", "peer sent this"),
                "peer",
            ),
        ],
    );
}

fn seed_held_job(root: &Path) {
    seed_manifest(root);
    seed_claim(root);
    seed_bus(
        root,
        &[
            row("j1", JOB, "2026-10-12T10:00:00Z", "job mail body"),
            row("r1", HANDLE, "2026-10-12T10:01:00Z", "regular mail body"),
        ],
    );
}

#[test]
fn nothing_to_drain() {
    let root = case_root();
    let leg = run_drain(&root, &["drain-self"]);
    assert_golden("mail_drain_self", "nothing_to_drain", &leg.golden(), None);
    assert!(
        marker_lines(&root).is_empty(),
        "an empty bus emitted markers"
    );
}

#[test]
fn nothing_to_drain_json() {
    let root = case_root();
    let leg = run_drain(&root, &["drain-self", "--json"]);
    assert_golden(
        "mail_drain_self",
        "nothing_to_drain_json",
        &leg.golden(),
        None,
    );
    assert!(
        marker_lines(&root).is_empty(),
        "an empty bus emitted markers"
    );
}

#[test]
fn unread_mail() {
    let root = case_root();
    seed_unread(&root);
    let leg = run_drain(&root, &["drain-self"]);
    assert_golden("mail_drain_self", "unread_mail", &leg.golden(), None);
    assert_eq!(
        marker_lines(&root).len(),
        3,
        "one receipt per drained message"
    );
}

#[test]
fn unread_mail_json() {
    let root = case_root();
    seed_unread(&root);
    let leg = run_drain(&root, &["drain-self", "--json"]);
    assert_golden("mail_drain_self", "unread_mail_json", &leg.golden(), None);
    assert_eq!(
        marker_lines(&root).len(),
        3,
        "one receipt per drained message"
    );
}

#[test]
fn all_address_forms() {
    let root = case_root();
    seed_bus(
        &root,
        &[
            row("f1", HANDLE, "2026-10-12T09:00:00Z", "canonical form"),
            row("f2", SID, "2026-10-12T09:01:00Z", "identity key form"),
            row("f3", LEGACY, "2026-10-12T09:02:00Z", "legacy suffix form"),
            row("dupe-id", SID, "2026-10-12T09:03:00Z", "dup first form"),
            row("dupe-id", HANDLE, "2026-10-12T09:04:00Z", "dup twin form"),
        ],
    );
    let leg = run_drain(&root, &["drain-self"]);
    assert_golden("mail_drain_self", "all_address_forms", &leg.golden(), None);
    assert_eq!(marker_lines(&root).len(), 4, "keep-first drops the twin");
}

#[test]
fn held_job_mail() {
    let root = case_root();
    seed_held_job(&root);
    let leg = run_drain(&root, &["drain-self"]);
    assert_golden("mail_drain_self", "held_job_mail", &leg.golden(), None);
    assert_eq!(marker_lines(&root).len(), 2, "job mail plus regular mail");
}

#[test]
fn held_job_mail_json() {
    let root = case_root();
    seed_held_job(&root);
    let leg = run_drain(&root, &["drain-self", "--json"]);
    assert_golden("mail_drain_self", "held_job_mail_json", &leg.golden(), None);
    assert_eq!(marker_lines(&root).len(), 2, "job mail plus regular mail");
}

#[test]
fn duplicates_skipped() {
    let root = case_root();
    seed_transcript(&root, &["d1", "d2"]);
    seed_bus(
        &root,
        &[
            row(
                "d1",
                HANDLE,
                "2026-10-12T11:00:00Z",
                "already delivered one",
            ),
            row(
                "d2",
                HANDLE,
                "2026-10-12T11:01:00Z",
                "already delivered two",
            ),
        ],
    );
    let leg = run_drain(&root, &["drain-self"]);
    assert_golden("mail_drain_self", "duplicates_skipped", &leg.golden(), None);
    let markers = marker_lines(&root);
    assert_eq!(markers.len(), 2, "a skipped duplicate still receipts");
    assert!(
        markers.iter().any(|m| m.contains("skipped-duplicate")),
        "no skipped-duplicate reason in: {markers:?}"
    );
}
