//! Dedup candidates by `source_pr` plus content hash, idempotently.
//!
//! The key is `{source_pr}:{hash}`, where the hash covers the normalized
//! finding body. Badges are stripped, so the same issue flagged by two
//! reviewers collapses to one node. A rerun finds the key on a live node
//! and files nothing. A node a human deleted is filed again, because dedup
//! reads live nodes only.
//!
//! `land` writes the key into each node's details as a machine trailer,
//! `<!-- retro-triage source_pr=343 finding_hash=ab12cd34ef56 -->`, so a later
//! run reads the keys back without a schema field.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use regex::Regex;
use serde_json::Value;

use super::reconcile_findings::{AddressedFinding, ScanFn};
use super::{str_or_empty, Candidate};
use crate::backlog::maintenance::validity::{
    retro_content_hash, retro_normalize_finding, retro_trailer_re,
};

pub fn normalize_finding(text: &str) -> String {
    retro_normalize_finding(text)
}

pub fn content_hash(text: &str) -> String {
    retro_content_hash(text)
}

/// Python's `str(source_pr)`: `None` for a PR-less candidate.
pub fn pr_label(source_pr: Option<i64>) -> String {
    source_pr.map_or_else(|| "None".to_string(), |n| n.to_string())
}

/// The machine trailer `land` appends to a node's details.
pub fn trailer(source_pr: Option<i64>, finding_hash: &str) -> String {
    format!(
        "<!-- retro-triage source_pr={} finding_hash={finding_hash} -->",
        pr_label(source_pr)
    )
}

fn candidate_text(c: &Candidate) -> String {
    if !c.finding_text.is_empty() {
        return c.finding_text.clone();
    }
    match c.extra.get("finding_text") {
        Some(v) if super::truthy(Some(v)) => super::py_str(Some(v)),
        _ => c.body.clone(),
    }
}

/// Fill `content_hash` on each candidate.
pub fn assign_hashes(candidates: Vec<Candidate>) -> Vec<Candidate> {
    candidates
        .into_iter()
        .map(|mut c| {
            c.content_hash = content_hash(&candidate_text(&c));
            c
        })
        .collect()
}

/// The structured cite a FILING writes for a carve-out id. Ownership, not
/// mention: a node whose prose merely names a cv-id does not track it.
pub fn cv_cite_needle(cv_id: &str) -> String {
    format!("source `{cv_id}`")
}

fn node_text(node: &Value) -> String {
    format!(
        "{}\n{}",
        str_or_empty(node, "title"),
        str_or_empty(node, "details")
    )
}

/// Which of `cv_ids` a node owns by the structured cite. The cv-id is the
/// one identifier stable across the PR-scoped harvest and the PR-less sweep.
pub fn cv_ids_cited_in_nodes(nodes: &[Value], cv_ids: &[String]) -> BTreeSet<String> {
    let wanted: BTreeSet<String> = cv_ids.iter().filter(|c| !c.is_empty()).cloned().collect();
    let mut found = BTreeSet::new();
    if wanted.is_empty() {
        return found;
    }
    for node in nodes {
        let text = node_text(node);
        for cv in &wanted {
            if !found.contains(cv) && text.contains(&cv_cite_needle(cv)) {
                found.insert(cv.clone());
            }
        }
        if found.len() == wanted.len() {
            break;
        }
    }
    found
}

/// Every dedup key the live nodes' trailers carry.
pub fn existing_keys_from_nodes(nodes: &[Value]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for node in nodes {
        let details = str_or_empty(node, "details");
        if details.is_empty() {
            continue;
        }
        for caps in retro_trailer_re().captures_iter(&details) {
            keys.insert(format!("{}:{}", &caps[1], &caps[2]));
        }
    }
    keys
}

/// `(kept, skipped)`: a candidate whose key a live node carries is skipped,
/// and within-run duplicates collapse to the first.
pub fn dedup_candidates(
    candidates: Vec<Candidate>,
    existing_keys: &HashSet<String>,
) -> (Vec<Candidate>, Vec<Candidate>) {
    let mut seen = existing_keys.clone();
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for c in assign_hashes(candidates) {
        let key = format!("{}:{}", pr_label(c.source_pr), c.content_hash);
        if seen.contains(&key) {
            skipped.push(c);
            continue;
        }
        seen.insert(key);
        kept.push(c);
    }
    (kept, skipped)
}

/// The filing-time anchor verdict per candidate source id, from ONE batched
/// scan: `dead` when the source PR now shows the finding addressed,
/// `unresolvable` when the scan errored or the PR was unavailable, else
/// `present`. An outage never reads as present.
pub fn anchor_verdicts(candidates: &[Candidate], scan: &ScanFn) -> BTreeMap<String, String> {
    let scanable: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.source_pr.is_some_and(|n| n != 0))
        .collect();
    if scanable.is_empty() {
        return candidates
            .iter()
            .map(|c| (c.source_id.clone(), "present".to_string()))
            .collect();
    }
    let pseudos: Vec<Value> = scanable
        .iter()
        .map(|c| {
            serde_json::json!({
                "id": format!("candidate:{}", c.source_id),
                "details": format!("{}\n{}", c.body, trailer(c.source_pr, &c.content_hash)),
            })
        })
        .collect();
    let mut warnings: Vec<String> = Vec::new();
    let addressed: Vec<AddressedFinding> = match scan(&pseudos, true, &mut warnings) {
        Ok(found) => found,
        Err(_) => {
            return scanable
                .iter()
                .map(|c| (c.source_id.clone(), "unresolvable".to_string()))
                .collect()
        }
    };
    let addressed_ids: HashSet<&str> = addressed.iter().map(|a| a.node_id.as_str()).collect();
    let unavailable = unavailable_prs_from_warnings(&warnings);
    let conservative = !warnings.is_empty() && unavailable.is_empty();
    let mut verdicts = BTreeMap::new();
    for c in scanable {
        let verdict = if addressed_ids.contains(format!("candidate:{}", c.source_id).as_str()) {
            "dead"
        } else if c.source_pr.is_some_and(|n| unavailable.contains(&n)) || conservative {
            "unresolvable"
        } else {
            "present"
        };
        verdicts.insert(c.source_id.clone(), verdict.to_string());
    }
    verdicts
}

fn unavailable_prs_from_warnings(warnings: &[String]) -> HashSet<i64> {
    let re = Regex::new(r"#(\d+)").expect("pr ref regex");
    warnings
        .iter()
        .flat_map(|w| {
            re.captures_iter(w)
                .filter_map(|c| c[1].parse().ok())
                .collect::<Vec<i64>>()
        })
        .collect()
}
