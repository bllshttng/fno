//! parity-stage: characterization
//! parity-oracle: cli/src/fno/pr/_review_hold.py
//!
//! Characterization tests for the `claim review-hold` port (x-1d1e). The
//! Python leg was captured into tests/golden/review_hold/ before deletion;
//! each case rebuilds the same fixture and asserts the Rust verb's
//! (exit, stdout, stderr) against the frozen golden. The lapsed case
//! additionally asserts the `review_hold_expired` event the Rust reader
//! now emits (the one measured divergence this port fixes).

mod common;

use common::Golden;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The 40-hex regex-free normalizer: every exactly-40 run of hex bounded by
/// non-hex becomes <SHA40>, and the fixture root becomes <FIXTURE>.
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
    gh_dir: PathBuf,
}

impl Fixture {
    fn bin_path(&self) -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_fno-agents"))
    }
}

/// Marker env vars scrubbed from every spawn: identity resolution must be
/// deterministic (unattributed) in the goldens.
const MARKERS: [&str; 8] = [
    "CODEX_THREAD_ID",
    "CLAUDE_CODE_SESSION_ID",
    "CODEX_SESSION_ID",
    "GEMINI_SESSION_ID",
    "OPENCODE_SESSION_ID",
    "CLAUDE_SESSION_ID",
    "FNO_HARNESS_SESSION_ID",
    "FNO_CONFIG",
];

/// Build the fixture: a temp git repo (branch feat/a at c0, fake origin), a
/// fake gh keyed by FGH_MODE, and a per-process env the run helper applies.
fn fixture(tag: &str) -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap().keep();
    let repo = tmp.join(tag);
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main", "."]);
    fs::write(repo.join("file.txt"), "v1\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "c0"]);
    git(&repo, &["branch", "feat/a"]);
    git(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/golden/repo.git",
        ],
    );
    let home = tmp.join("home");
    fs::create_dir_all(&home).unwrap();
    let gh_dir = write_fake_gh(&tmp, repo_head(&repo));
    Fixture {
        root: tmp,
        repo,
        home,
        gh_dir,
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

fn repo_head(repo: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The fake gh the Python capture used, behaviorally byte-for-byte except the
/// one seam the port moved: the reader now asks `gh pr view` for the
/// headRefName/headRefOid/state field set (the REST pulls shape the Python
/// leg parsed), so the payload carries that field set, keyed by FGH_MODE.
fn write_fake_gh(root: &Path, oid: String) -> PathBuf {
    let dir = root.join("fakegh");
    fs::create_dir_all(&dir).unwrap();
    let open =
        format!(r#"{{"headRefName":"feat/a","headRefOid":"{oid}","state":"open","number":41}}"#);
    let closed =
        format!(r#"{{"headRefName":"feat/a","headRefOid":"{oid}","state":"closed","number":41}}"#);
    let script = format!(
        "#!/bin/sh\ncase \"$FGH_MODE\" in\n  fail) echo 'gh: api error' >&2; exit 1 ;;\n  closed) echo '{closed}' ;;\n  *) echo '{open}' ;;\nesac\n"
    );
    let gh = dir.join("gh");
    fs::write(&gh, script).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    }
    dir
}

/// Run one `claim review-hold <args>` against the built binary with the
/// fixture env plus per-case overrides. Returns (exit, out, err) normalized.
fn run(fx: &Fixture, args: &[&str], extra_env: &[(&str, String)]) -> (i32, String, String) {
    let mut cmd = Command::new(&fx.bin_path());
    cmd.arg("claim").arg("review-hold");
    cmd.args(args);
    cmd.current_dir(&fx.repo);
    cmd.envs(fno_agents::test_run::self_owner_env());
    cmd.env("HOME", &fx.home);
    cmd.env("FNO_STATE_DIR", fx.root.join("state"));
    // The repo-space claims store resolves through the spaces root, so the
    // seed and the verb must both land in this tree; without the pin the
    // binary writes under its own home default and the test's db path (and
    // the raw sqlite stale-corruption edits below) read a store that was
    // never created.
    cmd.env("FNO_SPACES_DIR", fx.root.join("state/spaces"));
    cmd.env("FNO_EVENTS_PATH", fx.root.join("events.jsonl"));
    cmd.env("FGH_MODE", "open");
    let path = format!(
        "{}:{}",
        fx.gh_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path);
    for m in MARKERS {
        cmd.env_remove(m);
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run claim review-hold");
    let code = out.status.code().unwrap_or(-1);
    let stdout = normalize(&String::from_utf8_lossy(&out.stdout), &fx.root);
    let stderr = normalize(&String::from_utf8_lossy(&out.stderr), &fx.root);
    (code, stdout, stderr)
}

/// Seed one review hold through the same binary the verb runs under, so the
/// seeded record matches what a production acquire writes.
fn seed_hold(fx: &Fixture, ttl_ms: u64) {
    let head = repo_head(&fx.repo);
    let meta = format!(r#"{{"head_sha":"{head}"}}"#);
    let out = Command::new(fx.bin_path())
        .args(["claim", "acquire", "review:branch:feat/a"])
        .args(["--holder", "review-session:s1"])
        .args(["--ttl-ms", &ttl_ms.to_string()])
        .args(["--metadata", &meta])
        .current_dir(&fx.repo)
        .env("HOME", &fx.home)
        .env("FNO_STATE_DIR", fx.root.join("state"))
        .env("FNO_SPACES_DIR", fx.root.join("state/spaces"))
        .output()
        .expect("seed claim");
    assert!(
        out.status.success(),
        "seed acquire failed: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── check family ─────────────────────────────────────────────────────────────

fn assert_case(fx: &Fixture, label: &str, args: &[&str], env: &[(&str, String)]) {
    let (rc, out, err) = run(fx, args, env);
    let rust = Golden {
        exit: Some(rc),
        streams: vec![out, err],
    };
    common::assert_golden("review_hold", label, &rust, None);
}

#[test]
fn check_clear() {
    let fx = fixture("repo");
    assert_case(&fx, "check clear", &["check", "41"], &[]);
}

#[test]
fn check_live_hold() {
    let fx = fixture("repo");
    seed_hold(&fx, 3_600_000);
    assert_case(&fx, "check live hold", &["check", "41"], &[]);
}

#[test]
fn check_lapsed_hold() {
    let fx = fixture("repo");
    seed_hold(&fx, 3_600_000);
    force_stale(&fx);
    assert_case(&fx, "check lapsed hold", &["check", "41"], &[]);
    expired_event_matches(&fx);
}

/// Rewrite the seeded claim's expiry into the past, the way the capture
/// script forced STALE without sleeping out the TTL.
fn force_stale(fx: &Fixture) {
    let db = claims_db(&fx);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE claims SET expires_at = 1000 WHERE key = 'review:branch:feat/a'",
        [],
    )
    .unwrap();
}

/// The claims SQLite store: <state>/spaces/<spacekey>/db/graph.db.
fn claims_db(fx: &Fixture) -> PathBuf {
    let resolved = fs::canonicalize(&fx.repo).unwrap();
    let mut key = String::from("-");
    for c in resolved.to_string_lossy().trim_start_matches('/').chars() {
        key.push(if c == '/' { '-' } else { c });
    }
    fx.root
        .join("state")
        .join("spaces")
        .join(key)
        .join("db")
        .join("graph.db")
}

/// The one measured divergence this port fixes: the Rust reader emits the
/// review_hold_expired receipt the schema describes, with the fields
/// `_emit_expired` wrote.
fn expired_event_matches(fx: &Fixture) {
    let text = fs::read_to_string(fx.root.join("events.jsonl")).unwrap_or_default();
    let line = text
        .lines()
        .find(|l| l.contains("review_hold_expired"))
        .unwrap_or_else(|| panic!("no review_hold_expired event in {}", text));
    let event: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(event["source"], "fno-loop");
    let data = &event["data"];
    assert_eq!(data["key"], "review:branch:feat/a");
    assert_eq!(data["holder"], "review-session:s1");
    assert_eq!(data["expires_at"], 1000);
    assert_eq!(
        data["head_sha"],
        repo_head(&fx.repo).as_str(),
        "the expired receipt names the held head"
    );
}

#[test]
fn check_corrupted_hold() {
    let fx = fixture("repo");
    seed_hold(&fx, 3_600_000);
    let db = claims_db(&fx);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE claims SET metadata = 'not-json' WHERE key = 'review:branch:feat/a'",
        [],
    )
    .unwrap();
    assert_case(&fx, "check corrupted hold", &["check", "41"], &[]);
}

#[test]
fn check_dirty_worktree() {
    let fx = fixture("repo");
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            fx.root.join("wt1").to_str().unwrap(),
            "feat/a",
        ],
    );
    fs::write(fx.root.join("wt1").join("file.txt"), "dirty\n").unwrap();
    assert_case(&fx, "check dirty worktree", &["check", "41"], &[]);
}

#[test]
fn check_head_mismatch() {
    let fx = fixture("repo");
    fs::write(fx.repo.join("file.txt"), "v2\n").unwrap();
    git(&fx.repo, &["commit", "-qam", "c1"]);
    git(
        &fx.repo,
        &[
            "worktree",
            "add",
            "-q",
            fx.root.join("wt1").to_str().unwrap(),
            "feat/a",
        ],
    );
    assert_case(&fx, "check head mismatch", &["check", "41"], &[]);
}

#[test]
fn check_unresolvable_pr() {
    let fx = fixture("repo");
    assert_case(
        &fx,
        "check unresolvable pr",
        &["check", "41"],
        &[("FGH_MODE", "fail".to_string())],
    );
}

// ── acquire family ───────────────────────────────────────────────────────────

#[test]
fn acquire_by_branch() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    assert_case(
        &fx,
        "acquire by branch",
        &[
            "acquire",
            "--branch",
            "feat/a",
            "--head",
            &head,
            "--holder",
            "review-session:s1",
            "--verb",
            "/fno:review",
        ],
        &[],
    );
}

#[test]
fn acquire_by_pr() {
    let fx = fixture("repo");
    assert_case(
        &fx,
        "acquire by pr",
        &["acquire", "41", "--holder", "review-session:s1"],
        &[],
    );
}

#[test]
fn acquire_closed_pr() {
    let fx = fixture("repo");
    assert_case(
        &fx,
        "acquire closed pr",
        &["acquire", "41", "--holder", "review-session:s1"],
        &[("FGH_MODE", "closed".to_string())],
    );
}

#[test]
fn acquire_missing_holder() {
    let fx = fixture("repo");
    assert_case(
        &fx,
        "acquire missing holder",
        &["acquire", "--branch", "feat/a"],
        &[],
    );
}

#[test]
fn release_held() {
    let fx = fixture("repo");
    seed_hold(&fx, 3_600_000);
    assert_case(&fx, "release held", &["release", "--branch", "feat/a"], &[]);
}

#[test]
fn release_absent() {
    let fx = fixture("repo");
    assert_case(
        &fx,
        "release absent",
        &["release", "--branch", "feat/a"],
        &[],
    );
}

#[test]
fn round_with_journals() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    // The chain journal the round reads: one attestation row in scope.
    let row = format!(
        r#"{{"ts":"2026-10-15T00:00:00+00:00","type":"review_attestation","source":"target","data":{{"branch":"feat/a","head_sha":"{head}"}}}}"#
    );
    fs::write(fx.root.join("events.jsonl"), format!("{row}\n")).unwrap();
    let dotfno = fx.repo.join(".fno");
    fs::create_dir_all(&dotfno).unwrap();
    fs::write(dotfno.join("events.jsonl"), format!("{row}\n")).unwrap();
    assert_case(
        &fx,
        "round with journals",
        &["round", "--branch", "feat/a", "--head", &head],
        &[],
    );
}

#[test]
fn round_missing_journals() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    assert_case(
        &fx,
        "round missing journals",
        &["round", "--branch", "feat/a", "--head", &head],
        &[],
    );
}

// ── the cap-refusal family (chain journals seeded natively) ─────────────────

/// Two distinct in-scope reviewed heads: the per-PR total reads 2 of 2.
fn seed_chain(fx: &Fixture, heads: [&str; 2], blocking: i64) {
    let rows: Vec<String> = heads
        .iter()
        .map(|h| {
            format!(
                r#"{{"ts":"2026-10-15T00:00:0{}+00:00","type":"review_attestation","source":"target","data":{{"branch":"feat/a","head_sha":"{h}","findings":[{{"blocking":{blocking}}}]}}}}"#,
                if *h == heads[0] { 0 } else { 1 }
            )
        })
        .collect();
    let text = format!("{}\n", rows.join("\n"));
    fs::write(fx.root.join("events.jsonl"), text.clone()).unwrap();
    let dotfno = fx.repo.join(".fno");
    fs::create_dir_all(&dotfno).unwrap();
    fs::write(dotfno.join("events.jsonl"), text).unwrap();
}

#[test]
fn acquire_cap_refusal() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    let other = "1111111111111111111111111111111111111111";
    seed_chain(&fx, [other, "2222222222222222222222222222222222222222"], 0);
    assert_case(
        &fx,
        "acquire cap refusal",
        &[
            "acquire",
            "--branch",
            "feat/a",
            "--head",
            &head,
            "--holder",
            "review-session:s1",
        ],
        &[],
    );
}

#[test]
fn acquire_verify_fixes_advisory() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    let other = "1111111111111111111111111111111111111111";
    seed_chain(&fx, [other, "2222222222222222222222222222222222222222"], 2);
    assert_case(
        &fx,
        "acquire verify fixes advisory",
        &[
            "acquire",
            "--branch",
            "feat/a",
            "--head",
            &head,
            "--holder",
            "review-session:s1",
            "--flags-json",
            r#"["--verify-fixes"]"#,
        ],
        &[],
    );
}

// ── metadata: structural compare over the volatile fields ───────────────────

#[test]
fn metadata_fields() {
    let fx = fixture("repo");
    seed_hold(&fx, 3_600_000);
    let (rc, out, _err) = run(&fx, &["metadata", "--branch", "feat/a"], &[]);
    assert_eq!(rc, 0);
    let payload: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(payload["key"], "review:branch:feat/a");
    assert_eq!(payload["state"], "live");
    assert_eq!(payload["holder"], "review-session:s1");
    assert_eq!(
        payload["metadata"]["head_sha"],
        repo_head(&fx.repo).as_str()
    );
    // The volatile identity fields ride along, whatever they read here.
    assert!(payload.get("acquired_at").is_some());
    assert!(payload.get("host").is_some());
}

// ── the stdin JSON door (no argv) ────────────────────────────────────────────

#[test]
fn stdin_door_acquires_and_answers_registered() {
    let fx = fixture("repo");
    let head = repo_head(&fx.repo);
    let payload = format!(
        r#"{{"action":"acquire","branch":"feat/a","head":"{head}","holder":"review-session:s1","verb":"/fno:review","cwd":"REPO"}}"#
    );
    let payload = payload.replace("REPO", &fx.repo.to_string_lossy());
    let mut cmd = Command::new(fx.bin_path());
    cmd.args(["claim", "review-hold"]);
    cmd.current_dir(&fx.repo);
    cmd.env("HOME", &fx.home);
    cmd.env("FNO_STATE_DIR", fx.root.join("state"));
    cmd.env("FNO_EVENTS_PATH", fx.root.join("events.jsonl"));
    cmd.env("FGH_MODE", "open");
    let path = format!(
        "{}:{}",
        fx.gh_dir.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    cmd.env("PATH", path);
    for m in MARKERS {
        cmd.env_remove(m);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap();
    }
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let answer: serde_json::Value =
        serde_json::from_str(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(answer["registered"], true);
    assert_eq!(answer["branch"], "feat/a");
    // The hold actually took.
    let (rc, out_line, _err) = run(&fx, &["metadata", "--branch", "feat/a"], &[]);
    assert_eq!(rc, 0);
    assert!(out_line.contains("review-session:s1"));
}
