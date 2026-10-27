//! parity-stage: characterization
//! parity-oracle: fno.bus.log.iter_messages
//!
//! Characterization harness for the bus log's native read door
//! (`chats bus-read`): the Python read leg (bus/log.py's iter_messages, the
//! cursor scan in bus/cursor.py) is deleted in the same change, and these
//! goldens freeze its contract. The scan semantics (cursor walk, the
//! deliverability filter, the withdrawal and hosted-twin supersession, one
//! row per id) were already native in mail_control_drain::unread_messages;
//! this door exposes them, and the rows/sets/cursor modes cover
//! iter_messages, the id sets and the cursor read.
//!
//! Sanctioned divergences, uncharacterized by the goldens: the oracle's
//! `str()` coercion of odd field types (a numeric ts, a null thread) has no
//! golden, so the door answers with the row as parsed; a row whose required
//! id/from/to/kind is present but non-string skips as malformed. The door
//! sorts the sets answer for a stable contract where the oracle returned a
//! set; consumers test membership, not order. The door's warnings print to
//! its own stderr, which the Python door transport captures and surfaces
//! only on failure, where the oracle printed them to the caller's terminal;
//! the binary's direct surfaces still see them.
//!
//! Every case runs the client binary on a fresh temp root. The fixture ids,
//! bodies and timestamps are fixed strings, so the output carries no clock
//! or path. The door runs with no --bus, so FNO_BUS_DIR rules the path
//! exactly as it rules the Python tests.

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
fn bus_read_matches_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed_bus(root, case.log, case.cursor);
        let (exit, stdout, stderr) = run_client(root, case.args);
        let golden = Golden {
            exit: Some(exit),
            streams: vec![stdout, filter_stderr(&stderr)],
        };
        assert_golden("bus_read", case.label, &golden, None);
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
