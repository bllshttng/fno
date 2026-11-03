//! parity-stage: characterization
//! parity-oracle: fno.bus.log.iter_messages
//!
//! The mail conformance suite: what any implementation of the mail bus must
//! do, written against the public `mail::` API and the bus-read door. The
//! contract lives in `src/mail/SPEC.md`; each test names the section it pins.
//!
//! The door cases (`door_reads_match_goldens`, the two `stage_mints_*` tests)
//! are the characterization the Python read leg left behind: its cursor walk,
//! deliverability filter, withdrawal and hosted-twin supersession, and one
//! row per id. Sanctioned divergences, uncharacterized by the goldens: the
//! oracle's `str()` coercion of odd field types (a numeric ts, a null thread)
//! has no golden, so the door answers with the row as parsed; a row whose
//! required id/from/to/kind is present but non-string skips as malformed. The
//! door sorts the sets answer for a stable contract where the oracle returned
//! a set; consumers test membership, not order.
//!
//! Every door case runs the client binary on a fresh temp root. The fixture
//! ids, bodies and timestamps are fixed strings, so the output carries no
//! clock or path. The door runs with no --bus, so FNO_BUS_DIR rules the path.

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
        .args(["chats", "bus-read"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents chats bus-read");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Write a bus log and optional cursor into the isolated root.
fn seed_bus(root: &Path, log: &str, cursor: Option<(&str, &str)>) {
    let bus = root.join("bus");
    std::fs::create_dir_all(&bus).unwrap();
    std::fs::write(bus.join("messages.jsonl"), log).unwrap();
    if let Some((name, body)) = cursor {
        std::fs::create_dir_all(bus.join("cursors")).unwrap();
        std::fs::write(bus.join("cursors").join(format!("{name}.json")), body).unwrap();
    }
}

/// Drop the first-run setup noise on stderr only, the same filter the
/// mail_unread harness runs; the door's own warnings stay.
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

const DROP_PREFIXES: [&str; 5] = [
    "[setup] path migration complete",
    "  detected install:",
    "  settings written to:",
    "  obsidian enabled:",
    "  sentinel:",
];

/// The pair fixture: a cursor pair, a second-sender row, a withdrawal over
/// its target, a staged twin and its hosted receipt, and a typed row.
const PAIRS_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"before cursor","ts":"2026-10-12T09:00:00Z"}
{"id":"m2","thread":"t1","from":"lead","to":"worker","kind":"send","body":"at cursor","ts":"2026-10-12T09:05:00Z"}
{"id":"m3","thread":"t1","from":"alpha","to":"worker","kind":"send","body":"from another sender","ts":"2026-10-12T09:06:00Z"}
{"id":"m4","thread":"t1","from":"lead","to":"worker","kind":"withdraw","body":"","ts":"2026-10-12T09:07:00Z","meta":{"withdraws":"m5"}}
{"id":"m5","thread":"t1","from":"lead","to":"worker","kind":"send","body":"retracted","ts":"2026-10-12T09:08:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","body":"staged twin","ts":"2026-10-12T09:09:00Z"}
{"id":"m6","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"hosted","body":"hosted receipt","ts":"2026-10-12T09:10:00Z"}
{"id":"m7","thread":"t1","from":"lead","to":"worker","kind":"send","delivery":"typed","body":"typed delivery","ts":"2026-10-12T09:11:00Z"}
"#;

/// The second-sender fixture: one row from a named peer and one carrying a
/// from_session, the two legs the sender exclusion reads.
const SENDERS_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"first","ts":"2026-10-12T09:00:00Z"}
{"id":"m3","thread":"t1","from":"alpha","to":"worker","kind":"send","body":"from another sender","ts":"2026-10-12T09:06:00Z"}
{"id":"m10","thread":"t1","from":"alpha","to":"worker","kind":"send","from_session":"ses_j","body":"session sender","ts":"2026-10-12T09:16:00Z"}
"#;

/// The control-pair fixture: a landed row and a withdrawal over its target.
const CONTROL_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"first","ts":"2026-10-12T09:00:00Z"}
{"id":"m4","thread":"t1","from":"lead","to":"worker","kind":"withdraw","body":"","ts":"2026-10-12T09:07:00Z","meta":{"withdraws":"m5"}}
{"id":"m5","thread":"t1","from":"lead","to":"worker","kind":"send","body":"retracted","ts":"2026-10-12T09:08:00Z"}
{"id":"l1","thread":"l1","from":"lead","to":"worker","kind":"landed","body":"","ts":"2026-10-12T09:17:00Z","meta":{"landed":"m1"}}
"#;

/// The malformed fixtures: one row missing a required field, and one line
/// that is not JSON at all.
const MISSING_KIND_LOG: &str = r#"{"id":"m1","thread":"t1","from":"lead","to":"worker","kind":"send","body":"first","ts":"2026-10-12T09:00:00Z"}
{"id":"b1","thread":"t1","from":"lead","to":"worker","body":"no kind"}
{"id":"m2","thread":"t1","from":"lead","to":"worker","kind":"send","body":"second","ts":"2026-10-12T09:05:00Z"}
"#;

const BAD_JSON_LOG: &str = r#"{"id":"b1","thread":"t1", invalid
"#;

struct Case {
    label: &'static str,
    log: &'static str,
    cursor: Option<(&'static str, &'static str)>,
    args: &'static [&'static str],
}

const CURSOR_M2: &str = "{\"last_seen_id\": \"m2\", \"ts\": \"2026-10-12T09:14:00Z\"}\n";
const CURSOR_ROTATED: &str = "{\"last_seen_id\": \"mGONE\", \"ts\": \"2026-10-12T09:15:00Z\"}\n";

const CASES: &[Case] = &[
    Case {
        label: "rows reads the retained rows oldest first",
        log: PAIRS_LOG,
        cursor: None,
        args: &["rows"],
    },
    Case {
        label: "rows dedupes last over a receipt twin",
        log: PAIRS_LOG,
        cursor: None,
        args: &["rows", "--dedupe", "last"],
    },
    Case {
        label: "rows skips a malformed row with its position",
        log: MISSING_KIND_LOG,
        cursor: None,
        args: &["rows"],
    },
    Case {
        label: "rows warns on a line that is not JSON",
        log: BAD_JSON_LOG,
        cursor: None,
        args: &["rows"],
    },
    Case {
        label: "unread lists the deliverable rows after the cursor",
        log: PAIRS_LOG,
        cursor: Some(("worker", CURSOR_M2)),
        args: &["unread", "--name", "worker"],
    },
    Case {
        label: "unread rescans when the cursor rotated out",
        log: PAIRS_LOG,
        cursor: Some(("worker", CURSOR_ROTATED)),
        args: &["unread", "--name", "worker"],
    },
    Case {
        label: "unread warns on a corrupt cursor and rescans",
        log: PAIRS_LOG,
        cursor: Some(("worker", "{ invalid")),
        args: &["unread", "--name", "worker"],
    },
    Case {
        label: "unread excludes senders by name and session",
        log: SENDERS_LOG,
        cursor: None,
        args: &[
            "unread",
            "--name",
            "worker",
            "--name",
            "proj",
            "--exclude",
            "alpha,ses_j",
        ],
    },
    Case {
        label: "sets reads the control pairs",
        log: CONTROL_LOG,
        cursor: None,
        args: &["sets"],
    },
    Case {
        label: "cursor answers the last seen id",
        log: PAIRS_LOG,
        cursor: Some(("worker", CURSOR_M2)),
        args: &["cursor", "--name", "worker"],
    },
    Case {
        label: "cursor corrupt warns and rescans",
        log: PAIRS_LOG,
        cursor: Some(("worker", "{ invalid")),
        args: &["cursor", "--name", "worker"],
    },
];

#[test]
fn door_reads_match_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed_bus(root, case.log, case.cursor);
        let (exit, stdout, stderr) = run_client(root, case.args);
        let golden = Golden {
            exit: Some(exit),
            streams: vec![stdout, filter_stderr(&stderr)],
        };
        assert_golden("mail_conformance", case.label, &golden, None);
    }
}

/// `chats stage --delivery hosted` mints the receipt row the hosted lane
/// wrote: the shared id, the delivery tag in its slot, the to_harness
/// envelope field the audit render reads, and the to_session/to_harness
/// meta copy the landed check reads.
#[test]
fn stage_mints_the_hosted_receipt() {
    use std::io::Write as _;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_bus(root, "", None);
    let fields = r#"{"msg_id":"r1","sender":"lead","recipient":"worker","body":"b","to_session":"ses_1","to_harness":"claude","to_kind":"name","word_count":1,"subject":"S"}"#;
    let out = Command::new(binary_path())
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_BUS_DIR", root.join("bus"))
        .args(["chats", "stage", "--delivery", "hosted"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            // Close stdin before the wait: the door reads it to EOF.
            let mut stdin = c.stdin.take().unwrap();
            stdin.write_all(fields.as_bytes())?;
            drop(stdin);
            c.wait_with_output()
        })
        .expect("run fno-agents chats stage");
    assert_eq!(out.status.code(), Some(0));
    let rows = read_rows(root);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["id"], "r1");
    assert_eq!(row["thread"], "r1");
    assert_eq!(row["kind"], "send");
    assert_eq!(row["delivery"], "hosted");
    assert_eq!(row["meta"]["to_session"], "ses_1");
    assert_eq!(row["meta"]["to_harness"], "claude");
    assert_eq!(row["word_count"], 1);
    let keys: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec![
            "v",
            "id",
            "ts",
            "thread",
            "from",
            "to",
            "kind",
            "to_harness",
            "delivery",
            "to_kind",
            "word_count",
            "subject",
            "meta",
            "body"
        ]
    );
}

/// `chats stage --delivery typed` mints the pane receipt: the delivery tag
/// and the transport/pane meta naming the pane the bytes went into.
#[test]
fn stage_mints_the_typed_receipt() {
    use std::io::Write as _;
    use std::process::Stdio;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    seed_bus(root, "", None);
    let fields = r#"{"msg_id":"r2","sender":"lead","recipient":"worker","body":"pane body"}"#;
    let out = Command::new(binary_path())
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_BUS_DIR", root.join("bus"))
        .args([
            "chats",
            "stage",
            "--delivery",
            "typed",
            "--pane-id",
            "pane7",
            "--mux-session",
            "mux9",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut c| {
            // Close stdin before the wait: the door reads it to EOF.
            let mut stdin = c.stdin.take().unwrap();
            stdin.write_all(fields.as_bytes())?;
            drop(stdin);
            c.wait_with_output()
        })
        .expect("run fno-agents chats stage");
    assert_eq!(out.status.code(), Some(0));
    let rows = read_rows(root);
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row["delivery"], "typed");
    assert_eq!(row["meta"]["transport"], "pane");
    assert_eq!(row["meta"]["pane_id"], "pane7");
    assert_eq!(row["meta"]["mux_session"], "mux9");
    let keys: Vec<&str> = row
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        vec!["v", "id", "ts", "thread", "from", "to", "kind", "delivery", "meta", "body"]
    );
}

/// Read the seeded bus file back as parsed rows.
fn read_rows(root: &Path) -> Vec<serde_json::Value> {
    let text = std::fs::read_to_string(root.join("bus").join("messages.jsonl")).unwrap();
    text.lines()
        .map(serde_json::from_str)
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

// ---------------------------------------------------------------------------
// The public mail:: API (SPEC.md sections: envelope, threads, addresses,
// states, log, lanes)
// ---------------------------------------------------------------------------

use fno_agents::mail::address::Address;
use fno_agents::mail::envelope::{is_readable, tag, thread_root, Envelope, ENVELOPE_VERSION};
use fno_agents::mail::lane::{LaneKind, Target, LADDER};
use fno_agents::mail::log::{read_bus_segments_reported, resolve_log_path};
use fno_agents::mail::state::{newest_state, DeliveryState};
use serde_json::json;

/// Envelope: field order is byte order, an empty tag is absent, `v` leads.
#[test]
fn envelope_bytes_follow_the_field_order() {
    let mut env = Envelope::new(
        "fmail-aaaaaaaaaaaa",
        "2026-11-03T10:00:00Z",
        "fmail-aaaaaaaaaaaa",
        "lead",
        "worker",
        "send",
        "hello",
    );
    env.from_harness = tag(Some("claude"));
    env.in_reply_to = tag(Some(""));
    env.subject = tag(Some("S"));
    env.word_count = Some(1);
    env.meta.insert("owner".into(), json!("lead"));
    assert_eq!(ENVELOPE_VERSION, 1);
    assert_eq!(
        serde_json::to_string(&env.into_value()).unwrap(),
        r#"{"v":1,"id":"fmail-aaaaaaaaaaaa","ts":"2026-11-03T10:00:00Z","thread":"fmail-aaaaaaaaaaaa","from":"lead","to":"worker","kind":"send","from_harness":"claude","subject":"S","word_count":1,"meta":{"owner":"lead"},"body":"hello"}"#
    );
}

/// Envelope and log: a row with no `v` reads as 1; a newer `v` and a
/// malformed line each skip with a warning that names the segment and line.
#[test]
fn the_reader_skips_what_it_cannot_read_and_names_where() {
    assert!(is_readable(&json!({"id": "a"})));
    assert!(is_readable(&json!({"v": 1})));
    assert!(!is_readable(&json!({"v": 2})));
    let dir = tempfile::tempdir().unwrap();
    let live = dir.path().join("messages.jsonl");
    std::fs::write(
        &live,
        "{\"id\":\"a\",\"v\":1}\nnot json\n{\"id\":\"b\",\"v\":2}\n{\"id\":\"c\"}\n",
    )
    .unwrap();
    let (rows, warnings) = read_bus_segments_reported(&live);
    let ids: Vec<&str> = rows.iter().map(|r| r.row["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["a", "c"]);
    assert_eq!(rows[1].line, 4);
    assert_eq!(
        warnings,
        [
            "bus log: skipping malformed line messages.jsonl:2 (JSONDecodeError)",
            "bus log: skipping row messages.jsonl:3 (v=2 is newer than supported v=1)",
        ]
    );
    // The ladder: FNO_BUS_DIR, then FNO_INBOX_ROOT/.bus, then config, then the root.
    let root = std::path::Path::new("/r");
    let cfg = std::path::Path::new("/cfg");
    assert_eq!(
        resolve_log_path(root, Some("/e"), Some("/i"), Some(cfg)),
        std::path::PathBuf::from("/e/messages.jsonl")
    );
    assert_eq!(
        resolve_log_path(root, Some(""), Some("/i"), Some(cfg)),
        std::path::PathBuf::from("/i/.bus/messages.jsonl")
    );
    assert_eq!(
        resolve_log_path(root, None, None, Some(cfg)),
        std::path::PathBuf::from("/cfg/messages.jsonl")
    );
    assert_eq!(
        resolve_log_path(root, None, None, None),
        std::path::PathBuf::from("/r/bus/messages.jsonl")
    );
}

/// Threads: the root is one level. A reply to a reply carries the same root.
#[test]
fn a_reply_to_a_reply_keeps_the_first_root() {
    let a = "fmail-aaaaaaaaaaaa";
    let b_thread = thread_root(None, a);
    assert_eq!(b_thread, a);
    let c_thread = thread_root(Some(&b_thread), "fmail-bbbbbbbbbbbb");
    assert_eq!(c_thread, a);
}

/// Addresses: a handle, `name@host` with the host normalized, and two `@` refuse.
#[test]
fn address_forms_parse_to_the_table() {
    assert_eq!(
        Address::parse_target("quill").unwrap(),
        Address::Handle("quill".into())
    );
    assert_eq!(
        Address::parse_target("quill@iMac2D2.local").unwrap(),
        Address::Remote {
            name: "quill".into(),
            host: "imac2d2".into()
        }
    );
    let err = Address::parse_target("a@b@c").unwrap_err();
    assert!(err.contains("the form is <name>@<host>, one @"), "{err}");
}

/// States: an acceptance is never lowered; with no state row a legacy
/// `delivery` field and a `landed` row each imply one.
#[test]
fn the_state_fold_never_lowers_an_acceptance() {
    let msg = json!({"id": "m1", "kind": "send", "delivery": "typed"});
    assert_eq!(newest_state(&[msg.clone()], "m1"), DeliveryState::Injected);
    let state = |s: &DeliveryState| {
        let mut meta = s.to_json();
        meta["of"] = json!("m1");
        json!({"id": "s", "kind": "state", "meta": meta})
    };
    let accepted = DeliveryState::accepted(false, Default::default());
    let rows = vec![
        msg,
        state(&accepted),
        state(&DeliveryState::failed("io-error")),
    ];
    assert_eq!(newest_state(&rows, "m1"), accepted);
    assert_eq!(
        newest_state(&[json!({"id": "m2", "kind": "send"})], "m2"),
        DeliveryState::Queued
    );
}

/// Lanes: the first lane in the ladder that serves the target carries the send.
#[test]
fn the_lane_ladder_picks_one_lane_in_order() {
    let pick = |t: &Target| LADDER.iter().copied().find(|k| k.applies(t));
    let base = Target {
        harness: "claude".into(),
        session_id: Some("s1".into()),
        ..Target::default()
    };
    assert_eq!(pick(&base), Some(LaneKind::ClaudeSwitchboard));
    let pane = Target {
        has_mux: true,
        codex_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&pane), Some(LaneKind::Pane));
    let codex = Target {
        harness: "codex".into(),
        codex_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&codex), Some(LaneKind::CodexThread));
    let keeper = Target {
        keeper_thread: true,
        ..base.clone()
    };
    assert_eq!(pick(&keeper), Some(LaneKind::Keeper));
    // A keeper row with no session id falls through to the harness lanes.
    let keeper_no_session = Target {
        session_id: None,
        ..keeper
    };
    assert_eq!(pick(&keeper_no_session), Some(LaneKind::ClaudeSwitchboard));
    let acp = Target {
        harness: "gemini".into(),
        ..base
    };
    assert_eq!(pick(&acp), Some(LaneKind::DaemonDeliver));
}
