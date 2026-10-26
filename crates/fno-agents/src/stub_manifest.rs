//! The stub-manifest surface, ported from `cli/src/fno/stub_manifest.py`
//!: `fno do pr stub-manifest write|reconcile-validate|reconcile-finalize`.
//!
//! The manifest is the durable artifact a `contract`-tier dependent's first
//! pass emits so a later merge-triggered reconciliation can de-stub from it.
//! One op per CLI command; the write side door (fire the pending reconcile
//! sentinel) composes the same best-effort call the Python made.
//!
//! Exit contracts, pinned by `tests/golden/target_family/`:
//!
//! - `write`: 0 wrote (path on stdout), 1 malformed stubs JSON or manifest.
//! - `reconcile-validate`: 0 authorize/already-reconciled, 3 drift (suite
//!   failed or absent), 4 manifest-missing; one compact JSON verdict line.
//! - `reconcile-finalize`: 0 flipped, 4 no manifest, 1 malformed manifest.

use std::path::{Path, PathBuf};
use std::process::Command;

const STUB_REQUIRED: [&str; 3] = ["stub_id", "file", "kind"];

/// The `<root>/.fno/stub-manifest-<node>.json` path, displayed the way the
/// Python printed it: a "." root vanishes from the display form (the goldens
/// pin `.fno/stub-manifest-<node>.json`, never `./.fno/...`).
fn manifest_display(root: &str, node: &str) -> String {
    if root.is_empty() || root == "." {
        format!(".fno/stub-manifest-{node}.json")
    } else {
        format!("{root}/.fno/stub-manifest-{node}.json")
    }
}

fn manifest_path(root: &str, node: &str) -> PathBuf {
    Path::new(root)
        .join(".fno")
        .join(format!("stub-manifest-{node}.json"))
}

/// Python-style truthiness for the "is this key value present" check: an
/// explicit null counts as missing (str(None) would be the non-empty "None").
fn python_display(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "None".to_string(),
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

fn stub_missing(stub: &serde_json::Value, key: &str) -> bool {
    let value = stub.get(key);
    match value {
        None | Some(serde_json::Value::Null) => true,
        Some(v) => python_display(v).trim().is_empty(),
    }
}

/// Return Ok(()) or the message the Python StubManifestError carried. Checks
/// only the load-bearing shape: a `node` string and a list of stubs each
/// carrying the locators every de-stub pass needs.
fn validate(data: &serde_json::Value) -> Result<(), String> {
    let Some(obj) = data.as_object() else {
        return Err("manifest must be a JSON object".to_string());
    };
    let node = obj.get("node");
    let node_ok = matches!(node, Some(serde_json::Value::String(s)) if !s.trim().is_empty());
    if !node_ok {
        return Err("manifest 'node' must be a non-empty string".to_string());
    }
    let Some(stubs) = obj.get("stubs").and_then(|v| v.as_array()) else {
        return Err("manifest 'stubs' must be a list (empty is valid)".to_string());
    };
    for (i, stub) in stubs.iter().enumerate() {
        if !stub.is_object() {
            return Err(format!("stub #{} is not an object", i + 1));
        }
        let missing: Vec<&str> = STUB_REQUIRED
            .iter()
            .copied()
            .filter(|key| stub_missing(stub, key))
            .collect();
        if !missing.is_empty() {
            let who = match stub.get("stub_id") {
                None => "?".to_string(),
                Some(v) => python_display(v),
            };
            return Err(format!(
                "stub #{} ({}) missing: {}",
                i + 1,
                who,
                missing.join(", ")
            ));
        }
    }
    Ok(())
}

/// Read + validate. Err names absent ("absent") or malformed (the
/// StubManifestError text); an unreadable manifest must NOT escape as a raw
/// error - the merge guard's caller would swallow it and let a mocked PR
/// merge. Surface it as malformed so the guard holds.
fn load(path: &Path) -> Result<serde_json::Value, LoadError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(LoadError::Absent),
        Err(e) => {
            return Err(LoadError::Malformed(format!(
                "{}: unreadable ({e})",
                path.display()
            )))
        }
    };
    serde_json::from_str(&text)
        .map_err(|e| LoadError::Malformed(format!("{}: unreadable ({e})", path.display())))
        .and_then(|data| validate(&data).map(|_| data).map_err(LoadError::Malformed))
}

enum LoadError {
    Absent,
    Malformed(String),
}

/// The deadline Python's `_run_contract_test` bounded the subprocess with:
/// a contract test that waits on input or deadlocks must not strand the
/// draft PR and its reconciliation worker forever.
const CONTRACT_TEST_TIMEOUT_SECS: u64 = 600;

/// Run the blocker's contract-test command in the dependent's root. The
/// command is a trusted artifact written by the first-pass worker (same
/// trust boundary as a Makefile target), so it runs via the shell. Returns
/// (passed, "exit N: tail").
fn run_contract_test(cmd: &str, root: &str) -> (bool, String) {
    let mut child = match Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(root)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return (false, format!("contract-test did not complete: {e}")),
    };
    // Poll instead of `.output()`: a deadline needs try_wait, and the
    // kill-plus-drain on expiry mirrors subprocess.TimeoutExpired.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(CONTRACT_TEST_TIMEOUT_SECS);
    let output = loop {
        match child.try_wait() {
            Ok(Some(_)) => break child.wait_with_output().ok(),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (
                        false,
                        format!(
                            "contract-test did not complete: timed out after {}s",
                            CONTRACT_TEST_TIMEOUT_SECS
                        ),
                    );
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            Err(e) => return (false, format!("contract-test did not complete: {e}")),
        }
    };
    let output = match output {
        Some(out) => out,
        // The child was reaped by try_wait; wait_with_output only fails on a
        // stdin/stdout race this one-shot pipe setup cannot produce.
        None => return (false, "contract-test did not complete".to_string()),
    };
    let code = output.status.code().unwrap_or(-1);
    let raw = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        String::from_utf8_lossy(&output.stderr)
    };
    let trimmed = raw.trim();
    let skip = trimmed.chars().count().saturating_sub(300);
    let tail: String = trimmed.chars().skip(skip).collect();
    (code == 0, format!("exit {code}: {tail}"))
}

/// Decide whether the contract dependent may auto-de-stub. Pure read except
/// the contract-test subprocess; never mutates the manifest (finalize does
/// that). The four outcomes are the CLI exit-code contract.
fn reconcile_verdict(node: &str, root: &str, run_suite: bool) -> serde_json::Value {
    let display = manifest_display(root, node);
    if !Path::new(root)
        .join(".fno")
        .join(format!("stub-manifest-{node}.json"))
        .exists()
    {
        return serde_json::json!({
            "outcome": "manifest-missing",
            "node": node,
            "detail": format!("no manifest at {display}"),
            "stubs": 0,
        });
    }
    let path = manifest_path(root, node);
    let manifest = match load(&path) {
        Ok(m) => m,
        Err(LoadError::Malformed(detail)) => {
            return serde_json::json!({
                "outcome": "manifest-missing",
                "node": node,
                "detail": format!("manifest unusable: {detail}"),
                "stubs": 0,
            });
        }
        Err(LoadError::Absent) => serde_json::json!({
            "outcome": "manifest-missing",
            "node": node,
            "detail": format!("no manifest at {display}"),
            "stubs": 0,
        }),
    };
    let stubs = manifest
        .get("stubs")
        .and_then(|v| v.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    if manifest.get("reconciled") == Some(&serde_json::Value::Bool(true)) {
        return serde_json::json!({
            "outcome": "already-reconciled",
            "node": node,
            "detail": "manifest already reconciled",
            "stubs": stubs,
        });
    }
    let contract_test = manifest
        .get("contract_test")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(contract_test) = contract_test else {
        return serde_json::json!({
            "outcome": "drift",
            "node": node,
            "detail": "no contract-test suite in manifest; refusing auto-de-stub",
            "stubs": stubs,
        });
    };
    if !run_suite {
        return serde_json::json!({
            "outcome": "authorize",
            "node": node,
            "detail": "suite present (not executed: run_suite=False)",
            "stubs": stubs,
        });
    }
    let (passed, detail) = run_contract_test(contract_test, root);
    let outcome = if passed { "authorize" } else { "drift" };
    serde_json::json!({
        "outcome": outcome,
        "node": node,
        "detail": format!("contract-test {detail}"),
        "stubs": stubs,
    })
}

/// Flip the manifest's reconciled flag true, preserving every other field.
/// Called AFTER de-stubbing + tests pass; the manifest is retained (not
/// deleted) so "present and unreconciled" stays the single hold signal.
fn mark_reconciled(node: &str, root: &str) -> Result<PathBuf, LoadError> {
    let path = manifest_path(root, node);
    let mut manifest = load(&path)?;
    if let Some(obj) = manifest.as_object_mut() {
        obj.insert("reconciled".to_string(), serde_json::Value::Bool(true));
    }
    validate(&manifest).map_err(LoadError::Malformed)?;
    let written = serde_json::to_string_pretty(&manifest)
        .map_err(|e| LoadError::Malformed(format!("{}: unreadable ({e})", path.display())))?;
    std::fs::write(&path, written + "\n")
        .map_err(|e| LoadError::Malformed(format!("{}: unreadable ({e})", path.display())))?;
    Ok(path)
}

/// `stub-manifest write --node X ...`: validate and write the manifest,
/// then fire the pending reconcile sentinel best-effort. Exit 0/1.
fn run_write(args: &[String]) -> i32 {
    let mut node = String::new();
    let mut stubs_json = "[]".to_string();
    let mut contract_version: Option<i64> = None;
    let mut contract_ref: Option<String> = None;
    let mut contract_test: Option<String> = None;
    let mut root = ".".to_string();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--node" => node = it.next().cloned().unwrap_or_default(),
            "--stubs-json" => stubs_json = it.next().cloned().unwrap_or_default(),
            "--contract-version" => contract_version = it.next().and_then(|v| v.parse().ok()),
            "--contract-ref" => contract_ref = it.next().cloned(),
            "--contract-test" => contract_test = it.next().cloned(),
            "--root" => root = it.next().cloned().unwrap_or_else(|| ".".to_string()),
            _ => {}
        }
    }
    if node.is_empty() {
        eprintln!("stub-manifest: --node is required");
        return 1;
    }
    let stubs = match serde_json::from_str::<serde_json::Value>(&stubs_json) {
        Ok(serde_json::Value::Array(a)) => serde_json::Value::Array(a),
        Ok(_) => {
            eprintln!("stub-manifest: --stubs-json must be a JSON array");
            return 1;
        }
        Err(e) => {
            eprintln!("stub-manifest: {e}");
            return 1;
        }
    };
    let data = serde_json::json!({
        "node": node,
        "contract_version": contract_version,
        "contract_ref": contract_ref,
        "contract_test": contract_test,
        "reconciled": false,
        "stubs": stubs,
    });
    if let Err(e) = validate(&data) {
        eprintln!("stub-manifest: {e}");
        return 1;
    }
    let path = manifest_path(&root, &node);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let written = serde_json::to_string_pretty(&data).unwrap_or_default();
    if let Err(e) = std::fs::write(&path, written + "\n") {
        eprintln!("stub-manifest: {e}");
        return 1;
    }
    println!("{}", manifest_display(&root, &node));
    fire_pending_reconcile(&node, &root);
    0
}

/// The AC8 side door: a blocker that merged before the manifest existed left
/// a pending `reconcile:<node>` sentinel; fire it now, once. Best-effort and
/// non-fatal: a write must never fail because the re-fire stumbled.
fn fire_pending_reconcile(node: &str, root: &str) {
    let payload = serde_json::json!({
        "op": "fire-pending",
        "node_id": node,
        "root": root,
    });
    let fired = Command::new(crate::target_gates::self_exe())
        .arg("backlog")
        .arg("advance")
        .arg("--stdin")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn();
    let mut child = match fired {
        Ok(child) => child,
        Err(e) => {
            eprintln!("stub-manifest: pending-reconcile re-fire skipped: {e}");
            return;
        }
    };
    {
        use std::io::Write;
        let _ = child
            .stdin
            .as_mut()
            .and_then(|s| s.write_all(payload.to_string().as_bytes()).ok());
    }
    let out = match child.wait_with_output() {
        Ok(out) => out,
        Err(e) => {
            eprintln!("stub-manifest: pending-reconcile re-fire skipped: {e}");
            return;
        }
    };
    if !out.status.success() {
        eprintln!(
            "stub-manifest: pending-reconcile re-fire skipped: exit {}",
            out.status.code().unwrap_or(-1)
        );
        return;
    }
    let answer: Option<serde_json::Value> = serde_json::from_slice(&out.stdout).ok();
    let decision = answer
        .as_ref()
        .and_then(|a| a.get("decision").and_then(|d| d.as_str()));
    if decision == Some("scheduled") {
        eprintln!("reconcile scheduled for pending sentinel: {node}");
    }
}

/// `stub-manifest reconcile-validate --node X [--root R] [--no-run]`: the G4
/// drift gate. One compact JSON verdict line; exits 0/3/4.
fn run_reconcile_validate(args: &[String]) -> i32 {
    let (node, root, no_run) = match parse_node_root(args) {
        Some(parsed) => parsed,
        None => {
            eprintln!("stub-manifest: --node is required");
            return 1;
        }
    };
    let verdict = reconcile_verdict(&node, &root, !no_run);
    println!(
        "{}",
        serde_json::to_string(&verdict).unwrap_or_else(|_| "{}".to_string())
    );
    match verdict.get("outcome").and_then(|v| v.as_str()) {
        Some("authorize") | Some("already-reconciled") => 0,
        Some("drift") => 3,
        Some("manifest-missing") => 4,
        _ => 3,
    }
}

/// `stub-manifest reconcile-finalize --node X [--root R]`: flip the hold
/// signal off. Exits 0/4/1.
fn run_reconcile_finalize(args: &[String]) -> i32 {
    let (node, root, _) = match parse_node_root(args) {
        Some(parsed) => parsed,
        None => {
            eprintln!("stub-manifest: --node is required");
            return 1;
        }
    };
    match mark_reconciled(&node, &root) {
        Err(LoadError::Absent) => {
            eprintln!("stub-manifest: no manifest for {node} under {root}");
            4
        }
        Err(LoadError::Malformed(detail)) => {
            eprintln!("stub-manifest: cannot finalize a broken manifest: {detail}");
            1
        }
        Ok(_) => {
            let receipt = serde_json::json!({
                "node": node,
                "reconciled": true,
                "path": manifest_display(&root, &node),
            });
            println!("{}", serde_json::to_string(&receipt).unwrap_or_default());
            0
        }
    }
}

/// Shared `--node/--root/--no-run` flag parsing for the reconcile ops.
/// None names a missing required --node.
fn parse_node_root(args: &[String]) -> Option<(String, String, bool)> {
    let mut node = String::new();
    let mut root = ".".to_string();
    let mut no_run = false;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--node" => node = it.next().cloned().unwrap_or_default(),
            "--root" => root = it.next().cloned().unwrap_or_default(),
            "--no-run" => no_run = true,
            _ => {}
        }
    }
    if node.is_empty() {
        return None;
    }
    Some((node, root, no_run))
}

/// The op router: `fno-agents stub-manifest <op> [flags]`. Registered in
/// client.rs.
pub fn run(args: &[String]) -> i32 {
    let (op, rest) = match args.split_first() {
        Some((op, rest)) => (op.as_str(), rest),
        None => {
            eprintln!("fno-agents stub-manifest: name an op");
            return 2;
        }
    };
    // A --help that reaches the door prints the native usage line: the
    // Python group help died with the wheel, and a usage error instead of
    // help would strand the muscle-memory spelling.
    if rest.iter().any(|a| a == "--help" || a == "-h") {
        println!("usage: fno do pr stub-manifest write|reconcile-validate|reconcile-finalize --node <id> [--root <dir>] [--stubs-json <json>] [--contract-version <n>] [--contract-ref <ref>] [--contract-test <cmd>] [--no-run]");
        return 0;
    }
    match op {
        "write" => run_write(rest),
        "reconcile-validate" => run_reconcile_validate(rest),
        "reconcile-finalize" => run_reconcile_finalize(rest),
        other => {
            eprintln!("fno-agents stub-manifest: unknown op {other}");
            2
        }
    }
}
