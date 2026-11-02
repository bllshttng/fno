//! Synonym frontmatter-key collapse, byte-preserving, the port of
//! `cli/src/fno/plan/migrate_keys.py`. Line-level rename over a plans dir:
//! one canonical key per axis, no YAML round-trip, body byte-intact.

use regex::Regex;
use std::path::Path;
use std::sync::OnceLock;

use super::codec::atomic_write;

/// Legacy synonym -> canonical key. `deliverable_type` is intentionally absent
/// (a different axis with live readers, not a synonym of `type`).
pub const RENAME: &[(&str, &str)] = &[
    ("graph_node_id", "node"),
    ("created_at", "created"),
    ("depends_on", "blocked_by"),
    ("kind", "type"),
];

fn front_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    // Non-greedy so the FIRST frontmatter block wins even if the body carries
    // a `---` rule.
    CELL.get_or_init(|| Regex::new(r"\A(---\n)(?s)(.*?)\n---(.*)\z").unwrap())
}

fn key_re() -> &'static Regex {
    static CELL: OnceLock<Regex> = OnceLock::new();
    CELL.get_or_init(|| Regex::new(r"^([ \t]*)([A-Za-z0-9_]+)([ \t]*:)(.*)$").unwrap())
}

fn rename_target(key: &str) -> Option<&'static str> {
    RENAME.iter().find(|(k, _)| *k == key).map(|(_, t)| *t)
}

/// Strip surrounding whitespace, then one pair of matching quotes.
fn scalar(raw: &str) -> &str {
    let raw = raw.trim();
    if raw.len() >= 2 {
        let b = raw.as_bytes();
        if b[0] == b[raw.len() - 1] && (b[0] == b'\'' || b[0] == b'"') {
            return &raw[1..raw.len() - 1];
        }
    }
    raw
}

/// Top-level keys of a frontmatter block (indented lines are nested, not keys).
fn top_level_keys(fm: &str) -> std::collections::HashSet<String> {
    let mut keys = std::collections::HashSet::new();
    for line in fm.lines() {
        if let Some(c) = key_re().captures(line) {
            if c[1].is_empty() {
                keys.insert(c[2].to_string());
            }
        }
    }
    keys
}

/// Return (new_text, notes). new_text is None when nothing changed.
///
/// notes describe every rename / drop / kept-for-review decision, so the CLI
/// receipt is a faithful account of what the migration did to each file.
pub fn migrate_text(text: &str) -> (Option<String>, Vec<String>) {
    let m = match front_re().captures(text) {
        Some(m) => m,
        None => return (None, Vec::new()),
    };
    let fm = &m[2];
    let mut present = top_level_keys(fm);

    // Node identity resolves from `node` OR its legacy `graph_node_id` (they
    // collapse to the same key), so a file carrying graph_node_id + claims
    // drops the duplicate claims in the SAME pass as the rename: single-pass
    // idempotent.
    let mut node_val: Option<String> = None;
    for wanted in ["node", "graph_node_id"] {
        for line in fm.lines() {
            if let Some(c) = key_re().captures(line) {
                if c[1].is_empty() && &c[2] == wanted {
                    node_val = Some(scalar(&c[4]).to_string());
                    break;
                }
            }
        }
        if node_val.is_some() {
            break;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut notes: Vec<String> = Vec::new();
    let mut changed = false;
    for line in fm.lines() {
        let km = match key_re().captures(line) {
            Some(c) if c[1].is_empty() => c,
            // Indented or non-key lines pass through untouched.
            _ => {
                out.push(line.to_string());
                continue;
            }
        };
        let key = &km[2];
        let target = rename_target(key);
        if let Some(target) = target {
            if present.contains(target) {
                notes.push(format!("kept {key} ({target} also present) - review"));
                out.push(line.to_string());
            } else {
                out.push(format!("{}{}{}{}", &km[1], target, &km[3], &km[4]));
                present.remove(key);
                present.insert(target.to_string());
                changed = true;
                notes.push(format!("{key} -> {target}"));
            }
            continue;
        }
        if key == "claims" {
            let cval = scalar(&km[4]);
            if let Some(node) = &node_val {
                if cval == node {
                    changed = true;
                    notes.push("dropped claims (== node)".to_string());
                    continue; // drop the duplicate line
                }
                notes.push("kept claims (differs from node) - review".to_string());
            }
            out.push(line.to_string());
            continue;
        }
        out.push(line.to_string());
    }

    if !changed {
        return (None, notes);
    }
    let text = format!("---\n{}\n---{}", out.join("\n"), &m[3]);
    (Some(text), notes)
}

/// The scan result the sweep returns.
#[derive(Default)]
pub struct MigrateResult {
    pub migrated: usize,
    pub skipped: usize,
    /// Files with a kept legacy key needing a human decision.
    pub review: usize,
    /// (path, notes) per migrated file.
    pub changes: Vec<(String, Vec<String>)>,
    /// (path, review notes) per file a human must reconcile.
    pub review_files: Vec<(String, Vec<String>)>,
    pub warnings: Vec<String>,
}

impl MigrateResult {
    pub fn summary(&self) -> String {
        format!(
            "{} migrated, {} need review, {} skipped",
            self.migrated, self.review, self.skipped
        )
    }
}

/// Scan every `*.md` in *plans_dir*, collapse synonym keys byte-preservingly.
pub fn migrate(plans_dir: &Path, apply: bool) -> MigrateResult {
    let mut res = MigrateResult::default();
    let entries = match std::fs::read_dir(plans_dir) {
        Ok(e) => e,
        Err(_) => {
            res.warnings
                .push(format!("plans dir not found: {}", plans_dir.display()));
            return res;
        }
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "md").unwrap_or(false))
        .collect();
    paths.sort();
    for path in paths {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                res.skipped += 1;
                res.warnings
                    .push(format!("skip (unreadable): {}: {e}", path.display()));
                continue;
            }
        };
        let (new_text, notes) = migrate_text(&text);
        let review_notes: Vec<String> = notes
            .iter()
            .filter(|n| n.contains("review"))
            .cloned()
            .collect();
        if !review_notes.is_empty() {
            // Record the path so the receipt can name which files a human must
            // reconcile - a bare count is useless without the list.
            res.review += 1;
            res.review_files
                .push((path.display().to_string(), review_notes));
        }
        let new_text = match new_text {
            Some(t) => t,
            None => {
                res.skipped += 1;
                continue;
            }
        };
        res.migrated += 1;
        res.changes.push((path.display().to_string(), notes));
        if apply {
            // Atomic (tmp + rename): an interrupted migration never leaves a
            // plan half-written, so a re-run stays idempotent and recoverable.
            let _ = atomic_write(&path, &new_text);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(fm_lines: &str) -> String {
        format!("---\n{fm_lines}\n---\n\n# Title\n\nbody text\n")
    }

    #[test]
    fn renames_all_legacy_keys_body_intact() {
        let text =
            &plan("graph_node_id: x-1\ncreated_at: 2026-07-08\ndepends_on: [x-2]\nkind: feature");
        let (out, notes) = migrate_text(text);
        let out = out.unwrap();
        assert!(out.contains("\nnode: x-1\n"), "{out}");
        assert!(out.contains("\ncreated: 2026-07-08\n"), "{out}");
        assert!(out.contains("\nblocked_by: [x-2]\n"), "{out}");
        assert!(out.contains("\ntype: feature\n"), "{out}");
        assert!(!out.contains("graph_node_id") && !out.contains("created_at"));
        assert!(out.ends_with("\n\n# Title\n\nbody text\n")); // body byte-intact
        assert_eq!(notes.len(), 4);
    }

    #[test]
    fn keeps_claims_that_differs_from_node() {
        let text = &plan("node: x-1\nclaims: x-2\nstatus: done");
        let (out, notes) = migrate_text(text);
        // claims != node -> preserved, flagged for review, no rename churn -> no write.
        assert!(out.is_none());
        assert!(notes.iter().any(|n| n.contains("review")), "{notes:?}");
    }

    #[test]
    fn drops_claims_that_equals_node_in_the_same_pass_as_the_rename() {
        let text = &plan("graph_node_id: x-1\nclaims: x-1\nkind: feature");
        let (out, notes) = migrate_text(text);
        let out = out.unwrap();
        assert!(!out.contains("claims"), "{out}");
        assert!(!notes.iter().any(|n| n.contains("review")));
    }

    #[test]
    fn no_rename_when_canonical_target_present() {
        // kind + type both present -> keep kind (never create a duplicate type key).
        let text = &plan("type: feature\nkind: task\nnode: x-1");
        let (out, notes) = migrate_text(text);
        assert!(out.is_none()); // nothing safely renamable
        assert!(
            notes.iter().any(|n| n.contains("kind (type also present)")),
            "{notes:?}"
        );
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fno-plan-doc-migrate-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sweep_apply_writes_and_summarizes_idempotently() {
        let dir = tmp_dir("sweep");
        std::fs::write(dir.join("a.md"), plan("graph_node_id: x-1\nkind: feature")).unwrap();
        std::fs::write(dir.join("b.md"), plan("node: x-2\nclaims: x-2")).unwrap();
        // Already canonical.
        std::fs::write(dir.join("c.md"), plan("node: x-3\ntype: feature")).unwrap();
        let res = migrate(&dir, true);
        assert_eq!(res.migrated, 2, "{res:?}");
        assert_eq!(res.skipped, 1, "{res:?}");
        let a = std::fs::read_to_string(dir.join("a.md")).unwrap();
        assert!(a.contains("\nnode: x-1\n"), "{a}");
        let b = std::fs::read_to_string(dir.join("b.md")).unwrap();
        assert!(!b.contains("claims"), "{b}");
        // Re-run is idempotent.
        let res2 = migrate(&dir, true);
        assert_eq!(res2.migrated, 0, "{res2:?}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
