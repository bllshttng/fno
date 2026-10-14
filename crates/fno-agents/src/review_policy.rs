//! `fno do review --assess-assurance`: the review assurance verdict and the
//! kinds read, ported from the Python legs and frozen byte-for-byte against
//! goldens captured from them before deletion (`tests/review_policy_parity.rs`).
//!
//! The port composes existing readers and adds no second copy of any: the
//! exhausted verdict is `fallback_chain::headroom` (the one port of
//! `runtime_state.headroom`), the lock is `spawn_gate_lanes::account_locked_until`
//! over `route_capacity::runtime_state_payload`, the ambient harness is
//! `claims::resolve_harness`, the ledger path is `paths_cli::ledger_json`, and
//! the record line renders through `client_verbs::to_python_json`.
//!
//! Two mirrors are local because no Rust reader shared their shape:
//!
//! * the account-records read mirrors `loader.load_providers` — project-local
//!   then PWD-local then global `config.toml`, each candidate falling back to
//!   its `settings.yaml` sibling when the toml file is absent
//!   (`_read_candidates`; NOT `agents_config::config_candidates`, whose
//!   canonical tier the Python loader does not read), records overlaid by
//!   id, each record validated against the raise surface the pydantic model
//!   enforced. The mirror covers the fields the capacity reads consume plus the
//!   checks that make `load_providers` raise: required `id`/`name`/`harness`
//!   (with the legacy `cli` alias) /`auth` literals, `priority >= 0`,
//!   `model_name` non-empty, any duplicate id in the assembled list, a
//!   `route` that fails the `provider/model` parse, and the auth-strategy
//!   consistency rules. Pydantic
//!   refusals on untouched metadata fields (`tags`, `reset_timezone`,
//!   `window_minutes`, `account_id`, `description`) are not mirrored — a config
//!   valid everywhere else but wrong only there reads as loadable here.
//! * the cross-model switch read mirrors `fno.config.load_settings`' layered
//!   read STRICTLY (the shared `config_value_deep` skips an unparseable file,
//!   which would fail closed where Python fails open): any layer that fails to
//!   parse, or an `enabled` leaf that is not a bool, answers `true`.

use serde::Serialize;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::client_verbs::{expand_eq, to_python_json};
use crate::fallback_chain::{
    headroom, health_from_map, parse_provider_health, parse_usage, snapshot_from_map, Verdict,
};

pub(crate) const CLAUDE: &str = "claude";

/// Provider kinds the assurance gate counts as reviewers. The record
/// literal (`model.py::_HARNESS_LITERAL`) is wider; only these dispatch.
pub(crate) const DISPATCHABLE_PROVIDERS: [&str; 3] = ["claude", "codex", "gemini"];

/// Record `harness` literals `load_providers` accepts (the `cli` alias reads
/// the same set). A harness outside this set makes the load raise, which is
/// the fail-closed shape the goldens freeze.
const RECORD_HARNESS_LITERAL: [&str; 8] = [
    "claude", "codex", "opencode", "agy", "trae", "openclaw", "hermes", "gemini",
];

const RECORD_AUTH_LITERAL: [&str; 3] = ["oauth_dir", "api_key", "managed"];

const HIGH_ASSURANCE_SURFACES: [&str; 9] = [
    "auth",
    "security",
    "secrets",
    "merge-gate",
    "review-gate",
    "loopcheck",
    "migration",
    "money",
    "payments",
];

/// `^[a-z][a-z0-9-]{0,63}$` (`model.py::_ID_PATTERN`), the shape both `id`
/// and `route_provider_id` must carry when present.
fn valid_record_id(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && b[0].is_ascii_lowercase()
        && b[1..]
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

/// `model_routing._parse_target`: a `provider/model` (or legacy
/// `provider,model`) target. A comma, when present, wins as the separator
/// and must split into exactly two parts; otherwise the FIRST slash splits,
/// keeping a namespaced model id intact. Both halves non-empty after trim.
fn valid_route_target(raw: &str) -> bool {
    let raw = raw.trim();
    let (provider, model) = if raw.contains(',') {
        let parts: Vec<&str> = raw.split(',').collect();
        if parts.len() != 2 {
            return false;
        }
        (parts[0].trim(), parts[1].trim())
    } else {
        match raw.split_once('/') {
            Some((p, m)) => (p.trim(), m.trim()),
            None => return false,
        }
    };
    !provider.is_empty() && !model.is_empty()
}

/// Select the review policy from plan `size` and named `risk_surfaces`.
/// Deterministic and total (unknown size -> `portable`): any named
/// high-assurance surface wins over size, then L -> `full_sigma`,
/// M -> `diverse_preferred`, else `portable`.
pub(crate) fn classify_review_policy(size: Option<&str>, risk_surfaces: &[String]) -> &'static str {
    let hit = risk_surfaces.iter().any(|s| {
        let t = s.trim().to_ascii_lowercase();
        !t.is_empty() && HIGH_ASSURANCE_SURFACES.contains(&t.as_str())
    });
    if hit {
        return "high_assurance";
    }
    match size.unwrap_or("").trim().to_ascii_uppercase().as_str() {
        "L" => "full_sigma",
        "M" => "diverse_preferred",
        _ => "portable",
    }
}

/// True when a reviewer RESOLVED TO DISPATCH differs from the implementer.
/// `effective_reviewer_kinds` is what the run can actually dispatch to
/// (exhausted and non-serving kinds already removed), so a record that
/// exists but cannot serve never certifies diversity.
pub(crate) fn has_diverse_capacity(
    effective_reviewer_kinds: &[String],
    implementer_provider: &str,
) -> bool {
    let implementer = if implementer_provider.trim().is_empty() {
        CLAUDE.to_string()
    } else {
        implementer_provider.trim().to_ascii_lowercase()
    };
    effective_reviewer_kinds.iter().any(|cand| {
        let cand = cand.trim().to_ascii_lowercase();
        DISPATCHABLE_PROVIDERS.contains(&cand.as_str()) && cand != implementer
    })
}

/// The verdict `assess_assurance` answers: satisfied, the effective mode and
/// the reason string, byte-identical to the Python leg's four outputs.
pub(crate) struct AssuranceVerdict {
    pub satisfied: bool,
    pub effective: &'static str,
    pub reason: &'static str,
}

/// Resolve `policy` against the reviewer resolved to dispatch. Never
/// panics. A PREFLIGHT availability verdict, not a proof a cross-family
/// review completed. `satisfied` and `effective` are correlated, not
/// independent: unresolved iff not satisfied.
pub(crate) fn assess_assurance(
    policy: &str,
    effective_reviewer_kinds: &[String],
    implementer_provider: &str,
    identity_known: bool,
) -> AssuranceVerdict {
    let diverse = has_diverse_capacity(effective_reviewer_kinds, implementer_provider);
    if policy == "high_assurance" {
        if !identity_known {
            return AssuranceVerdict {
                satisfied: false,
                effective: "unresolved",
                reason: "high-assurance requires a known implementer family; identity unknown",
            };
        }
        if !diverse {
            return AssuranceVerdict {
                satisfied: false,
                effective: "unresolved",
                reason: "high-assurance requires different-family capacity; none available",
            };
        }
        return AssuranceVerdict {
            satisfied: true,
            effective: "diverse",
            reason: "different-family reviewer available",
        };
    }
    if diverse {
        return AssuranceVerdict {
            satisfied: true,
            effective: "diverse",
            reason: "different-family reviewer available",
        };
    }
    AssuranceVerdict {
        satisfied: true,
        effective: "portable",
        reason: "same-family fresh-context review (no different-family capacity)",
    }
}

/// The target manifest's `session_id` line, tolerating absence: the first
/// `^session_id:` line wins, its value trimmed, `None` when the file is
/// missing or names nothing.
fn read_state_session_id(state_path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(state_path).ok()?;
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("session_id:") else {
            continue;
        };
        let trimmed = rest.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

/// Resolve the session nonce: explicit arg (when non-empty), else the state
/// file's value. The ONE place assurance resolution reads the session.
pub(crate) fn resolve_session_id(session_id: Option<&str>, state_path: &Path) -> Option<String> {
    if let Some(sid) = session_id.filter(|s| !s.is_empty()) {
        return Some(sid.to_string());
    }
    read_state_session_id(state_path)
}

fn now_epoch() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// `fno.paths.resolve_repo_root`: the `FNO_REPO_ROOT` pin, then the git
/// toplevel of `cwd`, then `cwd`.
fn resolve_repo_root(cwd: &Path) -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT") {
        if !pin.is_empty() {
            return PathBuf::from(pin);
        }
    }
    let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
    else {
        return cwd.to_path_buf();
    };
    if out.status.success() {
        let text = String::from_utf8_lossy(&out.stdout);
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    cwd.to_path_buf()
}

/// `loader._global_settings_path`: the config.toml sibling of
/// `$FNO_GLOBAL_SETTINGS_PATH` when set non-empty, else `$HOME/.fno/config.toml`.
fn global_settings_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH") {
        if !p.is_empty() {
            return Some(PathBuf::from(p).with_file_name("config.toml"));
        }
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".fno/config.toml"))
}

/// `loader._provider_candidates` (first layer wins): the repo-root local,
/// then a PWD-anchored local when it differs, then the global path; an
/// explicit `$FNO_CONFIG` is the sole candidate.
fn provider_config_candidates(cwd: &Path) -> Vec<PathBuf> {
    if let Some(explicit) = std::env::var_os("FNO_CONFIG") {
        if !explicit.is_empty() {
            return vec![PathBuf::from(explicit)];
        }
    }
    let root = resolve_repo_root(cwd);
    let mut out = vec![root.join(".fno/config.toml")];
    let pwd = std::env::var("PWD")
        .map(PathBuf::from)
        .unwrap_or_else(|_| cwd.to_path_buf());
    let pwd_local = pwd.join(".fno/config.toml");
    if !out.contains(&pwd_local) {
        out.push(pwd_local);
    }
    if let Some(global) = global_settings_path() {
        if !out.contains(&global) {
            out.push(global);
        }
    }
    out
}

/// The `accounts` (or legacy `providers`) block of one parsed config, at the
/// top level or under a legacy `config:` wrapper (`loader._extract_accounts_block`).
fn extract_accounts_block(data: &toml::Table) -> Option<&toml::Table> {
    for key in ["accounts", "providers"] {
        if let Some(block) = data.get(key).and_then(toml::Value::as_table) {
            return Some(block);
        }
        if let Some(wrapper) = data.get("config").and_then(toml::Value::as_table) {
            if let Some(block) = wrapper.get(key).and_then(toml::Value::as_table) {
                return Some(block);
            }
        }
    }
    None
}

/// One record's `(id, harness)` when the row validates against the raise
/// surface `ProviderRecord` enforced. Any miss is a whole-load refusal.
fn validate_record(row: &toml::Value) -> Result<(String, String), ()> {
    let table = row.as_table().ok_or(())?;
    let s = |k: &str| -> Result<String, ()> {
        table
            .get(k)
            .and_then(toml::Value::as_str)
            .map(|v| v.trim().to_string())
            .ok_or(())
    };
    let id = s("id")?;
    if !valid_record_id(&id) {
        return Err(());
    }
    s("name")?;
    let harness = table
        .get("harness")
        .or_else(|| table.get("cli"))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .ok_or(())?;
    if !RECORD_HARNESS_LITERAL.contains(&harness) {
        return Err(());
    }
    let auth = s("auth")?;
    if !RECORD_AUTH_LITERAL.contains(&auth.as_str()) {
        return Err(());
    }
    if let Some(p) = table.get("priority") {
        let p = p.as_integer().ok_or(())?;
        if p < 0 {
            return Err(());
        }
    }
    if let Some(mn) = table.get("model_name") {
        if mn.as_str().map(str::trim).unwrap_or_default().is_empty() {
            return Err(());
        }
    }
    if let Some(rp) = table.get("route_provider_id") {
        let rp = rp.as_str().map(str::trim).ok_or(())?;
        if !valid_record_id(rp) {
            return Err(());
        }
    }
    // `_validate_route`: a route PRESENT as a string must parse in the
    // provider/model spelling, whatever its value; the empty string is
    // falsy downstream but still fails this validator.
    let route = table
        .get("route")
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|r| !r.is_empty());
    if let Some(route_raw) = table.get("route").and_then(toml::Value::as_str) {
        if !valid_route_target(route_raw) {
            return Err(());
        }
        if auth != "api_key" {
            return Err(());
        }
    }
    let env = table.get("env").and_then(toml::Value::as_table);
    if let Some(env) = env {
        if env.iter().any(|(_, v)| v.as_str().is_none()) {
            return Err(());
        }
    }
    match auth.as_str() {
        "oauth_dir" => {
            if table
                .get("credentials_source")
                .and_then(toml::Value::as_str)
                .is_none()
            {
                return Err(());
            }
        }
        "api_key" => {
            let env_nonempty = env.map(|e| !e.is_empty()).unwrap_or(false);
            let has_route = route.is_some();
            if env_nonempty == has_route {
                // Exactly one of a non-empty env or a route.
                return Err(());
            }
            if env_nonempty {
                let recognized = env.map(|e| {
                    e.keys()
                        .filter(|k| {
                            matches!(
                                k.as_str(),
                                "ANTHROPIC_API_KEY"
                                    | "ANTHROPIC_AUTH_TOKEN"
                                    | "GEMINI_API_KEY"
                                    | "OPENAI_API_KEY"
                            )
                        })
                        .count()
                });
                if recognized.unwrap_or(0) == 0 {
                    return Err(());
                }
            }
        }
        _ => {
            // managed: neither credentials_source nor env (the route ban is
            // the shared route arm above).
            if env.map(|e| !e.is_empty()).unwrap_or(false)
                || table
                    .get("credentials_source")
                    .and_then(toml::Value::as_str)
                    .is_some()
            {
                return Err(());
            }
        }
    }
    Ok((id, harness.to_string()))
}

/// `loader._read_parsed` for one layer: the config.toml candidate first, its
/// settings.yaml sibling second when the toml file is absent (the
/// unmigrated-install fallback `_read_candidates` serves). The first existing
/// file wins; an unreadable or unparseable layer reads as empty and stops,
/// it never raises.
fn read_layer(path: &Path) -> toml::Table {
    let yaml_path = path.with_file_name("settings.yaml");
    let candidates: [(&Path, bool); 2] = [(path, false), (yaml_path.as_path(), true)];
    for (candidate, is_yaml) in candidates {
        if !candidate.is_file() {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(candidate) else {
            break;
        };
        if is_yaml {
            let Ok(doc) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) else {
                break;
            };
            return yaml_mapping_to_table(&doc);
        }
        return text.parse::<toml::Table>().unwrap_or_default();
    }
    toml::Table::new()
}

/// The YAML document as the loader sees it: a top-level mapping's string keys
/// with scalar/sequence/nested-map values; a non-mapping document is no
/// config at all (`safe_load` returns it and every `.get` misses).
fn yaml_mapping_to_table(doc: &serde_yaml_ng::Value) -> toml::Table {
    let mut out = toml::Table::new();
    let Some(map) = doc.as_mapping() else {
        return out;
    };
    for (key, value) in map {
        let Some(key) = key.as_str() else {
            continue;
        };
        out.insert(key.to_string(), yaml_value_to_toml(value));
    }
    out
}

fn yaml_value_to_toml(value: &serde_yaml_ng::Value) -> toml::Value {
    if let Some(v) = value.as_bool() {
        return toml::Value::Boolean(v);
    }
    if let Some(v) = value.as_i64() {
        return toml::Value::Integer(v);
    }
    if let Some(v) = value.as_f64() {
        return toml::Value::Float(v);
    }
    if let Some(v) = value.as_str() {
        return toml::Value::String(v.to_string());
    }
    if let Some(seq) = value.as_sequence() {
        return toml::Value::Array(seq.iter().map(yaml_value_to_toml).collect());
    }
    if let Some(map) = value.as_mapping() {
        let mut out = toml::Table::new();
        for (key, item) in map {
            if let Some(key) = key.as_str() {
                out.insert(key.to_string(), yaml_value_to_toml(item));
            }
        }
        return toml::Value::Table(out);
    }
    // Null and any exotic tag degrade to an empty string: the reads this
    // port feeds (accounts.records, the cross-model switch leaf) never
    // consume either.
    toml::Value::String(String::new())
}

/// `loader.load_providers` as the capacity reads consume it: the declared
/// `(id, harness)` pairs in merge order, `None` when the load would raise
/// (any record invalid — the caller fails closed, claude-only).
pub(crate) fn load_provider_records(cwd: &Path) -> Option<Vec<(String, String)>> {
    let mut merged: Vec<toml::Value> = Vec::new();
    for path in provider_config_candidates(cwd).iter().rev() {
        let data = read_layer(path);
        let Some(block) = extract_accounts_block(&data) else {
            continue;
        };
        let Some(records) = block.get("records").and_then(toml::Value::as_array) else {
            continue;
        };
        if records.is_empty() {
            continue;
        }
        // `_overlay_records`: incoming wins per id against the layers already
        // merged, new ids append; duplicate ids within ONE file are
        // preserved (the within-file `seen` set skips the removal).
        let file_start = merged.len();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for record in records {
            let rid = record
                .as_table()
                .and_then(|t| t.get("id"))
                .and_then(toml::Value::as_str);
            match rid {
                Some(rid) => {
                    if seen.contains(rid) {
                        merged.push(record.clone());
                        continue;
                    }
                    seen.insert(rid.to_string());
                    if let Some(pos) = merged[..file_start].iter().position(|existing| {
                        existing
                            .as_table()
                            .and_then(|t| t.get("id"))
                            .and_then(toml::Value::as_str)
                            == Some(rid)
                    }) {
                        merged.remove(pos);
                    }
                    merged.push(record.clone());
                }
                None => merged.push(record.clone()),
            }
        }
    }
    // `_check_unique_record_ids`: the assembled load refuses ANY duplicate
    // id, including the within-file duplicates the overlay preserved.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for record in &merged {
        let Some(rid) = record
            .as_table()
            .and_then(|t| t.get("id"))
            .and_then(toml::Value::as_str)
        else {
            continue;
        };
        if !seen.insert(rid) {
            return None;
        }
    }
    let mut out = Vec::with_capacity(merged.len());
    for record in &merged {
        out.push(validate_record(record)?);
    }
    Some(out)
}

/// `runtime_state.is_in_cooldown(id)` for a `None` model: the shared
/// record-level health TTL applies, then `rate_limited_until > now`.
fn is_locked(payload: &Value, account: &str, now: f64) -> bool {
    crate::spawn_gate_lanes::account_locked_until(payload, account, now).is_some()
}

/// One account's `headroom(pid).state is EXHAUSTED` verdict through the
/// shared `fallback_chain` port.
fn account_exhausted(payload: &Value, account: &str, now: f64) -> bool {
    let health_map = parse_provider_health(payload);
    let usage_map = parse_usage(payload);
    let health = health_from_map(health_map.get(account));
    let usage = snapshot_from_map(usage_map.get(account));
    headroom(health.as_ref(), usage.as_ref(), now).verdict == Verdict::Exhausted
}

/// The dispatchable kinds whose provider records are ALL exhausted, `None`
/// when the read failed (the caller fails closed). A kind with no record
/// (e.g. the claude fallback) is never exhausted.
pub(crate) fn exhausted_provider_kinds(cwd: &Path, now: f64) -> Option<Vec<String>> {
    let records = load_provider_records(cwd)?;
    let payload = crate::route_capacity::runtime_state_payload(cwd);
    let mut by_kind: Vec<(String, Vec<String>)> = Vec::new();
    for (id, harness) in &records {
        if !DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) {
            continue;
        }
        match by_kind.iter_mut().find(|(k, _)| *k == *harness) {
            Some((_, ids)) => ids.push(id.clone()),
            None => by_kind.push((harness.clone(), vec![id.clone()])),
        }
    }
    Some(
        by_kind
            .into_iter()
            .filter(|(_, ids)| ids.iter().all(|id| account_exhausted(&payload, id, now)))
            .map(|(k, _)| k)
            .collect(),
    )
}

/// The ordered dispatchable provider kinds available right now: claude
/// first, then one entry per dispatchable harness in declared order with at
/// least one unlocked account, exhausted kinds stably demoted to the end.
pub(crate) fn available_provider_kinds(cwd: &Path, now: f64) -> Vec<String> {
    let mut kinds = vec![CLAUDE.to_string()];
    let Some(records) = load_provider_records(cwd) else {
        return kinds;
    };
    let payload = crate::route_capacity::runtime_state_payload(cwd);
    for (id, harness) in &records {
        if !DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) || kinds.contains(harness) {
            continue;
        }
        if !is_locked(&payload, id, now) {
            kinds.push(harness.clone());
        }
    }
    let Some(exhausted) = exhausted_provider_kinds(cwd, now) else {
        return kinds;
    };
    let mut head = Vec::with_capacity(kinds.len());
    let mut tail = Vec::new();
    for kind in kinds {
        if exhausted.contains(&kind) {
            tail.push(kind);
        } else {
            head.push(kind);
        }
    }
    head.extend(tail);
    head
}

/// `config.review.cross_model.enabled` through a STRICT layered read: every
/// readable layer must parse (one that fails answers `true`, exactly where
/// Python's whole-load settings refusal fails open), then the first layer
/// defining the leaf wins, a non-bool leaf fails open, and no definition
/// anywhere is `false`.
fn cross_model_enabled(cwd: &Path) -> bool {
    let mut parsed: Vec<toml::Table> = Vec::new();
    for path in crate::agents_config::config_candidates(cwd) {
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return true;
        };
        parsed.push(table);
    }
    for table in &parsed {
        let leaf = table
            .get("review")
            .and_then(|v| v.get("cross_model"))
            .and_then(|v| v.get("enabled"));
        if let Some(leaf) = leaf {
            return leaf.as_bool().unwrap_or(true);
        }
    }
    false
}

/// `policy._local_runtime_kind`: the harness this session runs on, `None`
/// when no marker resolves (an ambiguous two-family env included).
fn local_runtime_kind() -> Option<String> {
    crate::claims::resolve_harness().map(|h| h.trim().to_ascii_lowercase())
}

/// `policy.load_implementer_identity` via the ledger: `(kind, established)`,
/// established only when a row carried a provider_id mapping to a real
/// dispatchable kind. An unmappable id defaults to claude WITHOUT being an
/// established family. Never fails.
pub(crate) fn load_implementer_identity(session_id: &str, cwd: &Path) -> (String, bool) {
    if session_id.is_empty() {
        return (CLAUDE.to_string(), false);
    }
    let ledger_path = crate::paths_cli::ledger_json(cwd);
    let Ok(text) = std::fs::read_to_string(&ledger_path) else {
        return (CLAUDE.to_string(), false);
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return (CLAUDE.to_string(), false);
    };
    let Some(entries) = data.get("entries").and_then(Value::as_array) else {
        return (CLAUDE.to_string(), false);
    };
    let mut provider_id: Option<&str> = None;
    for row in entries {
        let Some(row) = row.as_object() else { continue };
        if row.get("session_id").and_then(Value::as_str) != Some(session_id) {
            continue;
        }
        if let Some(pid) = row
            .get("provider_id")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
        {
            provider_id = Some(pid);
        }
    }
    let Some(provider_id) = provider_id else {
        return (CLAUDE.to_string(), false);
    };
    // A value that is already a dispatchable kind is used directly.
    let lowered = provider_id.trim().to_ascii_lowercase();
    if DISPATCHABLE_PROVIDERS.contains(&lowered.as_str()) {
        return (lowered, true);
    }
    // Otherwise looked up in the records table by the RAW id; its harness is
    // the kind when dispatchable.
    if let Some(records) = load_provider_records(cwd) {
        for (id, harness) in records {
            if id == provider_id && DISPATCHABLE_PROVIDERS.contains(&harness.as_str()) {
                return (harness, true);
            }
        }
    }
    (CLAUDE.to_string(), false)
}

#[derive(Serialize)]
struct AssuranceRecord {
    policy: &'static str,
    satisfied: bool,
    effective: &'static str,
    reason: &'static str,
    implementer_provider: String,
    identity_known: bool,
    effective_reviewer_kinds: Vec<String>,
    exhausted_kinds: Vec<String>,
    headroom_unknown: bool,
}

/// The full assurance read as one pure function over (cwd, session inputs),
/// returning the exact stdout bytes and exit the door prints. The parity
/// test calls this under the same hermetic pins the capture driver froze.
pub fn assess_outcome(
    cwd: &Path,
    session_id: Option<&str>,
    state_path: Option<&Path>,
    size: Option<&str>,
    risk_surfaces: &[String],
) -> (String, i32) {
    let now = now_epoch();
    let state = state_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cwd.join(".fno/target-state.md"));
    let sid = resolve_session_id(session_id, &state).unwrap_or_default();
    let (mut implementer, mut identity_known) = load_implementer_identity(&sid, &cwd);
    let ambient = local_runtime_kind();
    if !identity_known {
        if let Some(ambient) = &ambient {
            implementer = ambient.clone();
            identity_known = true;
        }
    }
    let exhausted_opt = exhausted_provider_kinds(&cwd, now);
    let headroom_unknown = exhausted_opt.is_none();
    let exhausted: Vec<String> = exhausted_opt.unwrap_or_default();
    let mut effective: Vec<String> = vec![ambient.clone().unwrap_or_else(|| implementer.clone())];
    effective.sort();
    effective.dedup();
    if cross_model_enabled(&cwd) {
        for kind in available_provider_kinds(&cwd, now) {
            let kind = kind.trim().to_ascii_lowercase();
            if (exhausted.contains(&kind) || headroom_unknown) && kind != CLAUDE {
                continue;
            }
            if !effective.contains(&kind) {
                effective.push(kind);
            }
        }
        effective.sort();
    }
    let policy = classify_review_policy(size, risk_surfaces);
    let verdict = assess_assurance(policy, &effective, &implementer, identity_known);
    let record = AssuranceRecord {
        policy,
        satisfied: verdict.satisfied,
        effective: verdict.effective,
        reason: verdict.reason,
        implementer_provider: implementer,
        identity_known,
        effective_reviewer_kinds: effective,
        exhausted_kinds: {
            let mut e = exhausted;
            e.sort();
            e
        },
        headroom_unknown,
    };
    let exit = if verdict.satisfied { 0 } else { 3 };
    (format!("{}\n", to_python_json(&record)), exit)
}

#[derive(Serialize)]
struct KindsRecord {
    available: Vec<String>,
    dispatchable: Vec<&'static str>,
}

/// The kinds read in the door's record shape, frozen against the same
/// `available_provider_kinds` oracle the assess goldens used.
pub fn kinds_record(cwd: &Path) -> String {
    let available = available_provider_kinds(cwd, now_epoch());
    let record = KindsRecord {
        available,
        dispatchable: DISPATCHABLE_PROVIDERS.to_vec(),
    };
    format!("{}\n", to_python_json(&record))
}

#[derive(Debug, Default)]
pub struct AssessArgs {
    assess_flag: bool,
    size: Option<String>,
    surfaces: Vec<String>,
    session: Option<String>,
    state: Option<String>,
}

/// Door argv for `assess`: flags only, the Python command had no positional.
/// An unknown flag or a stray word is a usage error (exit 2, empty stdout).
/// Public for the parity test's usage-error case.
pub fn parse_assess_args(args: &[String]) -> Result<AssessArgs, String> {
    let expanded = expand_eq(args);
    let mut out = AssessArgs::default();
    let mut i = 0;
    while i < expanded.len() {
        let arg = &expanded[i];
        match arg.as_str() {
            "--assess-assurance" => out.assess_flag = true,
            "--policy-size" => {
                i += 1;
                out.size = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--policy-size needs a value".to_string())?
                        .clone(),
                );
            }
            "--risk-surface" => {
                i += 1;
                out.surfaces.push(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--risk-surface needs a value".to_string())?
                        .clone(),
                );
            }
            "--session-id" => {
                i += 1;
                out.session = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--session-id needs a value".to_string())?
                        .clone(),
                );
            }
            "--state" => {
                i += 1;
                out.state = Some(
                    expanded
                        .get(i)
                        .ok_or_else(|| "--state needs a value".to_string())?
                        .clone(),
                );
            }
            other => {
                if other.starts_with('-') {
                    return Err(format!("unknown flag: {other}"));
                }
                return Err(format!("unexpected argument: {other}"));
            }
        }
        i += 1;
    }
    Ok(out)
}

fn print_usage() {
    println!("usage: fno-agents review-policy <assess|kinds> [flags]");
    println!();
    println!("Review assurance verdict and reviewer kinds, the native fno do review root.");
    println!("  assess [--assess-assurance] [--policy-size S] [--risk-surface X]...");
    println!("         [--session-id ID] [--state PATH]");
    println!("    Print the assurance record; exit 3 when a high-assurance change is");
    println!("    unsatisfied, 2 with the refusal line when --assess-assurance is absent.");
    println!("  kinds");
    println!("    Print one record of the available and dispatchable reviewer kinds.");
    println!("  -h, --help    Print this help and exit 0");
}

/// The `fno do review` root's refusal, byte-identical to the Python
/// `typer.secho(..., err=True)` line the goldens froze.
fn print_refusal() {
    eprintln!("{REFUSAL}");
}

/// The refusal line, public for the parity test's refusal case.
pub(crate) const REFUSAL: &str =
    "sigma is removed: the review is bare /fno:review (the fno review lane), \
which runs inline on every harness and emits its own attestation. \
`fno do review classify` and `fno do review resolve-level` remain for \
the lane's emit step.";

/// `review-policy` review door: `assess` answers the verdict or the refusal;
/// `kinds` prints the kinds record; usage errors print one line on stderr
/// and exit 2 with empty stdout.
pub fn run_review_policy(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print_usage();
        return 0;
    }
    let Some((action, rest)) = args.split_first() else {
        eprintln!("usage: fno-agents review-policy <assess|kinds> [flags] (missing action)");
        return 2;
    };
    match action.as_str() {
        "kinds" => match parse_kinds_args(rest) {
            Ok(()) => {
                let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                print!("{}", kinds_record(&cwd));
                0
            }
            Err(msg) => {
                eprintln!("usage: fno-agents review-policy kinds ({msg})");
                2
            }
        },
        "assess" => {
            let parsed = match parse_assess_args(rest) {
                Ok(p) => p,
                Err(msg) => {
                    eprintln!("usage: fno-agents review-policy assess [flags] ({msg})");
                    return 2;
                }
            };
            if !parsed.assess_flag {
                print_refusal();
                return 2;
            }
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let (stdout, exit) = assess_outcome(
                &cwd,
                parsed.session.as_deref(),
                parsed.state.as_deref().map(Path::new),
                parsed.size.as_deref(),
                &parsed.surfaces,
            );
            print!("{stdout}");
            exit
        }
        other => {
            eprintln!(
                "usage: fno-agents review-policy <assess|kinds> [flags] (unknown action: {other})"
            );
            2
        }
    }
}

/// `kinds` takes no flags: anything present is a usage error.
fn parse_kinds_args(args: &[String]) -> Result<(), String> {
    let expanded = expand_eq(args);
    match expanded.first() {
        None => Ok(()),
        Some(arg) => Err(format!("unexpected argument: {arg}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_and_assess_points() {
        // The size matrix and the padded/unknown tokens.
        assert_eq!(classify_review_policy(Some("S"), &[]), "portable");
        assert_eq!(classify_review_policy(Some("M"), &[]), "diverse_preferred");
        assert_eq!(classify_review_policy(Some("L"), &[]), "full_sigma");
        assert_eq!(classify_review_policy(None, &[]), "portable");
        assert_eq!(classify_review_policy(Some("XL"), &[]), "portable");
        assert_eq!(classify_review_policy(Some(" l "), &[]), "full_sigma");
        // Surfaces win over size; padded case-insensitive; unknown surfaces don't.
        assert_eq!(
            classify_review_policy(Some("S"), &["auth".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("S"), &["docs".to_string(), " auth ".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("L"), &[" Auth ".to_string()]),
            "high_assurance"
        );
        assert_eq!(
            classify_review_policy(Some("L"), &["docs".to_string()]),
            "full_sigma"
        );

        let kinds =
            |items: &[&str]| -> Vec<String> { items.iter().map(|s| s.to_string()).collect() };
        // High assurance is the only blocker.
        let v = assess_assurance("high_assurance", &kinds(&["claude"]), "claude", false);
        assert!(!v.satisfied && v.effective == "unresolved");
        let v = assess_assurance("high_assurance", &kinds(&["claude"]), "claude", true);
        assert!(!v.satisfied && v.effective == "unresolved");
        let v = assess_assurance(
            "high_assurance",
            &kinds(&["claude", "codex"]),
            "claude",
            true,
        );
        assert!(v.satisfied && v.effective == "diverse");
        // portable / diverse_preferred / full_sigma never block.
        for policy in ["portable", "diverse_preferred", "full_sigma"] {
            let v = assess_assurance(policy, &kinds(&["claude"]), "claude", false);
            assert!(v.satisfied && v.effective == "portable", "{policy}");
            let v = assess_assurance(policy, &kinds(&["codex"]), "claude", true);
            assert!(v.satisfied && v.effective == "diverse", "{policy}");
        }
        // Diversity needs a DISPATCHABLE kind that differs from the implementer:
        // opencode is not dispatchable, an unknown kind is not either.
        let v = assess_assurance(
            "high_assurance",
            &kinds(&["claude", "opencode"]),
            "claude",
            true,
        );
        assert!(!v.satisfied);
        let v = assess_assurance("high_assurance", &kinds(&["codex", "wat"]), "codex", true);
        assert!(!v.satisfied);
        // A blank implementer defaults to claude.
        assert!(has_diverse_capacity(&kinds(&["codex"]), ""));
        assert!(!has_diverse_capacity(&kinds(&["claude"]), ""));
        // Verdict correlation: unresolved iff not satisfied, on every input above.
    }

    #[test]
    fn record_load_raise_surface() {
        // `valid_route_target` is `model_routing._parse_target`: a comma
        // wins as the separator and must split exactly two, else the FIRST
        // slash splits; both halves non-empty after trim.
        assert!(valid_route_target("zai/glm-5.3"));
        assert!(valid_route_target("zai/deepseek-ai/DeepSeek-V3"));
        assert!(valid_route_target(" zai , glm "));
        assert!(!valid_route_target("zai"));
        assert!(!valid_route_target("zai/"));
        assert!(!valid_route_target("/glm"));
        assert!(!valid_route_target("zai,glm,x"));
        assert!(!valid_route_target(""));

        let dir =
            std::env::temp_dir().join(format!("review-policy-records-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.toml");
        let prior = std::env::var("FNO_CONFIG").ok();
        std::env::set_var("FNO_CONFIG", &config);
        // (a) The settings.yaml sibling answers when the toml candidate is
        // absent (`_read_candidates`' second leg).
        std::fs::write(
            dir.join("settings.yaml"),
            "accounts:\n  records:\n    - id: yaml-a\n      name: Y\n      harness: claude\n      auth: managed\n",
        )
        .unwrap();
        assert_eq!(
            load_provider_records(&dir),
            Some(vec![("yaml-a".to_string(), "claude".to_string())])
        );
        let _ = std::fs::remove_file(dir.join("settings.yaml"));
        // (b) `_check_unique_record_ids`: ANY duplicate id in the assembled
        // list refuses the whole load, within one file included.
        std::fs::write(
            &config,
            "[[accounts.records]]\nid = \"dup\"\nname = \"A\"\nharness = \"claude\"\nauth = \"managed\"\n[[accounts.records]]\nid = \"dup\"\nname = \"B\"\nharness = \"codex\"\nauth = \"managed\"\n",
        )
        .unwrap();
        assert_eq!(load_provider_records(&dir), None);
        // (c) `_validate_route`: a route must parse as provider/model and
        // only an api_key record may carry one.
        std::fs::write(
            &config,
            "[[accounts.records]]\nid = \"r1\"\nname = \"R\"\nharness = \"claude\"\nauth = \"api_key\"\nroute = \"nope\"\n",
        )
        .unwrap();
        assert_eq!(load_provider_records(&dir), None);
        std::fs::write(
            &config,
            "[[accounts.records]]\nid = \"r1\"\nname = \"R\"\nharness = \"claude\"\nauth = \"api_key\"\nroute = \"zai/glm\"\n",
        )
        .unwrap();
        assert_eq!(
            load_provider_records(&dir),
            Some(vec![("r1".to_string(), "claude".to_string())])
        );
        std::fs::write(
            &config,
            "[[accounts.records]]\nid = \"r1\"\nname = \"R\"\nharness = \"claude\"\nauth = \"managed\"\nroute = \"zai/glm\"\n",
        )
        .unwrap();
        assert_eq!(load_provider_records(&dir), None);
        match prior {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_and_surface_edges() {
        let dir = std::env::temp_dir().join(format!("review-policy-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let state = dir.join("state.md");
        std::fs::write(
            &state,
            "---\nclaude_session_id: nope\nsession_id:  sess-a\n---\nbody session_id: nope\n",
        )
        .unwrap();
        assert_eq!(resolve_session_id(None, &state).as_deref(), Some("sess-a"));
        // Explicit wins; empty explicit falls through to the file.
        assert_eq!(
            resolve_session_id(Some("sess-b"), &state).as_deref(),
            Some("sess-b")
        );
        assert_eq!(
            resolve_session_id(Some(""), &state).as_deref(),
            Some("sess-a")
        );
        // A missing file names nothing.
        assert_eq!(resolve_session_id(None, &dir.join("absent.md")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
