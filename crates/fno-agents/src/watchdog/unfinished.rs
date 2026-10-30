//! The unfinished-work report: every finding names the one command that clears it.
//!
//! Four dimensions, one clearing command each; liveness from pid and transcript
//! truth only; unreadable evidence keeps the dimension unknown, never clean. The
//! full contract lives in docs/architecture/unfinished-work-and-row-registration.md.
//! The IO seam (observation collection and the publish sequence) is in `collect`.

mod collect;

pub use collect::{build_report, build_report_with_deadline, publish_report, report_roots};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub const KIND_STARTED: &str = "started_free_claim";
pub const KIND_DONE_AHEAD: &str = "done_ahead_of_main";
pub const KIND_DIRTY: &str = "dirty_ownerless_worktree";
pub const KIND_PR: &str = "open_pr_ownerless";

/// Canonical order = actionable severity order: work stopped mid-node first,
/// then possibly-stranded committed work, then unowned uncommitted work, then
/// stale PRs. Findings sort by this, then age descending, then subject.
pub const DIMENSIONS: [&str; 4] = [KIND_STARTED, KIND_DONE_AHEAD, KIND_DIRTY, KIND_PR];

pub const MEASURED: &str = "measured";
pub const UNKNOWN_DIM: &str = "unknown";

/// Transcript activity fresher than this window proves an owner live. Matches
/// the fleet's idle threshold semantics (recovery.idle_threshold_seconds).
const DEFAULT_LIVE_ACTIVITY_S: f64 = 900.0;

/// A PR is reportable only when strictly older than this.
const PR_STALE_AFTER_S: f64 = 24.0 * 3600.0;

const LIVE: &str = "live";
const GONE: &str = "gone";
const OWNER_UNKNOWN: &str = "unknown";

/// The durable task lane the report's ask rides.
pub const MARKER: &str = "watchdog-unfinished-work";
const MAX_LISTED_ROWS: usize = 10;

fn severity(kind: &str) -> usize {
    DIMENSIONS
        .iter()
        .position(|dim| *dim == kind)
        .unwrap_or(DIMENSIONS.len())
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Finding {
    pub kind: String,
    pub subject: String,
    pub basis: String,
    pub clear_command: String,
    pub node_id: Option<String>,
    pub pr_number: Option<i64>,
    pub pr_url: Option<String>,
    pub cwd: Option<String>,
    pub age_s: Option<f64>,
    pub ahead_count: Option<i64>,
    pub dirty_count: Option<i64>,
}

/// Stable identity: outcome kind plus the exact node/path/PR it names. Age and
/// counts are measurements, not identity, so a finding that only got older does
/// not re-mail or re-escalate.
pub fn finding_identity(finding: &Finding) -> String {
    format!("{}:{}", finding.kind, finding.subject)
}

#[derive(Debug, Clone, PartialEq)]
pub struct DimensionState {
    pub state: String,
    /// Findings when measured; meaningless when unknown.
    pub count: usize,
    pub warning: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub generated_at: String,
    pub findings: Vec<Finding>,
    /// In [`DIMENSIONS`] order.
    pub dimensions: Vec<(String, DimensionState)>,
    pub warnings: Vec<String>,
    pub complete: bool,
}

impl Snapshot {
    pub fn dimension(&self, kind: &str) -> Option<&DimensionState> {
        self.dimensions
            .iter()
            .find(|(dim, _)| dim == kind)
            .map(|(_, state)| state)
    }
}

/// One owner candidate (a session handle) with its readable evidence.
/// `pid_alive`: true/false only from a positive process probe; None when no pid
/// was recorded or the probe could not answer. `last_activity_basis`: how the
/// transcript age was taken (`mtime` is a file stamp, refused as positive
/// evidence). `stored_exited`: the one stored status that is itself a probe
/// result (reconcile writes it only after confirming the child was gone).
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct OwnerProbe {
    pub handle: String,
    pub pid_alive: Option<bool>,
    pub transcript_age_s: Option<f64>,
    pub last_activity_basis: Option<String>,
    pub claim_state: Option<String>,
    pub stored_exited: bool,
}

/// `live` | `gone` | `unknown`, through the one shared reachability predicate.
/// Falsifiers first (dead pid, exit stamp, never while the transcript is fresh:
/// a harness resume kills the pid while the session keeps writing), then positive
/// evidence. Unknown never reads as ownerless. The one clock-word outside the
/// predicate is a stale claim: only TTL expiry proves a lease dead.
pub(crate) fn owner_verdict(probe: &OwnerProbe) -> &'static str {
    let fresh = probe
        .transcript_age_s
        .is_some_and(|age| age <= DEFAULT_LIVE_ACTIVITY_S);
    let falsifier: Option<&'static str> = if probe.stored_exited && !fresh {
        Some("exit-recorded")
    } else if probe.pid_alive == Some(false) && !fresh {
        Some("process-gone")
    } else {
        None
    };
    let (verdict, _basis) = crate::session_truth::classify_reachability_at(
        probe.transcript_age_s.map(|_| "working"),
        probe.transcript_age_s.map(|age| age as i64),
        falsifier,
        DEFAULT_LIVE_ACTIVITY_S,
        probe.pid_alive.filter(|alive| *alive),
        probe.last_activity_basis.as_deref(),
        &Value::Null,
    );
    if verdict == "reachable" {
        return LIVE;
    }
    if verdict == "unreachable" {
        return GONE;
    }
    // Suspect keeps TTL protection, and the claims machinery itself refuses to
    // steal it.
    if probe.claim_state.as_deref() == Some("stale") {
        return GONE;
    }
    OWNER_UNKNOWN
}

/// Verdict over every candidate owner of one subject. No candidates at all is
/// ownerless only when the stores that would carry candidates read successfully;
/// a failed read is the unmeasurable case, not the empty one.
fn owner_set_verdict(probes: &[OwnerProbe], sources_ok: bool) -> &'static str {
    if probes.is_empty() {
        return if sources_ok { GONE } else { OWNER_UNKNOWN };
    }
    let verdicts: Vec<&str> = probes.iter().map(owner_verdict).collect();
    if verdicts.contains(&LIVE) {
        return LIVE;
    }
    if verdicts.iter().all(|verdict| *verdict == GONE) {
        return GONE;
    }
    OWNER_UNKNOWN
}

/// The claim view one node holds; every field None is the unreadable view.
#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct ClaimView {
    pub state: Option<String>,
    pub holder: Option<String>,
    pub pid: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct NodeObs {
    pub node_id: String,
    pub status: String,
    pub touched_at_epoch: Option<f64>,
    pub cwd: Option<String>,
    pub worktree_path: Option<String>,
    pub ahead_count: Option<i64>,
    pub claim: Option<ClaimView>,
    pub owner_probes: Vec<OwnerProbe>,
    pub is_container: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct WorktreeObs {
    pub path: String,
    pub repo_root: String,
    pub branch: Option<String>,
    /// None = the git read failed.
    pub dirty_count: Option<i64>,
    /// None = unmeasured or failed.
    pub ahead_count: Option<i64>,
    pub node_id: Option<String>,
    pub owner_probes: Vec<OwnerProbe>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct PrObs {
    pub pr_number: i64,
    pub pr_url: Option<String>,
    pub node_id: Option<String>,
    /// None = the GitHub read failed.
    pub state: Option<String>,
    pub opened_at_epoch: Option<f64>,
    pub owner_probes: Vec<OwnerProbe>,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[cfg_attr(test, derive(serde::Deserialize))]
#[cfg_attr(test, serde(default))]
pub struct Observations {
    pub now_epoch: f64,
    pub graph_ok: bool,
    pub claims_ok: bool,
    pub registry_ok: bool,
    pub github_ok: bool,
    pub nodes: Vec<NodeObs>,
    pub worktrees: Vec<WorktreeObs>,
    pub prs: Vec<PrObs>,
    pub unscanned_roots: Vec<String>,
    pub prs_unscanned: bool,
    pub warnings: Vec<String>,
}

fn hours(seconds: Option<f64>) -> String {
    match seconds {
        None => "unknown age".to_string(),
        Some(s) => format!("{}h", (s / 3600.0).floor() as i64),
    }
}

fn node_idle_age(node: &NodeObs, now_epoch: f64) -> Option<f64> {
    node.touched_at_epoch
        .map(|touched| (now_epoch - touched).max(0.0))
}

/// A finding without a clearing command is not a report, it is noise: it is
/// dropped with a scan warning naming it, never emitted as a row.
fn checked(finding: Finding, warnings: &mut Vec<String>) -> Option<Finding> {
    if finding.subject.is_empty() || finding.basis.is_empty() || finding.clear_command.is_empty() {
        warnings.push(format!(
            "finding without a clearing command withheld: {}",
            finding_identity(&finding)
        ));
        return None;
    }
    Some(finding)
}

/// The command that moves a dirty ownerless worktree out of its class. A resolved
/// node resumes delivery; a node-less tree gets a live owner spawned into it,
/// which is the adoption path this fleet actually has.
fn dirty_clear_command(w: &WorktreeObs) -> String {
    match w.node_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => format!("/fno:target {id}"),
        None => format!(
            "fno agents spawn --cwd \"{}\" \"adopt this worktree: finish or hand off the uncommitted work\"",
            w.path
        ),
    }
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.is_empty())
}

/// The first reason wins: later evidence never rewrites why a dimension went dark.
fn mark_unknown(unknown: &mut [Option<String>; 4], dim: usize, reason: String) {
    if unknown[dim].is_none() {
        unknown[dim] = Some(reason);
    }
}

const STARTED: usize = 0;
const DONE_AHEAD: usize = 1;
const DIRTY: usize = 2;
const PR: usize = 3;

/// The pure classifier: observations in, one snapshot out.
pub fn classify(obs: &Observations) -> Snapshot {
    let mut warnings: Vec<String> = obs.warnings.clone();
    let mut findings: Vec<Finding> = Vec::new();
    let mut unknown: [Option<String>; 4] = [None, None, None, None];

    if !obs.graph_ok {
        mark_unknown(&mut unknown, STARTED, "graph unreadable".to_string());
        mark_unknown(&mut unknown, DONE_AHEAD, "graph unreadable".to_string());
        // PR candidates are enumerated from the graph: an unreadable graph is
        // zero candidates for a reason, which is the unmeasurable case, not a
        // clean zero.
        mark_unknown(&mut unknown, PR, "graph unreadable".to_string());
    }
    if !obs.claims_ok {
        mark_unknown(&mut unknown, STARTED, "claims unreadable".to_string());
    }
    if !obs.registry_ok {
        // The registry is an owner-candidate source: unreadable means the owner
        // question is unanswerable for every candidate with no other evidence.
        mark_unknown(&mut unknown, STARTED, "registry unreadable".to_string());
        mark_unknown(&mut unknown, DIRTY, "registry unreadable".to_string());
        mark_unknown(&mut unknown, PR, "registry unreadable".to_string());
    }
    if !obs.github_ok {
        mark_unknown(&mut unknown, PR, "github unreadable".to_string());
    }
    if obs.prs_unscanned {
        // Budget ran out before every PR state was read: zero reads is not zero
        // findings.
        mark_unknown(
            &mut unknown,
            PR,
            "budget spent before PR states were read".to_string(),
        );
    }
    for root in &obs.unscanned_roots {
        let reason = format!("budget spent before scanning {root}");
        mark_unknown(&mut unknown, DIRTY, reason.clone());
        mark_unknown(&mut unknown, DONE_AHEAD, reason);
    }

    let mut worktree_by_node: HashMap<&str, &WorktreeObs> = HashMap::new();
    for w in &obs.worktrees {
        if let Some(id) = non_empty(&w.node_id) {
            worktree_by_node.entry(id).or_insert(w);
        }
    }

    if unknown[STARTED].is_none() {
        classify_started(
            obs,
            &worktree_by_node,
            &mut unknown,
            &mut findings,
            &mut warnings,
        );
    }
    if unknown[DONE_AHEAD].is_none() {
        classify_done_ahead(
            obs,
            &worktree_by_node,
            &mut unknown,
            &mut findings,
            &mut warnings,
        );
    }
    if unknown[DIRTY].is_none() {
        classify_dirty(obs, &mut unknown, &mut findings, &mut warnings);
    }
    if unknown[PR].is_none() {
        classify_prs(obs, &mut unknown, &mut findings, &mut warnings);
    }

    findings.sort_by(|a, b| {
        severity(&a.kind)
            .cmp(&severity(&b.kind))
            .then_with(|| {
                let (left, right) = (a.age_s.unwrap_or(0.0), b.age_s.unwrap_or(0.0));
                right
                    .partial_cmp(&left)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.subject.cmp(&b.subject))
    });

    let mut dimensions: Vec<(String, DimensionState)> = Vec::new();
    for (index, dim) in DIMENSIONS.iter().enumerate() {
        let state = match &unknown[index] {
            Some(reason) => {
                warnings.push(format!("{dim}: {reason}"));
                DimensionState {
                    state: UNKNOWN_DIM.to_string(),
                    count: 0,
                    warning: Some(reason.clone()),
                }
            }
            None => DimensionState {
                state: MEASURED.to_string(),
                count: findings.iter().filter(|f| f.kind == *dim).count(),
                warning: None,
            },
        };
        dimensions.push(((*dim).to_string(), state));
    }
    let complete = dimensions.iter().all(|(_, d)| d.state == MEASURED);
    Snapshot {
        generated_at: crate::provider_cap::epoch_to_rfc3339(obs.now_epoch.floor() as i64),
        findings,
        dimensions,
        warnings,
        complete,
    }
}

/// Dimension 1: started nodes with free claims.
fn classify_started(
    obs: &Observations,
    worktree_by_node: &HashMap<&str, &WorktreeObs>,
    unknown: &mut [Option<String>; 4],
    findings: &mut Vec<Finding>,
    warnings: &mut Vec<String>,
) {
    for node in &obs.nodes {
        if node.status != "in_progress" {
            continue;
        }
        if node.is_container {
            // A container's in_progress is rolled up from its children, never
            // claimed: "claim free" is vacuous for a row that can never hold one.
            continue;
        }
        let claim_state = node.claim.as_ref().and_then(|c| c.state.as_deref());
        let Some(claim_state) = claim_state else {
            // A claim view with no state word is unreadable, not non-free.
            mark_unknown(
                unknown,
                STARTED,
                format!("claim state unreadable for {}", node.node_id),
            );
            continue;
        };
        if claim_state != "free" {
            // Non-free (live, suspect, stale, corrupted) excludes the node
            // outright, whatever the graph's owner fields say.
            continue;
        }
        let verdict = owner_set_verdict(&node.owner_probes, obs.registry_ok);
        if verdict == LIVE {
            continue;
        }
        if verdict == OWNER_UNKNOWN {
            mark_unknown(
                unknown,
                STARTED,
                format!("owner liveness unreadable for {}", node.node_id),
            );
            continue;
        }
        let idle = node_idle_age(node, obs.now_epoch);
        let wt = worktree_by_node.get(node.node_id.as_str()).copied();
        let ahead = node.ahead_count.or(wt.and_then(|w| w.ahead_count));
        let ahead_clause = match ahead {
            Some(count) => format!(", {count} commits ahead of origin/main"),
            None => String::new(),
        };
        let cwd = match wt {
            Some(w) => Some(w.path.clone()),
            None => non_empty(&node.worktree_path)
                .map(str::to_string)
                .or_else(|| node.cwd.clone()),
        };
        let finding = Finding {
            kind: KIND_STARTED.to_string(),
            subject: node.node_id.clone(),
            basis: format!(
                "in_progress, claim free, idle {}{ahead_clause}",
                hours(idle)
            ),
            clear_command: format!("/fno:target {}", node.node_id),
            node_id: Some(node.node_id.clone()),
            cwd,
            age_s: idle,
            ahead_count: ahead,
            ..Finding::default()
        };
        findings.extend(checked(finding, warnings));
    }
}

/// Dimension 2: done nodes whose worktree branch never reached main.
fn classify_done_ahead(
    obs: &Observations,
    worktree_by_node: &HashMap<&str, &WorktreeObs>,
    unknown: &mut [Option<String>; 4],
    findings: &mut Vec<Finding>,
    warnings: &mut Vec<String>,
) {
    for node in &obs.nodes {
        if node.status != "done" {
            continue;
        }
        let mut wt = worktree_by_node.get(node.node_id.as_str()).copied();
        if wt.is_none() {
            if let Some(path) = non_empty(&node.worktree_path) {
                wt = obs.worktrees.iter().find(|w| w.path == path);
            }
        }
        // No worktree resolved: nothing to measure for this node.
        let Some(wt) = wt else { continue };
        let Some(ahead) = wt.ahead_count else {
            mark_unknown(
                unknown,
                DONE_AHEAD,
                format!("ahead count unreadable for {}", wt.path),
            );
            continue;
        };
        if ahead <= 0 {
            continue;
        }
        let finding = Finding {
            kind: KIND_DONE_AHEAD.to_string(),
            subject: node.node_id.clone(),
            basis: format!(
                "done, worktree {} is {ahead} commits ahead of fresh origin/main; a squash merge explains it innocently, nothing else does",
                wt.path
            ),
            clear_command: format!(
                "cd {} && fno agents workspace worktree stranded --apply",
                wt.repo_root
            ),
            node_id: Some(node.node_id.clone()),
            cwd: Some(wt.path.clone()),
            ahead_count: Some(ahead),
            ..Finding::default()
        };
        findings.extend(checked(finding, warnings));
    }
}

/// Dimension 3: dirty worktrees with no authoritative live owner.
fn classify_dirty(
    obs: &Observations,
    unknown: &mut [Option<String>; 4],
    findings: &mut Vec<Finding>,
    warnings: &mut Vec<String>,
) {
    for w in &obs.worktrees {
        let Some(dirty) = w.dirty_count else {
            mark_unknown(unknown, DIRTY, format!("git status failed for {}", w.path));
            continue;
        };
        if dirty == 0 {
            continue;
        }
        let verdict = owner_set_verdict(&w.owner_probes, obs.registry_ok);
        if verdict == LIVE {
            continue;
        }
        if verdict == OWNER_UNKNOWN {
            mark_unknown(
                unknown,
                DIRTY,
                format!("owner liveness unreadable for {}", w.path),
            );
            continue;
        }
        let node_clause = match non_empty(&w.node_id) {
            Some(id) => format!("node {id}"),
            None => "no resolved node".to_string(),
        };
        let finding = Finding {
            kind: KIND_DIRTY.to_string(),
            subject: w.path.clone(),
            basis: format!("{dirty} dirty path(s), no live owner, {node_clause}"),
            clear_command: dirty_clear_command(w),
            node_id: w.node_id.clone(),
            cwd: Some(w.path.clone()),
            dirty_count: Some(dirty),
            ..Finding::default()
        };
        findings.extend(checked(finding, warnings));
    }
}

/// Dimension 4: ownerless open PRs (a PR whose owner reads unknown waits 24h).
/// The owner question comes before the age question: a gone owner reads ownerless
/// at any age.
fn classify_prs(
    obs: &Observations,
    unknown: &mut [Option<String>; 4],
    findings: &mut Vec<Finding>,
    warnings: &mut Vec<String>,
) {
    for pr in &obs.prs {
        let Some(state) = pr.state.as_deref() else {
            mark_unknown(
                unknown,
                PR,
                format!("github read failed for pr {}", pr.pr_number),
            );
            continue;
        };
        if state != "OPEN" {
            continue;
        }
        let Some(opened) = pr.opened_at_epoch else {
            mark_unknown(
                unknown,
                PR,
                format!("opened_at unreadable for pr {}", pr.pr_number),
            );
            continue;
        };
        let age = obs.now_epoch - opened;
        let verdict = owner_set_verdict(&pr.owner_probes, obs.registry_ok);
        if verdict == LIVE {
            continue;
        }
        if verdict == OWNER_UNKNOWN {
            if age <= PR_STALE_AFTER_S {
                continue;
            }
            mark_unknown(
                unknown,
                PR,
                format!("owner liveness unreadable for pr {}", pr.pr_number),
            );
            continue;
        }
        let mut basis = format!("open {} with no live owner", hours(Some(age)));
        if let Some(id) = non_empty(&pr.node_id) {
            basis.push_str(&format!(", node {id}"));
        }
        if let Some(url) = non_empty(&pr.pr_url) {
            basis.push_str(&format!(", {url}"));
        }
        let finding = Finding {
            kind: KIND_PR.to_string(),
            subject: format!("pr-{}", pr.pr_number),
            basis,
            clear_command: format!("/fno:ship pr check {}", pr.pr_number),
            node_id: pr.node_id.clone(),
            pr_number: Some(pr.pr_number),
            pr_url: pr.pr_url.clone(),
            age_s: Some(age),
            ..Finding::default()
        };
        findings.extend(checked(finding, warnings));
    }
}

/// One-screen human digest, house style: one physical line per paragraph,
/// findings as list items (bare lines under a paragraph fail the mail style gate).
pub fn snapshot_digest(snapshot: &Snapshot) -> String {
    let limit = 8;
    let mut lines: Vec<String> = vec![format!(
        "unfinished work: {} finding(s)",
        snapshot.findings.len()
    )];
    for finding in snapshot.findings.iter().take(limit) {
        lines.push(format!(
            "- {} {}: {} -> clear: {}",
            finding.kind, finding.subject, finding.basis, finding.clear_command
        ));
    }
    if snapshot.findings.len() > limit {
        lines.push(format!(
            "- {} more finding(s) not shown",
            snapshot.findings.len() - limit
        ));
    }
    lines.push(String::new());
    for (dim, state) in &snapshot.dimensions {
        if state.state == MEASURED {
            lines.push(format!("{dim}={} {MEASURED}", state.count));
        } else {
            let reason = non_empty(&state.warning).unwrap_or("unreadable");
            lines.push(format!("{dim}={UNKNOWN_DIM} ({reason})"));
        }
    }
    lines.join("\n")
}

/// Stable identity of the finding set: two sweeps that agree on the outcomes
/// produce one signature, so mail and escalation speak on change and a finding
/// that only aged does not re-speak.
pub fn snapshot_signature(snapshot: &Snapshot) -> String {
    let mut identities: Vec<String> = snapshot.findings.iter().map(finding_identity).collect();
    identities.sort();
    identities.join(";")
}

/// Finding identities the event lane has not already published.
pub fn fresh_identities(snapshot: &Snapshot, prev_signature: &str) -> HashSet<String> {
    let prev: HashSet<&str> = prev_signature
        .split(';')
        .filter(|s| !s.is_empty())
        .collect();
    snapshot
        .findings
        .iter()
        .map(finding_identity)
        .filter(|identity| !prev.contains(identity.as_str()))
        .collect()
}

pub fn snapshot_payload(snapshot: &Snapshot) -> Value {
    let findings: Vec<Value> = snapshot
        .findings
        .iter()
        .map(|f| {
            json!({
                "kind": f.kind,
                "subject": f.subject,
                "basis": f.basis,
                "clear_command": f.clear_command,
                "node_id": f.node_id,
                "pr_number": f.pr_number,
                "pr_url": f.pr_url,
                "cwd": f.cwd,
                "age_s": f.age_s.map(|age| age as i64),
                "ahead_count": f.ahead_count,
                "dirty_count": f.dirty_count,
            })
        })
        .collect();
    let mut dimensions = Map::new();
    let mut counts = Map::new();
    for (dim, state) in &snapshot.dimensions {
        dimensions.insert(
            dim.clone(),
            json!({"state": state.state, "count": state.count, "warning": state.warning}),
        );
        counts.insert(
            dim.clone(),
            if state.state == MEASURED {
                json!(state.count)
            } else {
                Value::Null
            },
        );
    }
    json!({
        "generated_at": snapshot.generated_at,
        "findings": findings,
        "dimensions": dimensions,
        "counts": counts,
        "warnings": snapshot.warnings,
        "complete": snapshot.complete,
    })
}

pub fn dedupe_key(identities: &[String]) -> String {
    let mut unique: Vec<&str> = identities.iter().map(String::as_str).collect();
    unique.sort_unstable();
    unique.dedup();
    let digest = Sha256::digest(unique.join("\n").as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..12].to_string()
}

/// One row per identity, the last seen winning, in the digest's severity order:
/// "clear the top finding first" names the finding the digest lists first.
fn unique_findings(findings: &[Finding]) -> Vec<&Finding> {
    let mut by_identity: HashMap<String, &Finding> = HashMap::new();
    for finding in findings {
        by_identity.insert(finding_identity(finding), finding);
    }
    let mut unique: Vec<&Finding> = by_identity.into_values().collect();
    unique.sort_by(|a, b| {
        severity(&a.kind)
            .cmp(&severity(&b.kind))
            .then_with(|| a.subject.cmp(&b.subject))
            .then_with(|| a.kind.cmp(&b.kind))
    });
    unique
}

pub fn question_text(findings: &[Finding], key: &str, unknown_dimensions: &[String]) -> String {
    let unique = unique_findings(findings);
    let mut shown: Vec<String> = unique
        .iter()
        .take(MAX_LISTED_ROWS)
        .map(|f| {
            format!(
                "{} {}: {} -> clear: {}",
                f.kind, f.subject, f.basis, f.clear_command
            )
        })
        .collect();
    if unique.len() > MAX_LISTED_ROWS {
        shown.push(format!("and {} more", unique.len() - MAX_LISTED_ROWS));
    }
    // An incomplete scan escalates what it did reach and names what it did not.
    // Withholding the whole ask made the sweep permanently mute wherever a
    // deleted worktree root can never be fetched again.
    let caveat = if unknown_dimensions.is_empty() {
        String::new()
    } else {
        let mut sorted: Vec<&str> = unknown_dimensions.iter().map(String::as_str).collect();
        sorted.sort_unstable();
        format!(
            " The scan was INCOMPLETE - {} went unread, so more findings may exist.",
            sorted.join(", ")
        )
    };
    format!(
        "[{MARKER}:{key}] The fleet watchdog found {} unfinished-work finding(s). Each names the one command that clears it.{caveat} Findings: {}",
        unique.len(),
        shown.join("; ")
    )
}

fn ask_line(findings: &[Finding]) -> String {
    unique_findings(findings)
        .first()
        .map(|f| f.clear_command.clone())
        .unwrap_or_default()
}

/// The id of the open question carrying `[<marker>:<key>]`, else None. One shared
/// dedupe fold for every durable question emitter: a marker key asked once stays
/// asked. A listing that cannot run reads as un-asked, and the ask door's own
/// dedup refusal catches the duplicate.
pub fn already_asked(key: &str, marker: &str) -> Option<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let listing = crate::outstanding::read(&cwd, crate::outstanding::LIVENESS_BUDGET).ok()?;
    let needle = format!("[{marker}:{key}]");
    listing
        .questions
        .into_iter()
        .find(|q| !q.id.is_empty() && q.question.contains(&needle))
        .map(|q| q.id)
}

/// Reconcile the report's fleet task to the measured findings: same key is a
/// duplicate, a changed set supersedes, an empty set closes. Outcome in
/// `none | duplicate | recorded | closed`. A store failure writes nothing.
pub fn escalate_unfinished(
    findings: &[Finding],
    store: &Path,
    cwd: &str,
    unknown_dimensions: &[String],
) -> Result<(String, String), String> {
    if findings.is_empty() {
        let key = dedupe_key(&[]);
        let (outcome, id) = crate::fleet_task::reconcile(store, MARKER, &key, cwd, "", "", true)?;
        return Ok(if outcome == "none" {
            ("none".to_string(), String::new())
        } else {
            (outcome, id)
        });
    }
    let unique = unique_findings(findings);
    let identities: Vec<String> = unique.iter().map(|f| finding_identity(f)).collect();
    let key = dedupe_key(&identities);
    let text = question_text(findings, &key, unknown_dimensions);
    let run = format!("clear the top finding first: {}", ask_line(findings));
    let (outcome, id) = crate::fleet_task::reconcile(store, MARKER, &key, cwd, &text, &run, false)?;
    Ok(if outcome == "asked" {
        ("recorded".to_string(), id)
    } else {
        (outcome, id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Oracle: the deleted Python unfinished-work and stale-escalate lanes.
    const GOLDEN: &str = include_str!("../../tests/golden/watchdog/unfinished.json");

    #[test]
    fn classify_and_renders_match_the_python_leg() {
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        for case in golden["classify"].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let obs: Observations = serde_json::from_value(case["observations"].clone()).unwrap();
            let snapshot = classify(&obs);
            assert_eq!(
                snapshot_payload(&snapshot).to_string(),
                case["payload"].to_string(),
                "{name}: payload"
            );
            assert_eq!(snapshot_digest(&snapshot), case["digest"], "{name}: digest");
            assert_eq!(
                snapshot_signature(&snapshot),
                case["signature"],
                "{name}: signature"
            );
            let identities: Vec<String> = unique_findings(&snapshot.findings)
                .iter()
                .map(|f| finding_identity(f))
                .collect();
            let key = dedupe_key(&identities);
            assert_eq!(key, case["key"], "{name}: key");
            if !snapshot.findings.is_empty() {
                let unknown: Vec<String> =
                    serde_json::from_value(case["unknown_dimensions"].clone()).unwrap();
                assert_eq!(
                    question_text(&snapshot.findings, &key, &unknown),
                    case["question"],
                    "{name}: question"
                );
                assert_eq!(ask_line(&snapshot.findings), case["ask"], "{name}: ask");
            }
        }
    }

    #[test]
    fn owner_verdicts_match_the_python_leg() {
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        for case in golden["owner_verdict"].as_array().unwrap() {
            let probe: OwnerProbe = serde_json::from_value(case["probe"].clone()).unwrap();
            assert_eq!(owner_verdict(&probe), case["verdict"], "{probe:?}");
        }
    }
}
