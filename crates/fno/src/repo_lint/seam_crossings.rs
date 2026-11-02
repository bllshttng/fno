//! The Rust/Python seam-crossing ratchet (`fno doctor lint seam-crossings`).
//! Ported from `cli/src/fno/lint_seam_crossings.py` (deleted): the regexes,
//! the nearest-preceding-fn attribution, the multiset baseline keyed on
//! (rule, path, content), and the refusal texts are the Python leg's, so the
//! committed baseline regenerates byte-identically.

use super::Invocation;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const BASELINE_RELPATH: &str = "scripts/ci/seam-crossings-baseline.txt";

/// The one Python file allowed to exec the Rust runtime binary by literal
/// name; the pydoor rule reads the Python tree it guards.
const PY_DOOR_FILE: &str = "cli/src/fno/rust_binary.py";

/// A porcelain launch: `Command::new("fno")` in any qualification.
const CROSSING_LITERAL: &str = r#"\bCommand::new\(\s*"fno"\s*\)"#;

/// The porcelain env seam and the join construction, matched in a
/// three-line forward window and attributed to the nearest preceding fn.
const PORCELAIN_ENV: &str = r#"\b(?:var|var_os)\(\s*"(?:FNO_BIN|FNO_LOOPCHECK_FNO_BIN)"\s*,?\s*\)"#;
const PORCELAIN_JOIN: &str = r#"join\(\s*"fno"\s*,?\s*\)"#;
const FN_DEF: &str = r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?fn\s+([A-Za-z_][A-Za-z0-9_]*)";

/// The pydoor shapes: the argv[0]-position literal on any line, and the
/// direct string-argument forms next to an exec call on the same line.
const PY_DOOR_ARGV0: &str = r#"\[\s*["']fno-agents["']"#;
const PY_DOOR_CALL: &str = r#"\b(?:subprocess|os)\.(?:Popen|run|call|check_output|check_call|getoutput|getstatusoutput|exec[lv]p?e?|spawn[lv]p?e?)\b|\basyncio\.create_subprocess_(?:exec|shell)\b"#;
const PY_DOOR_DIRECT: &str = r#"\(\s*["']fno-agents(?:["'\s]|$)"#;
const PY_DOOR_BARE_DIRECT: &str = r#"\b(?:Popen|run|call|check_output|check_call|exec[lv]p?e?|spawn[lv]p?e?)\(\s*["']fno-agents(?:["'\s]|$)"#;

/// Matching keys on (rule, path, content) as a multiset; the line number is
/// not recorded.
type Multiset = BTreeMap<(String, String, String), usize>;

/// One rule's sites as (rel, line_no, stripped line text).
pub type SiteRows = Vec<(String, usize, String)>;

/// The three rule surfaces, uncached.
pub struct Sites {
    pub crossing: SiteRows,
    pub resolver: SiteRows,
    pub pydoor: SiteRows,
}

fn is_comment(line: &str) -> bool {
    let stripped = line.trim_start();
    stripped.starts_with("//")
        || stripped.starts_with('#')
        || stripped.starts_with('*')
        || stripped.starts_with("/*")
}

/// Production `.rs` files under crates/, tests dirs excluded.
fn rust_scan_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("crates")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                stack.push(path);
            } else if name.ends_with(".rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let parts: Vec<&str> = rel.split('/').collect();
                if parts.contains(&"tests") || parts.contains(&"target") {
                    continue;
                }
                out.push((rel, path));
            }
        }
    }
    out.sort();
    out
}

/// Production Python under cli/src/fno, the door file and tests excluded.
fn py_scan_files(root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    let mut stack = vec![root.join("cli/src/fno")];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                stack.push(path);
            } else if name.ends_with(".py") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                let parts: Vec<&str> = rel.split('/').collect();
                let stem = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned();
                if parts.contains(&"tests") || stem.starts_with("test_") || rel == PY_DOOR_FILE {
                    continue;
                }
                out.push((rel, path));
            }
        }
    }
    out.sort();
    out
}

/// 1-indexed line numbers inside an inline `#[cfg(test)] mod` block. Only a
/// `mod` boundary counts and the attribute must open its own line; a
/// declaration (`#[cfg(test)] pub mod frame_html;`) excludes nothing.
pub fn rust_cfg_test_lines(lines: &[&str]) -> std::collections::BTreeSet<usize> {
    let mut inside = std::collections::BTreeSet::new();
    let mut i = 0usize;
    while i < lines.len() {
        if lines[i].trim() != "#[cfg(test)]" {
            i += 1;
            continue;
        }
        let head = lines.get(i + 1).map(|l| l.trim()).unwrap_or("");
        let is_mod = head.starts_with("mod ")
            || head.starts_with("pub mod ")
            || head.starts_with("pub(crate) mod ");
        if !is_mod || head.ends_with(';') {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && !lines[j].contains('{') {
            j += 1;
        }
        if j >= lines.len() {
            break;
        }
        let mut depth = 0i64;
        while j < lines.len() {
            depth += lines[j].matches('{').count() as i64;
            depth -= lines[j].matches('}').count() as i64;
            if depth <= 0 {
                break;
            }
            j += 1;
        }
        let last = j.min(lines.len() - 1);
        for k in i..=last {
            inside.insert(k + 1);
        }
        i = j + 1;
    }
    inside
}

/// Resolver functions as (rel, line_no, def_line, fn_name): a function whose
/// body reads a porcelain env key or joins the porcelain name, attributed to
/// the fn definition nearest above - no brace counting. The name is for the
/// failure message only; detection is by shape.
fn find_resolvers(root: &Path) -> Vec<(String, usize, String, String)> {
    let env_re = regex::Regex::new(PORCELAIN_ENV).expect("static regex");
    let join_re = regex::Regex::new(PORCELAIN_JOIN).expect("static regex");
    let fn_re = regex::Regex::new(FN_DEF).expect("static regex");
    let mut hits = Vec::new();
    for (rel, path) in rust_scan_files(root) {
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let lines: Vec<&str> = body.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let skip = rust_cfg_test_lines(&lines);
        let mut fns: Vec<(usize, String, String)> = Vec::new();
        let mut attributed: BTreeMap<usize, (String, String)> = BTreeMap::new();
        for (i, line) in lines.iter().enumerate() {
            if skip.contains(&(i + 1)) || is_comment(line) {
                continue;
            }
            if let Some(caps) = fn_re.captures(line) {
                fns.push((i, line.trim().to_string(), caps[1].to_string()));
                continue;
            }
            if !line.contains('(') {
                continue;
            }
            let window = lines[i..(i + 3).min(lines.len())].join("\n");
            if env_re.is_match(&window) || join_re.is_match(&window) {
                if let Some((idx, def_line, name)) = fns.last() {
                    attributed.insert(*idx, (def_line.clone(), name.clone()));
                }
            }
        }
        for (idx, (def_line, name)) in attributed {
            hits.push((rel.clone(), idx + 1, def_line, name));
        }
    }
    hits
}

/// The baselined helper names, from the resolver definitions: every name
/// ending in `fno_bin`, shortest first.
fn helper_names(resolvers: &[(String, usize, String, String)]) -> Vec<String> {
    let mut names: Vec<String> = resolvers
        .iter()
        .filter(|(.., name)| name.ends_with("fno_bin"))
        .map(|(.., name)| name.clone())
        .collect();
    names.sort_by_key(|n| n.len());
    names
}

/// Crossing sites: the literal launch, or a call of a baselined helper name.
fn find_crossings(root: &Path, helper_res: &[regex::Regex]) -> SiteRows {
    let literal_re = regex::Regex::new(CROSSING_LITERAL).expect("static regex");
    let mut hits: SiteRows = Vec::new();
    for (rel, path) in rust_scan_files(root) {
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let lines: Vec<&str> = body.lines().collect();
        if lines.is_empty() {
            continue;
        }
        let skip = rust_cfg_test_lines(&lines);
        for (i, line) in lines.iter().enumerate() {
            if skip.contains(&(i + 1)) || is_comment(line) {
                continue;
            }
            if literal_re.is_match(line) || helper_res.iter().any(|re| re.is_match(line)) {
                hits.push((rel.clone(), i + 1, line.trim().to_string()));
            }
        }
    }
    hits
}

/// Literal `fno-agents` execs outside the single door, both shapes.
fn find_pydoor(root: &Path) -> SiteRows {
    let argv0_re = regex::Regex::new(PY_DOOR_ARGV0).expect("static regex");
    let call_re = regex::Regex::new(PY_DOOR_CALL).expect("static regex");
    let direct_re = regex::Regex::new(PY_DOOR_DIRECT).expect("static regex");
    let bare_re = regex::Regex::new(PY_DOOR_BARE_DIRECT).expect("static regex");
    let mut hits: SiteRows = Vec::new();
    for (rel, path) in py_scan_files(root) {
        let body = match std::fs::read_to_string(&path) {
            Ok(b) => b,
            Err(_) => continue,
        };
        let lines: Vec<&str> = body.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if is_comment(line) {
                continue;
            }
            let argv0 = argv0_re.is_match(line);
            let bare = bare_re.is_match(line);
            let direct = call_re.is_match(line) && direct_re.is_match(line);
            if argv0 || bare || direct {
                hits.push((rel.clone(), i + 1, line.trim().to_string()));
            }
        }
    }
    hits
}

/// The three rule surfaces, uncached.
pub fn enumerate_sites(root: &Path) -> Sites {
    let resolvers = find_resolvers(root);
    let names = helper_names(&resolvers);
    let helper_res: Vec<regex::Regex> = names
        .iter()
        .map(|name| {
            regex::Regex::new(&format!(r"(^|[^A-Za-z0-9_]){}\s*\(", regex::escape(name)))
                .expect("static regex")
        })
        .collect();
    Sites {
        crossing: find_crossings(root, helper_res.as_slice()),
        resolver: resolvers
            .into_iter()
            .map(|(rel, line, def_line, _)| (rel, line, def_line))
            .collect(),
        pydoor: find_pydoor(root),
    }
}

/// The baseline file's header, verbatim from the Python leg so the committed
/// file regenerates byte-identically.
fn baseline_header() -> String {
    String::from(
        "# Rust/Python seam ratchet, regenerated by\n\
         # `fno doctor lint seam-crossings --update`.\n\
         # Format: rule<TAB>path<TAB>content. Matching keys on (rule, path, content)\n\
         # as a multiset; the line number is not recorded, so the file changes only when a\n\
         # crossing is added or removed. pydoor takes NO baseline: cli/src/fno/rust_binary.py\n\
         # stays the only Python file that execs fno-agents by literal name.\n",
    )
}

/// Parse the baseline into a multiset of (rule, path, content).
fn read_baseline(text: &str) -> Multiset {
    let mut entries = Multiset::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = stripped.splitn(3, '\t').collect();
        if fields.len() < 3 || fields[0].is_empty() || fields[1].is_empty() {
            continue;
        }
        *entries
            .entry((
                fields[0].to_string(),
                fields[1].to_string(),
                fields[2].to_string(),
            ))
            .or_insert(0) += 1;
    }
    entries
}

/// The live multiset over the three rule surfaces, each named by its rule.
fn live_multiset(sites: &Sites) -> (Multiset, Vec<(String, usize, usize)>) {
    let mut live = Multiset::new();
    let mut counts: Vec<(String, usize, usize)> = Vec::new();
    for (rule, rows) in [
        ("crossing", &sites.crossing),
        ("resolver", &sites.resolver),
        ("pydoor", &sites.pydoor),
    ] {
        for (rel, _line, text) in rows {
            *live
                .entry((rule.to_string(), rel.clone(), text.clone()))
                .or_insert(0) += 1;
        }
        counts.push((rule.to_string(), rows.len(), 0));
    }
    (live, counts)
}

/// Compare live against the baseline multiset: the new-site messages, the
/// stale-site messages, and per-rule (measured, baselined) counts.
fn verify(
    sites: &Sites,
    baseline: &Multiset,
) -> (Vec<String>, Vec<String>, Vec<(String, usize, usize)>) {
    let (live, mut counts) = live_multiset(sites);
    for (rule, _measured, baselined_slot) in counts.iter_mut() {
        *baselined_slot = baseline
            .iter()
            .filter(|((r, _p, _t), _)| r == rule)
            .map(|(_, n)| n)
            .sum();
    }
    let mut new_msgs = Vec::new();
    for ((rule, rel, text), count) in &live {
        let baselined = baseline
            .get(&(rule.clone(), rel.clone(), text.clone()))
            .copied()
            .unwrap_or(0);
        let extra = count.saturating_sub(baselined);
        if extra > 0 {
            if rule == "pydoor" {
                new_msgs.push(format!(
                    "{rule}: {rel}: {text}\n    cli/src/fno/rust_binary.py is the Python side's ONE door to the Rust runtime.\n    Route this exec through it. The pydoor rule takes no baseline."
                ));
            } else {
                new_msgs.push(format!(
                    "{rule}: {rel}: unbaselined site: {text}\n    Add it to scripts/ci/seam-crossings-baseline.txt only with the node that owns draining it,\n    or fix the site in this change."
                ));
            }
        }
    }
    let mut stale_msgs = Vec::new();
    for ((rule, rel, text), count) in baseline {
        let live_count = live
            .get(&(rule.clone(), rel.clone(), text.clone()))
            .copied()
            .unwrap_or(0);
        let extra = count.saturating_sub(live_count);
        if extra > 0 {
            stale_msgs.push(format!(
                "{rule}: {rel}: baselined but no longer matches: {text}\n    Delete this line from scripts/ci/seam-crossings-baseline.txt. A drained exemption left in the baseline is a permanent one."
            ));
        }
    }
    new_msgs.sort();
    stale_msgs.sort();
    (new_msgs, stale_msgs, counts)
}

/// The door: `--update` regenerates the committed baseline; otherwise diff
/// live sites against it. Exit 0 clean, 1 drift, 3 unreadable baseline.
pub fn run(inv: &Invocation) -> i32 {
    let root = &inv.root;
    if inv.update {
        let rendered = build_baseline(root);
        let path = root.join(BASELINE_RELPATH);
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if std::fs::write(&path, &rendered).is_err() {
            eprintln!("seam-crossings: cannot write {}", path.display());
            return 3;
        }
        let rows = rendered
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
            .count();
        println!("seam-crossings: regenerated {BASELINE_RELPATH} ({rows} site(s))");
        return 0;
    }
    let baseline_text = std::fs::read_to_string(root.join(BASELINE_RELPATH));
    let baseline = read_baseline(&baseline_text.unwrap_or_default());
    let (new_msgs, stale_msgs, counts) = verify(&enumerate_sites(root), &baseline);
    for msg in new_msgs.iter().chain(stale_msgs.iter()) {
        eprintln!("{msg}");
    }
    if !new_msgs.is_empty() || !stale_msgs.is_empty() {
        let joined = counts
            .iter()
            .map(|(rule, m, b)| format!("{rule} {m}/{b}"))
            .collect::<Vec<_>>()
            .join(" ");
        eprintln!("seam-crossings: FAIL {joined}");
        return 1;
    }
    let joined = counts
        .iter()
        .map(|(rule, m, b)| format!("{rule} {m} measured / {b} baselined"))
        .collect::<Vec<_>>()
        .join(", ");
    println!("seam-crossings: ok ({joined})");
    0
}

/// The full baseline file: header plus sorted data lines, source order kept.
fn build_baseline(root: &Path) -> String {
    let sites = enumerate_sites(root);
    let mut out: Vec<String> = Vec::new();
    for (rule, rows) in [
        ("crossing", &sites.crossing),
        ("resolver", &sites.resolver),
        ("pydoor", &sites.pydoor),
    ] {
        let mut sorted: Vec<&(String, usize, String)> = rows.iter().collect();
        sorted.sort();
        for (rel, _line, text) in sorted {
            out.push(format!("{rule}\t{rel}\t{text}"));
        }
    }
    let mut rendered = baseline_header();
    rendered.push_str(&out.join("\n"));
    if !out.is_empty() {
        rendered.push('\n');
    }
    rendered
}
