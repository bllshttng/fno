//! The codex arm of `fno-agents plugin-install`: converge Codex onto one
//! footnote plugin identity, `fno@fno` from the `fno` marketplace, on the dev
//! channel (the filtered stage) or the release channel (GitHub `stable`).
//!
//! A switch is a transaction. The candidate is first proven in a throwaway
//! CODEX_HOME. The live state is then snapshotted, and a failure after the
//! first mutation restores the marketplaces, plugins, owned config tables,
//! caches and the marker bytes. A rollback that fails itself is persisted as
//! `footnote/rollback-failure.json`, and the freshness verdict names it first.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub const MARKETPLACE: &str = "fno";
pub const PLUGIN_ID: &str = "fno@fno";
/// The plugin's directory inside a marketplace cache. Named, not a bare
/// literal: the seam-crossings ratchet reads a bare join of it as a binary
/// resolver, and this joins a directory.
const PLUGIN_DIR: &str = "fno";
const RELEASE_SOURCE: &str = "bllshttng/fno";
/// The bare repo's default branch is slow to clone and can time out.
const RELEASE_REF: &str = "stable";
const LEGACY_DEV_MARKETPLACE: &str = "footnote-dev";
/// `footnote` is the marketplace name before the rename to `fno`. Sorted: the
/// quarantine walks them in this order.
const LEGACY_MARKETPLACES: [&str; 2] = ["footnote", "footnote-dev"];
const OWNED_MARKETPLACES: [&str; 3] = ["fno", "footnote", "footnote-dev"];
const LEGACY_PLUGIN_IDS: [&str; 3] = ["fno@footnote-dev", "fno@footnote-local", "fno@footnote"];
const OWNED_PLUGIN_IDS: [&str; 4] = [
    "fno@fno",
    "fno@footnote-dev",
    "fno@footnote-local",
    "fno@footnote",
];
const OUTPUT_LIMIT: usize = 500;
const DEV_REMEDY: &str = "fno config plugin install codex --force";

fn remedy(channel: &str) -> &'static str {
    // The dev install refuses a release marker; setup converges release.
    if channel == "release" {
        "fno config setup run --only harness-wiring"
    } else {
        DEV_REMEDY
    }
}

/// A failed step, named by stage. The detail keeps the last 500 characters.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvergeError {
    pub stage: String,
    pub detail: String,
}

impl ConvergeError {
    pub fn new(stage: &str, detail: impl AsRef<str>) -> Self {
        let trimmed = detail.as_ref().trim();
        let detail = if trimmed.is_empty() {
            "no output"
        } else {
            trimmed
        };
        Self {
            stage: stage.to_string(),
            detail: tail(detail, OUTPUT_LIMIT),
        }
    }
}

impl std::fmt::Display for ConvergeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.stage, self.detail)
    }
}

type Res<T> = Result<T, ConvergeError>;

fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(limit)).collect()
}

/// What one codex invocation returned.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Runs one argv with `CODEX_HOME` set to the given home. Tests pass a fake.
pub type Runner<'a> = &'a mut dyn FnMut(&[String], &Path) -> Ran;

/// The real runner: the named binary under a 120s wall-clock budget.
pub fn codex_runner(argv: &[String], home: &Path) -> Ran {
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]).env("CODEX_HOME", home);
    match crate::bounded_cmd::output_with_timeout_result(cmd, 120) {
        Ok(out) => Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        },
        Err(e) => Ran {
            code: 1,
            stdout: String::new(),
            stderr: e.to_string(),
        },
    }
}

fn run(r: Runner, stage: &str, argv: &[&str], home: &Path) -> Res<String> {
    let argv: Vec<String> = argv.iter().map(|a| a.to_string()).collect();
    let out = r(&argv, home);
    if out.code != 0 {
        let detail = [out.stderr, out.stdout]
            .into_iter()
            .find(|t| !t.trim().is_empty())
            .unwrap_or_else(|| format!("exit {}", out.code));
        return Err(ConvergeError::new(stage, detail));
    }
    Ok(out.stdout)
}

// --- state -------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
struct Marketplace {
    name: String,
    source_type: String,
    source: String,
    root: String,
}

#[derive(Debug, Clone, PartialEq)]
struct Plugin {
    plugin_id: String,
    marketplace_name: String,
    version: String,
    installed: bool,
    enabled: bool,
    source_type: String,
    marketplace_source: String,
}

#[derive(Debug, Clone, Default)]
struct State {
    marketplaces: Vec<Marketplace>,
    plugins: Vec<Plugin>,
}

fn object<'v>(raw: &'v Value, stage: &str) -> Res<&'v Map<String, Value>> {
    raw.as_object()
        .ok_or_else(|| ConvergeError::new(stage, "expected a JSON object"))
}

fn text(row: &Map<String, Value>, key: &str) -> String {
    match row.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
    }
}

fn source_of(raw: Option<&Value>) -> (String, String) {
    match raw.and_then(Value::as_object) {
        Some(src) => (text(src, "sourceType"), text(src, "source")),
        None => (String::new(), String::new()),
    }
}

fn parse_doc(raw: &str, stage: &str) -> Res<Value> {
    serde_json::from_str(raw).map_err(|e| ConvergeError::new(stage, format!("malformed JSON: {e}")))
}

fn parse_state(marketplaces_json: &str, plugins_json: &str) -> Res<State> {
    let market_doc = parse_doc(marketplaces_json, "marketplace-list")?;
    let plugin_doc = parse_doc(plugins_json, "plugin-list")?;
    let market_rows = object(&market_doc, "marketplace-list")?
        .get("marketplaces")
        .and_then(Value::as_array)
        .ok_or_else(|| ConvergeError::new("marketplace-list", "missing marketplaces array"))?;
    let plugin_rows = object(&plugin_doc, "plugin-list")?
        .get("installed")
        .and_then(Value::as_array)
        .ok_or_else(|| ConvergeError::new("plugin-list", "missing installed array"))?;
    let mut state = State::default();
    for row in market_rows {
        let item = object(row, "marketplace-list")?;
        let name = item.get("name").and_then(Value::as_str).unwrap_or_default();
        if name.is_empty() {
            return Err(ConvergeError::new(
                "marketplace-list",
                "marketplace missing name",
            ));
        }
        let (source_type, source) = source_of(item.get("marketplaceSource"));
        state.marketplaces.push(Marketplace {
            name: name.to_string(),
            source_type,
            source,
            root: text(item, "root"),
        });
    }
    for row in plugin_rows {
        let item = object(row, "plugin-list")?;
        let plugin_id = item
            .get("pluginId")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if plugin_id.is_empty() {
            return Err(ConvergeError::new(
                "plugin-list",
                "installed plugin missing pluginId",
            ));
        }
        let (Some(installed), Some(enabled)) = (
            item.get("installed").and_then(Value::as_bool),
            item.get("enabled").and_then(Value::as_bool),
        ) else {
            return Err(ConvergeError::new(
                "plugin-list",
                "installed and enabled must be booleans",
            ));
        };
        if enabled && !installed {
            return Err(ConvergeError::new(
                "plugin-list",
                "enabled plugin is not installed",
            ));
        }
        let (source_type, marketplace_source) = source_of(item.get("marketplaceSource"));
        state.plugins.push(Plugin {
            plugin_id: plugin_id.to_string(),
            marketplace_name: text(item, "marketplaceName"),
            version: text(item, "version"),
            installed,
            enabled,
            source_type,
            marketplace_source,
        });
    }
    Ok(state)
}

fn collect(r: Runner, home: &Path) -> Res<State> {
    let markets = run(
        r,
        "marketplace-list",
        &["codex", "plugin", "marketplace", "list", "--json"],
        home,
    )?;
    let plugins = run(
        r,
        "plugin-list",
        &["codex", "plugin", "list", "--json"],
        home,
    )?;
    parse_state(&markets, &plugins)
}

fn owned_plugin(id: &str) -> bool {
    OWNED_PLUGIN_IDS.contains(&id)
}

fn owned_marketplace(name: &str) -> bool {
    OWNED_MARKETPLACES.contains(&name)
}

// --- sources -----------------------------------------------------------------

fn expand(source: &str) -> PathBuf {
    match source.strip_prefix("~/") {
        Some(rest) => crate::paths::dirs_home().join(rest),
        None => PathBuf::from(source),
    }
}

fn resolved(p: &Path) -> PathBuf {
    fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn same_release_source(source_type: &str, source: &str) -> bool {
    let trimmed = source.strip_suffix(".git").unwrap_or(source);
    let trimmed = trimmed
        .strip_prefix("https://github.com/")
        .unwrap_or(trimmed);
    source_type == "git" && trimmed == RELEASE_SOURCE
}

fn same_local_source(source_type: &str, source: &str, expected: &Path) -> bool {
    source_type == "local" && resolved(&expand(source)) == resolved(expected)
}

/// The source a channel installs from: GitHub for release, a local root for dev.
#[derive(Clone, Copy)]
enum Want<'a> {
    Release,
    Local(&'a Path),
}

impl Want<'_> {
    fn matches(&self, source_type: &str, source: &str) -> bool {
        match self {
            Want::Release => same_release_source(source_type, source),
            Want::Local(root) => same_local_source(source_type, source, root),
        }
    }
}

fn marketplace_add_argv<'a>(source: &'a str, git_ref: Option<&'a str>) -> Vec<&'a str> {
    let mut argv = vec!["codex", "plugin", "marketplace", "add", source];
    if let Some(r) = git_ref {
        argv.extend(["--ref", r]);
    }
    argv.push("--json");
    argv
}

fn cache_dir(home: &Path, marketplace: &str) -> PathBuf {
    home.join("plugins").join("cache").join(marketplace)
}

fn version_cache(home: &Path, version: &str) -> PathBuf {
    cache_dir(home, MARKETPLACE).join(PLUGIN_DIR).join(version)
}

// --- payload -----------------------------------------------------------------

fn manifest_version(root: &Path) -> Res<String> {
    let raw = fs::read_to_string(root.join(".codex-plugin").join("plugin.json"))
        .map_err(|e| ConvergeError::new("plugin-manifest", e.to_string()))?;
    let doc: Value = serde_json::from_str(&raw)
        .map_err(|e| ConvergeError::new("plugin-manifest", e.to_string()))?;
    match doc.get("version") {
        Some(Value::String(v)) if !v.is_empty() => Ok(v.clone()),
        Some(Value::String(_)) => Err(ConvergeError::new("plugin-manifest", "missing version")),
        _ => Err(ConvergeError::new("plugin-manifest", "'version'")),
    }
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            walk_files(&path, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// Hash only the files Codex can load from the plugin payload.
pub fn payload_digest(root: &Path) -> Result<String, ConvergeError> {
    let mut files = vec![root.join(".codex-plugin").join("plugin.json")];
    for rel in [
        "skills",
        "agents",
        "commands",
        "hooks",
        "scripts",
        ".codex/agents",
    ] {
        let mut found = Vec::new();
        walk_files(&root.join(rel), &mut found);
        files.extend(found.into_iter().filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            !p.components().any(|c| c.as_os_str() == "__pycache__")
                && !name.ends_with(".pyc")
                && !name.ends_with(".pyo")
                && name != ".DS_Store"
        }));
    }
    let mut keyed: Vec<(String, PathBuf)> = files
        .into_iter()
        .filter(|p| p.is_file())
        .filter_map(|p| {
            let rel = p.strip_prefix(root).ok()?.to_string_lossy().into_owned();
            Some((rel, p))
        })
        .collect();
    keyed.sort();
    let mut digest = Sha256::new();
    for (rel, path) in keyed {
        let payload =
            fs::read(&path).map_err(|e| ConvergeError::new("plugin-digest", e.to_string()))?;
        digest.update((rel.len() as u64).to_be_bytes());
        digest.update(rel.as_bytes());
        digest.update((payload.len() as u64).to_be_bytes());
        digest.update(&payload);
    }
    Ok(format!("{:x}", digest.finalize()))
}

// --- owned files -------------------------------------------------------------

fn owned_dir(home: &Path) -> PathBuf {
    home.join("footnote")
}

fn marker_path(home: &Path) -> PathBuf {
    owned_dir(home).join("plugin-channel.json")
}

fn receipt_path(home: &Path) -> PathBuf {
    owned_dir(home).join("rollback-failure.json")
}

/// Write through a sibling temp file and a rename, so a reader never sees
/// half a file. The temp is removed on failure.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let temp = parent.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let written = fs::File::create(&temp)
        .and_then(|mut f| f.write_all(bytes))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

fn read_optional(path: &Path, stage: &str) -> Res<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConvergeError::new(stage, e.to_string())),
    }
}

/// Put back exact bytes, or remove the file when there were none.
fn restore_bytes(path: &Path, payload: Option<&[u8]>, stage: &str) -> Res<()> {
    let restored = match payload {
        None => match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
        Some(bytes) => write_atomic(path, bytes),
    };
    restored.map_err(|e| ConvergeError::new(stage, e.to_string()))
}

fn write_marker(home: &Path, channel: &str, source: &str, git_ref: Option<&str>) -> Res<()> {
    let payload = json!({
        "channel": channel,
        "marketplace": MARKETPLACE,
        "ref": git_ref,
        "source": source,
    });
    write_atomic(&marker_path(home), format!("{payload}\n").as_bytes())
        .map_err(|e| ConvergeError::new("desired-channel-marker", e.to_string()))
}

fn write_rollback_receipt(
    home: &Path,
    original: &ConvergeError,
    detail: &str,
    channel: &str,
) -> Res<()> {
    let payload = json!({
        "channel": channel,
        "detail": tail(detail, OUTPUT_LIMIT),
        "stage": original.stage,
    });
    write_atomic(&receipt_path(home), format!("{payload}\n").as_bytes())
        .map_err(|e| ConvergeError::new("rollback-receipt", e.to_string()))
}

/// The footnote-owned tables of `config.toml`, plus the whole document and
/// its exact bytes, so a rollback can prove nothing else moved.
#[derive(Clone)]
struct OwnedConfig {
    marketplaces: toml::Table,
    plugins: toml::Table,
    document: toml::Table,
    payload: Option<Vec<u8>>,
}

fn owned_tables(document: &toml::Table, stage: &str) -> Res<(toml::Table, toml::Table)> {
    let pick = |key: &str, owned: fn(&str) -> bool| -> Res<toml::Table> {
        match document.get(key) {
            None => Ok(toml::Table::new()),
            Some(toml::Value::Table(t)) => Ok(t
                .iter()
                .filter(|(k, _)| owned(k))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()),
            Some(_) => Err(ConvergeError::new(stage, "owned tables are not objects")),
        }
    };
    Ok((
        pick("marketplaces", owned_marketplace)?,
        pick("plugins", owned_plugin)?,
    ))
}

fn parse_config(payload: Option<&[u8]>, stage: &str) -> Res<toml::Table> {
    let Some(bytes) = payload else {
        return Ok(toml::Table::new());
    };
    let text = std::str::from_utf8(bytes).map_err(|e| ConvergeError::new(stage, e.to_string()))?;
    text.parse::<toml::Table>()
        .map_err(|e| ConvergeError::new(stage, e.to_string()))
}

fn read_config_payload(home: &Path, stage: &str) -> Res<Option<Vec<u8>>> {
    let path = home.join("config.toml");
    if !path.is_file() {
        return Ok(None);
    }
    fs::read(&path)
        .map(Some)
        .map_err(|e| ConvergeError::new(stage, e.to_string()))
}

fn read_owned_config(home: &Path) -> Res<OwnedConfig> {
    const STAGE: &str = "codex-config-snapshot";
    let payload = read_config_payload(home, STAGE)?;
    let document = parse_config(payload.as_deref(), STAGE)?;
    let (marketplaces, plugins) = owned_tables(&document, STAGE)?;
    Ok(OwnedConfig {
        marketplaces,
        plugins,
        document,
        payload,
    })
}

fn without_owned(document: &toml::Table) -> toml::Table {
    let mut result = document.clone();
    for (key, owned) in [
        ("marketplaces", owned_marketplace as fn(&str) -> bool),
        ("plugins", owned_plugin),
    ] {
        if let Some(toml::Value::Table(t)) = result.get_mut(key) {
            let gone: Vec<String> = t.keys().filter(|k| owned(k)).cloned().collect();
            for k in gone {
                t.remove(&k);
            }
            if t.is_empty() {
                result.remove(key);
            }
        }
    }
    result
}

fn restore_owned_config(home: &Path, snapshot: &OwnedConfig) -> Res<()> {
    const STAGE: &str = "rollback-config";
    let current_payload = read_config_payload(home, STAGE)?;
    let document = parse_config(current_payload.as_deref(), STAGE)?;
    let (marketplaces, plugins) = owned_tables(&document, STAGE)?;
    if marketplaces == snapshot.marketplaces && plugins == snapshot.plugins {
        return Ok(());
    }
    if without_owned(&document) != without_owned(&snapshot.document) {
        return Err(ConvergeError::new(
            STAGE,
            "non-Footnote config changed during convergence",
        ));
    }
    restore_bytes(
        &home.join("config.toml"),
        snapshot.payload.as_deref(),
        STAGE,
    )
}

// --- caches ------------------------------------------------------------------

struct Quarantine {
    original: PathBuf,
    quarantine: PathBuf,
}

fn refuse_non_dir(path: &Path, stage: &str) -> Res<bool> {
    if path.is_symlink() || (path.exists() && !path.is_dir()) {
        return Err(ConvergeError::new(
            stage,
            format!("refusing non-directory {}", path.display()),
        ));
    }
    Ok(path.exists())
}

fn quarantine_cache(home: &Path, marketplace: &str) -> Res<Option<Quarantine>> {
    const STAGE: &str = "cache-quarantine";
    let cache = cache_dir(home, marketplace);
    if !refuse_non_dir(&cache, STAGE)? {
        return Ok(None);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    // 32 lowercase hex digits: the shape reclaim's codex_cache_quarantines
    // lane sweeps, so a quarantine a crash leaves behind is not kept forever.
    let token = nanos ^ (u128::from(std::process::id()) << 96);
    let quarantine = owned_dir(home).join(format!(".{marketplace}.{token:032x}"));
    fs::rename(&cache, &quarantine).map_err(|e| ConvergeError::new(STAGE, e.to_string()))?;
    Ok(Some(Quarantine {
        original: cache,
        quarantine,
    }))
}

fn restore_cache(item: &Quarantine) -> Res<()> {
    if !item.quarantine.exists() {
        return Ok(());
    }
    let put_back = (|| {
        if item.original.exists() {
            fs::remove_dir_all(&item.original)?;
        }
        fs::rename(&item.quarantine, &item.original)
    })();
    put_back.map_err(|e| ConvergeError::new("rollback-cache", e.to_string()))
}

// --- rollback ----------------------------------------------------------------

struct Snapshot {
    state: State,
    marker: Option<Vec<u8>>,
    receipt: Option<Vec<u8>>,
    config: OwnedConfig,
    cache_names: Vec<&'static str>,
}

fn normalized_source(source_type: &str, source: &str) -> (String, String) {
    if same_release_source(source_type, source) {
        return ("git".into(), RELEASE_SOURCE.into());
    }
    if source_type == "local" {
        return (
            "local".into(),
            resolved(&expand(source)).display().to_string(),
        );
    }
    (source_type.into(), source.into())
}

/// Every footnote-owned registration, sorted, so two states compare equal
/// exactly when the owned part of Codex is the same.
fn fingerprint(state: &State) -> Vec<Vec<String>> {
    let mut markets: Vec<Vec<String>> = state
        .marketplaces
        .iter()
        .filter(|m| owned_marketplace(&m.name))
        .map(|m| {
            let (t, s) = normalized_source(&m.source_type, &m.source);
            vec!["marketplace".into(), m.name.clone(), t, s]
        })
        .collect();
    let mut plugins: Vec<Vec<String>> = state
        .plugins
        .iter()
        .filter(|p| owned_plugin(&p.plugin_id))
        .map(|p| {
            let (t, s) = normalized_source(&p.source_type, &p.marketplace_source);
            vec![
                "plugin".into(),
                p.plugin_id.clone(),
                p.marketplace_name.clone(),
                p.version.clone(),
                p.installed.to_string(),
                p.enabled.to_string(),
                t,
                s,
            ]
        })
        .collect();
    markets.sort();
    plugins.sort();
    markets.extend(plugins);
    markets
}

fn verify_owned_bytes(home: &Path, snapshot: &Snapshot) -> Res<()> {
    const STAGE: &str = "rollback-final-verify";
    if read_optional(&marker_path(home), STAGE)? != snapshot.marker {
        return Err(ConvergeError::new(STAGE, "marker bytes differ"));
    }
    if read_optional(&receipt_path(home), STAGE)? != snapshot.receipt {
        return Err(ConvergeError::new(STAGE, "rollback receipt bytes differ"));
    }
    Ok(())
}

fn restore_owned_files(home: &Path, snapshot: &Snapshot, quarantines: &[Quarantine]) -> Res<()> {
    for item in quarantines {
        restore_cache(item)?;
    }
    restore_bytes(
        &marker_path(home),
        snapshot.marker.as_deref(),
        "rollback-marker",
    )?;
    restore_bytes(
        &receipt_path(home),
        snapshot.receipt.as_deref(),
        "rollback-receipt-restore",
    )
}

fn restore_snapshot(
    r: Runner,
    home: &Path,
    snapshot: &Snapshot,
    quarantines: &[Quarantine],
) -> Res<()> {
    let expected = fingerprint(&snapshot.state);
    let current = collect(r, home)?;
    let config_saved =
        !snapshot.config.marketplaces.is_empty() || !snapshot.config.plugins.is_empty();
    if fingerprint(&current) == expected {
        if config_saved {
            restore_owned_config(home, &snapshot.config)?;
        }
        restore_owned_files(home, snapshot, quarantines)?;
        if fingerprint(&collect(r, home)?) != expected {
            return Err(ConvergeError::new(
                "rollback-final-verify",
                "owned state changed during rollback",
            ));
        }
        return verify_owned_bytes(home, snapshot);
    }
    for plugin in current
        .plugins
        .iter()
        .filter(|p| p.installed && owned_plugin(&p.plugin_id))
    {
        let argv = [
            "codex",
            "plugin",
            "remove",
            plugin.plugin_id.as_str(),
            "--json",
        ];
        run(r, "rollback-plugin-remove", &argv, home)?;
    }
    for market in collect(r, home)?
        .marketplaces
        .iter()
        .filter(|m| owned_marketplace(&m.name))
    {
        let argv = [
            "codex",
            "plugin",
            "marketplace",
            "remove",
            market.name.as_str(),
            "--json",
        ];
        run(r, "rollback-marketplace-remove", &argv, home)?;
    }
    for market in snapshot
        .state
        .marketplaces
        .iter()
        .filter(|m| owned_marketplace(&m.name))
    {
        let argv = [
            "codex",
            "plugin",
            "marketplace",
            "add",
            market.source.as_str(),
            "--json",
        ];
        run(r, "rollback-marketplace-add", &argv, home)?;
    }
    // A legacy identity Codex can no longer re-add is restored through its
    // config tables instead; only the current identity must re-add.
    let mut legacy_error: Option<ConvergeError> = None;
    for plugin in snapshot
        .state
        .plugins
        .iter()
        .filter(|p| p.installed && owned_plugin(&p.plugin_id))
    {
        let argv = [
            "codex",
            "plugin",
            "add",
            plugin.plugin_id.as_str(),
            "--json",
        ];
        if let Err(e) = run(r, "rollback-plugin-add", &argv, home) {
            if !LEGACY_PLUGIN_IDS.contains(&plugin.plugin_id.as_str()) {
                return Err(e);
            }
            legacy_error = Some(e);
        }
    }
    if config_saved || legacy_error.is_some() {
        restore_owned_config(home, &snapshot.config)?;
    }
    for name in OWNED_MARKETPLACES
        .iter()
        .filter(|n| !snapshot.cache_names.contains(*n))
    {
        let introduced = cache_dir(home, name);
        if refuse_non_dir(&introduced, "rollback-cache")? {
            fs::remove_dir_all(&introduced)
                .map_err(|e| ConvergeError::new("rollback-cache", e.to_string()))?;
        }
    }
    restore_owned_files(home, snapshot, quarantines)?;
    let found = fingerprint(&collect(r, home)?);
    if found != expected {
        let mut detail = format!("expected state={expected:?}; found={found:?}");
        if let Some(e) = legacy_error {
            detail.push_str(&format!("; legacy plugin restore failed: {e}"));
        }
        return Err(ConvergeError::new("rollback-final-verify", detail));
    }
    verify_owned_bytes(home, snapshot)
}

// --- candidate ---------------------------------------------------------------

struct Candidate {
    version: String,
    digest: String,
}

/// Prove Codex can parse and install the candidate without touching live state.
fn validate_candidate(
    r: Runner,
    want: Want,
    source: &str,
    git_ref: Option<&str>,
) -> Res<Candidate> {
    let scratch = tempfile::Builder::new()
        .prefix("fno-codex-candidate-")
        .tempdir()
        .map_err(|e| ConvergeError::new("candidate-home", e.to_string()))?;
    let home = scratch.path();
    run(
        r,
        "candidate-marketplace-add",
        &marketplace_add_argv(source, git_ref),
        home,
    )?;
    let listed = run(
        r,
        "candidate-plugin-list",
        &[
            "codex",
            "plugin",
            "list",
            "--available",
            "--marketplace",
            MARKETPLACE,
            "--json",
        ],
        home,
    )?;
    let doc = parse_doc(&listed, "candidate-plugin-list")?;
    let rows = object(&doc, "candidate-plugin-list")?
        .get("available")
        .and_then(Value::as_array)
        .ok_or_else(|| ConvergeError::new("candidate-plugin-list", "missing available array"))?;
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|row| row.get("pluginId").and_then(Value::as_str) == Some(PLUGIN_ID))
        .collect();
    let versioned = matches.len() == 1
        && matches[0]
            .get("version")
            .is_some_and(|v| !v.is_null() && v.as_str() != Some(""));
    if !versioned {
        return Err(ConvergeError::new(
            "candidate-plugin-list",
            format!(
                "expected one available {PLUGIN_ID}; found {}",
                matches.len()
            ),
        ));
    }
    run(
        r,
        "candidate-plugin-add",
        &["codex", "plugin", "add", PLUGIN_ID, "--json"],
        home,
    )?;
    let state = collect(r, home)?;
    let selected: Vec<&Plugin> = state
        .plugins
        .iter()
        .filter(|p| p.installed && p.enabled && p.plugin_id == PLUGIN_ID)
        .collect();
    let market = state.marketplaces.iter().find(|m| m.name == MARKETPLACE);
    let verified = match (selected.as_slice(), market) {
        ([plugin], Some(m)) => {
            !m.root.is_empty()
                && want.matches(&m.source_type, &m.source)
                && want.matches(&plugin.source_type, &plugin.marketplace_source)
        }
        _ => false,
    };
    let (true, Some(market)) = (verified, market) else {
        return Err(ConvergeError::new(
            "candidate-final-verify",
            format!("expected one enabled {PLUGIN_ID}"),
        ));
    };
    let version = selected[0].version.clone();
    let cache = version_cache(home, &version);
    let digest = payload_digest(Path::new(&market.root))?;
    if !cache.is_dir() || digest != payload_digest(&cache)? {
        return Err(ConvergeError::new(
            "candidate-final-verify",
            "candidate cache payload differs from source",
        ));
    }
    Ok(Candidate { version, digest })
}

// --- freshness ---------------------------------------------------------------

/// The advisory plugin verdict `fno doctor` and the spawn gate read. It is
/// independent of CLI freshness. The dev channel compares the cache with the
/// local root its marker records; release compares with the marketplace root.
pub fn inspect(r: Runner, home: &Path) -> Value {
    if let Ok(raw) = fs::read_to_string(receipt_path(home)) {
        let (detail, channel) = match serde_json::from_str::<Value>(&raw) {
            Ok(Value::Object(doc)) => {
                let detail = doc
                    .get("detail")
                    .and_then(Value::as_str)
                    .unwrap_or("rollback did not complete")
                    .to_string();
                let channel = match doc.get("channel").and_then(Value::as_str) {
                    Some("dev") => "dev",
                    _ => "release",
                };
                (detail, channel)
            }
            Ok(_) => ("rollback-failure: expected a JSON object".into(), "release"),
            Err(e) => (e.to_string(), "release"),
        };
        return json!({
            "status": "error",
            "issue": "rollback-failure",
            "detail": tail(&detail, OUTPUT_LIMIT),
            "remedy": remedy(channel),
        });
    }
    let marker = read_marker(home);
    let state = match collect(r, home) {
        Ok(state) => state,
        Err(e) => {
            return json!({
                "status": "unknown",
                "issue": "state-unreadable",
                "detail": tail(&e.to_string(), OUTPUT_LIMIT),
                "remedy": DEV_REMEDY,
            })
        }
    };
    let installed: Vec<&Plugin> = state
        .plugins
        .iter()
        .filter(|p| p.installed && owned_plugin(&p.plugin_id))
        .collect();
    let enabled: Vec<&Plugin> = installed.iter().copied().filter(|p| p.enabled).collect();
    let ids =
        |rows: &[&Plugin]| -> Vec<String> { rows.iter().map(|p| p.plugin_id.clone()).collect() };
    let owned_markets: Vec<&Marketplace> = state
        .marketplaces
        .iter()
        .filter(|m| owned_marketplace(&m.name))
        .collect();
    let marker_channel = |fallback: &str| -> String {
        marker
            .as_ref()
            .ok()
            .and_then(|m| m.get("channel").and_then(Value::as_str))
            .unwrap_or(fallback)
            .to_string()
    };
    if ids(&installed) == ["fno@footnote"] && owned_markets.len() <= 1 {
        let channel = marker_channel("release");
        return json!({
            "enabled_plugin_ids": ids(&enabled),
            "remedy": remedy(&channel),
            "status": "stale",
            "issue": "legacy-marketplace",
        });
    }
    let ambiguous = installed.len() > 1
        || enabled.len() > 1
        || installed.iter().any(|p| p.plugin_id != PLUGIN_ID)
        || owned_markets.len() > 1
        || owned_markets
            .iter()
            .any(|m| m.name == LEGACY_DEV_MARKETPLACE);
    if ambiguous {
        let channel = marker_channel("unknown");
        return json!({
            "channel": channel,
            "installed_plugin_ids": ids(&installed),
            "enabled_plugin_ids": ids(&enabled),
            "status": "conflict",
            "issue": "ambiguous-duplicate-state",
            "remedy": remedy(&channel),
        });
    }
    let marker = match marker {
        Ok(m) => m,
        Err(None) => {
            return json!({
                "status": "unknown",
                "issue": "desired-channel-missing",
                "remedy": DEV_REMEDY,
            })
        }
        Err(Some(e)) => {
            return json!({
                "status": "unknown",
                "issue": "state-unreadable",
                "detail": tail(&e.to_string(), OUTPUT_LIMIT),
                "remedy": DEV_REMEDY,
            })
        }
    };
    let channel = text(&marker, "channel");
    let source = text(&marker, "source");
    let mut base = json!({
        "channel": channel,
        "marketplace": MARKETPLACE,
        "marketplace_source": source,
        "installed_plugin_ids": ids(&installed),
        "enabled_plugin_ids": ids(&enabled),
        "remedy": remedy(&channel),
    });
    let verdict = |base: &mut Value, status: &str, issue: &str, extra: Value| -> Value {
        let row = base.as_object_mut().expect("verdict base is an object");
        if let Value::Object(fields) = extra {
            row.extend(fields);
        }
        row.insert("status".into(), status.into());
        row.insert("issue".into(), issue.into());
        base.clone()
    };
    let Some(selected) = enabled.first() else {
        return verdict(&mut base, "missing", "plugin-missing", json!({}));
    };
    if selected.plugin_id != PLUGIN_ID || selected.marketplace_name != MARKETPLACE {
        return verdict(&mut base, "wrong-channel", "wrong-channel", json!({}));
    }
    let local = expand(&source);
    let want = if channel == "release" {
        Want::Release
    } else {
        Want::Local(&local)
    };
    let market = state.marketplaces.iter().find(|m| m.name == MARKETPLACE);
    let ref_ok =
        channel != "release" || marker.get("ref").and_then(Value::as_str) == Some(RELEASE_REF);
    if !want.matches(&selected.source_type, &selected.marketplace_source)
        || !market.is_some_and(|m| want.matches(&m.source_type, &m.source))
        || !ref_ok
    {
        return verdict(&mut base, "stale", "wrong-marketplace-source", json!({}));
    }
    let canonical = match market {
        Some(m) if channel == "release" && !m.root.is_empty() => PathBuf::from(&m.root),
        _ => local.clone(),
    };
    let source_version = match manifest_version(&canonical) {
        Ok(v) => v,
        Err(e) => {
            let extra = json!({"detail": e.to_string()});
            return verdict(&mut base, "unknown", "source-unreadable", extra);
        }
    };
    // Every verdict past this point carries both versions.
    let versions = json!({"source_version": source_version, "cache_version": selected.version});
    if let (Some(row), Value::Object(fields)) = (base.as_object_mut(), versions) {
        row.extend(fields);
    }
    if source_version != selected.version {
        return verdict(&mut base, "stale", "version-mismatch", json!({}));
    }
    let cache = version_cache(home, &selected.version);
    if !cache.is_dir() {
        return verdict(&mut base, "missing", "cache-missing", json!({}));
    }
    let (source_digest, cache_digest) = match (payload_digest(&canonical), payload_digest(&cache)) {
        (Ok(s), Ok(c)) => (s, c),
        (Err(e), _) | (_, Err(e)) => {
            let extra = json!({"detail": e.to_string()});
            return verdict(&mut base, "unknown", "digest-unreadable", extra);
        }
    };
    let digests = json!({"source_digest": source_digest, "cache_digest": cache_digest});
    if source_digest != cache_digest {
        return verdict(&mut base, "stale", "payload-drift", digests);
    }
    let mut fresh = verdict(&mut base, "fresh", "", digests);
    fresh["issue"] = Value::Null;
    fresh
}

/// The validated marker, `Err(None)` when absent, `Err(Some)` when unusable.
fn read_marker(home: &Path) -> Result<Map<String, Value>, Option<ConvergeError>> {
    const STAGE: &str = "desired-channel-marker";
    let raw = match fs::read_to_string(marker_path(home)) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(None),
        Err(e) => return Err(Some(ConvergeError::new(STAGE, e.to_string()))),
    };
    let doc: Value =
        serde_json::from_str(&raw).map_err(|e| Some(ConvergeError::new(STAGE, e.to_string())))?;
    let marker = object(&doc, STAGE).map_err(Some)?.clone();
    let channel = text(&marker, "channel");
    if channel != "release" && channel != "dev" {
        return Err(Some(ConvergeError::new(
            STAGE,
            "missing or unsupported channel",
        )));
    }
    let source = text(&marker, "source");
    let source_ok = if channel == "release" {
        same_release_source("git", &source)
    } else {
        !source.is_empty()
    };
    if text(&marker, "marketplace") != MARKETPLACE || !source_ok {
        return Err(Some(ConvergeError::new(
            STAGE,
            "channel contract does not match marker",
        )));
    }
    Ok(marker)
}

// --- converge ----------------------------------------------------------------

/// Holds SIGINT, SIGTERM and SIGHUP pending while the live install is mid
/// switch. An interrupt then lands after the commit or the rollback, never
/// between a removal and its restore. The plugin-install door runs on the
/// client's main thread, so this thread's mask is the one that receives them.
struct DeferSignals(libc::sigset_t);

impl DeferSignals {
    fn hold() -> DeferSignals {
        // SAFETY: both sets are initialised by sigemptyset before use, and
        // pthread_sigmask only reads `set` and writes `previous`.
        unsafe {
            let mut set: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut set);
            libc::sigemptyset(&mut previous);
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::sigaddset(&mut set, signal);
            }
            libc::pthread_sigmask(libc::SIG_BLOCK, &set, &mut previous);
            DeferSignals(previous)
        }
    }
}

impl Drop for DeferSignals {
    fn drop(&mut self) {
        // SAFETY: restores the mask `hold` saved; a pending signal is
        // delivered here, after the install is whole again.
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut());
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Converged {
    pub channel: String,
    pub action: &'static str,
    pub plugin_id: String,
    pub version: String,
}

pub struct Request<'a> {
    /// `release` or `dev`.
    pub channel: &'a str,
    /// Remove and re-add even when the install already matches.
    pub refresh: bool,
    /// The dev channel's local root; release ignores it.
    pub source: Option<&'a Path>,
    pub validate_candidate: bool,
}

pub fn converge(r: Runner, home: &Path, req: &Request) -> Result<Converged, ConvergeError> {
    let (version, market_source, git_ref, local) = match (req.channel, req.source) {
        ("release", _) => (
            String::new(),
            RELEASE_SOURCE.to_string(),
            Some(RELEASE_REF),
            None,
        ),
        ("dev", Some(source)) => {
            let version = manifest_version(source)?;
            let root = resolved(source);
            (version, root.display().to_string(), None, Some(root))
        }
        ("dev", None) => {
            return Err(ConvergeError::new(
                "channel",
                "the dev channel needs a source",
            ))
        }
        (other, _) => {
            return Err(ConvergeError::new(
                "channel",
                format!("unsupported channel: {other}"),
            ))
        }
    };
    let want = match &local {
        Some(root) => Want::Local(root),
        None => Want::Release,
    };
    let owned = owned_dir(home);
    fs::create_dir_all(&owned)
        .map_err(|e| ConvergeError::new("convergence-lock", e.to_string()))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(owned.join("plugin-channel.lock"))
        .map_err(|e| ConvergeError::new("convergence-lock", e.to_string()))?;
    lock.lock()
        .map_err(|e| ConvergeError::new("convergence-lock", e.to_string()))?;

    let state = collect(r, home)?;
    let snapshot = Snapshot {
        marker: read_optional(&marker_path(home), "desired-channel-marker")?,
        receipt: read_optional(&receipt_path(home), "rollback-receipt-snapshot")?,
        config: read_owned_config(home)?,
        cache_names: OWNED_MARKETPLACES
            .into_iter()
            .filter(|n| cache_dir(home, n).is_dir())
            .collect(),
        state,
    };
    let state = &snapshot.state;
    let selected = state.plugins.iter().find(|p| p.plugin_id == PLUGIN_ID);
    let footnote_plugins: Vec<&Plugin> = state
        .plugins
        .iter()
        .filter(|p| p.installed && owned_plugin(&p.plugin_id))
        .collect();
    let market = state.marketplaces.iter().find(|m| m.name == MARKETPLACE);
    // The marker is the only record of a prior ref.
    let marker: Option<Map<String, Value>> = snapshot
        .marker
        .as_deref()
        .and_then(|b| serde_json::from_slice::<Value>(b).ok())
        .and_then(|v| v.as_object().cloned());
    let marker_ref = marker
        .as_ref()
        .and_then(|m| m.get("ref"))
        .and_then(Value::as_str);
    let ref_ok = git_ref.is_none() || (marker.is_some() && marker_ref == git_ref);
    let market_ok = market.is_some_and(|m| want.matches(&m.source_type, &m.source)) && ref_ok;
    let legacy_markets: Vec<&Marketplace> = state
        .marketplaces
        .iter()
        .filter(|m| LEGACY_MARKETPLACES.contains(&m.name.as_str()))
        .collect();
    let selected_ok = selected.is_some_and(|p| {
        p.installed
            && p.enabled
            && !p.version.is_empty()
            && p.marketplace_name == MARKETPLACE
            && want.matches(&p.source_type, &p.marketplace_source)
    });
    let only_selected = footnote_plugins.len() == 1
        && footnote_plugins[0].plugin_id == PLUGIN_ID
        && legacy_markets.is_empty();
    let marker_matches = marker.as_ref().is_some_and(|m| {
        text(m, "channel") == req.channel
            && text(m, "marketplace") == MARKETPLACE
            && text(m, "source") == market_source
            && m.get("ref").and_then(Value::as_str) == git_ref
    });
    let legacy_cache = LEGACY_MARKETPLACES
        .iter()
        .any(|n| cache_dir(home, n).exists() || cache_dir(home, n).is_symlink());
    if market_ok
        && selected_ok
        && only_selected
        && !req.refresh
        && marker_matches
        && !legacy_cache
        && snapshot.receipt.is_none()
    {
        // The live state already matches the marker, so candidate validation
        // could only fail an already-converged host (offline, version drift).
        let plugin = selected.expect("selected_ok implies a selected plugin");
        return Ok(Converged {
            channel: req.channel.into(),
            action: "no-op",
            plugin_id: plugin.plugin_id.clone(),
            version: plugin.version.clone(),
        });
    }
    let candidate = if req.validate_candidate {
        Some(validate_candidate(r, want, &market_source, git_ref)?)
    } else {
        None
    };
    let needs_install = selected.is_none() || req.refresh || !selected_ok || !market_ok;
    let _deferred = DeferSignals::hold();
    let mut changed = false;
    let mut quarantines: Vec<Quarantine> = Vec::new();
    let mut transaction = || -> Res<Converged> {
        if let (Some(root), Some(c)) = (&local, &candidate) {
            if manifest_version(root)? != c.version || payload_digest(root)? != c.digest {
                return Err(ConvergeError::new(
                    "candidate-recheck",
                    "dev source changed after candidate validation",
                ));
            }
        }
        if snapshot.receipt.is_some() {
            changed = true;
            restore_bytes(&receipt_path(home), None, "rollback-receipt-clear")?;
        }
        let current = needs_install.then_some(MARKETPLACE);
        for name in LEGACY_MARKETPLACES.into_iter().chain(current) {
            if let Some(q) = quarantine_cache(home, name)? {
                changed = true;
                quarantines.push(q);
            }
        }
        // Legacy identities go first, the selected one last.
        let mut removals = footnote_plugins.clone();
        removals.sort_by_key(|p| p.plugin_id == PLUGIN_ID);
        for plugin in removals {
            let keep =
                plugin.plugin_id == PLUGIN_ID && !(req.refresh || !selected_ok || !market_ok);
            if keep {
                continue;
            }
            changed = true;
            let argv = [
                "codex",
                "plugin",
                "remove",
                plugin.plugin_id.as_str(),
                "--json",
            ];
            run(r, "plugin-remove", &argv, home)?;
        }
        for legacy in &legacy_markets {
            changed = true;
            let argv = [
                "codex",
                "plugin",
                "marketplace",
                "remove",
                legacy.name.as_str(),
                "--json",
            ];
            run(r, "legacy-marketplace-remove", &argv, home)?;
        }
        if needs_install {
            if market.is_some() && !market_ok {
                changed = true;
                let argv = [
                    "codex",
                    "plugin",
                    "marketplace",
                    "remove",
                    MARKETPLACE,
                    "--json",
                ];
                run(r, "marketplace-remove", &argv, home)?;
            }
            if !market_ok {
                changed = true;
                run(
                    r,
                    "marketplace-add",
                    &marketplace_add_argv(&market_source, git_ref),
                    home,
                )?;
            }
            if git_ref.is_some() {
                changed = true;
                let argv = [
                    "codex",
                    "plugin",
                    "marketplace",
                    "upgrade",
                    MARKETPLACE,
                    "--json",
                ];
                run(r, "marketplace-upgrade", &argv, home)?;
            }
            changed = true;
            run(
                r,
                "plugin-add",
                &["codex", "plugin", "add", PLUGIN_ID, "--json"],
                home,
            )?;
        }
        let last = collect(r, home)?;
        let installed: Vec<&Plugin> = last
            .plugins
            .iter()
            .filter(|p| p.installed && owned_plugin(&p.plugin_id))
            .collect();
        let enabled: Vec<&Plugin> = installed.iter().copied().filter(|p| p.enabled).collect();
        let markets: Vec<&Marketplace> = last
            .marketplaces
            .iter()
            .filter(|m| owned_marketplace(&m.name))
            .collect();
        let one = match (installed.as_slice(), enabled.as_slice(), markets.as_slice()) {
            ([_], [p], [m]) => Some((*p, *m)).filter(|(p, m)| {
                p.plugin_id == PLUGIN_ID
                    && p.marketplace_name == MARKETPLACE
                    && !p.version.is_empty()
                    && want.matches(&p.source_type, &p.marketplace_source)
                    && m.name == MARKETPLACE
                    && want.matches(&m.source_type, &m.source)
            }),
            _ => None,
        };
        let Some((plugin, final_market)) = one else {
            let found: Vec<&str> = enabled.iter().map(|p| p.plugin_id.as_str()).collect();
            let found = if found.is_empty() {
                "none".into()
            } else {
                found.join(", ")
            };
            return Err(ConvergeError::new(
                "final-verify",
                format!("expected {PLUGIN_ID}; enabled={found}"),
            ));
        };
        if candidate.is_some() || req.refresh {
            let canonical = local
                .clone()
                .unwrap_or_else(|| PathBuf::from(&final_market.root));
            let final_version = manifest_version(&canonical)?;
            let cache = version_cache(home, &plugin.version);
            if !cache.is_dir() {
                return Err(ConvergeError::new(
                    "final-verify",
                    format!("cache missing after refresh: {}", cache.display()),
                ));
            }
            if plugin.version != final_version {
                return Err(ConvergeError::new(
                    "final-verify",
                    format!(
                        "refreshed version {} != source {final_version}",
                        plugin.version
                    ),
                ));
            }
            let digest = payload_digest(&canonical)?;
            if digest != payload_digest(&cache)? {
                return Err(ConvergeError::new(
                    "final-verify",
                    "cache payload differs after refresh",
                ));
            }
            if candidate
                .as_ref()
                .is_some_and(|c| final_version != c.version || digest != c.digest)
            {
                return Err(ConvergeError::new(
                    "final-verify",
                    "live payload differs from validated candidate",
                ));
            }
        }
        changed = true;
        write_marker(home, req.channel, &market_source, git_ref)?;
        let written = read_marker(home).map_err(|e| {
            e.unwrap_or_else(|| {
                ConvergeError::new("desired-channel-marker", "marker missing after write")
            })
        })?;
        let readback = (
            text(&written, "channel"),
            text(&written, "source"),
            written
                .get("ref")
                .and_then(Value::as_str)
                .map(str::to_string),
        );
        if readback
            != (
                req.channel.to_string(),
                market_source.clone(),
                git_ref.map(str::to_string),
            )
        {
            return Err(ConvergeError::new(
                "desired-channel-marker",
                "marker readback differs after write",
            ));
        }
        for q in &quarantines {
            let _ = fs::remove_dir_all(&q.quarantine);
        }
        let action = if req.refresh {
            "refreshed"
        } else if selected.is_none() {
            "installed"
        } else {
            "repaired"
        };
        let version = if plugin.version.is_empty() {
            version.clone()
        } else {
            plugin.version.clone()
        };
        Ok(Converged {
            channel: req.channel.into(),
            action,
            plugin_id: plugin.plugin_id.clone(),
            version,
        })
    };
    let outcome = transaction();
    let original = match outcome {
        Ok(done) => return Ok(done),
        Err(e) => e,
    };
    if !changed {
        return Err(original);
    }
    if let Err(rollback) = restore_snapshot(r, home, &snapshot, &quarantines) {
        let mut detail = format!("{original}; rollback failed: {rollback}");
        if let Err(receipt) = write_rollback_receipt(home, &original, &detail, req.channel) {
            detail.push_str(&format!("; receipt failed: {receipt}"));
        }
        return Err(ConvergeError::new("rollback-failure", detail));
    }
    Err(original)
}

// --- the arm -----------------------------------------------------------------

pub fn codex_home() -> PathBuf {
    match std::env::var_os("CODEX_HOME") {
        Some(dir) if !dir.is_empty() => expand(&dir.to_string_lossy()),
        _ => crate::paths::dirs_home().join(".codex"),
    }
}

/// `plugin-install codex --status`: the freshness verdict as one JSON line.
pub fn run_status() -> i32 {
    println!("{}", inspect(&mut codex_runner, &codex_home()));
    0
}

/// `plugin-install codex --release`: setup's wiring. A fresh install and any
/// dev-channel install are left alone; anything else converges onto release.
/// `--json` prints `{status, note}` for the auto-wire receipt.
pub fn run_release(json: bool) -> i32 {
    let home = codex_home();
    let runner: Runner = &mut codex_runner;
    let report = |status: &str, note: String| {
        if json {
            println!("{}", json!({"status": status, "note": note}));
        } else {
            println!(
                "plugin install codex: {status}{}",
                if note.is_empty() {
                    String::new()
                } else {
                    format!(" ({note})")
                }
            );
        }
    };
    let verdict = inspect(runner, &home);
    if verdict["status"] == "fresh" {
        report("already-installed", String::new());
        return 0;
    }
    // A developer's checkout install is theirs to refresh; wiring never
    // moves it off the dev channel.
    if verdict["channel"] == "dev" {
        report(
            "already-installed",
            format!("dev channel kept; refresh with `{DEV_REMEDY}`"),
        );
        return 0;
    }
    let req = Request {
        channel: "release",
        refresh: false,
        source: None,
        validate_candidate: true,
    };
    match converge(runner, &home, &req) {
        Ok(done) => {
            let status = if done.action == "no-op" {
                "already-installed"
            } else {
                "installed"
            };
            report(
                status,
                format!(
                    "{} {}; start a new Codex session",
                    done.plugin_id, done.version
                ),
            );
            0
        }
        Err(e) => {
            if json {
                report("failed", e.to_string());
            }
            eprintln!("plugin install codex FAILED: {e}");
            1
        }
    }
}

/// The dev install: converge onto the built stage.
pub fn run_dev(stage: &Path, force: bool) -> Result<String, String> {
    let req = Request {
        channel: "dev",
        refresh: force,
        source: Some(stage),
        validate_candidate: true,
    };
    converge(&mut codex_runner, &codex_home(), &req)
        .map(|done| {
            format!(
                "converged {} {} (action={})",
                done.plugin_id, done.version, done.action
            )
        })
        .map_err(|e| format!("codex arm: {e}"))
}

#[cfg(test)]
#[path = "codex_converge_tests.rs"]
mod tests;
