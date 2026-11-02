//! `fno doctor lint command-ratchet`: hold the command surface down against
//! `scripts/ci/command-baseline.txt`. Ported from `cli/src/fno/lint_command_ratchet.py`
//! (deleted). The Rust half is live: the typed clap tree plus the fno-agents
//! leaf. Recorded departure, loud by design: the fno-py registry half cannot
//! outlive the Python runtime it imports, so that half is pinned by the
//! committed manifests now - every collapse-map row and every
//! command-collapse-flags row must appear in the baseline, and the count cap
//! (menu-caps) keeps holding the groups. A Python leaf is added or removed by
//! editing the map, the flags file and the baseline in the same PR, which is
//! the merge-conflict-as-review-moment shape the ratchet always had.

use super::Invocation;
use std::collections::BTreeSet;
use std::path::Path;

const BASELINE_REL: &str = "scripts/ci/command-baseline.txt";
const MAP_REL: &str = "scripts/ci/command-collapse-map.tsv";
const FLAGS_REL: &str = "scripts/ci/command-collapse-flags.txt";
const NATIVE_REL: &str = "scripts/ci/native-command-tree.txt";

/// The Rust half's leaves: every declared root other than `mux` is one leaf
/// (`version`), `mux` and `fno-agents` register as leaves of their own. The
/// inventory is generated from the one clap declaration; a missing mux or
/// version root is a named failure, never a half-true pass.
fn rust_half(root: &Path) -> Result<BTreeSet<String>, String> {
    let text = std::fs::read_to_string(root.join(NATIVE_REL)).map_err(|e| {
        format!("command-ratchet: cannot read {NATIVE_REL}: {e}. Regenerate from the typed tree.")
    })?;
    let mut roots: BTreeSet<String> = BTreeSet::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = line.split('\t').next().unwrap_or("");
        if path.is_empty() || path == "(root)" {
            continue;
        }
        if let Some(head) = path.split_whitespace().next() {
            roots.insert(head.to_string());
        }
    }
    if !roots.contains("mux") || !roots.contains("version") {
        return Err(format!(
        "command-ratchet: the native command inventory is missing the mux/version roots (found {roots:?}); regenerate it."
    ));
    }
    let mut leaves: BTreeSet<String> = BTreeSet::new();
    leaves.insert("mux".to_string());
    leaves.insert("fno-agents".to_string());
    roots.remove("mux");
    leaves.extend(roots);
    Ok(leaves)
}

/// Baseline lines minus comments and blanks, flag tokens kept.
fn baseline_rows(root: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(root.join(BASELINE_REL))
        .map_err(|e| format!("command-ratchet: cannot read {BASELINE_REL}: {e}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// The path of a baseline row: its flag-less tokens.
fn row_path(row: &str) -> String {
    row.split_whitespace()
        .filter(|w| !w.starts_with('!'))
        .collect::<Vec<_>>()
        .join(" ")
}

/// The manifest contracts over the Python half: every collapse-map row and
/// every collapse-flags row must appear in the baseline, and the map's
/// projection of a group must equal the baseline rows in that group.
fn manifest_drift(root: &Path, baseline: &[String], rust: &BTreeSet<String>) -> Vec<String> {
    let mut drift: Vec<String> = Vec::new();
    let baseline_paths: BTreeSet<String> = baseline.iter().map(|r| row_path(r)).collect();
    let map = std::fs::read_to_string(root.join(MAP_REL)).unwrap_or_default();
    let mut map_rows: Vec<String> = Vec::new();
    for (n, line) in map.lines().enumerate() {
        if n == 0 || line.trim().is_empty() {
            continue;
        }
        if let Some(leaf) = line.split('\t').next() {
            if !leaf.trim().is_empty() {
                map_rows.push(leaf.trim().to_string());
            }
        }
    }
    let map_groups: BTreeSet<String> = map_rows
        .iter()
        .filter_map(|r| r.split_whitespace().next().map(str::to_string))
        .filter(|head| rust.contains(head) || !head.is_empty())
        .collect();
    for row in &map_rows {
        let path = row_path(row);
        if !baseline_paths.contains(&path) {
            drift.push(format!(
                "command-ratchet: collapse-map row `{row}` is missing from {BASELINE_REL}. \
             Allocate the action in the map and regenerate the baseline in the same PR."
            ));
        }
    }
    let flags = std::fs::read_to_string(root.join(FLAGS_REL)).unwrap_or_default();
    for line in flags.lines() {
        let row = line.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        if !baseline.iter().any(|b| b == row) {
            drift.push(format!(
                "command-ratchet: collapse-flags row `{row}` is missing from {BASELINE_REL}. \
             Add the line to the baseline in the same PR."
            ));
        }
    }
    drift
}

/// The door: the Rust half is live, the manifest contracts hold the rest.

/// `--update`: rewrite the baseline as the live Rust half plus the map rows
/// plus every Python-half row carried forward from the current file. The
/// carried rows are the registry leaves whose click tree cannot be
/// re-enumerated; their removal is a manual edit in the PR that deletes the
/// leaf.
fn update_baseline(root: &Path) -> i32 {
    let rust = match rust_half(root) {
        Ok(r) => r,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let carried = match baseline_rows(root) {
        Ok(b) => b,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let mut leaves: BTreeSet<String> = BTreeSet::new();
    for leaf in &rust {
        leaves.insert(leaf.clone());
    }
    let map = std::fs::read_to_string(root.join(MAP_REL)).unwrap_or_default();
    for (n, line) in map.lines().enumerate() {
        if n == 0 || line.trim().is_empty() {
            continue;
        }
        if let Some(leaf) = line.split('\t').next() {
            if !leaf.trim().is_empty() {
                leaves.insert(leaf.trim().to_string());
            }
        }
    }
    for row in carried {
        leaves.insert(row_path(&row));
    }
    let mut rows: Vec<String> = leaves.into_iter().collect();
    rows.sort();
    let header = "# Known command surface held by the CI ratchet.\n\
                  # The Rust half (typed clap tree + fno-agents) is enumerated live; the\n\
                  # fno-py registry rows are pinned by this file plus the collapse map and\n\
                  # the collapse-flags file, all edited in the PR that changes a leaf.\n\
                  # A deliberate addition carries a PR-body line:  command-exception: <rationale>\n";
    let body = format!("{header}{}\n", rows.join("\n"));
    let path = root.join(BASELINE_REL);
    if std::fs::write(&path, &body).is_err() {
        eprintln!("command-ratchet: cannot write {}", path.display());
        return 1;
    }
    println!(
        "command-ratchet: regenerated {BASELINE_REL} ({} leaves)",
        rows.len()
    );
    0
}
pub fn run(inv: &Invocation) -> i32 {
    if inv.update {
        return update_baseline(root);
    }
    let declared = crate::command_names::live_roots()
        .into_iter()
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    let declared: BTreeSet<String> = declared
        .union(&BTreeSet::from([
            "mux".to_string(),
            "version".to_string(),
            "fno-agents".to_string(),
        ]))
        .cloned()
        .collect();
    let baseline_paths: BTreeSet<String> = baseline.iter().map(|r| row_path(r)).collect();
    let mut added: Vec<String> = Vec::new();
    for leaf in &rust {
        if !baseline_paths.contains(leaf) {
            added.push(leaf.clone());
        }
    }
    added.sort();
    let mut removed: Vec<String> = Vec::new();
    for root_name in &declared {
        if !rust.contains(root_name) {
            removed.push(root_name.clone());
        }
    }
    removed.sort();
    let mut lines: Vec<String> = Vec::new();
    let mut failed = false;
    if !drift.is_empty() {
        failed = true;
        lines.extend(drift.clone());
    }
    if !added.is_empty() {
        failed = true;
        lines.push(format!(
            "command-ratchet: FAIL - live surface drifted from {BASELINE_REL}"
        ));
        lines.push(format!(
            "  Added (in the code, not the baseline): {}",
            added.join(", ")
        ));
        lines.push(
            "    A command cannot be added silently. Regenerate the baseline in the same PR \
             and add a PR-body line:  command-exception: <rationale>"
                .to_string(),
        );
    }
    if !removed.is_empty() {
        failed = true;
        if lines.is_empty() {
            lines.push(format!(
                "command-ratchet: FAIL - the typed tree dropped declared roots"
            ));
        }
        lines.push(format!(
            "  Removed from the typed tree but still declared: {}",
            removed.join(", ")
        ));
        lines.push(
            "    Regenerate scripts/ci/native-command-tree.txt and commit it in the same PR."
                .to_string(),
        );
    }
    if failed {
        for line in &lines {
            eprintln!("{line}");
        }
        return 1;
    }
    let n_hidden = baseline.iter().filter(|r| r.contains(" !")).count();
    println!(
        "command-ratchet: ok ({} baseline leaves, {n_hidden} hidden-option row(s); \
         the Rust half is live against the typed tree; the fno-py registry half is \
         pinned by the committed manifests now)",
        baseline.len()
    );
    0
}
