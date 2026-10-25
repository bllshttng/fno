//! Find open retro nodes whose source finding a later commit already
//! addressed.
//!
//! Retro files a node from a reviewer's inline comment. On a bot-reviewed PR
//! the fixing commit often lands after the comment, with nobody resolving the
//! thread, so the node is filed for work already done. This re-runs the
//! harvest's addressed-detection against each open retro node's source PR.
//! A PR whose thread state cannot be read is skipped with a warning, never
//! closed on uncertainty.

use std::collections::{BTreeMap, HashSet};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

use super::harvest::{self, GhRunner};
use crate::backlog::maintenance::validity::retro_trailer_re;

/// One open retro node whose originating finding is now addressed.
#[derive(Clone, Debug, PartialEq)]
pub struct AddressedFinding {
    pub node_id: String,
    pub pr_number: i64,
    pub comment_id: String,
    /// `owner/repo`, for gh scoping and the close reason.
    pub repo: Option<String>,
    /// `resolved/outdated thread` or `commit-after`.
    pub signal: String,
}

/// The injectable scan: `(nodes, include_planned, warnings)`. An `Err` means
/// the scan itself failed, which the anchor check reads as unresolvable.
pub type ScanFn = dyn Fn(&[Value], bool, &mut Vec<String>) -> Result<Vec<AddressedFinding>, String>;

/// One scannable retro node: `(node_id, pr_number, comment_id, repo, pr_url)`.
pub type Target = (String, i64, String, Option<String>, String);

/// The `Source:` permalink: the PR url scopes gh to the right repo, and the
/// `#discussion_r<id>` fragment is the REST comment id the addressed sets key on.
fn comment_url_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(https?://github\.com/[^\s)]+/pull/\d+)#discussion_r(\d+)")
            .expect("comment url regex")
    })
}

/// Open retro nodes carrying both a numeric retro trailer and a reviewer
/// comment permalink. A node with a `plan_path` is real tracked work: the
/// commit-after signal cannot tell a fully addressed finding from a residual,
/// so only untouched raw nodes qualify unless `include_planned`.
pub fn retro_targets(entries: &[Value], include_planned: bool) -> Vec<Target> {
    let mut out = Vec::new();
    for node in entries {
        let Some(nid) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        if !crate::backlog_ready::node_is_open(node) {
            continue;
        }
        if super::truthy(node.get("plan_path")) && !include_planned {
            continue;
        }
        let details = super::str_or_empty(node, "details");
        let Some(trailer) = retro_trailer_re().captures(&details) else {
            continue;
        };
        if &trailer[1] == "None" {
            continue;
        }
        let Some(url) = comment_url_re().captures(&details) else {
            continue;
        };
        let pr_url = url[1].to_string();
        // The permalink's own number is authoritative: it is self-consistent
        // with the comment id and repo.
        let Some(pr_number) = pr_url
            .rsplit_once("/pull/")
            .and_then(|(_, n)| n.parse::<i64>().ok())
        else {
            continue;
        };
        let repo = crate::backlog::pr_link::repo_slug_from_url(Some(&pr_url));
        out.push((nid.to_string(), pr_number, url[2].to_string(), repo, pr_url));
    }
    out
}

/// Open retro nodes whose finding the source PR now shows addressed: a
/// comment in a resolved or outdated thread, or a bot finding a later commit
/// addressed. Each `(repo, pr)` is fetched once.
pub fn scan_addressed_findings(
    entries: &[Value],
    include_planned: bool,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> Vec<AddressedFinding> {
    let mut by_pr: BTreeMap<(Option<String>, i64), Vec<(String, String)>> = BTreeMap::new();
    for (nid, pr, comment_id, repo, _url) in retro_targets(entries, include_planned) {
        by_pr.entry((repo, pr)).or_default().push((nid, comment_id));
    }
    let mut found = Vec::new();
    for ((repo, pr), items) in by_pr {
        let (threads, unavailable) =
            harvest::fetch_review_thread_state(pr, repo.as_deref(), gh, warnings);
        if unavailable {
            warnings.push(format!(
                "reconcile-findings: PR #{pr} thread state unavailable; skipping {} node(s) (never close on uncertainty)",
                items.len()
            ));
            continue;
        }
        let thread_addressed = harvest::addressed_ids_from_threads(&threads);
        let comments = harvest::fetch_review_comments(pr, repo.as_deref(), gh, warnings);
        let (dates, _) = harvest::fetch_pr_commit_dates(pr, repo.as_deref(), gh, warnings);
        let comment_addressed: HashSet<String> =
            harvest::addressed_ids_from_comments(&comments, &dates);
        for (nid, comment_id) in items {
            let signal = if thread_addressed.contains(&comment_id) {
                "resolved/outdated thread"
            } else if comment_addressed.contains(&comment_id) {
                "commit-after"
            } else {
                continue;
            };
            found.push(AddressedFinding {
                node_id: nid,
                pr_number: pr,
                comment_id,
                repo: repo.clone(),
                signal: signal.to_string(),
            });
        }
    }
    found
}

/// The live scan over the real gh runner, in `ScanFn` shape.
pub fn live_scan(
    entries: &[Value],
    include_planned: bool,
    warnings: &mut Vec<String>,
) -> Result<Vec<AddressedFinding>, String> {
    Ok(scan_addressed_findings(
        entries,
        include_planned,
        &harvest::default_gh_runner,
        warnings,
    ))
}
