//! parity-stage: characterization
//! parity-oracle: fno.claims.cli.status (the Python body, goldens captured
//! before the shim swap)
//!
//! Characterization harness for the `claim status` leaf port. The goldens
//! are captured from the Python leaf (`cli/src/fno/claims/cli.py::status`)
//! while it still runs its own body: a held claim on the human and JSON
//! surfaces, a never-held key, a dead-pid node claim with and without
//! `--no-roster` (the leg froze Python's verdict: the state reads
//! `suspect`/`pid-absent`, so the roster crosscheck - gated on
//! free-or-stale - never fires and stderr stays empty), a claim whose TTL
//! lapsed under a live pid (the leg froze `suspect`/
//! `ttl-expired-unresolved`, so the expiry clause never renders), a
//! corrupted store, the missing-argument and unknown-option usage errors,
//! and the bare-node-id normalization (the one frozen case that reaches
//! the roster crosscheck, against the empty stub roster). The bare-key
//! refusal is deliberately not a frozen case: it is Python-surface
//! behavior (see the colon-less-key test below), so the legs cannot and
//! must not agree on it.
//! Captured with `FNO_CAPTURE_GOLDEN=1` while the oracle still exists; in
//! normal mode the frozen bytes are the contract and the Python leg never
//! runs.
//!
//! `FNO_GLOBAL_SETTINGS_PATH=/dev/null` is pinned in every runner (both
//! legs and the plant): it makes the store resolve to the per-root local
//! database at `<root>/.fno/db/graph.db` and keeps node-keyed cases off any
//! configured remote, identically on a developer machine and in CI.
//!
//! Two more pins keep the frozen bytes machine-independent. Every runner
//! (both legs and the plant) runs under one synthetic session identity
//! (`identity_env`): a session id with no transcript on any host, so a
//! holder's liveness rests on its pid alone and no golden carries a real
//! session id. And every runner's PATH resolves `claude` to a stub whose
//! roster is empty (`roster_stub_path`): the vendor roster read shells out
//! to `claude agents --json --all`, so unpinned it would freeze this
//! machine's live fleet counts into the goldens; stubbed, both legs freeze
//! the same consulted-empty roster CI sees.
//!
//! Volatile values - pid, host, machine_id, acquired_at, expires_at - are
//! masked in place, keeping the payload's own key order, so a Python row
//! and a Rust row freeze to the same bytes when they match. The live plants
//! anchor to this test process's pid, which is alive on every host for the
//! duration of both legs; the stale plants anchor to a spawned-and-reaped
//! child pid, which is dead on every host.

use common::{assert_golden, Golden};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn pythonpath() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../cli/src")
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn python_executable() -> PathBuf {
    let venv = pythonpath().join("../.venv/bin/python");
    if venv.is_file() {
        venv
    } else {
        PathBuf::from("python3")
    }
}

fn binary_path() -> &'static str {
    env!("CARGO_BIN_EXE_fno-agents")
}

const PARITY_SESSION_ID: &str = "a1b2c3d4-1111-2222-3333-444455556666";

/// The synthetic session identity every runner shares (both legs and the
/// plant). The blanks close the same doors the pins open: a real session id
/// inherited from an outer harness would lead the transcript probe to THIS
/// session, freezing its identity and reading plants as live.
fn identity_env() -> [(&'static str, &'static str); 8] {
    [
        ("FNO_HARNESS_NAME", "claude"),
        ("FNO_HARNESS_SESSION_ID", PARITY_SESSION_ID),
        ("CODEX_THREAD_ID", ""),
        ("CLAUDE_CODE_SESSION_ID", ""),
        ("CODEX_SESSION_ID", ""),
        ("GEMINI_SESSION_ID", ""),
        ("OPENCODE_SESSION_ID", ""),
        ("CLAUDE_SESSION_ID", ""),
    ]
}

/// A PATH entry holding a `claude` stub whose roster is empty on every
/// host. The vendor roster read resolves `claude` from PATH; unpinned, a
/// developer machine freezes its live fleet's row counts into the goldens
/// and a CI machine (no claude at all) would read different bytes. Both
/// legs resolve the same stub, so the crosscheck freezes the
/// consulted-empty shape: zero scanned rows, no warnings, no riders.
fn roster_stub_path(root: &Path) -> PathBuf {
    let dir = root.join("bin-stub");
    let _ = std::fs::create_dir_all(&dir);
    let claude = dir.join("claude");
    std::fs::write(&claude, "#!/bin/sh\necho '{\"agents\": []}'\n").expect("write claude stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755))
            .expect("chmod claude stub");
    }
    dir
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

fn run_rust(root: &Path, args: &[&str]) -> Leg {
    let out = Command::new(binary_path())
        .args(["claim", "status"])
        .args(args)
        .env("FNO_AGENTS_BIN", binary_path())
        .env("PYTHONPATH", pythonpath())
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .current_dir(repo_root())
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .envs(identity_env())
        .env("PATH", roster_stub_path(root))
        .output()
        .expect("run fno-agents claim status");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: normalize(&String::from_utf8_lossy(&out.stdout)),
        stderr: normalize(&String::from_utf8_lossy(&out.stderr)),
    }
}

fn run_python(root: &Path, args: &[&str]) -> Leg {
    // The oracle runs behind an `fno` shim so the Usage line names the
    // production prog, not `python -m fno.cli`.
    let shim_dir = root.join("bin");
    let _ = std::fs::create_dir_all(&shim_dir);
    let shim = shim_dir.join("fno");
    // A shebang script, not `python -m`: typer reconstructs the prog name
    // from argv[0], so the oracle's Usage line must come from a real `fno`
    // basename to match production.
    let script = format!(
        "#!{}\nimport sys\nsys.path.insert(0, {:?})\nfrom fno.cli import app\napp()\n",
        python_executable().display(),
        pythonpath().display()
    );
    std::fs::write(&shim, script).expect("write fno shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
            .expect("chmod fno shim");
    }
    let out = Command::new("fno")
        .args(["agents", "claim", "status"])
        .args(args)
        .env("PYTHONPATH", pythonpath())
        .env("FNO_AGENTS_BIN", binary_path())
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .current_dir(repo_root())
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .envs(fno_agents::test_run::self_owner_env())
        .envs(identity_env())
        .env(
            "PATH",
            format!(
                "{}:{}",
                shim_dir.display(),
                roster_stub_path(root).display()
            ),
        )
        .output()
        .expect("run python fno agents claim status");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: normalize(&String::from_utf8_lossy(&out.stdout)),
        stderr: normalize(&String::from_utf8_lossy(&out.stderr)),
    }
}

const SUBJECT: &str = "claim_status";

fn run_raw(root: &Path, args: &[&str]) -> (i32, String, String) {
    let out = Command::new(binary_path())
        .args(["claim", "acquire"])
        .args(args)
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("FNO_GLOBAL_SETTINGS_PATH", "/dev/null")
        .envs(fno_agents::test_run::self_owner_env())
        .envs(identity_env())
        .env("PATH", roster_stub_path(root))
        .output()
        .expect("run setup acquire");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Plant state through the native binary - identical for both legs, so the
/// setup never carries a leg's own behavior.
fn plant(root: &Path, args: &[&str]) {
    let (code, _, stderr) = run_raw(root, args);
    assert_eq!(code, 0, "setup acquire failed: {stderr}");
}

/// A pid that is dead on every host: spawn a child and reap it.
fn reaped_pid() -> String {
    let mut child = Command::new("true")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn true for a dead pid");
    let pid = child.id();
    child.wait().expect("reap true");
    pid.to_string()
}

/// Overwrite the planted claim's expiry. The claims database lives at
/// `<root>/.fno/db/graph.db` under the settings pin.
fn set_expires_at(root: &Path, key: &str, expires_at: i64) {
    let db = root.join(".fno/db/graph.db");
    let conn = rusqlite::Connection::open(&db).expect("open claims db");
    let n = conn
        .execute(
            "UPDATE claims SET expires_at = ?1 WHERE key = ?2",
            rusqlite::params![expires_at, key],
        )
        .expect("update expires_at");
    assert_eq!(n, 1, "expected exactly one claim row for {key}");
}

// -------------------------------------------------------------------------
// Normalization: volatile fields masked in place, JSON kept in its own key
// order, so a Python row and a Rust row freeze to the same bytes.
// -------------------------------------------------------------------------

fn normalize(raw: &str) -> String {
    // The Python CLI bootstraps a fresh state root with a one-time path
    // migration banner on stderr; that is oracle-side setup noise, not leaf
    // behavior, so it is dropped before comparison.
    let raw = raw
        .lines()
        .filter(|line| {
            !line.starts_with("[setup] path migration complete")
                && !line.starts_with("  detected install:")
                && !line.starts_with("  settings written to:")
                && !line.starts_with("  obsidian enabled:")
                && !line.starts_with("  sentinel:")
        })
        .collect::<Vec<_>>()
        .join("\n");
    // The python events layer's store-client verb is skewed against the
    // rust front (a pre-existing break, filed separately); its best-effort
    // refusal banner is oracle-infra noise, not leaf contract, so the whole
    // embedded block is dropped before comparison.
    let raw = drop_event_store_noise(&raw);
    let trimmed = raw.trim_end_matches('\n');
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if v.is_object() {
            return canonical_json(&v);
        }
    }
    trimmed
        .lines()
        .map(scrub_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn drop_event_store_noise(raw: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in raw.lines() {
        if line.starts_with("claims: failed to emit '") {
            skipping = true;
            continue;
        }
        if skipping {
            // The banner embeds the refused subprocess's usage text; it ends
            // at the first line starting with the store's own Error prefix.
            if line.starts_with("Error: ") {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

fn scrub_line(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(pos) = rest.find("pid=").or_else(|| rest.find("host=")) {
        let (kind, placeholder) = if rest[pos..].starts_with("pid=") {
            ("pid=", "pid=PID")
        } else {
            ("host=", "host=HOST")
        };
        out.push_str(&rest[..pos]);
        out.push_str(placeholder);
        rest = &rest[pos + kind.len()..];
        let stop = rest
            .find(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .unwrap_or(rest.len());
        rest = &rest[stop..];
    }
    out.push_str(rest);
    out
}

fn canonical_json(v: &serde_json::Value) -> String {
    let obj = v.as_object().expect("checked object upstream");
    if !(obj.contains_key("schema_version") && obj.contains_key("holder")) {
        // Not a claim record (free and corrupted payloads, usage errors):
        // serialize as-is; both legs print the same native bytes there.
        return serde_json::to_string(v).unwrap();
    }
    // The status payload freezes in its own key order with the volatile
    // per-run values masked in place. pid, host, machine_id and the two
    // timestamps are the only fields that legitimately differ between the
    // legs: the plants run seconds apart on the same machine, and a live
    // plant anchors to this test process's pid.
    let masked = obj
        .iter()
        .map(|(k, val)| {
            let v = match k.as_str() {
                "pid" if !val.is_null() => serde_json::json!("PID"),
                "host" if !val.is_null() => serde_json::json!("HOST"),
                "machine_id" if !val.is_null() => serde_json::json!("MACHINE_ID"),
                "acquired_at" if !val.is_null() => serde_json::json!("ACQUIRED_AT"),
                "expires_at" if !val.is_null() => serde_json::json!("EXPIRES_AT"),
                _ => val.clone(),
            };
            (k.clone(), v)
        })
        .collect::<serde_json::Map<String, serde_json::Value>>();
    serde_json::to_string(&serde_json::Value::Object(masked)).unwrap()
}

fn case(label: &'static str, args: &[&str], setup: impl Fn(&Path)) {
    let rust_root = tempfile::tempdir().unwrap();
    setup(rust_root.path());
    let rust = run_rust(rust_root.path(), args);
    let oracle = common::capture_mode().then(|| {
        let py_root = tempfile::tempdir().unwrap();
        setup(py_root.path());
        run_python(py_root.path(), args)
    });
    assert_golden(SUBJECT, label, &rust.golden(), oracle.map(|o| o.golden()));
}

#[test]
fn the_status_leaf_matches_the_python_leg_on_every_frozen_case() {
    // 1-2. Held claim, human and JSON surfaces. No crosscheck (session key),
    // no expiry clause (TTL in the future).
    case("held_human", &["session:ref-live"], |root| {
        plant(
            root,
            &[
                "session:ref-live",
                "--holder",
                "pty:parity",
                "--pid",
                &std::process::id().to_string(),
                "--ttl",
                "30m",
            ],
        )
    });
    case("held_json", &["--json", "session:ref-json"], |root| {
        plant(
            root,
            &[
                "session:ref-json",
                "--holder",
                "pty:parity",
                "--pid",
                &std::process::id().to_string(),
                "--ttl",
                "30m",
            ],
        )
    });

    // 3. Never held: the two-field free payload.
    case("never_held", &["session:ref-none"], |_| {});

    // 4-5. Dead-pid node claim (reaped pid, dead on every host). The leg
    // froze Python's state machine: the claim reads suspect/pid-absent,
    // outside the crosscheck's free-or-stale gate, so no roster read and
    // no verdict line; --no-roster parses but changes nothing here.
    case("stale_node_verdict", &["node:ref-stale"], |root| {
        plant(
            root,
            &[
                "node:ref-stale",
                "--holder",
                "pty:parity",
                "--pid",
                &reaped_pid(),
                "--ttl",
                "30m",
            ],
        )
    });
    case(
        "stale_node_no_roster",
        &["node:ref-nr", "--no-roster"],
        |root| {
            plant(
                root,
                &[
                    "node:ref-nr",
                    "--holder",
                    "pty:parity",
                    "--pid",
                    &reaped_pid(),
                    "--ttl",
                    "30m",
                ],
            )
        },
    );

    // 6. Live pid whose TTL lapsed ~100 seconds ago. The leg froze
    // Python's verdict: the state reads suspect/ttl-expired-unresolved
    // rather than live, so the live-gated expiry clause never renders;
    // what the case pins is the expired flag and the basis wording.
    case("live_expired_clause", &["session:ref-exp"], |root| {
        let before = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        plant(
            root,
            &[
                "session:ref-exp",
                "--holder",
                "pty:parity",
                "--pid",
                &std::process::id().to_string(),
                "--ttl",
                "30m",
            ],
        );
        set_expires_at(root, "session:ref-exp", before - 100_000);
    });

    // 7. A corrupted claims database: the corrupted payload alone is the
    // answer (the transport swallows the engine's stderr), exit 0.
    case("corrupted_store", &["session:ref-bad"], |root| {
        let db = root.join(".fno/db/graph.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"this is not a database").unwrap();
    });

    // 8-9. The typer usage errors: missing KEY, unknown option.
    case("missing_key_arg", &[], |_| {});
    case("unknown_option", &["--bogus", "session:ref-x"], |_| {});

    // 10. A bare well-formed node id reads as the node key: free, with the
    // crosscheck verdict.
    case("bare_node_id_normalized", &["ab-1234"], |_| {});
}

/// The colon-less-key refusal is Python-surface behavior, not a binary
/// one: `_refuse_unknown_status_key` (cli.py) refuses on the dict the core
/// read returns, so it fires after the read on both the legacy body and
/// the post-swap forwarder, and `cli/tests/unit/test_claims_cli.py` pins
/// it there. The binary layer answers a colon-less key with the free
/// payload and exit 0 - exactly what the engine arm it replaces did -
/// which is what lets the Python refusal fire cleanly. Cross-layer exit
/// parity cannot hold here: the transport wraps any binary non-zero in a
/// ClaimVerdictError traceback, so a refusing binary would regress the
/// user surface from exit 2 to a traceback.
#[test]
fn the_binary_answers_a_colonless_key_with_the_free_payload() {
    let root = tempfile::tempdir().unwrap();
    let leg = run_rust(root.path(), &["unknownkey"]);
    assert_eq!(leg.exit, 0, "stdout: {} stderr: {}", leg.stdout, leg.stderr);
    let payload: serde_json::Value =
        serde_json::from_str(&leg.stdout).expect("free payload is a JSON object");
    assert_eq!(
        payload,
        serde_json::json!({"key": "unknownkey", "state": "free"})
    );
    assert!(leg.stderr.is_empty(), "stderr: {}", leg.stderr);
}
