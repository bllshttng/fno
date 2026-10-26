//! The hermetic child env: one ambient inventory, `neutralise` and `poison`.
//!
//! Ported from the env-builder half of `cli/src/fno/hermetic.py`
//! (operator ruling: Plan A). The problem the module exists to solve is in that
//! file's docstring and `docs/architecture/test-hermeticity.md`; the short
//! form: CI runs with a clean HOME, so a test reading the developer's ambient
//! state passes in CI forever. Ambient state is therefore neutralised as a
//! CLASS (deny-by-default) and the survivors are named in one table.
//!
//! The vocabulary lives in `ambient_vocab.toml`, canonical beside this file
//! and byte-copied to `cli/src/fno/ambient_vocab.toml` by `build.rs` for the
//! Python `classify` service. The state-root fence (`declared_root`) stays
//! Python beside the events gate that shares it.
//!
//! Characterization: `crates/fno-agents/tests/test_command_parity.rs` asserts the
//! captured goldens under `tests/golden/test_command/`, taken from the Python leg
//! before its deletion.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

const VOCAB_TEXT: &str = include_str!("ambient_vocab.toml");

#[derive(Debug, serde::Deserialize)]
struct Vocab {
    prefixes: Vec<String>,
    identity_markers: Vec<String>,
    ambient_names: Vec<String>,
    environment: Vec<String>,
    runner_passthrough: Vec<String>,
    cache_names: Vec<String>,
    cache_defaults: BTreeMap<String, String>,
    xdg_sandboxed: Vec<String>,
    ambient_leak_canary: String,
}

fn vocab() -> &'static Vocab {
    static VOCAB: OnceLock<Vocab> = OnceLock::new();
    VOCAB.get_or_init(|| {
        toml::from_str(VOCAB_TEXT).expect("ambient_vocab.toml must parse beside this module")
    })
}

pub fn ambient_leak_canary() -> &'static str {
    &vocab().ambient_leak_canary
}

/// The capture specimen's source lists, in capture order (identity
/// markers + ambient names + environment + runner passthrough): the
/// golden test rebuilds the specimen from the same lists the Python
/// capture walked.
#[cfg(test)]
pub(crate) fn vocab_lists() -> Vec<Vec<String>> {
    let v = vocab();
    vec![
        v.identity_markers.clone(),
        v.ambient_names.clone(),
        v.environment.clone(),
        v.runner_passthrough.clone(),
    ]
}

/// `"ambient"`, `"environment"`, or `"unclassified"`. The Python
/// `fno.hermetic.classify` mirrors this from the copied vocabulary; the
/// ambient-surface test walks every env read in source and fails on an
/// `"unclassified"` result, so a new env var cannot join source without
/// someone deciding here whether a test may see the developer's value.
pub fn classify(name: &str) -> &'static str {
    let v = vocab();
    if v.runner_passthrough.iter().any(|n| n == name) || v.environment.iter().any(|n| n == name) {
        return "environment";
    }
    if is_ambient(name) {
        return "ambient";
    }
    "unclassified"
}

/// An explicit keep always wins over a prefix sweep, so a future TARGET_/FNO_
/// name the runner genuinely needs can be exempted in one place.
pub fn is_ambient(name: &str) -> bool {
    let v = vocab();
    if v.runner_passthrough.iter().any(|n| n == name) || v.environment.iter().any(|n| n == name) {
        return false;
    }
    if v.ambient_names.iter().any(|n| n == name) || v.identity_markers.iter().any(|n| n == name) {
        return true;
    }
    v.prefixes.iter().any(|p| name.starts_with(p.as_str()))
}

/// Names in `env` this module considers ambient. Sorted, for reporting.
pub fn ambient_names(env: &BTreeMap<String, String>) -> Vec<String> {
    env.keys().filter(|n| is_ambient(n)).cloned().collect()
}

/// The real home from the passwd database, NOT from `$HOME`.
///
/// Load-bearing for the lane invariant: the dirty lane poisons `$HOME`, so
/// deriving a cache default from the environment would make `CARGO_HOME`
/// differ between lanes and report a leak that is really just lane skew. The
/// passwd entry is the same in both lanes.
pub(crate) fn passwd_home() -> Option<PathBuf> {
    let mut buf = vec![0u8; 4096];
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    let dir = unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
        .to_string_lossy()
        .to_string();
    if dir.is_empty() {
        None
    } else {
        Some(PathBuf::from(dir))
    }
}

/// Cache locations resolved against the REAL home, before HOME is swapped.
/// An empty parent value falls through to the default exactly as the Python
/// `or` chain did.
fn real_caches(src: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let v = vocab();
    let home = passwd_home().unwrap_or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/"))
    });
    let mut out = BTreeMap::new();
    for name in &v.cache_names {
        let value = match src.get(name) {
            Some(val) if !val.is_empty() => val.clone(),
            _ => match v.cache_defaults.get(name) {
                Some(def) => home.join(def).to_string_lossy().to_string(),
                None => continue,
            },
        };
        if !value.is_empty() {
            out.insert(name.clone(), value);
        }
    }
    out
}

const GIT_IDENTITY: &str = "\
[user]
\tname = fno doctor test
\temail = fno-test@localhost
[init]
\tdefaultBranch = main
[commit]
\tgpgsign = false
";

/// A synthetic global gitconfig pinned to a WRITABLE sandbox file. `~/.gitconfig`
/// is an ambient channel HOME does not close: git resolves it before HOME in
/// some layouts, and nothing in the FNO_/TARGET_ sweep touches it.
fn git_pins(sandbox: &Path) -> BTreeMap<String, String> {
    let gitconfig = sandbox.join("gitconfig");
    if !gitconfig.exists() {
        let _ = std::fs::write(&gitconfig, GIT_IDENTITY);
    }
    BTreeMap::from([
        (
            "GIT_CONFIG_GLOBAL".to_string(),
            gitconfig.to_string_lossy().to_string(),
        ),
        ("GIT_CONFIG_SYSTEM".to_string(), "/dev/null".to_string()),
    ])
}

/// Return `env` with every ambient channel neutralised against `sandbox`.
///
/// Drops the ambient surface wholesale, then re-establishes the pins from
/// `sandbox` rather than from the parent, so a poisoned parent and a clean
/// parent produce the same result. That equality is the invariant the dirty
/// lane rests on. The caller owns the sandbox lifetime.
pub fn neutralise(
    env: &BTreeMap<String, String>,
    sandbox: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let v = vocab();
    let caches = real_caches(env);
    let mut out: BTreeMap<String, String> = env
        .iter()
        .filter(|(k, _)| !is_ambient(k))
        .map(|(k, val)| (k.clone(), val.clone()))
        .collect();

    let home = sandbox.join("home");
    std::fs::create_dir_all(home.join(".fno"))
        .map_err(|e| format!("neutralise: cannot create the sandbox state root: {e}"))?;

    // State: HOME (POSIX) and USERPROFILE (which Path.home() reads on Windows).
    out.insert("HOME".into(), home.to_string_lossy().to_string());
    out.insert("USERPROFILE".into(), home.to_string_lossy().to_string());
    // Cargo intermediates are pinned into the sandbox like HOME: the scrubbed
    // var would otherwise let cargo resolve its own default, and the tracked
    // .cargo/config.toml template lands that default in the REAL
    // ~/.cargo/build. All trees (pytest, shell, cargo) route through this
    // function, so all three build into the sandbox.
    out.insert(
        "CARGO_BUILD_BUILD_DIR".into(),
        format!(
            "{}/.fno/cargo-build/{{workspace-path-hash}}",
            home.to_string_lossy()
        ),
    );
    for name in &v.xdg_sandboxed {
        out.insert(
            name.clone(),
            sandbox
                .join("xdg")
                .join(name.to_lowercase())
                .to_string_lossy()
                .to_string(),
        );
    }

    // The canonical config climb is the part HOME cannot do: the candidate
    // chain climbs to the canonical checkout through `git worktree list`, and
    // git ignores HOME. Both loaders drop that candidate under this pin.
    out.insert("FNO_NO_CANONICAL_CONFIG".into(), "1".into());
    // TMPDIR is in the ceiling because a test's OWN config has to be findable:
    // shell harnesses build fixtures under `mktemp -d` and pytest builds
    // basetemp under the same root. The real checkout and real home stay
    // outside every entry, which is the point of the ceiling.
    let mut ceiling = vec![
        sandbox.to_string_lossy().to_string(),
        home.to_string_lossy().to_string(),
    ];
    let tmpdir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let mut tmp_candidates: Vec<String> = vec![tmpdir.clone()];
    if let Ok(canon) = std::fs::canonicalize(&tmpdir) {
        tmp_candidates.push(canon.to_string_lossy().to_string());
    }
    for candidate in tmp_candidates {
        if !ceiling.contains(&candidate) {
            ceiling.push(candidate);
        }
    }
    out.insert("FNO_CONFIG_SEARCH_ROOT".into(), ceiling.join(":"));

    // Env-gated side effects that would reach the host machine.
    out.insert("FNO_THINK_SPAWN".into(), "0".into());
    out.insert("FNO_SPAWN_GATE".into(), "0".into());
    out.insert("FNO_E2E".into(), "1".into());
    // The spawn seam publishes its writable-dir grant on the ambient env via
    // execv; in a test tree xdist reuses one process per worker, so a grant
    // leaks onto every later test in that worker. Scrub it like the markers.
    out.remove("FNO_WORKER_ADD_DIRS");
    // The operator needs panel folds the checkout journal; an unpathed
    // append_event under test would land a production-shaped row on a live
    // operator surface. Pin the journal itself. Rust shares the claim journal
    // AND its `.lock.d` mutex as a wire contract, so both sides honoring this
    // pin is what keeps the writers serialized on one file.
    out.insert(
        "FNO_EVENTS_PATH".into(),
        sandbox.join("events.jsonl").to_string_lossy().to_string(),
    );
    out.insert(
        "FNO_SPACES_DIR".into(),
        sandbox.join("spaces").to_string_lossy().to_string(),
    );

    out.extend(caches);
    out.extend(git_pins(sandbox));
    // Receipt: this env came through neutralise().
    out.insert("FNO_TEST_HERMETIC".into(), "1".into());
    Ok(out)
}

/// Return `env` with synthetic ambient state set, for the dirty lane.
///
/// Values are sentinels (`fno-poison-*`) rather than plausible-looking data: a
/// leak has to fail loudly, and a realistic value might quietly pass and teach
/// nobody anything. The profile is derived from the three 2026-08-11
/// specimens recorded in the Python module this ports.
pub fn poison(
    env: &BTreeMap<String, String>,
    fixtures: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let v = vocab();
    let mut out = env.clone();
    // Specimen 1: identity the command itself resolves.
    for marker in &v.identity_markers {
        out.insert(
            marker.clone(),
            format!("fno-poison-session-{}", marker.to_lowercase()),
        );
    }
    let fx = |rel: &str| fixtures.join(rel).to_string_lossy().to_string();
    out.insert("CLAUDE_PLUGIN_ROOT".into(), fx("poison-plugin-root"));
    out.insert("FNO_NODE".into(), "x-poison".into());
    out.insert("FNO_SLUG".into(), "fno-poison-slug".into());
    out.insert("FNO_PLAN".into(), fx("fno-poison-plan.md"));
    // Specimen 2: the config chain.
    out.insert("FNO_CONFIG".into(), fx("config.toml"));
    out.insert("FNO_GLOBAL_SETTINGS_PATH".into(), fx("settings.yaml"));
    out.remove("FNO_CONFIG_SEARCH_ROOT"); // unbounded, as a real dev box is
                                          // Specimen 3: the real state ledger.
    out.insert("HOME".into(), fx("home"));
    out.insert("USERPROFILE".into(), fx("home"));
    out.insert("FNO_REPO_ROOT".into(), fx("repo"));
    // The simulated missed channel: the canary the dirty lane must be able to
    // leak, and the clean lane must scrub nothing over. See the toml entry.
    out.insert(v.ambient_leak_canary.clone(), "fno-poison-canary".into());
    Ok(out)
}

/// Parse a `BTreeMap<String, String>` out of a JSON object payload value.
fn env_from_json(value: &Value) -> Result<BTreeMap<String, String>, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "test-env: the env payload must be a JSON object".to_string())?;
    let mut out = BTreeMap::new();
    for (k, v) in obj {
        let s = v
            .as_str()
            .ok_or_else(|| format!("test-env: env value for {k} must be a string"))?;
        out.insert(k.clone(), s.to_string());
    }
    Ok(out)
}

fn env_to_json(env: &BTreeMap<String, String>) -> Value {
    let mut obj = Map::new();
    for (k, v) in env {
        obj.insert(k.clone(), Value::String(v.clone()));
    }
    Value::Object(obj)
}

/// The `test-env` door: `fno-agents test-env '<json>'`.
///
/// One JSON payload: `{"mode": "neutralise"|"poison", "env": {...},
/// "sandbox": path, "fixtures": path?}`. Prints the resulting env as a JSON
/// object with sorted keys. Python's conftest and the hermetic-lane harness
/// read the boundary the native command itself uses, so a harness never
/// re-implements the env assembly.
pub fn run_test_env(args: &[String]) -> i32 {
    let payload: Value = match serde_json::from_str(args.first().map(String::as_str).unwrap_or(""))
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("test-env: payload is not JSON: {e}");
            return 2;
        }
    };
    let mode = payload.get("mode").and_then(Value::as_str).unwrap_or("");
    let env = match payload.get("env") {
        Some(v) => match env_from_json(v) {
            Ok(e) => e,
            Err(msg) => {
                eprintln!("{msg}");
                return 2;
            }
        },
        None => BTreeMap::new(),
    };
    let result = match mode {
        "neutralise" => match payload.get("sandbox").and_then(Value::as_str) {
            Some(sb) => neutralise(&env, Path::new(sb)),
            None => Err("test-env: neutralise needs a sandbox directory".to_string()),
        },
        "poison" => match payload.get("fixtures").and_then(Value::as_str) {
            Some(fx) => poison(&env, Path::new(fx)),
            None => Err("test-env: poison needs the ambient-poison fixture directory".to_string()),
        },
        other => Err(format!("test-env: unknown mode {other:?}")),
    };
    match result {
        Ok(env) => {
            println!("{}", json!({ "env": env_to_json(&env) }));
            0
        }
        Err(msg) => {
            eprintln!("{msg}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn classify_buckets_match_the_vocabulary() {
        assert_eq!(classify("CODEX_THREAD_ID"), "ambient");
        assert_eq!(classify("CLAUDECODE"), "ambient");
        assert_eq!(classify("FNO_FUTURE_VAR"), "ambient");
        assert_eq!(classify("TARGET_FUTURE_VAR"), "ambient");
        assert_eq!(classify("TOTALLY_UNKNOWN_VAR"), "unclassified");
        assert_eq!(classify(ambient_leak_canary()), "unclassified");
        assert_eq!(classify("PATH"), "environment");
        assert_eq!(classify("FNO_AGENTS_BIN"), "environment");
    }

    #[test]
    fn keep_wins_over_the_prefix_sweep() {
        // An explicit keep beats the FNO_ sweep, so a runner-needed FNO_ name
        // is exempted in one place.
        assert!(!is_ambient("FNO_PYTEST_SHARD"));
        assert!(is_ambient("FNO_RANDOM_NEW_THING"));
    }

    #[test]
    fn neutralise_pins_state_into_the_sandbox_and_drops_the_surface() {
        let tmp = tempfile::tempdir().unwrap();
        let sandbox = tmp.path().join("sb");
        let parent = env(&[
            ("PATH", "/bin:/usr/bin"),
            ("FNO_NODE", "x-live"),
            ("ANTHROPIC_API_KEY", "sk-live"),
            ("HOME", "/Users/someone"),
        ]);
        let out = neutralise(&parent, &sandbox).unwrap();
        assert!(!out.contains_key("FNO_NODE"));
        assert!(!out.contains_key("ANTHROPIC_API_KEY"));
        assert_eq!(out.get("PATH").unwrap(), "/bin:/usr/bin");
        let home = sandbox.join("home");
        assert_eq!(out.get("HOME").unwrap(), home.to_string_lossy());
        assert_eq!(
            out.get("CARGO_BUILD_BUILD_DIR").unwrap(),
            format!(
                "{}/.fno/cargo-build/{{workspace-path-hash}}",
                home.to_string_lossy()
            )
        );
        assert_eq!(out.get("FNO_TEST_HERMETIC").unwrap(), "1");
        assert_eq!(out.get("FNO_THINK_SPAWN").unwrap(), "0");
        assert_eq!(out.get("FNO_SPAWN_GATE").unwrap(), "0");
        assert_eq!(out.get("FNO_E2E").unwrap(), "1");
        assert_eq!(out.get("FNO_NO_CANONICAL_CONFIG").unwrap(), "1");
        // The gitconfig pin is written into the sandbox with the synthetic
        // identity.
        let gitconfig = std::fs::read_to_string(sandbox.join("gitconfig")).unwrap();
        assert!(gitconfig.contains("name = fno doctor test"));
        assert_eq!(
            out.get("GIT_CONFIG_SYSTEM").unwrap(),
            "/dev/null",
            "system git config is refused, not redirected"
        );
    }

    #[test]
    fn neutralise_is_lane_stable_over_a_poisoned_parent() {
        // The invariant the dirty lane rests on: a poisoned parent and a clean
        // parent produce the same result.
        let tmp = tempfile::tempdir().unwrap();
        let fixtures = tmp.path.join("fx");
        let sandbox = tmp.path.join("sb");
        let clean = env(&[("PATH", "/bin"), ("HOME", "/Users/someone")]);
        let poisoned = poison(&clean, &fixtures).unwrap();
        let a = neutralise(&clean, &sandbox).unwrap();
        let b = neutralise(&poisoned, &sandbox).unwrap();
        let a_keys: Vec<_> = a.keys().collect();
        let b_keys: Vec<_> = b.keys().collect();
        assert_eq!(a_keys, b_keys);
        for k in a_keys {
            assert_eq!(a[&k], b[&k], "lane skew on {k}");
        }
    }

    #[test]
    fn poison_sets_the_specimen_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let fixtures = tmp.path.join("fx");
        let clean = env(&[("PATH", "/bin")]);
        let out = poison(&clean, &fixtures).unwrap();
        assert_eq!(
            out.get("CODEX_THREAD_ID").unwrap(),
            "fno-poison-session-codex_thread_id"
        );
        assert_eq!(
            out.get("CLAUDE_PLUGIN_ROOT").unwrap(),
            fixtures.join("poison-plugin-root").to_string_lossy()
        );
        assert_eq!(out.get("FNO_NODE").unwrap(), "x-poison");
        assert_eq!(
            out.get(ambient_leak_canary()).unwrap(),
            "fno-poison-canary",
            "the missed channel must leak for the dirty lane's positive control"
        );
    }

    #[test]
    fn cache_defaults_resolve_against_the_passwd_home_not_the_env_home() {
        let tmp = tempfile::tempdir().unwrap();
        let sandbox = tmp.path.join("sb");
        let parent = env(&[("HOME", "/Users/someone")]);
        let out = neutralise(&parent, &sandbox).unwrap();
        let expected = passwd_home().unwrap().join(".rustup");
        assert_eq!(
            out.get("RUSTUP_HOME").unwrap(),
            expected.to_string_lossy(),
            "an unset cache resolves from the passwd entry, never from $HOME"
        );
    }
}
