//! Worktree policy resolution, ported from the deleted
//! `cli/src/fno/worktree_paths.py`: project id, the merged-config policy
//! resolver with its fail-closed refusals, and the undeclared-repo dispatch
//! pin. Characterized by the `policy_*` and `dispatch_pin_*` goldens.

use super::{expanduser, resolve_lenient, sorted_json, state_root};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

pub const VALID_POLICIES: [&str; 3] = ["never", "harness-native", "external"];
const NATIVE_WORKTREE_HARNESSES: [&str; 1] = ["claude"];

/// `UNDECLARED_REPO_RECEIPT`, byte-identical.
pub const UNDECLARED_REPO_RECEIPT: &str =
    "worktree=never (undeclared repo; declare work.workspaces.<slug>.projects[].worktree)";

pub struct Resolved {
    pub policy: String,
    pub base: PathBuf,
    pub project: String,
    pub source: String,
    pub requested_policy: String,
    pub degraded: bool,
    pub note: String,
}

// --- config reading (config_io's _load_raw / _unwrap_config_dict / merge) ---

fn parse_doc(path: &Path) -> Result<Map<String, Value>, ()> {
    let text = std::fs::read_to_string(path).map_err(|_| ())?;
    let value: Value = if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
        serde_yaml::from_str(&text).map_err(|_| ())?
    } else {
        toml::from_str(&text).map_err(|_| ())?
    };
    match value {
        Value::Object(map) => Ok(map),
        _ => Ok(Map::new()),
    }
}

fn load_flat(path: &Path) -> Result<Map<String, Value>, ()> {
    if !path.exists() {
        return Err(());
    }
    parse_doc(path)
}

/// Lift a legacy `config = {...}` wrapper; wrapper keys win conflicts.
fn unwrap_config(raw: Map<String, Value>) -> Map<String, Value> {
    let wrapper = raw.get("config").and_then(Value::as_object).cloned();
    let Some(cfg) = wrapper else {
        return raw;
    };
    let rest: Map<String, Value> = raw.into_iter().filter(|(k, _)| k != "config").collect();
    deep_merge(rest, cfg)
}

fn deep_merge(mut base: Map<String, Value>, over: Map<String, Value>) -> Map<String, Value> {
    for (key, value) in over {
        match (base.get(&key), &value) {
            (Some(Value::Object(_)), Value::Object(inner)) => {
                let existing = base.get(&key).unwrap().as_object().unwrap().clone();
                base.insert(key, Value::Object(deep_merge(existing, inner.clone())));
            }
            (Some(Value::Object(_)), Value::Null) => {}
            _ => {
                base.insert(key, value);
            }
        }
    }
    base
}

fn repo_config_path(repo_root: &Path) -> Option<PathBuf> {
    let fno_dir = repo_root.join(".fno");
    for name in ["config.toml", "settings.yaml"] {
        let p = fno_dir.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn global_config_path() -> PathBuf {
    // _global_settings_path honors FNO_GLOBAL_SETTINGS_PATH; config.toml
    // beside it wins when it exists.
    let yaml = std::env::var_os("FNO_GLOBAL_SETTINGS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".fno").join("settings.yaml"));
    let toml = yaml.with_file_name("config.toml");
    if toml.exists() {
        toml
    } else {
        yaml
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn merged_config(repo_root: &Path) -> Result<Map<String, Value>, String> {
    if let Some(fno_config) = std::env::var_os("FNO_CONFIG") {
        let fc = PathBuf::from(&fno_config);
        let parsed = if fc.exists() {
            load_flat(&fc).map_err(|()| format!("config parse error: {}", fc.display()))?
        } else {
            Map::new()
        };
        return Ok(unwrap_config(parsed));
    }
    let mut global = load_flat(&global_config_path())
        .map_err(|()| format!("config parse error: {}", global_config_path().display()))?;
    global = unwrap_config(global);
    let repo_cfg = match repo_config_path(repo_root) {
        Some(path) => unwrap_config(
            load_flat(&path).map_err(|()| format!("config parse error: {}", path.display()))?,
        ),
        None => Map::new(),
    };
    Ok(deep_merge(global, repo_cfg))
}

// --- project id -------------------------------------------------------------

fn safe_component(value: &str) -> Result<String, String> {
    if value.is_empty() {
        return Err("project_id must be a non-empty string".to_string());
    }
    if value.contains("..") {
        return Err(format!(
            "project_id contains '..' (path traversal): {value:?}"
        ));
    }
    let ok = value.chars().enumerate().all(|(i, c)| {
        c.is_ascii_alphanumeric()
            || ((i > 0 || c.is_ascii_alphanumeric()) && matches!(c, '.' | '_' | '-'))
    }) && value
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if !ok {
        return Err(format!(
            "project_id must match ^[A-Za-z0-9][A-Za-z0-9._-]*$: {value:?}"
        ));
    }
    Ok(value.to_string())
}

fn settings_project_id(repo_root: &Path) -> Option<String> {
    let fno_dir = repo_root.join(".fno");
    let mut settings_path = fno_dir.join("config.toml");
    if !settings_path.exists() {
        settings_path = fno_dir.join("settings.yaml");
    }
    if !settings_path.exists() {
        return None;
    }
    let doc = parse_doc(&settings_path).ok()?;
    let flat = unwrap_config(doc);
    match flat.get("project") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        Some(Value::Object(table)) => table
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn git_derived_project_id(repo_root: &Path) -> Option<String> {
    let out = super::git_out(repo_root, &["remote", "get-url", "origin"])?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if url.is_empty() {
        return None;
    }
    let mut cleaned = url.trim_end_matches('/').to_string();
    if let Some(stripped) = cleaned.strip_suffix(".git") {
        cleaned = stripped.to_string();
    }
    let basename = cleaned
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if basename.is_empty() {
        None
    } else {
        Some(basename)
    }
}

pub fn resolve_project_id(repo_root: &Path) -> Result<String, String> {
    let repo_root = resolve_lenient(repo_root);
    let pid = settings_project_id(&repo_root)
        .or_else(|| git_derived_project_id(&repo_root))
        .unwrap_or_else(|| {
            repo_root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        });
    safe_component(&pid)
}

// --- the resolver ------------------------------------------------------------

fn explicit_worktrees_base(merged: &Map<String, Value>) -> Option<PathBuf> {
    let raw = merged
        .get("paths")
        .and_then(Value::as_object)
        .and_then(|paths| paths.get("worktrees_base"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())?;
    Some(resolve_lenient(&expanduser(&expandvars(raw))))
}

fn expandvars(raw: &str) -> String {
    // $VAR and ${VAR} forms against the process env; an unset name passes through.
    let mut out = String::new();
    let mut rest = raw;
    while let Some(at) = rest.find('$') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if let Some(var) = rest.strip_prefix("${").and_then(|v| v.split_once('}')) {
            out.push_str(&std::env::var(var.0).unwrap_or_default());
            rest = var.1;
        } else if let Some(after) = rest.strip_prefix('$') {
            let end = after
                .char_indices()
                .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
                .map(|(i, _)| i)
                .unwrap_or(after.len());
            let (name, tail) = after.split_at(end);
            out.push_str(&std::env::var(name).unwrap_or_default());
            rest = tail;
        } else {
            out.push('$');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    out
}

fn state_dir_worktrees(merged: &Map<String, Value>) -> PathBuf {
    let state = merged
        .get("state_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("~/.fno/");
    resolve_lenient(&expanduser(&expandvars(state)).join("worktrees"))
}

fn conductor_base() -> PathBuf {
    resolve_lenient(&expanduser("~/conductor/workspaces"))
}

fn conductor_canonical(merged: &Map<String, Value>) -> bool {
    matches!(
        merged
            .get("worktree")
            .and_then(Value::as_object)
            .and_then(|wt| wt.get("use_conductor_canonical")),
        Some(Value::Bool(true)) | Some(Value::String(s)) if s == "true"
    )
}

fn match_project_entry(
    merged: &Map<String, Value>,
    repo_root: &Path,
    project_id: &str,
) -> Result<Option<Map<String, Value>>, String> {
    let Some(work) = merged.get("work") else {
        return Ok(None);
    };
    let Some(work) = work.as_object() else {
        return Err(
            "config `work` is present but not a table; cannot affirm worktree policy".to_string(),
        );
    };
    let Some(workspaces) = work.get("workspaces") else {
        return Ok(None);
    };
    let Some(workspaces) = workspaces.as_object() else {
        return Err(
            "config `work.workspaces` is present but not a table; cannot affirm \
             per-project worktree policy"
                .to_string(),
        );
    };
    let mut entries: Vec<Map<String, Value>> = Vec::new();
    for ws in workspaces.values() {
        if let Some(projects) = ws.as_object().and_then(|ws| ws.get("projects")) {
            if let Some(list) = projects.as_array() {
                entries.extend(list.iter().filter_map(Value::as_object).cloned());
            }
        }
    }
    for entry in &entries {
        if let Some(path) = entry
            .get("path")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let expanded = expanduser(path);
            if resolve_lenient(&expanded) == resolve_lenient(repo_root) {
                return Ok(Some(entry.clone()));
            }
        }
    }
    for entry in &entries {
        if entry.get("name").and_then(Value::as_str) == Some(project_id) {
            return Ok(Some(entry.clone()));
        }
    }
    Ok(None)
}

pub fn resolve(repo_root: &Path, harness: Option<&str>) -> Result<Resolved, String> {
    let repo_root = resolve_lenient(repo_root);
    let merged = merged_config(&repo_root)?;

    let project_id = resolve_project_id(&repo_root).unwrap_or_else(|_| {
        repo_root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    });

    let mut raw_policy: Option<Value> = None;
    let mut source = "default";
    let entry = match_project_entry(&merged, &repo_root, &project_id)?;
    let entry_policy = entry.as_ref().and_then(|e| e.get("worktree")).cloned();
    if entry_policy.is_some() {
        raw_policy = entry_policy;
        source = "per-project";
    } else if let Some(policy) = merged
        .get("worktree")
        .and_then(Value::as_object)
        .and_then(|wt| wt.get("policy"))
    {
        raw_policy = Some(policy.clone());
        source = "global";
    }
    let mut requested = raw_policy.unwrap_or(Value::String("harness-native".into()));
    if let Some(env_policy) = std::env::var_os("FNO_WORKTREE_POLICY") {
        requested = Value::String(env_policy.to_string_lossy().into_owned());
        source = "env";
    }

    let Some(raw_str) = requested.as_str().map(str::to_string) else {
        return Err(format!(
            "worktree policy {requested} is invalid; valid: never | harness-native | external"
        ));
    };
    if !VALID_POLICIES.contains(&raw_str.as_str()) {
        return Err(format!(
            "worktree policy {raw_str:?} is invalid; valid: never | harness-native | external"
        ));
    }

    let mut policy = raw_str.clone();
    let mut base: Option<PathBuf> = None;
    let mut note = String::new();
    if policy == "harness-native"
        && !harness
            .map(|h| NATIVE_WORKTREE_HARNESSES.contains(&h))
            .unwrap_or(false)
    {
        policy = "external".to_string();
        base = Some(state_dir_worktrees(&merged));
    }
    if policy == "harness-native" {
        if let Some(explicit) = explicit_worktrees_base(&merged) {
            policy = "external".to_string();
            base = Some(explicit);
        } else if conductor_canonical(&merged) {
            note = "worktree.use_conductor_canonical is DEPRECATED; set config.paths.worktrees_base instead.".to_string();
            policy = "external".to_string();
            base = Some(conductor_base());
        }
    }
    let base = base.unwrap_or_else(|| state_dir_worktrees(&merged));

    Ok(Resolved {
        degraded: policy != raw_str,
        policy,
        base,
        project: project_id,
        source: source.to_string(),
        requested_policy: raw_str,
        note,
    })
}

// --- the dispatch pin ---------------------------------------------------------

fn repo_identity(path: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = super::git_out(path, &["rev-parse", "--show-toplevel", "--git-common-dir"])?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut parts = text.split_whitespace();
    let top = PathBuf::from(parts.next()?).to_string_lossy().into_owned();
    let top = resolve_lenient(Path::new(&top));
    let common = parts.next()?;
    let common = Path::new(common);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        top.join(common)
    };
    Some((top, resolve_lenient(&common)))
}

pub fn undeclared_dispatch_pin(
    target_cwd: &Path,
    caller_cwd: &Path,
    harness: Option<&str>,
) -> Option<()> {
    let target = repo_identity(target_cwd)?;
    let caller = repo_identity(caller_cwd)?;
    if target.1 == caller.1 {
        return None;
    }
    match resolve(&target.0, harness) {
        Ok(pol) if pol.source == "default" => Some(()),
        _ => None,
    }
}

// --- client ops ----------------------------------------------------------------

pub fn run_client(args: &[String]) -> i32 {
    worktree_policy_client(args)
}

fn worktree_policy_client(args: &[String]) -> i32 {
    let mut repo: Option<String> = None;
    let mut harness: Option<String> = None;
    let mut as_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" if i + 1 < args.len() => {
                repo = Some(args[i + 1].clone());
                i += 2;
            }
            "--harness" if i + 1 < args.len() => {
                harness = Some(args[i + 1].clone());
                i += 2;
            }
            "--json" => {
                as_json = true;
                i += 1;
            }
            other => {
                let _ = other;
                eprintln!("fno-agents worktree-policy: unexpected argument {other}");
                return 2;
            }
        }
    }
    let Some(repo) = repo else {
        eprintln!("fno-agents worktree-policy: --repo is required");
        return 2;
    };
    match resolve(Path::new(&repo), harness.as_deref()) {
        Ok(pol) => {
            if as_json {
                let payload = json!({
                    "policy": pol.policy,
                    "base": pol.base.to_string_lossy(),
                    "project": pol.project,
                    "source": pol.source,
                    "requested_policy": pol.requested_policy,
                    "degraded": pol.degraded,
                    "note": pol.note,
                });
                println!("{}", sorted_json(&payload));
                return 0;
            }
            // The line contract the worktree-create hook reads: line 1 is
            // the policy word, `base=` carries the location, and a
            // degradation names what was requested.
            if !pol.note.is_empty() {
                eprintln!("{}", pol.note);
            }
            println!("{}", pol.policy);
            if pol.policy != "never" {
                println!("base={}", pol.base.display());
            }
            println!("source={}", pol.source);
            if pol.degraded {
                println!("requested={} degraded=true", pol.requested_policy);
            }
            0
        }
        Err(msg) => {
            eprintln!("WorktreePolicyError: {msg}");
            1
        }
    }
}

/// `worktree-dispatch-pin --target T --caller C [--harness H]`: one JSON
/// envelope of the pin env and the receipt the Python seam printed.
pub fn dispatch_pin_client(args: &[String]) -> i32 {
    let mut target = None;
    let mut caller = None;
    let mut harness = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--target" if i + 1 < args.len() => {
                target = Some(args[i + 1].clone());
                i += 2;
            }
            "--caller" if i + 1 < args.len() => {
                caller = Some(args[i + 1].clone());
                i += 2;
            }
            "--harness" if i + 1 < args.len() => {
                harness = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                eprintln!("fno-agents worktree-dispatch-pin: unexpected argument {other}");
                return 2;
            }
        }
    }
    let (Some(target), Some(caller)) = (target, caller) else {
        eprintln!("fno-agents worktree-dispatch-pin: --target and --caller are required");
        return 2;
    };
    let pinned =
        undeclared_dispatch_pin(Path::new(&target), Path::new(&caller), harness.as_deref());
    let payload = if pinned.is_some() {
        json!({
            "pin": {"FNO_WORKTREE_POLICY": "never"},
            "receipt": UNDECLARED_REPO_RECEIPT,
        })
    } else {
        json!({"pin": {}, "receipt": null})
    };
    println!("{}", sorted_json(&payload));
    0
}

/// The state-root anchor ops like the events journal resolve through.
pub(crate) fn events_journal() -> PathBuf {
    state_root().join("events.jsonl")
}

/// `config.backlog.id_prefix` or the legacy `ab-` fallback, fail-open like
/// the Python `node_id_prefix`.
pub fn node_id_prefix(repo_root: &Path) -> String {
    match merged_config(&resolve_lenient(repo_root)) {
        Ok(merged) => merged
            .get("backlog")
            .and_then(Value::as_object)
            .and_then(|b| b.get("id_prefix"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("ab-")
            .to_string(),
        Err(_) => "ab-".to_string(),
    }
}
