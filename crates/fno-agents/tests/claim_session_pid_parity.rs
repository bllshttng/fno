//! parity-stage: characterization
//! parity-oracle: fno.claims.cli.session_pid (the retired Python body; the
//! surviving Python leaf is a pure binary-direct forward)
//!
//! Characterization harness for the `claim session-pid` leaf port (wave 7 of
//! the claims-port epic). The Python body is retired and the surviving
//! Python leaf is a pure forward, so there is no second leg to differ from:
//! capture freezes the native verb's own bytes, and the frozen goldens are
//! the contract. Every behavioral case pins `--from-pid` or an env stamp, so
//! no assertion depends on the test process's own ancestry (the default
//! start pid is getppid).
//!
//! Fake ancestors are real executables with argv[0] renamed through a bash
//! `exec -a` shim: census identity matches argv, not the exec path, and
//! copies of platform binaries get SIGKILLed at exec on macOS. The
//! pool-machinery fake renames /bin/sh (sleep itself rejects extra
//! arguments) and runs a two-command list so sh stays resident instead of
//! exec-optimizing into sleep, keeping the claude-named argv[0] and the
//! --bg-spare rider token visible.

use common::{assert_golden, Golden};
use std::path::Path;
use std::process::Command;

mod common;

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

/// One leg's observable outcome, already normalized.
struct Leg {
    exit: i32,
    stdout: String,
    stderr: String,
}

impl Leg {
    fn golden(&self) -> Golden {
        Golden {
            exit: Some(self.exit),
            streams: vec![self.stdout.clone(), self.stderr.clone()],
        }
    }
}

/// Volatile pids to <PID>, root paths to <ROOT>. Root replaced FIRST (a
/// tempdir path can carry digits); pid needles longest-first so a pid that
/// is a substring of another cannot half-replace.
fn normalize(raw: &str, root: &Path, pids: &[u32]) -> String {
    let raw = raw.replace(&root.display().to_string(), "<ROOT>");
    let mut needles: Vec<String> = pids.iter().map(|p| p.to_string()).collect();
    needles.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut out = raw;
    for pid in needles {
        out = out.replace(&pid, "<PID>");
    }
    out
}

fn run_leg(root: &Path, stamps: &[(&str, String)], args: &[&str], pids: &[u32]) -> Leg {
    let out = Command::new(binary_path())
        .args(["claim", "session-pid"])
        .args(args)
        .envs(fno_agents::test_run::self_owner_env())
        // Insurance against an ambient host stamp: stamp cases set their own,
        // fall-through cases need the env silent.
        .env("FNO_SESSION_PID", "")
        .env("FNO_SESSION_HARNESS", "")
        .envs(stamps.iter().map(|(k, v)| (*k, v.as_str())))
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .output()
        .expect("run fno-agents claim session-pid");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: normalize(&String::from_utf8_lossy(&out.stdout), root, pids),
        stderr: normalize(&String::from_utf8_lossy(&out.stderr), root, pids),
    }
}

/// The sleep binary the codex fake renames, for the /usr/bin split.
fn sleep_source() -> &'static str {
    ["/bin/sleep", "/usr/bin/sleep"]
        .into_iter()
        .find(|p| Path::new(p).is_file())
        .expect("no sleep binary found")
}

/// A spawned fake ancestor, killed and reaped when dropped.
struct KeptChild(std::process::Child);

impl KeptChild {
    /// A fake that died at exec leaves a census-visible zombie and freezes
    /// wrong goldens; refuse loudly instead.
    fn ensure_alive(&mut self, what: &str) {
        assert!(self.0.try_wait().unwrap().is_none(), "{what} died at exec");
    }
}

impl Drop for KeptChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// The verb reads a fresh census per invocation; settle until THIS process's
/// census names the child (5s budget, 25ms interval).
fn wait_in_census(pid: u32) {
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(5) {
        if fno_agents::spawn_context::ancestry_table().contains_key(&pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("fake ancestor {pid} never entered the census");
}

/// A pid the stamp path must refuse as dead: spawned and reaped.
fn reaped_pid() -> u32 {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg("exit 0")
        .spawn()
        .expect("spawn throwaway");
    let pid = child.id();
    child.wait().expect("reap throwaway");
    pid
}

const SUBJECT: &str = "claim_session_pid";

/// Fresh tempdir, the leg, freeze-or-compare. The self-oracle is the freeze
/// mechanism only: the Python body is retired, so capture has no second leg
/// to differ from; the frozen bytes then ARE the contract.
fn case(label: &'static str, stamps: &[(&str, String)], args: &[&str], pids: &[u32]) {
    let root = tempfile::tempdir().unwrap();
    let leg = run_leg(root.path(), stamps, args, pids);
    let oracle = common::capture_mode().then(|| leg.golden());
    assert_golden(SUBJECT, label, &leg.golden(), oracle);
}

#[test]
fn the_session_pid_leaf_matches_its_frozen_contract() {
    let self_pid = std::process::id();

    // 1-2. A live env stamp answers both halves (json) and the pid alone
    // (plain), whatever the ancestry says.
    case(
        "json_env_stamp_answers_both_halves",
        &[
            ("FNO_SESSION_PID", self_pid.to_string()),
            ("FNO_SESSION_HARNESS", "claude".into()),
        ],
        &["--json", "--from-pid", "1"],
        &[self_pid],
    );
    case(
        "plain_env_stamp_answers_pid",
        &[
            ("FNO_SESSION_PID", self_pid.to_string()),
            ("FNO_SESSION_HARNESS", "claude".into()),
        ],
        &["--from-pid", "1"],
        &[self_pid],
    );

    // 3. A stale stamp is refused as dead and the walk decides: from pid 1
    // there is no harness ancestor, so both halves answer null.
    case(
        "json_stale_stamp_falls_through",
        &[
            ("FNO_SESSION_PID", reaped_pid().to_string()),
            ("FNO_SESSION_HARNESS", "claude".into()),
        ],
        &["--json", "--from-pid", "1"],
        &[],
    );

    // 4-7. Fake ancestors, --from-pid pinned at the child so the walk's
    // first examined pid IS the fake. Machinery fake: sh renamed claude via
    // `exec -a`, carrying --bg-spare - the refusing walk refuses it (null
    // pid) while the plain walk still names claude. Codex fake: sleep
    // renamed codex - both walks answer codex + the child pid.
    let mut machinery = KeptChild(
        Command::new("bash")
            .args([
                "-c",
                "exec -a claude /bin/sh -c 'sleep 30 ; :' -- --bg-spare",
            ])
            .spawn()
            .expect("spawn machinery fake"),
    );
    machinery.ensure_alive("machinery fake");
    wait_in_census(machinery.0.id());
    let machinery_pid = machinery.0.id().to_string();
    case(
        "plain_degrades_on_pool_machinery",
        &[],
        &["--from-pid", &machinery_pid],
        &[machinery.0.id()],
    );
    case(
        "json_degrades_on_pool_machinery",
        &[],
        &["--json", "--from-pid", &machinery_pid],
        &[machinery.0.id()],
    );
    drop(machinery);

    let mut codex = KeptChild(
        Command::new("bash")
            .args(["-c", &format!("exec -a codex {} 30", sleep_source())])
            .spawn()
            .expect("spawn codex fake"),
    );
    codex.ensure_alive("codex fake");
    wait_in_census(codex.0.id());
    let codex_pid = codex.0.id().to_string();
    case(
        "json_names_a_live_ancestor",
        &[],
        &["--json", "--from-pid", &codex_pid],
        &[codex.0.id()],
    );
    case(
        "plain_names_a_live_ancestor",
        &[],
        &["--from-pid", &codex_pid],
        &[codex.0.id()],
    );
    drop(codex);

    // 8-9. Uncapturable: from pid 1 there is no harness ancestor, so the
    // plain surface prints NOTHING (the caller degrades to TTL-only
    // liveness) and json answers nulls. Always exit 0.
    case(
        "plain_uncapturable_is_empty",
        &[],
        &["--from-pid", "1"],
        &[],
    );
    case(
        "json_uncapturable_is_nulls",
        &[],
        &["--json", "--from-pid", "1"],
        &[],
    );

    // 10-12. The flag refusals: an unknown flag names itself (exit 2);
    // --from-pid without a value and a non-numeric value are silent exit 2.
    case("unknown_flag_refused", &[], &["--bogus"], &[]);
    case("from_pid_without_value_refused", &[], &["--from-pid"], &[]);
    case(
        "from_pid_non_numeric_refused",
        &[],
        &["--from-pid", "12x"],
        &[],
    );
}
