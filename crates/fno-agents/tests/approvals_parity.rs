//! parity-stage: characterization
//! parity-oracle: fno.approvals.cli (ls, show, decide) with fno.approvals.store
//! and fno.approvals.policy, the deleted Python leg; goldens captured from it
//! over the fixture below before deletion
//!
//! Characterization tests for `compaction approvals`, the native answer behind
//! `fno inbox approvals`. The contract is the Python commands' human and JSON
//! output, the stable refusal shape, and the exit code: 0 on success, 2 on any
//! refusal or failure. Timestamps the run mints normalize to `<NOW>`.

use common::{assert_golden, Golden};
use fno_agents::effect_gate::EffectRequest;
use rusqlite::{params, Connection};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

const CREATED: &str = "2025-01-01T00:00:00+00:00";
const FUTURE: &str = "2098-12-31T23:59:59+00:00";
const PAST: &str = "2020-01-01T00:00:00+00:00";
const OLD: &str = "2019-06-01T00:00:00+00:00";

/// (key, n, effect class, destination, created_at, expires_at)
const ROWS: &[(&str, u32, &str, &str, &str, &str)] = &[
    (
        "a",
        1,
        "communication.external",
        "mail:list-a",
        CREATED,
        FUTURE,
    ),
    (
        "b",
        2,
        "repo.write",
        "github:org/repo",
        "2025-01-02T00:00:00+00:00",
        FUTURE,
    ),
    (
        "c",
        3,
        "communication.external",
        "mail:list-c",
        "2025-01-03T00:00:00+00:00",
        FUTURE,
    ),
    ("d", 4, "repo.write", "github:org/old", OLD, PAST),
    (
        "e",
        5,
        "communication.external",
        "mail:list-e",
        "2025-01-05T00:00:00+00:00",
        FUTURE,
    ),
    (
        "g",
        6,
        "repo.write",
        "github:org/new",
        "2025-01-06T00:00:00+00:00",
        FUTURE,
    ),
];

fn request(n: u32, class: &str, dest: &str, created: &str, expires: &str) -> EffectRequest {
    EffectRequest {
        request_id: format!("req-{n}"),
        principal_id: "agent-1".into(),
        work_order_id: format!("wo-{n}"),
        attempt_id: format!("att-{n}"),
        effect_id: format!("eff-{n}"),
        effect_class: class.into(),
        destination: dest.into(),
        action_digest: format!("act-{n}"),
        created_at: created.into(),
        expires_at: expires.into(),
    }
}

fn digest_of(key: &str) -> String {
    if key == "f" {
        return "f".repeat(64);
    }
    let (_, n, class, dest, created, expires) = ROWS.iter().find(|r| r.0 == key).unwrap();
    request(*n, class, dest, created, expires).request_digest()
}

#[derive(Clone, Copy, PartialEq)]
enum Policy {
    Founder,
    ClassOnly,
    None,
}

fn build(root: &Path, populated: bool, policy: Policy) -> PathBuf {
    fs::create_dir_all(root.join(".fno")).unwrap();
    match policy {
        Policy::Founder => fs::write(
            root.join(".fno/config.toml"),
            "[approvals.authorized_principals]\n\"*\" = [\"founder\"]\n\"communication.external\" = [\"intern\"]\n",
        )
        .unwrap(),
        Policy::ClassOnly => fs::write(
            root.join(".fno/config.toml"),
            "[approvals.authorized_principals]\n\"communication.external\" = [\"intern\"]\n",
        )
        .unwrap(),
        Policy::None => {}
    }
    let db = root.join("approvals.db");
    let conn = fno_agents::effect_gate::open_db(&db).unwrap();
    if populated {
        for (_, n, class, dest, created, expires) in ROWS {
            let req = request(*n, class, dest, created, expires);
            conn.execute(
                "INSERT INTO requests VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                params![
                    req.request_digest(),
                    req.request_id,
                    req.principal_id,
                    req.work_order_id,
                    req.attempt_id,
                    req.effect_id,
                    req.effect_class,
                    req.destination,
                    req.action_digest,
                    req.created_at,
                    req.expires_at,
                ],
            )
            .unwrap();
        }
        let decide = |key: &str, who: &str, what: &str, at: &str, transport: Option<&str>| {
            conn.execute(
                "INSERT INTO decisions VALUES (?,?,?,?,?)",
                params![digest_of(key), who, what, at, transport],
            )
            .unwrap();
        };
        decide(
            "b",
            "founder",
            "approved",
            "2025-02-01T10:00:00+00:00",
            Some("slack"),
        );
        decide(
            "c",
            "founder",
            "declined",
            "2025-02-02T11:30:00.250000+00:00",
            None,
        );
        decide(
            "e",
            "founder",
            "approved",
            "2025-02-03T09:00:00+00:00",
            None,
        );
        for (key, state, external, recon) in [
            ("key-a", "acknowledged", Some("msg-9"), None),
            ("key-b", "unknown", None, Some("recon-1")),
        ] {
            conn.execute(
                "INSERT INTO attempts VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,1,'tok',0)",
                params![
                    key,
                    "eff-5",
                    "wo-5",
                    "att-5",
                    digest_of("e"),
                    "act-5",
                    "mail:list-e",
                    "communication.external",
                    "mail-adapter",
                    "1.2.0",
                    state,
                    external,
                    recon,
                ],
            )
            .unwrap();
        }
    }
    drop::<Connection>(conn);
    db
}

/// Fold the run's own paths and clock readings into stable tokens: fixed
/// 2025 stamps stay, anything else shaped like a UTC stamp reads `<NOW>`.
fn norm(text: &str, root: &Path) -> String {
    let mut text = text.replace(&root.display().to_string(), "<FIXTURE>");
    text = text.replace("/private<FIXTURE>", "<FIXTURE>");
    for (needle, token) in [
        (CREATED, "<CREATED>"),
        (FUTURE, "<FUTURE>"),
        (PAST, "<PAST>"),
        (OLD, "<OLD>"),
    ] {
        text = text.replace(needle, token);
    }
    scrub_now(&text)
}

fn scrub_now(text: &str) -> String {
    let bytes = text.as_bytes();
    let shape = b"dddd-dd-ddTdd:dd:dd";
    let is_stamp_at = |i: usize| -> Option<usize> {
        if i + shape.len() > bytes.len() {
            return None;
        }
        for (k, want) in shape.iter().enumerate() {
            let b = bytes[i + k];
            let ok = if *want == b'd' {
                b.is_ascii_digit()
            } else {
                b == *want
            };
            if !ok {
                return None;
            }
        }
        let mut end = i + shape.len();
        if bytes.get(end) == Some(&b'.') {
            end += 1;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
        }
        text[end..].starts_with("+00:00").then_some(end + 6)
    };
    let mut out = String::new();
    let mut i = 0;
    while i < text.len() {
        if let Some(end) = is_stamp_at(i) {
            if text[i..end].starts_with("2025-") {
                out.push_str(&text[i..end]);
            } else {
                out.push_str("<NOW>");
            }
            i = end;
        } else {
            let ch = text[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

fn run(label: &str, argv: &[&str], populated: bool, policy: Policy) {
    let mut root = std::env::temp_dir();
    root.push(format!(
        "fnoappr-{}-{}",
        std::process::id(),
        common::slug(label)
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let db = build(&root, populated, policy);
    let args: Vec<String> = argv
        .iter()
        .map(|a| {
            let a = a.replace("<DB>", &db.display().to_string());
            match a.as_str() {
                "<A>" | "<B>" | "<C>" | "<D>" | "<E>" | "<F>" | "<G>" => {
                    digest_of(&a[1..2].to_lowercase())
                }
                _ => a,
            }
        })
        .collect();
    let out = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_STATE_DIR", &root)
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_TEST_HERMETIC", "1")
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(&root)
        .args(["compaction", "approvals"])
        .args(&args)
        .output()
        .unwrap();
    let rust = Golden {
        exit: out.status.code(),
        streams: vec![
            norm(&String::from_utf8_lossy(&out.stdout), &root),
            norm(&String::from_utf8_lossy(&out.stderr), &root),
        ],
    };
    assert_golden("approvals", label, &rust, None);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn approvals_match_the_python_commands() {
    let db = ["--db", "<DB>"];
    let with_db = |head: &[&'static str]| -> Vec<&'static str> {
        head.iter().copied().chain(db.iter().copied()).collect()
    };
    let cases: Vec<(&str, Vec<&str>, bool, Policy)> = vec![
        ("ls empty", with_db(&["ls"]), false, Policy::Founder),
        (
            "ls pending empty",
            with_db(&["ls", "--pending"]),
            false,
            Policy::Founder,
        ),
        ("ls all", with_db(&["ls"]), true, Policy::Founder),
        (
            "ls pending",
            with_db(&["ls", "--pending"]),
            true,
            Policy::Founder,
        ),
        ("ls json", with_db(&["ls", "--json"]), true, Policy::Founder),
        (
            "show pending",
            with_db(&["show", "<A>"]),
            true,
            Policy::Founder,
        ),
        (
            "show approved with transport",
            with_db(&["show", "<B>"]),
            true,
            Policy::Founder,
        ),
        (
            "show declined",
            with_db(&["show", "<C>"]),
            true,
            Policy::Founder,
        ),
        (
            "show expired",
            with_db(&["show", "<D>"]),
            true,
            Policy::Founder,
        ),
        (
            "show attempts",
            with_db(&["show", "<E>"]),
            true,
            Policy::Founder,
        ),
        (
            "show json",
            with_db(&["show", "<E>", "--json"]),
            true,
            Policy::Founder,
        ),
        (
            "show unknown digest",
            with_db(&["show", "<F>"]),
            true,
            Policy::Founder,
        ),
        (
            "decide approve",
            with_db(&["decide", "<A>", "--as", "founder", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide approve json",
            with_db(&["decide", "<A>", "--as", "founder", "--approve", "--json"]),
            true,
            Policy::Founder,
        ),
        (
            "decide decline",
            with_db(&["decide", "<A>", "--as", "founder", "--decline"]),
            true,
            Policy::Founder,
        ),
        (
            "decide with transport",
            with_db(&[
                "decide",
                "<A>",
                "--as",
                "founder",
                "--approve",
                "--transport",
                "slack-thread",
            ]),
            true,
            Policy::Founder,
        ),
        (
            "decide same decision again",
            with_db(&["decide", "<B>", "--as", "founder", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide replay conflict",
            with_db(&["decide", "<B>", "--as", "founder", "--decline"]),
            true,
            Policy::Founder,
        ),
        (
            "decide replay conflict json",
            with_db(&["decide", "<B>", "--as", "founder", "--decline", "--json"]),
            true,
            Policy::Founder,
        ),
        (
            "decide unknown digest",
            with_db(&["decide", "<F>", "--as", "founder", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide expired",
            with_db(&["decide", "<D>", "--as", "founder", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide unauthorized",
            with_db(&["decide", "<A>", "--as", "stranger", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide unauthorized json",
            with_db(&["decide", "<A>", "--as", "stranger", "--approve", "--json"]),
            true,
            Policy::Founder,
        ),
        (
            "decide class scoped principal ok",
            with_db(&["decide", "<A>", "--as", "intern", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide class scoped principal refused",
            with_db(&["decide", "<G>", "--as", "intern", "--approve"]),
            true,
            Policy::Founder,
        ),
        (
            "decide both flags",
            with_db(&["decide", "<A>", "--as", "founder", "--approve", "--decline"]),
            true,
            Policy::Founder,
        ),
        (
            "decide neither flag",
            with_db(&["decide", "<A>", "--as", "founder"]),
            true,
            Policy::Founder,
        ),
        (
            "decide no policy configured",
            with_db(&["decide", "<A>", "--as", "founder", "--approve"]),
            true,
            Policy::None,
        ),
        (
            "decide class only policy",
            with_db(&["decide", "<A>", "--as", "founder", "--approve"]),
            true,
            Policy::ClassOnly,
        ),
        (
            "ls no policy still reads",
            with_db(&["ls"]),
            true,
            Policy::None,
        ),
    ];
    for (label, argv, populated, policy) in cases {
        run(label, &argv, populated, policy);
    }
}

fn events_stores(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(events_stores(&path));
        } else if path.file_name().is_some_and(|n| n == "events.db") {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_decision_owes_one_event_and_lands_in_the_events_file() {
    let mut root = std::env::temp_dir();
    root.push(format!("fnoappr-events-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let db = build(&root, true, Policy::Founder);
    let status = Command::new(env!("CARGO_BIN_EXE_fno-agents"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", &root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_STATE_DIR", &root)
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_TEST_HERMETIC", "1")
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(&root)
        .args([
            "compaction",
            "approvals",
            "decide",
            &digest_of("a"),
            "--as",
            "founder",
            "--approve",
            "--db",
        ])
        .arg(&db)
        .output()
        .unwrap()
        .status;
    assert!(status.success());
    let conn = Connection::open(&db).unwrap();
    let owed: i64 = conn
        .query_row("SELECT COUNT(*) FROM outbox", [], |r| r.get(0))
        .unwrap();
    // The sqlite store is the write boundary and the state root decides where
    // it sits, so count the event in whichever events.db the run created.
    let landed: i64 = events_stores(&root)
        .iter()
        .filter_map(|store| fno_agents::event_store::open_read(store).ok())
        .filter_map(|conn| {
            conn.query_row(
                "SELECT COUNT(*) FROM events WHERE type = 'approval_decided'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .ok()
        })
        .sum();
    assert!(
        owed == 0 && landed == 1 || owed == 1,
        "the event is drained or still owed, never lost: owed={owed} landed={landed}"
    );
    let _ = fs::remove_dir_all(&root);
}
