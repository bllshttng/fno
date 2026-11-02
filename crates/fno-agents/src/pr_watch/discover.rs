//! Open-PR discovery, the per-PR read, the batch listing, and the pure
//! dispatch verdict.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::clock::{self, DoorError};
use super::state::{make_key, parse_key};

/// Under this much budget the reviews leg is not started: a sliver spawns a
/// gh process certain to time out. Its measured cost was 1.42s mean.
const REVIEWS_LEG_FLOOR_S: f64 = 1.0;

#[derive(Debug, Clone, Default)]
pub(crate) struct Candidate {
    pub node_id: String,
    pub pr: i64,
    pub repo_dir: Option<PathBuf>,
    pub repo_slug: Option<String>,
    pub source_session_id: Option<String>,
    pub source_harness: Option<String>,
    pub source_cwd: Option<String>,
    /// The latest `phase: ship` entry names who delivered the PR; the warm or
    /// cold ritual route prefers it over the node-birth source session.
    pub ship_session_id: Option<String>,
    pub ship_harness: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Observation {
    pub state: String,
    pub latest_review_ts: Option<String>,
    pub opened_at: Option<String>,
    pub merge_sha: Option<String>,
    pub is_draft: bool,
}

fn nonempty(node: &Value, key: &str) -> Option<String> {
    node.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn resolve_repo_dir(node: &Value) -> Option<PathBuf> {
    let raw = nonempty(node, "_resolved_cwd").or_else(|| nonempty(node, "cwd"))?;
    let path = PathBuf::from(raw);
    (path.is_dir() && path.join(".git").exists()).then_some(path)
}

/// Day distance over the date prefix; `None` on a stamp that will not parse.
pub(crate) fn days_between(a: &str, b: &str) -> Option<i64> {
    let day = |s: &str| chrono::NaiveDate::parse_from_str(s.get(..10)?, "%Y-%m-%d").ok();
    Some((day(a)? - day(b)?).num_days().abs())
}

/// Open, or done-at-PR-green within the grace window: finalize stamps
/// `completed_at` at PR-green, hours before the merge the watch must still see.
fn watchable(node: &Value, now_iso: &str, max_age_days: i64) -> bool {
    if nonempty(node, "superseded_by").is_some() {
        return false;
    }
    match nonempty(node, "completed_at") {
        None => true,
        Some(done) => days_between(&done, now_iso).is_some_and(|d| d <= max_age_days),
    }
}

fn latest_ship_session(node: &Value) -> (Option<String>, Option<String>) {
    let mut found = (None, None);
    for entry in node
        .get("sessions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if entry.get("phase").and_then(Value::as_str) != Some("ship") {
            continue;
        }
        if let Some(session) = nonempty(entry, "session_id") {
            found = (Some(session), nonempty(entry, "harness"));
        }
    }
    found
}

pub(crate) fn discover_open_prs(
    entries: &[Value],
    now_iso: &str,
    max_age_days: i64,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for node in entries {
        if !watchable(node, now_iso, max_age_days) {
            continue;
        }
        let refs = crate::backlog::node_ref::node_pr_refs(node);
        let repo_dir = resolve_repo_dir(node);
        for (pr, url) in refs {
            let (ship_session_id, ship_harness) = latest_ship_session(node);
            candidates.push(Candidate {
                node_id: nonempty(node, "id").unwrap_or_default(),
                pr,
                repo_dir: repo_dir.clone(),
                repo_slug: crate::backlog::pr_link::repo_slug_from_url(url.as_deref()),
                source_session_id: nonempty(node, "source_session_id"),
                source_harness: nonempty(node, "source_harness"),
                source_cwd: nonempty(node, "source_cwd"),
                ship_session_id,
                ship_harness,
            });
        }
    }
    candidates
}

/// The live graph rows, minus archived residents. Any non-graph tracker reads
/// as nothing to sweep rather than reading the wrong store.
pub(crate) fn graph_entries(path: &Path) -> Vec<Value> {
    if crate::tracker::backend_name(None) != "graph" {
        return Vec::new();
    }
    let store = crate::backlog::api::Store::new(path);
    crate::backlog::api::rows(&store)
        .map(|rows| {
            rows.into_iter()
                .filter(|row| row.get("archived_at").is_none_or(Value::is_null))
                .collect()
        })
        .unwrap_or_default()
}

/// One candidate's rich read: the merge-state leg, then the reviews leg
/// through the `status-pr-activity` op. `timeout_s` bounds the WHOLE call.
pub(crate) fn read_pr_state(
    cand: &Candidate,
    reviewers: &[String],
    timeout_s: f64,
) -> Result<Observation, DoorError> {
    let started = std::time::Instant::now();
    let cwd = cand.repo_dir.as_ref().map(|p| p.display().to_string());
    let (pr, slug, leg_cwd) = (cand.pr, cand.repo_slug.clone(), cwd.clone());
    let merge = clock::bounded_read(timeout_s, move || {
        crate::backlog::merge_state::query_pr_merge_state(
            pr,
            slug.as_deref(),
            leg_cwd.as_deref(),
            false,
        )
        .map_err(|e| format!("{e:?}"))
    })?
    .map_err(DoorError::Failed)?;
    let remaining = timeout_s - started.elapsed().as_secs_f64();
    if remaining < REVIEWS_LEG_FLOOR_S {
        return Err(DoorError::Failed(format!(
            "read_pr_state #{} left {remaining:.2}s of its {timeout_s:.1}s budget after the merge-state read",
            cand.pr
        )));
    }
    let here = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let answer = clock::door(
        &["authorized-merge"],
        Some(&json!({
            "op": "status-pr-activity",
            "cwd": cwd.unwrap_or(here),
            "slug": cand.repo_slug.clone().unwrap_or_default(),
            "pr": cand.pr,
            "reviewers": reviewers,
        })),
        remaining,
    )?;
    if let Some(error) = answer.get("error").filter(|e| !e.is_null()) {
        return Err(DoorError::Failed(format!(
            "read_pr_state #{} (reviews) failed: {error}",
            cand.pr
        )));
    }
    let text = |key: &str| answer.get(key).and_then(Value::as_str).map(str::to_string);
    Ok(Observation {
        state: merge.state,
        latest_review_ts: text("latest_review_ts"),
        opened_at: text("opened_at"),
        merge_sha: merge.merge_sha,
        is_draft: answer
            .get("is_draft")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// One REST open-PR listing per repository. A tracked key absent from a
/// successful listing is NOT_OPEN; a failed listing leaves its keys UNKNOWN
/// and counts as a sweep failure, so an outage cannot read as a clean sweep.
pub(crate) fn read_tracked_states(
    keys: &BTreeSet<String>,
) -> Result<(BTreeMap<String, String>, i64), DoorError> {
    const TIMEOUT_S: f64 = 30.0;
    let mut grouped: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    let mut states = BTreeMap::new();
    for key in keys {
        if let Some((repo, number)) = parse_key(key) {
            states.insert(format!("{repo}#{number}"), "UNKNOWN".to_string());
            grouped.entry(repo).or_default().insert(number);
        }
    }
    let here = std::env::current_dir()
        .unwrap_or_default()
        .display()
        .to_string();
    let mut failures = 0;
    for (repo, requested) in &grouped {
        let answer = clock::door(
            &["authorized-merge"],
            Some(
                &json!({"op": "status-pr-list", "slug": repo, "state": "open",
                         "cwd": here, "timeout_s": TIMEOUT_S}),
            ),
            TIMEOUT_S + 30.0,
        );
        let rows = match answer {
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(reason)) => {
                eprintln!("pr-watch: tracked-state sweep failed for {repo}: {reason}");
                failures += 1;
                continue;
            }
            Ok(answer) => match answer.get("rows").and_then(Value::as_array) {
                Some(rows) => rows.clone(),
                None => {
                    let reason = answer.get("error").cloned().unwrap_or(Value::Null);
                    eprintln!("pr-watch: tracked-state sweep failed for {repo}: {reason}");
                    failures += 1;
                    continue;
                }
            },
        };
        let mut returned = BTreeSet::new();
        for row in rows {
            let (Some(number), Some(state)) = (
                row.get("number").and_then(Value::as_i64),
                row.get("state").and_then(Value::as_str),
            ) else {
                continue;
            };
            if !requested.contains(&number) && state != "OPEN" {
                continue;
            }
            if let Some(key) = make_key(Some(repo), number) {
                states.insert(key, state.to_string());
                returned.insert(number);
            }
        }
        for number in requested.difference(&returned) {
            states.insert(format!("{repo}#{number}"), "NOT_OPEN".to_string());
        }
    }
    Ok((states, failures))
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum Verdict {
    Merge,
    Review,
    Noop(&'static str),
    Park(&'static str),
}

/// The pure verdict; first match wins. Zero configured reviewers skips the
/// review arm, and the merge arm still works.
pub(crate) fn decide(
    obs: &Observation,
    watermark: &Value,
    reviewers: &[String],
    merge_ready: bool,
    now_iso: &str,
    max_age_days: i64,
) -> Verdict {
    if obs.state == "MERGED" {
        let dispatched = watermark.get("merge_dispatched").and_then(Value::as_bool) == Some(true);
        return match (dispatched, merge_ready) {
            (true, _) => Verdict::Noop("merge-already-dispatched"),
            (false, false) => Verdict::Noop("merge-not-ready"),
            (false, true) => Verdict::Merge,
        };
    }
    if obs.state == "CLOSED" {
        return Verdict::Park("closed");
    }
    if let Some(age) = obs
        .opened_at
        .as_deref()
        .and_then(|o| days_between(o, now_iso))
    {
        if age > max_age_days {
            return Verdict::Park("max-age");
        }
    }
    if let (false, Some(latest)) = (reviewers.is_empty(), obs.latest_review_ts.as_deref()) {
        match watermark.get("last_review_ts").and_then(Value::as_str) {
            Some(last) if latest <= last => {}
            _ => return Verdict::Review,
        }
    }
    Verdict::Noop("no-new-activity")
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-01T00:00:00Z";

    fn obs(state: &str) -> Observation {
        Observation {
            state: state.into(),
            ..Default::default()
        }
    }

    #[test]
    fn the_verdict_precedence_holds() {
        let none = json!({});
        let bots = vec!["bot".to_string()];
        assert_eq!(
            decide(&obs("MERGED"), &none, &[], true, NOW, 14),
            Verdict::Merge
        );
        assert_eq!(
            decide(&obs("MERGED"), &none, &[], false, NOW, 14),
            Verdict::Noop("merge-not-ready")
        );
        assert_eq!(
            decide(
                &obs("MERGED"),
                &json!({"merge_dispatched": true}),
                &[],
                true,
                NOW,
                14
            ),
            Verdict::Noop("merge-already-dispatched")
        );
        assert_eq!(
            decide(&obs("CLOSED"), &none, &bots, true, NOW, 14),
            Verdict::Park("closed")
        );
        let mut old = obs("OPEN");
        old.opened_at = Some("2026-01-01T00:00:00Z".into());
        old.latest_review_ts = Some("2026-10-31T00:00:00Z".into());
        assert_eq!(
            decide(&old, &none, &bots, true, NOW, 14),
            Verdict::Park("max-age")
        );
    }

    #[test]
    fn only_strictly_newer_review_activity_fires() {
        let bots = vec!["bot".to_string()];
        let mut seen = obs("OPEN");
        seen.latest_review_ts = Some("2026-10-31T00:00:00Z".into());
        let at = |ts: &str| json!({"last_review_ts": ts});
        assert_eq!(
            decide(&seen, &json!({}), &bots, false, NOW, 14),
            Verdict::Review
        );
        assert_eq!(
            decide(&seen, &at("2026-10-30T00:00:00Z"), &bots, false, NOW, 14),
            Verdict::Review
        );
        assert_eq!(
            decide(&seen, &at("2026-10-31T00:00:00Z"), &bots, false, NOW, 14),
            Verdict::Noop("no-new-activity")
        );
        assert_eq!(
            decide(&seen, &json!({}), &[], false, NOW, 14),
            Verdict::Noop("no-new-activity")
        );
    }

    #[test]
    fn a_done_node_stays_watched_only_inside_the_grace_window() {
        let node = |done: &str| json!({"id": "n", "completed_at": done});
        assert!(watchable(&node("2026-10-25T00:00:00Z"), NOW, 14));
        assert!(!watchable(&node("2026-09-01T00:00:00Z"), NOW, 14));
        assert!(watchable(&json!({"id": "n"}), NOW, 14));
        assert!(!watchable(&json!({"superseded_by": "m"}), NOW, 14));
    }

    #[test]
    fn the_last_ship_session_wins() {
        let node = json!({"sessions": [
            {"phase": "ship", "session_id": "a", "harness": "codex"},
            {"phase": "execute", "session_id": "b"},
            {"phase": "ship", "session_id": "c"},
        ]});
        assert_eq!(latest_ship_session(&node), (Some("c".to_string()), None));
    }
}
