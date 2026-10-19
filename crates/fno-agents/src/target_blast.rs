//! Deterministic blast-radius classifier for the `target-init` door.
//!
//! Rust port of `cli/src/fno/target/blast.py` plus the size-modulation
//! decision that used to live in the Python init body: a plan's
//! File Ownership Map is classified high / low / unknown against the blast
//! map, and the verdict floors the size profile at `M` (high) or downgrades
//! an unpinned size to `S` (low). Every failure degrades to "unknown", so a
//! classifier hiccup can never block init.
//!
//! The blast map has two parts, each with its own glob dialect:
//! - in-footnote-repo control plane: the include entries of the packaged
//!   `control_plane_scope.yaml` (trailing `/` directory prefix, trailing `*`
//!   path-prefix glob, otherwise exact);
//! - general (any repo): the locked default list plus the per-project
//!   `high_blast_globs` extension, matched as recursive `**` globs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) const HIGH: &str = "high";
pub(crate) const LOW: &str = "low";
pub(crate) const UNKNOWN: &str = "unknown";

/// General high-blast default list. The four categories are locked (auth /
/// migrations+sql / infra+secrets / billing); patterns are precision-tunable.
const GENERAL_HIGH_BLAST_GLOBS: [&str; 13] = [
    "**/auth/**",
    "**/*auth*",
    "**/migrations/**",
    "**/*.sql",
    "supabase/migrations/**",
    "prisma/migrations/**",
    "**/.env*",
    "**/secrets/**",
    "**/Dockerfile",
    "**/*.tf",
    "terraform/**",
    "**/billing/**",
    "**/payment*/**",
];

const CONTROL_PLANE_SCOPE_REL: &str = "cli/src/fno/target/control_plane_scope.yaml";

/// `config.target.blast`, fail-safe to the disabled default on any load or
/// parse problem: a config typo can never turn the blast read on.
#[derive(Debug)]
pub(crate) struct BlastCfg {
    pub(crate) enabled: bool,
    pub(crate) downgrade: bool,
    pub(crate) reuse_loc_manifest: bool,
    pub(crate) high_blast_globs: Vec<String>,
}

impl Default for BlastCfg {
    fn default() -> Self {
        BlastCfg {
            enabled: false,
            downgrade: true,
            reuse_loc_manifest: true,
            high_blast_globs: Vec::new(),
        }
    }
}

/// Translate an rsync-style glob (with `**` recursive) to a regex string.
/// `**/` becomes `(?:.*/)?` so it matches zero or more leading directories
/// (a plain `.*` translation leaves a mandatory `/` and misses root-level
/// files). A standalone `**` is `.*` (cross-segment), `*` is `[^/]*`
/// (single segment), `?` is one non-slash char. Everything else is escaped.
fn glob_to_regex(pat: &str) -> String {
    let mut out = String::new();
    let mut it = pat.chars().peekable();
    while let Some(c) = it.next() {
        if c == '*' && it.peek() == Some(&'*') {
            it.next();
            if it.peek() == Some(&'/') {
                it.next();
                out.push_str("(?:.*/)?");
            } else {
                out.push_str(".*");
            }
        } else if c == '*' {
            out.push_str("[^/]*");
        } else if c == '?' {
            out.push_str("[^/]");
        } else {
            out.push_str(&regex::escape(&c.to_string()));
        }
    }
    out
}

/// True when `file_path` matches the recursive glob `pat`. A pattern that
/// cannot compile (a malformed glob) is a non-match, never a raise, so one
/// bad entry in `high_blast_globs` cannot abort a classification.
fn match_recursive(file_path: &str, pat: &str) -> bool {
    match regex::Regex::new(&format!("^(?:{})$", glob_to_regex(pat))) {
        Ok(re) => re.is_match(file_path),
        Err(_) => false,
    }
}

/// Match a loc-ratchet manifest include entry (prefix/star/exact semantics).
/// Trailing `/` is a directory prefix, trailing `*` a path-prefix glob,
/// otherwise an exact path match.
fn match_manifest_entry(file_path: &str, entry: &str) -> bool {
    let entry = entry.trim();
    if entry.is_empty() {
        return false;
    }
    if let Some(dir) = entry.strip_suffix('/') {
        return file_path == dir || file_path.starts_with(entry);
    }
    if let Some(prefix) = entry.strip_suffix('*') {
        return file_path.starts_with(prefix);
    }
    file_path == entry
}

/// `os.path.normpath` as a textual walk (no filesystem), then forward slashes
/// and no leading slash, so an absolute-looking `/hooks/...` can still match
/// a repo-relative manifest entry like `hooks/`.
fn logical_normpath(s: &str) -> String {
    let absolute = s.starts_with('/');
    let mut out: Vec<&str> = Vec::new();
    for seg in s.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().map_or(false, |last| *last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// `Path.resolve()` for a path that may not exist yet (a planned file):
/// canonicalize the deepest existing ancestor and re-join the remainder.
fn resolve_lenient(p: &Path) -> Option<PathBuf> {
    if let Ok(resolved) = std::fs::canonicalize(p) {
        return Some(resolved);
    }
    let mut ancestor = p.to_path_buf();
    let mut tail: Vec<PathBuf> = Vec::new();
    while let Some(name) = ancestor.file_name().map(PathBuf::from) {
        let parent = ancestor.parent()?.to_path_buf();
        tail.push(name);
        ancestor = parent;
        if let Ok(resolved) = std::fs::canonicalize(&ancestor) {
            let mut out = resolved;
            for seg in tail.iter().rev() {
                out.push(seg);
            }
            return Some(out);
        }
    }
    None
}

/// Normalize one ownership-map path to a repo-relative POSIX string.
/// With a repo root, an absolute or repo-anchored path resolves (symlinks
/// best-effort) and relativizes, so a path that physically lands inside a
/// blast glob is not missed; on any failure it falls back to the logical
/// normalization of the original input.
pub(crate) fn normalize_path(raw: &str, repo_root: Option<&Path>) -> String {
    let trimmed = raw.trim();
    let expanded = match trimmed.strip_prefix("~/") {
        Some(rest) => match std::env::var("HOME") {
            Ok(home) => format!("{home}/{rest}"),
            Err(_) => trimmed.to_string(),
        },
        None => trimmed.to_string(),
    };
    if expanded.is_empty() {
        return String::new();
    }
    if let Some(root) = repo_root {
        let p = PathBuf::from(&expanded);
        let anchored = if p.is_absolute() {
            p
        } else {
            PathBuf::from(root).join(&p)
        };
        if let (Some(resolved), Some(root_resolved)) =
            (resolve_lenient(&anchored), resolve_lenient(Path::new(root)))
        {
            if let Ok(rel) = resolved.strip_prefix(&root_resolved) {
                return rel.to_string_lossy().replace('\\', "/");
            }
        }
    }
    logical_normpath(&expanded)
}

/// Read the `include:` entries from a loc-ratchet manifest, fail-safe to an
/// empty list. A tiny line-oriented parse of the YAML subset: a top-level
/// `key:` opens a section, `- value` lines inside `include:` are entries, and
/// only a matched surrounding quote pair is stripped.
fn manifest_globs_from_text(text: &str) -> Vec<String> {
    let mut globs = Vec::new();
    let mut in_include = false;
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') && stripped.ends_with(':') {
            in_include = stripped == "include:";
            continue;
        }
        if in_include {
            if let Some(val) = stripped.strip_prefix("- ") {
                let val = val.trim();
                let quoted = val.chars().next().zip(val.chars().last());
                let val = match quoted {
                    Some((q, _))
                        if val.len() >= 2 && (q == '\'' || q == '"') && val.ends_with(q) =>
                    {
                        val[1..val.len() - 1].to_string()
                    }
                    _ => val.to_string(),
                };
                globs.push(val);
            }
        }
    }
    globs
}

/// The packaged control-plane scope beside the plugin checkout, located with
/// the same discovery chain as the init helper script. `None` (no manifest
/// globs) when it cannot be found: the general list still carries the weight.
pub(crate) fn control_plane_scope_path() -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["CLAUDE_PLUGIN_ROOT", "FNO_REPO_ROOT"] {
        if let Ok(root) = std::env::var(key) {
            roots.push(PathBuf::from(root));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let pointer = PathBuf::from(&home).join(".fno/install/plugin-root");
        if let Ok(root) = std::fs::read_to_string(&pointer) {
            roots.push(PathBuf::from(root.trim()));
        }
    }
    if let Ok(out) = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
        .output()
    {
        if out.status.success() {
            let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !root.is_empty() {
                roots.push(PathBuf::from(root));
            }
        }
    }
    roots
        .into_iter()
        .map(|root| root.join(CONTROL_PLANE_SCOPE_REL))
        .find(|candidate| candidate.is_file())
}

/// Load `config.target.blast` from the repo's `.fno/config.toml`. A missing
/// file, a malformed block, or one field of the wrong type all degrade to the
/// disabled default, mirroring the Python loader's fail-safe.
pub(crate) fn load_blast_cfg(project_root: &Path) -> BlastCfg {
    let mut cfg = BlastCfg::default();
    let path = project_root.join(".fno").join("config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return cfg;
    };
    let Ok(root) = text.parse::<toml::Value>() else {
        return cfg;
    };
    let Some(block) = root.get("target").and_then(|t| t.get("blast")) else {
        return cfg;
    };
    let read_bool = |key: &str, default: bool| -> Option<bool> {
        match block.get(key) {
            None => Some(default),
            Some(v) => v.as_bool(),
        }
    };
    let enabled = read_bool("enabled", false);
    let downgrade = read_bool("downgrade", true);
    let reuse = read_bool("reuse_loc_manifest", true);
    let globs = match block.get("high_blast_globs") {
        None => Some(Vec::new()),
        Some(v) => v.as_array().map(|a| {
            a.iter()
                .filter_map(|g| g.as_str().map(String::from))
                .collect()
        }),
    };
    match (enabled, downgrade, reuse, globs) {
        (Some(enabled), Some(downgrade), Some(reuse), Some(globs)) => {
            cfg.enabled = enabled;
            cfg.downgrade = downgrade;
            cfg.reuse_loc_manifest = reuse;
            cfg.high_blast_globs = globs;
        }
        _ => cfg = BlastCfg::default(),
    }
    cfg
}

/// The file whose `## File Ownership Map` to parse: a folder plan keeps its
/// map in `00-INDEX.md`; a single-file plan is the file itself.
pub(crate) fn resolve_plan_index(plan_path: &str) -> PathBuf {
    let p = Path::new(plan_path);
    if p.is_dir() {
        p.join("00-INDEX.md")
    } else {
        p.to_path_buf()
    }
}

/// A first-column cell is a path candidate (not a prose description). `@` is
/// allowed so npm scoped-package paths are not dropped.
fn looks_like_path(cell: &str, path_like: &regex::Regex) -> bool {
    if cell.is_empty() || !path_like.is_match(cell) {
        return false;
    }
    cell.contains('/') || cell.contains('.')
}

/// Extract path candidates from a plan's `## File Ownership Map` table: the
/// first table column under the heading, backticks stripped, only path-like
/// cells kept. Empty when the section is absent or holds no path-like rows,
/// which classifies as `unknown`.
pub(crate) fn parse_ownership_map(plan_text: &str) -> Vec<String> {
    let path_like = regex::Regex::new(r"^[0-9A-Za-z_./+@*-]+$").unwrap();
    let mut out = Vec::new();
    let mut in_section = false;
    for line in plan_text.lines() {
        let stripped = line.trim();
        if stripped.starts_with('#') {
            // Any heading toggles the section: enter on the ownership map,
            // leave on the next heading.
            let heading = stripped.trim_start_matches('#').trim().to_lowercase();
            in_section = heading.starts_with("file ownership map");
            continue;
        }
        if !in_section || !stripped.starts_with('|') {
            continue;
        }
        let first_raw = stripped.trim_matches('|').split('|').next().unwrap_or("");
        let first = first_raw.trim().trim_matches('`').trim();
        let low = first.to_lowercase();
        if low == "file" || low == "files" || first.is_empty() {
            continue;
        }
        if first.chars().all(|c| c == '-' || c == ':' || c == ' ') {
            continue;
        }
        if looks_like_path(first, &path_like) {
            out.push(first.to_string());
        }
    }
    out
}

/// Classify a touched-surface path list. Any normalized path matching a blast
/// glob is `high`; all paths known and none matching is `low`; no usable
/// paths is `unknown`. Returns the verdict, the matched original paths, and
/// the reason string the blast-check JSON carries.
pub(crate) fn classify(
    paths: &[String],
    cfg: &BlastCfg,
    repo_root: Option<&Path>,
    manifest_path: Option<&Path>,
) -> (String, Vec<String>, String) {
    let pairs: Vec<(&String, String)> = paths
        .iter()
        .filter(|p| !p.trim().is_empty())
        .map(|p| (p, normalize_path(p, repo_root)))
        .filter(|(_, norm)| !norm.is_empty())
        .collect();
    if pairs.is_empty() {
        return (
            UNKNOWN.to_string(),
            Vec::new(),
            "no usable paths in the File Ownership Map".to_string(),
        );
    }
    let manifest_entries = if cfg.reuse_loc_manifest {
        match manifest_path {
            Some(path) => std::fs::read_to_string(path)
                .map(|text| manifest_globs_from_text(&text))
                .unwrap_or_default(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let mut recursive: Vec<&str> = GENERAL_HIGH_BLAST_GLOBS.to_vec();
    recursive.extend(cfg.high_blast_globs.iter().map(|g| g.as_str()));
    let mut matched = Vec::new();
    let mut reasons = Vec::new();
    for (orig, norm) in &pairs {
        let hit = manifest_entries
            .iter()
            .find(|e| match_manifest_entry(norm, e))
            .map(|e| e.as_str())
            .or_else(|| recursive.iter().find(|g| match_recursive(norm, g)).copied());
        if let Some(hit) = hit {
            matched.push((*orig).clone());
            reasons.push(format!("{orig} ~ {hit}"));
        }
    }
    if matched.is_empty() {
        (
            LOW.to_string(),
            Vec::new(),
            format!("all {} path(s) outside the blast map", pairs.len()),
        )
    } else {
        (
            HIGH.to_string(),
            matched,
            format!("high-blast surface: {}", reasons.join("; ")),
        )
    }
}

/// Read the resolved plan and classify its ownership map. Any read problem
/// degrades to `unknown`, the fail-safe verdict.
pub(crate) fn classify_plan_file(
    plan_path: &str,
    cfg: &BlastCfg,
    repo_root: Option<&Path>,
    manifest_path: Option<&Path>,
) -> (String, Vec<String>, String) {
    let index = resolve_plan_index(plan_path);
    match std::fs::read_to_string(&index) {
        Ok(text) => classify(&parse_ownership_map(&text), cfg, repo_root, manifest_path),
        Err(_) => (UNKNOWN.to_string(), Vec::new(), String::new()),
    }
}

/// The `fno do target blast-check` door: classify one plan and print the
/// verdict. Always exits 0 on a classification, even an `unknown` from an
/// unreadable plan: the caller treats a non-high/low verdict as "leave
/// ceremony unchanged", so this door can never block a target init.
pub(crate) fn run_blast_check(rest: &[String]) -> i32 {
    let mut quiet = false;
    let mut plans: Vec<&String> = Vec::new();
    for arg in rest {
        if arg == "--quiet" {
            quiet = true;
        } else {
            plans.push(arg);
        }
    }
    let Some(plan) = plans.first() else {
        eprintln!("fno do target blast-check: needs the plan path to classify");
        return 2;
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let repo_root = crate::target_init::git_toplevel(&cwd);
    let cfg = load_blast_cfg(repo_root.as_deref().unwrap_or(&cwd));
    let index = resolve_plan_index(plan);
    let (verdict, matched, reason) = match std::fs::read_to_string(&index) {
        Ok(text) => classify(
            &parse_ownership_map(&text),
            &cfg,
            repo_root.as_deref(),
            control_plane_scope_path().as_deref(),
        ),
        Err(err) => (
            UNKNOWN.to_string(),
            Vec::new(),
            format!("cannot read plan '{plan}': {err}"),
        ),
    };
    if quiet {
        println!("{verdict}");
    } else {
        let payload = serde_json::json!({
            "verdict": verdict,
            "matched_paths": matched,
            "reason": reason,
        });
        println!("{payload}");
    }
    0
}

/// The plan the blast read should classify, or `None` to skip. An explicit
/// `--plan-path` wins; otherwise each `--input` token is matched (exact,
/// case-insensitive) against a graph entry id, so modifier-prefixed node
/// inputs are covered while free text resolves nothing. Exactly one distinct
/// match is required; zero or ambiguous returns `None`.
pub(crate) fn resolve_plan_for_blast(
    plan_path: Option<&str>,
    input: Option<&str>,
    entries: Option<&[serde_json::Value]>,
) -> Option<String> {
    if let Some(p) = plan_path {
        if !p.is_empty() {
            return Some(p.to_string());
        }
    }
    let entries = entries?;
    let tokens: Vec<String> = input
        .unwrap_or("")
        .split_whitespace()
        .map(|t| t.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    let by_id: HashMap<String, &serde_json::Value> = entries
        .iter()
        .filter_map(|e| {
            let id = e.get("id")?.as_str()?;
            if e.get("archived_at").map_or(false, |a| !a.is_null()) {
                return None;
            }
            Some((id.to_lowercase(), e))
        })
        .collect();
    let matched: HashSet<&String> = tokens.iter().filter(|t| by_id.contains_key(*t)).collect();
    if matched.len() != 1 {
        return None;
    }
    by_id
        .get(*matched.iter().next().unwrap())
        .and_then(|e| e.get("plan_path"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(String::from)
}

fn size_order(size: &str) -> u8 {
    match size {
        "S" => 0,
        "M" => 1,
        _ => 2,
    }
}

/// Pure blast -> effective-size decision: floor up, cautious down. Returns
/// `(effective_size, announce)`: the token to force into TARGET_SIZE, or
/// `None` meaning leave the operator size untouched, plus a one-line note
/// when something changed. A high verdict floors at `M` even over an explicit
/// `S`; a low verdict downgrades to `S` only when `downgrade` holds and the
/// size was NOT pinned; anything else leaves the size alone.
pub(crate) fn modulate_size(
    verdict: &str,
    size_explicit: bool,
    operator_size: Option<&str>,
    downgrade: bool,
    matched_paths: &[String],
) -> (Option<String>, Option<String>) {
    let base = operator_size.unwrap_or("M");
    if verdict == HIGH {
        let floor = "M";
        let effective = if size_order(base) >= size_order(floor) {
            base.to_string()
        } else {
            floor.to_string()
        };
        let hit = matched_paths
            .first()
            .map(String::as_str)
            .unwrap_or("control-plane");
        let note = if effective != base {
            format!("blast: high ({hit}) -> floor {effective} (operator size {base} raised)")
        } else {
            format!("blast: high ({hit}); ceremony already at/above floor M (size {base})")
        };
        return (Some(effective), Some(note));
    }
    if verdict == LOW && downgrade && !size_explicit {
        return (
            Some("S".to_string()),
            Some("blast: low -> fast path S (no size pinned)".to_string()),
        );
    }
    (None, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_map_parse() {
        let plan = "# Plan\n\n## File Ownership Map\n\n| File | Action | Owner |\n\
                    |---|---|---|\n| `db/migrations/2026.sql` | modify | 1.1 |\n\
                    | init-modulation test | doc | 2.1 |\n\n## Steps\n\n| `x.sql` | edit | 1 |\n";
        assert_eq!(parse_ownership_map(plan), vec!["db/migrations/2026.sql"]);
        assert!(parse_ownership_map("# Plan\n\nNo ownership map.\n").is_empty());
    }

    #[test]
    fn classify_high_low_unknown() {
        let cfg = BlastCfg::default();
        let (verdict, matched, reason) =
            classify(&["db/migrations/2026.sql".to_string()], &cfg, None, None);
        assert_eq!(verdict, HIGH);
        assert_eq!(matched, vec!["db/migrations/2026.sql".to_string()]);
        assert_eq!(
            reason,
            "high-blast surface: db/migrations/2026.sql ~ **/migrations/**"
        );
        let (verdict, matched, reason) = classify(
            &["src/widget.ts".to_string(), "docs/a.md".to_string()],
            &cfg,
            None,
            None,
        );
        assert_eq!(verdict, LOW);
        assert!(matched.is_empty());
        assert_eq!(reason, "all 2 path(s) outside the blast map");
        let (verdict, _, reason) = classify(&[], &cfg, None, None);
        assert_eq!(verdict, UNKNOWN);
        assert_eq!(reason, "no usable paths in the File Ownership Map");
    }

    #[test]
    fn blast_cfg_fail_safe_to_disabled() {
        let dir = std::env::temp_dir().join(format!("fno-blast-cfg-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".fno")).unwrap();
        std::fs::write(
            dir.join(".fno/config.toml"),
            "[target.blast]\nenabled = true\ndowngrade = \"yes\"\n",
        )
        .unwrap();
        assert!(!load_blast_cfg(&dir).enabled);
        std::fs::write(
            dir.join(".fno/config.toml"),
            "[target.blast]\nenabled = true\n",
        )
        .unwrap();
        let cfg = load_blast_cfg(&dir);
        assert!(cfg.enabled);
        assert!(cfg.downgrade);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn modulation_matrix() {
        let (eff, note) = modulate_size("low", false, None, true, &[]);
        assert_eq!(eff.as_deref(), Some("S"));
        assert!(note.unwrap().contains("fast path S"));
        let (eff, note) = modulate_size(
            "high",
            true,
            Some("S"),
            true,
            &["scripts/lib/x.sh".to_string()],
        );
        assert_eq!(eff.as_deref(), Some("M"));
        assert!(note.unwrap().contains("scripts/lib/x.sh"));
        let (eff, _) = modulate_size("high", false, None, true, &[]);
        assert_eq!(eff.as_deref(), Some("M"));
        let (eff, _) = modulate_size("high", true, Some("L"), true, &[]);
        assert_eq!(eff.as_deref(), Some("L"));
        let (eff, note) = modulate_size("low", true, Some("M"), true, &[]);
        assert!(eff.is_none() && note.is_none());
        let (eff, _) = modulate_size("low", false, None, false, &[]);
        assert!(eff.is_none());
        let (eff, _) = modulate_size("high", false, None, false, &[]);
        assert_eq!(eff.as_deref(), Some("M"));
        let (eff, note) = modulate_size("unknown", false, None, true, &[]);
        assert!(eff.is_none() && note.is_none());
        let (eff, note) = modulate_size("high", false, Some("M"), true, &[]);
        assert_eq!(eff.as_deref(), Some("M"));
        assert!(note.unwrap().contains("already at/above floor M"));
    }
}
