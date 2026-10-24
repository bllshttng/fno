//! The one shared retro routine: harvest -> classify -> dedup -> land.
//!
//! Both triggers (the retro-pending sentinel and the ship-gate fast path)
//! call it, so two triggers for one PR produce one deduped node set. IO is
//! injected (comments, existing nodes, landing seams), so the routine runs
//! without a live gh or graph.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::classify::{classify, classify_postmortem, BODY_CAP, DISPOSITION_ARCHIVE};
use super::dedup::{cv_ids_cited_in_nodes, dedup_candidates, existing_keys_from_nodes, pr_label};
use super::harvest::{self, GhRunner};
use super::keep_going::FollowupResult;
use super::land::{
    land_candidates, LandOptions, LandResult, MODE_AUTONOMOUS, OUTCOME_ACTIVE, OUTCOME_FAILED,
    OUTCOME_INBOX, OUTCOME_QUEUED, OUTCOME_SKIPPED,
};
use super::{Candidate, KIND_CARVEOUT};

#[derive(Clone, Debug, Default)]
pub struct TriageReport {
    pub pr_number: i64,
    pub carveout_count: usize,
    pub review_count: usize,
    pub deferred_count: usize,
    pub results: Vec<LandResult>,
    pub skipped_dupes: usize,
    pub uncited: Vec<Candidate>,
    pub warnings: Vec<String>,
    /// The review fetch could not fully read its source (gh down, or
    /// unparseable output). A flag, never a warning-string scan.
    pub gh_unavailable: bool,
    /// Carve-out ids harvested this run; the caller consumes them on a
    /// clean land so a later PR never re-files them.
    pub harvested_carveout_ids: Vec<String>,
    /// Carve-outs shown read-only: no owning session resolved for the PR,
    /// so they were neither filed nor consumed.
    pub readonly_carveout_count: usize,
    pub followups: Vec<FollowupResult>,
}

impl TriageReport {
    /// A harvest source was incomplete: the caller retains the trigger.
    pub fn partial(&self) -> bool {
        self.gh_unavailable
    }

    pub fn failed(&self) -> bool {
        self.results.iter().any(|r| r.outcome == OUTCOME_FAILED)
    }

    pub fn nothing_to_do(&self) -> bool {
        self.results.is_empty() && self.uncited.is_empty()
    }
}

/// The inputs of one PR triage. `comments: None` fetches them live.
pub struct TriageInput<'a> {
    pub carveout_root: &'a Path,
    pub pr_number: i64,
    pub session_ids: Option<Vec<String>>,
    pub completion_md: Option<PathBuf>,
    pub comments: Option<Vec<Value>>,
    pub resolved_ids: HashSet<String>,
    pub skipped_ids: HashSet<String>,
    pub commit_dates: Option<Vec<String>>,
    pub author_login: Option<String>,
    pub existing_nodes: &'a [Value],
    pub gh: &'a GhRunner,
    pub repo: Option<String>,
    pub carveouts_readonly: bool,
    pub origin_node_id: Option<String>,
    /// Whether the keep-going engine is armed for this run.
    pub keep_going: bool,
    pub project_root: Option<&'a Path>,
}

/// The node that shipped `pr` in `repo`, when exactly one matches. The
/// graph is global and bare numbers collide across projects, so no repo
/// context, or an ambiguous match, links nothing.
fn origin_from_graph(nodes: &[Value], pr: i64, repo: Option<&str>) -> Option<String> {
    let repo = repo?.to_lowercase();
    if pr <= 0 {
        return None;
    }
    let hits: HashSet<String> = nodes
        .iter()
        .filter(|n| n.get("pr_number").and_then(Value::as_i64) == Some(pr))
        .filter(|n| {
            crate::backlog::pr_link::repo_slug_from_url(n.get("pr_url").and_then(Value::as_str))
                .is_some_and(|s| s.to_lowercase() == repo)
        })
        .filter_map(|n| n.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    if hits.len() == 1 {
        hits.into_iter().next()
    } else {
        None
    }
}

pub fn triage_pr(input: TriageInput, land: LandOptions) -> TriageReport {
    let mut warnings = Vec::new();
    let pr = input.pr_number;
    let mut carveouts = harvest::harvest_carveouts(
        input.carveout_root,
        input.session_ids.as_deref(),
        Some(pr),
        &mut warnings,
    );
    // No owning session: carve-outs are session-scoped, so filing them under
    // an arbitrary PR mis-attributes another session's work. Show the count.
    let mut readonly_carveout_count = 0;
    if input.carveouts_readonly {
        readonly_carveout_count = carveouts.len();
        carveouts.clear();
    }

    let mut gh_unavailable = false;
    let comments = match input.comments {
        Some(c) => c,
        None => {
            let before = warnings.len();
            let fetched =
                harvest::fetch_review_comments(pr, input.repo.as_deref(), input.gh, &mut warnings);
            gh_unavailable = warnings[before..].iter().any(|w| w.contains("gh api"));
            fetched
        }
    };
    let reviews = harvest::harvest_reviews(
        &comments,
        &input.resolved_ids,
        &input.skipped_ids,
        input.commit_dates.as_deref(),
        input.author_login.as_deref(),
        Some(pr),
        &mut warnings,
    );
    let deferred = input
        .completion_md
        .as_deref()
        .map(|p| harvest::harvest_deferred_findings(p, Some(pr)))
        .unwrap_or_default();

    let (carveout_count, review_count, deferred_count) =
        (carveouts.len(), reviews.len(), deferred.len());
    let harvested_carveout_ids: Vec<String> =
        carveouts.iter().map(|c| c.source_id.clone()).collect();
    let raw: Vec<_> = carveouts
        .into_iter()
        .chain(reviews)
        .chain(deferred)
        .collect();
    let (cited, uncited) = classify(&raw);
    let existing = existing_keys_from_nodes(input.existing_nodes);
    let (mut kept, mut skipped) = dedup_candidates(cited, &existing);

    // Cross-boundary dedup: the PR-less sweep files a carve-out under
    // source_pr=None, so the hash key misses it here. The cv-id is stable
    // across both paths.
    let carveout_ids: Vec<String> = kept
        .iter()
        .filter(|c| c.extra_str("kind") == Some(KIND_CARVEOUT))
        .map(|c| c.source_id.clone())
        .collect();
    let already = cv_ids_cited_in_nodes(input.existing_nodes, &carveout_ids);
    if !already.is_empty() {
        let (drop, keep): (Vec<_>, Vec<_>) = kept.into_iter().partition(|c| {
            c.extra_str("kind") == Some(KIND_CARVEOUT) && already.contains(&c.source_id)
        });
        skipped.extend(drop);
        kept = keep;
    }

    // A follow-up points back at the node that shipped the PR: the
    // sentinel's node id first, else the one graph node carrying this PR.
    let caused_by = input
        .origin_node_id
        .filter(|s| !s.is_empty())
        .or_else(|| origin_from_graph(input.existing_nodes, pr, input.repo.as_deref()));
    let land = LandOptions { caused_by, ..land };
    let results = land_candidates(&kept, &land);

    // Keep-going: autonomous only, since interactive runs queue nodes for a
    // human ack that auto-dispatch would bypass.
    let followups = if land.mode == MODE_AUTONOMOUS && input.keep_going {
        let landed: Vec<LandResult> = results
            .iter()
            .filter(|r| r.node_id.is_some() && r.candidate.extra_str("kind") == Some(KIND_CARVEOUT))
            .cloned()
            .collect();
        super::keep_going::dispatch_followups(&landed, input.project_root, land.cwd.as_deref())
    } else {
        Vec::new()
    };

    TriageReport {
        pr_number: pr,
        carveout_count,
        review_count,
        deferred_count,
        results,
        skipped_dupes: skipped.len(),
        uncited,
        warnings,
        gh_unavailable,
        harvested_carveout_ids,
        readonly_carveout_count,
        followups,
    }
}

#[derive(Clone, Debug, Default)]
pub struct PostmortemReport {
    pub harvested: usize,
    /// source id -> node | inbox | archived | dupe | skipped | failed
    pub dispositions: BTreeMap<String, String>,
    pub results: Vec<LandResult>,
    pub stamped: usize,
    pub warnings: Vec<String>,
}

impl PostmortemReport {
    pub fn failed(&self) -> bool {
        self.dispositions.values().any(|d| d == "failed")
    }

    pub fn count(&self, disposition: &str) -> usize {
        self.dispositions
            .values()
            .filter(|d| *d == disposition)
            .count()
    }
}

/// Drain unconsumed postmortems: harvest -> disposition -> dedup -> land ->
/// stamp `consumed_at`. The stamp lands after the disposition, so an
/// interrupted run re-processes only the unstamped tail. A failed stamp
/// after a good land is caught by dedup next run.
pub fn triage_postmortems(
    dir: &Path,
    existing_nodes: &[Value],
    land: &LandOptions,
    stamp: &dyn Fn(&Path) -> Result<(), String>,
) -> PostmortemReport {
    let mut report = PostmortemReport::default();
    let items = super::postmortem::harvest_postmortems(dir, &mut report.warnings);
    report.harvested = items.len();
    let mut seen = existing_keys_from_nodes(existing_nodes);
    for item in items {
        let file = item
            .source_id
            .split_once(':')
            .map_or("", |(_, f)| f)
            .to_string();
        let mut do_stamp = |report: &mut PostmortemReport| match stamp(&dir.join(&file)) {
            Ok(()) => report.stamped += 1,
            Err(e) => report
                .warnings
                .push(format!("{}: consumed_at stamp failed: {e}", item.source_id)),
        };
        let (disposition, candidate) = classify_postmortem(&item, BODY_CAP);
        if disposition == DISPOSITION_ARCHIVE {
            report
                .dispositions
                .insert(item.source_id.clone(), "archived".to_string());
            do_stamp(&mut report);
            continue;
        }
        let Some(candidate) = candidate else { continue };
        let (kept, _) = dedup_candidates(vec![candidate], &seen);
        let Some(first) = kept.first() else {
            // Landed by a prior run whose stamp failed: consume, never re-file.
            report
                .dispositions
                .insert(item.source_id.clone(), "dupe".to_string());
            do_stamp(&mut report);
            continue;
        };
        seen.insert(format!(
            "{}:{}",
            pr_label(first.source_pr),
            first.content_hash
        ));
        let results = land_candidates(&kept, land);
        let outcome = results
            .first()
            .map_or(OUTCOME_FAILED.to_string(), |r| r.outcome.clone());
        report.results.extend(results);
        let label = match outcome.as_str() {
            OUTCOME_FAILED => {
                // Unstamped: the entry retries next run.
                report
                    .dispositions
                    .insert(item.source_id.clone(), "failed".to_string());
                continue;
            }
            OUTCOME_SKIPPED => "skipped",
            OUTCOME_ACTIVE | OUTCOME_QUEUED => "node",
            OUTCOME_INBOX => "inbox",
            _ => "inbox",
        };
        report
            .dispositions
            .insert(item.source_id.clone(), label.to_string());
        do_stamp(&mut report);
    }
    report
}

/// Stamp with the current UTC time.
pub fn stamp_now(path: &Path) -> Result<(), String> {
    super::postmortem::stamp_consumed(path, &crate::graph_store::now_isoformat())
}
