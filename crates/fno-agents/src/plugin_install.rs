//! `fno-agents plugin-install <harness> [--force]`, surfaced as
//! `fno config plugin install`. One local-dev install door for every
//! plugin harness. Everything installs from the FILTERED stage
//! (`<state-root>/plugin-stage/fno`: git-tracked + untracked-but-not-ignored
//! files only, rebuilt wholesale), never from the repo root, because harness
//! caches copy what they are pointed at.
//!
//! This module owns stage build, the `--check`/`--restage` deploy commands, the
//! claude, opencode and agy arms plus the old env-export cleanup. The codex arm
//! converges through `codex_converge`.
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;

use crate::pack_profile::{MuxPaths, Profile, HOOK_FILES};
use crate::paths::{dirs_home, AgentsHome};

const BUILD_DIR_KEY: &str = "CARGO_BUILD_BUILD_DIR";
const SCCACHE_DIR_KEY: &str = "SCCACHE_DIR";
/// The reclaim pass an install chains: every lane except the uv cache prune,
/// which can hold a first install for its full 120s cap.
const RECLAIM_ON_INSTALL: [&str; 2] = ["--apply", "--no-uv-prune"];
const RC_MARK: &str = "# fno: cargo build-dir";

/// Same shape as the Python gate's `_PLUGIN_ROOT_RE` in cli/src/fno/lint_cli.py.
const HOOK_REF_PATTERN: &str =
    r#"\$\{(?:CLAUDE_PLUGIN_ROOT|CODEX_PLUGIN_ROOT|PLUGIN_ROOT)\}/([^\s"'\\]+)"#;

pub(crate) fn state_root() -> PathBuf {
    std::env::var_os("FNO_RECLAIM_STATE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs_home().join(".fno"))
}

fn run_checked(cmd: &[String], cwd: Option<&Path>) -> Result<String, String> {
    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let out = command.output().map_err(|e| format!("{}: {e}", cmd[0]))?;
    if !out.status.success() {
        let mut detail = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if detail.is_empty() {
            detail = String::from_utf8_lossy(&out.stdout).trim().to_string();
        }
        return Err(format!(
            "{} exited {}: {}",
            cmd[0],
            out.status.code().unwrap_or(-1),
            detail
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn run_checked_stdin(cmd: &[String], cwd: Option<&Path>, stdin: &str) -> Result<String, String> {
    use std::io::Write;
    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]);
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    let mut child = command.spawn().map_err(|e| format!("{}: {e}", cmd[0]))?;
    let mut stdin_handle = child
        .stdin
        .take()
        .ok_or_else(|| format!("{}: no stdin", cmd[0]))?;
    let input = stdin.to_string();
    // The child's stdout pipe fills while it consumes stdin, so write from a
    // thread: hashing a 3.8k-file stage deadlocks a single-threaded write.
    let writer = std::thread::spawn(move || {
        let _ = stdin_handle.write_all(input.as_bytes());
    });
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{}: {e}", cmd[0]))?;
    let _ = writer.join();
    if !out.status.success() {
        return Err(format!(
            "{} exited {}: {}",
            cmd[0],
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The git checkout that owns `dir`.
fn repo_root(dir: &Path) -> Result<PathBuf, String> {
    let root = run_checked(
        &[
            "git".to_string(),
            "rev-parse".to_string(),
            "--show-toplevel".to_string(),
        ],
        Some(dir),
    )?;
    Ok(PathBuf::from(root.trim()))
}

/// Copy hook-config-referenced scripts the new tree lacks from the old stage.
/// Sessions started before the restage hold the old hook config for their
/// whole life; the hook-tombstones lint protects one merge range, but a stage
/// stale across many merges can still reference a script the new tree deleted.
fn carry_referenced_scripts(old_stage: &Path, new_stage: &Path, mux: Option<&MuxPaths>) -> usize {
    let re = match Regex::new(HOOK_REF_PATTERN) {
        Ok(r) => r,
        Err(_) => return 0,
    };
    let mut carried = 0usize;
    for config in [
        "hooks/hooks.json",
        "hooks/codex-hooks.json",
        "hooks/context-hooks.json",
    ] {
        let text = match std::fs::read_to_string(old_stage.join(config)) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for rel in re.captures_iter(&text).map(|c| c[1].to_string()) {
            let target = new_stage.join(&rel);
            // A script reclassified as mux stays out of a pack, old stage or not.
            if target.exists() || mux.is_some_and(|m| m.covers(&rel)) {
                continue;
            }
            let src = old_stage.join(&rel);
            if !src.is_file() {
                continue;
            }
            if let Some(parent) = target.parent() {
                if std::fs::create_dir_all(parent).is_err() {
                    continue;
                }
            }
            if std::fs::copy(&src, &target).is_ok() {
                carried += 1;
            }
        }
    }
    carried
}

/// The marketplace manifest path, relative to the stage root.
const MARKETPLACE_REL: &str = ".claude-plugin/marketplace.json";
const CODEX_MARKETPLACE_REL: &str = ".agents/plugins/marketplace.json";

/// The plugin marketplace name, and the name it had before the rename. An
/// install made under the old name keeps loading: every read accepts both,
/// and a dev install moves the old registration to the new name.
const MARKETPLACE_NAME: &str = "fno";
const LEGACY_MARKETPLACE_NAME: &str = "footnote";
const PLUGIN_ID: &str = "fno@fno";
const LEGACY_PLUGIN_ID: &str = "fno@footnote";

/// The fno record in a known_marketplaces.json, under either name.
fn known_fno_marketplace(known: &Value) -> Option<&Value> {
    known
        .get(MARKETPLACE_NAME)
        .or_else(|| known.get(LEGACY_MARKETPLACE_NAME))
}

/// The fno entries in an installed_plugins.json, under both plugin ids: a
/// failed migration leaves both registered, and the sweep's guard must see
/// each. None when neither id is registered.
fn registry_fno_entries(registry: &Value) -> Option<Vec<&Value>> {
    let plugins = registry.get("plugins")?;
    let lists: Vec<&Vec<Value>> = [PLUGIN_ID, LEGACY_PLUGIN_ID]
        .iter()
        .filter_map(|id| plugins.get(*id).and_then(Value::as_array))
        .collect();
    (!lists.is_empty()).then(|| lists.into_iter().flatten().collect())
}

/// The commands that retire an old-name Claude registration, so the
/// following `marketplace add` + `install` leave one fno@fno. Empty when
/// known_marketplaces.json names no old-name marketplace.
fn legacy_claude_migration(claude_dir: &Path) -> Vec<Vec<String>> {
    let known = std::fs::read_to_string(claude_dir.join("plugins/known_marketplaces.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if known
        .and_then(|k| k.get(LEGACY_MARKETPLACE_NAME).cloned())
        .is_none()
    {
        return Vec::new();
    }
    let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    vec![
        argv(&["claude", "plugin", "uninstall", LEGACY_PLUGIN_ID]),
        argv(&[
            "claude",
            "plugin",
            "marketplace",
            "remove",
            LEGACY_MARKETPLACE_NAME,
        ]),
    ]
}

/// The stage serves its own copy of the public manifest with the fno entry
/// pointed at the stage root (`source: "./"`), so `claude plugin install
/// fno@fno` resolves in place and never clones a GitHub ref: the public
/// pins (`stable`, `nightly`) are release-side refs a dev machine cannot rely
/// on. The buddy entry points at `./plugins/buddy` the same way. The repo file
/// stays verbatim. Anything that would leave the fno entry
/// unserved is Err: a verbatim copy would silently resurrect the clone trap.
fn staged_marketplace_bytes(source_bytes: &str, marketplace: &str) -> Result<String, String> {
    let mut manifest: Value =
        serde_json::from_str(source_bytes).map_err(|e| format!("{marketplace}: {e}"))?;
    let Some(entries) = manifest.get_mut("plugins").and_then(Value::as_array_mut) else {
        return Err(format!(
            "{marketplace}: no plugins array; cannot serve the fno entry locally"
        ));
    };
    let mut rewritten = false;
    for entry in entries.iter_mut() {
        if entry.get("name").and_then(Value::as_str) == Some("fno") {
            if !entry.is_object() {
                return Err(format!(
                    "{marketplace}: the fno entry is not an object; cannot serve it locally"
                ));
            }
            entry["source"] = if marketplace == CODEX_MARKETPLACE_REL {
                json!({"source": "local", "path": "."})
            } else {
                json!("./")
            };
            rewritten = true;
        } else if entry.get("name").and_then(Value::as_str) == Some("buddy") && entry.is_object() {
            entry["source"] = json!("./plugins/buddy");
        }
    }
    if !rewritten {
        return Err(format!(
            "{marketplace}: no fno entry; cannot serve the plugin locally"
        ));
    }
    serde_json::to_string_pretty(&manifest).map_err(|e| format!("{marketplace}: {e}"))
}

/// Refuse a root whose `.claude-plugin/plugin.json` does not name the fno
/// plugin. The stage replaces the live plugin tree, so a stray cwd (another
/// repo, a sibling project) must never become the stage.
fn require_fno_source(source_root: &Path) -> Result<(), String> {
    let manifest = source_root.join(".claude-plugin").join("plugin.json");
    let names_fno = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .is_some_and(|v| v.get("name").and_then(Value::as_str) == Some("fno"));
    if names_fno {
        return Ok(());
    }
    let hint =
        match std::fs::read_to_string(crate::state_layout::place(&state_root(), "source-path")) {
            Ok(pin) if !pin.trim().is_empty() => format!(
                "the pinned fno checkout is {0}: pass --source {0}",
                pin.trim()
            ),
            _ => "cd into the fno checkout and rerun, or pass --source".to_string(),
        };
    Err(format!(
        "{} is not the fno plugin source (no .claude-plugin/plugin.json naming \"fno\"); the stage is untouched; {hint}",
        source_root.display()
    ))
}

/// Rebuild `<stage_parent>/fno` from `source_root` (git-tracked +
/// untracked-but-not-ignored files). Builds into a sibling temp dir and swaps
/// by rename so live sessions execing hooks from the stage never see a
/// half-built tree; running bash hooks keep their open inode and finish on
/// old bytes. Returns the stage path and the file count copied.
fn build_stage(source_root: &Path, stage_parent: &Path) -> Result<(PathBuf, usize), String> {
    build_stage_as(source_root, stage_parent, Profile::Full)
}

/// `build_stage` for one profile. A pack stage lives at `plugin-stage/fno-pack`
/// and holds no mux path and no hook entry that runs one.
fn build_stage_as(
    source_root: &Path,
    stage_parent: &Path,
    profile: Profile,
) -> Result<(PathBuf, usize), String> {
    require_fno_source(source_root)?;
    let mux = match profile {
        Profile::Full => None,
        Profile::Pack => Some(MuxPaths::load(source_root)?),
    };
    let listed = run_checked(
        &[
            "git".to_string(),
            "ls-files".to_string(),
            "-z".to_string(),
            "-co".into(),
            "--exclude-standard".into(),
        ],
        Some(source_root),
    )?;
    let dir = profile.stage_dir();
    let dest = stage_parent.join(dir);
    let pid = std::process::id();
    let new_dir = stage_parent.join(format!(".{dir}.new-{pid}"));
    let old_dir = stage_parent.join(format!(".{dir}.old-{pid}"));
    let _ = std::fs::remove_dir_all(&new_dir);
    let _ = std::fs::remove_dir_all(&old_dir);
    std::fs::create_dir_all(&new_dir).map_err(|e| format!("stage: {e}"))?;

    let copy_all = || -> Result<usize, String> {
        let mut copied = 0usize;
        for rel in listed.split('\0').filter(|s| !s.is_empty()) {
            if mux.as_ref().is_some_and(|m| m.covers(rel)) {
                continue;
            }
            let src = source_root.join(rel);
            let meta = match std::fs::symlink_metadata(&src) {
                Ok(m) => m,
                Err(_) => continue, // cached by git but deleted in the tree
            };
            if meta.is_symlink() || !meta.is_file() {
                continue;
            }
            let target = new_dir.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("stage: {e}"))?;
            }
            std::fs::copy(&src, &target).map_err(|e| format!("stage: {rel}: {e}"))?;
            copied += 1;
        }
        if let Some(m) = &mux {
            for rel in HOOK_FILES {
                let path = new_dir.join(rel);
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let filtered = m
                    .filter_hooks(&text)
                    .map_err(|e| format!("stage: {rel}: {e}"))?;
                std::fs::write(&path, filtered).map_err(|e| format!("stage: {rel}: {e}"))?;
            }
        }
        Ok(copied)
    };
    let copied = match copy_all() {
        Ok(n) => n,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&new_dir);
            return Err(e);
        }
    };
    if let Ok(head) = run_checked(
        &["git".to_string(), "rev-parse".into(), "HEAD".into()],
        Some(source_root),
    ) {
        let stamp = json!({
            "source_head": head,
            "built_at": chrono::Utc::now().to_rfc3339(),
            "profile": if profile == Profile::Pack { "pack" } else { "full" },
        });
        let _ = std::fs::write(new_dir.join(STAGE_STAMP), stamp.to_string());
    }

    for marketplace in [MARKETPLACE_REL, CODEX_MARKETPLACE_REL] {
        let manifest = new_dir.join(marketplace);
        if manifest.is_file() {
            let text = std::fs::read_to_string(&manifest)
                .map_err(|e| format!("stage: {marketplace}: {e}"))?;
            let rewritten = staged_marketplace_bytes(&text, marketplace)?;
            std::fs::write(&manifest, rewritten)
                .map_err(|e| format!("stage: {marketplace}: {e}"))?;
        }
    }

    if dest.exists() {
        carry_referenced_scripts(&dest, &new_dir, mux.as_ref());
        // ponytail: two concurrent manual installs can race these renames;
        // the update:fno claim serializes the automated path.
        std::fs::rename(&dest, &old_dir)
            .map_err(|e| format!("stage: could not set aside the live stage: {e}"))?;
        if let Err(e) = std::fs::rename(&new_dir, &dest) {
            let _ = std::fs::rename(&old_dir, &dest);
            return Err(format!("stage: could not activate the new stage: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old_dir);
    } else if let Err(e) = std::fs::rename(&new_dir, &dest) {
        let _ = std::fs::remove_dir_all(&new_dir);
        return Err(format!("stage: {e}"));
    }
    Ok((dest, copied))
}

#[derive(Serialize, Default)]
struct StageCheck {
    status: &'static str,
    stage: String,
    source: String,
    source_head: String,
    differing_count: usize,
    missing_count: usize,
    sample: Vec<String>,
    remedy: String,
    detail: Option<String>,
    /// From the stage stamp; None for a stage built before stamps existed.
    stage_head: Option<String>,
    built_at: Option<String>,
    /// Commits from `stage_head` to source HEAD, when git can count them.
    commits_behind: Option<u64>,
    /// Drift (differing + missing) per top-level directory.
    groups: std::collections::BTreeMap<String, usize>,
}

impl StageCheck {
    /// "skills 3, hooks 1; staged from <rev> at <when>, N commit(s) behind",
    /// or empty when there is no drift to explain.
    fn lag_summary(&self) -> String {
        let mut out = self
            .groups
            .iter()
            .map(|(group, n)| format!("{group} {n}"))
            .collect::<Vec<_>>()
            .join(", ");
        if out.is_empty() {
            return out;
        }
        if let Some(head) = &self.stage_head {
            out.push_str(&format!(
                "; staged from {} at {}",
                head.chars().take(12).collect::<String>(),
                self.built_at.as_deref().unwrap_or("?")
            ));
            if let Some(n) = self.commits_behind {
                out.push_str(&format!(", {n} commit(s) behind"));
            }
        }
        out
    }
}

fn absent_check(stage: &Path, source: &Path) -> StageCheck {
    StageCheck {
        status: "absent",
        stage: stage.display().to_string(),
        source: source.display().to_string(),
        source_head: String::new(),
        differing_count: 0,
        missing_count: 0,
        sample: Vec::new(),
        remedy: String::new(),
        detail: None,
        ..Default::default()
    }
}

fn unknown_check(stage: &Path, source: &Path, detail: String) -> StageCheck {
    StageCheck {
        status: "unknown",
        stage: stage.display().to_string(),
        source: source.display().to_string(),
        source_head: String::new(),
        differing_count: 0,
        missing_count: 0,
        sample: Vec::new(),
        remedy: String::new(),
        detail: Some(detail),
        ..Default::default()
    }
}

/// Byte verdict for the stage against the source checkout's HEAD. Stage files
/// HEAD lacks are ignored: the swap carries referenced scripts forward on
/// purpose, so extra files are not drift.
fn check_stage_report(stage: &Path, source_dir: &Path) -> StageCheck {
    if !stage.exists() {
        return absent_check(stage, source_dir);
    }
    let root = match repo_root(source_dir) {
        Ok(r) => r,
        Err(e) => return unknown_check(stage, source_dir, e),
    };
    let head = match run_checked(
        &[
            "git".to_string(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ],
        Some(&root),
    ) {
        Ok(h) => h,
        Err(e) => return unknown_check(stage, source_dir, e),
    };
    let listing = match run_checked(
        &[
            "git".to_string(),
            "ls-tree".to_string(),
            "-r".to_string(),
            "-z".to_string(),
            "HEAD".to_string(),
        ],
        Some(&root),
    ) {
        Ok(l) => l,
        Err(e) => return unknown_check(stage, source_dir, e),
    };
    // A pack stage leaves the mux paths out and rewrites the hook files on
    // purpose, so neither is drift.
    let pack = std::fs::read_to_string(stage.join(STAGE_STAMP))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .is_some_and(|v| v.get("profile").and_then(Value::as_str) == Some("pack"));
    let mux = match pack.then(|| MuxPaths::load(&root)).transpose() {
        Ok(m) => m,
        Err(e) => return unknown_check(stage, source_dir, e),
    };

    let mut tracked: Vec<(String, String)> = Vec::new(); // (path, blob sha)
    for entry in listing.split('\0').filter(|s| !s.is_empty()) {
        let (meta, path) = match entry.split_once('\t') {
            Some(pair) => pair,
            None => continue,
        };
        let mut parts = meta.split_whitespace();
        let mode = parts.next().unwrap_or("");
        let kind = parts.next().unwrap_or("");
        let sha = parts.next().unwrap_or("").to_string();
        // Symlinks (120000) and submodules (160000) are not files; build_stage
        // skips them too.
        if kind != "blob" || mode == "120000" || mode == "160000" {
            continue;
        }
        tracked.push((path.to_string(), sha));
    }

    let mut missing: Vec<String> = Vec::new();
    let mut to_hash: Vec<(&str, &str)> = Vec::new(); // (stage path, HEAD sha)
    let mut manifests = Vec::new();
    for (path, sha) in &tracked {
        if mux.as_ref().is_some_and(|m| m.covers(path)) {
            continue;
        }
        if [MARKETPLACE_REL, CODEX_MARKETPLACE_REL].contains(&path.as_str())
            || (mux.is_some() && HOOK_FILES.contains(&path.as_str()))
        {
            // The stage serves a rewritten copy of this file
            // (staged_marketplace_bytes, filter_hooks), so no HEAD sha can match it.
            manifests.push(path.as_str());
            continue;
        }
        if stage.join(path).is_file() {
            to_hash.push((path, sha));
        } else {
            missing.push(path.clone());
        }
    }
    let mut differing: Vec<String> = Vec::new();
    if !to_hash.is_empty() {
        let stdin = format!(
            "{}\n",
            to_hash
                .iter()
                .map(|(p, _)| *p)
                .collect::<Vec<_>>()
                .join("\n")
        );
        match run_checked_stdin(
            &[
                "git".to_string(),
                "hash-object".to_string(),
                "--stdin-paths".to_string(),
            ],
            // Relative paths resolve against the STAGE, wherever --check
            // was invoked from; hash-object needs no repo for plain hashing.
            Some(stage),
            &stdin,
        ) {
            Ok(out) => {
                for (line, (path, sha)) in out.lines().zip(to_hash.iter()) {
                    if line.trim() != *sha {
                        differing.push((*path).to_string());
                    }
                }
            }
            Err(e) => return unknown_check(stage, source_dir, e),
        }
    }

    for marketplace in manifests {
        let stage_manifest = stage.join(marketplace);
        if !stage_manifest.is_file() {
            missing.push(marketplace.to_string());
        } else {
            // A pack hook file is filtered from its HEAD blob, so a dirty
            // working-tree hook file reads stale here as it does on a full stage.
            let verdict = match &mux {
                Some(m) if HOOK_FILES.contains(&marketplace) => run_checked(
                    &["git".into(), "show".into(), format!("HEAD:{marketplace}")],
                    Some(&root),
                )
                .and_then(|t| m.filter_hooks(&t)),
                _ => std::fs::read_to_string(root.join(marketplace))
                    .map_err(|e| format!("{marketplace}: {e}"))
                    .and_then(|t| staged_marketplace_bytes(&t, marketplace)),
            }
            .and_then(|expected| {
                std::fs::read_to_string(&stage_manifest)
                    .map(|actual| actual == expected)
                    .map_err(|e| format!("{marketplace}: {e}"))
            });
            match verdict {
                Err(e) => return unknown_check(stage, source_dir, e),
                Ok(false) => differing.push(marketplace.to_string()),
                Ok(true) => {}
            }
        }
    }

    let mut combined = differing.clone();
    combined.extend(missing.clone());
    combined.sort();
    let status = if combined.is_empty() {
        "fresh"
    } else {
        "stale"
    };
    let mut groups = std::collections::BTreeMap::new();
    for path in &combined {
        let group = path.split_once('/').map_or("(root)", |(top, _)| top);
        *groups.entry(group.to_string()).or_insert(0) += 1;
    }
    let stamp: Option<Value> = std::fs::read_to_string(stage.join(STAGE_STAMP))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let stamp_field = |key: &str| {
        stamp
            .as_ref()
            .and_then(|v| v.get(key))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let stage_head = stamp_field("source_head");
    let commits_behind = stage_head.as_ref().and_then(|from| {
        run_checked(
            &[
                "git".to_string(),
                "rev-list".into(),
                "--count".into(),
                format!("{from}..HEAD"),
            ],
            Some(&root),
        )
        .ok()
        .and_then(|n| n.parse().ok())
    });
    let source_root = root.display().to_string();
    StageCheck {
        status,
        stage: stage.display().to_string(),
        source: source_root.clone(),
        source_head: head,
        differing_count: differing.len(),
        missing_count: missing.len(),
        sample: combined.into_iter().take(10).collect(),
        remedy: format!("fno config plugin install claude --source {source_root}"),
        detail: None,
        stage_head,
        built_at: stamp_field("built_at"),
        commits_behind,
        groups,
    }
}

/// One fno plugin root on disk: a tree that looks like (or is recorded as) an
/// installed copy of this plugin.
#[derive(Debug, Clone, Serialize)]
struct PluginRoot {
    path: PathBuf,
    origin: &'static str,
    live: bool,
}

/// Canonical-equality path compare; falls back to raw equality when either
/// side cannot be canonicalized (a not-yet-created path).
fn paths_equal(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

fn push_root(
    roots: &mut Vec<PluginRoot>,
    seen: &mut Vec<PathBuf>,
    path: PathBuf,
    origin: &'static str,
    live: bool,
) {
    if !path.exists() {
        return;
    }
    let key = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    if seen.contains(&key) {
        return;
    }
    seen.push(key);
    roots.push(PluginRoot { path, origin, live });
}

/// The installPath strings installed_plugins.json registers for fno,
/// existence unchecked: the sweep's guard must fire on the registry's word
/// alone, since a registered-but-deleted path is exactly the lie the sweep
/// must never create.
fn registry_install_paths(home: &Path) -> Vec<PathBuf> {
    let text = match std::fs::read_to_string(home.join(".claude/plugins/installed_plugins.json")) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    registry_fno_entries(&v)
        .map(|list| {
            list.iter()
                .filter_map(|e| e.get("installPath").and_then(Value::as_str))
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The fno marketplace's `source.source` kind ("directory", "file",
/// "github", ...). None when the registry is unreadable or names no fno
/// marketplace.
fn marketplace_source_kind(home: &Path) -> Option<String> {
    let text =
        std::fs::read_to_string(home.join(".claude/plugins/known_marketplaces.json")).ok()?;
    let v = serde_json::from_str::<Value>(&text).ok()?;
    known_fno_marketplace(&v)?
        .get("source")?
        .get("source")?
        .as_str()
        .map(String::from)
}

/// Every fno plugin root this machine could load or mistake for the loaded
/// one, plus one detail line per registry file that could not be read. A
/// path that does not exist on disk is never reported, and an unreadable
/// registry never contributes a root reported as fresh.
///
/// Live means "the harness loads this tree in place": Claude records a local
/// (directory or file) marketplace's own path as its install location and
/// execs from there, so the marketplace root is live exactly when the
/// marketplace shape is local. `installPath` in installed_plugins.json is a
/// recorded string contradicted by that behaviour, never live; nor are the
/// orphan copies.
fn plugin_roots_for(home: &Path) -> (Vec<PluginRoot>, Vec<String>) {
    let mut roots: Vec<PluginRoot> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut detail: Vec<String> = Vec::new();

    let marketplaces = home.join(".claude/plugins/known_marketplaces.json");
    match std::fs::read_to_string(&marketplaces)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
    {
        Err(e) => detail.push(format!("{} unreadable: {e}", marketplaces.display())),
        Ok(v) => match (
            known_fno_marketplace(&v),
            known_fno_marketplace(&v).and_then(|f| f.get("source")),
        ) {
            (Some(rec), Some(source)) => {
                let local = matches!(
                    source.get("source").and_then(Value::as_str),
                    Some("directory" | "file")
                );
                let path = rec
                    .get("installLocation")
                    .and_then(Value::as_str)
                    .or_else(|| source.get("path").and_then(Value::as_str));
                if let Some(p) = path {
                    push_root(
                        &mut roots,
                        &mut seen,
                        PathBuf::from(p),
                        "marketplace",
                        local,
                    );
                }
            }
            _ => detail.push(format!(
                "{} names no fno marketplace",
                marketplaces.display()
            )),
        },
    }

    let registry = home.join(".claude/plugins/installed_plugins.json");
    match std::fs::read_to_string(&registry)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string()))
    {
        Err(e) => detail.push(format!("{} unreadable: {e}", registry.display())),
        Ok(v) => {
            let entries = registry_fno_entries(&v);
            match entries {
                None => detail.push(format!("{} names no {PLUGIN_ID} entry", registry.display())),
                Some(list) => {
                    for entry in list {
                        if let Some(p) = entry.get("installPath").and_then(Value::as_str) {
                            push_root(&mut roots, &mut seen, PathBuf::from(p), "registry", false);
                        }
                    }
                }
            }
        }
    }

    for path in [
        home.join(".gemini/config/plugins/footnote"),
        home.join(".codex/plugins/cache/footnote-local"),
        home.join(".claude/plugins/cache/fno"),
        home.join(".claude/plugins/cache/footnote"),
    ] {
        push_root(&mut roots, &mut seen, path, "orphan", false);
    }

    (roots, detail)
}

fn plugin_roots() -> (Vec<PluginRoot>, Vec<String>) {
    plugin_roots_for(&dirs_home())
}

/// A single root's verdict carrying the role it played in the enumeration,
/// plus the rendered one-line note and blocker text the doctor printer and
/// blocker list print verbatim (the Python side is transport only).
#[derive(Serialize)]
struct RootVerdict {
    #[serde(flatten)]
    check: StageCheck,
    path: String,
    origin: &'static str,
    live: bool,
    kind: &'static str,
    note: String,
    blocker: Option<String>,
}

/// The doctor-shaped fold: flat keys pointed at the live root, status the
/// worst across roots, and the per-root detail under `roots`.
#[derive(Serialize)]
struct RootsReport {
    status: String,
    sha: Option<String>,
    installed_at: Option<String>,
    kind: Option<&'static str>,
    stage: Option<String>,
    remedy: Option<String>,
    detail: Option<String>,
    roots: Vec<RootVerdict>,
    enumeration_detail: Vec<String>,
}

fn root_note(live: bool, check: &StageCheck) -> String {
    if let Some(detail) = check
        .detail
        .as_deref()
        .filter(|_| check.status == "unknown")
    {
        return format!(" (check could not run: {detail})");
    }
    let drift = check.differing_count + check.missing_count;
    let remedy = check.remedy.as_str();
    let mut note = format!(
        " ({drift} file(s) differ from source HEAD: {})",
        check.lag_summary()
    );
    if drift == 0 {
        note.clear();
    }
    if !live && drift > 0 {
        let remedy = if remedy.is_empty() {
            "fno config plugin install claude"
        } else {
            remedy
        };
        note.push_str(&format!(". Fix: {remedy} removes the stale second copy"));
    }
    note
}

/// Byte verdict for EVERY enumerated root against source HEAD. The exit code
/// is the worst status across roots, so one stale root exits 3 even when the
/// live root is fresh.
fn check_roots_report(
    scan: &[PluginRoot],
    detail: Vec<String>,
    source_dir: &Path,
) -> (RootsReport, i32) {
    // Worst status reads stale above unknown: stale carries a remedy and the
    // doctor exit gate keys on it, while unknown only names its gap.
    fn rank(status: &str) -> u8 {
        match status {
            "stale" => 2,
            "unknown" => 1,
            _ => 0,
        }
    }
    let mut roots = Vec::new();
    let mut worst: Option<(u8, &'static str)> = None;
    // A live root means the marketplace shape is local and the harness loads
    // it in place; every other root is then a retired copy (B7): the leftover
    // GitHub cache path installed_plugins.json still records. Its drift stays
    // reported per root, but it never blocks the check - a correct directory
    // install must not exit 3 over a cache nobody loads.
    let any_live = scan.iter().any(|r| r.live);
    for root in scan {
        let check = check_stage_report(&root.path, source_dir);
        let drift = check.differing_count + check.missing_count;
        let retired = !root.live && any_live;
        let blocker = if check.status == "stale" && !retired {
            let role = if root.live { "live" } else { "second copy" };
            Some(format!(
                "plugin root {} ({}) differs from source HEAD in {} file(s) ({}; e.g. {}). Fix: {}",
                root.path.display(),
                role,
                drift,
                check.lag_summary(),
                check.sample.first().map(String::as_str).unwrap_or("?"),
                check.remedy
            ))
        } else {
            None
        };
        let note = if retired {
            " (retired cache: kept because installed_plugins.json registers it; \
             the harness loads the live root in place)"
                .to_string()
        } else {
            root_note(root.live, &check)
        };
        let last_status = check.status;
        roots.push(RootVerdict {
            path: root.path.display().to_string(),
            origin: root.origin,
            live: root.live,
            kind: "stage",
            note,
            blocker,
            check,
        });
        // A retired copy folds as fresh: the byte verdict above stays stale in
        // roots[], but the install it belongs to is correct.
        let fold_status = if retired { "fresh" } else { last_status };
        let rank_cur = rank(fold_status);
        if worst.map_or(true, |w| rank_cur > w.0) {
            worst = Some((rank_cur, fold_status));
        }
    }
    let live = roots.iter().find(|r| r.live);
    let joined = if detail.is_empty() {
        None
    } else {
        Some(detail.join("; "))
    };
    let report = RootsReport {
        status: worst.map(|w| w.1).unwrap_or("unknown").to_string(),
        sha: live.map(|r| r.check.source_head.clone()),
        installed_at: None,
        kind: if live.is_some() || roots.is_empty() {
            Some("stage")
        } else {
            None
        },
        stage: live.map(|r| r.path.clone()),
        remedy: live.map(|r| r.check.remedy.clone()),
        detail: joined,
        enumeration_detail: detail,
        roots,
    };
    let exit = worst.map(|w| check_exit_code(w.1)).unwrap_or(4);
    (report, exit)
}

fn print_roots_report(report: &RootsReport) {
    for line in &report.enumeration_detail {
        println!("plugin root: {line}");
    }
    for root in &report.roots {
        let role = if root.live { "live" } else { "second copy" };
        println!("plugin root ({}, {role}): {}", root.origin, root.path);
        print_check(&root.check, false);
    }
}

#[derive(Debug)]
enum RestageOutcome {
    Absent,
    Restaged {
        files: usize,
        root: PathBuf,
        head12: String,
    },
}

/// Rebuild an EXISTING stage from `source_dir`. Never installs, exports env,
/// or reclaims: those belong to install, not deploy. A machine that never set
/// up the directory marketplace has no stage and gets a no-op. Each profile
/// stage on disk rebuilds; the file count sums them.
fn restage_stage(source_dir: &Path, stage_parent: &Path) -> Result<RestageOutcome, String> {
    let present: Vec<Profile> = [Profile::Full, Profile::Pack]
        .into_iter()
        .filter(|p| stage_parent.join(p.stage_dir()).exists())
        .collect();
    if present.is_empty() {
        return Ok(RestageOutcome::Absent);
    }
    let root = repo_root(source_dir)?;
    let head = run_checked(
        &[
            "git".to_string(),
            "rev-parse".to_string(),
            "HEAD".to_string(),
        ],
        Some(&root),
    )?;
    let mut files = 0;
    for profile in present {
        files += build_stage_as(&root, stage_parent, profile)?.1;
    }
    Ok(RestageOutcome::Restaged {
        files,
        root,
        head12: head.chars().take(12).collect(),
    })
}

fn install_claude(stage: &Path, force: bool) -> Result<String, String> {
    // Best effort: a step that fails leaves the old registration loading,
    // and the add + install below still land fno@fno.
    let claude_dir = match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => dirs_home().join(".claude"),
    };
    for argv in legacy_claude_migration(&claude_dir) {
        let _ = run_checked(&argv, None);
    }
    run_checked(
        &[
            "claude".into(),
            "plugin".into(),
            "marketplace".into(),
            "add".into(),
            stage.display().to_string(),
        ],
        None,
    )?;
    let plugin = |action: &str| -> Vec<String> {
        vec![
            "claude".into(),
            "plugin".into(),
            action.into(),
            PLUGIN_ID.into(),
        ]
    };
    if force {
        // update keeps same-version files; force means delete and reinstall,
        // and also covers not-yet-installed, so uninstall failure is fine.
        let _ = run_checked(&plugin("uninstall"), None);
        run_checked(&plugin("install"), None)?;
        return Ok(format!("reinstalled {PLUGIN_ID} from {}", stage.display()));
    }
    match run_checked(&plugin("install"), None) {
        Ok(_) => Ok(format!("installed {PLUGIN_ID} from {}", stage.display())),
        Err(e) if e.contains("already installed") => {
            run_checked(&plugin("update"), None)?;
            Ok(format!("updated {PLUGIN_ID} from {}", stage.display()))
        }
        Err(e) => Err(e),
    }
}

/// A value fno wrote: under a `.fno` state dir, or the exact value this
/// machine's build base resolves to (a custom `paths.cargo_targets_base`).
fn fno_env_value(key: &str, value: &str) -> bool {
    if value.contains(".fno") {
        return true;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let ours = if key == BUILD_DIR_KEY {
        crate::cargo_build_dirs::build_dir_env_value(&cwd)
    } else {
        crate::cargo_build_dirs::sccache_dir(&cwd)
            .display()
            .to_string()
    };
    value == ours
}

/// Drop fno's build-dir and sccache keys from a Claude settings `env` map.
/// An `env` this pass empties goes too. Returns the count removed.
fn strip_claude_env(
    data: &mut serde_json::Map<String, Value>,
    ours: impl Fn(&str, &str) -> bool,
) -> usize {
    let Some(env) = data.get_mut("env").and_then(Value::as_object_mut) else {
        return 0;
    };
    let before = env.len();
    for key in [BUILD_DIR_KEY, SCCACHE_DIR_KEY] {
        if env
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|v| ours(key, v))
        {
            env.remove(key);
        }
    }
    let removed = before - env.len();
    if removed > 0 && env.is_empty() {
        data.remove("env");
    }
    removed
}

/// Line-based, so a codex config keeps its comments and layout. Drops fno's
/// keys from `[shell_environment_policy.set]`, and the header when this pass
/// emptied the table. `None` when nothing matched.
fn strip_codex_env(text: &str, ours: impl Fn(&str, &str) -> bool) -> Option<String> {
    let owned = |t: &str| {
        [BUILD_DIR_KEY, SCCACHE_DIR_KEY].into_iter().any(|key| {
            t.strip_prefix(key)
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
                .is_some_and(|v| ours(key, v.trim().trim_matches('"')))
        })
    };
    let mut out: Vec<&str> = Vec::new();
    let mut removed = 0;
    let mut in_set = false;
    // The open set header: its index in `out` and the keys removed under it.
    // A surviving key closes it, so the header stays.
    let mut open: Option<(usize, usize)> = None;
    let mut emptied = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            emptied.extend(open.take().filter(|&(_, n)| n > 0).map(|(at, _)| at));
            in_set = t == "[shell_environment_policy.set]";
            if in_set {
                open = Some((out.len(), 0));
            }
        } else if in_set {
            if owned(t) {
                removed += 1;
                if let Some((_, n)) = open.as_mut() {
                    *n += 1;
                }
                continue;
            }
            if !t.is_empty() {
                open = None;
            }
        }
        out.push(line);
    }
    emptied.extend(open.filter(|&(_, n)| n > 0).map(|(at, _)| at));
    for at in emptied.into_iter().rev() {
        out.remove(at);
    }
    if removed == 0 {
        return None;
    }
    let mut joined = out.join("\n");
    joined.push('\n');
    Some(joined)
}

/// Drop the marked rc block older installs appended. `None` when absent.
fn strip_rc_block(text: &str) -> Option<String> {
    let mark_at = text.find(RC_MARK)?;
    let end = marked_block_end(text, mark_at);
    Some([&text[..mark_at], &text[end..]].concat())
}

/// Where the marked block ends in `text`: past the mark line plus every
/// consecutive export of a key we own. The mark line's own newline is
/// consumed first, so the scan starts on the first export, never on the
/// empty string before it.
fn marked_block_end(text: &str, mark_at: usize) -> usize {
    let after_mark = mark_at + RC_MARK.len();
    let mut block_end = (after_mark + 1).min(text.len());
    for line in text[block_end..].split('\n') {
        let ours = line.starts_with(&format!("export {BUILD_DIR_KEY}="))
            || line.starts_with(&format!("export {SCCACHE_DIR_KEY}="));
        if !ours {
            break;
        }
        block_end += line.len() + 1;
    }
    block_end.min(text.len())
}

/// Older installs exported CARGO_BUILD_BUILD_DIR and SCCACHE_DIR machine-wide:
/// the shell rc, Claude settings env and Codex shell env. That moved every
/// Rust project's build files under ~/.fno, where reclaim sweeps them. Install
/// now removes those lines instead. Footnote builds still land in the fno
/// base: fno commands, the target stop hook and the daemon set the build dir per
/// process, and the repo's rustc wrapper sets SCCACHE_DIR. Returns the files
/// rewritten, and an error line per file left alone.
fn retract_env_exports() -> (Vec<PathBuf>, Vec<String>) {
    let home = dirs_home();
    let mut changed = Vec::new();
    let mut errors = Vec::new();
    let claude = home.join(".claude/settings.json");
    if let Ok(text) = std::fs::read_to_string(&claude) {
        // Unparseable: leave it. The file is the user's.
        match serde_json::from_str::<serde_json::Map<String, Value>>(&text) {
            Ok(mut data) => {
                if strip_claude_env(&mut data, fno_env_value) > 0 {
                    let text = serde_json::to_string_pretty(&data).unwrap_or_default() + "\n";
                    match std::fs::write(&claude, text) {
                        Ok(()) => changed.push(claude),
                        Err(e) => errors.push(format!("claude env: {e}")),
                    }
                }
            }
            Err(e) => errors.push(format!(
                "claude env: {} is not valid JSON ({e}); left alone",
                claude.display()
            )),
        }
    }
    let codex = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"))
        .join("config.toml");
    let edits: [(PathBuf, fn(&str) -> Option<String>); 3] = [
        (codex, |t| strip_codex_env(t, fno_env_value)),
        (home.join(".zshrc"), strip_rc_block),
        (home.join(".bashrc"), strip_rc_block),
    ];
    for (path, strip) in edits {
        let Some(stripped) = std::fs::read_to_string(&path).ok().and_then(|t| strip(&t)) else {
            continue;
        };
        match std::fs::write(&path, stripped) {
            Ok(()) => changed.push(path),
            Err(e) => errors.push(format!("{}: {e}", path.display())),
        }
    }
    (changed, errors)
}

/// Remove the second copies a successful install leaves behind. The gemini
/// and codex-dev-channel copies are unconditional. The Claude cache copy is
/// guarded: removed only when the marketplace loads in place (directory or
/// file shape) and a live root was proven and that live root is not the
/// cache itself. `home` and `roots` come from the caller's scan, so the
/// removal and any report can never disagree about which root is live.
fn remove_stale_copies(home: &Path, roots: &[PluginRoot]) -> (Vec<PathBuf>, Vec<String>) {
    let mut removed = Vec::new();
    let mut refused = Vec::new();
    for path in [
        home.join(".gemini/config/plugins/footnote"),
        home.join(".codex/plugins/cache/footnote-local"),
    ] {
        if path.exists() {
            let _ = std::fs::remove_dir_all(&path);
            removed.push(path);
        }
    }
    for name in [MARKETPLACE_NAME, LEGACY_MARKETPLACE_NAME] {
        let cache = home.join(".claude/plugins/cache").join(name);
        if !cache.exists() {
            continue;
        }
        // installed_plugins.json is Claude's own registry: a tree it registers
        // inside is never the sweep's to delete, whatever its staleness. Install
        // registered cache/<name>/fno/<ver> here and this sweep deleted it in
        // the same run, leaving the registry pointing at a missing tree and the
        // review preflight's plugin-file check reading unknown.
        if let Some(registered) = registry_install_paths(home)
            .into_iter()
            .find(|p| p.starts_with(&cache))
        {
            refused.push(format!(
                "claude cache {} kept: installed_plugins.json registers {} inside it",
                cache.display(),
                registered.display()
            ));
            continue;
        }
        let live_roots: Vec<&PluginRoot> = roots.iter().filter(|r| r.live).collect();
        let cache_is_live = live_roots.iter().any(|r| paths_equal(&r.path, &cache));
        match marketplace_source_kind(home) {
            None => refused.push(format!(
                "claude cache {} kept: fno marketplace shape unreadable",
                cache.display()
            )),
            Some(kind) if kind != "directory" && kind != "file" => refused.push(format!(
                "claude cache {} kept: marketplace source is '{kind}', not a local directory",
                cache.display()
            )),
            _ if cache_is_live => refused.push(format!(
                "claude cache {} kept: it is the live root",
                cache.display()
            )),
            _ if live_roots.is_empty() => refused.push(format!(
                "claude cache {} kept: no live root proven on this machine",
                cache.display()
            )),
            _ => {
                // A bare removed-path line reads as an install failure, so the
                // receipt names the live root the harness loads instead. Rides
                // the second channel: call sites print its lines verbatim.
                let live = live_roots
                    .iter()
                    .map(|r| r.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                refused.push(format!(
                    "claude loads fno in place from {live}; removed the unused cache copy {}",
                    cache.display()
                ));
                let _ = std::fs::remove_dir_all(&cache);
                removed.push(cache);
            }
        }
    }
    (removed, refused)
}

/// `--source` default: the source checkout `fno doctor update` pinned at its
/// last successful install, else the cwd.
fn default_source_dir() -> PathBuf {
    let pin = crate::state_layout::place(&state_root(), "source-path");
    if let Ok(text) = std::fs::read_to_string(&pin) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return PathBuf::from(trimmed);
        }
    }
    std::env::current_dir().unwrap_or_default()
}

/// The release channel the harness's footnote plugin comes from, or None for
/// a local (dev) install or no install. A dev install must refuse to replace
/// a release one: `claude plugin marketplace add <stage>` silently repoints
/// an existing GitHub fno marketplace at the stage, exit 0.
fn release_channel(harness: &str, claude_dir: &Path, codex_dir: &Path) -> Option<String> {
    let read = |path: PathBuf| -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    };
    let (place, git_ref) = match harness {
        "claude" => {
            let known = read(claude_dir.join("plugins").join("known_marketplaces.json"))?;
            let source = known_fno_marketplace(&known)?.get("source")?;
            let place = match source.get("source").and_then(Value::as_str)? {
                "github" => format!("github:{}", source.get("repo")?.as_str()?),
                "git" | "url" => source.get("url")?.as_str()?.to_string(),
                _ => return None,
            };
            (
                place,
                source
                    .get("ref")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        "codex" => {
            let marker = read(codex_dir.join("footnote").join("plugin-channel.json"))?;
            if marker.get("channel").and_then(Value::as_str) != Some("release") {
                return None;
            }
            let place = marker
                .get("source")
                .and_then(Value::as_str)
                .unwrap_or("github")
                .to_string();
            (
                place,
                marker
                    .get("ref")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            )
        }
        _ => return None,
    };
    let channel = match git_ref.as_deref() {
        None => "main",
        Some(r) if r.contains("nightly") => "nightly",
        Some(r) => r,
    };
    let git_ref = git_ref.as_deref().unwrap_or("main");
    Some(format!("{channel} ({place}#{git_ref})"))
}

fn release_channel_here(harness: &str) -> Option<String> {
    let home = dirs_home();
    let env_dir = |key: &str, fallback: &str| match std::env::var_os(key) {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => home.join(fallback),
    };
    release_channel(
        harness,
        &env_dir("CLAUDE_CONFIG_DIR", ".claude"),
        &env_dir("CODEX_HOME", ".codex"),
    )
}

fn release_refusal(harness: &str, channel: &str) -> String {
    format!(
        "plugin install {harness}: refused. Your footnote plugin comes from the {channel} channel. \
         This command installs your local checkout for development, and it would move you off that channel. \
         To switch to your local checkout, run: fno config plugin install {harness} --dev"
    )
}

fn check_exit_code(status: &str) -> i32 {
    // Matches fno doctor plugin-file: fresh/absent 0, stale 3, unknown 4.
    match status {
        "stale" => 3,
        "unknown" => 4,
        _ => 0,
    }
}

fn print_check(report: &StageCheck, json: bool) {
    if json {
        match serde_json::to_string(report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("plugin-install --check: serialization error: {e}"),
        }
        return;
    }
    match report.status {
        "fresh" => println!(
            "plugin stage: fresh (source HEAD {})",
            &report.source_head[..report.source_head.len().min(12)]
        ),
        "stale" => println!(
            "plugin stage: stale ({} differing, {} missing: {}; e.g. {})",
            report.differing_count,
            report.missing_count,
            report.lag_summary(),
            report.sample.first().map(String::as_str).unwrap_or("?")
        ),
        "absent" => println!("plugin stage: absent"),
        _ => println!(
            "plugin stage: unknown ({})",
            report.detail.as_deref().unwrap_or("no detail")
        ),
    }
}

struct PluginInstallArgs {
    mode: Option<String>,
    source: Option<String>,
    stage: Option<String>,
    json: bool,
    force: bool,
    uninstall: bool,
    status: bool,
    quick: bool,
    hooks: bool,
    hooks_status: bool,
    adapter: Option<String>,
    role: Option<String>,
    guard: Option<String>,
    hooks_file: Option<String>,
    extension_src: Option<String>,
    yes: bool,
    dry_run: bool,
    /// Consent to replace a release-channel plugin with the local checkout.
    dev: bool,
    /// Converge codex onto the release channel (setup's wiring).
    release: bool,
    /// A deploy/read-only mode flag (`--check`, `--restage`, ...) that arrived
    /// AFTER a harness positional and was therefore not applied. Never silent:
    /// `plugin-install claude --check` once fell through to a full mutating
    /// install because this was dropped (hurdle B5).
    dropped_mode: Option<String>,
}

fn parse_plugin_install_args(args: &[String]) -> PluginInstallArgs {
    let mut parsed = PluginInstallArgs {
        mode: None,
        source: None,
        stage: None,
        json: false,
        force: false,
        uninstall: false,
        status: false,
        quick: false,
        hooks: false,
        hooks_status: false,
        adapter: None,
        role: None,
        guard: None,
        hooks_file: None,
        extension_src: None,
        yes: false,
        dry_run: false,
        dev: false,
        release: false,
        dropped_mode: None,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--source" => {
                parsed.source = args.get(i + 1).cloned();
                i += 2;
            }
            "--stage" => {
                parsed.stage = args.get(i + 1).cloned();
                i += 2;
            }
            "--adapter" => {
                parsed.adapter = args.get(i + 1).cloned();
                i += 2;
            }
            // The pre-rename spelling rides one release: python still sends it.
            "--team" | "--promote" => {
                parsed.role = args.get(i + 1).cloned();
                i += 2;
            }
            "--guard" => {
                parsed.guard = args.get(i + 1).cloned();
                i += 2;
            }
            "--hooks-file" => {
                parsed.hooks_file = args.get(i + 1).cloned();
                i += 2;
            }
            "--extension-src" => {
                parsed.extension_src = args.get(i + 1).cloned();
                i += 2;
            }
            "--yes" => {
                parsed.yes = true;
                i += 1;
            }
            "--dry-run" => {
                parsed.dry_run = true;
                i += 1;
            }
            "--dev" => {
                parsed.dev = true;
                i += 1;
            }
            "--release" => {
                parsed.release = true;
                i += 1;
            }
            "--json" | "-J" => {
                parsed.json = true;
                i += 1;
            }
            "--force" | "-F" => {
                parsed.force = true;
                i += 1;
            }
            "--uninstall" => {
                parsed.uninstall = true;
                i += 1;
            }
            "--status" => {
                parsed.status = true;
                i += 1;
            }
            "--installed" => {
                parsed.quick = true;
                i += 1;
            }
            "--hooks" => {
                parsed.hooks = true;
                i += 1;
            }
            "--hooks-status" => {
                parsed.hooks_status = true;
                i += 1;
            }
            "--check" | "--restage" => {
                if parsed.mode.is_none() {
                    parsed.mode = Some(args[i].clone());
                } else {
                    parsed.dropped_mode = Some(args[i].clone());
                }
                i += 1;
            }
            other => {
                if !other.starts_with('-') && parsed.mode.is_none() {
                    parsed.mode = Some(other.to_string());
                }
                i += 1;
            }
        }
    }
    parsed
}

pub fn run_plugin_install(args: &[String]) -> i32 {
    let PluginInstallArgs {
        mode,
        source,
        stage,
        json,
        force,
        uninstall,
        status,
        quick,
        hooks,
        hooks_status,
        adapter,
        role,
        guard,
        hooks_file,
        extension_src,
        yes,
        dry_run,
        dev,
        release,
        dropped_mode,
    } = parse_plugin_install_args(args);
    if let Some(flag) = dropped_mode {
        // A mode flag after a harness name (`plugin-install claude --check`)
        // used to be dropped and the mutating install ran (hurdle B5). Refuse
        // and name the read-only form instead.
        eprintln!(
            "plugin install: `{flag}` after a harness name is not a form; \
             the read-only drift check is `fno-agents plugin-install --check` \
             (no harness name; it checks every plugin root)"
        );
        return 2;
    }
    if hooks || hooks_status {
        return run_agy_hooks(
            mode.as_deref(),
            hooks,
            hooks_status,
            adapter.as_deref(),
            role.as_deref(),
            guard.as_deref(),
            hooks_file.as_deref(),
            json,
        );
    }
    match mode.as_deref() {
        // Byte verdict for the stage; doctor's plugin_cache leg calls this.
        // With no --stage, EVERY enumerated root is checked and the exit code
        // is the worst status across roots.
        Some("--check") => {
            let source_dir = source.map(PathBuf::from).unwrap_or_else(default_source_dir);
            if let Some(dir) = stage {
                let stage_path = PathBuf::from(dir);
                let report = check_stage_report(&stage_path, &source_dir);
                print_check(&report, json);
                check_exit_code(report.status)
            } else {
                let (roots, detail) = plugin_roots();
                let (report, worst) = check_roots_report(&roots, detail, &source_dir);
                if json {
                    match serde_json::to_string(&report) {
                        Ok(s) => println!("{s}"),
                        Err(e) => {
                            eprintln!("plugin-install --check: serialization error: {e}")
                        }
                    }
                } else {
                    print_roots_report(&report);
                }
                worst
            }
        }
        // Deploy-only rebuild of an existing stage; fno doctor update calls
        // this after a successful install.
        Some("--restage") => {
            let source_dir = source.map(PathBuf::from).unwrap_or_else(default_source_dir);
            let stage_parent = state_root().join("plugin-stage");
            match restage_stage(&source_dir, &stage_parent) {
                Ok(RestageOutcome::Absent) => {
                    println!("plugin stage: absent, skipped");
                    0
                }
                Ok(RestageOutcome::Restaged {
                    files,
                    root,
                    head12,
                }) => {
                    println!(
                        "plugin stage: restaged {files} files from {} at {head12}",
                        root.display()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("plugin install: {e}");
                    1
                }
            }
        }
        Some(harness) => {
            if harness == "codex" && status {
                return crate::codex_converge::run_status();
            }
            if harness == "codex" && release {
                return crate::codex_converge::run_release(json);
            }
            if harness == "opencode" {
                return run_opencode_arm(harness, json, uninstall, status, quick, yes, dry_run);
            }
            if harness == "grok" && status {
                let receipt = grok_status_receipt();
                println!("{receipt}");
                return if receipt.starts_with("unknown") { 1 } else { 0 };
            }
            if harness == "pi" {
                return run_pi_arm(status, json, extension_src.as_deref());
            }
            if yes || dry_run {
                eprintln!("plugin install: --yes/--dry-run apply to the opencode arm only");
                return 2;
            }
            if uninstall || status || quick {
                eprintln!(
                    "plugin install: --uninstall/--status/--installed apply to the opencode or grok arms"
                );
                return 2;
            }
            if let Some(channel) = release_channel_here(harness).filter(|_| !dev) {
                eprintln!("{}", release_refusal(harness, &channel));
                return 3;
            }
            let source_dir = source.map(PathBuf::from).unwrap_or_else(default_source_dir);
            let outcome = install_harness(harness, force, &source_dir);
            match outcome {
                Ok(detail) => {
                    println!("plugin install {harness}: {detail}");
                    env_retract_receipt();
                    let (roots, _) = plugin_roots();
                    let (stale, refused) = remove_stale_copies(&dirs_home(), &roots);
                    for line in refused {
                        println!("{line}");
                    }
                    if !stale.is_empty() {
                        let joined: Vec<String> =
                            stale.iter().map(|p| p.display().to_string()).collect();
                        println!("removed stale copies: {}", joined.join(", "));
                    }
                    let home = AgentsHome::from_env();
                    let _ =
                        crate::reclaim::run_reclaim(&RECLAIM_ON_INSTALL.map(String::from), &home);
                    0
                }
                Err(e) => {
                    eprintln!("plugin install {harness} FAILED: {e}");
                    1
                }
            }
        }
        None => {
            eprintln!(
                "usage: fno-agents plugin-install <claude|codex|opencode|agy|grok> [--force] [--dev] [--source <dir>]"
            );
            eprintln!("         installs your local checkout for development; exit 3 refuses to replace a release-channel plugin without --dev");
            eprintln!("       fno-agents plugin-install --check [--stage <dir>] [--source <dir>] [--json|-J]");
            eprintln!("         (no --stage checks every plugin root; exit is the worst status across roots)");
            eprintln!("       fno-agents plugin-install --restage [--source <dir>]");
            eprintln!("       fno-agents plugin-install codex --release [--json] | --status");
            eprintln!("         converges codex onto the GitHub stable channel, or prints its freshness verdict");
            2
        }
    }
}

/// The opencode arm: one install/uninstall/status door onto
/// `opencode_install`, plus the shared env cleanup and stale-copy sweep the
/// other harness arms run. `--json` keeps stdout to the receipt alone (the
/// Python door parses it), so the prose side lines move to stderr there.
fn run_opencode_arm(
    _harness: &str,
    json: bool,
    uninstall: bool,
    status: bool,
    quick: bool,
    yes: bool,
    dry_run: bool,
) -> i32 {
    let say = |line: String| {
        if json {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    };
    if uninstall {
        return match crate::opencode_install::uninstall() {
            Ok(receipt) => {
                if json {
                    println!("{}", serde_json::to_string(&receipt).unwrap_or_default());
                } else {
                    println!(
                        "plugin uninstall opencode: {} ({} file(s) removed from {})",
                        receipt.status, receipt.removed, receipt.config_dir
                    );
                    for rel in &receipt.kept {
                        println!(
                            "kept user file: {rel} (its bytes changed since the install; \
                             remove it by hand if it is yours to remove)"
                        );
                    }
                }
                if receipt.kept.is_empty() {
                    0
                } else {
                    3
                }
            }
            Err(e) => {
                eprintln!("plugin uninstall opencode FAILED: {e}");
                1
            }
        };
    }
    if status {
        let value = crate::opencode_install::status_json();
        if json {
            println!("{value}");
        } else {
            print_status_prose(&value);
        }
        return 0;
    }
    if quick {
        let value = crate::opencode_install::installed_status();
        if json {
            println!("{value}");
        } else {
            println!("opencode install: {}", value["status"]);
        }
        return 0;
    }
    // --dry-run: the audit prints, nothing is written - the file install
    // is skipped with it, so the command is a pure preview.
    if dry_run {
        let cwd = std::env::current_dir().unwrap_or_default();
        let findings = crate::opencode_config::audit(&cwd);
        if findings.is_empty() {
            say("plugin array: clean; nothing to disable".to_string());
        }
        for f in &findings {
            say(f.summary());
        }
        for path in crate::opencode_config::omo_config_paths(&crate::opencode_install::config_dir())
        {
            say(format!(
                "INFO opencode: {} keeps oh-my-openagent's settings; inert while no omo plugin loads, footnote never edits it",
                path.display()
            ));
        }

        if let Some(models) = crate::opencode_config::omo_agent_models() {
            let (block, not_carried) = crate::opencode_config::suggested_agent_block(&models);
            if !block.is_empty() && block != "{}" {
                say("suggested agent block (oh-my-openagent's model assignments, mapped onto footnote's agents; copy into opencode.json if you want them):".to_string());
                say(block);
            }
            if !not_carried.is_empty() {
                say(format!(
                    "not carried from oh-my-openagent (no footnote counterpart): {}",
                    not_carried.join(", ")
                ));
            }
        }
        say("--dry-run: nothing written; re-run without --dry-run to apply".to_string());
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    match crate::opencode_install::install(&cwd) {
        Ok(receipt) => {
            let findings = crate::opencode_config::audit(&cwd);
            for path in
                crate::opencode_config::omo_config_paths(&crate::opencode_install::config_dir())
            {
                say(format!(
                    "INFO opencode: {} keeps oh-my-openagent's settings; inert while no omo plugin loads, footnote never edits it",
                    path.display()
                ));
            }

            if let Some(models) = crate::opencode_config::omo_agent_models() {
                let (block, not_carried) = crate::opencode_config::suggested_agent_block(&models);
                if !block.is_empty() && block != "{}" {
                    say("suggested agent block (oh-my-openagent's model assignments, mapped onto footnote's agents; copy into opencode.json if you want them):".to_string());
                    say(block);
                }
                if !not_carried.is_empty() {
                    say(format!(
                        "not carried from oh-my-openagent (no footnote counterpart): {}",
                        not_carried.join(", ")
                    ));
                }
            }
            let (_action, undo, refusals) = decide_plugin_array(&findings, yes, json);
            if json {
                let mut value = serde_json::to_value(&receipt).unwrap_or_else(|_| json!({}));
                value["plugin_array"] = json!({
                    "findings": findings.iter().map(|f| json!({
                        "file": f.file.display().to_string(),
                        "entry": f.entry,
                        "kind": f.kind.label(),
                        "project": f.project,
                    })).collect::<Vec<_>>(),
                    "action": "audit-only: --json never prompts and never edits",
                });
                println!("{}", value);
            } else {
                println!(
                    "plugin install opencode: {} (footnote {} -> {})",
                    receipt.status, receipt.version, receipt.config_dir
                );
                for replaced in &receipt.replaced_legacy {
                    println!(
                        "replaced legacy bridge: {} (backup: {})",
                        replaced.path, replaced.backup
                    );
                }
                for rel in &receipt.kept {
                    println!("kept user file: {rel} (footnote did not overwrite it)");
                }
                for line in undo {
                    println!("{line}");
                }
                for line in refusals {
                    eprintln!("{line}");
                }
            }
            for (line, is_error) in env_retract_lines() {
                if is_error {
                    eprintln!("{line}");
                } else {
                    say(line);
                }
            }
            let (roots, _) = plugin_roots();
            let (stale, refused) = remove_stale_copies(&dirs_home(), &roots);
            for line in refused {
                say(line);
            }
            if !stale.is_empty() {
                let joined: Vec<String> = stale.iter().map(|p| p.display().to_string()).collect();
                say(format!("removed stale copies: {}", joined.join(", ")));
            }
            let home = AgentsHome::from_env();
            let _ = crate::reclaim::run_reclaim(&RECLAIM_ON_INSTALL.map(String::from), &home);
            let full = Profile::Full.stage_dir();
            prime_plugin_root_pointer(&state_root().join("plugin-stage").join(full));
            if receipt.status == "partial" {
                3
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("plugin install opencode FAILED: {e}");
            1
        }
    }
}

/// Print the audit summary, take consent once (--yes, else the inherited
/// TTY), and disable the approved findings. Returns (action, undo lines,
/// refusal lines). A finding inside a project config is reported, never
/// edited; with no TTY and no --yes nothing is edited and stdout names
/// --yes. Under `json` stdout carries the receipt alone: findings and the
/// kept-line move to stderr, and the prompt never fires.
fn decide_plugin_array(
    findings: &[crate::opencode_config::Finding],
    yes: bool,
    json: bool,
) -> (String, Vec<String>, Vec<String>) {
    use std::io::IsTerminal as _;
    use std::io::Write as _;
    if findings.is_empty() {
        return ("none".into(), vec![], vec![]);
    }
    for f in findings {
        if json {
            eprintln!("{}", f.summary());
        } else {
            println!("{}", f.summary());
        }
    }
    let editable: Vec<crate::opencode_config::Finding> =
        findings.iter().filter(|f| !f.project).cloned().collect();
    if editable.is_empty() {
        if json {
            eprintln!("project config entries are reported, never edited");
        } else {
            println!("project config entries are reported, never edited");
        }
        return ("reported".into(), vec![], vec![]);
    }
    let approved = if json {
        // --json never edits, whatever --yes says: the receipt's action text
        // and the guide both promise audit-only under --json.
        false
    } else if yes {
        true
    } else if std::io::stdin().is_terminal() {
        print!("\nDisable these plugin entries? [Y/n] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().read_line(&mut answer);
        !answer.trim().to_lowercase().starts_with('n')
    } else {
        false
    };
    if approved {
        let refs: Vec<&crate::opencode_config::Finding> = editable.iter().collect();
        let (undo, refusals) = crate::opencode_config::disable(&refs);
        return ("disabled".into(), undo, refusals);
    }
    if json {
        eprintln!("kept: --json never edits; re-run without --json to disable them");
    } else {
        println!("kept: re-run `fno config plugin install opencode --yes` to disable them");
    }
    ("kept".into(), vec![], vec![])
}

fn print_status_prose(value: &serde_json::Value) {
    let Some(lines) = value["doctor_lines"].as_array() else {
        println!("opencode: status unknown (the receipt carries no doctor lines)");
        return;
    };
    for line in lines {
        let level = line["level"].as_str().unwrap_or("INFO");
        let text = line["text"].as_str().unwrap_or("");
        println!("{level} {text}");
    }
}

/// The pi arm: install the loop extension into pi's agent dir (honoring
/// `PI_CODING_AGENT_DIR`), or read the install status. Neither path builds
/// the plugin stage or needs a repo checkout, so a relocated pi install
/// works from the binary alone.
///
/// - `--extension-src <path>` (install): copy the source to
///   `<agent dir>/extensions/footnote.ts` through a temp file in the same
///   directory and a rename, so a half-written extension is never loadable.
/// - `--status`: answer `{"installed", "dest", "agent_dir_source", "skills"}`;
///   `installed` means the dest exists and, when a source was named, is
///   byte-equal to it. `skills` reads the same plugin-root pointer the command
///   renderer reads.
fn run_pi_arm(status: bool, json: bool, extension_src: Option<&str>) -> i32 {
    let dest = crate::pi::pi_agent_dir()
        .join("extensions")
        .join("footnote.ts");
    let agent_dir_source = match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(v) if !v.trim().is_empty() => "env",
        _ => "default",
    };
    let skills = match crate::provider::plugin_root() {
        Some(root) if root.join("skills").is_dir() => format!("{}/skills", root.display()),
        Some(root) => format!("unresolved: {} names no skills dir", root.display()),
        None => "unresolved: no plugin-root pointer".to_string(),
    };
    if status {
        let mut installed = dest.is_file();
        if installed {
            if let Some(src) = extension_src {
                installed = files_byte_equal(Path::new(src), &dest).unwrap_or(false);
            }
        }
        print_pi_receipt(installed, &dest, agent_dir_source, &skills, json);
        return 0;
    }
    let Some(src) = extension_src else {
        eprintln!("plugin install pi: --extension-src <path> is required (or pass --status)");
        return 2;
    };
    let src_path = Path::new(src);
    if !src_path.is_file() {
        eprintln!(
            "plugin install pi: extension source {} is not a file",
            src_path.display()
        );
        return 1;
    }
    let dir = dest.parent().unwrap_or_else(|| Path::new("/"));
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("plugin install pi: cannot create {}: {e}", dir.display());
        return 1;
    }
    // Same-directory temp + rename: a reader (pi loading its extensions)
    // never sees a partial copy.
    let tmp = dir.join(format!(".footnote.ts.{}.tmp", std::process::id()));
    if let Err(e) = std::fs::copy(src_path, &tmp) {
        eprintln!("plugin install pi: cannot stage the copy: {e}");
        let _ = std::fs::remove_file(&tmp);
        return 1;
    }
    if let Err(e) = std::fs::rename(&tmp, &dest) {
        eprintln!("plugin install pi: cannot finalize {}: {e}", dest.display());
        let _ = std::fs::remove_file(&tmp);
        return 1;
    }
    print_pi_receipt(true, &dest, agent_dir_source, &skills, json);
    0
}

fn print_pi_receipt(
    installed: bool,
    dest: &Path,
    agent_dir_source: &str,
    skills: &str,
    json: bool,
) {
    if json {
        // cli/label/status/note name the setup wizard's receipt fields, so
        // the Python door relays the answer instead of translating it.
        let answer = serde_json::json!({
            "installed": installed,
            "cli": "pi",
            "label": "pi",
            "status": if installed { "installed" } else { "failed" },
            "note": format!(
                "extension -> {} (agent dir from {}, skills: {})",
                dest.display(),
                agent_dir_source,
                skills
            ),
            "dest": dest.display().to_string(),
            "agent_dir_source": agent_dir_source,
            "skills": skills,
        });
        println!("{answer}");
    } else {
        println!(
            "pi extension {}: {} (agent dir from {}, skills: {})",
            if installed { "installed" } else { "absent" },
            dest.display(),
            agent_dir_source,
            skills
        );
    }
}

fn files_byte_equal(a: &Path, b: &Path) -> std::io::Result<bool> {
    use std::io::Read;
    if a == b {
        return Ok(true);
    }
    let mut left = std::fs::File::open(a)?;
    let mut right = std::fs::File::open(b)?;
    let mut la = [0u8; 8192];
    let mut rb = [0u8; 8192];
    loop {
        let n = left.read(&mut la)?;
        let m = right.read(&mut rb)?;
        if n != m || la[..n] != rb[..m] {
            return Ok(false);
        }
        if n == 0 {
            return Ok(true);
        }
    }
}

/// zcode's CLI config: `~/.zcode/cli/config.json`, HOME-derived. Tests pass
/// an explicit path to the inner fn.
fn zcode_config_path() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| "HOME is unset; cannot locate ~/.zcode/cli/config.json".to_string())?;
    Ok(PathBuf::from(home)
        .join(".zcode")
        .join("cli")
        .join("config.json"))
}

/// Link footnote's stage into zcode's `plugins.dirs` (the CLI's local
/// plugin-dir list). agy_hooks discipline: read the file, refuse and write
/// NOTHING over bytes that do not parse, set `plugins.enabled` and add the
/// stage dir exactly once, leave every other key untouched, then read the
/// file back.
fn install_zcode(stage: &Path, _force: bool) -> Result<String, String> {
    zcode_install_config(&zcode_config_path()?, stage)
}

fn zcode_install_config(config_path: &Path, stage: &Path) -> Result<String, String> {
    let existing = match std::fs::read_to_string(config_path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("cannot read {}: {e}", config_path.display())),
    };
    let mut config: serde_json::Value = match existing.as_deref() {
        Some(text) => serde_json::from_str(text).map_err(|e| {
            format!(
                "refusing: {} does not parse as JSON ({e}); fix or remove the file and retry",
                config_path.display()
            )
        })?,
        None => serde_json::json!({}),
    };

    if !config.is_object() {
        return Err(format!(
            "refusing: {} is not a JSON object",
            config_path.display()
        ));
    }
    let obj = config.as_object_mut().expect("checked object");
    let plugins = obj
        .entry("plugins")
        .or_insert_with(|| serde_json::json!({}));
    if !plugins.is_object() {
        return Err(format!(
            "refusing: plugins key in {} is not an object",
            config_path.display()
        ));
    }
    let plugins = plugins.as_object_mut().expect("checked object");
    plugins.insert("enabled".into(), serde_json::Value::Bool(true));
    let stage_str = stage.to_string_lossy().to_string();
    let dirs = plugins
        .entry("dirs")
        .or_insert_with(|| serde_json::Value::Array(Vec::new()));
    if !dirs.is_array() {
        return Err(format!(
            "refusing: plugins.dirs in {} is not an array",
            config_path.display()
        ));
    }
    let dirs = dirs.as_array_mut().expect("checked array");
    if !dirs.iter().any(|d| d.as_str() == Some(stage_str.as_str())) {
        dirs.push(serde_json::Value::String(stage_str.clone()));
    }
    let written = serde_json::to_string_pretty(&config)
        .map_err(|e| format!("cannot serialize {}: {e}", config_path.display()))?;
    std::fs::write(config_path, written)
        .map_err(|e| format!("cannot write {}: {e}", config_path.display()))?;
    // Read back: the receipt prints only when the file now lists the stage
    // dir exactly once (the desktop may rewrite the file while it runs).
    let back = std::fs::read_to_string(config_path)
        .map_err(|e| format!("cannot re-read {}: {e}", config_path.display()))?;
    let parsed: serde_json::Value = serde_json::from_str(&back).map_err(|e| {
        format!(
            "{} no longer parses after write: {e}",
            config_path.display()
        )
    })?;
    let listed = parsed
        .get("plugins")
        .and_then(|p| p.get("dirs"))
        .and_then(|d| d.as_array())
        .map(|dirs| {
            dirs.iter()
                .filter(|d| d.as_str() == Some(stage_str.as_str()))
                .count()
        })
        .unwrap_or(0);
    if listed != 1 {
        return Err(format!(
            "post-write read lists the stage dir {listed} times, want 1"
        ));
    }
    Ok(format!(
        "zcode plugins.dirs lists {} (plugins.enabled=true)",
        stage.display()
    ))
}

/// Written into the stage at build: the source HEAD it was built from and
/// when, so the check can say how far behind a stale stage is. HEAD never
/// tracks it, and the check ignores files HEAD lacks.
const STAGE_STAMP: &str = ".fno-stage.json";

fn install_harness(harness: &str, force: bool, source_dir: &Path) -> Result<String, String> {
    let root = repo_root(source_dir)?;
    let parent = state_root().join("plugin-stage");
    let (stage, _) = build_stage(&root, &parent)?;
    let detail = match harness {
        "claude" => install_claude(&stage, force)?,
        "agy" => install_agy(&stage, force)?,
        "codex" => crate::codex_converge::run_dev(&stage, force)?,
        "grok" => install_grok(&stage, force)?,
        "zcode" => install_zcode(&stage, force)?,
        other => {
            return Err(format!(
                "unknown harness '{other}'; want claude, codex, opencode, agy, grok or zcode"
            ))
        }
    };
    prime_plugin_root_pointer(&stage);
    Ok(format!("{detail}; local checkout {}", root.display()))
}

/// Persist the installed stage to `<state-root>/install/plugin-root`, the
/// pointer the session-start hook primes and every env-less reader (provider
/// command rosters, opencode install, the lead skill's fallback) resolves. The
/// install command must not depend on a session ever starting: on a fresh machine
/// the first `fno config plugin install` is exactly when no hook has run yet
/// (2026-10-02 gap audit 6). Best-effort: a failed write never fails the
/// install. The stage is a self-contained copy (no `.git`), so unlike a
/// worktree root it is safe as the machine-global value.
fn prime_plugin_root_pointer(stage: &Path) {
    if !stage.join(".claude-plugin").join("plugin.json").is_file() {
        return;
    }
    let anchor = match std::env::var_os("FNO_HOME") {
        Some(home) if !home.is_empty() => std::path::PathBuf::from(home),
        _ => state_root(),
    };
    let ptr = crate::state_layout::place(&anchor, "plugin-root");
    if let Some(parent) = ptr.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&ptr, format!("{}\n", stage.display()));
}

/// The env retraction as (line, is_error) pairs, so an arm that must keep
/// stdout to a JSON receipt can still run it with everything on stderr. A
/// machine with nothing to remove prints nothing.
fn env_retract_lines() -> Vec<(String, bool)> {
    let (changed, errors) = retract_env_exports();
    let mut lines: Vec<(String, bool)> = errors
        .into_iter()
        .map(|e| (format!("fno plugin install: {e}"), true))
        .collect();
    if !changed.is_empty() {
        let joined: Vec<String> = changed.iter().map(|p| p.display().to_string()).collect();
        lines.push((
            format!(
                "removed the machine-wide {BUILD_DIR_KEY}/{SCCACHE_DIR_KEY} exports an older install wrote: {}",
                joined.join(", ")
            ),
            false,
        ));
    }
    lines
}

fn env_retract_receipt() {
    for (line, is_error) in env_retract_lines() {
        if is_error {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

fn install_agy(stage: &Path, force: bool) -> Result<String, String> {
    let _ = force; // agy imports the stage fresh on every install
    run_checked(
        &[
            "agy".into(),
            "plugin".into(),
            "install".into(),
            stage.display().to_string(),
        ],
        None,
    )?;
    // The old receipt CLAIMED "hooks.json carries the stop hook" without
    // reading it. Report the real state of the global file against the
    // stage's own shipped adapters instead.
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let Some(home) = home else {
        return Ok("agy plugin imported; hooks.json status unknown (no HOME)".to_string());
    };
    let hooks = home.join(".gemini").join("config").join("hooks.json");
    let adapter = stage.join("hooks").join("footnote-agy-target-stop-hook.sh");
    let team_inject = stage.join("hooks").join("agy-team-inject.sh");
    let guard = stage.join("hooks").join("agy-lead-guard.sh");
    let s = crate::agy_hooks::status(
        &hooks,
        adapter.is_file().then_some(adapter.as_path()),
        team_inject.is_file().then_some(team_inject.as_path()),
        guard.is_file().then_some(guard.as_path()),
    );
    Ok(format!("agy plugin imported; {}", s.summary()))
}

/// The `plugin-install agy --hooks / --hooks-status` arm: an agy hooks.json
/// read or preserving-install that builds no plugin stage and needs no repo
/// checkout. Exit 0 on success; exit 1 on an install refusal (message on
/// stderr); exit 2 on a usage fault (missing adapter, no HOME, flags on a
/// non-agy harness).
fn run_agy_hooks(
    harness: Option<&str>,
    _hooks: bool,
    status_flag: bool,
    adapter: Option<&str>,
    role: Option<&str>,
    guard: Option<&str>,
    hooks_file: Option<&str>,
    json: bool,
) -> i32 {
    if harness != Some("agy") {
        eprintln!("plugin install: --hooks/--hooks-status apply to the agy arm; name it: plugin-install agy --hooks");
        return 2;
    }
    let Some(home) = std::env::var_os("HOME") else {
        eprintln!("plugin install agy --hooks: no HOME; cannot resolve the hooks file");
        return 2;
    };
    let hooks_path = match hooks_file {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(home)
            .join(".gemini")
            .join("config")
            .join("hooks.json"),
    };
    if status_flag {
        let adapter = adapter.map(PathBuf::from);
        let role = role.map(PathBuf::from);
        let guard = guard.map(PathBuf::from);
        let s = crate::agy_hooks::status(
            &hooks_path,
            adapter.as_deref(),
            role.as_deref(),
            guard.as_deref(),
        );
        if json {
            match serde_json::to_string(&s) {
                Ok(text) => println!("{text}"),
                Err(e) => {
                    eprintln!("plugin install agy --hooks-status: serialization error: {e}");
                    return 1;
                }
            }
        } else {
            println!("agy hooks {}: {}", hooks_path.display(), s.summary());
        }
        return 0;
    }
    // --hooks (install)
    let Some(adapter) = adapter else {
        eprintln!("plugin install agy --hooks: --adapter <path> is required");
        return 2;
    };
    let role = role.map(PathBuf::from);
    let guard = guard.map(PathBuf::from);
    match crate::agy_hooks::install(
        &hooks_path,
        Path::new(adapter),
        role.as_deref(),
        guard.as_deref(),
    ) {
        Ok(receipt) => {
            if json {
                match serde_json::to_string(&receipt) {
                    Ok(text) => println!("{text}"),
                    Err(e) => {
                        eprintln!("plugin install agy --hooks: serialization error: {e}");
                        return 1;
                    }
                }
            } else {
                println!("{}", receipt.note);
            }
            0
        }
        Err(reason) => {
            eprintln!("plugin install agy --hooks refused: {reason}");
            1
        }
    }
}

/// Read whether footnote's hooks reach grok on THIS machine: run
/// `grok inspect --json` (no auth needed) and look for an enabled fno plugin
/// with hooks. Trust is read from the plugin's location, because grok's
/// inspect exposes no trust field: a plugin under `<grok_home>/plugins` or
/// `<grok_home>/installed-plugins` is trusted, anything else (the
/// Claude-compat scan) is not. Reports `reachable` (naming the plugin path),
/// `untrusted` (naming the path with the fix command), `absent`, or
/// `unknown` (grok missing, inspect failed, or unparseable output).
fn grok_status_receipt() -> String {
    grok_receipt_for(grok_reachability())
}

/// The receipt line for an already-classified reading, so the status arm and
/// the loop probe classify once and print without a second `grok inspect` run.
fn grok_receipt_for(reachability: GrokReachability) -> String {
    match reachability {
        GrokReachability::Reachable { path } => format!("reachable: {path}"),
        GrokReachability::Untrusted { path } => format!(
            "untrusted: {path} (grok found fno only through the Claude-compat scan and runs no hooks from an untrusted plugin; run: fno config plugin install grok)"
        ),
        GrokReachability::Absent => "absent: no enabled fno plugin with hooks".to_string(),
        GrokReachability::Unknown { reason } => format!("unknown: {reason}"),
    }
}

/// Whether footnote's loop artifacts reach `harness` on THIS machine: the
/// machine fact behind the capability row. `None` names a harness with no
/// fno-installed loop gate (a native row whose loop closes through the
/// harness's own hook surface); `Some(Ok)` admits; `Some(Err(detail))`
/// carries the receipt line the dispatch refusal embeds.
pub(crate) fn loop_install_probe(
    harness: &str,
    extension_src: Option<&Path>,
) -> Option<Result<(), String>> {
    match harness {
        "opencode" => {
            let v = crate::opencode_install::installed_status();
            let status = v["status"].as_str().unwrap_or_default();
            Some(if status == "installed" {
                Ok(())
            } else {
                let field = |k: &str| v[k].as_str().unwrap_or("unknown").to_string();
                let why = if v["binary_missing"] == true {
                    "the opencode binary was not found; set FNO_OPENCODE_BIN or install it at ~/.opencode/bin/opencode"
                } else if status == "absent" {
                    "no footnote extension is installed"
                } else if status == "partial" {
                    "installed extension files are missing"
                } else if v["contract_changed"] == true {
                    "the opencode contract changed since install"
                } else {
                    "the installed extension differs from source"
                };
                Err(format!(
                    "opencode install status reads {status}: {why}; installed {} vs source {}",
                    field("version"),
                    field("source_version")
                ))
            })
        }
        "pi" => {
            let dest = crate::pi::pi_agent_dir()
                .join("extensions")
                .join("footnote.ts");
            let mut installed = dest.is_file();
            if installed {
                if let Some(src) = extension_src {
                    installed = files_byte_equal(Path::new(src), &dest).unwrap_or(false);
                }
            }
            Some(if installed {
                Ok(())
            } else {
                Err(format!(
                    "pi extension {} is absent or stale",
                    dest.display()
                ))
            })
        }
        "zcode" => {
            let config_path = match zcode_config_path() {
                Ok(path) => path,
                Err(reason) => return Some(Err(reason)),
            };
            let root = match repo_root(&default_source_dir()) {
                Ok(root) => root,
                Err(reason) => return Some(Err(reason)),
            };
            let stage = match build_stage(&root, &state_root().join("plugin-stage")) {
                Ok(stage) => stage.0,
                Err(reason) => return Some(Err(reason)),
            };
            let text = match std::fs::read_to_string(&config_path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    return Some(Err(format!(
                        "zcode config {} is absent; run fno config plugin install zcode",
                        config_path.display()
                    )))
                }
                Err(e) => return Some(Err(format!("cannot read {}: {e}", config_path.display()))),
            };
            let parsed: serde_json::Value = match serde_json::from_str(&text) {
                Ok(parsed) => parsed,
                Err(e) => {
                    return Some(Err(format!(
                        "{} does not parse: {e}",
                        config_path.display()
                    )))
                }
            };
            let listed = parsed
                .get("plugins")
                .and_then(|p| p.get("dirs"))
                .and_then(|d| d.as_array())
                .map(|dirs| {
                    dirs.iter()
                        .filter(|d| d.as_str() == Some(stage.to_string_lossy().as_ref()))
                        .count()
                })
                .unwrap_or(0);
            Some(if listed >= 1 {
                Ok(())
            } else {
                Err(format!(
                    "zcode config {} does not list the current stage dir; run fno config plugin install zcode",
                    config_path.display()
                ))
            })
        }
        "grok" => {
            let receipt = grok_receipt_for(grok_reachability());
            Some(if receipt.starts_with("reachable") {
                Ok(())
            } else {
                Err(receipt)
            })
        }
        _ => None,
    }
}

#[derive(Debug)]
enum GrokReachability {
    Reachable { path: String },
    Untrusted { path: String },
    Absent,
    Unknown { reason: String },
}

/// The 30-second-bounded `grok inspect --json` read, classified.
fn grok_reachability() -> GrokReachability {
    match which_grok() {
        None => GrokReachability::Unknown {
            reason: "grok not found on PATH".to_string(),
        },
        Some(_) => match run_grok_inspect() {
            Some(text) => parse_grok_inspect(&text, &crate::grok_store::grok_home()),
            None => GrokReachability::Unknown {
                reason: "grok inspect --json failed or timed out".to_string(),
            },
        },
    }
}

fn which_grok() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("grok"))
        .find(|candidate| candidate.is_file())
}

fn run_grok_inspect() -> Option<String> {
    let mut cmd = std::process::Command::new("grok");
    cmd.arg("inspect")
        .arg("--json")
        .env_remove("GROK_SESSION_ID");
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, 30).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8(out.stdout).ok()
}

/// Classify recorded `grok inspect --json` text. grok exposes no trust
/// field, so trust is read from the plugin's location: only a path under
/// `<grok_home>/plugins` or `<grok_home>/installed-plugins` loads hooks
/// (vendor guide 09-plugins.md). The linked stage is discovered under the
/// root manifest's name ("footnote"), so entries match by either name.
/// `Untrusted` when an enabled hooks-bearing fno entry sits outside those
/// roots (the Claude-compat scan); `Absent` when no such entry exists;
/// `Unknown` on unparseable JSON.
fn parse_grok_inspect(text: &str, grok_home: &Path) -> GrokReachability {
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return GrokReachability::Unknown {
            reason: "inspect output did not parse as JSON".to_string(),
        };
    };
    let Some(plugins) = v.get("plugins").and_then(Value::as_array) else {
        return GrokReachability::Unknown {
            reason: "inspect output has no plugins array".to_string(),
        };
    };
    let trusted_roots = [
        grok_home.join("plugins"),
        grok_home.join("installed-plugins"),
    ];
    let mut untrusted: Option<GrokReachability> = None;
    for plugin in plugins {
        let name = plugin
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let name_ok = name == "fno" || name == "footnote";
        let enabled = plugin.get("enabled").and_then(Value::as_bool) == Some(true);
        let hooks = plugin
            .get("provides")
            .and_then(|p| p.get("hooks"))
            .and_then(Value::as_bool)
            == Some(true);
        if !(name_ok && enabled && hooks) {
            continue;
        }
        let path = plugin
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let trusted = trusted_roots
            .iter()
            .any(|root| lexically_normalized(&path).starts_with(root));
        if trusted {
            return GrokReachability::Reachable { path };
        }
        if untrusted.is_none() {
            untrusted = Some(GrokReachability::Untrusted { path });
        }
    }
    untrusted.unwrap_or(GrokReachability::Absent)
}

/// The plugin entry name the install links under `<grok_home>/plugins`; the
/// name constant also keeps the link-name join out of the seam linter's
/// porcelain-path scan, which ratchets on the literal `join("fno")`.
const GROK_PLUGIN_LINK_NAME: &str = "fno";

/// Link the stage into `<grok_home>/plugins/fno`, then a second status
/// read: the `linked` receipt prints only when that read is reachable. Why
/// a link and not `grok plugin install --trust`: the plugins directory is
/// trusted automatically (vendor guide 09-plugins.md), and the link
/// follows every `fno doctor update` restage of the 186 MB stage where a
/// copied install would go stale. Measured 2026-09-26 (grok 1.0.34): the
/// linked copy reads under the root manifest's name ("footnote"), so the
/// compat copy is NOT deduped away, and grok still auto-disabled the
/// unlisted plugin; the stop-contract fixture carries the readings.
fn install_grok(stage: &Path, _force: bool) -> Result<String, String> {
    let home = crate::grok_store::grok_home();
    let read = grok_reachability();
    let read_is_copy = trusted_path_is_copy(&read, &home);
    match read {
        GrokReachability::Reachable { path } if !read_is_copy => {
            Ok(format!("already installed, reachable: {path}"))
        }
        GrokReachability::Unknown { reason } => Err(format!("unknown: {reason}")),
        // Absent, untrusted, or a reachable read through the pre-link
        // installed-plugins copy (stale on every restage): link, then
        // verify with a second read.
        _ => migrate_grok_to_link(&home, stage),
    }
}

/// Whether a reachable read names a copied install (anywhere but the
/// plugins-dir link) that the install must migrate to the link.
fn trusted_path_is_copy(read: &GrokReachability, home: &Path) -> bool {
    match read {
        GrokReachability::Reachable { path } => !Path::new(path).starts_with(home.join("plugins")),
        _ => false,
    }
}

fn migrate_grok_to_link(home: &Path, stage: &Path) -> Result<String, String> {
    let receipt = link_grok_plugin(home, stage)?;
    match grok_reachability() {
        GrokReachability::Reachable { path } => Ok(format!("{receipt}, reachable: {path}")),
        GrokReachability::Absent => {
            Err("linked but the post-link status read is absent".to_string())
        }
        GrokReachability::Untrusted { .. } => {
            Err("linked but the post-link status read is untrusted".to_string())
        }
        GrokReachability::Unknown { reason } => Err(format!(
            "linked but the post-link status read is unknown: {reason}"
        )),
    }
}

/// Link `<grok_home>/plugins/fno` to the stage. A real file or directory at
/// the link path is refused, never deleted. The swap builds the new link as
/// `plugins/.fno.new-<pid>` and renames it over the link path, so a reader
/// never sees a half-made link.
fn link_grok_plugin(grok_home: &Path, stage: &Path) -> Result<String, String> {
    let plugins = grok_home.join("plugins");
    std::fs::create_dir_all(&plugins)
        .map_err(|e| format!("cannot create {}: {e}", plugins.display()))?;
    let link = plugins.join(GROK_PLUGIN_LINK_NAME);
    let link_str = link.display().to_string();
    let stage_str = stage.display().to_string();
    match std::fs::read_link(&link) {
        Ok(old) if old == stage => Ok(format!("already linked {link_str} -> {stage_str}")),
        Ok(old) => {
            swap_grok_symlink(&plugins, &link, stage)?;
            Ok(format!(
                "relinked {link_str} -> {stage_str} from {}",
                old.display()
            ))
        }
        Err(_) if link.symlink_metadata().is_ok() => Err(format!(
            "refused: {link_str} is not a symlink; move it aside, then rerun fno config plugin install grok"
        )),
        Err(_) => {
            swap_grok_symlink(&plugins, &link, stage)?;
            Ok(format!("linked {link_str} -> {stage_str}"))
        }
    }
}

/// Resolve `.` and `..` lexically so a reported path cannot name a parent
/// directory while a raw-component prefix check still matches a trusted root.
fn lexically_normalized(path: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in Path::new(path).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn swap_grok_symlink(plugins: &Path, link: &Path, stage: &Path) -> Result<(), String> {
    let tmp = plugins.join(format!(".fno.new-{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    std::os::unix::fs::symlink(stage, &tmp)
        .map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, link).map_err(|e| {
        format!(
            "cannot rename {} over {}: {e}",
            tmp.display(),
            link.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Install removes the machine-wide exports older installs wrote, and
    // nothing else: a user's own keys, comments and tables survive.
    #[test]
    fn env_strippers_remove_only_fno_exports() {
        let fno = |_: &str, v: &str| v.contains(".fno");

        // rc: the legacy one-line block, the two-line block, and a block at
        // EOF with no trailing newline all go; the next export stays.
        let mark = "# fno: cargo build-dir\n";
        let cargo = "export CARGO_BUILD_BUILD_DIR=\"/u/.fno/b\"\n";
        let sccache = "export SCCACHE_DIR=\"/u/.fno/b/sccache\"\n";
        let keep = "alias ll=ls\nexport KEEP=1\n";
        for block in [[mark, cargo].concat(), [mark, cargo, sccache].concat()] {
            let rc = ["alias ll=ls\n", block.as_str(), "export KEEP=1\n"].concat();
            assert_eq!(strip_rc_block(&rc).as_deref(), Some(keep));
        }
        let bare = ["alias ll=ls\n", mark, cargo.trim_end()].concat();
        assert_eq!(strip_rc_block(&bare).as_deref(), Some("alias ll=ls\n"));
        assert_eq!(strip_rc_block(keep), None);

        // claude: fno's keys go, a user's own value for the same key stays,
        // and an env this pass empties goes too.
        let mut data: serde_json::Map<String, Value> = serde_json::from_str(
            r#"{"env": {"CARGO_BUILD_BUILD_DIR": "/u/.fno/b", "SCCACHE_DIR": "/mine", "KEEP": "1"}}"#,
        )
        .unwrap();
        assert_eq!(strip_claude_env(&mut data, fno), 1);
        assert_eq!(
            Value::Object(data),
            json!({"env": {"SCCACHE_DIR": "/mine", "KEEP": "1"}})
        );
        let mut only: serde_json::Map<String, Value> =
            serde_json::from_str(r#"{"env": {"SCCACHE_DIR": "/u/.fno/b/sccache"}, "x": 1}"#)
                .unwrap();
        assert_eq!(strip_claude_env(&mut only, fno), 1);
        assert_eq!(Value::Object(only), json!({"x": 1}));

        // codex: an emptied set table loses its header; a table that keeps a
        // key keeps it; keys outside the set table are never touched.
        let codex = "model = \"x\"\n# mine\n\n[shell_environment_policy.set]\nCARGO_BUILD_BUILD_DIR = \"/u/.fno/b/{workspace-path-hash}\"\nSCCACHE_DIR = \"/u/.fno/b/sccache\"\n\n[plugins.\"other@x\"]\nenabled = true\nSCCACHE_DIR = \"/u/.fno/b\"\n";
        assert_eq!(
            strip_codex_env(codex, fno).as_deref(),
            Some("model = \"x\"\n# mine\n\n\n[plugins.\"other@x\"]\nenabled = true\nSCCACHE_DIR = \"/u/.fno/b\"\n")
        );
        let shared =
            "[shell_environment_policy.set]\nFOO = \"1\"\nSCCACHE_DIR = \"/u/.fno/b/sccache\"\n";
        assert_eq!(
            strip_codex_env(shared, fno).as_deref(),
            Some("[shell_environment_policy.set]\nFOO = \"1\"\n")
        );
        assert_eq!(strip_codex_env("model = \"x\"\n", fno), None);
    }

    // zcode install: a malformed config is refused byte-identical.
    #[test]
    fn zcode_malformed_config_refused_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{not json").unwrap();
        let before = std::fs::read(&cfg).unwrap();
        let err = zcode_install_config(&cfg, Path::new("/stage")).unwrap_err();
        assert!(err.contains("does not parse"), "{err}");
        assert_eq!(std::fs::read(&cfg).unwrap(), before, "no bytes written");
    }

    /// One git command, panicking on failure - fixtures abort the test loudly.
    fn git_in(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(["-C", dir.to_str().unwrap()])
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git binary available");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repo with one commit carrying a hook config, one script, and the
    /// public marketplace manifest with its GitHub release pins.
    fn new_repo(dir: &Path) {
        fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        git_in(dir, &["init", "-b", "main", "-q"]);
        fs::create_dir_all(dir.join("hooks")).unwrap();
        fs::write(
            dir.join("hooks/hooks.json"),
            "{\"hooks\":[{\"command\":\"${CLAUDE_PLUGIN_ROOT}/hooks/live.sh\"}]}",
        )
        .unwrap();
        fs::write(dir.join("hooks/live.sh"), "live\n").unwrap();
        fs::write(dir.join(".claude-plugin/plugin.json"), r#"{"name":"fno"}"#).unwrap();
        fs::write(
            dir.join(MARKETPLACE_REL),
            r#"{"name":"fno","plugins":[{"name":"fno","source":{"source":"github","repo":"bllshttng/fno","ref":"stable"}},{"name":"fno-nightly","source":{"source":"github","repo":"bllshttng/fno","ref":"nightly"}}]}"#,
        )
        .unwrap();
        git_in(dir, &["add", "-A"]);
        git_in(dir, &["commit", "-q", "-m", "c1"]);
    }

    /// A stage built from `source`, with every tracked file present.
    fn fresh_stage(base: &Path) -> (PathBuf, PathBuf) {
        let source = base.join("source");
        new_repo(&source);
        let stage_parent = base.join("stage-parent");
        fs::create_dir_all(&stage_parent).unwrap();
        let (stage, _) = build_stage(&source, &stage_parent).unwrap();
        (source, stage)
    }

    /// AC3/AC4: a pack stage drops mux paths and mux hook entries, checks
    /// fresh against its own source, and a manifest naming a missing path
    /// refuses before any temp dir exists.
    #[test]
    fn pack_stage_drops_mux_paths_and_checks_fresh() {
        let base = std::env::temp_dir().join(format!("pi-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("source");
        new_repo(&source);
        fs::create_dir_all(source.join("skills/mail")).unwrap();
        fs::write(source.join("skills/mail/SKILL.md"), "mail\n").unwrap();
        fs::write(source.join("hooks/mail.sh"), "mail\n").unwrap();
        fs::write(
            source.join("hooks/hooks.json"),
            r#"{"hooks":{"Stop":[{"hooks":[{"command":"${R}/hooks/mail.sh"},{"command":"${R}/hooks/live.sh"}]}]}}"#,
        )
        .unwrap();
        let manifest = r#"{"mux":["hooks/mail.sh","skills/mail"]}"#;
        fs::write(source.join(crate::pack_profile::MANIFEST), manifest).unwrap();
        git_in(&source, &["add", "-A"]);
        git_in(&source, &["commit", "-q", "-m", "c2"]);
        let parent = base.join("stage-parent");
        fs::create_dir_all(&parent).unwrap();

        let (stage, _) = build_stage_as(&source, &parent, Profile::Pack).unwrap();
        assert!(stage.ends_with("fno-pack"));
        assert!(!stage.join("skills/mail").exists());
        assert!(!stage.join("hooks/mail.sh").exists());
        let hooks = fs::read_to_string(stage.join("hooks/hooks.json")).unwrap();
        assert!(
            !hooks.contains("mail.sh") && hooks.contains("live.sh"),
            "{hooks}"
        );
        assert_eq!(check_stage_report(&stage, &source).status, "fresh");

        fs::write(
            source.join(crate::pack_profile::MANIFEST),
            r#"{"mux":["skills/gone"]}"#,
        )
        .unwrap();
        let err = build_stage_as(&source, &parent, Profile::Pack).unwrap_err();
        assert!(err.contains("skills/gone"), "{err}");
        let temp = fs::read_dir(&parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with(".fno-pack."));
        assert!(!temp, "a refused pack build left a temp dir");
        let _ = fs::remove_dir_all(&base);
    }

    /// AC1-HP: the swap is atomic (no temp dirs left), new bytes land, and a
    /// handle opened before the rebuild still reads the old bytes.
    #[test]
    fn rebuild_swaps_atomically_and_keeps_open_handle_stable() {
        let base = std::env::temp_dir().join(format!("pi-ac1hp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (source, stage) = fresh_stage(&base);
        let live = stage.join("hooks/live.sh");
        let old_handle = fs::File::open(&live).unwrap();

        let built_from = run_checked(
            &["git".to_string(), "rev-parse".into(), "HEAD".into()],
            Some(&source),
        )
        .unwrap();
        fs::write(source.join("hooks/live.sh"), "updated\n").unwrap();
        git_in(&source, &["add", "-A"]);
        git_in(&source, &["commit", "-q", "-m", "c2"]);

        // Before the rebuild the stage trails by one commit in hooks/, and
        // the check says so from the stamp.
        let lag = check_stage_report(&stage, &source);
        assert_eq!(lag.status, "stale");
        assert_eq!(lag.stage_head.as_deref(), Some(built_from.as_str()));
        assert_eq!(lag.commits_behind, Some(1));
        let summary = lag.lag_summary();
        assert!(summary.starts_with("hooks 1; staged from "), "{summary}");
        assert!(summary.ends_with(", 1 commit(s) behind"), "{summary}");

        let stage_parent = stage.parent().unwrap().to_path_buf();
        let (_, copied) = build_stage(&source, &stage_parent).unwrap();
        assert!(copied >= 2);
        assert_eq!(fs::read_to_string(&live).unwrap(), "updated\n");
        let mut old_text = String::new();
        use std::io::Read;
        let mut old_handle2 = old_handle;
        old_handle2.read_to_string(&mut old_text).unwrap();
        assert_eq!(old_text, "live\n");
        let leftovers: Vec<_> = fs::read_dir(&stage_parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".fno.new-") || n.starts_with(".fno.old-"))
            .collect();
        assert!(leftovers.is_empty(), "temp dirs left behind: {leftovers:?}");
        let _ = fs::remove_dir_all(&base);
    }

    /// The staged manifest serves the fno entry from the stage itself while
    /// the source checkout keeps the public GitHub pins, and the rewrite is
    /// not reported as drift.
    #[test]
    fn stage_serves_fno_entry_locally_without_drift() {
        // The env mutations below (FNO_HOME, FNO_RECLAIM_STATE_ROOT) race
        // every guarded hermetic fixture unless this test holds the shared
        // lock; an unguarded FNO_HOME removal mid-test once erased a seeded
        // law and flipped a spared door.
        let _env = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("pi-mkt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (source, stage) = fresh_stage(&base);
        fs::create_dir_all(source.join(".agents/plugins")).unwrap();
        fs::write(source.join(CODEX_MARKETPLACE_REL), r#"{"plugins":[{"name":"fno","source":{"source":"url","url":"https://github.com/bllshttng/fno.git","ref":"stable"}}]}"#).unwrap();
        git_in(&source, &["add", CODEX_MARKETPLACE_REL]);
        git_in(&source, &["commit", "-q", "-m", "codex marketplace"]);
        let (stage, _) = build_stage(&source, stage.parent().unwrap()).unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(stage.join(CODEX_MARKETPLACE_REL)).unwrap())
                .unwrap();
        assert_eq!(
            codex["plugins"][0]["source"],
            json!({"source":"local","path":"."})
        );
        let staged: Value =
            serde_json::from_str(&fs::read_to_string(stage.join(MARKETPLACE_REL)).unwrap())
                .unwrap();
        let entries = staged["plugins"].as_array().unwrap();
        let fno = entries.iter().find(|p| p["name"] == "fno").unwrap();
        assert_eq!(fno["source"], json!("./"));
        let nightly = entries.iter().find(|p| p["name"] == "fno-nightly").unwrap();
        assert_eq!(nightly["source"]["ref"], json!("nightly"));
        let public: Value =
            serde_json::from_str(&fs::read_to_string(source.join(MARKETPLACE_REL)).unwrap())
                .unwrap();
        assert_eq!(public["plugins"][0]["source"]["ref"], json!("stable"));
        assert_eq!(check_stage_report(&stage, &source).status, "fresh");
        // The install command primes the plugin-root pointer from the built
        // stage and ignores a manifest-less one, so the same contract rides
        // this real stage: the pointer lands at <state>/install/plugin-root.
        let previous_root = std::env::var_os("FNO_RECLAIM_STATE_ROOT");
        let previous_home = std::env::var_os("FNO_HOME");
        // The fixture stage carries only the marketplace manifest, and the
        // prime ignores a stage without the plugin manifest - make it a real
        // plugin root first.
        fs::create_dir_all(stage.join(".claude-plugin")).unwrap();
        fs::write(stage.join(".claude-plugin").join("plugin.json"), b"{}").unwrap();
        std::env::remove_var("FNO_HOME");
        std::env::set_var("FNO_RECLAIM_STATE_ROOT", &base);
        prime_plugin_root_pointer(&stage);
        assert_eq!(
            fs::read_to_string(base.join("install").join("plugin-root"))
                .unwrap()
                .trim(),
            stage.to_str().unwrap(),
            "the primed pointer names the built stage"
        );
        let bare = base.join("bare");
        fs::create_dir_all(&bare).unwrap();
        prime_plugin_root_pointer(&bare);
        assert_eq!(
            fs::read_to_string(base.join("install").join("plugin-root"))
                .unwrap()
                .trim(),
            stage.to_str().unwrap(),
            "a manifest-less stage writes nothing"
        );
        match previous_root {
            Some(v) => std::env::set_var("FNO_RECLAIM_STATE_ROOT", v),
            None => std::env::remove_var("FNO_RECLAIM_STATE_ROOT"),
        }
        match previous_home {
            Some(v) => std::env::set_var("FNO_HOME", v),
            None => std::env::remove_var("FNO_HOME"),
        }
        let _ = fs::remove_dir_all(&base);
    }

    /// A manifest that cannot serve the fno entry (entry renamed away) fails
    /// the build loud instead of silently shipping the github pin back.
    #[test]
    fn build_refuses_manifest_without_fno_entry() {
        let base = std::env::temp_dir().join(format!("pi-mkt3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let source = base.join("source");
        new_repo(&source);
        fs::write(
            source.join(MARKETPLACE_REL),
            r#"{"name":"fno","plugins":[{"name":"fno-nightly","source":{"source":"github","repo":"bllshttng/fno","ref":"nightly"}}]}"#,
        )
        .unwrap();
        git_in(&source, &["add", "-A"]);
        git_in(&source, &["commit", "-q", "-m", "c2"]);
        let stage_parent = base.join("stage-parent");
        fs::create_dir_all(&stage_parent).unwrap();
        let err = build_stage(&source, &stage_parent).unwrap_err();
        assert!(err.contains("no fno entry"), "err: {err}");
        let _ = fs::remove_dir_all(&base);
    }

    /// A restage from a git repo that is not fno refuses and leaves the live
    /// stage byte-for-byte untouched (fno-books once replaced the plugin).
    #[test]
    fn restage_from_non_fno_repo_refuses_and_keeps_stage() {
        let base = std::env::temp_dir().join(format!("pi-notfno-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (_, stage) = fresh_stage(&base);
        let stage_parent = stage.parent().unwrap().to_path_buf();
        let before = fs::read(stage.join("hooks/live.sh")).unwrap();
        let books = base.join("books");
        fs::create_dir_all(&books).unwrap();
        git_in(&books, &["init", "-b", "main", "-q"]);
        fs::write(books.join("README.md"), "# Books\n").unwrap();
        git_in(&books, &["add", "-A"]);
        git_in(&books, &["commit", "-q", "-m", "c1"]);
        let err = restage_stage(&books, &stage_parent).unwrap_err();
        assert!(err.contains("not the fno plugin source"), "err: {err}");
        assert_eq!(fs::read(stage.join("hooks/live.sh")).unwrap(), before);
        assert!(!stage.join("README.md").exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// Regression: the --force arm once skipped `i += 1`, so the parse loop
    /// spun on the flag forever. The test returning at all is the proof.
    /// Also pins B5 at the same parser boundary: a mode flag after a harness
    /// positional is recorded, and the command refuses before any install work -
    /// `plugin-install claude --check` once dropped the flag and ran the full
    /// mutating install.
    #[test]
    fn parse_force_flag_returns_and_sets_flags() {
        let args: Vec<String> = ["--force", "--status"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let parsed = parse_plugin_install_args(&args);
        assert!(parsed.force);
        assert!(parsed.status);

        let args: Vec<String> = ["claude", "--check"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let parsed = parse_plugin_install_args(&args);
        assert_eq!(parsed.mode.as_deref(), Some("claude"));
        assert_eq!(parsed.dropped_mode.as_deref(), Some("--check"));
        assert_eq!(run_plugin_install(&args), 2);

        let args: Vec<String> = ["--json", "--release", "codex", "-F"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        let parsed = parse_plugin_install_args(&args);
        assert_eq!(parsed.mode.as_deref(), Some("codex"));
        assert!(parsed.release && parsed.json && parsed.force && !parsed.dev);
    }

    /// A GitHub-sourced claude marketplace or a codex release marker is a
    /// release channel the dev install must not replace; a directory source,
    /// a dev marker, or no record at all is not.
    #[test]
    fn release_channel_names_github_and_codex_release_only() {
        let tmp = tempfile::tempdir().unwrap();
        let claude = tmp.path().join("claude");
        let codex = tmp.path().join("codex");
        fs::create_dir_all(claude.join("plugins")).unwrap();
        fs::create_dir_all(codex.join("footnote")).unwrap();
        assert_eq!(release_channel("claude", &claude, &codex), None);
        assert_eq!(release_channel("codex", &claude, &codex), None);

        let known = claude.join("plugins/known_marketplaces.json");
        let claude_with = |source: Value| {
            fs::write(&known, json!({"fno": {"source": source}}).to_string()).unwrap();
            release_channel("claude", &claude, &codex)
        };
        assert_eq!(
            claude_with(json!({"source": "github", "repo": "bllshttng/fno", "ref": "stable"}))
                .as_deref(),
            Some("stable (github:bllshttng/fno#stable)")
        );
        assert_eq!(
            claude_with(
                json!({"source": "github", "repo": "bllshttng/fno", "ref": "plugin-nightly"})
            )
            .as_deref(),
            Some("nightly (github:bllshttng/fno#plugin-nightly)")
        );
        assert_eq!(
            claude_with(json!({"source": "github", "repo": "bllshttng/fno"})).as_deref(),
            Some("main (github:bllshttng/fno#main)")
        );
        assert_eq!(
            claude_with(json!({"source": "directory", "path": "/stage"})),
            None
        );

        let marker = codex.join("footnote/plugin-channel.json");
        let codex_with = |channel: &str| {
            let v = json!({"channel": channel, "source": "bllshttng/fno", "ref": "stable"});
            fs::write(&marker, v.to_string()).unwrap();
            release_channel("codex", &claude, &codex)
        };
        assert_eq!(
            codex_with("release").as_deref(),
            Some("stable (bllshttng/fno#stable)")
        );
        assert_eq!(codex_with("dev"), None);
        assert_eq!(release_channel("agy", &claude, &codex), None);
    }

    /// A throwaway HOME for the root-enumeration fixtures. The marketplace
    /// carries both a path-bearing source and an installLocation; the
    /// installLocation must win (Claude records the in-place load path there).
    fn fixture_home(base: &Path) -> PathBuf {
        let home = base.join("home");
        fs::create_dir_all(home.join(".claude/plugins")).unwrap();
        home
    }

    fn write_marketplace(home: &Path, shape: &str, install_location: &Path, source_path: &Path) {
        let v = json!({"fno": {"source": {"source": shape, "path": source_path.display().to_string()},
            "installLocation": install_location.display().to_string()}});
        fs::write(
            home.join(".claude/plugins/known_marketplaces.json"),
            serde_json::to_string(&v).unwrap(),
        )
        .unwrap();
    }

    fn write_registry(home: &Path, install_path: &Path) {
        let v = json!({"version": 2, "plugins": {"fno@fno": [
            {"scope": "user", "installPath": install_path.display().to_string()}]}});
        fs::write(
            home.join(".claude/plugins/installed_plugins.json"),
            serde_json::to_string(&v).unwrap(),
        )
        .unwrap();
    }

    /// An install made under the old marketplace name keeps reading as fno,
    /// and a dev install retires the old registration before it adds fno@fno.
    #[test]
    fn legacy_footnote_marketplace_is_read_and_migrated() {
        let tmp = tempfile::tempdir().unwrap();
        let home = fixture_home(tmp.path());
        let claude = home.join(".claude");
        assert!(legacy_claude_migration(&claude).is_empty());

        let known = json!({"footnote": {"source": {"source": "github", "repo": "bllshttng/fno", "ref": "stable"}}});
        fs::write(
            claude.join("plugins/known_marketplaces.json"),
            known.to_string(),
        )
        .unwrap();
        let registry = json!({"version": 2, "plugins": {"fno@footnote": [
            {"scope": "user", "installPath": "/c/cache/footnote/fno/0.4.3"}]}});
        fs::write(
            claude.join("plugins/installed_plugins.json"),
            registry.to_string(),
        )
        .unwrap();

        assert_eq!(marketplace_source_kind(&home).as_deref(), Some("github"));
        assert_eq!(
            registry_install_paths(&home),
            vec![PathBuf::from("/c/cache/footnote/fno/0.4.3")]
        );
        assert_eq!(
            release_channel("claude", &claude, &home.join(".codex")).as_deref(),
            Some("stable (github:bllshttng/fno#stable)")
        );
        // A failed migration leaves both ids registered; the guard sees both.
        let both = json!({"version": 2, "plugins": {
            "fno@fno": [{"scope": "user", "installPath": "/c/cache/fno/fno/0.5.0"}],
            "fno@footnote": [{"scope": "project", "installPath": "/c/cache/footnote/fno/0.4.3"}]}});
        fs::write(
            claude.join("plugins/installed_plugins.json"),
            both.to_string(),
        )
        .unwrap();
        assert_eq!(
            registry_install_paths(&home),
            vec![
                PathBuf::from("/c/cache/fno/fno/0.5.0"),
                PathBuf::from("/c/cache/footnote/fno/0.4.3")
            ]
        );
        assert_eq!(
            legacy_claude_migration(&claude),
            vec![
                vec!["claude", "plugin", "uninstall", "fno@footnote"],
                vec!["claude", "plugin", "marketplace", "remove", "footnote"],
            ]
        );

        // Once the new name is registered, nothing is left to migrate.
        let known = json!({"fno": {"source": {"source": "github", "repo": "bllshttng/fno", "ref": "stable"}}});
        fs::write(
            claude.join("plugins/known_marketplaces.json"),
            known.to_string(),
        )
        .unwrap();
        assert!(legacy_claude_migration(&claude).is_empty());
    }

    /// AC2-HP: trust reads from the location, not from the name alone.
    #[test]
    fn grok_parse_trust_comes_from_location() {
        let gh = Path::new("/gh");
        let linked = serde_json::json!({
            "name": "footnote", "scope": "user", "enabled": true,
            "path": "/gh/plugins/fno",
            "provides": {"skills": 25, "agents": 1, "hooks": true, "mcpServers": 0}
        });
        let text = serde_json::to_string(&serde_json::json!({ "plugins": [linked] })).unwrap();
        match parse_grok_inspect(&text, gh) {
            GrokReachability::Reachable { path } => assert_eq!(path, "/gh/plugins/fno"),
            other => panic!("want Reachable, got {other:?}"),
        }
        // grok's own installer path is trusted too.
        let installed = serde_json::json!({
            "name": "fno", "scope": "user", "enabled": true,
            "path": "/gh/installed-plugins/fno-1a2b",
            "provides": {"hooks": true}
        });
        let text = serde_json::to_string(&serde_json::json!({ "plugins": [installed] })).unwrap();
        assert!(matches!(
            parse_grok_inspect(&text, gh),
            GrokReachability::Reachable { .. }
        ));
        // Both a compat copy and a linked copy: the trusted one wins.
        let both = {
            let compat = serde_json::json!({
                "name": "fno", "enabled": true,
                "path": "/claude/plugins/cache/footnote/fno/0.3.2",
                "provides": {"hooks": true}
            });
            let linked_copy = serde_json::json!({
                "name": "footnote", "enabled": true,
                "path": "/gh/plugins/fno",
                "provides": {"hooks": true}
            });
            serde_json::json!({ "plugins": [compat, linked_copy] })
        };
        let text = serde_json::to_string(&both).unwrap();
        match parse_grok_inspect(&text, gh) {
            GrokReachability::Reachable { path } => assert_eq!(path, "/gh/plugins/fno"),
            other => panic!("want the trusted entry, got {other:?}"),
        }
        // A .. inside the reported path resolves outside the trusted root,
        // so the raw prefix must not match.
        let escape = serde_json::json!({
            "name": "fno", "enabled": true,
            "path": "/gh/plugins/../evil/fno",
            "provides": {"hooks": true}
        });
        let text = serde_json::to_string(&serde_json::json!({ "plugins": [escape] })).unwrap();
        assert!(matches!(
            parse_grok_inspect(&text, gh),
            GrokReachability::Untrusted { .. }
        ));
        // A .. that stays inside the trusted root still reads trusted.
        let inner = serde_json::json!({
            "name": "fno", "enabled": true,
            "path": "/gh/plugins/sub/../fno",
            "provides": {"hooks": true}
        });
        let text = serde_json::to_string(&serde_json::json!({ "plugins": [inner] })).unwrap();
        assert!(matches!(
            parse_grok_inspect(&text, gh),
            GrokReachability::Reachable { .. }
        ));
    }

    /// AC4-HP/AC5-EDGE/AC6-ERR: the link installs, relinks, and refuses a
    /// real directory without deleting it.
    #[test]
    fn grok_link_installs_relinks_and_refuses_a_real_dir() {
        let base = std::env::temp_dir().join(format!("grok-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let gh = base.join("grok-home");
        let stage = base.join("stage");
        let other = base.join("other");
        fs::create_dir_all(&stage).unwrap();
        fs::create_dir_all(&other).unwrap();

        let receipt = link_grok_plugin(&gh, &stage).unwrap();
        assert!(receipt.starts_with("linked "), "{receipt}");
        assert_eq!(fs::read_link(gh.join("plugins/fno")).unwrap(), stage);

        let receipt = link_grok_plugin(&gh, &stage).unwrap();
        assert!(receipt.starts_with("already linked "), "{receipt}");

        let receipt = link_grok_plugin(&gh, &other).unwrap();
        assert!(receipt.starts_with("relinked "), "{receipt}");
        assert!(receipt.contains(" from "), "{receipt}");
        assert!(receipt.contains(&stage.display().to_string()), "{receipt}");
        assert_eq!(fs::read_link(gh.join("plugins/fno")).unwrap(), other);

        // A real directory at the link path is refused, contents intact.
        let real = gh.join("plugins/fno");
        fs::remove_file(&real).unwrap();
        fs::create_dir(&real).unwrap();
        fs::write(real.join("keep.txt"), "keep").unwrap();
        let err = link_grok_plugin(&gh, &stage).unwrap_err();
        assert!(err.starts_with("refused: "), "{err}");
        assert!(
            real.join("keep.txt").exists(),
            "refused dir must be untouched"
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// AC1-HP: with no --stage, the check runs once per enumerated root. A
    /// byte-identical marketplace stage reads live and fresh; a differing
    /// registry installPath keeps its per-root stale byte verdict, but beside
    /// a LIVE root it is a retired cache (B7): it never drives the worst
    /// status, so a correct directory-source install exits 0.
    #[test]
    fn check_without_stage_reports_every_root_and_worst_exit() {
        let base = std::env::temp_dir().join(format!("pi-roots-ac1-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (source, stage) = fresh_stage(&base);
        let home = fixture_home(&base);
        write_marketplace(&home, "directory", &stage, &base.join("elsewhere"));
        // A registry installPath whose bytes differ from source HEAD in
        // exactly one file.
        let cache = base.join("registry-tree");
        fs::create_dir_all(cache.join("hooks")).unwrap();
        fs::write(
            cache.join("hooks/hooks.json"),
            "{\"hooks\":[{\"command\":\"${CLAUDE_PLUGIN_ROOT}/hooks/live.sh\"}]}",
        )
        .unwrap();
        fs::write(cache.join("hooks/live.sh"), "tampered\n").unwrap();
        write_registry(&home, &cache);

        let (roots, detail) = plugin_roots_for(&home);
        assert_eq!(roots.len(), 2, "roots: {roots:?} detail: {detail:?}");
        let (report, exit) = check_roots_report(&roots, detail, &source);
        // B7: the registered cache nobody loads must not fail a correct
        // directory-source install.
        assert_eq!(exit, 0);
        assert_eq!(report.status, "fresh");
        assert_eq!(report.roots.len(), 2);
        let live = &report.roots[0];
        assert!(live.live, "the marketplace root must read live");
        assert_eq!(live.check.status, "fresh");
        assert_eq!(live.path, stage.display().to_string());
        let second = &report.roots[1];
        assert!(!second.live);
        // The byte truth survives per root, but nothing blocks on it.
        assert_eq!(second.check.status, "stale");
        assert!(second.blocker.is_none(), "{:?}", second.blocker);
        assert!(second.note.contains("retired cache"), "{}", second.note);
        // The registry copy also lacks the manifests HEAD tracks.
        assert_eq!(
            second.check.sample,
            vec![
                ".claude-plugin/marketplace.json".to_string(),
                ".claude-plugin/plugin.json".to_string(),
                "hooks/live.sh".to_string()
            ]
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// AC2.1-HP + AC2.1-ERR: the Claude cache copy is removed only under the
    /// guard. A local marketplace with a proven live root removes it and
    /// receipts the path; a non-local marketplace and a cache that IS the
    /// live root both keep it, naming the condition that refused.
    #[test]
    fn claude_cache_removal_is_guarded() {
        let base = std::env::temp_dir().join(format!("pi-rm-ac3-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = fixture_home(&base);
        let stage = base.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let cache = home.join(".claude/plugins/cache/footnote");
        fs::create_dir_all(&cache).unwrap();
        let live_stage = vec![PluginRoot {
            path: stage.clone(),
            live: true,
            origin: "marketplace",
        }];

        // HP: directory marketplace + live stage + existing cache -> removed,
        // and the receipt names the live root so a removed copy does not
        // read as a failure.
        write_marketplace(&home, "directory", &stage, &stage);
        let (removed, refused) = remove_stale_copies(&home, &live_stage);
        assert_eq!(removed, vec![cache.clone()], "removed: {removed:?}");
        assert!(
            refused.iter().any(|l| l.contains("in place from")
                && l.contains(stage.display().to_string().as_str())
                && l.contains("unused cache copy")),
            "{refused:?}"
        );
        assert!(!cache.exists());
        fs::create_dir_all(&cache).unwrap();

        // ERR: a non-local marketplace refuses, naming the shape.
        write_marketplace(&home, "github", &stage, &stage);
        let (removed, refused) = remove_stale_copies(&home, &live_stage);
        assert!(removed.is_empty());
        assert!(
            refused.iter().any(|l| l.contains("'github'")),
            "{refused:?}"
        );
        assert!(cache.exists());
        fs::create_dir_all(&cache).unwrap();

        // ERR: the cache itself is the live root -> kept, naming why.
        write_marketplace(&home, "directory", &cache, &cache);
        let live_cache = vec![PluginRoot {
            path: cache.clone(),
            live: true,
            origin: "marketplace",
        }];
        let (removed, refused) = remove_stale_copies(&home, &live_cache);
        assert!(removed.is_empty());
        assert!(
            refused.iter().any(|l| l.contains("live root")),
            "{refused:?}"
        );
        assert!(cache.exists());
        let _ = fs::remove_dir_all(&base);
    }

    /// The sweep must never delete a tree installed_plugins.json registers
    /// inside: install registers cache/footnote/fno/<ver> and the same run's
    /// sweep once deleted cache/footnote, leaving the registry pointing at a
    /// missing tree and the review preflight's plugin-file check reading
    /// unknown. The registry's word outranks the copy's staleness.
    #[test]
    fn sweep_keeps_the_cache_the_registry_registers_inside() {
        let base = std::env::temp_dir().join(format!("pi-rm-registry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let home = fixture_home(&base);
        let stage = base.join("stage");
        fs::create_dir_all(&stage).unwrap();
        let cache = home.join(".claude/plugins/cache/footnote");
        let install_path = cache.join("fno/0.4.0");
        fs::create_dir_all(&install_path).unwrap();
        write_registry(&home, &install_path);
        write_marketplace(&home, "directory", &stage, &stage);
        let live_stage = vec![PluginRoot {
            path: stage.clone(),
            live: true,
            origin: "marketplace",
        }];

        let (removed, refused) = remove_stale_copies(&home, &live_stage);
        assert!(removed.is_empty(), "removed: {removed:?}");
        assert!(
            refused.iter().any(|l| l.contains("installed_plugins.json")),
            "{refused:?}"
        );
        assert!(
            install_path.exists(),
            "the registered installPath must survive the sweep"
        );
        let _ = fs::remove_dir_all(&base);
    }
}
