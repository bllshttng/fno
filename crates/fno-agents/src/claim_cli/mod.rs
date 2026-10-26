//! The operator leaf surface for `fno agents claim`, ported wave by wave
//! from `cli/src/fno/claims/cli.py`.
//!
//! `claim_commands.rs::run_claim` dispatches one op per wave here. The Python
//! leaf forwards binary-direct (the `_forward_to_binary` shape), so this
//! module owns the operator contract: flag spellings, validation messages,
//! exit codes (0 acquired; 1 held-by-other/contention; 2 validation; 3
//! transient) and the human/JSON output shapes. The goldens frozen from the
//! Python leaf (`tests/claim_acquire_parity.rs`) are the contract.

pub mod acquire;
pub mod list;
pub mod manifest_refresh;
pub mod reap;
pub mod refresh;
pub mod release;
pub mod session_pid;
pub mod status;

use serde_json::Value;
use std::path::PathBuf;

/// typer 0.27's UsageError layout, shared by the leaves whose goldens pin
/// the exact lines.
pub(crate) fn usage_refusal(usage: &str, help_hint: &str, detail: &str) -> i32 {
    eprintln!("Usage: {usage}");
    eprintln!("Try '{help_hint}' for help.");
    eprintln!();
    eprintln!("{detail}");
    2
}

/// `--ttl` expression ("30m" / "1h" / "3600s" / "5000") into milliseconds.
/// `Ok(None)` for the empty string (the caller decides the default); plain
/// digits are seconds. The error text is the frozen Python `typer
/// .BadParameter` message body. i128 carries the unbounded-int cases
/// Python's parse survives so the range refusal can name the number.
pub fn parse_ttl_expression(value: &str) -> Result<Option<i128>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    let t = value.trim();
    let (num_part, unit) = match t.chars().last() {
        Some(c @ ('s' | 'm' | 'h' | 'S' | 'M' | 'H')) => {
            let head = &t[..t.len() - c.len_utf8()];
            (head.trim_end(), c.to_ascii_lowercase())
        }
        _ => (t, '\0'),
    };
    if num_part.is_empty() || !num_part.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!(
            "invalid TTL format: '{value}' (use '30m', '1h', '3600s')"
        ));
    }
    let n: i128 = num_part.parse().unwrap_or(i128::MAX);
    let mult = match unit {
        'm' => 60_000i128,
        'h' => 3_600_000,
        _ => 1_000,
    };
    let ms = n.checked_mul(mult).ok_or_else(|| {
        format!(
            "ttl_ms={num_part} out of range [{}{}]",
            crate::claims::MIN_TTL_MS,
            crate::claims::MAX_TTL_MS
        )
    })?;
    Ok(Some(ms))
}

/// The parsed TTL narrowed to the engine's i64. Out-of-range names the
/// number exactly as `core._validate_inputs` does (the engine's own range
/// check cannot see an i128 that overflowed it).
pub fn ttl_ms_checked(ttl: i128) -> Result<i64, String> {
    const MIN: i128 = crate::claims::MIN_TTL_MS as i128;
    const MAX: i128 = crate::claims::MAX_TTL_MS as i128;
    if ttl < MIN || ttl > MAX {
        return Err(format!("ttl_ms={ttl} out of range [{MIN}, {MAX}]"));
    }
    Ok(ttl as i64)
}

/// `--metadata`: empty means `{}`; else a JSON object or the frozen Python
/// refusal.
pub fn parse_metadata_arg(value: &str) -> Result<serde_json::Map<String, Value>, String> {
    if value.is_empty() {
        return Ok(serde_json::Map::new());
    }
    match serde_json::from_str::<Value>(value) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err("--metadata must be a JSON object".into()),
        Err(e) => Err(format!("--metadata is not valid JSON: {e}")),
    }
}

/// The claims root a key routes to: `Some` for the global-id prefixes, None
/// for repo-local keys. The one routing read (`claims_root.rs` owns the
/// prefix list), matching the Python leaf's `_node_aware_root`.
pub fn node_aware_root(key: &str) -> Option<PathBuf> {
    crate::claims_root::claims_root_for(key)
}

/// The registry path the Python leaf read (`paths.agents_registry_path`):
/// a declared `FNO_AGENTS_HOME` wins, else the configured state root.
/// `AgentsHome::shared_registry_json` pins the ambient `$HOME/.fno`, so a
/// pinned state root would read the wrong graph store.
fn agents_registry_path() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os(crate::paths::HOME_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join("registry.json"));
    }
    let cwd = std::env::current_dir().ok()?;
    Some(
        crate::agents_config::state_dir(&cwd)?
            .join("agents")
            .join("registry.json"),
    )
}

/// The registry effort for the owned identity, read through the shared
/// registry path (mirrors `_owned_registry_effort`: unreadable or malformed
/// state leaves effort unknown).
fn owned_registry_effort(harness: &str, session_id: &str) -> Option<String> {
    if harness.is_empty() || session_id.is_empty() {
        return None;
    }
    let path = agents_registry_path()?;
    let raw = crate::registry_store::read(&path).ok()?;
    let rows = raw.get("agents")?.as_array()?;
    let legacy_key = match harness {
        "claude" => Some("claude_session_uuid"),
        "codex" => Some("codex_session_id"),
        "gemini" => Some("gemini_session_id"),
        _ => None,
    };
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        let Some(row_harness) = obj
            .get("harness")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                obj.get("provider")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            })
        else {
            continue;
        };
        if row_harness != harness {
            continue;
        }
        let mut row_session = obj
            .get("harness_session_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if row_session.is_none() {
            row_session = legacy_key
                .and_then(|k| obj.get(k))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        if row_session.as_deref() != Some(session_id) {
            continue;
        }
        return obj
            .get("effort")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    None
}

/// The registry row's harness for one session id, for the claim list
/// silence report: reads the shared registry, and answers "claude" when
/// the registry is unreadable, malformed, or no row names the session. A
/// matching row answers its `harness`, empty or absent, exactly like the
/// Python lookup it ports.
pub(crate) fn harness_for_session(session_id: &str) -> String {
    let Some(path) = agents_registry_path() else {
        return "claude".to_string();
    };
    let Ok(raw) = crate::registry_store::read(&path) else {
        return "claude".to_string();
    };
    let Some(rows) = raw.get("agents").and_then(Value::as_array) else {
        return "claude".to_string();
    };
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        if obj.get("harness_session_id").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        return obj
            .get("harness")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "claude".to_string());
    }
    "claude".to_string()
}

/// The (harness, session_id) an execute provenance row is written under:
/// the OWNED identity, ambient only as the fallback when the owned values
/// are absent (mirrors `_owned_do_identity`; the acquire and release stamps
/// must agree on the row key so release fills the row acquire opened).
fn owned_do_identity(claim: &crate::claims::ClaimRecord, holder: &str) -> (String, String) {
    let ambient = crate::claims::resolve_identity();
    let harness = claim
        .harness
        .as_deref()
        .filter(|h| !h.trim().is_empty())
        .map(str::to_string)
        .or_else(|| ambient.1.filter(|h| !h.trim().is_empty()))
        .unwrap_or_default();
    let session_id = holder
        .strip_prefix("target-session:")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| ambient.0.filter(|s| !s.trim().is_empty()))
        .unwrap_or_default();
    (harness, session_id)
}

/// The `(node_id, harness, session_id, started_at, effort)` naming the do row
/// for this claim, or None after printing the named skip (mirrors
/// `_do_row_coordinates`): every do-row writer addresses the SAME row, and
/// `started_at` is the claim's own acquire time. `action` names the caller
/// in the skip line.
fn do_row_coordinates(
    key: &str,
    claim: &crate::claims::ClaimRecord,
    holder: &str,
    action: &str,
) -> Option<(String, String, String, String, Option<String>)> {
    let node_id = key.split_once(':').map(|(_, id)| id).unwrap_or("");
    if node_id.is_empty() {
        return None;
    }
    let (harness, session_id) = owned_do_identity(claim, holder);
    if harness.is_empty() || session_id.is_empty() {
        eprintln!(
            "claim {action}: no owned identity for the execute provenance row of \
             {node_id}; the row is skipped. Skipped."
        );
        return None;
    }
    let started = chrono::DateTime::from_timestamp_millis(claim.acquired_at)
        .map(|t| t.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default();
    let effort = owned_registry_effort(&harness, &session_id);
    Some((node_id.to_string(), harness, session_id, started, effort))
}

/// Open the do lifecycle row at claim acquire (mirrors
/// `_stamp_do_on_acquire`): best-effort, node-keyed, named skips on stderr,
/// never fails the acquire.
pub fn stamp_do_on_acquire(key: &str, claim: &crate::claims::ClaimRecord, holder: &str) {
    let Some((node_id, harness, session_id, started, effort)) =
        do_row_coordinates(key, claim, holder, "acquire")
    else {
        return;
    };
    let row = match crate::graph_keeper::session_row(
        "execute",
        &harness,
        &session_id,
        effort.as_deref(),
        Some(&started),
        None,
        None,
        None,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("claim acquire: execute provenance open skipped for {node_id}: {e}");
            return;
        }
    };
    let found = std::cell::Cell::new(false);
    let graph = crate::backlog::settings::graph_path();
    let ok = crate::backlog::mutate_single_row(&graph, "session_append", |rows| {
        let (f, _added) = crate::graph_keeper::session_append(rows, &node_id, row.clone())
            .map_err(|e| e.to_string())?;
        found.set(f);
        Ok(f)
    });
    if let Err(e) = ok {
        eprintln!("claim acquire: execute provenance open skipped for {node_id}: {e}");
        return;
    }
    if !found.get() {
        eprintln!(
            "claim acquire: execute provenance open skipped for {node_id} \
             (node not in graph); the row was not written. Skipped."
        );
    }
}

/// Close the do lifecycle row the acquire stamp opened: fills `ended_at`
/// (mirrors `_stamp_do_on_release`). The fill never appends: a release whose
/// node carries no matching row records nothing, so a claim released without
/// running the work (a blueprint release, a rollback a caller forgot to mark)
/// cannot gain an execute completion. Best-effort: a graph failure or missing
/// identity is a named stderr skip and never fails the release.
pub fn stamp_do_on_release(key: &str, claim: &crate::claims::ClaimRecord, holder: &str) {
    let Some((node_id, harness, session_id, started, effort)) =
        do_row_coordinates(key, claim, holder, "release")
    else {
        return;
    };
    let ended = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let row = match crate::graph_keeper::session_row(
        "execute",
        &harness,
        &session_id,
        effort.as_deref(),
        Some(&started),
        Some(&ended),
        None,
        None,
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("claim release: execute provenance stamp skipped for {node_id}: {e}");
            return;
        }
    };
    let found = std::cell::Cell::new(false);
    let filled = std::cell::Cell::new(false);
    let graph = crate::backlog::settings::graph_path();
    let ok = crate::backlog::mutate_single_row(&graph, "session_fill_open", |rows| {
        let (f, filled_now) = crate::graph_keeper::session_fill_open(rows, &node_id, row.clone())
            .map_err(|e| e.to_string())?;
        found.set(f);
        filled.set(filled_now);
        Ok(f)
    });
    if let Err(e) = ok {
        eprintln!("claim release: execute provenance stamp skipped for {node_id}: {e}");
        return;
    }
    if !found.get() {
        eprintln!(
            "claim release: execute provenance stamp skipped for {node_id} \
             (node not in graph); the row was not written. Skipped."
        );
    } else if !filled.get() {
        eprintln!(
            "claim release: no open execute row to close for {node_id} \
             (none was opened, or it is already closed); nothing was stamped. Skipped."
        );
    }
}

/// Drop the open do row this claim's acquire opened, for a releaser whose
/// post-acquire validation refused it (mirrors `_rollback_do_on_release`).
/// The graph primitive only removes an OPEN row whose started_at equals this
/// claim's acquire time; best-effort and named on skip.
pub fn rollback_do_on_release(key: &str, claim: &crate::claims::ClaimRecord, holder: &str) {
    let Some((node_id, harness, session_id, started, _effort)) =
        do_row_coordinates(key, claim, holder, "release --rollback-do")
    else {
        return;
    };
    let outcome = std::cell::Cell::new((false, false));
    let graph = crate::backlog::settings::graph_path();
    let ok = crate::backlog::mutate_single_row(&graph, "session_remove_open", |rows| {
        let (f, removed) = crate::graph_keeper::session_remove_open(
            rows,
            &node_id,
            "execute",
            &harness,
            &session_id,
            &started,
        )
        .map_err(|e| e.to_string())?;
        outcome.set((f, removed));
        Ok(f || removed)
    });
    match ok {
        Err(e) => {
            eprintln!("claim release: execute provenance rollback skipped for {node_id}: {e}");
        }
        _ if !outcome.get().0 => {
            eprintln!(
                "claim release: execute provenance rollback skipped for {node_id} \
                 (node not in graph); nothing was removed. Skipped."
            );
        }
        _ if !outcome.get().1 => {
            eprintln!(
                "claim release: no open execute row to roll back for {node_id} \
                 (none was opened, or the row is already closed)."
            );
        }
        _ => {}
    }
}

/// Serialize the claim the way the Python leaf printed it:
/// `Claim.to_yaml_dict` field order under `json.dumps` default separators
/// (", " / ": "). `pid` stays `null` for a pid-unavailable claim exactly as
/// Python printed `None`. Nested metadata keys arrive sorted (serde_json's
/// map), where Python preserved source order; the goldens use one-key
/// metadata so the frozen bytes do not depend on it.
pub fn claim_json_string(claim: &crate::claims::ClaimRecord) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut push = |k: &str, v: Value| {
        parts.push(format!(
            "{}: {}",
            serde_json::to_string(k).unwrap(),
            spaced(&v)
        ));
    };
    push("schema_version", claim.schema_version.into());
    push("key", Value::String(claim.key.clone()));
    push("holder", Value::String(claim.holder.clone()));
    push("acquired_at", claim.acquired_at.into());
    push("pid", claim.pid.map(Value::from).unwrap_or(Value::Null));
    push("host", Value::String(claim.host.clone()));
    if claim.pid_unavailable {
        push("pid_unavailable", Value::Bool(true));
    }
    if let Some(m) = &claim.machine_id {
        push("machine_id", Value::String(m.clone()));
    }
    if let Some(e) = claim.expires_at {
        push("expires_at", e.into());
    }
    if let Some(r) = &claim.reason {
        push("reason", Value::String(r.clone()));
    }
    if let Some(h) = &claim.harness {
        push("harness", Value::String(h.clone()));
    }
    if let Some(s) = &claim.session_id {
        push("session_id", Value::String(s.clone()));
    }
    if let Some(p) = &claim.pid_provenance {
        push("pid_provenance", Value::String(p.clone()));
    }
    if !claim.metadata.is_empty() {
        push("metadata", Value::Object(claim.metadata.clone()));
    }
    format!("{{{}}}", parts.join(", "))
}

/// `json.dumps` default separators over a serde value. Object keys iterate in
/// serde_json's map order.
pub(crate) fn spaced(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let parts: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{}: {}", serde_json::to_string(k).unwrap(), spaced(val)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        Value::Array(a) => {
            let parts: Vec<String> = a.iter().map(spaced).collect();
            format!("[{}]", parts.join(", "))
        }
        other => serde_json::to_string(other).unwrap_or_else(|_| "null".into()),
    }
}

#[cfg(test)]
mod release_stamp_tests {
    use super::*;
    use crate::claims::SCHEMA_VERSION;

    fn fill_claim() -> crate::claims::ClaimRecord {
        crate::claims::ClaimRecord {
            schema_version: SCHEMA_VERSION,
            key: "node:rel-fill".into(),
            holder: "target-session:sess-r".into(),
            acquired_at: 1_760_000_000_000,
            pid: None,
            host: "test-host".into(),
            pid_unavailable: false,
            expires_at: None,
            reason: None,
            harness: Some("claude".into()),
            session_id: Some("sess-r".into()),
            pid_provenance: None,
            machine_id: None,
            metadata: serde_json::Map::new(),
        }
    }

    fn isolate_graph() -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let db = root.path().join("db/graph.db");
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let backup = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", root.path());
        (root, backup.unwrap_or_default().into())
    }

    fn restore_graph(root: tempfile::TempDir, backup: std::path::PathBuf) {
        match backup.as_os_str().is_empty() {
            true => std::env::remove_var("FNO_STATE_DIR"),
            false => std::env::set_var("FNO_STATE_DIR", backup),
        }
        let _ = std::fs::remove_dir_all(root);
    }

    fn seed_node(db: &std::path::Path, sessions: Value) {
        crate::graph_store::seed_rows(
            db,
            &[serde_json::json!({
                "id": "rel-fill",
                "title": "release stamp fixture",
                "status": "ready",
                "project": "fno",
                "slug": "release-stamp-fixture",
                "sessions": sessions,
            })],
        )
        .unwrap();
    }

    fn read_sessions(db: &std::path::Path) -> Vec<Value> {
        crate::graph_store::read_rows_strict(db)
            .unwrap()
            .into_iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some("rel-fill"))
            .map(|e| {
                e.get("sessions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .unwrap_or_default()
    }

    /// The release stamp closes the row acquire opened: `ended_at` lands on
    /// the matched row, no second row appears.
    #[test]
    fn release_stamp_fills_the_open_row_acquire_opened() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (root, backup) = isolate_graph();
        let db = root.path().join("db/graph.db");
        seed_node(
            &db,
            serde_json::json!([
                {"session_id": "sess-r", "phase": "execute", "harness": "claude",
                 "started_at": "2026-10-25T00:00:00Z"},
            ]),
        );
        stamp_do_on_release("node:rel-fill", &fill_claim(), "target-session:sess-r");
        let rows = read_sessions(&db);
        assert_eq!(rows.len(), 1, "the fill must not add a row: {rows:?}");
        assert!(
            rows[0].get("ended_at").and_then(Value::as_str).is_some(),
            "the fill must close the row: {rows:?}"
        );
        restore_graph(root, backup);
    }

    /// A release whose node carries no matching row records nothing: the
    /// append here is the completion a rolled-back or blueprint release never
    /// earned, which is the defect this fill-only shape exists to prevent.
    #[test]
    fn release_stamp_never_appends_a_row_that_is_not_there() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (root, backup) = isolate_graph();
        let db = root.path().join("db/graph.db");
        seed_node(&db, serde_json::json!([]));
        stamp_do_on_release("node:rel-fill", &fill_claim(), "target-session:sess-r");
        let rows = read_sessions(&db);
        assert!(rows.is_empty(), "the stamp must not append: {rows:?}");
        restore_graph(root, backup);
    }

    /// An already-closed matching row stays closed and stays one row: the
    /// fill is idempotent on a re-release shape.
    #[test]
    fn release_stamp_leaves_a_closed_row_closed() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (root, backup) = isolate_graph();
        let db = root.path().join("db/graph.db");
        seed_node(
            &db,
            serde_json::json!([
                {"session_id": "sess-r", "phase": "execute", "harness": "claude",
                 "started_at": "2026-10-25T00:00:00Z", "ended_at": "2026-10-25T01:00:00Z"},
            ]),
        );
        stamp_do_on_release("node:rel-fill", &fill_claim(), "target-session:sess-r");
        let rows = read_sessions(&db);
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].get("ended_at").and_then(Value::as_str),
            Some("2026-10-25T01:00:00Z")
        );
        restore_graph(root, backup);
    }
}
