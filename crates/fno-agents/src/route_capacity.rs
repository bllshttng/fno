//! The lane-capacity read the slot walk judges lanes with, and the one
//! refresh that un-stales an outdated reading before the walk judges.
//!
//! Port of the Python `route_resolve.runtime_capacity` + `runtime_state.headrooms`
//! (both deleted there, law d-b6cc1a2a): one headroom implementation lives in
//! `fallback_chain::headroom`, one state-file reader here, and one probe path
//! through `accounts::usage::refreshed`, the `usage --refresh --json` report.
//! The refresh is fail-open: a failed probe leaves the readings as they were
//! and the walk judges on the stale evidence, naming it in the chain.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::accounts::state::{parse_health, parse_usage};
use crate::fallback_chain::headroom;

/// The harnesses the Python default named, plus every inventory row's harness.
pub(crate) const DEFAULT_PROVIDERS: [&str; 4] = ["claude", "codex", "gemini", "opencode"];

/// How long the one refresh subprocess may run: the command probes every
/// account record, each network probe bounded at 10s, so the walk pays this
/// at most once per call.
pub(crate) const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The provider runtime-state payload at `provider_cap::runtime_state_path`.
/// An unreadable file is an empty payload (unlocked), matching the Python
/// reader's None arm.
pub(crate) fn runtime_state_payload(config_cwd: &Path) -> Value {
    crate::accounts::state::read_raw(&crate::provider_cap::runtime_state_path(config_cwd))
        .unwrap_or(Value::Null)
}

fn default_state_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".fno")
}

/// Root of the managed store the identity stamps live under:
/// `<state_dir>/providers`, matching `managed.store_root`.
fn providers_root(cwd: &Path) -> PathBuf {
    crate::agents_config::state_dir(cwd)
        .unwrap_or_else(default_state_dir)
        .join("providers")
}

/// The ACCOUNT record ids a harness can speak for: inventory-row accounts
/// first, then `[[accounts.records]]` bound to it, de-duplicated in that
/// order. A row's `route` names a vendor, never an account id, and
/// contributes nothing (the Python row.accounts() rule verbatim).
fn harness_accounts(harness: &str, inventory: &Value, records: &[toml::Value]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |a: &str| {
        let a = a.trim();
        if !a.is_empty() && !out.iter().any(|x| x == a) {
            out.push(a.to_string());
        }
    };
    if let Some(rows) = inventory.get("rows").and_then(Value::as_array) {
        for row in rows {
            if row.get("harness").and_then(Value::as_str) != Some(harness) {
                continue;
            }
            if let Some(a) = row.get("account").and_then(Value::as_str) {
                push(a);
            }
        }
    }
    for rec in records {
        let bound = rec
            .get("harness")
            .or_else(|| rec.get("cli"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if bound != harness {
            continue;
        }
        if let Some(id) = rec.get("id").and_then(|v| v.as_str()) {
            push(id);
        }
    }
    out
}

/// `proven` for the stamped account, `mismatch` for the others; empty when
/// the stamp is missing, unreadable, or tainted (`_identity_evidence`): the
/// attribution owner alone answers, never credentials.
fn identity_evidence(harness: &str, accounts: &[String], root: &Path) -> Map<String, Value> {
    let mut out = Map::new();
    let active = crate::accounts::slot::active_slot_id(harness, Some(root), root)
        .ok()
        .flatten();
    let Some(active) = active.filter(|_| !crate::accounts::store::slot_tainted(root, harness))
    else {
        return out;
    };
    for a in accounts {
        out.insert(
            a.clone(),
            json!(if *a == active { "proven" } else { "mismatch" }),
        );
    }
    out
}

/// Probe every account once, in process, and return the `usage --refresh
/// --json` map. Returns None on an unreadable config or past `timeout`:
/// fail-open, the readings stay as they were. A probe still running at the
/// deadline finishes on its thread; its cache write is atomic.
#[cfg(not(test))]
pub(crate) fn refresh_usage_readings(
    cwd: &Path,
    timeout: std::time::Duration,
) -> Option<Map<String, Value>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let cwd = cwd.to_path_buf();
    std::thread::spawn(move || {
        let _ = tx.send(crate::accounts::usage::refreshed(&cwd));
    });
    rx.recv_timeout(timeout).ok().flatten()
}

/// A test never probes the live machine: the installed stub answers.
#[cfg(test)]
pub(crate) fn refresh_usage_readings(
    _cwd: &Path,
    _timeout: std::time::Duration,
) -> Option<Map<String, Value>> {
    REFRESH_STUB
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|answer| answer())
}

#[cfg(test)]
type RefreshAnswer = Box<dyn Fn() -> Option<Map<String, Value>> + Send>;

#[cfg(test)]
static REFRESH_STUB: std::sync::Mutex<Option<RefreshAnswer>> = std::sync::Mutex::new(None);

/// The refresh answer a test installs; it is removed again on drop.
#[cfg(test)]
pub(crate) struct RefreshStub;

#[cfg(test)]
impl RefreshStub {
    pub(crate) fn install(
        answer: impl Fn() -> Option<Map<String, Value>> + Send + 'static,
    ) -> Self {
        *REFRESH_STUB.lock().unwrap_or_else(|e| e.into_inner()) = Some(Box::new(answer));
        RefreshStub
    }
}

#[cfg(test)]
impl Drop for RefreshStub {
    fn drop(&mut self) {
        *REFRESH_STUB.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

/// Aggregate state: exhausted only when EVERY account is (rank
/// ok=available=3, low=2, unknown=1, exhausted=blocked=0).
fn rank(state: &str) -> i32 {
    match state {
        "ok" | "available" => 3,
        "low" => 2,
        "exhausted" | "blocked" => 0,
        _ => 1,
    }
}

/// The capacity map the walk reads: `{harness: {state, window, accounts,
/// sources, observed_at, evidence, resets}}`, byte-compatible with what the
/// Python resolver produced and `route_slot::row_capacity` already reads.
pub(crate) fn capacity(
    inventory: &Value,
    cwd: &Path,
    now: f64,
    refreshed: Option<&Map<String, Value>>,
) -> Value {
    let mut harnesses: Vec<String> = DEFAULT_PROVIDERS.iter().map(|s| s.to_string()).collect();
    if let Some(rows) = inventory.get("rows").and_then(Value::as_array) {
        for row in rows {
            if let Some(h) = row.get("harness").and_then(Value::as_str) {
                let h = h.trim();
                if !h.is_empty() && !harnesses.iter().any(|x| x == h) {
                    harnesses.push(h.to_string());
                }
            }
        }
    }
    let raw = runtime_state_payload(cwd);
    let health = parse_health(&raw);
    let usage = parse_usage(&raw);
    let roots = providers_root(cwd);
    let records = crate::accounts::records::raw_records(None, cwd);
    let mut out = Map::new();
    for harness in &harnesses {
        let accounts = harness_accounts(harness, inventory, &records);
        let evidence = identity_evidence(harness, &accounts, &roots);
        let mut detail: Map<String, Value> = Map::new();
        let mut sources: Map<String, Value> = Map::new();
        let mut observed_at: Map<String, Value> = Map::new();
        let mut resets: Map<String, Value> = Map::new();
        let mut best: Option<String> = None;
        let mut window = "absent".to_string();
        for account in &accounts {
            let (state, source, observed, reset): (String, String, Option<f64>, Option<f64>) =
                match refreshed.and_then(|m| m.get(account)) {
                    // The in-memory observation wins over the disk read.
                    Some(row) if row.get("windows").is_some() => {
                        let snap = crate::accounts::state::parse_snapshot(row);
                        let hv = headroom(None, snap.as_ref(), now);
                        (
                            hv.verdict.as_str().to_string(),
                            hv.source.to_string(),
                            hv.observed_at,
                            hv.resets_at,
                        )
                    }
                    // The refresh itself reported unknown for this account:
                    // its reason is the provenance.
                    Some(row) => {
                        let reason = row
                            .get("reason")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        (
                            "unknown".to_string(),
                            format!("refresh:{reason}"),
                            None,
                            None,
                        )
                    }
                    None => {
                        let hv = headroom(health.get(account), usage.get(account), now);
                        (
                            hv.verdict.as_str().to_string(),
                            hv.source.to_string(),
                            hv.observed_at,
                            hv.resets_at,
                        )
                    }
                };
            detail.insert(account.clone(), json!(state));
            sources.insert(account.clone(), json!(source));
            observed_at.insert(account.clone(), json!(observed));
            resets.insert(account.clone(), json!(reset));
            if best.as_deref().map_or(true, |b| rank(&state) > rank(b)) {
                best = Some(state.clone());
                window = source.clone();
            }
        }
        let proven: Option<String> = evidence
            .iter()
            .find(|(_, v)| v.as_str() == Some("proven"))
            .map(|(k, _)| k.clone());
        if let Some(p) = &proven {
            if let Some(state) = detail.get(p).and_then(Value::as_str) {
                best = Some(state.to_string());
                window = format!("identity:{p}");
            }
        } else if !evidence.is_empty() && evidence.values().any(|v| v.as_str() == Some("mismatch"))
        {
            best = Some("unknown".to_string());
            window = "identity-unproven".to_string();
        }
        out.insert(
            harness.clone(),
            json!({
                "state": best.unwrap_or_else(|| "unknown".into()),
                "window": window,
                "accounts": detail,
                "sources": sources,
                "observed_at": observed_at,
                "evidence": evidence,
                "resets": resets,
            }),
        );
    }
    json!(out)
}

/// The `{harness: state}` summary the walk answer carries: the one piece the
/// renderer built, now read straight off the command's answer.
pub(crate) fn capacity_summary(capacity: &Value) -> Value {
    Value::Object(
        capacity
            .as_object()
            .map(|m| {
                m.iter()
                    .map(|(h, d)| {
                        (
                            h.clone(),
                            json!(d.get("state").and_then(Value::as_str).unwrap_or("unknown")),
                        )
                    })
                    .collect::<Map<String, Value>>()
            })
            .unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims;

    /// A hermetic world: FNO_CONFIG pins the sole config candidate (records
    /// come from the records argument), FNO_RUNTIME_STATE_PATH pins the state
    /// file, and the identity stamps live under the pinned state_dir.
    struct Env {
        _guard: std::sync::MutexGuard<'static, ()>,
        dir: tempfile::TempDir,
    }

    impl Env {
        fn new(state_json: &str, records: &[&str]) -> Self {
            let guard = claims::test_env_lock()
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("tempdir");
            let mut cfg = format!("state_dir = '{}'\n", dir.path().display());
            if !records.is_empty() {
                cfg.push_str("[[accounts.records]]\n".repeat(0).as_str());
                for r in records {
                    cfg.push_str(r);
                }
            }
            let path = dir.path().join("config.toml");
            std::fs::write(&path, cfg).unwrap();
            let state = dir.path().join("state.json");
            std::fs::write(&state, state_json).unwrap();
            std::env::set_var("FNO_CONFIG", &path);
            std::env::set_var("FNO_RUNTIME_STATE_PATH", &state);
            Self { _guard: guard, dir }
        }

        fn providers(&self) -> std::path::PathBuf {
            self.dir.path().join("providers")
        }
    }

    impl Drop for Env {
        fn drop(&mut self) {
            std::env::remove_var("FNO_CONFIG");
            std::env::remove_var("FNO_RUNTIME_STATE_PATH");
        }
    }

    fn now() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64()
    }

    fn inv(rows: Value) -> Value {
        json!({"declared": true, "rows": rows})
    }

    fn account_of(cap: &Value, harness: &str) -> String {
        cap[harness]["state"].as_str().unwrap_or("").to_string()
    }

    /// AC1-HP: a codex snapshot older than 300s reads unknown with source
    /// stale: exactly the reading that used to drop the lane silently.
    #[test]
    fn a_stale_snapshot_reads_unknown_with_source_stale() {
        let state = format!(
            r#"{{"usage": {{"codex": {{"probed_at": {:.0}, "partial": false, "windows": [{{"label": "weekly", "used_pct": 5.0, "resets_at": null}}]}}}}}}"#,
            now() - 600.0
        );
        let env = Env::new(
            &state,
            &["[[accounts.records]]\nid = \"codex\"\nharness = \"codex\"\n"],
        );
        let cap = capacity(&inv(json!([])), env.dir.path(), now(), None);
        assert_eq!(account_of(&cap, "codex"), "unknown");
        assert_eq!(cap["codex"]["sources"]["codex"], "stale");
    }

    /// AC3-HP: a proven slot stamp makes the proven account the aggregate and
    /// marks the rest mismatch.
    #[test]
    fn a_proven_stamp_drives_the_harness_state() {
        let state = format!(
            r#"{{"usage": {{"makers": {{"probed_at": {:.0}, "partial": false, "windows": [{{"label": "daily", "used_pct": 100.0, "resets_at": null}}]}}, "backup": {{"probed_at": {:.0}, "partial": false, "windows": [{{"label": "daily", "used_pct": 1.0, "resets_at": null}}]}}}}}}"#,
            now(),
            now()
        );
        let env = Env::new(
            &state,
            &[
                "[[accounts.records]]\nid = \"makers\"\nharness = \"claude\"\n",
                "[[accounts.records]]\nid = \"backup\"\nharness = \"claude\"\n",
            ],
        );
        std::fs::create_dir_all(env.providers()).unwrap();
        std::fs::write(env.providers().join(".active-claude"), "makers").unwrap();
        let cap = capacity(&inv(json!([])), env.dir.path(), now(), None);
        assert_eq!(cap["claude"]["evidence"]["makers"], "proven");
        assert_eq!(cap["claude"]["evidence"]["backup"], "mismatch");
        // The proven account's state IS the harness state, exhausted included.
        assert_eq!(account_of(&cap, "claude"), "exhausted");
        assert_eq!(cap["claude"]["window"], "identity:makers");
    }

    /// The port of test_runtime_capacity_aggregates_max_over_accounts:
    /// exhausted only when EVERY account is.
    #[test]
    fn aggregates_max_over_accounts() {
        let state = format!(
            r#"{{"usage": {{"primary": {{"probed_at": {:.0}, "partial": false, "windows": [{{"label": "daily", "used_pct": 100.0, "resets_at": null}}]}}, "backup": {{"probed_at": {:.0}, "partial": false, "windows": [{{"label": "daily", "used_pct": 2.0, "resets_at": null}}]}}}}}}"#,
            now(),
            now()
        );
        let env = Env::new(
            &state,
            &[
                "[[accounts.records]]\nid = \"primary\"\nharness = \"claude\"\n",
                "[[accounts.records]]\nid = \"backup\"\nharness = \"claude\"\n",
            ],
        );
        let cap = capacity(&inv(json!([])), env.dir.path(), now(), None);
        assert_eq!(account_of(&cap, "claude"), "ok");
        assert_eq!(cap["claude"]["accounts"]["primary"], "exhausted");
        assert_eq!(cap["claude"]["accounts"]["backup"], "ok");

        // Drop the healthy backup from the records: exhausted + nothing. The
        // first env releases the test-env lock before the second pins one.
        drop(env);
        let env = Env::new(
            &state,
            &["[[accounts.records]]\nid = \"primary\"\nharness = \"claude\"\n"],
        );
        let cap = capacity(&inv(json!([])), env.dir.path(), now(), None);
        assert_eq!(account_of(&cap, "claude"), "exhausted");
    }

    /// The port of test_harness_accounts_expands_rows_then_registered_records:
    /// row accounts first, then records; a route never names an account.
    #[test]
    fn harness_accounts_expands_rows_then_records() {
        let env = Env::new(
            "{}",
            &[
                "[[accounts.records]]\nid = \"rec-a\"\nharness = \"claude\"\n",
                "[[accounts.records]]\nid = \"rec-b\"\nharness = \"codex\"\n",
            ],
        );
        let inventory = inv(json!([
            {"name": "opus-x", "harness": "claude", "model": "o", "route": "zai/glm-5.3"},
            {"name": "flash-x", "harness": "claude", "model": "f", "account": "paid-lane"},
        ]));
        let records = crate::accounts::records::raw_records(None, env.dir.path());
        let accounts = harness_accounts("claude", &inventory, &records);
        assert_eq!(accounts, vec!["paid-lane".to_string(), "rec-a".to_string()]);
        let accounts = harness_accounts("codex", &inventory, &records);
        assert_eq!(accounts, vec!["rec-b".to_string()]);
    }
}
