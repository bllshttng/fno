//! The fno-agents binary's verb surface for the command ratchet.
//!
//! Two independent readings of one surface, cross-checked:
//! - the dispatch: every verb the client answers, read from the dispatcher's
//!   own source - the run-region scan menu-caps also counts with, the main()
//!   argv doors, the gh transport door, the `family_doors` table, the
//!   `pr_door`/`review_doors`/`client_transport`/`pr_closure` delegate
//!   tables, and the claim family's op arms in `claim_commands.rs`;
//! - the refusal: `ALL_CLIENT_ACTIONS`, the exact slice the
//!   unknown-command error prints, so the refusal can never name a verb
//!   nothing dispatches.
//!
//! A dispatched verb the refusal does not name is operator-invisible. The
//! ruling that owns this surface (2026-09-25: hidden top-level verbs retire
//! into nested groups) is carried by `menu-caps-debt.tsv`'s fno-agents row;
//! until a verb folds, its invisibility is deliberate and recorded in
//! `scripts/ci/agents-hidden-verbs.tsv`, one row per verb with a reason.
//!
//! Declared boundary: the scan reaches one level of dispatch tables, not the
//! deeper group CLIs (`backlog::cli`, the mail family's own parser, the
//! `do`/`pr` fronts' leaves, the closure produce commands). A group CLI's
//! leaves defend nothing yet; the fold that re-shapes a group regenerates
//! `command-baseline.txt` in the same PR, which is where its rows appear.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use regex::Regex;

use crate::menu_caps;

const CLIENT_REL: &str = "crates/fno-agents/src/bin/client.rs";
const FAMILY_DOORS_REL: &str = "crates/fno-agents/src/family_doors.rs";
const CLAIM_COMMANDS_REL: &str = "crates/fno-agents/src/claim_commands.rs";
const PR_DOOR_REL: &str = "crates/fno-agents/src/pr_door.rs";
const REVIEW_DOORS_REL: &str = "crates/fno-agents/src/review_doors.rs";
const TRANSPORT_REL: &str = "crates/fno-agents/src/client_transport.rs";
const GH_ADAPTER_REL: &str = "crates/fno-agents/src/pr_status/adapter.rs";
pub const HIDDEN_REL: &str = "scripts/ci/agents-hidden-verbs.tsv";

/// Pinned sub-verb seams: baseline rows the scanner asserts exist in the
/// client source by their distinctive spellings. A seam the source no longer
/// matches is a removed sub-surface; the ratchet refuses until the row goes
/// in the same PR that removed it. A NEW seam is invisible to this list -
/// the recorded boundary above, retired by the fold the debt row tracks.
const SEAMS: &[(&str, &str)] = &[
    ("claim birth", r#"== Some\("birth"\)"#),
    (
        "claude-birth-exec revive-proof",
        r#"== Some\("revive-proof"\)"#,
    ),
    ("pr-heal list", r#"== Some\("list"\)"#),
    (
        "do pr",
        r#"== Some\("do"\)\s*\n\s*&& args\.get\(1\)\.map\(String::as_str\) == Some\("pr"\)"#,
    ),
    (
        "do ci",
        r#"== Some\(&"do"\.to_string\(\)\) && args\.get\(1\) == Some\(&"ci"\.to_string\(\)\)"#,
    ),
    ("do show", r#"Some\("show" \| "diff"\)"#),
    ("do diff", r#"Some\("show" \| "diff"\)"#),
    ("test-run env", r#"Some\("env"\) => return"#),
    ("test-run sweep", r#"Some\("sweep"\) => return"#),
    ("test-run doctor", r#"Some\("doctor"\) => return"#),
];

/// One cross-checked reading of the binary's surface.
pub struct AgentsSurface {
    /// Every dispatched verb: run-region arms, argv doors, delegate tables,
    /// the gh door, plus the help/version spellings menu-caps inserts.
    pub top: BTreeSet<String>,
    /// The `ALL_CLIENT_ACTIONS` slice: what the unknown-command refusal
    /// prints.
    pub registered: BTreeSet<String>,
    /// The claim family's op arms, `birth` included.
    pub claim_ops: BTreeSet<String>,
    /// What the claim no-operation refusal names.
    pub claim_refusal_ops: Vec<String>,
    /// Seam rows present in the source (SEAMS entries that matched).
    pub seams: Vec<String>,
    /// SEAMS entries whose pattern the source no longer matches.
    pub missing_seams: Vec<String>,
}

fn read(root: &Path, rel: &str) -> Result<String, String> {
    std::fs::read_to_string(root.join(rel))
        .map_err(|e| format!("agents-surface: cannot read {rel}: {e}"))
}

fn static_regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex")
}

/// The quoted lowercase tokens of a source region.
fn literals(text: &str) -> BTreeSet<String> {
    let arm = literal_regex();
    arm.captures_iter(text).map(|c| c[1].to_string()).collect()
}

/// The lowercase-token literal regex, shared by the const readers.
fn literal_regex() -> Regex {
    static_regex(r#""([a-z][a-z0-9-]*)""#)
}

/// The bracket-balanced region a `const ... = &[...]` declaration holds.
/// `braced_region` cannot read it: the slice closes with `]`, and the first
/// `{` after the marker is the next function's body.
fn bracket_region<'a>(text: &'a str, marker: &str) -> Result<&'a str, String> {
    let start = text
        .find(marker)
        .ok_or_else(|| format!("agents-surface: no marker {marker:?}"))?;
    // Anchor on the value bracket: the type annotation `&[&str]` closes
    // first, so the first `[` after the marker is not the slice.
    let open = start
        + text[start..]
            .find("= &[")
            .ok_or_else(|| format!("agents-surface: no slice literal after {marker:?}"))?
        + 3;
    let mut depth = 0i64;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(&text[open..=open + offset]);
                }
            }
            _ => {}
        }
    }
    Err(format!("agents-surface: no ] after {marker:?}"))
}

impl AgentsSurface {
    pub fn read(root: &Path) -> Result<Self, String> {
        let client = read(root, CLIENT_REL)?;
        let family = read(root, FAMILY_DOORS_REL)?;
        let claim = read(root, CLAIM_COMMANDS_REL)?;
        let pr_door = read(root, PR_DOOR_REL)?;
        let review_doors = read(root, REVIEW_DOORS_REL)?;
        let transport = read(root, TRANSPORT_REL)?;
        let gh_adapter = read(root, GH_ADAPTER_REL)?;

        let mut top = menu_caps::fno_agents_commands(&client)?;
        // The `|| command == "x"` continuation arms menu-caps' single-literal
        // regex cannot see; the shared extraction must not change its count,
        // because the fno-agents debt ceiling pins that number.
        let continuation = static_regex(r#"\|\|\s*command == "([a-z][a-z0-9-]*)""#);
        top.extend(
            continuation
                .captures_iter(&client)
                .map(|c| c[1].to_string()),
        );

        // main() argv doors: `args.first().map(String::as_str) == Some("x")`.
        let door = static_regex(
            r#"args\.first\(\)\.map\(String::as_str\) == Some\("([a-z][a-z0-9-]*)"\)"#,
        );
        top.extend(door.captures_iter(&client).map(|c| c[1].to_string()));

        // The delegate tables: one match per module, read in place. The arm
        // regex has no line anchor: Rust's `^` matches only at the haystack
        // start, and the regions are small flat match bodies.
        let table_arm = static_regex(r#""([a-z][a-z0-9-]*)"\s*=>"#);
        let some_arm = static_regex(r#"Some\("([a-z][a-z0-9-]*)"\)\s*=>"#);
        for (text, marker, regex) in [
            (&family, "pub fn client_dispatch", &some_arm),
            (&pr_door, "pub fn run_command", &table_arm),
            (&review_doors, "pub fn dispatch", &table_arm),
            (&transport, "pub fn transport_early_arms", &some_arm),
        ] {
            let stripped = menu_caps::strip_comments(text);
            let region = menu_caps::braced_region(&stripped, marker)?;
            top.extend(regex.captures_iter(region).map(|c| c[1].to_string()));
        }
        // pr_closure's produce door: an alternation arm, so read the literals
        // of the match region instead of the arm shapes.
        let closure = read(root, "crates/fno-agents/src/org_board/pr_closure.rs")?;
        let closure_stripped = menu_caps::strip_comments(&closure);
        let closure_region = menu_caps::braced_region(&closure_stripped, "pub fn run_command")?;
        top.extend(literals(closure_region));

        // The gh transport door: plain literal arms over its own match.
        let gh_stripped = menu_caps::strip_comments(&gh_adapter);
        let gh_region = menu_caps::braced_region(&gh_stripped, "pub fn gh_transport_door")?;
        top.extend(table_arm.captures_iter(gh_region).map(|c| c[1].to_string()));

        // The refusal's registered spellings.
        let client_stripped = menu_caps::strip_comments(&client);
        let all_region = bracket_region(&client_stripped, "const ALL_CLIENT_ACTIONS")?;
        let registered = literals(all_region);

        // The claim family: op arms plus what the no-operation refusal names.
        let claim_stripped = menu_caps::strip_comments(&claim);
        let claim_region = menu_caps::braced_region(&claim_stripped, "pub fn run_claim")?;
        let op_arm = static_regex(r#"if op == "([a-z][a-z0-9-]*)""#);
        let mut claim_ops: BTreeSet<String> = op_arm
            .captures_iter(claim_region)
            .map(|c| c[1].to_string())
            .collect();
        let refusal = static_regex(r#"claim requires an operation:\s*([a-z0-9-|]+)"#);
        let claim_refusal_ops: Vec<String> = refusal
            .captures(&claim)
            .map(|c| c[1].split('|').map(str::trim).map(str::to_string).collect())
            .unwrap_or_default();

        // The seams: each pinned pattern must still match the client source.
        let mut seams = Vec::new();
        let mut missing_seams = Vec::new();
        for (row, pattern) in SEAMS {
            if static_regex(pattern).is_match(&client) {
                seams.push((*row).to_string());
            } else {
                missing_seams.push((*row).to_string());
            }
        }
        claim_ops.extend(
            seams
                .iter()
                .filter(|r| r.starts_with("claim "))
                .map(|r| r.trim_start_matches("claim ").to_string()),
        );

        Ok(Self {
            top,
            registered,
            claim_ops,
            claim_refusal_ops,
            seams,
            missing_seams,
        })
    }

    /// The verbs the dispatch answers but the refusal does not name.
    pub fn hidden(&self) -> BTreeSet<String> {
        self.top.difference(&self.registered).cloned().collect()
    }

    /// The baseline rows the live surface earns: the binary leaf, one row per
    /// dispatched verb, the claim family's ops, and the seams.
    pub fn rows(&self) -> Vec<String> {
        let mut rows: BTreeSet<String> = BTreeSet::new();
        rows.insert("fno-agents".to_string());
        for verb in &self.top {
            rows.insert(format!("fno-agents {verb}"));
        }
        for op in &self.claim_ops {
            rows.insert(format!("fno-agents claim {op}"));
        }
        for row in &self.seams {
            rows.insert(format!("fno-agents {row}"));
        }
        rows.into_iter().collect()
    }

    /// The cross-checks between the two readings and the hidden-verb record.
    /// Each refusal names the row and the fix.
    pub fn drift(&self, hidden_rows: &BTreeMap<String, String>) -> Vec<String> {
        let mut failures = Vec::new();
        for verb in &self.hidden() {
            if !hidden_rows.contains_key(verb) {
                failures.push(format!(
                    "agents-surface: `{verb}` dispatches but the refusal never names it, and \
                     {HIDDEN_REL} has no row. Record the deliberate invisibility as \
                     `{verb}<TAB><why it has no operator surface>`, register the spelling, or \
                     remove the verb in the same PR."
                ));
            }
        }
        for verb in hidden_rows.keys() {
            let stale = if let Some(op) = verb.strip_prefix("claim ") {
                !self.claim_ops.contains(op) || self.claim_refusal_ops.iter().any(|r| r == op)
            } else {
                !self.top.contains(verb) || self.registered.contains(verb)
            };
            if stale {
                failures.push(format!(
                    "agents-surface: {HIDDEN_REL} row `{verb}` is stale - the verb now \
                     dispatches registered, no longer dispatches, or its op is refusal-named. \
                     Delete the row in the same PR that changed the verb."
                ));
            }
        }
        for verb in &self.registered {
            if !self.top.contains(verb) {
                failures.push(format!(
                    "agents-surface: the refusal names `{verb}` but no dispatch arm answers it. \
                     A refused command must never read as registered: restore the arm, remove \
                     the spelling from ALL_CLIENT_ACTIONS, or land it in the retired pointer."
                ));
            }
        }
        for op in &self.claim_refusal_ops {
            if !self.claim_ops.contains(op) {
                failures.push(format!(
                    "agents-surface: the claim refusal names op `{op}` but no `if op ==` arm \
                     answers it. The refusal must not lie: restore the arm or drop the token."
                ));
            }
        }
        for row in &self.missing_seams {
            failures.push(format!(
                "agents-surface: seam row `fno-agents {row}` has no matching dispatch shape in \
                 the client source. The sub-verb was removed or re-spelled: delete its baseline \
                 row in the same PR, or fix the seam pattern."
            ));
        }
        failures
    }
}

/// Parse `agents-hidden-verbs.tsv`: `path<TAB>reason`, comments skipped.
pub fn hidden_rows(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut rows = BTreeMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        match (cols.first(), cols.get(1)) {
            (Some(path), Some(reason)) if !path.trim().is_empty() && !reason.trim().is_empty() => {
                rows.insert(path.trim().to_string(), reason.trim().to_string());
            }
            _ => {
                return Err(format!(
                    "agents-surface: {HIDDEN_REL}:{} is not `path<TAB>reason`",
                    n + 1
                ))
            }
        }
    }
    Ok(rows)
}

/// The two-sided baseline check for the fno-agents rows: verbs the binary
/// dispatches that no baseline row defends, and rows whose verb is gone.
pub fn baseline_drift(live: &[String], baseline: &[String]) -> (Vec<String>, Vec<String>) {
    let live: BTreeSet<String> = live.iter().cloned().collect();
    let baseline_agents: BTreeSet<String> = baseline
        .iter()
        .filter(|r| r.starts_with("fno-agents"))
        .map(|r| {
            r.split_whitespace()
                .filter(|w| !w.starts_with('!'))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect();
    let added: Vec<String> = live.difference(&baseline_agents).cloned().collect();
    let removed: Vec<String> = baseline_agents.difference(&live).cloned().collect();
    (added, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agents_surface_cross_checks_refuse_each_drift() {
        let surface = AgentsSurface {
            top: BTreeSet::from(["spawn".to_string(), "claim".to_string()]),
            registered: BTreeSet::from(["spawn".to_string(), "gone".to_string()]),
            claim_ops: BTreeSet::from(["acquire".to_string(), "sweep".to_string()]),
            claim_refusal_ops: vec!["acquire".to_string(), "dead-op".to_string()],
            seams: vec!["claim birth".to_string()],
            missing_seams: vec!["test-run env".to_string()],
        };
        // A hidden verb with no recorded row refuses, naming the fix.
        let rows = hidden_rows("claim sweep\thidden transport\n").unwrap();
        let failures = surface.drift(&rows);
        assert!(
            failures.iter().any(|f| f.contains("`claim` dispatches")),
            "{failures:?}"
        );
        // A registered spelling nothing dispatches refuses.
        assert!(
            failures.iter().any(|f| f.contains("`gone`")),
            "{failures:?}"
        );
        // A refusal op with no arm refuses.
        assert!(
            failures.iter().any(|f| f.contains("`dead-op`")),
            "{failures:?}"
        );
        // A seam pattern the source lost refuses.
        assert!(
            failures.iter().any(|f| f.contains("test-run env")),
            "{failures:?}"
        );
        // A stale hidden row refuses: the op became refusal-named.
        let stale = hidden_rows("claim acquire\tnow listed\n").unwrap();
        let failures = surface.drift(&stale);
        assert!(
            failures
                .iter()
                .any(|f| f.contains("stale") && f.contains("claim acquire")),
            "{failures:?}"
        );
        assert_eq!(failures.len(), 1, "{failures:?}");
    }

    #[test]
    fn agents_surface_baseline_drift_runs_both_directions() {
        let live = vec![
            "fno-agents".to_string(),
            "fno-agents claim".to_string(),
            "fno-agents claim sweep".to_string(),
        ];
        // A missing row is an addition; a stale row is a removal.
        let baseline = vec![
            "mux".to_string(),
            "fno-agents claim".to_string(),
            "fno-agents detect".to_string(),
        ];
        let (added, removed) = baseline_drift(&live, &baseline);
        assert_eq!(added, vec!["fno-agents", "fno-agents claim sweep"]);
        assert_eq!(removed, vec!["fno-agents detect"]);
    }

    #[test]
    fn agents_surface_holds_on_the_committed_tree() {
        // The CI gate: the real sources, the real hidden record, the real
        // baseline. Pins the surface to its rows - the node's specimen,
        // `fno-agents claim sweep`, is defended here by name.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let surface = AgentsSurface::read(&root).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            surface.rows().iter().any(|r| r == "fno-agents claim sweep"),
            "the claim sweep specimen row is missing: {:?}",
            surface.claim_ops
        );
        let hidden_text = std::fs::read_to_string(root.join(HIDDEN_REL))
            .unwrap_or_else(|e| panic!("agents-surface: cannot read {HIDDEN_REL}: {e}"));
        let rows = hidden_rows(&hidden_text).unwrap();
        let failures = surface.drift(&rows);
        assert!(
            failures.is_empty(),
            "surface drift:\n  {}",
            failures.join("\n  ")
        );
        let baseline =
            std::fs::read_to_string(root.join("scripts/ci/command-baseline.txt")).unwrap();
        let baseline_rows: Vec<String> = baseline
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(str::to_string)
            .collect();
        let (added, removed) = baseline_drift(&surface.rows(), &baseline_rows);
        assert!(added.is_empty(), "verbs missing baseline rows: {added:?}");
        assert!(
            removed.is_empty(),
            "baseline rows with no verb: {removed:?}"
        );
        let _ = surface;
    }
}
