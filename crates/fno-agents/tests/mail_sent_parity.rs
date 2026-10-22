//! parity-stage: characterization
//! parity-oracle: cli/src/fno/mail/cli.py:cmd_sent
//!
//! The sent view was frozen from the Python front while it still ran its own
//! body: each case seeded a hermetic root (bus log, cursors, recipient
//! transcripts, identity markers) and invoked the command in a non-tty
//! subprocess. Those frozen bytes are the contract; the Python command now
//! forwards here, so this test pins the port to the answers the oracle gave.
//!
//! The matrix: the full-view filter (kind == send stamped with this
//! session's handle, retracted ids dropped), the unclaimed probe at TTL -1
//! so a just-sent row never reports picked up, the claimed tri-state
//! (cursor-claimed, typed false, hosted null), the landed ladder (bus
//! record, transcript hit, transcript miss), the `--from-name` override,
//! the "fno" identity floor, the `-J` shorthand, and dedupe with last-wins.
//!
//! Divergences from the hold harness this mirrors: sent prints a top-level
//! JSON array, so the sorted-compact re-dump accepts arrays too (element
//! order preserved, exactly as the capture normalized); timestamps scrub
//! via full ISO instants rather than bare clock times; fixtures carry
//! runtime-relative ages (7200s / 60s / 0s) instead of frozen stamps; and
//! the transcript base is pinned with FNO_CLAUDE_PROJECTS_DIR.

mod common;

use common::{assert_golden, Golden};
use serde_json::{json, Map, Value};
use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

const SID: &str = "abcd1234efgh5678";
const H: &str = "abcd1234";
/// Older than the default unclaimed TTL (1800), younger than the abandon
/// window (21600): unclaimed but not yet stale.
const OLD: i64 = 7200;
const FRESH: i64 = 60;

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

fn iso_ago(seconds_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::seconds(seconds_ago))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// The capture's env block: hermetic state roots, the identity markers
/// popped so only the case's own marker can stamp a handle, and the landed
/// settle knob removed so the transcript read runs its default budget.
fn sent_env<'a>(cmd: &'a mut Command, root: &Path, ident: bool) -> &'a mut Command {
    cmd.envs(fno_agents::test_run::self_owner_env())
        .env("HOME", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_HOME", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_BUS_DIR", root.join("bus"))
        .env("FNO_CLAUDE_PROJECTS_DIR", root.join(".claude/projects"))
        .env("FNO_AGENTS_BIN", binary_path())
        .env_remove("FNO_AGENT_SELF")
        .env_remove("FNO_MAIL_LANDED_SETTLE_S")
        .env_remove("FNO_HARNESS_NAME")
        .env_remove("FNO_HARNESS_SESSION_ID")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CLAUDE_SESSION_ID")
        .env_remove("CODEX_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        .env_remove("GEMINI_SESSION_ID")
        .env_remove("OPENCODE_SESSION_ID")
        .env_remove("AGY_SESSION_ID");
    if ident {
        cmd.env("CLAUDE_CODE_SESSION_ID", SID);
    }
    cmd
}

fn run_client(root: &Path, ident: bool, args: &[&str]) -> Leg {
    let mut cmd = Command::new(binary_path());
    let out = sent_env(&mut cmd, root, ident)
        .args(["mail"])
        .args(args)
        .current_dir(root)
        .output()
        .expect("run fno-agents mail sent");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// Keys sorted, array element order preserved, scalars untouched: the exact
/// re-dump the capture applied before freezing.
fn sorted_compact(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let mut out = Map::new();
            for k in keys {
                out.insert(k.clone(), sorted_compact(&m[k]));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted_compact).collect()),
        other => other.clone(),
    }
}

fn normalize(text: &str, root: &Path) -> String {
    const DROP_PREFIXES: &[&str] = &[
        "  detected install:",
        "  settings written to:",
        "  obsidian enabled:",
        "  sentinel:",
        "[setup] path migration complete",
    ];
    let kept: Vec<&str> = text
        .split('\n')
        .filter(|ln| !DROP_PREFIXES.iter().any(|p| ln.starts_with(p)))
        .collect();
    let mut s = kept.join("\n");
    while s.ends_with('\n') {
        s.pop();
    }
    if let Ok(v) = serde_json::from_str::<Value>(&s) {
        if matches!(v, Value::Object(_) | Value::Array(_)) {
            s = sorted_compact(&v).to_string();
        }
    }
    static ISO_RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = ISO_RE.get_or_init(|| {
        regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?(Z|[+-]\d{2}:?\d{2})?")
            .unwrap()
    });
    let s = re.replace_all(&s, "TS").into_owned();
    let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    s.replace(canon.to_string_lossy().as_ref(), "ROOT")
        .replace(root.to_string_lossy().as_ref(), "ROOT")
}

/// One bus envelope in the compact shape the send path stamps. Optional
/// delivery and meta stay absent unless given, mirroring the capture's
/// builder field-for-field.
fn env_row(
    eid: &str,
    to: &str,
    age_s: i64,
    from_: &str,
    delivery: Option<&str>,
    meta: &[(&str, &str)],
    body: &str,
    kind: &str,
) -> String {
    let mut row = json!({
        "v": 1,
        "id": eid,
        "ts": iso_ago(age_s),
        "thread": format!("th-{eid}"),
        "from": from_,
        "to": to,
        "kind": kind,
    });
    if let Some(d) = delivery {
        row["delivery"] = json!(d);
    }
    if !meta.is_empty() {
        let mut m = Map::new();
        for (k, v) in meta {
            m.insert((*k).to_string(), json!(v));
        }
        row["meta"] = Value::Object(m);
    }
    row["body"] = json!(body);
    row.to_string()
}

fn write_bus(root: &Path, lines: &[String]) {
    let dir = root.join("bus");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("messages.jsonl"), lines.join("\n") + "\n").unwrap();
}

fn write_cursor(root: &Path, name: &str, seen: &str) {
    let dir = root.join("bus/cursors");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{name}.json")),
        json!({"last_seen_id": seen, "ts": iso_ago(0)}).to_string(),
    )
    .unwrap();
}

fn write_transcript(root: &Path, sid: &str, text: &str) {
    let dir = root.join(".claude/projects/proj-a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{sid}.jsonl")),
        json!({"text": text}).to_string() + "\n",
    )
    .unwrap();
}

fn seed(root: &Path, label: &str) {
    match label {
        "empty-bus" | "json-shorthand" => {}
        "durable-unclaimed-old" => write_bus(
            root,
            &[env_row(
                "m-old1",
                "worker1",
                OLD,
                H,
                None,
                &[],
                "hello",
                "send",
            )],
        ),
        "claimed-by-cursor" => {
            write_bus(
                root,
                &[
                    env_row("m-old2", "worker2", OLD, H, None, &[], "hello", "send"),
                    env_row("m-old3", "worker2", OLD, H, None, &[], "hello", "send"),
                ],
            );
            write_cursor(root, "worker2", "m-old2");
        }
        "hosted-unknown" => write_bus(
            root,
            &[env_row(
                "m-h1",
                "worker3",
                OLD,
                H,
                Some("hosted"),
                &[],
                "hello",
                "send",
            )],
        ),
        "typed-delivery" => write_bus(
            root,
            &[env_row(
                "m-t1",
                "worker4",
                OLD,
                H,
                Some("typed"),
                &[],
                "hello",
                "send",
            )],
        ),
        "withdrawn-send" => write_bus(
            root,
            &[
                env_row("m-w1", "worker5", OLD, H, None, &[], "hello", "send"),
                env_row(
                    "m-w2",
                    "worker5",
                    OLD,
                    H,
                    None,
                    &[("withdraws", "m-w1")],
                    "",
                    "withdraw",
                ),
            ],
        ),
        "landed-record" => write_bus(
            root,
            &[
                env_row("m-l1", "worker6", OLD, H, None, &[], "hello", "send"),
                env_row(
                    "m-l2",
                    "worker6",
                    OLD,
                    H,
                    None,
                    &[("landed", "m-l1")],
                    "",
                    "landed",
                ),
            ],
        ),
        "landed-transcript" => {
            write_bus(
                root,
                &[env_row(
                    "m-tx",
                    "worker7",
                    OLD,
                    H,
                    None,
                    &[("to_harness", "claude"), ("to_session", "sess-tx7")],
                    "<fno_mail id=\"m-tx\">",
                    "send",
                )],
            );
            write_transcript(root, "sess-tx7", "<fno_mail id=\"m-tx\">");
        }
        "landed-transcript-miss" => {
            write_bus(
                root,
                &[env_row(
                    "m-ty",
                    "worker8",
                    OLD,
                    H,
                    None,
                    &[("to_harness", "claude"), ("to_session", "sess-ty8")],
                    "<fno_mail id=\"m-ty\">",
                    "send",
                )],
            );
            write_transcript(root, "sess-ty8", "nothing relevant here");
        }
        "from-name-override" => write_bus(
            root,
            &[env_row(
                "m-f1",
                "worker9",
                OLD,
                "labeler",
                None,
                &[],
                "hello",
                "send",
            )],
        ),
        "unclaimed-scope" => {
            write_bus(
                root,
                &[
                    env_row("m-u1", "w10", OLD, H, None, &[], "hello", "send"),
                    env_row("m-u2", "w11", FRESH, H, None, &[], "hello", "send"),
                    env_row("m-u3", "w12", OLD, H, None, &[], "hello", "send"),
                ],
            );
            write_cursor(root, "w12", "m-u3");
        }
        "no-identity-floor" => write_bus(
            root,
            &[
                env_row("m-g1", "w13", OLD, "fno", None, &[], "hello", "send"),
                env_row("m-g2", "w14", OLD, "abcd1234", None, &[], "hello", "send"),
            ],
        ),
        "mixed-ordering" => write_bus(
            root,
            &[
                env_row("m-n1", "w15", OLD, H, None, &[], "nudge", "notify"),
                env_row("m-x1", "w15", OLD, H, None, &[], "hello", "send"),
                env_row("m-x2", "w16", OLD, "otheragent", None, &[], "hello", "send"),
                env_row("m-x3", "w17a", OLD, H, None, &[], "hello", "send"),
                env_row("m-x3", "w17b", OLD, H, None, &[], "twin", "send"),
            ],
        ),
        // Age 0 pins the TTL -1 invariant: with the strict `>` age test even
        // a this-second send stays unclaimed in the full view.
        "just-sent" => write_bus(
            root,
            &[env_row("m-js", "w-js", 0, H, None, &[], "hello", "send")],
        ),
        other => panic!("no fixture for case {other}"),
    }
}

struct Case {
    label: &'static str,
    args: &'static [&'static str],
    /// true seeds CLAUDE_CODE_SESSION_ID so the ambient handle is
    /// abcd1234; false removes every identity marker so the "fno" floor
    /// applies.
    ident: bool,
}

const CASES: &[Case] = &[
    Case {
        label: "empty-bus",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "durable-unclaimed-old",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "claimed-by-cursor",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "hosted-unknown",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "typed-delivery",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "withdrawn-send",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "landed-record",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "landed-transcript",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "landed-transcript-miss",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "from-name-override",
        args: &["sent", "--json", "--from-name", "labeler"],
        ident: true,
    },
    Case {
        label: "unclaimed-scope",
        args: &["sent", "--json", "--unclaimed"],
        ident: true,
    },
    Case {
        label: "no-identity-floor",
        args: &["sent", "--json"],
        ident: false,
    },
    Case {
        label: "json-shorthand",
        args: &["sent", "-J"],
        ident: true,
    },
    Case {
        label: "mixed-ordering",
        args: &["sent", "--json"],
        ident: true,
    },
    Case {
        label: "just-sent",
        args: &["sent", "--json"],
        ident: true,
    },
];

#[test]
fn sent_front_matches_python_goldens() {
    for case in CASES {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        seed(root, case.label);
        let leg = run_client(root, case.ident, case.args);
        assert_golden("mail_sent", case.label, &leg.normalized_golden(root), None);
    }
}
