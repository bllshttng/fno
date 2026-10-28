//! Shell-harness discovery for the smoke runner: the owned trees, shebang
//! gating, the reverse source-reference map, and the small helpers (fnmatch,
//! weights, prereqs). Ported from test_cmd.py's discovery half.
use std::collections::BTreeMap;
use std::path::Path;

use regex::Regex;

use crate::test_smoke_registry as reg;

/// (name, cwd, cmd) as the registry steps spell themselves.
pub type Step = (String, String, String);

/// True when `name` matches any glob in a comma-separated list. `--only` and
/// `--skip` share this, which is what makes them exact complements: the same
/// list partitions any step list into the steps one selector keeps and the
/// steps the other keeps, with nothing in neither. The whole value is tried
/// BEFORE the split, because three registry steps have a comma in their own
/// name.
pub fn name_matches(name: &str, globs: &str) -> bool {
    if fnmatch(name, globs.trim()) {
        return true;
    }
    globs
        .split(',')
        .filter(|g| !g.trim().is_empty())
        .any(|g| fnmatch(name, g.trim()))
}

/// A fnmatch-style matcher: `*`, `?` and `[seq]` against one whole string.
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    compile(pattern).is_match(name)
}

fn compile(pattern: &str) -> regex::Regex {
    let mut re = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => re.push_str(".*"),
            '?' => re.push('.'),
            '[' => {
                re.push('[');
                if chars.peek() == Some(&'!') {
                    chars.next();
                    re.push('^');
                }
                while let Some(&c2) = chars.peek() {
                    chars.next();
                    match c2 {
                        ']' => break,
                        '\\' => re.push_str("\\\\"),
                        _ => re.push(c2),
                    }
                }
                re.push(']');
            }
            c if " .+()|{}^$".contains(c) => {
                re.push('\\');
                re.push(c);
            }
            c => re.push(c),
        }
    }
    re.push('$');
    regex::Regex::new(&re).unwrap_or_else(|_| regex::Regex::new("$^").unwrap())
}

/// The real smoke registry: the structural steps plus what discovery finds.
/// One copy, so a caller reasoning about the registry reads the list the
/// runner will actually execute.
pub fn smoke_steps(root: &Path) -> Vec<Step> {
    let structural: Vec<Step> = reg::STRUCTURAL_STEPS
        .iter()
        .map(|(n, c, m)| (n.to_string(), c.to_string(), m.to_string()))
        .collect();
    let referenced = referenced_sh_files(&structural);
    structural
        .into_iter()
        .chain(discovered_steps_with(root, &referenced))
        .collect()
}

/// Auto-discovered shell steps the full gate runs: a harness discovery finds,
/// that no structural step already references and that is not quarantined.
/// Both consumers (full gate and changed selector) subtract
/// DISCOVERY_DEFERRED, so dropping a line widens both gates.
pub fn discovered_steps_with(
    root: &Path,
    referenced: &std::collections::BTreeSet<String>,
) -> Vec<Step> {
    discover_shell_harnesses(root)
        .into_iter()
        .filter(|rel| !referenced.contains(rel) && !is_deferred(rel))
        .map(|rel| (rel.clone(), ".".to_string(), format!("bash {rel}")))
        .collect()
}

pub fn is_deferred(rel: &str) -> bool {
    reg::DISCOVERY_DEFERRED.iter().any(|d| *d == rel)
}

/// Auto-discover owned shell harnesses; return repo-relative paths.
///
/// A file is a harness iff it has a shell shebang, is not a library
/// (scripts/lib/) or an orchestrated subtree (cli/tests/smoke/), and is not a
/// source-only helper. This is what makes a new tests/hooks/test_x.sh run
/// with no registry edit.
pub fn discover_shell_harnesses(root: &Path) -> Vec<String> {
    let mut candidates: std::collections::BTreeSet<String> = Default::default();
    for pattern in reg::HARNESS_GLOBS {
        for rel in walk_glob(root, pattern) {
            if rel.starts_with("scripts/lib/") || rel.starts_with("cli/tests/smoke/") {
                continue;
            }
            if !has_shell_shebang(&root.join(&rel)) {
                continue;
            }
            candidates.insert(rel);
        }
    }
    let sourced = sourced_helpers(root, &candidates);
    candidates.retain(|c| !sourced.contains(c));
    candidates.into_iter().collect()
}

/// Walk one repo-relative glob pattern (the registry's flat shapes:
/// `dir/*.sh`, `dir/*/*.sh`, `skills/*/tests/*.sh`, `tests/*.sh`). No `**`.
pub fn walk_glob(root: &Path, pattern: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let parts: Vec<&str> = pattern.split('/').collect();
    let (dirs, last) = parts.split_at(parts.len() - 1);
    let mut current: Vec<PathBuf> = vec![root.to_path_buf()];
    for part in dirs {
        let mut next: Vec<PathBuf> = Vec::new();
        for base in &current {
            let Ok(read) = std::fs::read_dir(base) else {
                continue;
            };
            for entry in read.flatten() {
                if !entry.path().is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                if part == &"*" || fnmatch(&name, part) {
                    next.push(entry.path());
                }
            }
        }
        current = next;
    }
    for base in &current {
        let Ok(read) = std::fs::read_dir(base) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if last[0] == "*" || fnmatch(&name, last[0]) {
                out.push(
                    entry
                        .path()
                        .strip_prefix(root)
                        .unwrap_or(entry.path().as_path())
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }
    out.sort();
    out
}

use std::path::PathBuf;

/// A file is a shell harness only if its first line opens `#!` with `sh` in
/// it.
pub fn has_shell_shebang(path: &Path) -> bool {
    let Ok(first) = read_first_line(path) else {
        return false;
    };
    first.starts_with(b"#!") && first.windows(2).any(|w| w == b"sh")
}

fn read_first_line(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut fh = std::fs::File::open(path)?;
    let mut buf = [0u8; 512];
    let n = fh.read(&mut buf)?;
    let line = &buf[..n];
    Ok(line.split(|b| *b == b'\n').next().unwrap_or(line).to_vec())
}

/// Repo-relative .sh paths a structural step already invokes (for dedup).
pub fn referenced_sh_files(steps: &[Step]) -> std::collections::BTreeSet<String> {
    let re = Regex::new(r"[\w./@{}$+-]*\.sh\b").expect("static regex");
    let mut out = std::collections::BTreeSet::new();
    for (_name, _cwd, cmd) in steps {
        for token in re.find_iter(cmd) {
            let token = token.as_str().trim_start_matches("./");
            out.insert(token.to_string());
        }
    }
    out
}

/// Reverse source-reference map: helper path -> the files that source it.
/// Read forward by discovery (a sourced file is a helper, not a standalone
/// harness; running it directly produces a spurious failure) and backward by
/// changed-selection (a touched helper must select its owning harnesses).
pub fn source_edges(root: &Path) -> BTreeMap<String, Vec<String>> {
    let re =
        Regex::new(r#"(?:^|[\s;|&(])(?:source|\.)\s+["']?((?:\$\([^)]*\)|[\w./@{}$+-])*\.sh)"#)
            .expect("static regex");
    let mut edges: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for dir in ["tests", "scripts/tests", "skills"] {
        for rel in walk_glob_any(root, dir) {
            let text = match std::fs::read_to_string(root.join(&rel)) {
                Ok(t) => t,
                Err(_) => continue,
            };
            for line in text.lines() {
                if line.trim_start().starts_with('#') {
                    continue;
                }
                for m in re.captures_iter(line) {
                    let target = m.get(1).map(|g| g.as_str()).unwrap_or("");
                    if let Some(resolved) = resolve_source_ref(&rel, target, Some(root)) {
                        if resolved != rel {
                            edges.entry(resolved).or_default().push(rel.clone());
                        }
                    }
                }
            }
        }
    }
    edges
}

/// Every `.sh` under one repo-relative dir, at any depth.
fn walk_glob_any(root: &Path, dir: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack = vec![root.join(dir)];
    while let Some(base) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in read.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().map(|e| e == "sh").unwrap_or(false) {
                out.push(
                    path.strip_prefix(root)
                        .unwrap_or(path.as_path())
                        .to_string_lossy()
                        .to_string(),
                );
            }
        }
    }
    out
}

/// Sourced helpers among the candidates, so discovery excludes them.
pub fn sourced_helpers(
    root: &Path,
    candidates: &std::collections::BTreeSet<String>,
) -> std::collections::BTreeSet<String> {
    if candidates.is_empty() {
        return Default::default();
    }
    source_edges(root)
        .into_keys()
        .filter(|h| candidates.contains(h))
        .collect()
}

/// Resolve a `source`/`.` target to a repo-relative path.
///
/// Handles `./x.sh`, a bare `x.sh` beside the sourcer, and the variable forms
/// (`$SCRIPT_DIR/x.sh`, `$(dirname "$0")/x.sh`, `$SCRIPT_DIR/../lib/x.sh`,
/// `$REPO_ROOT/scripts/lib/x.sh`). A leading variable is ambiguous, so both
/// readings are built, `..` is normalized away, and the one that exists on
/// disk wins.
pub fn resolve_source_ref(sourcer_rel: &str, target: &str, root: Option<&Path>) -> Option<String> {
    let t = target.trim().trim_matches(|c| c == '\'' || c == '"');
    if !t.ends_with(".sh") {
        return None;
    }
    let base = sourcer_rel.rsplit_once('/').map(|x| x.0).unwrap_or("");
    let norm = |candidate: &str| -> Option<String> {
        let mut parts: Vec<&str> = Vec::new();
        for seg in candidate.split('/') {
            match seg {
                "." => {}
                ".." => {
                    if parts.pop().is_none() {
                        return None; // escaping the checkout is not a repo ref
                    }
                }
                s => parts.push(s),
            }
        }
        Some(parts.join("/"))
    };
    if t.contains('$') {
        // Drop the leading variable / command-substitution component; what
        // follows is the path, read either way.
        let rest = t
            .split_once('/')
            .map(|x| x.1)
            .unwrap_or_else(|| t.rsplit('/').next().unwrap_or(t));
        let cands: Vec<String> = [norm(&format!("{base}/{rest}")), norm(rest)]
            .into_iter()
            .flatten()
            .collect();
        if cands.is_empty() {
            return None;
        }
        if let Some(root) = root {
            for c in &cands {
                if root.join(c).exists() {
                    return Some(c.clone());
                }
            }
        }
        return cands.into_iter().next();
    }
    let t = t.strip_prefix("./").unwrap_or(t);
    if t.starts_with('/') {
        return None;
    }
    norm(&format!(
        "{}/{}",
        if base.is_empty() { "" } else { base },
        t
    ))
}

/// Measured seconds per step from scripts/ci/smoke-step-seconds.tsv.
pub fn smoke_step_weights(root: &Path) -> BTreeMap<String, f64> {
    let mut out = BTreeMap::new();
    let Ok(text) = std::fs::read_to_string(root.join("scripts/ci/smoke-step-seconds.tsv")) else {
        return out;
    };
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((seconds, name)) = line.split_once('\t') {
            if let Ok(v) = seconds.parse::<f64>() {
                out.insert(name.to_string(), v);
            }
        }
    }
    out
}

/// Assert only what the selected steps need (uv / python3 / cargo).
pub fn smoke_prereqs(selected_cmds: &[&str]) -> Option<String> {
    let blob = selected_cmds.join("\n");
    if blob.contains("uv ") || blob.ends_with("uv") || blob.contains("\nuv") {
        if !which("uv") {
            return Some("uv (install from https://docs.astral.sh/uv)".into());
        }
    }
    if blob.contains("python3") && !which("python3") {
        return Some("python3 (install Python 3)".into());
    }
    if blob.contains("cargo") && !which("cargo") {
        return Some("cargo (install the Rust toolchain)".into());
    }
    None
}

fn which(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|dir| dir.join(program).exists()))
        .unwrap_or(false)
}
