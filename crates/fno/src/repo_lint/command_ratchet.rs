//! `fno doctor lint command-ratchet`: hold the command surface down against
//! `scripts/ci/command-baseline.txt`. Ported from `cli/src/fno/lint_command_ratchet.py`
//! (deleted). The Rust half is live: the typed clap tree plus the fno-agents
//! binary's own command surface (`agents_surface`), defended row-by-row in both
//! directions - a command cannot be added or deleted silently. Recorded
//! departure, loud by design: the registry half cannot outlive the
//! Python runtime it imports, so that half is pinned by the committed
//! manifests now - every collapse-map row and every command-collapse-flags
//! row must appear in the baseline, and the count cap (menu-caps) keeps
//! holding the groups. A Python leaf is added or removed by editing the map,
//! the flags file and the baseline in the same PR, which is the
//! merge-conflict-as-review-moment shape the ratchet always had.

use super::agents_surface::{self, AgentsSurface};
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

/// The manifest contracts over the Python half: each collapse-map row's
/// projection (the leaf commandatim on a KEEP tier, else the group head) and
/// every collapse-flags row must appear in the baseline.
fn manifest_drift(root: &Path, baseline: &[String]) -> Vec<String> {
    let mut drift: Vec<String> = Vec::new();
    let baseline_paths: BTreeSet<String> = baseline.iter().map(|r| row_path(r)).collect();
    let map = std::fs::read_to_string(root.join(MAP_REL)).unwrap_or_default();
    for (n, line) in map.lines().enumerate() {
        if n == 0 || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let leaf = fields[0].trim();
        if leaf.is_empty() {
            continue;
        }
        // The agents rows are the native binary's old-spelling contract
        // (rust_half enumerates the fno-agents surface); no baseline line
        // backs them, the same exclusion the Python inventory test held.
        if leaf.split_whitespace().next() == Some("agents") {
            continue;
        }
        let projected = if fields[1].trim() == "KEEP" {
            leaf.to_string()
        } else {
            leaf.split_whitespace().next().unwrap_or(leaf).to_string()
        };
        if !baseline_paths.contains(&projected) {
            drift.push(format!(
                "command-ratchet: collapse-map row `{leaf}` projects to `{projected}`, \
             which is missing from {BASELINE_REL}. \
             Allocate the action in the map and regenerate the baseline in the same PR."
            ));
        }
    }
    let flags = std::fs::read_to_string(root.join(FLAGS_REL)).unwrap_or_default();
    let map_leaves: BTreeSet<String> = map
        .lines()
        .skip(1)
        .filter_map(|l| l.split('\t').next().map(str::trim).map(str::to_string))
        .collect();
    for line in flags.lines() {
        let row = line.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        // The flag rides a T1-dispatched action, so the contract is the map
        // allocation, not a baseline row: the baseline holds the group head.
        let action = row.split(" !").next().unwrap_or(row).trim().to_string();
        if !map_leaves.contains(&action) {
            drift.push(format!(
                "command-ratchet: collapse-flags row `{row}` has no allocated action in {MAP_REL}. \
             Allocate the action in the map and regenerate the baseline in the same PR."
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
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 2 {
            continue;
        }
        let leaf = fields[0].trim();
        // The agents rows are the native binary's old-spelling contract; the
        // live surface rows below carry that half, so the map rows are skipped
        // here exactly as manifest_drift skips them.
        if leaf.is_empty() || leaf.split_whitespace().next() == Some("agents") {
            continue;
        }
        // The baseline holds the projected surface (the leaf itself on KEEP,
        // the group head otherwise), the same projection manifest_drift
        // checks; the raw map rows are the registry, not the surface.
        let projected = if fields[1].trim() == "KEEP" {
            leaf.to_string()
        } else {
            leaf.split_whitespace().next().unwrap_or(leaf).to_string()
        };
        leaves.insert(projected);
    }
    for row in carried {
        leaves.insert(row_path(&row));
    }
    // The fno-agents half regenerates live from the client source, so a
    // surface change carries its rows in the same PR.
    let surface = match AgentsSurface::read(root) {
        Ok(surface) => surface,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    for row in surface.rows() {
        leaves.insert(row);
    }
    let mut rows: Vec<String> = leaves.into_iter().collect();
    rows.sort();
    // The hand-maintained header carries the load-bearing regeneration
    // instructions; a regen preserves it and replaces only the row block. A
    // file with no comment block (first generation) gets the template.
    let path = root.join(BASELINE_REL);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let comments: Vec<&str> = existing
        .lines()
        .take_while(|l| l.starts_with('#'))
        .collect();
    let preserved = comments.join("\n");
    let header: &str = if comments.is_empty() {
        "# Known command surface held by the CI ratchet.\n\
         # The Rust half (typed clap tree + fno-agents) is enumerated live; the\n\
         # registry rows are pinned by this file plus the collapse map and\n\
         # the collapse-flags file, all edited in the PR that changes a leaf.\n\
         # A deliberate addition carries a PR-body line:  command-exception: <rationale>\n"
    } else {
        preserved.as_str()
    };
    let body = format!("{header}\n{}\n", rows.join("\n"));
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
        return update_baseline(&inv.root);
    }
    let rust = match rust_half(&inv.root) {
        Ok(r) => r,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let baseline = match baseline_rows(&inv.root) {
        Ok(b) => b,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let drift = manifest_drift(&inv.root, &baseline);
    let baseline_paths: BTreeSet<String> = baseline.iter().map(|r| row_path(r)).collect();
    let mut added: Vec<String> = Vec::new();
    for leaf in &rust {
        if !baseline_paths.contains(leaf) {
            added.push(leaf.clone());
        }
    }
    added.sort();
    // The removed-side ratchet lives in the freshness unit test
    // (inventory_artifact_is_fresh pins the committed tree to the typed
    // tree); the manifest contracts hold the Python-forward roots, which
    // live_roots() legitimately declares without a typed arm.
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
    // The fno-agents half: the binary's own command surface, the two readings
    // cross-checked against the committed record of the deliberate hidden
    // commands, and the baseline rows defended in both directions.
    let mut agents_ok_note = String::new();
    match AgentsSurface::read(&inv.root) {
        Ok(surface) => {
            let hidden = surface.hidden();
            let live_rows = surface.rows();
            let (added_agents, removed_agents) =
                agents_surface::baseline_drift(&live_rows, &baseline);
            if !added_agents.is_empty() {
                failed = true;
                lines.push(format!(
                    "command-ratchet: FAIL - the fno-agents surface drifted from {BASELINE_REL}"
                ));
                lines.push(format!(
                    "  Added (dispatched, no baseline row): {}",
                    added_agents.join(", ")
                ));
                lines.push(
                    "    A command cannot be added silently. Regenerate the baseline in the same PR \
                     and add a PR-body line:  command-exception: <rationale>"
                        .to_string(),
                );
            }
            if !removed_agents.is_empty() {
                failed = true;
                lines.push(format!(
                    "  Removed (baseline row, no dispatch): {}",
                    removed_agents.join(", ")
                ));
                lines.push(
                    "    A command cannot be deleted silently either: the row goes in the same PR \
                     that removes it. If the removal is right, delete the rows and regenerate."
                        .to_string(),
                );
            }
            match std::fs::read_to_string(inv.root.join(agents_surface::HIDDEN_REL)) {
                Ok(text) => match agents_surface::hidden_rows(&text) {
                    Ok(rows) => {
                        for failure in surface.drift(&rows) {
                            failed = true;
                            lines.push(failure);
                        }
                    }
                    Err(line) => {
                        failed = true;
                        lines.push(line);
                    }
                },
                Err(e) => {
                    failed = true;
                    lines.push(format!(
                        "agents-surface: cannot read {}: {e}",
                        agents_surface::HIDDEN_REL
                    ));
                }
            }
            agents_ok_note = format!(
                "; fno-agents surface: {} row(s), {} hidden command(s) recorded",
                live_rows.len(),
                hidden.len()
            );
        }
        Err(message) => {
            failed = true;
            lines.push(message);
        }
    }
    if failed {
        for line in &lines {
            eprintln!("{line}");
        }
        return 1;
    }
    let n_hidden = baseline.iter().filter(|r| r.contains(" !")).count();
    println!(
        "command-ratchet: ok ({} baseline leaves, {n_hidden} hidden-option row(s){}; \
         the Rust half is live against the typed tree; the registry half is \
         pinned by the committed manifests now)",
        baseline.len(),
        agents_ok_note
    );
    0
}
