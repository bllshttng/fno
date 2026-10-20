//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr/_attestation.py
//!
//! Characterization tests for the `attest retract` port (x-1d1e). The
//! Python leg was captured into tests/golden/attest_retract/ before
//! deletion; each case rebuilds the same fixture and asserts the Rust
//! verb's (exit, stdout, stderr) against the frozen golden, plus a
//! structural read of the retraction row it appended. The identity-conflict
//! golden rides a src unit test: real ancestry cannot be faked from an
//! integration process (attest.rs::retraction_conflict_message).

mod common;

use common::Golden;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The 40-hex normalizer from review_hold_parity: every exactly-40 run of
/// lowercase hex bounded by non-hex becomes <SHA40>, and the fixture root
/// becomes <FIXTURE>.
fn normalize(s: &str, tmp: &Path) -> String {
    let replaced = s.replace(tmp.to_str().unwrap(), "<FIXTURE>");
    let mut out = String::with_capacity(replaced.len());
    let chars: Vec<char> = replaced.chars().collect();
    let (mut i, n) = (0, chars.len());
    let is_hex = |c: char| c.is_ascii_hexdigit() && !c.is_ascii_uppercase();
    while i < n {
        if is_hex(chars[i]) {
            let mut j = i;
            while j < n && is_hex(chars[j]) {
                j += 1;
            }
            if j - i == 40 && (i == 0 || !is_hex(chars[i - 1])) {
                out.push_str("<SHA40>");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

struct Fixture {
    root: PathBuf,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn bin_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_fno-agents"))
    }
}

/// Identity markers scrubbed from every spawn, as the capture scrubbed
/// them: the retracting process must resolve unattributed/env_only.
const MARKERS: [&str; 7] = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "FNO_HARNESS_SESSION_ID",
];

/// The capture's synthetic attested head: fixed, so the seeded pass row, the
/// retract argv and the retraction row all carry the same value, and the
/// goldens' `0f0f0f0f0f0f` truncation and `<SHA40>` normalization both hold.
const HEAD40: &str = "0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f";

// ── xtail: fixture + runners (appended below) ───────────────────────────────

/// Build the fixture: a temp git repo on main at one commit (the `branch`
/// the retraction row records), HOME and FNO_STATE_DIR pinned like the
/// capture's base_env.
fn fixture(tag: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap().keep();
    let repo = tmp.join(tag);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "c0"]);
    let home = tmp.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::create_dir_all(tmp.join("state")).unwrap();
    Fixture {
        root: tmp,
        repo,
        home,
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "golden")
        .env("GIT_AUTHOR_EMAIL", "golden@example.com")
        .env("GIT_COMMITTER_NAME", "golden")
        .env("GIT_COMMITTER_EMAIL", "golden@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Run one `attest retract` against the built binary with the fixture env.
fn run(fx: &Fixture, args: &[&str]) -> (i32, String, String) {
    let mut cmd = Command::new(fx.bin_path());
    cmd.arg("attest").arg("retract");
    cmd.args(args);
    cmd.current_dir(&fx.repo);
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.env("HOME", &fx.home);
    cmd.env("FNO_STATE_DIR", fx.root.join("state"));
    cmd.env("FNO_EVENTS_PATH", fx.root.join("events.jsonl"));
    for m in MARKERS {
        cmd.env_remove(m);
    }
    let out = cmd.output().expect("run attest retract");
    let code = out.status.code().unwrap_or(-1);
    let stdout = normalize(&String::from_utf8_lossy(&out.stdout), &fx.root);
    let stderr = normalize(&String::from_utf8_lossy(&out.stderr), &fx.root);
    (code, stdout, stderr)
}

/// The pass row the capture seeded, byte-for-byte (json.dumps spacing).
fn seed_pass(fx: &Fixture, path: &Path, reviewer: &str) {
    fs::write(
        path,
        format!(
            r#"{{"ts": "2026-10-15T00:00:00+00:00", "type": "review_attestation", "source": "target", "data": {{"reviewer": "{reviewer}", "attester_session_id": "s9", "head_sha": "{HEAD40}", "verdict": "pass"}}}}"#
        ),
    )
    .unwrap();
}

// ── xtail: cases (appended below) ───────────────────────────────────────────

/// The shared argv: the exact flags the capture passed.
fn retract_args(events: &Path) -> Vec<String> {
    vec![
        "--reviewer".to_string(),
        "code-review".to_string(),
        "--attester".to_string(),
        "s9".to_string(),
        "--head".to_string(),
        HEAD40.to_string(),
        "--reason".to_string(),
        "forged pass".to_string(),
        "--events".to_string(),
        events.to_string_lossy().into_owned(),
    ]
}

/// The retraction row the verb appended: found by retraction_reason, then
/// asserted field by field (the golden's .events file is the reference).
fn retraction_row(journal: &Path) -> serde_json::Value {
    let text = fs::read_to_string(journal).unwrap();
    let line = text
        .lines()
        .find(|l| l.contains("retraction_reason"))
        .unwrap_or_else(|| panic!("no retraction row in {text}"));
    serde_json::from_str(line).unwrap()
}

fn assert_retraction_row(journal: &Path, head: &str) {
    let row = retraction_row(journal);
    assert_eq!(row["type"], "review_attestation");
    assert_eq!(row["source"], "test");
    let data = &row["data"];
    assert_eq!(data["reviewer"], "code-review");
    assert_eq!(data["head_sha"], head);
    assert_eq!(data["verdict"], "fail");
    assert_eq!(data["session_id"], "");
    assert_eq!(data["retracts_attester"], "s9");
    assert_eq!(data["attester_session_id"], "");
    assert_eq!(data["attester_witness"], "env_only");
    assert_eq!(data["retraction_reason"], "forged pass");
    assert_eq!(data["branch"], "main");
}

// ── xtail: tests (appended below) ───────────────────────────────────────────

#[test]
fn retract_match() {
    let fx = fixture("repo");
    let events = fx.root.join("events.jsonl");
    seed_pass(&fx, &events, "code-review");
    let args = retract_args(&events);
    let (rc, out, err) = run(&fx, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden("attest_retract", "retract match", &rust, None);
    assert_retraction_row(&events, HEAD40);
}

#[test]
fn retract_no_pass() {
    let fx = fixture("repo");
    let events = fx.root.join("events.jsonl");
    seed_pass(&fx, &events, "other");
    let args = retract_args(&events);
    let (rc, out, err) = run(&fx, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden("attest_retract", "retract no pass", &rust, None);
    // Nothing was appended: the refusal prints without writing a row.
    let text = fs::read_to_string(&events).unwrap();
    assert_eq!(text.lines().count(), 1, "no retraction may be appended");
}

#[test]
fn retract_match_via_global() {
    let fx = fixture("repo");
    let events = fx.root.join("events.jsonl");
    fs::write(&events, "").unwrap();
    seed_pass(&fx, &fx.root.join("state/events.jsonl"), "code-review");
    let args = retract_args(&events);
    let (rc, out, err) = run(&fx, &args.iter().map(String::as_str).collect::<Vec<_>>());
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden("attest_retract", "retract match via global", &rust, None);
    assert_retraction_row(&events, HEAD40);
}
