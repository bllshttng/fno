//! parity-stage: characterization
//! parity-oracle: fno.claims.cli.list_cmd (the Python body until the shim
//! swap in this same wave)
//!
//! Characterization harness for the `claim list` leaf port. The goldens are
//! captured from the Python leaf (`cli/src/fno/claims/cli.py::list_cmd`)
//! while it still runs its own body: the empty message, the live human and
//! JSON rows, stale hidden/listed/JSON-included, the prefix filter, an
//! explicit root, the silence report (JSON + human + unreadable), and the
//! --silent-after refusals. Captured with `FNO_CAPTURE_GOLDEN=1` while the
//! oracle still exists; in normal mode the frozen bytes are the contract and
//! the Python leg never runs.
//!
//! Both legs run in a pinned state dir (`FNO_CLAIMS_ROOT`/`FNO_STATE_DIR`)
//! with `FNO_AGENTS_HOME` empty-of-registry. The silence cases additionally
//! pin HOME and CLAUDE_DIR_OVERRIDE into the temp root so neither leg reads
//! the real transcript store. Ages move between runs, so every age field
//! (`"age_s"` in JSON, `age=<n>m` in the human report) scrubs to a
//! placeholder; everything else freezes byte-exact.

use common::{assert_golden, Golden};
use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

type ExtraEnv = Vec<(&'static str, String)>;

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

fn base_env() -> Vec<(&'static str, String)> {
    vec![
        ("PYTHONPATH", pythonpath().display().to_string()),
        ("FNO_AGENTS_BIN", binary_path().to_string()),
        ("FNO_HARNESS_NAME", "claude".into()),
        (
            "FNO_HARNESS_SESSION_ID",
            "a1b2c3d4-1111-2222-3333-444455556666".into(),
        ),
        ("CODEX_THREAD_ID", "".into()),
        ("CLAUDE_CODE_SESSION_ID", "".into()),
        ("CODEX_SESSION_ID", "".into()),
        ("GEMINI_SESSION_ID", "".into()),
        ("OPENCODE_SESSION_ID", "".into()),
        ("CLAUDE_SESSION_ID", "".into()),
        ("CLAUDECODE", "".into()),
    ]
}

fn run_rust(root: &Path, args: &[String], extra_env: &ExtraEnv) -> Leg {
    let out = Command::new(binary_path())
        .args(["claim", "list"])
        .args(args)
        .envs(base_env())
        .envs(fno_agents::test_run::self_owner_env())
        .envs(extra_env.iter().map(|(k, v)| (*k, v.as_str())))
        .current_dir(repo_root())
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .output()
        .expect("run fno-agents claim list");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: normalize(&String::from_utf8_lossy(&out.stdout), root),
        stderr: normalize(&String::from_utf8_lossy(&out.stderr), root),
    }
}

fn run_python(root: &Path, args: &[String], extra_env: &ExtraEnv) -> Leg {
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
    let path_var = std::env::var("PATH").unwrap_or_default();
    // Pin FNO_BIN to the host's own `fno` resolved BEFORE the shim prepend,
    // so any oracle-side subprocess resolves the production substrate.
    let host_fno = String::from_utf8_lossy(
        &Command::new("which")
            .arg("fno")
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default(),
    )
    .trim()
    .to_string();
    let out = Command::new("fno")
        .args(["agents", "claim", "list"])
        .args(args)
        .envs(base_env())
        .envs(fno_agents::test_run::self_owner_env())
        .envs(extra_env.iter().map(|(k, v)| (*k, v.as_str())))
        .current_dir(repo_root())
        .env("FNO_CLAIMS_ROOT", root)
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env("PATH", format!("{}:{}", shim_dir.display(), path_var))
        .env("FNO_BIN", host_fno)
        .output()
        .expect("run python fno agents claim list");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: normalize(&String::from_utf8_lossy(&out.stdout), root),
        stderr: normalize(&String::from_utf8_lossy(&out.stderr), root),
    }
}

const SUBJECT: &str = "claim_list";

/// Plant a claim FILE directly under `dir_parent`'s `.fno/claims`, with the
/// holder and trailing metadata exposed so the silence cases can plant a
/// target-session holder plus a worktree path.
fn plant_lock(dir_parent: &Path, key: &str, holder: &str, expires_at: &str, metadata: &str) {
    plant_lock_in(
        &dir_parent.join(".fno/claims"),
        key,
        holder,
        expires_at,
        metadata,
    )
}

/// Same, but naming the claims dir itself: the two-roots case plants into
/// the resolved space store, whose parent is not the root.
fn plant_lock_in(dir: &Path, key: &str, holder: &str, expires_at: &str, metadata: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let encoded = key.replace(':', "%3A");
    std::fs::write(
        dir.join(format!("{encoded}.lock")),
        format!(
            "schema_version: 1\nkey: {key}\nholder: {holder}\n\
             acquired_at: 1791138000000\npid: 4242\nhost: testhost\n\
             expires_at: {expires_at}\nharness: claude\nsession_id: sess-file\n\
             pid_provenance: ambient\nmachine_id: MACH\n{metadata}"
        ),
    )
    .unwrap();
}

fn plant_file(root: &Path, key: &str, expires_at: &str) {
    plant_lock(root, key, "pty:par", expires_at, "");
}

/// The same file-plant with a FUTURE expires_at: the live shape.
fn plant_live_file(root: &Path, key: &str) {
    plant_file(root, key, "1891138000000");
}

/// One live claim whose holder names a session and whose metadata carries
/// the worktree, plus a transcript for that session under the pinned HOME:
/// a September 1 timestamp, older than any sane threshold, so the row lands
/// in the silent bucket on both legs deterministically.
fn plant_silent_claim(root: &Path) {
    let worktree = root.display().to_string();
    plant_lock(
        root,
        "session:sil-1",
        "target-session:sil-1",
        "1891138000000",
        &format!("metadata:\n  worktree: {worktree}\n"),
    );
    let project = root.join(".claude/projects/proj");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::write(
        project.join("sil-1.jsonl"),
        "{\"type\":\"user\",\"timestamp\":\"2026-09-01T00:00:00Z\"}\n",
    )
    .unwrap();
}

/// Pin the transcript store into the temp root: HOME for the Python side's
/// default resolution, CLAUDE_DIR_OVERRIDE for the Rust roster.
fn silence_env(root: &Path) -> ExtraEnv {
    vec![
        ("HOME", root.display().to_string()),
        (
            "CLAUDE_DIR_OVERRIDE",
            root.join(".claude").display().to_string(),
        ),
    ]
}

// -------------------------------------------------------------------------
// Normalization: volatile ages to placeholders, JSON to a canonical key
// order, so a Python row and a Rust row freeze to the same bytes.
// -------------------------------------------------------------------------

fn normalize(raw: &str, root: &Path) -> String {
    // Every path under the pinned root differs between the two legs'
    // tempdirs; the root itself is the only variable part.
    let root_str = root.display().to_string();
    let raw = raw.replace(&root_str, "<ROOT>");
    // Without cli/.venv the python shim's fallback shebang cannot exec and
    // the host launcher serves the oracle as the `fno-py` console script, so
    // typer names it in the Usage lines. The prog name is launcher noise,
    // not leaf behavior; the rust port prints the production `fno`. Applied
    // to both legs, so a venv machine (which reports `fno` natively) is a
    // no-op away from the same goldens.
    let raw = raw.replace("fno-py", "fno");
    // Ages move between runs; the FACT of an age is the contract, the
    // wall-clock digits are not.
    let raw = scrub_ages(&raw);
    // The python events layer's store-client command is skewed against the
    // rust front (a pre-existing break, filed separately); its best-effort
    // refusal banner is oracle-infra noise, not leaf contract, so the whole
    // embedded block is dropped before comparison.
    let raw = drop_event_store_noise(&raw);
    // The python CLI bootstraps a fresh state root with a one-time path
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
    // The HOME-pinned silence cases send the python leg's launcher through
    // its one-time uv provisioning under the temp HOME (banner, resolve and
    // download progress, the installed-executables line, the PATH warning);
    // oracle-infra noise, not leaf behavior, so the whole range drops.
    let raw = drop_first_run_provisioning(&raw);
    let trimmed = raw.trim_end_matches('\n');
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        if v.is_object() || v.is_array() {
            return canonical_json(&v);
        }
    }
    trimmed.to_string()
}

/// `age_s": <float>` (inside the silence JSON) and `age=<float>m` (the human
/// report) become placeholders. The JSON form keeps quotes around the
/// placeholder so the blob stays parseable and the canonicalization pass
/// still reorders its keys. Words that merely contain "age" ("message")
/// fall through every branch verbatim.
fn scrub_ages(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(pos) = rest.find("age") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + "age".len()..];
        if let Some(v) = after.strip_prefix("_s") {
            if let Some(v) = v.strip_prefix("\":") {
                let trimmed = v.trim_start();
                let f = float_prefix(trimmed);
                if !f.is_empty() {
                    out.push_str("\"age_s\": \"<AGE>\"");
                    rest = &v[v.len() - trimmed.len() + f.len()..];
                    continue;
                }
            }
            out.push_str("age_s");
            rest = after;
            continue;
        }
        if let Some(v) = after.strip_prefix('=') {
            let f = float_prefix(v);
            if !f.is_empty() && v[f.len()..].starts_with('m') {
                out.push_str("age=<AGE>m");
                rest = &v[f.len() + 1..];
                continue;
            }
        }
        out.push_str("age");
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The leading run of a JSON/Python float literal: sign, integer digits,
/// and a fraction only when the dot has at least one digit after it.
fn float_prefix(s: &str) -> &str {
    let off = usize::from(s.starts_with('-'));
    let rest = &s[off..];
    let int_end = rest
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    if int_end == 0 {
        return "";
    }
    let mut end = off + int_end;
    if let Some(frac) = rest[int_end..].strip_prefix('.') {
        let frac_end = frac
            .char_indices()
            .find(|(_, c)| !c.is_ascii_digit())
            .map(|(i, _)| i)
            .unwrap_or(frac.len());
        if frac_end > 0 {
            end += 1 + frac_end;
        }
    }
    &s[..end]
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

fn canonical_json(v: &serde_json::Value) -> String {
    serde_json::to_string(v).unwrap()
}

/// Drop the launcher's first-run uv provisioning block: from the banner line
/// through the `fno: verified fno` line, inclusive.
fn drop_first_run_provisioning(raw: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in raw.lines() {
        if line.starts_with("fno: first run - provisioning") {
            skipping = true;
            continue;
        }
        if skipping {
            if line.starts_with("fno: verified fno") {
                skipping = false;
            }
            continue;
        }
        out.push(line);
    }
    out.join("\n")
}

fn case_ext(
    label: &'static str,
    args: &[&str],
    extra_env: impl Fn(&Path) -> ExtraEnv,
    setup: impl Fn(&Path),
    extra_args: impl Fn(&Path) -> Vec<String>,
) {
    let rust_root = tempfile::tempdir().unwrap();
    setup(rust_root.path());
    let env = extra_env(rust_root.path());
    let mut argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    argv.extend(extra_args(rust_root.path()));
    let rust = run_rust(rust_root.path(), &argv, &env);
    let oracle = common::capture_mode().then(|| {
        let py_root = tempfile::tempdir().unwrap();
        setup(py_root.path());
        let py_env = extra_env(py_root.path());
        let mut py_argv: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        py_argv.extend(extra_args(py_root.path()));
        run_python(py_root.path(), &py_argv, &py_env)
    });
    assert_golden(SUBJECT, label, &rust.golden(), oracle.map(|o| o.golden()));
}

fn case(label: &'static str, args: &[&str], setup: impl Fn(&Path)) {
    case_ext(label, args, |_| Vec::new(), setup, |_| Vec::new());
}

#[test]
fn the_list_leaf_matches_the_python_leg_on_every_frozen_case() {
    // 1. Empty store: the named no-live-claims message with the one-root
    // count.
    case("empty_store", &[], |_| {});

    // 2-3. One live claim: the human row and the JSON row.
    case("single_live_human", &[], |root| {
        plant_live_file(root, "session:live-1")
    });
    case("single_live_json", &["--json"], |root| {
        plant_live_file(root, "session:live-2")
    });

    // 4-6. Stale rows: hidden by default (the empty message carries the
    // include-stale hint), listed with the flag, and included in JSON.
    case("stale_hidden_by_default", &[], |root| {
        plant_file(root, "session:gone-1", "1591138000000")
    });
    case("stale_listed_with_flag", &["--include-stale"], |root| {
        plant_file(root, "session:gone-2", "1591138000000")
    });
    case(
        "stale_json_included",
        &["--json", "--include-stale"],
        |root| plant_file(root, "session:gone-3", "1591138000000"),
    );

    // 7. The prefix filter narrows the listing.
    case("prefix_filter", &["--prefix", "session:a"], |root| {
        plant_live_file(root, "session:aa-1");
        plant_live_file(root, "session:bb-1");
    });

    // 8. An explicit --root names the one store.
    case_ext(
        "explicit_root",
        &[],
        |_| Vec::new(),
        |root| {
            plant_lock(
                &root.join("alt"),
                "session:exp-1",
                "pty:par",
                "1891138000000",
                "",
            )
        },
        |root| vec!["--root".to_string(), root.join("alt").display().to_string()],
    );

    // 9-10. The silence report: JSON and human, over one planted silent
    // claim whose transcript sits in the pinned HOME.
    case_ext(
        "silence_json",
        &["--json", "--silent-after", "1h"],
        silence_env,
        plant_silent_claim,
        |_| Vec::new(),
    );
    case_ext(
        "silence_human",
        &["--silent-after", "1h"],
        silence_env,
        plant_silent_claim,
        |_| Vec::new(),
    );

    // 11. The silence scan with no HOME pin: the planted pty: holder is
    // offhost-suspect by fixture, so no row is live and none is scanned.
    case(
        "silence_unreadable",
        &["--json", "--silent-after", "1h"],
        |root| plant_live_file(root, "session:sil-u"),
    );

    // 12-14. The --silent-after refusals: missing value, empty, and a bad
    // format.
    case("silent_after_missing_value", &["--silent-after"], |_| {});
    case("silent_after_empty", &["--silent-after", ""], |_| {});
    case(
        "silent_after_bad_format",
        &["--silent-after", "abc"],
        |_| {},
    );
}

/// The bare form (no --root, no FNO_CLAIMS_ROOT): the leaf scans the
/// HOME-resolved global store and the cwd-resolved store, so both pins
/// ride the child directly.
fn run_rust_bare_form(root: &Path, args: &[String]) -> Leg {
    let out = Command::new(binary_path())
        .args(["claim", "list"])
        .args(args)
        .envs(base_env())
        .envs(fno_agents::test_run::self_owner_env())
        .current_dir(root)
        .env("HOME", root)
        .env("FNO_SPACES_DIR", root.join("spaces"))
        .env("FNO_STATE_DIR", root)
        .env("FNO_AGENTS_HOME", root.join("agents-home"))
        .env("FNO_EVENTS_PATH", root.join("events.jsonl"))
        .env_remove("FNO_CLAIMS_ROOT")
        .output()
        .expect("run rust claim list in bare form");
    Leg {
        exit: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
    }
}

/// The merge contract the goldens cannot reach: one key sighted in two
/// roots. The global store (HOME-resolved) holds a stale sighting and the
/// space store (cwd-resolved) holds a live one. The live sighting wins, the
/// surviving row names the winning sighting's root, and the key lists
/// exactly once.
#[test]
fn two_roots_one_key_live_in_second_root_wins() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path();
    // The global store: HOME == root makes global_claims_root land here.
    plant_lock(root, "node:tworoots", "pty:old", "1591138000000", "");
    // The space store: resolve it with the same pins the child gets. The
    // env pin is set and removed around the call (edition 2021).
    let spaces = root.join("spaces");
    std::env::set_var("FNO_SPACES_DIR", &spaces);
    let space_claims = fno_agents::paths::space_dir(root).join("claims");
    std::env::remove_var("FNO_SPACES_DIR");
    plant_lock_in(
        &space_claims,
        "node:tworoots",
        "pty:new",
        "1891138000000",
        "",
    );

    let leg = run_rust_bare_form(root, &[]);

    let rows: Vec<&str> = leg
        .stdout
        .lines()
        .filter(|l| l.contains("node:tworoots"))
        .collect();
    assert_eq!(rows.len(), 1, "exactly one merged row, got: {:?}", rows);
    let global_claims = root.join(".fno/claims");
    assert!(
        !leg.stdout.contains(&global_claims.display().to_string()),
        "the losing root leaks into the row: {}",
        leg.stdout
    );
    let root_label = format!("root={}", space_claims.display());
    // The winner is unexpired but offhost by fixture (host=testhost), so it
    // renders "suspect", the same basis as every planted lock in this file;
    // "stale" would mean the expired global sighting won.
    assert!(
        rows[0].starts_with("suspect"),
        "the unexpired sighting wins: {}",
        rows[0]
    );
    assert!(
        rows[0].contains(&root_label),
        "the row names the winning sighting's root: {}\nwant: {}",
        rows[0],
        root_label
    );
}
