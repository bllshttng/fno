//! The advisory assembly: silent switches, dead letters, the per-harness
//! surface, the config-key readers, evals, and the cost-check +
//! transcript ops, ported from the deleted `cli/src/fno/doctor.py`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{command_call, resolve_binary};
use crate::digest_overlay::{config_roots, global_config_toml, non_empty_env_os};

// ---------------------------------------------------------------------------
// Config reading (the silent-switch inputs)
// ---------------------------------------------------------------------------

/// One layer of the config chain as a parsed TOML document.
fn read_config_layer(path: &Path) -> Option<toml::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    toml::from_str(&text).ok()
}

fn config_bool(doc: Option<&toml::Value>, section: &str) -> Option<bool> {
    doc?.get(section)?.get("enabled")?.as_bool()
}

fn config_string(doc: Option<&toml::Value>, section: &str, key: &str) -> Option<String> {
    doc?.get(section)?.get(key)?.as_str().map(str::to_string)
}

/// The config chain the silent-switch pair reads: global, then the canonical
/// checkout, then this checkout. Later layers win; unset keys take the
/// Python loader's defaults at the read sites.
struct DoctorConfig {
    active_backlog: Option<bool>,
    think_spawn: Option<bool>,
    auto_merge: Option<bool>,
    grant: Option<String>,
}

fn load_doctor_config() -> DoctorConfig {
    let mut cfg = DoctorConfig {
        active_backlog: None,
        think_spawn: None,
        auto_merge: None,
        grant: None,
    };
    for path in config_candidates() {
        let doc = read_config_layer(&path);
        if let Some(v) = config_bool(doc.as_ref(), "active_backlog") {
            cfg.active_backlog = Some(v);
        }
        if let Some(v) = config_bool(doc.as_ref(), "think_spawn") {
            cfg.think_spawn = Some(v);
        }
        if let Some(v) = config_bool(doc.as_ref(), "auto_merge") {
            cfg.auto_merge = Some(v);
        }
        if let Some(v) = config_string(doc.as_ref(), "auto_merge", "grant") {
            cfg.grant = Some(v);
        }
    }
    cfg
}

/// The config ladder, lowest precedence first: `$FNO_CONFIG` is the sole
/// candidate when set; otherwise the global config.toml, then the project
/// roots (the canonical checkout behind this worktree, then this checkout on
/// top). Same merge order as the Python loader and the mux keymap ladder
/// (digest_overlay.rs), so the doctor cannot read a value `fno config get`
/// disagrees with.
pub(crate) fn config_candidates() -> Vec<PathBuf> {
    if let Some(explicit) = non_empty_env_os("FNO_CONFIG").and_then(|v| v.into_string().ok()) {
        return vec![PathBuf::from(explicit)];
    }
    let mut paths: Vec<PathBuf> = global_config_toml().into_iter().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for root in config_roots(&cwd).into_iter().rev() {
        paths.push(root.join(".fno/config.toml"));
    }
    paths
}

// ---------------------------------------------------------------------------
// Silent-switch legibility (doctor.py:1434-1791)
// ---------------------------------------------------------------------------

/// Backlog entries carrying `mission_active`, from the store's nodes table
/// extras (the canonical fail-safe reader: a torn graph reads as 0).
fn mission_active_count() -> i64 {
    let path = super::graph_store_path();
    if !path.exists() {
        return 0;
    }
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return 0;
    };
    let Ok(mut stmt) = conn.prepare("SELECT id, extras FROM nodes") else {
        return 0;
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return 0;
    };
    let mut count = 0i64;
    for row in rows.flatten() {
        let extras: Value = serde_json::from_str(&row.1).unwrap_or(Value::Null);
        if extras.get("mission_active").and_then(Value::as_bool) == Some(true) {
            count += 1;
        }
    }
    count
}

/// Worktree manifests whose resolved `auto_merge_approved` is true, counted
/// by source layer. Two layouts: `<base>/<name>/.fno/target-state.md`
/// (harness-native) and `<base>/<repo>/<name>/.fno/target-state.md`
/// (fno-managed); deduped by canonical path.
pub(crate) fn auto_merge_armed_manifests() -> BTreeMap<String, i64> {
    let mut bases: Vec<PathBuf> = vec![super::state_dir().join("worktrees")];
    let repo = super::resolve_source(None);
    if let Some(repo) = repo {
        bases.push(repo.join(".claude/worktrees"));
    }
    let mut counts: BTreeMap<String, i64> = BTreeMap::new();
    let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
    for base in bases {
        if !base.is_dir() {
            continue;
        }
        let Ok(top) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in top.flatten() {
            let first = entry.path();
            let direct = first.join(".fno/target-state.md");
            if direct.is_file() {
                if let Ok(real) = direct.canonicalize() {
                    if seen.insert(real) {
                        count_manifest(&direct, &mut counts);
                    }
                }
                continue;
            }
            let Ok(mid) = std::fs::read_dir(&first) else {
                continue;
            };
            for sub in mid.flatten() {
                let nested = sub.path().join(".fno/target-state.md");
                if nested.is_file() {
                    if let Ok(real) = nested.canonicalize() {
                        if seen.insert(real) {
                            count_manifest(&nested, &mut counts);
                        }
                    }
                }
            }
        }
    }
    counts
}

fn count_manifest(path: &PathBuf, counts: &mut BTreeMap<String, i64>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let approved = text.lines().any(|l| {
        let l = l.trim();
        l == "auto_merge_approved: true" || l == "auto_merge_approved: \"true\""
    });
    if !approved {
        return;
    }
    let source = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("auto_merge_source: "))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "unknown".to_string());
    *counts.entry(source).or_insert(0) += 1;
}

fn read_posture_stamp() -> Value {
    let path = super::state_dir().join("posture.json");
    match super::read_json_object(&path) {
        Some(map) => Value::Object(map),
        None => Value::Null,
    }
}

pub(crate) fn silent_switch_report(plugin_cache: &Value, armed: &BTreeMap<String, i64>) -> Value {
    let cfg = load_doctor_config();
    let ab = cfg.active_backlog;
    let ts = cfg.think_spawn;
    let am = cfg.auto_merge;
    let grant = cfg.grant.as_deref();
    let mut findings: Vec<Value> = Vec::new();
    let missions = mission_active_count();
    if ab == Some(false) && missions > 0 {
        findings.push(json!({
            "direction": "inaction",
            "switch": "active_backlog.enabled",
            "count": missions,
            "count_label": "mission_active epic(s) queued",
            "command": "fno config set active_backlog.enabled true",
        }));
    }
    if ts == Some(false) {
        findings.push(json!({
            "direction": "inaction",
            "switch": "think_spawn.enabled",
            "command": "fno config set think_spawn.enabled true",
        }));
    }
    if am == Some(true) {
        findings.push(json!({
            "direction": "irreversible",
            "switch": "auto_merge.enabled",
            "command": "fno config set auto_merge.enabled false",
        }));
    }
    if grant == Some("dispatch") {
        findings.push(json!({
            "direction": "irreversible",
            "switch": "auto_merge.grant",
            "command": "fno config set auto_merge.grant none",
        }));
    }
    if !armed.is_empty() && am == Some(true) {
        let total: i64 = armed.values().sum();
        let breakdown = breakdown_line(armed);
        let mut finding = json!({
            "direction": "irreversible",
            "switch": "auto_merge_approved (worktree manifests)",
            "count": total,
            "count_label": format!("manifest(s): {breakdown}"),
            "command": "fno config set auto_merge.enabled false",
        });
        if armed.contains_key("unknown") {
            let no_roots = plugin_cache
                .get("roots")
                .and_then(Value::as_array)
                .map(|a| a.is_empty())
                .unwrap_or(true);
            let stale_stage =
                plugin_cache.get("status").and_then(Value::as_str) == Some("stale") && no_roots;
            if stale_stage {
                let sha = plugin_cache
                    .get("sha")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(12)
                    .collect::<String>();
                let when_raw = plugin_cache
                    .get("installed_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(10)
                    .collect::<String>();
                let when = if when_raw.is_empty() {
                    "?"
                } else {
                    when_raw.as_str()
                };
                finding["cause"] = Value::String(format!(
                    "deployed plugin cache is stale ({sha}, {when}); likely predates the auto_merge_source writer. Fix: claude plugin update fno@fno (or fno@footnote; not fno doctor update), then restart"
                ));
            }
        }
        findings.push(finding);
    }
    json!({
        "findings": findings,
        "posture": read_posture_stamp(),
    })
}

fn breakdown_line(armed: &BTreeMap<String, i64>) -> String {
    let mut rows: Vec<(i64, String)> = armed.iter().map(|(src, n)| (*n, src.clone())).collect();
    rows.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    rows.into_iter()
        .map(|(n, src)| format!("{n} {src}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The durable-grant observer coupling (doctor.py:2570-2577): a standing
/// dispatch grant implies a live watcher; an unreadable config reads no
/// grant.
pub(crate) fn standing_dispatch_grant() -> bool {
    let cfg = load_doctor_config();
    cfg.auto_merge == Some(true) && cfg.grant.as_deref() == Some("dispatch")
}

// ---------------------------------------------------------------------------
// auto-merge review gap + merge-gating opt-out residue (doctor.py:1660-1788)
// ---------------------------------------------------------------------------

/// The invoking repo's root, resolved as the review predicate resolves it
/// (cwd -> toplevel); outside a repo this degrades to cwd (doctor.py:1660).
fn cwd_repo_root() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
    {
        Ok(out) if out.status.code() == Some(0) => {
            let top = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if top.is_empty() {
                cwd
            } else {
                PathBuf::from(top)
            }
        }
        _ => cwd,
    }
}

/// Python truthiness over a JSON value (`if lane:` on the door's answer).
fn lane_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(_) => true,
    }
}

/// Python list repr with single quotes (`['a', 'b']`), for the keys lines.
fn py_list(items: &[String]) -> String {
    let inner = items
        .iter()
        .map(|i| format!("'{i}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// The invoking repo's review config: global layer, then the repo's own
/// layer; later wins. Loader defaults where a layer leaves a key unset
/// (`load_settings_for_repo(root).review` in the deleted leg).
fn read_review_keys(root: &Path) -> Vec<String> {
    let mut self_review_required = true;
    let mut required_bots: Vec<String> = Vec::new();
    let mut reviewers: Vec<String> = Vec::new();
    let mut peer_identity: Option<String> = None;
    let mut layers: Vec<PathBuf> = global_config_toml().into_iter().collect();
    for dir in config_roots(root).into_iter().rev() {
        layers.push(dir.join(".fno/config.toml"));
    }
    for path in layers {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(doc) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        let review = doc.get("review");
        if let Some(v) = review
            .and_then(|r| r.get("self_review_required"))
            .and_then(|v| v.as_bool())
        {
            self_review_required = v;
        }
        if let Some(rows) = review
            .and_then(|r| r.get("required_bots"))
            .and_then(|v| v.as_array())
        {
            required_bots = rows
                .iter()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect();
        }
        if let Some(rows) = review
            .and_then(|r| r.get("reviewers"))
            .and_then(|v| v.as_array())
        {
            reviewers = rows
                .iter()
                .filter_map(|r| r.as_str().map(str::to_string))
                .collect();
        }
        if let Some(v) = review
            .and_then(|r| r.get("peer_identity"))
            .and_then(|v| v.as_str())
        {
            peer_identity = Some(v.to_string());
        }
    }
    let mut keys = vec![
        format!(
            "review.self_review_required={}",
            bool_py(self_review_required)
        ),
        format!("review.required_bots={}", py_list(&required_bots)),
        format!("review.reviewers={}", py_list(&reviewers)),
        "no identity-free review.peers".to_string(),
    ];
    if let Some(pid) = &peer_identity {
        keys.push(format!("review.peer_identity={pid}"));
    }
    keys
}

/// Python `bool(...)` spelling for the keys lines.
fn bool_py(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

/// The pair check: auto_merge armed while the merge gate's own lane predicate
/// requires no review for a code payload (doctor.py:1678-1749). The lane
/// verdict comes from the native lane door, never a doctor-side recount; a
/// config the predicate cannot read fail-closes inside it, and every read
/// failure here degrades to no report.
pub(crate) fn auto_merge_review_gap(armed: &BTreeMap<String, i64>) -> Value {
    if load_doctor_config().auto_merge != Some(true) {
        return Value::Null;
    }
    let root = cwd_repo_root();
    let lane = command_call(
        &["review-coverage", "op"],
        json!({"op": "lane", "cwd": root.to_string_lossy(), "code_payload": true}),
        30,
    )
    .ok()
    .and_then(|p| p.get("lane").cloned());
    match lane {
        // Named lanes: the operator's policy, no gap.
        Some(l) if lane_truthy(&l) => return Value::Null,
        // Door failure: the deleted leg's except read no gap either.
        None => return Value::Null,
        Some(_) => {}
    }
    json!({
        "keys": read_review_keys(&root),
        "manifests": armed.values().sum::<i64>(),
        "breakdown": breakdown_line(armed),
        "remedy": "fno config set review.self_review_required true (or name a lane via review.required_bots / review.reviewers)",
    })
}

/// One merge-gating opt-out key with the value that reads as opted out.
struct OptoutKey {
    key: &'static str,
    /// The opt-out value as JSON (false, or the empty list for optional_apps).
    optout: Value,
    is_list: bool,
}

const MERGE_GATING_OPTOUTS: [OptoutKey; 3] = [
    OptoutKey {
        key: "review.self_review_required",
        optout: Value::Bool(false),
        is_list: false,
    },
    OptoutKey {
        key: "review.optional_apps",
        optout: Value::Array(Vec::new()),
        is_list: true,
    },
    OptoutKey {
        key: "auto_merge.require_checks_pass",
        optout: Value::Bool(false),
        is_list: false,
    },
];

/// The release command a holder runs to hand the opt-out back
/// (`optout_release_command` in the deleted leg).
fn optout_release_command(key: &str) -> String {
    if key == "review.optional_apps" {
        format!("fno config unset {key}")
    } else {
        format!("fno config set {key} true")
    }
}

/// Read the opt-out instrument, distinguishing unreadable from absent; an
/// unreadable instrument is NOT live, so it revokes the opt-out
/// (`_claim_state` in optout_lease.py).
fn optout_claim_state(key: &str) -> String {
    let Some(binary) = resolve_binary() else {
        return "unreadable".to_string();
    };
    let argv: Vec<String> = vec![
        binary,
        "claims".into(),
        "status".into(),
        format!("config-optout:{key}"),
        "--json".into(),
    ];
    let Some((code, out, _err)) = super::super::bounded_command_for(&argv, 10) else {
        return "unreadable".to_string();
    };
    if code != 0 {
        return "unreadable".to_string();
    }
    serde_json::from_str::<Value>(&out)
        .ok()
        .and_then(|v| v.get("state").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "unreadable".to_string())
}

/// Report opt-out values left in files without a live claim
/// (doctor.py:1751-1788). Explicit presence matters for optional_apps:
/// absent means the built-in list, while [] is the actual opt-out.
pub(crate) fn merge_gating_optout_report() -> Value {
    let mut residue: Vec<Value> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for path in config_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(raw) = toml::from_str::<toml::Value>(&text) else {
            continue;
        };
        for entry in MERGE_GATING_OPTOUTS.iter() {
            // _raw_leaf: walk the dotted path; absent or a non-matching
            // value is not an opt-out.
            let mut node = &raw;
            let mut present = true;
            for part in entry.key.split('.') {
                match node.get(part) {
                    Some(child) => node = child,
                    None => {
                        present = false;
                        break;
                    }
                }
            }
            let is_optout = if entry.is_list {
                node.as_array().map(|a| a.is_empty()) == Some(true)
            } else {
                node.as_bool() == entry.optout.as_bool()
            };
            if !present || !is_optout {
                continue;
            }
            let state = optout_claim_state(entry.key);
            let marker = format!("{}\x1f{}", path.display(), entry.key);
            if state != "live" && seen.insert(marker) {
                residue.push(json!({
                    "key": entry.key,
                    "path": path.display().to_string(),
                    "claim_state": state,
                    "command": optout_release_command(entry.key),
                }));
            }
        }
    }
    json!({ "residue": residue })
}
