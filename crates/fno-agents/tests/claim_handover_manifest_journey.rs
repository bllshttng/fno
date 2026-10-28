//! The handover-to-manifest journey: a worker whose spawn-handover claim
//! vanished (or went suspect) before init still ends up with a claim and an
//! unblocked manifest.
//!
//! Measured live 2026-10-28: the worker's init met a spawn-handover claim
//! held by an already-dead pid, its acquire (running bytes without the
//! handover flags) refused, and the manifest froze with
//! `target_claim_blocked_reason: claim_held_by_other` plus an init-authored
//! `.target-cancelled` sentinel - the stop gate then refused every watching
//! tag. The repair chain (armed `--handover-from`, the suspect rebind, the
//! vanished-claim fallthrough, the manifest refresh) spans the acquire leaf
//! and the manifest refresh; the refresh's own unit tests never run the
//! acquire, so the wiring on both prior-claim states was unpinned. These two
//! cases pin it through the real binary, host-independent: the dead pid is a
//! waited child, never a guessed constant.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

/// One sandbox: a git repo the acquire runs in, a claims root, and the
/// spaces dir the manifest resolution lands in.
struct Sandbox {
    base: PathBuf,
    repo: PathBuf,
    claims_root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("fno-handover-journey-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let repo = base.join("repo");
        fs::create_dir_all(&repo).unwrap();
        let status = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["-C", repo.to_str().unwrap(), "init", "-q"])
            .status()
            .unwrap();
        assert!(status.success(), "git init failed for {}", repo.display());
        let claims_root = base.join("claims");
        fs::create_dir_all(&claims_root).unwrap();
        Self {
            base,
            repo,
            claims_root,
        }
    }

    /// The manifest path, computed by the same rule the binary resolves: the
    /// canonical repo root with `/` replaced by `-`, under the spaces dir.
    /// The parent is created here because nothing else makes it before the
    /// first write.
    fn manifest_path(&self) -> PathBuf {
        let canonical = fs::canonicalize(&self.repo).unwrap();
        let slug = canonical.to_string_lossy().replace('/', "-");
        let manifest = self.base.join("spaces").join(slug).join("target-state.md");
        fs::create_dir_all(manifest.parent().unwrap()).unwrap();
        manifest
    }

    fn write_poisoned_manifest(&self) -> PathBuf {
        let manifest = self.manifest_path();
        fs::write(
            &manifest,
            "---\n\
             fno_id: f\n\
             session_id: sess-j1\n\
             harness_session_id: hsess-j1\n\
             created_at: '2026-10-28T00:00:00Z'\n\
             input: x-j1\n\
             ---\n\
             # Target Session State\n\
             \n\
             target_claim_blocked_reason: claim_held_by_other\n\
             graph_node_id: x-j1\n",
        )
        .unwrap();
        let sentinel = manifest.parent().unwrap().join(".target-cancelled");
        fs::write(&sentinel, "author: init\nreason: claim_held_by_other\n").unwrap();
        manifest
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

/// Run `claim <verb>` in the sandbox repo with the harness-marker set
/// scrubbed, so ambient resolution never reads the invoking shell.
fn run(sandbox: &Sandbox, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(binary_path())
        .args(args)
        .current_dir(&sandbox.repo)
        .env("FNO_CLAIMS_ROOT", &sandbox.claims_root)
        .env("FNO_STATE_DIR", sandbox.base.join("state"))
        .env("FNO_AGENTS_HOME", sandbox.base.join("agents-home"))
        .env("FNO_SPACES_DIR", sandbox.base.join("spaces"))
        .env("FNO_EVENTS_PATH", sandbox.base.join("events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .env("FNO_HARNESS_NAME", "claude")
        .env("CLAUDE_CODE_SESSION_ID", "")
        .env("CODEX_THREAD_ID", "")
        .env("CODEX_SESSION_ID", "")
        .env("CODEX_COMPANION_SESSION_ID", "")
        .env("CODEX_COMPANION_TRANSCRIPT_PATH", "")
        .env("GEMINI_SESSION_ID", "")
        .env("OPENCODE_SESSION_ID", "")
        .env("CLAUDE_SESSION_ID", "")
        .output()
        .expect("run fno-agents");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A pid guaranteed dead on this host: wait() reaped it, so the pid is free
/// and the claim it anchors reads suspect, never live.
fn waited_dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn true");
    child.wait().expect("reap child");
    child.id()
}

const HANDOVER_HOLDER: &str = "spawn-handover:worker-j1";
const SESSION_HOLDER: &str = "target-session:sess-j1";

fn plant_launch_window_claim(sandbox: &Sandbox, pid: u32) {
    let (rc, stdout, stderr) = run(
        sandbox,
        &[
            "claim",
            "acquire",
            "node:x-j1",
            "--holder",
            HANDOVER_HOLDER,
            "--ttl",
            "30m",
            "--pid",
            &pid.to_string(),
            "--reason",
            "spawn handover window for x-j1",
        ],
    );
    assert_eq!(rc, 0, "plant failed: {stdout} / {stderr}");
}

fn assert_manifest_unblocked(manifest: &Path) {
    let healed = fs::read_to_string(manifest).unwrap();
    assert!(
        !healed.contains("target_claim_blocked_reason:"),
        "blocked line must be gone: {healed}"
    );
    assert!(
        healed.contains("target_claim_key: \"node:x-j1\""),
        "claim key must be appended: {healed}"
    );
    assert!(
        healed.contains(&format!("target_claim_holder: \"{SESSION_HOLDER}\"")),
        "claim holder must be appended: {healed}"
    );
    let sentinel = manifest.parent().unwrap().join(".target-cancelled");
    assert!(
        !sentinel.exists(),
        "init sentinel must be consumed: {}",
        sentinel.display()
    );
}

#[test]
fn vanished_prior_claim_falls_through_to_fresh_acquire_and_heals_manifest() {
    let sandbox = Sandbox::new("vanished");
    // The spawn claimed the node, then the window closed (released) before
    // the worker's init ran - the measured release shape.
    plant_launch_window_claim(&sandbox, waited_dead_pid());
    let (rc, stdout, stderr) = run(
        &sandbox,
        &["claim", "release", "node:x-j1", "--holder", HANDOVER_HOLDER],
    );
    assert_eq!(rc, 0, "release failed: {stdout} / {stderr}");

    let manifest = sandbox.write_poisoned_manifest();

    let (rc, stdout, stderr) = run(
        &sandbox,
        &[
            "claim",
            "acquire",
            "node:x-j1",
            "--holder",
            SESSION_HOLDER,
            "--ttl",
            "30m",
            "--pid-unavailable",
            "--handover-from",
            HANDOVER_HOLDER,
            "--reason",
            "target dispatch",
        ],
    );
    assert_eq!(rc, 0, "acquire failed: {stdout} / {stderr}");
    assert!(
        stderr.contains("handover declined"),
        "the vanished prior claim must decline the handover: {stderr}"
    );
    assert!(
        stdout.contains("acquired: node:x-j1"),
        "the fallthrough must acquire fresh: {stdout}"
    );
    assert!(
        stderr.contains("manifest refreshed for node:x-j1"),
        "the fresh acquire must heal the manifest: {stderr}"
    );
    assert_manifest_unblocked(&manifest);
}

#[test]
fn suspect_prior_claim_adopts_as_handover_and_heals_manifest() {
    let sandbox = Sandbox::new("suspect");
    // The window claim's spawner died before the worker reached init: the
    // claim sits on disk anchored to a dead pid - the measured live shape.
    plant_launch_window_claim(&sandbox, waited_dead_pid());

    let manifest = sandbox.write_poisoned_manifest();

    let (rc, stdout, stderr) = run(
        &sandbox,
        &[
            "claim",
            "acquire",
            "node:x-j1",
            "--holder",
            SESSION_HOLDER,
            "--ttl",
            "30m",
            "--pid-unavailable",
            "--handover-from",
            HANDOVER_HOLDER,
            "--reason",
            "target dispatch",
        ],
    );
    assert_eq!(rc, 0, "acquire failed: {stdout} / {stderr}");
    assert!(
        !stderr.contains("handover declined"),
        "a suspect same-host prior claim must adopt, not decline: {stderr}"
    );
    assert!(
        stdout.contains("acquired node:x-j1 (handover from spawn-handover:worker-j1)"),
        "the adopt must answer the handover form: {stdout}"
    );
    assert_manifest_unblocked(&manifest);
}
