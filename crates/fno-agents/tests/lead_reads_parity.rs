//! parity-stage: characterization
//! parity-oracle: fno.lead.cli (faq list, manifest-path, history, verdict, shape)
//!
//! Characterization for the lead read commands on their native argvs. The
//! goldens under tests/golden/lead_reads/ were captured on 2026-10-24 from
//! the deployed Python commands over the fixtures in tests/fixtures/lead_reads/,
//! before the Python legs were deleted. Capture mode refuses: a golden can
//! only be captured while the old leg runs.
//!
//! The temp root folds to `<ROOT>` (raw and canonical spellings), its space
//! slug to `<ROOTSLUG>`, and the wall-clock coverage stamps to `<NOW>`.

use common::{assert_golden, Golden};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

mod common;

const SUBJECT: &str = "lead_reads";

/// Identity markers a caller could carry; every case runs with none.
const IDENTITY_ENV: &[&str] = &[
    "FNO_SESSION",
    "FNO_AGENT_SELF",
    "FNO_AGENT_SESSION",
    "FNO_AGENT_HARNESS",
    "FNO_AGENT_PROVIDER",
    "FNO_AGENT_SUBSTRATE",
    "FNO_HARNESS_NAME",
    "FNO_HARNESS_SESSION_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "CODEX_THREAD_ID",
    "CODEX_SESSION_ID",
    "CODEX_COMPANION_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "FNO_EVENTS_PATH",
    "FNO_CONFIG",
];

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/lead_reads")
        .join(name)
}

/// One fresh root: an agents home holding the fixture registry, a state dir,
/// and a project journal holding the fixture events.
fn root() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    for dir in ["home", "state", "project", "spaces"] {
        fs::create_dir_all(tmp.path().join(dir)).unwrap();
    }
    fs::copy(
        fixture("registry.json"),
        tmp.path().join("home/registry.json"),
    )
    .unwrap();
    fs::copy(
        fixture("events.jsonl"),
        tmp.path().join("project/events.jsonl"),
    )
    .unwrap();
    tmp
}

fn slug(path: &Path) -> String {
    path.to_string_lossy().replace('/', "-")
}

/// Replace each wall-clock stamp after `marker` with `<NOW>`.
fn fold_stamps(text: &str, marker: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find(marker) {
        let (head, tail) = rest.split_at(at + marker.len());
        out.push_str(head);
        let end = tail
            .find(|c: char| !(c.is_ascii_digit() || matches!(c, 'T' | ':' | '.' | '-' | 'Z')))
            .unwrap_or(tail.len());
        out.push_str("<NOW>");
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn normalize(text: &str, root: &Path) -> String {
    let canonical = fs::canonicalize(root).unwrap();
    let text = text
        .replace(&slug(&canonical), "<ROOTSLUG>")
        .replace(&slug(root), "<ROOTSLUG>")
        .replace(&canonical.to_string_lossy().into_owned(), "<ROOT>")
        .replace(&root.to_string_lossy().into_owned(), "<ROOT>");
    fold_stamps(
        &fold_stamps(&text, "complete since "),
        "\"complete_since\": \"",
    )
}

/// Run one native argv from `cwd` under the root's pins and assert its golden.
fn case(label: &str, root: &Path, cwd: &Path, argv: &[&str], fold_root: &Path) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fno-agents"));
    cmd.args(argv).current_dir(cwd);
    for var in IDENTITY_ENV {
        cmd.env_remove(var);
    }
    let out = cmd
        .env("HOME", root)
        .env("FNO_AGENTS_HOME", root.join("home"))
        .env("FNO_STATE_DIR", root.join("state"))
        .env("FNO_SPACES_DIR", root.join("spaces"))
        .env("FNO_EVENTS_PATH", root.join("project/events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .output()
        .unwrap();
    let streams = vec![
        normalize(&String::from_utf8_lossy(&out.stdout), fold_root),
        normalize(&String::from_utf8_lossy(&out.stderr), fold_root),
    ];
    assert_golden(
        SUBJECT,
        label,
        &Golden {
            exit: out.status.code(),
            streams,
        },
        None,
    );
}

#[test]
fn faq_list_matches_the_python_goldens() {
    let tmp = root();
    let dir = tmp.path().join("state/briefs/lead-faqs");
    fs::create_dir_all(&dir).unwrap();
    // Order is by mtime, not name: 0003 is the oldest fno entry.
    let day = |d: u64| SystemTime::UNIX_EPOCH + Duration::from_secs(1_790_000_000 + d * 86_400);
    for (name, age) in [
        ("lead-fno-0003.md", 1),
        ("lead-ops-0002.md", 2),
        ("lead-fno-0001.md", 5),
        ("other-fno.md", 6),
    ] {
        let path = dir.join(name);
        fs::copy(fixture(&format!("faqs/{name}")), &path).unwrap();
        fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(day(age))
            .unwrap();
    }
    let p = tmp.path();
    case(
        "faq list scope fno",
        p,
        p,
        &["lead-state", "--faq", "list", "--scope", "fno"],
        p,
    );
    case(
        "faq list max entries one",
        p,
        p,
        &[
            "lead-state",
            "--faq",
            "list",
            "--scope",
            "fno",
            "--max-entries",
            "1",
        ],
        p,
    );
    case(
        "faq list unknown scope",
        p,
        p,
        &["lead-state", "--faq", "list", "--scope", "nobody"],
        p,
    );
}

#[test]
fn manifest_path_matches_the_python_goldens() {
    let tmp = root();
    let p = tmp.path();
    let state_root = p.join("root");
    fs::create_dir_all(state_root.join("leads")).unwrap();
    // The manifest names the predecessor's session: the read keys on the
    // live row's role scope, so a respawned lead still finds its manifest.
    fs::write(
        state_root.join("leads/epic-y.md"),
        "---\nscope: epic-y\nharness_session_id: sess-predecessor\n---\n",
    )
    .unwrap();
    let sr = state_root.to_string_lossy().into_owned();
    let cases: &[(&str, &[&str])] = &[
        (
            "manifest path found",
            &["--harness-session-id", "sess-lead"],
        ),
        (
            "manifest path found under harness",
            &["--harness-session-id", "sess-lead", "--harness", "claude"],
        ),
        (
            "manifest path wrong harness",
            &["--harness-session-id", "sess-lead", "--harness", "codex"],
        ),
        (
            "manifest path stranger",
            &["--harness-session-id", "sess-nobody"],
        ),
        (
            "manifest path terminal row",
            &["--harness-session-id", "sess-exited"],
        ),
        (
            "manifest path unstamped row",
            &["--harness-session-id", "sess-plain"],
        ),
        (
            "manifest path no manifest",
            &["--harness-session-id", "sess-bare"],
        ),
        ("manifest path empty session", &["--harness-session-id", ""]),
    ];
    for (label, flags) in cases {
        let mut argv = vec!["lead-state", "--manifest-path"];
        argv.extend_from_slice(flags);
        argv.extend_from_slice(&["--state-root", sr.as_str()]);
        case(label, p, p, &argv, &state_root);
    }
}

#[test]
fn history_matches_the_python_goldens() {
    let cases: &[(&str, &[&str])] = &[
        ("history scope epic y", &["--scope", "epic-y"]),
        (
            "history scope epic y json",
            &["--scope", "epic-y", "--json"],
        ),
        ("history scope without rows", &["--scope", "epic-q"]),
        ("history no scope no identity", &[]),
    ];
    for (label, flags) in cases {
        // A fresh root per case: the read syncs a store beside each journal.
        let tmp = root();
        let p = tmp.path();
        let mut argv = vec!["lead-history"];
        argv.extend_from_slice(flags);
        case(label, p, p, &argv, p);
    }
}

#[test]
fn verdict_matches_the_python_goldens() {
    let cases: &[(&str, bool, &[&str])] = &[
        ("verdict scope epic y", true, &["epic-y"]),
        ("verdict scope without manifest", false, &["epic-y"]),
        ("verdict no scope no identity", false, &[]),
    ];
    for (label, with_manifest, flags) in cases {
        let tmp = root();
        let p = tmp.path();
        let repo = p.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let init = Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(init.success());
        if *with_manifest {
            let space = p
                .join("spaces")
                .join(slug(&fs::canonicalize(&repo).unwrap()));
            fs::create_dir_all(space.join("leads")).unwrap();
            fs::copy(fixture("manifest-epic-y.md"), space.join("leads/epic-y.md")).unwrap();
        }
        let mut argv = vec!["lead-history", "--verdict", "--user-command"];
        argv.extend_from_slice(flags);
        case(label, p, &repo, &argv, p);
    }
}

#[test]
fn shape_refusals_match_the_python_goldens() {
    let tmp = root();
    let p = tmp.path();
    case(
        "shape refuses an unknown shape",
        p,
        p,
        &["lead-shape", "--user-command", "wide"],
        p,
    );
    case(
        "shape refuses an attended shell",
        p,
        p,
        &["lead-shape", "--user-command", "team"],
        p,
    );
}
