//! The two settings reads the porcelain decisions need, walked in the same
//! candidate order the Python config readers use: the repo-local config
//! first, then the global file (honoring `FNO_GLOBAL_SETTINGS_PATH`).
//!
//! `backlog.id_prefix` feeds the bare-hex resolution tier; the
//! `work.workspaces` project map feeds the `_resolved_cwd` display
//! annotation `backlog get` stamps on every served row (and that
//! `validate-plan.sh` reads back through `--field _resolved_cwd`). Both are
//! best-effort: a missing or unparseable file contributes nothing, and the
//! legacy defaults hold.

use serde_json::Value;
use std::path::PathBuf;

/// The config candidates, highest priority first. Each settings.yaml
/// location also yields its config.toml sibling, which wins (the flat
/// config.toml-first cut the Python reader applies). The cwd and HOME
/// entries are DISCOVERED, so they honor `FNO_CONFIG_SEARCH_ROOT`
/// ([`crate::agents_config::within_search_ceiling`]); the FNO_CONFIG and
/// FNO_GLOBAL_SETTINGS_PATH entries are explicit and read as-is.
pub(crate) fn candidates() -> Vec<PathBuf> {
    candidates_at(std::env::current_dir().ok().as_deref())
}

/// The same walk with the repo-local candidate seeded at an explicit cwd: the
/// cwd a launch runs the child under, not the spawner's.
pub(crate) fn candidates_at(cwd: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut push_dir = |dir: PathBuf| {
        out.push(dir.join("config.toml"));
        out.push(dir.join("settings.yaml"));
    };
    if let Some(cwd) = cwd {
        let dir = cwd.join(".fno");
        if crate::agents_config::within_search_ceiling(&dir.join("config.toml")) {
            push_dir(dir);
        }
    }
    // The project config the FNO_CONFIG redirect names (python's
    // `config_file()` candidate), then the global file. The explicit file is
    // usually its parent's settings.yaml sibling, so the sibling push already
    // covered it: skip the repeat so one file is read once (a doubled work
    // map was the symptom; Python's FNO_CONFIG branch reads the file exactly
    // once).
    if let Some(cfg) = std::env::var_os("FNO_CONFIG").filter(|v| !v.is_empty()) {
        let cfg = PathBuf::from(cfg);
        if let Some(parent) = cfg.parent() {
            push_dir(parent.to_path_buf());
        }
        if !out.contains(&cfg) {
            out.push(cfg);
        }
    }
    match std::env::var_os("FNO_GLOBAL_SETTINGS_PATH") {
        Some(v) if !v.is_empty() => out.push(PathBuf::from(v)),
        _ => {
            if let Some(home) = std::env::var_os("HOME") {
                let home = PathBuf::from(home);
                if crate::agents_config::within_search_ceiling(
                    &home.join(".fno").join("config.toml"),
                ) {
                    out.push(home.join(".fno").join("config.toml"));
                    out.push(home.join(".fno").join("settings.yaml"));
                }
            }
        }
    }
    // A FNO_CONFIG that names `<dir>/settings.yaml` is also its parent's
    // candidate: read each file once, at its first position.
    let mut seen = std::collections::HashSet::new();
    out.retain(|p| seen.insert(p.clone()));
    out
}

/// Parse one config file into a JSON object: TOML by suffix, YAML otherwise.
/// `{}` on a missing or unparseable file.
fn read_flat(path: &std::path::Path) -> Value {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Value::Object(Default::default());
    };
    let parsed: Value = if path.extension().is_some_and(|e| e == "toml") {
        text.parse::<toml::Value>()
            .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
            .unwrap_or(Value::Null)
    } else {
        serde_yaml_ng::from_str::<serde_json::Value>(&text).unwrap_or(Value::Null)
    };
    parsed
}

/// The parsed config candidates, first-priority order. The birth hook reads
/// nested config flags through this walk (autonomy master switch,
/// think_spawn gate and caps).
pub(crate) fn config_candidates() -> Vec<Value> {
    candidates().iter().map(|p| read_flat(p)).collect()
}

/// The configured node-id prefix, or the legacy `ab-`. The first candidate
/// carrying a non-empty `backlog.id_prefix` wins.
pub fn node_id_prefix() -> String {
    for path in candidates() {
        let doc = read_flat(&path);
        let prefix = doc
            .get("backlog")
            .and_then(|b| b.get("id_prefix"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !prefix.is_empty() {
            return prefix.to_string();
        }
    }
    "ab-".to_string()
}

/// The board's id display: `hex` (default) or `number`. `number` shows a
/// node's sequential display number first on the board with the hex id
/// still beside it.
pub fn id_display() -> String {
    for path in candidates() {
        let doc = read_flat(&path);
        let mode = doc
            .get("backlog")
            .and_then(|b| b.get("id_display"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if !mode.is_empty() {
            return mode.to_string();
        }
    }
    "hex".to_string()
}

/// Yield `(name, path)` pairs from one file's work map: the multi-workspace
/// shape first, then the legacy flat shape, in declaration order.
fn work_pairs(doc: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Some(work) = doc.get("work") else {
        return out;
    };
    if let Some(projects) = work.get("workspaces").and_then(Value::as_object) {
        for ws in projects.values() {
            let Some(rows) = ws.get("projects").and_then(Value::as_array) else {
                continue;
            };
            for row in rows {
                let name = row.get("name").and_then(Value::as_str);
                let path = row.get("path").and_then(Value::as_str);
                if let (Some(name), Some(path)) = (name, path) {
                    out.push((name.to_string(), path.to_string()));
                }
            }
        }
    }
    if let Some(projects) = work.get("projects").and_then(Value::as_object) {
        for (name, cfg) in projects {
            if let Some(path) = cfg.get("path").and_then(Value::as_str) {
                out.push((name.clone(), path.to_string()));
            }
        }
    }
    out
}

pub(crate) fn expand_home(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

/// Project name -> work-map path (abspath of the expanded entry). The first
/// candidate file naming the project wins; None when nothing maps it.
pub fn project_root(project: &str) -> Option<String> {
    if project.is_empty() {
        return None;
    }
    for path in candidates() {
        for (name, raw) in work_pairs(&read_flat(&path)) {
            if name == project {
                return Some(expand_home(&raw).to_string_lossy().into_owned());
            }
        }
    }
    None
}

/// Every registered project path, in candidate then declaration order,
/// first occurrence kept.
pub fn project_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for path in candidates() {
        for (_, raw) in work_pairs(&read_flat(&path)) {
            let root = expand_home(&raw);
            if !out.contains(&root) {
                out.push(root);
            }
        }
    }
    out
}

/// The configured node-id hex width, or the legacy 8. Fail-open like the
/// prefix: an absent key resolves to 8.
pub fn node_id_hex_width() -> usize {
    for path in candidates() {
        let n = read_flat(&path)
            .get("backlog")
            .and_then(|b| b.get("id_hex_width"))
            .and_then(Value::as_u64);
        if let Some(n) = n {
            return n as usize;
        }
    }
    8
}

/// Lexical `normpath`: drop `.` components and resolve `..` against the
/// buffer, never touching the filesystem. The Python readers compared paths
/// in this form (os.path.normpath), so a `dir/./x/..` entry still matches.
pub(crate) fn normalize(path: &std::path::Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The work-map pairs across every candidate, in walk order, paths
/// home-expanded and lexically normalized: the form the Python work-map
/// readers yielded. Empty names are skipped (the Python consumers guarded
/// them per row; the door owns the guard now).
pub(crate) fn work_map_pairs() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for path in candidates() {
        for (name, raw) in work_pairs(&read_flat(&path)) {
            if name.is_empty() || raw.is_empty() {
                continue;
            }
            let normalized = normalize(&expand_home(&raw));
            out.push((name, normalized.to_string_lossy().into_owned()));
        }
    }
    out
}

/// The cwd -> project detect over the work map: first pair whose normalized
/// path equals the normalized cwd (the argument, or the process cwd). Silent
/// None, like the Python reader it answers: a missing map never blocks.
pub(crate) fn detect_project(cwd: Option<&str>) -> Option<String> {
    let target = match cwd.filter(|c| !c.is_empty()) {
        Some(c) => {
            let expanded = expand_home(c);
            if expanded.is_absolute() {
                normalize(&expanded)
            } else {
                let base = std::env::current_dir().ok()?;
                normalize(&base.join(expanded))
            }
        }
        None => std::env::current_dir().ok()?,
    };
    let target = target.to_string_lossy().into_owned();
    work_map_pairs()
        .into_iter()
        .find(|(_, path)| path == &target)
        .map(|(name, _)| name)
}

/// The `state_dir` value of one parsed candidate: the bare key first, then
/// the legacy settings.yaml envelope (`schema_version:` + `config:`), the
/// same bare-then-envelope walk live_lookup applies.
fn state_dir_value(doc: &Value) -> Option<&str> {
    doc.get("state_dir")
        .and_then(Value::as_str)
        .or_else(|| {
            doc.get("config")
                .and_then(|c| c.get("state_dir"))
                .and_then(Value::as_str)
        })
        .filter(|s| !s.is_empty())
}

/// The state directory the porcelain reads serve from: `FNO_STATE_DIR` when
/// the env names a root (the carrier `fno.paths.state_dir` resolves and
/// `accounts::credentials::seal` pins, so a sealed read serves the store its parent
/// wrote), else `FNO_CONFIG`'s `state_dir` when the env names a file, else
/// the first candidate carrying a `state_dir`, else `~/.fno`. `FNO_HOME`
/// does not move the backlog. Python's `state_dir()` always answers
/// absolute: a relative value anchors at the cwd and collapses `.`/`..`.
pub fn state_dir() -> Option<PathBuf> {
    state_dir_at(std::env::current_dir().ok().as_deref())
}

/// The state directory walk anchored at an explicit cwd: the cwd a launch
/// runs the child under, so a relative value anchors where the child reads.
pub fn state_dir_at(cwd: Option<&std::path::Path>) -> Option<PathBuf> {
    state_dir_configured(cwd).or_else(|| {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|h| h.join(".fno"))
    })
}

/// The configured state root only: the env pins and the settings candidates,
/// anchored at the cwd. `None` when nothing is configured, so a caller can
/// fall back its own way (the `$HOME/.fno` default stays with
/// [`state_dir_at`]).
pub fn state_dir_configured(cwd: Option<&std::path::Path>) -> Option<PathBuf> {
    if let Some(v) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
        return Some(anchored(expand_home(&v.to_string_lossy()), cwd));
    }
    if let Some(cfg) = std::env::var_os("FNO_CONFIG").filter(|v| !v.is_empty()) {
        if let Some(sd) = state_dir_value(&read_flat(&PathBuf::from(cfg))) {
            return Some(anchored(expand_home(sd), cwd));
        }
    }
    for path in candidates_at(cwd) {
        if let Some(sd) = state_dir_value(&read_flat(&path)) {
            return Some(anchored(expand_home(sd), cwd));
        }
    }
    None
}

/// Anchor at `cwd` through the shared lexical walk; with no cwd (it cannot
/// be read), the value passes through.
fn anchored(p: PathBuf, cwd: Option<&std::path::Path>) -> PathBuf {
    match cwd {
        Some(base) => crate::paths_cli::anchor_at(base, p),
        None => p,
    }
}

/// The graph file the porcelain reads serve: `<state_dir>/graph.json`,
/// resolved through the layout table so a migrated root reads the `db/` twin.
pub fn graph_path() -> PathBuf {
    graph_path_at(std::env::current_dir().ok().as_deref())
}

/// The same answer anchored at the cwd a launch runs the child under. This is
/// the resolver a footnote launch names in `FNO_GRAPH_JSON`: it never reads
/// an inherited value, so a declared sandbox beats one.
pub fn graph_path_at(cwd: Option<&std::path::Path>) -> PathBuf {
    match state_dir_at(cwd) {
        Some(d) => crate::state_layout::graph_anchor(&d),
        None => crate::state_layout::place(&PathBuf::from(".fno"), "graph.json"),
    }
}

/// `config.batch.enabled`, fail-safe false: an unreadable settings file must
/// not enable the batch lane. Same candidate walk as the other readers.
pub fn batch_enabled() -> bool {
    for path in candidates() {
        if let Some(v) = read_flat(&path)
            .get("batch")
            .and_then(|b| b.get("enabled"))
            .and_then(Value::as_bool)
        {
            return v;
        }
    }
    false
}

/// `config.batch.max_nodes`, default 3 (the config coercion floor).
pub fn batch_max_nodes() -> i64 {
    for path in candidates() {
        if let Some(v) = read_flat(&path)
            .get("batch")
            .and_then(|b| b.get("max_nodes"))
            .and_then(Value::as_i64)
        {
            return v;
        }
    }
    3
}

/// The configured dispatch-command extension: `(allowed_commands, command_registry
/// keys)`. Unset `allowed_commands` defaults to the shipped trio, the same
/// default the Python config model carries.
pub fn dispatch_commands_from_config() -> (Vec<String>, Vec<String>) {
    const DEFAULT_DISPATCH_COMMANDS: [&str; 3] = ["/target", "/think", "/blueprint"];
    if let Some(path) = candidates().first() {
        let doc = read_flat(path);
        let dispatch = doc.get("dispatch");
        let allowed: Vec<String> = dispatch
            .and_then(|d| d.get("allowed_commands"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_else(|| {
                DEFAULT_DISPATCH_COMMANDS
                    .iter()
                    .map(|s| s.to_string())
                    .collect()
            });
        let registry: Vec<String> = dispatch
            .and_then(|d| d.get("command_registry"))
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        return (allowed, registry);
    }
    (
        DEFAULT_DISPATCH_COMMANDS
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Vec::new(),
    )
}

// ---------------------------------------------------------------------------
// The settings-read receipt's `paths` fact set: a vendored loader replay.
//
// Production rule: fno-agents never links fno (the fno edge is a
// dev-dependency, and the publish gate blocks a real one), so the walk the
// Python loader ran re-authors here. Every helper names its canonical twin in
// crates/fno/src/config_cli, and the unit test at the bottom pins this whole
// projection against that loader across layered fixtures. Drift between the
// two fails CI, which is the drift-detection the dual-implementation
// inventory asks of a vendored copy.
// ---------------------------------------------------------------------------

/// The `paths.*` leaves the Python resolver consumes, bare-keyed in the
/// receipt. All default to null, like the model.
const PATH_KEYS: [&str; 15] = [
    "worktrees_base",
    "spaces_dir",
    "retro_pending_dir",
    "postmortems_dir",
    "operator_lane",
    "observer_reports_dir",
    "memory_dir",
    "ledger_json",
    "inbox_path",
    "hook_logs_dir",
    "handoffs_dir",
    "fleet_dir",
    "evals_history",
    "bus_dir",
    "briefs_dir",
];

/// config_cli::canonical_repo_root's twin of the Python loader's
/// `_canonical_root_from_gitfile`: the main working tree from a linked
/// worktree's `.git` pointer file, no subprocess. None on a plain checkout.
fn door_canonical_root(root: &std::path::Path) -> Option<PathBuf> {
    let git = root.join(".git");
    if git.is_dir() {
        return None;
    }
    let text = std::fs::read_to_string(&git).ok()?;
    for line in text.lines() {
        if let Some(gitdir) = line.strip_prefix("gitdir:") {
            let idx = gitdir.find("/.git/worktrees/")?;
            return Some(PathBuf::from(gitdir[..idx].trim()));
        }
    }
    None
}

/// config_cli::global_settings_path: FNO_GLOBAL_SETTINGS_PATH when set, else
/// ~/.fno/settings.yaml.
fn door_global_path() -> PathBuf {
    match std::env::var_os("FNO_GLOBAL_SETTINGS_PATH") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => match std::env::var_os("HOME") {
            Some(home) => PathBuf::from(home).join(".fno").join("settings.yaml"),
            None => PathBuf::from("/.fno/settings.yaml"),
        },
    }
}

/// config_cli::settings_yaml_locations + prefer_toml + candidate_paths: the
/// ordered candidates, config.toml over its settings.yaml sibling per dir,
/// canonical root second, global last. A FNO_CONFIG pin is the whole chain
/// only on the ambient walk (`pinned=true`); an explicit root walks unpinned,
/// the way load_settings_for_repo read a foreign repo's config
/// (config_cli::chain_at).
fn door_candidates_pinned(root: &std::path::Path, pinned: bool) -> Vec<PathBuf> {
    if pinned {
        let pin = std::env::var_os("FNO_CONFIG").filter(|v| !v.is_empty());
        if let Some(pin) = pin {
            return vec![PathBuf::from(pin)];
        }
    }
    let mut yaml_locations = vec![root.join(".fno").join("settings.yaml")];
    if std::env::var("FNO_NO_CANONICAL_CONFIG").as_deref() != Ok("1") {
        if let Some(canonical) = door_canonical_root(root) {
            let path = canonical.join(".fno").join("settings.yaml");
            if !yaml_locations.contains(&path) {
                yaml_locations.push(path);
            }
        }
    }
    yaml_locations.push(door_global_path());
    let mut out: Vec<PathBuf> = Vec::new();
    for path in yaml_locations {
        let toml = path.with_file_name("config.toml");
        if !out.contains(&toml) {
            out.push(toml);
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// config_cli::repo_root: FNO_REPO_ROOT, then git toplevel, then cwd.
fn door_repo_root() -> PathBuf {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT") {
        if !pin.is_empty() {
            return PathBuf::from(pin);
        }
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&cwd)
        .output()
    {
        if out.status.success() {
            let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !line.is_empty() {
                return PathBuf::from(line);
            }
        }
    }
    cwd
}
/// config_cli::deep_merge, byte for byte: the over layer wins per leaf, and
/// a null in the over layer never wipes an object below it.
fn door_deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base_map), Value::Object(over_map)) => {
            for (key, value) in over_map {
                match base_map.get_mut(key) {
                    Some(existing) if existing.is_object() && value.is_object() => {
                        door_deep_merge(existing, value);
                    }
                    Some(existing) if existing.is_object() && value.is_null() => {}
                    _ => {
                        base_map.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (slot, value) => *slot = value.clone(),
    }
}

/// config_cli::unwrap_config_dict: a legacy top-level `config:` block lifts
/// to the top level, its leaves winning over stray same-named keys.
fn door_unwrap(raw: Value) -> Value {
    let map = match raw.as_object() {
        Some(map) => map.clone(),
        None => return raw,
    };
    let wrapped = match map.get("config").and_then(|c| c.as_object()) {
        Some(wrapped) => wrapped.clone(),
        None => return raw,
    };
    let mut rest = Value::Object(map.into_iter().filter(|(k, _)| k != "config").collect());
    door_deep_merge(&mut rest, &Value::Object(wrapped));
    rest
}

/// config_cli::worktree_local_override against WORKTREE_LOCAL_KEYS (just
/// "project.id"): the local file reduced to its allowlisted leaves.
fn door_worktree_local(parsed: &Value) -> Option<Value> {
    let project = parsed.get("project")?.as_object()?;
    let id = project.get("id")?;
    Some(serde_json::json!({ "project": { "id": id.clone() } }))
}

/// One parsed layer with the alias pass the Python loader ran per layer,
/// reduced to the arm that touches the receipt's key set: a legacy
/// config:-wrapped file's top-level `project.id` lifts into the wrapped
/// block when the wrapped block has none (config_cli::alias_legacy_keys).
fn door_layer(path: &std::path::Path) -> Value {
    let mut doc = read_flat(path);
    let (has_config, top_project_id) = {
        let map = match doc.as_object() {
            Some(map) => map,
            None => return doc,
        };
        let has_config = map.get("config").map(|c| c.is_object()).unwrap_or(false);
        let top_id = map
            .get("project")
            .and_then(|p| p.get("id"))
            .filter(|v| !v.is_null())
            .cloned();
        (has_config, top_id)
    };
    if let (true, Some(id)) = (has_config, top_project_id) {
        if let Some(config) = doc.get_mut("config").and_then(Value::as_object_mut) {
            let cfg_project = config
                .entry("project")
                .or_insert_with(|| Value::Object(serde_json::Map::new()));
            if let Some(slot) = cfg_project.as_object_mut() {
                let absent = slot.get("id").map(|v| v.is_null()).unwrap_or(true);
                if absent {
                    slot.insert("id".to_string(), id);
                }
            }
        }
    }
    doc
}

/// The aliased layers, highest priority first: existing parseable candidates
/// only (config_cli::aliased_layers minus the readback warnings, which the
/// receipt never needed). `pinned` walks the FNO_CONFIG pin alone; an
/// unpinned walk expands the full candidate chain at that root.
fn door_layers_pinned(root: &std::path::Path, pinned: bool) -> Vec<(PathBuf, Value)> {
    let mut out = Vec::new();
    for path in door_candidates_pinned(root, pinned) {
        if !path.is_file() {
            continue;
        }
        // Per-layer unwrap, config_cli::aliased_layers's order (the envelope
        // lifts in its own layer, so a higher flat layer wins per key). An
        // empty TOML table stays a layer (it decides `source`); a Null parse
        // (empty YAML) is dropped, protecting the merge.
        let doc = door_unwrap(door_layer(&path));
        if doc.is_object() {
            out.push((path, doc));
        }
    }
    out
}
/// The receipt's `paths` fact set: layered merge over the vendored walk
/// (each layer envelope-unwrapped at its own layer, config_cli's order),
/// `config.local.toml` allowlist leaf layered in, then the resolver's keys
/// projected flat. No defaults here: an unset key reads null and the Python
/// caller applies the model default (the fail-open contract - a refused read
/// answers nulls, not guesses). `source` is the highest-priority layer that
/// existed and parsed (the `loaded_from` fact).
pub(crate) fn paths_facts(root: Option<&std::path::Path>) -> Value {
    let pinned = root.is_none();
    let root = match root {
        Some(p) => p.to_path_buf(),
        None => door_repo_root(),
    };
    let layers = door_layers_pinned(&root, pinned);
    let mut merged = Value::Object(serde_json::Map::new());
    for (_path, parsed) in layers.iter().rev() {
        door_deep_merge(&mut merged, parsed);
    }
    if let Some(top) = door_candidates_pinned(&root, pinned).first().cloned() {
        if let Some(local) = top.parent().map(|dir| dir.join("config.local.toml")) {
            if local.is_file() && !local.is_symlink() {
                let parsed = read_flat(&local);
                if let Some(layer) = door_worktree_local(&parsed) {
                    door_deep_merge(&mut merged, &layer);
                }
            }
        }
    }
    let at = |dotted: &str| -> Value {
        dotted
            .split('.')
            .try_fold(&merged, |node, part| node.get(part))
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut out = serde_json::Map::new();
    out.insert(
        "source".to_string(),
        layers
            .first()
            .map(|(path, _)| Value::String(path.to_string_lossy().into_owned()))
            .unwrap_or(Value::Null),
    );
    out.insert("state_dir".to_string(), at("state_dir"));
    out.insert("project_id".to_string(), at("project.id"));
    out.insert("obsidian_enabled".to_string(), at("obsidian.enabled"));
    out.insert("obsidian_vault".to_string(), at("obsidian.vault"));
    out.insert(
        "post_merge_parking_lot_path".to_string(),
        at("post_merge.parking_lot_path"),
    );
    out.insert(
        "post_merge_maintainer_marker".to_string(),
        at("post_merge.maintainer_marker"),
    );
    for key in PATH_KEYS {
        let value = at(&format!("paths.{key}"));
        out.insert(key.to_string(), value);
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_dir_carrier_outranks_config_and_home() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var_os("FNO_STATE_DIR");
        std::env::set_var("FNO_STATE_DIR", "/pinned-state");

        assert_eq!(state_dir(), Some(PathBuf::from("/pinned-state")));

        match prior {
            Some(v) => std::env::set_var("FNO_STATE_DIR", v),
            None => std::env::remove_var("FNO_STATE_DIR"),
        }
    }

    #[test]
    fn detect_matches_the_normalized_work_map_path() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("settings.yaml");
        std::fs::write(
            &cfg,
            "work:\n  projects:\n    my-proj:\n      path: /repo/my-proj\n",
        )
        .unwrap();
        let prior = std::env::var_os("FNO_CONFIG");
        std::env::set_var("FNO_CONFIG", &cfg);

        // The dot-dot walk normalizes back to the mapped path.
        assert_eq!(
            detect_project(Some("/repo/my-proj/./x/..")).as_deref(),
            Some("my-proj")
        );
        assert_eq!(detect_project(Some("/elsewhere")), None);

        match prior {
            Some(v) => std::env::set_var("FNO_CONFIG", v),
            None => std::env::remove_var("FNO_CONFIG"),
        }
    }
}
