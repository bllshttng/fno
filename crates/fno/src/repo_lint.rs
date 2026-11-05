//! The repository lint checks behind `fno doctor lint <check>`.
//!
//! The Python residue (`cli/src/fno/lint_cli.py`, `lint_command_ratchet.py`,
//! `lint_shellout_drift.py`, `lint_seam_crossings.py`) is deleted; this module
//! is the one owner, native at the front door beside menu-caps. `registry` and
//! `field-coverage` forward to the fno-agents binary (the registry readers and
//! the row projection live there), and `style` keeps its style-check door.
//!
//! Per-check behavior mirrors the Python dispatcher it replaces: each check
//! declares the flags it accepts, a supplied flag a check does not declare is
//! silently dropped (the Python dispatch called each check with only its own
//! parameters), an unknown check name refuses with the real names at exit 2,
//! and `--root <dir>` points every check at a fixture tree (the seam
//! selftest's injection path; the Python selftest drove the module API the
//! same way).

pub mod agents_surface;
pub mod command_ratchet;
pub mod seam_crossings;
pub mod shellout_drift;
pub mod state_roots;

use std::path::{Path, PathBuf};
use std::process::Command;

/// The check names in the order the dispatcher's refusal lists them (sorted,
/// like the Python `CHECKS` dict's keys were).
pub const CHECKS: &[&str] = &[
    "command-ratchet",
    "field-coverage",
    "hook-tombstones",
    "internal-refs",
    "plan-filenames",
    "preamble-budget",
    "registry",
    "retired-commands",
    "seam-crossings",
    "shellout-drift",
    "spawn-paths",
    "stale-skill-refs",
    "state-roots",
];

/// One check's accepted flags, as the Python dispatcher's signature dispatch
/// held them. A flag outside the set is dropped for that check, not refused.
fn accepts(check: &str, flag: &str) -> bool {
    match check {
        "command-ratchet" | "seam-crossings" => flag == "--update",
        "shellout-drift" => flag == "--no-degrade",
        "registry" => flag == "--json" || flag == "-J",
        "field-coverage" => flag == "--json" || flag == "-J" || flag == "--live",
        "hook-tombstones" => flag == "--base",
        _ => false,
    }
}

/// The parsed invocation: the check, the repo root, and the per-check flags.
#[derive(Debug)]
pub struct Invocation {
    pub check: String,
    pub root: PathBuf,
    pub update: bool,
    pub no_degrade: bool,
    pub json: bool,
    pub live: bool,
    pub base: Option<String>,
}

/// Parse the check tail: the check name, then the check's flags. `--root`
/// is a door-level flag and is accepted before or after the check name.
/// Refusals name what is missing, like the Python surface.
pub fn parse(args: &[String]) -> Result<Invocation, String> {
    let mut root_override: Option<String> = None;
    let rest: &[String] = args;
    let mut check: Option<String> = None;
    let mut flags: Vec<String> = Vec::new();
    for (i, token) in rest.iter().enumerate() {
        if token == "--root" {
            match rest.get(i + 1) {
                Some(dir) => {
                    root_override = Some(dir.clone());
                }
                None => return Err("--root needs a directory".into()),
            }
            continue;
        }
        if i > 0 && rest[i - 1] == "--root" {
            continue;
        }
        if check.is_none() && token.starts_with('-') {
            return Err(format!(
                "fno doctor lint: unknown check {token:?}. Known: {}",
                CHECKS.join(", ")
            ));
        }
        if check.is_none() {
            check = Some(token.clone());
            continue;
        }
        flags.push(token.clone());
    }
    let check = match check {
        Some(name) => name,
        None => {
            return Err(format!(
                "fno doctor lint: unknown check ''. Known: {}",
                CHECKS.join(", ")
            ))
        }
    };
    if !CHECKS.contains(&check.as_str()) {
        return Err(format!(
            "fno doctor lint: unknown check {check:?}. Known: {}",
            CHECKS.join(", ")
        ));
    }
    let mut out = Invocation {
        check: check.clone(),
        root: repo_root(root_override.as_deref())?,
        update: false,
        no_degrade: false,
        json: false,
        live: false,
        base: None,
    };
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        let name = flag.as_str();
        let takes_value = name == "--dispatch-path" || name == "--base";
        let value = if takes_value {
            match it.next() {
                Some(v) => Some(v.clone()),
                None => return Err(format!("{name} needs a value")),
            }
        } else {
            None
        };
        let short_json = name == "-J";
        let canonical = if short_json { "--json" } else { name };
        if !accepts(&check, canonical) {
            // The Python dispatch dropped a flag the check does not declare.
            continue;
        }
        match (canonical, value) {
            ("--update", _) => out.update = true,
            ("--no-degrade", _) => out.no_degrade = true,
            ("--json", _) => out.json = true,
            ("--live", _) => out.live = true,
            ("--base", v) => out.base = v,
            _ => {}
        }
    }
    Ok(out)
}

/// The door. Exit codes are each check's own; parse refusals exit 2.
pub fn run(args: &[String]) -> i32 {
    let inv = match parse(args) {
        Ok(inv) => inv,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    match inv.check.as_str() {
        "command-ratchet" => command_ratchet::run(&inv),
        "field-coverage" => forward_registry_family(&inv),
        "hook-tombstones" => hook_tombstones(&inv),
        "internal-refs" => run_ci_gate(&inv.root, "scripts/ci/check-no-internal-refs.sh"),
        "plan-filenames" => plan_filenames(&inv),
        "preamble-budget" => run_ci_gate(&inv.root, "scripts/ci/check-preamble-budget.sh"),
        "registry" => forward_registry_family(&inv),
        "retired-commands" => run_ci_gate(&inv.root, "scripts/ci/check-retired-command-strings.sh"),
        "seam-crossings" => seam_crossings::run(&inv),
        "shellout-drift" => shellout_drift::run(&inv),
        "spawn-paths" => spawn_paths(&inv),
        "stale-skill-refs" => run_ci_gate(&inv.root, "scripts/ci/check-no-stale-skill-refs.sh"),
        "state-roots" => state_roots::run(&inv),
        _ => unreachable!("parse rejects unknown checks"),
    }
}

/// The repo root: `FNO_REPO_ROOT`, then `git rev-parse --show-toplevel` from
/// the cwd, then the cwd - the order `fno.paths.resolve_repo_root` held.
pub fn repo_root(override_dir: Option<&str>) -> Result<PathBuf, String> {
    let env_pin = std::env::var_os("FNO_REPO_ROOT")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string_lossy().into_owned());
    if let Some(pin) = override_dir.or(env_pin.as_deref()) {
        return Ok(PathBuf::from(pin));
    }
    let cwd = std::env::current_dir().map_err(|e| format!("cwd unreadable: {e}"))?;
    let out = git_capture(&cwd, &["rev-parse", "--show-toplevel"]);
    match out {
        Some(top) if !top.trim().is_empty() => Ok(PathBuf::from(top.trim())),
        _ => Ok(cwd),
    }
}

/// One git call from `dir`, stdout on success, None on any failure. None means
/// the instrument never ran, never "it found nothing".
pub fn git_capture(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// Run a `scripts/ci` bash gate from the repo root; its exit code passes
/// through. Exit 2 here is the wrapper's alone - the script is missing or
/// unrunnable - since every gate failure path exits 1 itself.
fn run_ci_gate(root: &Path, rel_path: &str) -> i32 {
    let script = root.join(rel_path);
    if !script.exists() {
        eprintln!("gate script not found at {}", script.display());
        return 2;
    }
    match Command::new("bash").arg(&script).current_dir(root).status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(e) => {
            eprintln!("failed to run gate script: {e}");
            2
        }
    }
}

/// `registry` and `field-coverage` read the live registry rows, whose
/// liveness legs and row projection live in the fno-agents binary. Forward
/// as the style-check door does; output and exit codes pass through.
fn forward_registry_family(inv: &Invocation) -> ! {
    // One flag per family member: the client's status door reads the FIRST
    // rest token as the op.
    let op = if inv.check == "field-coverage" {
        "--lint-field-coverage"
    } else {
        "--lint-registry"
    };
    let mut argv: Vec<std::ffi::OsString> = vec!["status".into(), op.into()];
    if inv.check == "field-coverage" && inv.live {
        argv.push("--live".into());
    }
    if inv.json {
        argv.push("--json".into());
    }
    crate::bootstrap::forward_agents(&argv, &format!("fno doctor lint {}", inv.check))
}

/// `spawn-paths`: reject new single-line hand-assembled Claude/Codex session
/// argv shapes outside the allowlist. The scan is the Python leg's narrow
/// grep-style guard, line regexes verbatim; multi-line assembly stays
/// documented coverage debt.
fn spawn_paths(inv: &Invocation) -> i32 {
    const ALLOWLIST: &[&str] = &["cli/src/fno/llm.py"];
    let argv_shape = regex::Regex::new(
        r#"\[\s*['\"](?:claude|codex)['\"](?:\s*,\s*[^,\]]+)*\s*,\s*['\"](?:--print|--bg|-p|--exec)['\"]"#,
    )
    .expect("static regex");
    let shell_shape =
        regex::Regex::new(r"\bclaude\s+(?:--print|--bg|-p)\b|\bcodex\s+(?:--exec|exec)\b")
            .expect("static regex");
    let mut violations: Vec<String> = Vec::new();
    for root_rel in ["cli/src/fno", "scripts"] {
        let base = inv.root.join(root_rel);
        if !base.is_dir() {
            continue;
        }
        let mut files: Vec<PathBuf> = walkdir_py_sh(&base);
        files.sort();
        for path in files {
            let rel = path
                .strip_prefix(&inv.root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if ALLOWLIST.contains(&rel.as_str()) || rel.split('/').any(|part| part == "tests") {
                continue;
            }
            let body = match std::fs::read_to_string(&path) {
                Ok(body) => body,
                Err(_) => continue,
            };
            let is_sh = path.extension().is_some_and(|ext| ext == "sh");
            for (no, line) in body.lines().enumerate() {
                let mut hit = argv_shape.is_match(line);
                if !hit && is_sh {
                    hit = shell_shape.is_match(line);
                }
                if let Some(m) = hit
                    .then(|| {
                        argv_shape
                            .find(line)
                            .or_else(|| shell_shape.find(line))
                            .map(|m| m.as_str().to_string())
                    })
                    .flatten()
                {
                    violations.push(format!(
                        "{rel}:{}: hand-assembled session spawn shape {m:?}",
                        no + 1
                    ));
                }
            }
        }
    }
    if !violations.is_empty() {
        eprintln!("spawn-paths: violations:");
        for v in &violations {
            eprintln!("  {v}");
        }
        eprintln!(
            "\nFix: route session launches through fno agents spawn, or add the exact \
             source file to SPAWN_SHAPE_ALLOWLIST in \
             crates/fno-agents/src/repo_lint.rs with a census-backed reason."
        );
        return 1;
    }
    println!("spawn-paths: ok");
    0
}

/// `hook-tombstones`: refuse deleting a hook script the base revision's hook
/// config referenced. Two steps across two releases: stub, then delete.
fn hook_tombstones(inv: &Invocation) -> i32 {
    let root = &inv.root;
    let resolved = match inv.base.as_deref() {
        Some(base) => Some(base.to_string()),
        None => resolve_range_base(root),
    };
    let resolved = match resolved {
        Some(rev) => rev,
        None => {
            eprintln!(
                "hook-tombstones: no git repo, or no revision to compare against (pass --base <rev>)."
            );
            return 2;
        }
    };
    let refs = referenced_at_revision(root, &resolved);
    let deleted = deleted_in_range(root, &resolved, "HEAD");
    let (refs, deleted) = match (refs, deleted) {
        (Some(r), Some(d)) => (r, d),
        _ => {
            eprintln!(
                "hook-tombstones: git failed reading {resolved}..HEAD; refusing to read a \
                 broken probe as a clean pass."
            );
            return 2;
        }
    };
    let mut findings: Vec<String> = refs.intersection(&deleted).cloned().collect();
    findings.sort();
    if !findings.is_empty() {
        eprintln!(
            "hook-tombstones: {} referenced hook script(s) deleted since {resolved}:",
            findings.len()
        );
        for rel in &findings {
            eprintln!("  {rel}");
        }
        eprintln!(
            "  Running sessions answer hook config from an init-time snapshot, and a \
             PreToolUse hook that cannot launch fails the whole Bash call, so each \
             deletion above bricks every pre-merge session's shell. Fix: restore the \
             path as a no-op stub and remove only its config entry this release; \
             delete the stub in a later release. A session already bricked recovers \
             without Bash by writing any watched settings file, which forces a \
             hook-config reload."
        );
        return 1;
    }
    println!(
        "hook-tombstones: ok ({} referenced script(s) at {resolved}, 0 deleted)",
        refs.len()
    );
    0
}

/// The revision a deletion is measured against, by trigger shape: merge-base
/// on a PR checkout, `GITHUB_EVENT_BEFORE` on a push to main, `HEAD~1` local.
fn resolve_range_base(root: &Path) -> Option<String> {
    let head = git_capture(root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    if let Some(origin_main) = git_capture(root, &["rev-parse", "--verify", "origin/main"]) {
        let origin_main = origin_main.trim();
        if origin_main != head {
            if let Some(merged) = git_capture(root, &["merge-base", &head, "origin/main"]) {
                let merged = merged.trim();
                if merged != head {
                    return Some(merged.to_string());
                }
            }
        }
    }
    let event_before = std::env::var("GITHUB_EVENT_BEFORE").unwrap_or_default();
    if !event_before.is_empty() {
        let probe = format!("{event_before}^{{commit}}");
        if let Some(verified) = git_capture(root, &["rev-parse", "--verify", &probe]) {
            let verified = verified.trim();
            if verified != head {
                return Some(verified.to_string());
            }
        }
    }
    if let Some(parent) = git_capture(root, &["rev-parse", "--verify", "HEAD~1"]) {
        let parent = parent.trim();
        if parent != head {
            return Some(parent.to_string());
        }
    }
    None
}

/// Script paths a hook config document names, spelled
/// `${CLAUDE_PLUGIN_ROOT|CODEX_PLUGIN_ROOT|PLUGIN_ROOT}/<path-to-whitespace>`.
pub fn referenced_scripts(config_text: &str) -> Vec<String> {
    let re = regex::Regex::new(
        r#"\$\{(?:CLAUDE_PLUGIN_ROOT|CODEX_PLUGIN_ROOT|PLUGIN_ROOT)\}/([^\s\"'\\]+)"#,
    )
    .expect("static regex");
    re.captures_iter(config_text)
        .map(|c| c[1].to_string())
        .collect()
}

/// Every `*hooks.json` under hooks/ tracked at `rev`, repo-relative.
fn hook_configs_at(root: &Path, rev: &str) -> Option<Vec<String>> {
    let out = git_capture(root, &["ls-tree", "-r", "--name-only", rev, "--", "hooks/"])?;
    Some(
        out.lines()
            .filter(|line| line.ends_with("hooks.json"))
            .map(str::to_string)
            .collect(),
    )
}

/// Union of script paths referenced by all hook configs at `rev`.
fn referenced_at_revision(root: &Path, rev: &str) -> Option<std::collections::BTreeSet<String>> {
    let configs = hook_configs_at(root, rev)?;
    let mut refs = std::collections::BTreeSet::new();
    for config in configs {
        if let Some(text) = git_capture(root, &["show", &format!("{rev}:{config}")]) {
            refs.extend(referenced_scripts(&text));
        }
    }
    Some(refs)
}

/// Files removed between `base` and `head`, two-dot. `--no-renames` is
/// load-bearing: a same-content move otherwise reads as R, not D.
fn deleted_in_range(
    root: &Path,
    base: &str,
    head: &str,
) -> Option<std::collections::BTreeSet<String>> {
    let out = git_capture(
        root,
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "--diff-filter=D",
            &format!("{base}..{head}"),
        ],
    )?;
    Some(
        out.lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

/// `plan-filenames`: report node-bearing plan filenames that disagree with
/// the one node the plan's frontmatter claims.
fn plan_filenames(inv: &Invocation) -> i32 {
    match plan_filenames::scan(&inv.root) {
        plan_filenames::Scan::Clean => {
            println!("plan-filenames: ok");
            0
        }
        plan_filenames::Scan::Mismatched(mismatches) => {
            println!("plan-filenames: mismatches:");
            for m in &mismatches {
                println!("  {}", m.line);
            }
            println!("plan-filenames: {} mismatch(es)", mismatches.len());
            1
        }
    }
}

/// Production `.py` and `.sh` files under `base`, tests dirs excluded.
fn walkdir_py_sh(base: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name == "tests" || name == "target" || name.starts_with('.') {
                    continue;
                }
                stack.push(path);
            } else if name.ends_with(".py") || name.ends_with(".sh") {
                out.push(path);
            }
        }
    }
    out
}

/// The plan-filename mismatch row: the plan path, the filename's node, the
/// claim, rendered exactly as the Python leg printed them.
pub mod plan_filenames {
    use std::path::{Path, PathBuf};

    #[derive(Debug)]
    pub struct Mismatch {
        pub line: String,
    }

    /// Walk the plans content dir; each `.md` whose basename encodes a node id
    /// must claim that same node (frontmatter `node:` or `claims:`).
    pub enum Scan {
        Clean,
        Mismatched(Vec<Mismatch>),
    }

    pub fn scan(root: &Path) -> Scan {
        let plans_dir = match super::plans_content_dir_from(root) {
            Some(dir) => dir,
            None => {
                eprintln!("plan-filenames: plans dir not found");
                std::process::exit(1);
            }
        };
        let prefixes = door_prefixes();
        scan_dir(&plans_dir, &prefixes)
    }

    /// The filename-prefix allowlist: the configured node prefix plus the
    /// legacy `ab`, exactly the set the Python leg passed as
    /// `{LEGACY_PREFIX, node_id_prefix()}`.
    fn door_prefixes() -> std::collections::BTreeSet<String> {
        let mut set = std::collections::BTreeSet::new();
        set.insert(super::door_node_prefix());
        set.insert("ab".to_string());
        set
    }

    pub fn scan_dir(plans_dir: &Path, prefixes: &std::collections::BTreeSet<String>) -> Scan {
        if !plans_dir.is_dir() {
            eprintln!(
                "plan-filenames: plans dir not found: {}",
                plans_dir.display()
            );
            std::process::exit(1);
        }
        let wellformed =
            regex::Regex::new(r"^[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}$").expect("static regex");
        let filename_node =
            regex::Regex::new(r"-(?P<prefix>[a-z][a-z0-9]{0,7})-(?P<hex>[0-9a-f]{4,8})\.md$")
                .expect("static regex");
        let mut mismatches: Vec<Mismatch> = Vec::new();
        let mut plans: Vec<PathBuf> = Vec::new();
        let mut stack = vec![plans_dir.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "md") {
                    plans.push(path);
                }
            }
        }
        plans.sort();
        for path in plans {
            let base = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let base = base.split('#').next().unwrap_or(&base).to_string();
            let Some(caps) = filename_node.captures(&base) else {
                continue;
            };
            let prefix = caps["prefix"].to_string();
            if !prefixes.contains(&prefix) {
                continue;
            }
            let filename_node_id = format!("{}-{}", prefix, &caps["hex"]);
            let body = match std::fs::read_to_string(&path) {
                Ok(body) => body,
                Err(_) => continue,
            };
            let claims = frontmatter_claims(&body, &wellformed);
            if claims.len() == 1 && !claims.contains(&filename_node_id) {
                let claim = claims.into_iter().next().expect("len 1");
                // The Python leg printed the rglob path as it walked it, so
                // the row shows the resolved plans dir's own spelling.
                let shown = path.to_string_lossy().replace('\\', "/");
                mismatches.push(Mismatch {
                    line: format!("{shown}: filename names {filename_node_id}, claims {claim}"),
                });
            }
        }
        if mismatches.is_empty() {
            Scan::Clean
        } else {
            Scan::Mismatched(mismatches)
        }
    }

    /// The node ids a plan's frontmatter declares: `claims:` (inline list or
    /// block items) and `node:`, wellformed ids only, empty on any
    /// unreadable-plan case.
    fn frontmatter_claims(body: &str, wellformed: &regex::Regex) -> Vec<String> {
        let mut lines = body.lines().peekable();
        if lines.peek().map(|l| l.trim_end()) != Some("---") {
            return Vec::new();
        }
        lines.next();
        let mut claims: Vec<String> = Vec::new();
        let mut current_list: Option<String> = None;
        for line in lines {
            if line.trim_end() == "---" || line.trim_end() == "..." {
                break;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("- ") && current_list.as_deref() == Some("claims") {
                let item = trimmed[2..].trim().trim_matches('"').trim_matches('\'');
                if wellformed.is_match(item) {
                    claims.push(item.to_string());
                }
                continue;
            }
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim();
                let value = value.trim();
                current_list = Some(key.to_string());
                if value.is_empty() {
                    continue;
                }
                let strip = value
                    .trim_matches('"')
                    .trim_matches('\'')
                    .trim_start_matches('[')
                    .trim_end_matches(']');
                if key == "claims" {
                    for part in strip.split(',') {
                        let item = part.trim().trim_matches('"').trim_matches('\'');
                        if wellformed.is_match(item) {
                            claims.push(item.to_string());
                        }
                    }
                } else if key == "node" && strip != "null" && !strip.is_empty() {
                    let item = strip.trim_matches('"').trim_matches('\'');
                    if wellformed.is_match(item) {
                        claims.push(item.to_string());
                    }
                } else {
                    current_list = None;
                }
            }
        }
        claims
    }
}

/// The plans content dir this check scans: `$FNO_PLANS_DIR`, then the sibling
/// door that owns the chain (`fno-agents state path plan-dir`, the same door
/// the Python leg's `plans_content_dir()` resolved through), then the vault
/// plans dir as the offline fallback.
pub fn plans_content_dir_from(root: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("FNO_PLANS_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = door_path(&["state", "path", "plan-dir"], root) {
        return Some(dir);
    }
    let vault = root.join("internal").join("fno").join("plans");
    vault.is_dir().then_some(vault)
}

/// The configured node-id prefix, rstripped of its dash, from the sibling
/// settings door; the legacy `ab` fallback matches the deleted
/// `fno.graph._constants.node_id_prefix` fail-open.
fn door_node_prefix() -> String {
    door_fact(&["state", "path", "settings-read"])
        .and_then(|out| {
            serde_json::from_str::<serde_json::Value>(&out)
                .ok()?
                .get("prefix")?
                .as_str()
                .map(str::to_string)
        })
        .map(|p| p.trim_end_matches('-').to_string())
        .unwrap_or_else(|| "ab".to_string())
}

/// One stdout fact from the sibling fno-agents binary; None when the sibling
/// is missing or the door fails.
fn door_fact<I, S>(args: I) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let bin = crate::digest_overlay::fno_agents_bin();
    let out = Command::new(bin).args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn door_path(args: &[&str], root: &Path) -> Option<PathBuf> {
    let mut argv: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    argv.push(root.as_os_str().to_os_string());
    door_fact(argv)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
}

#[cfg(test)]
mod tests;
