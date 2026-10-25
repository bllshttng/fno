//! The PR-independent sweep of the carve-out ledger.
//!
//! The PR-scoped harvest sees only carve-outs a trigger points it at, so a
//! row with no session, or one whose PR never dropped a sentinel, is never
//! harvested by any path. This sweep reads the whole ledger and gives each
//! row one of three dispositions:
//!
//! - `resolve`: an exact match (the structured cv-id cite, or a done or
//!   superseded owner the row names) proves the work is tracked. Consume the
//!   row and file nothing.
//! - `review`: an ambiguous match (a bare cv-id mention, a description-hash
//!   hit, a fuzzy subject hit, or a twin earlier in the batch). Never
//!   consumed and never filed: a wrong match either loses the work or
//!   duplicates it, so a human decides.
//! - `file`: no match. Mint a node, then consume the row.
//!
//! Nothing is consumed without a node id or inbox line to hold the work.
//! Applying is manual: a background hook minting nodes unattended leaves
//! cleanup nobody asked for. A wrong filing is recoverable with
//! `fno backlog remove`. `backfill` rows are skipped; they belong to
//! `/fno:ship pr merged`.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Value};

use super::classify::{classify_item, derive_title};
use super::dedup::{assign_hashes, content_hash, cv_cite_needle};
use super::land::{land_candidates, LandOptions, OUTCOME_FAILED};
use super::ledger::{consume_carveouts, read_carveouts, BACKFILL_KIND};
use super::{py_str, str_or_empty, truthy, Candidate, RawItem, KIND_CARVEOUT};

pub const DISPOSITION_FILE: &str = "file";
pub const DISPOSITION_RESOLVE: &str = "resolve";
pub const DISPOSITION_REVIEW: &str = "review";

/// Deliberately high: a false positive parks real work for a human, a false
/// negative costs one triage row. Short strings score noisily, so they are
/// excluded outright.
pub const TITLE_MATCH_THRESHOLD: f64 = 0.85;
pub const TITLE_MATCH_MIN_LEN: usize = 20;

const TERMINAL_STATUSES: [&str; 2] = ["done", "superseded"];

/// One carve-out and what the sweep decided about it.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepItem {
    pub carveout: Value,
    pub disposition: String,
    pub candidate: Option<Candidate>,
    /// For resolve and review: the node already tracking the work. For
    /// file: the node an applied run minted.
    pub node_id: Option<String>,
    pub match_reason: Option<String>,
    pub error: Option<String>,
    /// File only: a live node the row names as the parent of this work.
    pub link_to: Option<String>,
}

impl SweepItem {
    fn new(carveout: &Value, disposition: &str) -> Self {
        SweepItem {
            carveout: carveout.clone(),
            disposition: disposition.to_string(),
            candidate: None,
            node_id: None,
            match_reason: None,
            error: None,
            link_to: None,
        }
    }

    pub fn carveout_id(&self) -> String {
        let id = str_or_empty(&self.carveout, "id");
        if id.is_empty() {
            "?".to_string()
        } else {
            id
        }
    }

    pub fn kind(&self) -> String {
        let kind = str_or_empty(&self.carveout, "kind");
        if kind.is_empty() {
            "?".to_string()
        } else {
            kind
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SweepReport {
    pub items: Vec<SweepItem>,
    pub applied: bool,
    pub consumed: usize,
    pub warnings: Vec<String>,
    /// The ledger exists but could not be read. Never the same as empty.
    pub read_failed: bool,
}

impl SweepReport {
    pub fn by_disposition(&self, disposition: &str) -> Vec<&SweepItem> {
        self.items
            .iter()
            .filter(|i| i.disposition == disposition)
            .collect()
    }

    pub fn failed(&self) -> bool {
        self.read_failed || self.items.iter().any(|i| i.error.is_some())
    }
}

/// Lowercase, strip punctuation, collapse whitespace.
fn normalize_subject(text: &str) -> String {
    static PUNCT: OnceLock<Regex> = OnceLock::new();
    static WS: OnceLock<Regex> = OnceLock::new();
    let punct = PUNCT.get_or_init(|| Regex::new(r"[^a-z0-9 ]+").expect("punct regex"));
    let ws = WS.get_or_init(|| Regex::new(r"\s+").expect("ws regex"));
    let lowered = text.to_lowercase();
    ws.replace_all(&punct.replace_all(&lowered, " "), " ")
        .trim()
        .to_string()
}

fn node_text(node: &Value) -> String {
    format!(
        "{}\n{}",
        str_or_empty(node, "title"),
        str_or_empty(node, "details")
    )
}

fn node_id_of(node: &Value) -> String {
    str_or_empty(node, "id")
}

/// A node id the row's text names as where the work lives:
/// `(node_id, reason, terminal)`. Terminal means done or superseded, so the
/// row is covered; a live owner means file, but link.
fn named_owner(rec: &Value, nodes: &[Value]) -> (Option<String>, Option<String>, bool) {
    static NODE_ID: OnceLock<Regex> = OnceLock::new();
    let re = NODE_ID
        .get_or_init(|| Regex::new(r"\b([a-z][a-z0-9]{0,9}-[0-9a-f]{4,16})\b").expect("id regex"));
    let by_id: HashMap<String, &Value> = nodes
        .iter()
        .filter(|n| n.is_object())
        .map(|n| (node_id_of(n), n))
        .collect();
    let text = format!(
        "{}\n{}",
        str_or_empty(rec, "description"),
        str_or_empty(rec, "need")
    );
    let mut live: Option<(String, String)> = None;
    for caps in re.captures_iter(&text) {
        let tok = caps[1].to_string();
        let Some(node) = by_id.get(&tok) else {
            continue;
        };
        let status = str_or_empty(node, "status");
        if TERMINAL_STATUSES.contains(&status.as_str()) || truthy(node.get("superseded_by")) {
            let label = if status.is_empty() {
                "superseded".to_string()
            } else {
                status
            };
            let reason = format!("carve-out names owner {tok}, which is {label}");
            return (Some(tok), Some(reason), true);
        }
        if live.is_none() {
            let reason = format!("carve-out names live owner {tok}");
            live = Some((tok, reason));
        }
    }
    match live {
        Some((id, reason)) => (Some(id), Some(reason), false),
        None => (None, None, false),
    }
}

/// One ledger row as a harvest item. `source_pr` is `None` by construction:
/// the sweep has no PR, and a carve-out's cite is its own cv-id.
fn raw_item(rec: &Value) -> RawItem {
    let opt = |key: &str| match rec.get(key) {
        None | Some(Value::Null) => None,
        Some(v) => Some(py_str(Some(v))),
    };
    RawItem {
        kind: KIND_CARVEOUT.to_string(),
        text: str_or_empty(rec, "description"),
        source_pr: None,
        source_id: str_or_empty(rec, "id"),
        priority: opt("priority"),
        title_hint: opt("need"),
        subkind: opt("kind"),
        ..RawItem::default()
    }
}

/// A tracking verdict: `(node_id, reason, exact, link_to)`.
pub type Tracking = (Option<String>, Option<String>, bool, Option<String>);

/// Find a node that already tracks this carve-out. `exact` is true only for
/// a provable match: the structured cite a filing writes, or a done or
/// superseded owner the row names. A bare cv-id mention, a description-hash
/// hit, or a fuzzy subject hit is not exact. `link_to` is set only when the
/// row names a live owner.
pub fn find_tracking_node(rec: &Value, nodes: &[Value]) -> Tracking {
    let cv_id = str_or_empty(rec, "id");
    if !cv_id.is_empty() {
        let needle = cv_cite_needle(&cv_id);
        let mut mention: Option<&Value> = None;
        for node in nodes {
            let text = node_text(node);
            if text.contains(&needle) {
                let id = node_id_of(node);
                let reason = format!("cv-id cited in {}", py_str(node.get("id")));
                return (Some(id), Some(reason), true, None);
            }
            if mention.is_none() && text.contains(&cv_id) {
                mention = Some(node);
            }
        }
        if let Some(node) = mention {
            let reason = format!(
                "cv-id mentioned in {} (citation is not ownership)",
                py_str(node.get("id"))
            );
            return (Some(node_id_of(node)), Some(reason), false, None);
        }
    }

    let (owner, owner_reason, terminal) = named_owner(rec, nodes);
    if owner.is_some() && terminal {
        return (owner, owner_reason, true, None);
    }
    if owner.is_some() {
        return (None, owner_reason, false, owner);
    }

    // The hash covers the description only, so two rows can share a generic
    // description while differing in kind, need and scope. Review, never
    // resolve.
    let digest = content_hash(&str_or_empty(rec, "description"));
    if !digest.is_empty() {
        let needle = format!("finding_hash={digest}");
        for node in nodes {
            if str_or_empty(node, "details").contains(&needle) {
                let reason = format!(
                    "same description hash as {} (kind/need/scope not compared)",
                    py_str(node.get("id"))
                );
                return (Some(node_id_of(node)), Some(reason), false, None);
            }
        }
    }

    // Compare on the same string the node would be titled with.
    let subject = normalize_subject(&derive_title(&raw_item(rec)));
    if super::char_len(&subject) >= TITLE_MATCH_MIN_LEN {
        let mut best: Option<(String, f64)> = None;
        for node in nodes {
            let title = normalize_subject(&str_or_empty(node, "title"));
            if super::char_len(&title) < TITLE_MATCH_MIN_LEN {
                continue;
            }
            let ratio = super::difflib::ratio(&subject, &title);
            if best.as_ref().is_none_or(|(_, b)| ratio > *b) {
                best = Some((node_id_of(node), ratio));
            }
        }
        if let Some((id, ratio)) = best {
            if !id.is_empty() && ratio >= TITLE_MATCH_THRESHOLD {
                let reason = format!("subject {ratio:.2} similar to {id}");
                return (Some(id), Some(reason), false, None);
            }
        }
    }
    (None, None, false, None)
}

/// A disposition for every carve-out. Pure, so the dry run and the applied
/// run cannot diverge; within-batch twins collapse here for the same reason.
pub fn plan_sweep(carveouts: &[Value], nodes: &[Value]) -> Vec<SweepItem> {
    let mut items = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    for rec in carveouts {
        if rec.get("kind").and_then(Value::as_str) == Some(BACKFILL_KIND) {
            continue;
        }
        let cv_id = str_or_empty(rec, "id");
        if cv_id.is_empty() {
            items.push(SweepItem {
                match_reason: Some("row carries no id; cannot be cited or consumed".to_string()),
                ..SweepItem::new(rec, DISPOSITION_REVIEW)
            });
            continue;
        }
        let (node_id, reason, exact, link_to) = find_tracking_node(rec, nodes);
        if node_id.is_some() {
            let disposition = if exact {
                DISPOSITION_RESOLVE
            } else {
                DISPOSITION_REVIEW
            };
            items.push(SweepItem {
                node_id,
                match_reason: reason,
                ..SweepItem::new(rec, disposition)
            });
            continue;
        }
        let candidate = assign_hashes(vec![classify_item(
            &raw_item(rec),
            super::classify::BODY_CAP,
        )])
        .remove(0);
        if let Some(twin) = seen.get(&candidate.content_hash) {
            // Park rather than resolve: the twin's node does not exist yet.
            let reason = format!("same text as {twin} earlier in this sweep");
            items.push(SweepItem {
                candidate: Some(candidate),
                match_reason: Some(reason),
                ..SweepItem::new(rec, DISPOSITION_REVIEW)
            });
            continue;
        }
        seen.insert(candidate.content_hash.clone(), cv_id);
        items.push(SweepItem {
            candidate: Some(candidate),
            link_to,
            ..SweepItem::new(rec, DISPOSITION_FILE)
        });
    }
    items
}

/// A related edge from a just-minted node to its named live owner, both
/// halves in one locked write. A missing endpoint skips.
pub fn default_link(node_id: &str, owner_id: &str) -> Result<(), String> {
    let graph = crate::backlog::settings::graph_path();
    crate::graph_store::mutate_rows(
        &graph,
        crate::graph_store::DEFAULT_LOCK_TIMEOUT,
        None,
        None,
        |rows| {
            let has = |id: &str| {
                rows.iter()
                    .any(|r| r.get("id").and_then(Value::as_str) == Some(id))
            };
            if !has(owner_id) || !has(node_id) {
                return Ok(false);
            }
            let me = rows
                .iter()
                .find(|r| r.get("id").and_then(Value::as_str) == Some(node_id))
                .cloned()
                .unwrap_or(Value::Null);
            let mut desired: Vec<String> = me
                .get("related")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if !desired.iter().any(|d| d == owner_id) {
                desired.push(owner_id.to_string());
            }
            crate::graph_keeper::set_related(rows, node_id, &desired)?;
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The seams of an applied sweep.
pub struct SweepSeams<'a> {
    pub land: LandOptions<'a>,
    pub read: &'a dyn Fn(&Path, Option<&str>) -> Result<Vec<Value>, String>,
    pub consume: &'a dyn Fn(&Path, &[String]) -> usize,
    pub link: &'a dyn Fn(&str, &str) -> Result<(), String>,
}

impl<'a> SweepSeams<'a> {
    pub fn live(land: LandOptions<'a>) -> Self {
        SweepSeams {
            land,
            read: &read_carveouts,
            consume: &consume_carveouts,
            link: &default_link,
        }
    }
}

/// Plan, and with `apply` file and consume, a sweep of the whole ledger.
/// `kind` narrows to one carve-out kind without changing any disposition.
pub fn sweep_carveouts(
    carveout_root: &Path,
    nodes: &[Value],
    apply: bool,
    kind: Option<&str>,
    seams: &SweepSeams,
) -> SweepReport {
    let mut report = SweepReport {
        applied: apply,
        ..SweepReport::default()
    };
    let rows = match (seams.read)(carveout_root, kind) {
        Ok(rows) => rows,
        Err(e) => {
            report
                .warnings
                .push(format!("cannot read carve-out ledger: {e}"));
            report.read_failed = true;
            return report;
        }
    };
    report.items = plan_sweep(&rows, nodes);
    if !apply {
        return report;
    }
    let mut to_consume = Vec::new();
    let mut link_warnings = Vec::new();
    for item in report.items.iter_mut() {
        match item.disposition.as_str() {
            DISPOSITION_REVIEW => continue,
            DISPOSITION_RESOLVE => {
                to_consume.push(item.carveout_id());
                continue;
            }
            _ => {}
        }
        let Some(candidate) = item.candidate.clone() else {
            item.error = Some("no candidate to file".to_string());
            continue;
        };
        let results = land_candidates(std::slice::from_ref(&candidate), &seams.land);
        let Some(result) = results.into_iter().next() else {
            item.error = Some("candidate did not land (no result)".to_string());
            continue;
        };
        if result.outcome == OUTCOME_FAILED {
            item.error = Some(result.error.unwrap_or_else(|| "land failed".to_string()));
            continue;
        }
        item.node_id = result.node_id;
        if let (Some(node_id), Some(owner)) = (&item.node_id, &item.link_to) {
            if let Err(e) = (seams.link)(node_id, owner) {
                link_warnings.push(format!("could not link {node_id} to {owner}: {e}"));
            }
        }
        // An inbox line carries no node id but is a durable record.
        to_consume.push(item.carveout_id());
    }
    report.warnings.extend(link_warnings);
    if !to_consume.is_empty() {
        let removed = (seams.consume)(carveout_root, &to_consume);
        report.consumed = removed;
        if removed < to_consume.len() {
            report.warnings.push(format!(
                "consumed only {removed}/{} carve-out row(s); the remainder stay in the ledger and re-sweep next run",
                to_consume.len()
            ));
        }
    }
    report
}

/// The human-readable report: one line per carve-out plus a summary. A
/// ledger that could not be read never renders as a count.
pub fn render_sweep(report: &SweepReport) -> Vec<String> {
    if report.read_failed {
        return vec![
            "carve-out sweep: FAILED to read the ledger; no count is available (this is NOT an empty ledger)"
                .to_string(),
        ];
    }
    let command = if report.applied { "applied" } else { "dry run" };
    let count = |d: &str| report.by_disposition(d).len();
    let mut lines = vec![format!(
        "carve-out sweep ({command}): {} unharvested -> {} file, {} already tracked, {} need a human look",
        report.items.len(),
        count(DISPOSITION_FILE),
        count(DISPOSITION_RESOLVE),
        count(DISPOSITION_REVIEW)
    )];
    for (disposition, label) in [
        (DISPOSITION_FILE, "FILE"),
        (DISPOSITION_RESOLVE, "TRACKED"),
        (DISPOSITION_REVIEW, "REVIEW"),
    ] {
        let rows = report.by_disposition(disposition);
        if rows.is_empty() {
            continue;
        }
        lines.push(String::new());
        lines.push(format!("{label} ({}):", rows.len()));
        for item in rows {
            let head = format!("  {} [{}]", item.carveout_id(), item.kind());
            if disposition == DISPOSITION_FILE {
                let (title, priority, tier) = match &item.candidate {
                    Some(c) => (c.title.as_str(), c.priority.as_str(), c.tier.as_str()),
                    None => ("(unclassified)", "?", "?"),
                };
                let landed = item
                    .node_id
                    .as_ref()
                    .map(|n| format!(" -> {n}"))
                    .unwrap_or_default();
                let linked = item
                    .link_to
                    .as_ref()
                    .map(|n| format!(" (links {n})"))
                    .unwrap_or_default();
                lines.push(format!("{head} {priority} {tier}: {title}{landed}{linked}"));
            } else {
                lines.push(format!(
                    "{head} {}",
                    item.match_reason.as_deref().unwrap_or("None")
                ));
            }
            if let Some(e) = &item.error {
                lines.push(format!("    FAILED: {e}"));
            }
        }
    }
    if report.applied {
        lines.push(String::new());
        lines.push(format!("consumed {} ledger row(s)", report.consumed));
    }
    lines
}

/// A sweep item as the characterization goldens project it.
pub fn item_json(item: &SweepItem) -> Value {
    json!({
        "carveout_id": item.carveout_id(),
        "disposition": item.disposition,
        "node_id": item.node_id,
        "match_reason": item.match_reason,
        "link_to": item.link_to,
    })
}
