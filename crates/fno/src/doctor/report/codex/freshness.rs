//! The codex plugin freshness verdict (codex_plugin.py:767-937).

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::super::codex_home;
use super::super::resolve_source;

const MARKETPLACE: &str = "footnote";
const PLUGIN_ID: &str = "fno@footnote";
const RELEASE_SOURCE: &str = "bllshttng/fno";
const RELEASE_REF: &str = "stable";
const LEGACY_DEV_MARKETPLACE: &str = "footnote-dev";
const OUTPUT_LIMIT: usize = 500;

fn owned_plugin_id(id: &str) -> bool {
    id == PLUGIN_ID || id == "fno@footnote-dev" || id == "fno@footnote-local"
}

fn remedy(channel: &str) -> String {
    if channel == "release" {
        return "fno config setup run --only harness-wiring".to_string();
    }
    "fno config plugin install codex --force".to_string()
}

fn same_release_source(source_type: &str, source: &str) -> bool {
    if source_type != "git" {
        return false;
    }
    let no_suffix = source.strip_suffix(".git").unwrap_or(source);
    let normalized = no_suffix
        .strip_prefix("https://github.com/")
        .unwrap_or(no_suffix);
    normalized == RELEASE_SOURCE
}

fn same_local_source(source_type: &str, source: &str, expected: &Path) -> bool {
    if source_type != "local" {
        return false;
    }
    std::fs::canonicalize(source)
        .map(|p| p == expected)
        .unwrap_or(false)
}

struct MarketplaceRow {
    name: String,
    source_type: String,
    source: String,
    root: String,
}

struct PluginRow {
    plugin_id: String,
    marketplace_name: String,
    version: String,
    installed: bool,
    enabled: bool,
    source_type: String,
    marketplace_source: String,
}

fn source_of(row: &Value) -> (String, String) {
    let src = row.get("marketplaceSource");
    let kind = src
        .and_then(|s| s.get("sourceType"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let target = src
        .and_then(|s| s.get("source"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    (kind.to_string(), target.to_string())
}

fn parse_state(
    marketplaces_json: &str,
    plugins_json: &str,
) -> Result<(Vec<MarketplaceRow>, Vec<PluginRow>), String> {
    let doc: Value = serde_json::from_str(marketplaces_json)
        .map_err(|e| format!("marketplace-list: malformed JSON: {e}"))?;
    if !doc.is_object() {
        return Err("marketplace-list: expected a JSON object".to_string());
    }
    let rows = doc
        .get("marketplaces")
        .and_then(Value::as_array)
        .ok_or("marketplace-list: missing marketplaces array")?;
    let mut marketplaces = Vec::new();
    for row in rows {
        let obj = row
            .as_object()
            .ok_or("marketplace-list: expected a JSON object")?;
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .ok_or("marketplace-list: marketplace missing name")?;
        let (kind, target) = source_of(row);
        marketplaces.push(MarketplaceRow {
            name: name.to_string(),
            source_type: kind,
            source: target,
            root: obj
                .get("root")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        });
    }
    parse_plugin_rows(plugins_json).map(|plugins| (marketplaces, plugins))
}

fn parse_plugin_rows(plugins_json: &str) -> Result<Vec<PluginRow>, String> {
    let doc: Value = serde_json::from_str(plugins_json)
        .map_err(|e| format!("plugin-list: malformed JSON: {e}"))?;
    if !doc.is_object() {
        return Err("plugin-list: expected a JSON object".to_string());
    }
    let rows = doc
        .get("installed")
        .and_then(Value::as_array)
        .ok_or("plugin-list: missing installed array")?;
    let mut plugins = Vec::new();
    for row in rows {
        let obj = row
            .as_object()
            .ok_or("plugin-list: expected a JSON object")?;
        let plugin_id = obj
            .get("pluginId")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty())
            .ok_or("plugin-list: installed plugin missing pluginId")?;
        let (kind, target) = source_of(row);
        let installed = obj
            .get("installed")
            .and_then(Value::as_bool)
            .ok_or("plugin-list: installed and enabled must be booleans")?;
        let enabled = obj
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or("plugin-list: installed and enabled must be booleans")?;
        if enabled && !installed {
            return Err("plugin-list: enabled plugin is not installed".to_string());
        }
        plugins.push(PluginRow {
            plugin_id: plugin_id.to_string(),
            marketplace_name: obj
                .get("marketplaceName")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            version: obj
                .get("version")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            installed,
            enabled,
            source_type: kind,
            marketplace_source: target,
        });
    }
    Ok(plugins)
}

fn run_codex(parts: &[&str], stage: &str) -> Result<(i32, String, String), String> {
    let owned: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    super::super::bounded_command_for(&owned, 120)
        .ok_or_else(|| format!("{stage}: failed to run codex CLI"))
}

fn tail_chars(s: &str, n: usize) -> String {
    let start = s
        .char_indices()
        .rev()
        .nth(n - 1)
        .map(|(i, _)| i)
        .unwrap_or(0);
    s[start..].to_string()
}

fn stage_err(stage: &str, stderr: &str, stdout: &str, code: i32) -> String {
    let mut detail = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    if detail.trim().is_empty() {
        detail = "exit 1";
    }
    format!("{}: {}", stage, tail_chars(detail, OUTPUT_LIMIT))
}

fn collect_state() -> Result<(Vec<MarketplaceRow>, Vec<PluginRow>), String> {
    let first = run_codex(
        &["codex", "plugin", "marketplace", "list", "--json"],
        "marketplace-list",
    )?;
    if first.0 != 0 {
        return Err(stage_err("marketplace-list", &first.2, &first.1, first.0));
    }
    let second = run_codex(&["codex", "plugin", "list", "--json"], "plugin-list")?;
    if second.0 != 0 {
        return Err(stage_err("plugin-list", &second.2, &second.1, second.0));
    }
    parse_state(&first.1, &second.1)
}

pub(crate) fn inspect_codex_plugin_freshness() -> Value {
    let home = codex_home();
    if let Some(v) = rollback_check(&home) {
        return v;
    }
    let outcome = read_marker(&home);
    let state = match collect_state() {
        Ok(s) => s,
        Err(e) => {
            return json!({
                "status": "unknown", "issue": "state-unreadable",
                "detail": tail_chars(&e, OUTPUT_LIMIT),
                "remedy": "fno config plugin install codex --force",
            })
        }
    };
    let (marketplaces, plugins) = state;
    let installed: Vec<&PluginRow> = plugins
        .iter()
        .filter(|p| p.installed && owned_plugin_id(&p.plugin_id))
        .collect();
    let enabled: Vec<&PluginRow> = installed.iter().filter(|p| p.enabled).copied().collect();
    let owned_markets = marketplaces
        .iter()
        .filter(|m| m.name == MARKETPLACE || m.name == LEGACY_DEV_MARKETPLACE)
        .collect::<Vec<_>>();
    let ambiguous = installed.len() > 1
        || enabled.len() > 1
        || installed.iter().any(|p| p.plugin_id != PLUGIN_ID)
        || owned_markets.len() > 1
        || owned_markets
            .iter()
            .any(|m| m.name == LEGACY_DEV_MARKETPLACE);
    if ambiguous {
        let installed_ids: Vec<Value> = installed.iter().map(|p| json!(p.plugin_id)).collect();
        let enabled_ids: Vec<Value> = enabled.iter().map(|p| json!(p.plugin_id)).collect();
        let channel = outcome
            .marker
            .as_ref()
            .and_then(|m| m.get("channel"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        return json!({
            "channel": channel,
            "installed_plugin_ids": installed_ids,
            "enabled_plugin_ids": enabled_ids,
            "status": "conflict",
            "issue": "ambiguous-duplicate-state",
            "remedy": remedy(channel),
        });
    }
    marker_gate(&outcome, &installed, &enabled, &marketplaces, &home)
}

fn marker_gate(
    outcome: &MarkerOutcome,
    installed: &[&PluginRow],
    enabled: &[&PluginRow],
    marketplaces: &[MarketplaceRow],
    home: &Path,
) -> Value {
    if let Some(err) = &outcome.error {
        if outcome.missing {
            return json!({
                "status": "unknown", "issue": "desired-channel-missing",
                "remedy": "fno config plugin install codex --force",
            });
        }
        return json!({
            "status": "unknown", "issue": "state-unreadable",
            "detail": tail_chars(err, OUTPUT_LIMIT),
            "remedy": "fno config plugin install codex --force",
        });
    }
    let marker = outcome.marker.clone().unwrap_or(Value::Null);
    let channel = marker
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let expected_source = marker
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let installed_ids: Vec<Value> = installed.iter().map(|p| json!(p.plugin_id)).collect();
    let enabled_ids: Vec<Value> = enabled.iter().map(|p| json!(p.plugin_id)).collect();
    let base = json!({
        "channel": channel,
        "marketplace": MARKETPLACE,
        "marketplace_source": expected_source,
        "installed_plugin_ids": installed_ids,
        "enabled_plugin_ids": enabled_ids,
        "remedy": remedy(channel),
    });
    base_gate(base, &channel, enabled, marketplaces, home, &marker)
}

fn base_gate(
    base: Value,
    channel: &str,
    enabled: &[&PluginRow],
    marketplaces: &[MarketplaceRow],
    home: &Path,
    marker: &Value,
) -> Value {
    if enabled.is_empty() {
        return merge(
            base,
            json!({"status": "missing", "issue": "plugin-missing"}),
        );
    }
    let selected = &enabled[0];
    if selected.plugin_id != PLUGIN_ID || selected.marketplace_name != MARKETPLACE {
        return merge(
            base,
            json!({"status": "wrong-channel", "issue": "wrong-channel"}),
        );
    }
    let marketplace = marketplaces.iter().find(|m| m.name == MARKETPLACE);
    let expected_source = marker
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source_ok = if channel == "release" {
        same_release_source(&selected.source_type, &selected.marketplace_source)
    } else {
        same_local_source(
            &selected.source_type,
            &selected.marketplace_source,
            Path::new(expected_source),
        )
    };
    let marketplace_ok = marketplace.is_some_and(|m| {
        if channel == "release" {
            same_release_source(&m.source_type, &m.source)
        } else {
            same_local_source(&m.source_type, &m.source, Path::new(expected_source))
        }
    });
    let ref_ok =
        channel != "release" || marker.get("ref").and_then(Value::as_str) == Some(RELEASE_REF);
    if !source_ok || !marketplace_ok || !ref_ok {
        return merge(
            base,
            json!({"status": "stale", "issue": "wrong-marketplace-source"}),
        );
    }
    payload_gate(base, channel, enabled, marketplace, home)
}

fn merge(base: Value, extra: Value) -> Value {
    let mut out = base.as_object().cloned().unwrap_or_default();
    if let Some(extra_obj) = extra.as_object() {
        for (k, v) in extra_obj {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out)
}

struct MarkerOutcome {
    marker: Option<Value>,
    missing: bool,
    error: Option<String>,
}

fn rollback_check(home: &Path) -> Option<Value> {
    let path = home.join("footnote/rollback-failure.json");
    if !path.is_file() {
        return None;
    }
    let text = std::fs::read_to_string(&path).ok()?;
    let payload: Value = serde_json::from_str(&text).ok()?;
    let detail = payload
        .get("detail")
        .and_then(Value::as_str)
        .unwrap_or("rollback did not complete");
    let channel = payload
        .get("channel")
        .and_then(Value::as_str)
        .filter(|c| *c == "release" || *c == "dev")
        .unwrap_or("release");
    Some(json!({
        "status": "error",
        "issue": "rollback-failure",
        "detail": tail_chars(detail, OUTPUT_LIMIT),
        "remedy": remedy(channel),
    }))
}

fn read_marker(home: &Path) -> MarkerOutcome {
    let path = home.join("footnote/plugin-channel.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return MarkerOutcome {
                marker: None,
                missing: true,
                error: None,
            };
        }
        Err(e) => {
            return MarkerOutcome {
                marker: None,
                missing: false,
                error: Some(e.to_string()),
            };
        }
    };
    let payload: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            return MarkerOutcome {
                marker: None,
                missing: false,
                error: Some(e.to_string()),
            };
        }
    };
    if !payload.is_object() {
        return MarkerOutcome {
            marker: None,
            missing: false,
            error: Some("desired-channel-marker: expected a JSON object".into()),
        };
    }
    let channel = payload
        .get("channel")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if channel != "release" && channel != "dev" {
        return MarkerOutcome {
            marker: None,
            missing: false,
            error: Some("desired-channel-marker: missing or unsupported channel".into()),
        };
    }
    let expected_source = payload
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source_ok = if channel == "release" {
        same_release_source("git", expected_source)
    } else {
        !expected_source.is_empty()
    };
    let marketplace_ok = payload.get("marketplace").and_then(Value::as_str) == Some(MARKETPLACE);
    if !marketplace_ok || !source_ok {
        return MarkerOutcome {
            marker: None,
            missing: false,
            error: Some("desired-channel-marker: channel contract does not match marker".into()),
        };
    }
    MarkerOutcome {
        marker: Some(payload),
        missing: false,
        error: None,
    }
}

fn payload_gate(
    base: Value,
    channel: &str,
    enabled: &[&PluginRow],
    marketplace: Option<&MarketplaceRow>,
    home: &Path,
) -> Value {
    let canonical = canonical_source_root(marketplace, channel);
    let Some(canonical) = canonical else {
        return merge(
            base,
            json!({"status": "unknown", "issue": "source-unreadable", "detail": "plugin root not resolved"}),
        );
    };
    let Some(source_version) = manifest_version(&canonical) else {
        return merge(
            base,
            json!({"status": "unknown", "issue": "source-unreadable", "detail": "unreadable manifest"}),
        );
    };
    let selected = &enabled[0];
    let cache_version = selected.version.clone();
    if source_version != cache_version {
        return merge(
            base,
            json!({
                "source_version": source_version,
                "cache_version": cache_version,
                "status": "stale",
                "issue": "version-mismatch",
            }),
        );
    }
    let cache = home
        .join("plugins/cache")
        .join(MARKETPLACE)
        .join("fno")
        .join(&cache_version);
    if !cache.is_dir() {
        return merge(
            base,
            json!({
                "source_version": source_version,
                "cache_version": cache_version,
                "status": "missing",
                "issue": "cache-missing",
            }),
        );
    }
    let source_result = payload_digest(&canonical);
    let cache_result = payload_digest(&cache);
    let (Ok(source_hex), Ok(cache_hex)) = (source_result, cache_result) else {
        return merge(
            base,
            json!({"status": "unknown", "issue": "digest-unreadable", "detail": "digest walk failed"}),
        );
    };
    if source_hex != cache_hex {
        return merge(
            base,
            json!({
                "source_version": source_version,
                "cache_version": cache_version,
                "source_digest": source_hex,
                "cache_digest": cache_hex,
                "status": "stale",
                "issue": "payload-drift",
            }),
        );
    }
    merge(base, json!({"status": "fresh", "issue": Value::Null}))
}

fn canonical_source_root(marketplace: Option<&MarketplaceRow>, channel: &str) -> Option<PathBuf> {
    if channel == "release" {
        if let Some(root) = marketplace
            .map(|m| m.root.as_str())
            .filter(|r| !r.is_empty())
        {
            return Some(PathBuf::from(root));
        }
    }
    env_or_source_walk()
}

fn env_or_source_walk() -> Option<PathBuf> {
    for var in ["CLAUDE_PLUGIN_ROOT", "PLUGIN_ROOT"] {
        if let Some(val) = std::env::var_os(var) {
            let root = PathBuf::from(val);
            if root.join(".codex-plugin/plugin.json").is_file() {
                return Some(root);
            }
        }
    }
    let src = resolve_source(None)?;
    for parent in std::iter::successors(Some(src), |p| p.parent().map(|x| x.to_path_buf())) {
        if parent.join(".codex-plugin/plugin.json").is_file() {
            return Some(parent);
        }
    }
    None
}

fn manifest_version(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(".codex-plugin/plugin.json")).ok()?;
    let payload: Value = serde_json::from_str(&text).ok()?;
    payload
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn payload_digest(root: &Path) -> Result<String, ()> {
    use sha2::{Digest, Sha256};
    let mut candidates: Vec<PathBuf> = vec![root.join(".codex-plugin/plugin.json")];
    for relative in [
        "skills",
        "agents",
        "commands",
        "hooks",
        "scripts",
        ".codex/agents",
    ] {
        let base = root.join(relative);
        let Ok(read) = std::fs::read_dir(&base) else {
            continue;
        };
        walk_digest_candidates(&base, read, &mut candidates);
    }
    candidates.retain(|p| p.is_file());
    candidates.sort_by_key(|p| {
        p.strip_prefix(root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    let mut hasher = Sha256::new();
    for path in &candidates {
        let relative = path
            .strip_prefix(root)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_default();
        let payload = std::fs::read(path).map_err(|_| ())?;
        hasher.update((relative.len() as u64).to_be_bytes());
        hasher.update(relative.as_bytes());
        hasher.update((payload.len() as u64).to_be_bytes());
        hasher.update(&payload);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn walk_digest_candidates(dir: &Path, read: std::fs::ReadDir, out: &mut Vec<PathBuf>) {
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(inner) = std::fs::read_dir(&path) {
                walk_digest_candidates(&path, inner, out);
            }
            continue;
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name == ".DS_Store" || name == "__pycache__" {
            continue;
        }
        if matches!(path.extension(), Some(ext) if ext == "pyc" || ext == "pyo") {
            continue;
        }
        out.push(path);
    }
}
