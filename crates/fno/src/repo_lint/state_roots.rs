//! The state-roots ratchet (`fno doctor lint state-roots`), ported from
//! `cli/src/fno/lint_cli.py` (deleted). Two rules, one baseline, both
//! directions. Rule A matches a `.fno` join token on a line whose two-line
//! window also names a state file; rule B matches a zero-argument cache over
//! a root read. Recorded departure: rule B is a line-scoped scanner, not a
//! Python-AST walk; the both-directions baseline keeps a drained site loud.

use super::Invocation;
use std::path::{Path, PathBuf};

const STATE_ROOTS_BASELINE: &str = "scripts/ci/state-roots-baseline.txt";

/// (filename, resolver, owning_modules) - the `fno.paths.STATE_FILES` table.
const STATE_FILES: &[(&str, Option<&str>, &[&str])] = &[
    (
        "ledger.json",
        Some("fno.paths.ledger_json"),
        &["cli/src/fno/paths.py"],
    ),
    (
        "registry.json",
        Some("fno.paths.agents_registry_path"),
        &["cli/src/fno/paths.py", "crates/fno-agents/src/paths.rs"],
    ),
    (
        "claims",
        Some("fno_agents.claims_root.global_claims_dir"),
        &["crates/fno-agents/src/claims_root.rs"],
    ),
    (
        "messages.jsonl",
        Some("fno.paths.bus_dir"),
        &["cli/src/fno/paths.py", "crates/fno-agents/src/intel.rs"],
    ),
    (
        "briefs",
        Some("fno.paths.briefs_dir"),
        &["cli/src/fno/paths.py"],
    ),
    (
        "events.jsonl",
        Some("fno.paths.project_events_json"),
        &["cli/src/fno/paths.py", "crates/fno-agents/src/paths.rs"],
    ),
    (
        "target-state.md",
        Some("fno.paths.target_state_path"),
        &["cli/src/fno/paths.py", "cli/src/fno/agent/state.py"],
    ),
];

/// The root reads rule B matches beyond the table's resolvers.
const ROOT_READS: &[&str] = &[
    "load_settings",
    "_candidate_paths",
    "global_claims_root",
    "resolve_repo_root",
    "resolve_canonical_repo_root",
    "resolve_canonical_worktree",
    "state_dir",
    "home",
    "getcwd",
    "cwd",
];

/// Production Python and Rust sources in scope, tests dirs excluded. The
/// Python leg's table: `cli/src/fno` for `.py`, `crates/` for `.rs`.
fn scan_files(root: &Path) -> Vec<(String, PathBuf, String)> {
    let mut out: Vec<(String, PathBuf, String)> = Vec::new();
    let bases = [root.join("cli/src/fno"), root.join("crates")];
    for base in bases {
        let mut stack = Vec::new();
        stack.push(base);
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if path.is_dir() {
                    if name == "target" || name.starts_with('.') {
                        continue;
                    }
                    stack.push(path);
                } else if name.ends_with(".py") || name.ends_with(".rs") {
                    push_scannable(&mut out, root, path);
                }
            }
        }
    }
    out.sort();
    out
}

fn push_scannable(out: &mut Vec<(String, PathBuf, String)>, root: &Path, path: PathBuf) {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(&path)
        .to_string_lossy()
        .replace(chr_bs(), "/");
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.contains(&"tests") || parts.contains(&"target") {
        return;
    }
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if name.starts_with("test_") || name == "paths_testing.py" {
        return;
    }
    let suffix = suffix_guess(root, &path);
    out.push((rel, path, suffix));
}

fn suffix_guess(root: &Path, path: &Path) -> String {
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(chr_bs(), "/");
    let _ = rel;
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!(".{ext}")
}

/// Repo-relative path with forward slashes.
pub(crate) fn rel_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(chr_bs(), "/")
}

/// The backslash as a one-codepoint string, kept out of the writer's way.
fn chr_bs() -> char {
    char::from_u32(92).expect("backslash")
}

pub(crate) fn rule_a_violations(root: &Path) -> Vec<(String, String, String)> {
    let mut violations: Vec<(String, String, String)> = Vec::new();
    let py_join = regex::Regex::new(r#"(?:/\s*["']\.fno["']|["']\.fno/)"#).unwrap();
    let rs_join = regex::Regex::new(r#"join\(\s*["']\.fno"#).unwrap();
    // One compile per state file, not per scanned line.
    let rules: Vec<(&str, Option<&str>, &[&str], regex::Regex)> = STATE_FILES
        .iter()
        .map(|(filename, resolver, owning)| {
            // The regex crate has no look-around; the boundary is a consumed
            // non-word char (or an edge) instead.
            let name_re = regex::Regex::new(&format!(
                r"(^|[^\w.\-]){}($|[^\w.\-])",
                regex::escape(filename)
            ))
            .unwrap();
            (*filename, *resolver, *owning, name_re)
        })
        .collect();
    for (rel, path, suffix) in scan_files(root) {
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let lines: Vec<&str> = body.lines().collect();
        let skip = if suffix == ".rs" {
            crate::repo_lint::seam_crossings::rust_cfg_test_lines(&lines)
        } else {
            Default::default()
        };
        for (no, line) in lines.iter().enumerate() {
            let join_re = if suffix == ".rs" { &rs_join } else { &py_join };
            if skip.contains(&(no + 1)) || !join_re.is_match(line) {
                continue;
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("#")
                || trimmed.starts_with("//")
                || trimmed.starts_with("///")
                || trimmed.starts_with("*")
            {
                continue;
            }
            let lnum = no + 1;
            let window: String = lines[no..(no + 3).min(lines.len())].join("\n");
            for (filename, resolver, owning, name_re) in &rules {
                if owning.contains(&rel.as_str()) {
                    continue;
                }
                if !name_re.is_match(&window) {
                    continue;
                }
                let owner = resolver.unwrap_or("NOTHING - no resolver today");
                violations.push((
                    filename.to_string(),
                    rel.clone(),
                    format!(
                        "{rel}:{lnum}: hand-built state path '.fno/{filename}' \
                     This file has ONE resolver: {owner}. Fix: call the resolver; \
                     if this site cannot move yet, add it to \
                     scripts/ci/state-roots-baseline.txt."
                    ),
                ));
            }
        }
    }
    violations
}

/// Rule B: a zero-argument cache over a root read. The Python leg walked the
/// ast module; this scanner reads the same three facts off the lines: a
/// cache decorator, a zero-parameter def, and a call in the body to a root
/// read. Recorded departure: a signature wrapped across lines can hide a
/// parameter; the both-directions baseline keeps any miss loud.
pub(crate) fn rule_b_violations(root: &Path) -> Vec<(String, String, String)> {
    let mut violations = Vec::new();
    let mut reads: Vec<&str> = ROOT_READS.to_vec();
    for (_f, resolver, _o) in STATE_FILES {
        if let Some(res) = resolver {
            let tail = res.rsplit('.').next().unwrap_or(res);
            if !reads.contains(&tail) {
                reads.push(tail);
            }
        }
    }
    for (rel, path, suffix) in scan_files(root) {
        if suffix != ".py" {
            continue;
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        scan_rule_b_file(&rel, &body, &reads, &mut violations);
    }
    violations
}

/// One Python file's rule B scan.
fn scan_rule_b_file(
    rel: &str,
    body: &str,
    reads: &[&str],
    violations: &mut Vec<(String, String, String)>,
) {
    let decorator =
        regex::Regex::new(r"^\s*@(?:functools\.)?(?:lru_cache(?:\([^)]*\))?|cache)\s*$").unwrap();
    let zero_def = regex::Regex::new(r"^\s*def\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(\s*\)\s*:").unwrap();
    let lines: Vec<&str> = body.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if !decorator.is_match(line) {
            continue;
        }
        let Some((def_idx, fn_name)) = next_zero_def(&lines, i + 1, &zero_def) else {
            continue;
        };
        let Some(read) = (0..reads.len())
            .map(|k| reads[k])
            .find(|r| body_calls(&lines, def_idx, r))
        else {
            continue;
        };
        violations.push((
            rel.to_string(),
            fn_name.to_string(),
            format!(
                "{rel}:{}: {fn_name} is a zero-argument cache over a reader that \
                 resolves a state root (calls {read}). Fix: make the root an \
                 ARGUMENT so the cache keys on it.",
                def_idx + 1
            ),
        ));
    }
}

/// The next zero-parameter def at or after `from`, skipping blanks and
/// decorators; None at the first other item.
fn next_zero_def<'a>(
    lines: &[&'a str],
    from: usize,
    zero_def: &regex::Regex,
) -> Option<(usize, &'a str)> {
    for (i, line) in lines.iter().enumerate().skip(from) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('@') {
            continue;
        }
        // Index the source line through the match bounds; the Captures value
        // itself dies here, so the returned slice must borrow the text.
        return zero_def.captures(line).and_then(|c| {
            let m = c.get(1)?;
            Some((i, line[m.start()..m.end()].trim()))
        });
    }
    None
}

/// True when the zero-arg def's body calls `name`, stopping at the next line
/// at or outside the def's indent.
fn body_calls(lines: &[&str], def_idx: usize, name: &str) -> bool {
    let def_line = lines[def_idx];
    let indent = def_line.len() - def_line.trim_start().len();
    let call =
        regex::Regex::new(&format!(r"(^|[^A-Za-z0-9_]){}\s*\(", regex::escape(name))).unwrap();
    for line in lines.iter().skip(def_idx + 1) {
        if line.trim().is_empty() {
            continue;
        }
        let line_indent = line.len() - line.trim_start().len();
        if line_indent <= indent && !line.trim().starts_with('#') {
            return false;
        }
        if call.is_match(line) {
            return true;
        }
    }
    false
}

/// Baseline keys: (rule, path, key).
type Baseline = std::collections::BTreeSet<(String, String, String)>;

/// Parse the ratchet file into baseline keys.
fn read_baseline_keys(path: &Path) -> Baseline {
    let mut entries = Baseline::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return entries;
    };
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = stripped.split('\t').map(str::trim).collect();
        if fields.len() < 3 || fields[..3].iter().any(|f| f.is_empty()) {
            continue;
        }
        entries.insert((fields[0].into(), fields[1].into(), fields[2].into()));
    }
    entries
}

/// Both rules' findings: (rule, rel, key, message).
fn find_all(root: &Path) -> Vec<(String, String, String, String)> {
    let mut out: Vec<(String, String, String, String)> = Vec::new();
    for (filename, rel, msg) in rule_a_violations(root) {
        out.push(("A".into(), rel, filename, msg));
    }
    for (rel, name, msg) in rule_b_violations(root) {
        out.push(("B".into(), rel, name, msg));
    }
    out
}

/// The door: findings minus baseline is new, baseline minus findings is
/// stale; either direction fails. The ok line names the baselined count.
pub fn run(inv: &Invocation) -> i32 {
    let root = &inv.root;
    let findings = find_all(root);
    let baseline = read_baseline_keys(&root.join(STATE_ROOTS_BASELINE));
    let keys: Baseline = findings
        .iter()
        .map(|f| (f.0.clone(), f.1.clone(), f.2.clone()))
        .collect();
    let new: Vec<_> = findings
        .iter()
        .filter(|f| !baseline.contains(&(f.0.clone(), f.1.clone(), f.2.clone())))
        .collect();
    let stale: Vec<_> = baseline.iter().filter(|k| !keys.contains(k)).collect();
    if !new.is_empty() || !stale.is_empty() {
        eprintln!("state-roots: new violations:");
        for (rule, rel, _key, msg) in &new {
            eprintln!("  [{rule}] {rel}: {msg}");
        }
        eprintln!("state-roots: fixed, remove these baseline lines:");
        for (rule, rel, key) in &stale {
            eprintln!(
                "  {rel}\t{key}\t(rule {rule}) no longer matches - delete this line from \
                 {STATE_ROOTS_BASELINE}. A drained exemption left in the baseline is a \
                 permanent one."
            );
        }
        return 1;
    }
    println!(
        "state-roots: ok ({} baselined site(s) awaiting their owning node)",
        baseline.len()
    );
    0
}
