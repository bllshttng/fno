//! The by-lane scorecard: one record per node whose last execute session
//! ended in the window, folded into cells keyed
//! `(kind, harness, provider, model)`. Computation only; `calibration_cli`
//! owns argv, the reads and the render, matching the calibration split.
//!
//! Lane axes are never inferred from one another: harness comes from the
//! session row, model from the row's `observed_model` or the node-tagged
//! ledger row, provider only from the ledger row. A missing axis reads as
//! its own visible fallback (`unattributed` harness, `unrecorded` provider,
//! `unknown` model) rather than a dropped record, and the coverage block
//! reports what share of records carry each axis. An unread value never
//! reads as zero: an unmeasured cost, an unreadable friction store and an
//! absent bounce signal all emit null, and a node whose session time does
//! not parse is counted, not dropped silently.

use crate::context_window::normalize_model;
use crate::evals_trend::{round4, wilson_rate};
use crate::scoreboard_provider::{node_outcome, num_opt, parse_local, pct, percentile, round2};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

pub(crate) const UNATTRIBUTED: &str = "unattributed";
pub(crate) const UNRECORDED: &str = "unrecorded";
pub(crate) const UNKNOWN: &str = "unknown";

/// The per-PR outcome feeds the fold joins through the node's PR number.
/// `None` means the feed could not be read this run, so the column it feeds
/// stays null on every record instead of a measured zero.
#[derive(Default)]
pub struct OutcomeFeeds<'a> {
    /// Fail review verdicts per PR number (the latest verdict per reviewer
    /// and head, from the review_coverage + review_attestation rows).
    pub review_fails_by_pr: Option<&'a HashMap<i64, u64>>,
}

/// The verb part of `kind`: every execute/target session ran the delivery
/// pipeline, so `target/<difficulty>` is the only slice-1 kind.
const TARGET_VERB: &str = "target";

/// `host/owner/repo` from a GitHub PR URL, the same lowercase identity shape
/// `finalize::repo_identity_from_git_remote` produces for the origin remote:
/// the one comparison that keeps a foreign repo's PR number out of this
/// repo's joins. The `/pull/<n>` tail is cut first so the shared
/// remote-url identity read sees owner/repo only; a URL without one reads
/// as foreign (None), and so does anything the shared read refuses.
fn pr_url_identity(url: &str) -> Option<String> {
    let repo_url = &url[..url.find("/pull/")?];
    crate::finalize::repo_identity_from_remote_url(repo_url)
}

/// One batched ledger read, indexed by session id and by graph node id: the
/// two joins the lane resolution and the cost fallback need. Execution rows
/// only, the same filter the by-provider view applies.
#[derive(Default)]
pub struct LedgerIndex {
    by_session: HashMap<String, Vec<Value>>,
    by_node: HashMap<String, Vec<Value>>,
}

impl LedgerIndex {
    pub fn build(rows: &[Value]) -> LedgerIndex {
        let mut out = LedgerIndex::default();
        for row in rows {
            if row.get("type").and_then(Value::as_str) != Some("execution") {
                continue;
            }
            for sid in row
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sid) = sid.as_str() {
                    out.by_session
                        .entry(sid.to_string())
                        .or_default()
                        .push(row.clone());
                }
            }
            if let Some(nid) = row
                .get("graph_node_id")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                out.by_node
                    .entry(nid.to_string())
                    .or_default()
                    .push(row.clone());
            }
        }
        out
    }

    /// The ledger beside the graph store, at the same resolve the escalation
    /// view uses; a missing file is an empty index, so every lane axis falls
    /// back honestly instead of erroring.
    pub fn load() -> LedgerIndex {
        LedgerIndex::build(&crate::scoreboard_provider::ledger_rows())
    }

    fn first_by_session(&self, sid: &str) -> Option<&Value> {
        self.by_session.get(sid).and_then(|rows| rows.first())
    }

    fn rows_by_node(&self, nid: &str) -> &[Value] {
        self.by_node.get(nid).map(Vec::as_slice).unwrap_or(&[])
    }
}

/// The session's observed model id: an `{"kind": "observed", "model": ...}`
/// object or a bare string; anything else is unrecorded.
fn observed_model(session: &Value) -> Option<&str> {
    let v = session.get("observed_model")?;
    match v {
        Value::String(s) if !s.is_empty() => Some(s.as_str()),
        Value::Object(_) => {
            if v.get("kind").and_then(Value::as_str) == Some("observed") {
                v.get("model")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The lane axes one session row fills: harness and effort from the row,
/// model from the row's observed model or the ledger row joined by session
/// id, provider only ever from that ledger row's own field. The single lane
/// read; the blueprint credit (slice 3) reuses it verbatim so a credited
/// lane is built exactly the way the execute lane is.
fn session_lane(session: &Value, ledger: &LedgerIndex) -> (String, String, String, String) {
    let harness = session
        .get("harness")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNATTRIBUTED.to_string());
    let sid = session
        .get("session_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let ledger_row = ledger.first_by_session(sid);
    let model = observed_model(session)
        .or_else(|| {
            ledger_row
                .and_then(|r| r.get("model"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(normalize_model)
        .unwrap_or_else(|| UNKNOWN.to_string());
    // Never infer provider from harness or model: only a ledger row's
    // own provider field fills the axis.
    let provider = ledger_row
        .and_then(|r| r.get("provider"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| UNRECORDED.to_string());
    let effort = session
        .get("effort")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (harness, model, provider, effort)
}

/// One session's elapsed worker hours from its own span: None unstarted or
/// unended, negative spans excluded, matching the execute-sum read.
fn session_hours(session: &Value) -> Option<f64> {
    match (
        session.get("started_at").and_then(Value::as_str),
        session.get("ended_at").and_then(Value::as_str),
    ) {
        (Some(a), Some(b)) => parse_local(a)
            .zip(parse_local(b))
            .map(|(x, y)| (y - x).num_minutes() as f64 / 60.0)
            .filter(|h| *h > 0.0),
        _ => None,
    }
}

/// One build over the node set: the lane records plus the count of nodes
/// dropped because their last execute session's timestamps did not parse -
/// a silent drop would read the coverage denominator as smaller and cleaner
/// than the world is.
pub struct RecordSet {
    pub records: Vec<Value>,
    pub dropped_unparsed: u64,
}

/// One record per node: the lane of its last execute session plus the
/// outcome columns. `shared` marks a node whose execute sessions ran on more
/// than one (harness, model) lane, mirroring the by-provider view's shared
/// read. `bounce` is null when the graph carries no bounce signal at all
/// (no `reverted` flag and no `caused_by` fix node anywhere), and the
/// friction counts are null when the friction store could not be read, so
/// an unread source never prints as a measured zero.
///
/// The PR-keyed outcome columns ride the same rule one store narrower: a
/// node without an in-repo PR number has no CI, review or fix-of-fix surface
/// at all (null, not a measured zero), `repo_scope` (the cwd's
/// host/owner/repo) keeps a foreign repo's PR number out of the joins
/// entirely, a PR-bearing node is measured only where its feed proves a
/// read, and `fix_of_fix` is stamped afterwards by [`stamp_fix_of_fix`] once
/// the blame read knows which PRs it answers.
pub fn build_records(
    nodes: &[Value],
    ledger: &LedgerIndex,
    friction_by_node: Option<&BTreeMap<String, BTreeMap<String, u32>>>,
    feeds: &OutcomeFeeds<'_>,
    repo_scope: Option<&str>,
    since_days: i64,
    now: chrono::NaiveDateTime,
    domain: Option<&str>,
) -> RecordSet {
    // node_outcome context: every node by id, plus fix nodes keyed by the
    // origin they fix - the same two maps the by-provider view builds.
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    let mut fixes: HashMap<&str, Vec<&Value>> = HashMap::new();
    for n in nodes {
        if let Some(id) = n.get("id").and_then(Value::as_str) {
            by_id.insert(id, n);
            if let Some(origin) = n.get("caused_by").and_then(Value::as_str) {
                if !origin.is_empty() {
                    fixes.entry(origin).or_default().push(n);
                }
            }
        }
    }
    // The by-provider view's W4 gate: with no `reverted` flag and no
    // `caused_by` fix node anywhere, bounce is unmeasured for every node,
    // never a clean 0%.
    let w4_available = nodes.iter().any(|n| {
        n.get("reverted").is_some()
            || n.get("caused_by")
                .is_some_and(|v| !v.is_null() && *v != json!(false))
    });
    let cutoff = now - chrono::Duration::days(since_days);

    let mut records = Vec::new();
    let mut dropped_unparsed: u64 = 0;
    for node in nodes {
        // Archived rows read through the key's presence, null included; a
        // live row never carries the key.
        if node.get("archived_at").is_some() {
            continue;
        }
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        let sessions: &[Value] = node
            .get("sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let execs: Vec<&Value> = sessions
            .iter()
            .filter(|s| {
                matches!(
                    s.get("phase").and_then(Value::as_str),
                    Some("execute") | Some("target")
                )
            })
            .collect();
        // The last execute session decides the window: a node whose newest
        // ended_at (started_at as the un-ended fallback) sits outside it is
        // not this window's work. Ordered by parsed time, not string order,
        // so an offset-bearing stamp cannot misorder the race; a session
        // with no parseable time loses to any that parses.
        let last = execs
            .iter()
            .max_by_key(|s| {
                s.get("ended_at")
                    .and_then(Value::as_str)
                    .or_else(|| s.get("started_at").and_then(Value::as_str))
                    .and_then(parse_local)
            })
            .copied();
        let Some(last) = last else { continue };
        let end_raw = last
            .get("ended_at")
            .and_then(Value::as_str)
            .or_else(|| last.get("started_at").and_then(Value::as_str))
            .unwrap_or_default();
        let Some(ended) = parse_local(end_raw) else {
            dropped_unparsed += 1;
            continue;
        };
        if ended < cutoff || ended > now {
            continue;
        }

        let (harness, model, provider, effort) = session_lane(last, ledger);

        // Distinct (harness, model) lanes over ALL execute sessions decide
        // `shared`; the by-provider view counts the same way per node.
        let mut lanes: HashSet<(String, String)> = HashSet::new();
        let mut work_hours = 0.0;
        for s in &execs {
            lanes.insert((
                s.get("harness")
                    .and_then(Value::as_str)
                    .filter(|h| !h.is_empty())
                    .unwrap_or(UNATTRIBUTED)
                    .to_string(),
                observed_model(s)
                    .map(normalize_model)
                    .unwrap_or_else(|| UNKNOWN.to_string()),
            ));
            let hours = session_hours(s);
            if let Some(h) = hours {
                work_hours += h;
            }
        }

        // Cost: the node's own cost_sessions coerce unpriced rows to 0.0, so
        // a non-empty list is a measured sum even when it reads zero. The
        // ledger is the fallback and only counts when a cost parses. With
        // neither source the cost is unmeasured, never 0.
        let cost_sessions: &[Value] = node
            .get("cost_sessions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let cost_usd = if !cost_sessions.is_empty() {
            Some(
                cost_sessions
                    .iter()
                    .filter_map(|s| s.get("cost_usd").and_then(Value::as_f64))
                    .sum::<f64>(),
            )
        } else {
            let sums: Vec<f64> = ledger
                .rows_by_node(id)
                .iter()
                .filter_map(|r| num_opt(r.get("cost_usd")))
                .collect();
            (!sums.is_empty()).then(|| sums.iter().sum())
        };

        let merged = node.get("merge_status").and_then(Value::as_str) == Some("merged");
        let shipped_at = node
            .get("merged_at")
            .and_then(Value::as_str)
            .and_then(parse_local)
            .or_else(|| {
                node.get("completed_at")
                    .and_then(Value::as_str)
                    .and_then(parse_local)
            });
        let bounce = w4_available.then(|| node_outcome(id, shipped_at, &by_id, &fixes));
        // Repo scope: a node whose PR belongs to another repository joins no
        // local feed - its friction lives in another project's journal, its
        // PR number collides with this repo's line, and a verdict for it
        // would be somebody else's. No pr_url (or no origin remote to
        // compare) keeps the slice-1 read: the node stays in scope.
        let same_repo = match (node.get("pr_url").and_then(Value::as_str), repo_scope) {
            (None, _) => true,
            (Some(_), None) => true,
            (Some(url), Some(id)) => pr_url_identity(url).as_deref() == Some(id),
        };
        // The store maps, never the entry: a readable store with no events
        // for this node is a measured zero, and only an unreadable store
        // (None) stays null on the record.
        let ci_failure = if same_repo {
            friction_by_node.map(|m| {
                m.get(id)
                    .and_then(|f| f.get("ci_failure"))
                    .copied()
                    .unwrap_or(0)
            })
        } else {
            None
        };
        // The PR-keyed outcomes: no in-repo PR number, no surface. A red is
        // the same recorded ci_failure count the friction column reads, and
        // it is only measured when the review gate's coverage rows prove a
        // loop actually read that PR - without one, a missing row is an
        // unread, not a clean bill.
        let pr = if same_repo {
            node.get("pr_number").and_then(Value::as_i64)
        } else {
            None
        };
        let ci_red = match (pr, ci_failure, feeds.review_fails_by_pr) {
            (Some(p), Some(cf), Some(rv)) if rv.contains_key(&p) => Some(cf > 0),
            _ => None,
        };
        let review_findings = match (pr, feeds.review_fails_by_pr) {
            (Some(pr), Some(m)) => m.get(&pr).copied(),
            _ => None,
        };

        if let Some(d) = domain {
            if node.get("domain").and_then(Value::as_str) != Some(d) {
                continue;
            }
        }

        records.push(json!({
            "id": id,
            "kind": format!("{TARGET_VERB}/{}",
                node.get("difficulty").and_then(Value::as_str).unwrap_or("unknown")),
            "harness": harness,
            "provider": provider,
            "model": model,
            "effort": effort,
            "merged": merged,
            "pr": pr,
            "cost_usd": cost_usd,
            "work_hours": if work_hours > 0.0 { Some(round2(work_hours)) } else { None },
            "bounce": bounce,
            "ci_failure": ci_failure,
            "ci_red": ci_red,
            "review_findings": review_findings,
            "shared": lanes.len() > 1,
        }));

        // The blueprint credit: a node with a blueprint session row also
        // credits that session's lane with the node's downstream outcome,
        // under a `blueprint/<difficulty>` kind. The population gate stays
        // the target record's (the last execute session decides the
        // window); the effort measure is the blueprint session's own span,
        // not the execute sum.
        let blueprint = sessions
            .iter()
            .filter(|s| s.get("phase").and_then(Value::as_str) == Some("blueprint"))
            .max_by_key(|s| {
                s.get("ended_at")
                    .and_then(Value::as_str)
                    .or_else(|| s.get("started_at").and_then(Value::as_str))
                    .and_then(parse_local)
            });
        if let Some(bp) = blueprint {
            let (bp_harness, bp_model, bp_provider, bp_effort) = session_lane(bp, ledger);
            let bp_hours = session_hours(bp);
            records.push(json!({
                "id": id,
                "kind": format!("blueprint/{}",
                    node.get("difficulty").and_then(Value::as_str).unwrap_or("unknown")),
                "harness": bp_harness,
                "provider": bp_provider,
                "model": bp_model,
                "effort": bp_effort,
                "merged": merged,
                "pr": pr,
                "cost_usd": cost_usd,
                "work_hours": bp_hours.map(round2),
                "bounce": bounce,
                "ci_failure": ci_failure,
                "ci_red": ci_red,
                "review_findings": review_findings,
                "shared": lanes.len() > 1,
            }));
        }
    }
    RecordSet {
        records,
        dropped_unparsed,
    }
}

/// Stamps the fix-of-fix verdict onto each record after the blame read: the
/// verdict set is keyed by PR number and only computed for the PRs this fold
/// carries, so the read blames nothing else. Every record gains
/// `fix_answered` (the walk ran; this PR is or is not one of its fix
/// deliveries), so a cell can read "no fix PRs walked" apart from a walk
/// that never ran - one carries coverage 0, the other stays null. A PR
/// inside the map with no measurable verdict (blame read failed) answers
/// without entering the rate: `fix_of_fix` stays null there, never a
/// measured clean.
pub fn stamp_fix_of_fix(records: &mut [Value], fix_by_pr: &HashMap<u64, Option<bool>>) {
    for r in records.iter_mut() {
        r["fix_answered"] = json!(false);
        let Some(pr) = r.get("pr").and_then(Value::as_u64) else {
            continue;
        };
        if let Some(verdict) = fix_by_pr.get(&pr) {
            r["fix_answered"] = json!(true);
            r["fix_of_fix"] = json!(verdict);
        }
    }
}

/// The fix-of-fix verdicts for this fold: the PR set the records carry,
/// walked on the merged line of the repo at `cwd`. `None` on a non-repo cwd
/// or a failed walk - the column then reads null with its coverage saying
/// so, never a measured clean.
pub fn fix_by_pr(
    records: &[Value],
    cwd: &std::path::Path,
    since_days: i64,
) -> Option<HashMap<u64, Option<bool>>> {
    let wanted: HashSet<u64> = records
        .iter()
        .filter_map(|r| r.get("pr").and_then(Value::as_u64))
        .collect();
    if wanted.is_empty() {
        return Some(HashMap::new());
    }
    // The repo root: the nearest ancestor holding .git (a worktree's is a
    // file). Outside a repo the feed is unreadable, not clean.
    let Some(repo) = repo_root(cwd) else {
        eprintln!(
            "fno backlog calibration: no git repo above {}; the fix-of-fix column reads as unknown",
            cwd.display()
        );
        return None;
    };
    let rev = crate::stability_report::scorecard_rev(&repo);
    match crate::stability_report::fix_of_fix_by_pr(
        &repo,
        &rev,
        since_days + 7,
        7,
        chrono::Utc::now(),
        &wanted,
    ) {
        Ok(map) => Some(map.into_iter().collect()),
        Err(err) => {
            eprintln!(
                "fno backlog calibration: could not walk the merged line ({err}); \
the fix-of-fix column reads as unknown rather than a measured zero."
            );
            None
        }
    }
}

/// The repo root for the git-backed feeds: the nearest ancestor holding
/// .git (a worktree's is a file). `None` outside a repo - the caller owns
/// the message, since each feed degrades differently.
fn repo_root(cwd: &std::path::Path) -> Option<std::path::PathBuf> {
    std::fs::canonicalize(cwd)
        .unwrap_or_else(|_| cwd.to_path_buf())
        .ancestors()
        .find(|p| p.join(".git").exists())
        .map(|p| p.to_path_buf())
}

/// The (commit type, surface) parts per PR behind the second kind part: the
/// same repo discovery and merged-line walk the fix-of-fix feed uses, read
/// once per fold when the kind runs deeper than `verb/difficulty`. `None`
/// on a non-repo cwd or a failed walk - the kinds then keep their depth-1
/// shape after the caller prints why, never a guessed type.
pub fn kind_parts_by_pr(
    records: &[Value],
    cwd: &std::path::Path,
    since_days: i64,
) -> Option<HashMap<u64, Option<(&'static str, &'static str)>>> {
    let wanted: HashSet<u64> = records
        .iter()
        .filter_map(|r| r.get("pr").and_then(Value::as_u64))
        .collect();
    if wanted.is_empty() {
        return Some(HashMap::new());
    }
    let Some(repo) = repo_root(cwd) else {
        eprintln!(
            "fno backlog calibration: no git repo above {}; the kind stays at its verb/difficulty depth",
            cwd.display()
        );
        return None;
    };
    let rev = crate::stability_report::scorecard_rev(&repo);
    match crate::stability_report::kind_parts_by_pr(
        &repo,
        &rev,
        since_days + 7,
        chrono::Utc::now(),
        &wanted,
    ) {
        Ok(map) => Some(map.into_iter().collect()),
        Err(err) => {
            eprintln!(
                "fno backlog calibration: could not walk the merged line ({err}); \
the kind stays at its verb/difficulty depth rather than a guessed type."
            );
            None
        }
    }
}

/// Stamps the second kind part onto each record at depth 2 or 3: the merge
/// commit's conventional type, then the type-surface pair, joined onto the
/// depth-1 kind with `/` and `-`. A record whose PR is missing from the
/// parts map (no PR, a foreign repo, a merge outside the walked window)
/// reads `unknown` - the bucket stays visible, so the deeper grouping never
/// quietly rewrites the population a cell covers.
pub fn stamp_kind_parts(
    records: &mut [Value],
    parts: &HashMap<u64, Option<(&'static str, &'static str)>>,
    depth: u8,
) {
    if depth < 2 {
        return;
    }
    for r in records.iter_mut() {
        let (kind_type, kind_surface) = match r
            .get("pr")
            .and_then(Value::as_u64)
            .and_then(|pr| parts.get(&pr))
        {
            Some(Some((kind_type, kind_surface))) => (*kind_type, *kind_surface),
            _ => (UNKNOWN, UNKNOWN),
        };
        let base = r
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let suffix = if depth >= 3 {
            format!("{kind_type}-{kind_surface}")
        } else {
            kind_type.to_string()
        };
        r["kind"] = json!(format!("{base}/{suffix}"));
    }
}

/// Folds records into one cell per `(kind, harness, provider, model)`.
#[derive(Default)]
struct CellAcc {
    nodes: u64,
    merged: u64,
    merged_with_cost: u64,
    bounced: u64,
    bounce_known: u64,
    spend: f64,
    measured_cost: bool,
    work_hours: Vec<f64>,
    ci_failure: u64,
    friction_readable: u64,
    ci_red: u64,
    ci_red_measured: u64,
    review_findings: u64,
    review_measured: u64,
    fix_of_fix: u64,
    fix_measured: u64,
    fix_answered: u64,
    fix_walked: u64,
    shared: u64,
}

pub fn fold_cells(records: &[Value]) -> Vec<Value> {
    let mut cells: BTreeMap<(String, String, String, String), CellAcc> = BTreeMap::new();
    for r in records {
        let key = (
            r["kind"].as_str().unwrap_or_default().to_string(),
            r["harness"].as_str().unwrap_or_default().to_string(),
            r["provider"].as_str().unwrap_or_default().to_string(),
            r["model"].as_str().unwrap_or_default().to_string(),
        );
        let c = cells.entry(key).or_default();
        c.nodes += 1;
        let merged = r["merged"].as_bool() == Some(true);
        if merged {
            c.merged += 1;
            if let Some(b) = r["bounce"].as_str() {
                c.bounce_known += 1;
                if b == "bounced" || b == "reverted" {
                    c.bounced += 1;
                }
            }
        }
        if let Some(cost) = num_opt(r.get("cost_usd")) {
            c.spend += cost;
            c.measured_cost = true;
            if merged {
                c.merged_with_cost += 1;
            }
        }
        if let Some(h) = num_opt(r.get("work_hours")) {
            c.work_hours.push(h);
        }
        // Both friction columns ride one store read: readable or null
        // together.
        if let Some(ci) = r["ci_failure"].as_u64() {
            c.friction_readable += 1;
            c.ci_failure += ci;
        }
        // The PR-keyed outcome columns each count their own denominator: a
        // merged node with a PR is measured whenever its feed was readable,
        // so a cell's rate never dilutes across nodes the feed cannot see.
        if merged {
            if r["ci_red"].as_bool().is_some() {
                c.ci_red_measured += 1;
                if r["ci_red"].as_bool() == Some(true) {
                    c.ci_red += 1;
                }
            }
            if let Some(f) = r["review_findings"].as_u64() {
                c.review_measured += 1;
                c.review_findings += f;
            }
            // The walk mark rides on every record the stamp pass touched, so
            // a cell with no marks at all is a walk that never ran, not a
            // walk that found no fix deliveries.
            if r.get("fix_answered").is_some() {
                c.fix_walked += 1;
                if r["fix_answered"].as_bool() == Some(true) {
                    c.fix_answered += 1;
                }
            }
            if let Some(v) = r["fix_of_fix"].as_bool() {
                c.fix_measured += 1;
                if v {
                    c.fix_of_fix += 1;
                }
            }
        }
        if r["shared"].as_bool() == Some(true) {
            c.shared += 1;
        }
    }
    let mut out: Vec<Value> = cells
        .into_iter()
        .map(|((kind, harness, provider, model), c)| {
            let merged_pct = if c.nodes > 0 {
                Some(pct(c.merged, c.nodes))
            } else {
                None
            };
            // The money columns stay null with no measured cost, and the
            // per-merged price divides by the merged nodes that actually
            // carry a cost, so unpriced work never dilutes the rate toward
            // cheap.
            let spend_usd = if c.measured_cost {
                Some(round2(c.spend))
            } else {
                None
            };
            let cost_per_merged = if c.merged_with_cost > 0 {
                Some(round2(c.spend / c.merged_with_cost as f64))
            } else {
                None
            };
            let bounce_pct = if c.bounce_known > 0 {
                Some(pct(c.bounced, c.bounce_known))
            } else {
                None
            };
            let per_node = |sum: u64| -> Option<f64> {
                if c.friction_readable > 0 {
                    Some(round2(sum as f64 / c.nodes.max(1) as f64))
                } else {
                    None
                }
            };
            // The Wilson score interval over the merged share, the same
            // closed form the eval trend emits: the interval is computed
            // from the cell's own counts, so it always reads when the cell
            // has nodes.
            let (_, ci_lo, ci_hi) = wilson_rate(c.nodes as usize, c.merged as usize);
            // An outcome rate measures only the merged nodes its feed can
            // see; with none measured the rate is null and its coverage
            // says so - never a clean 0%.
            let rate = |hits: u64, measured: u64| -> Option<i64> {
                (measured > 0).then(|| pct(hits, measured))
            };
            let cover =
                |measured: u64| -> Option<i64> { (c.merged > 0).then(|| pct(measured, c.merged)) };
            json!({
                "kind": kind,
                "harness": harness,
                "provider": provider,
                "model": model,
                "nodes": c.nodes,
                "merged": c.merged,
                "merged_pct": merged_pct,
                "merged_ci95": [round4(ci_lo), round4(ci_hi)],
                "spend_usd": spend_usd,
                "cost_per_merged_usd": cost_per_merged,
                "median_work_hours": percentile(&c.work_hours, 50),
                "bounce_pct": bounce_pct,
                "ci_failure_per_node": per_node(c.ci_failure),
                "ci_red_pct": rate(c.ci_red, c.ci_red_measured),
                "ci_red_coverage_pct": cover(c.ci_red_measured),
                "review_findings_per_node": if c.review_measured > 0 {
                    Some(round2(c.review_findings as f64 / c.review_measured as f64))
                } else {
                    None
                },
                "review_findings_coverage_pct": cover(c.review_measured),
                "fix_of_fix_pct": rate(c.fix_of_fix, c.fix_measured),
                "fix_of_fix_coverage_pct": if c.fix_walked > 0 {
                    cover(c.fix_answered)
                } else {
                    None
                },
                "shared": c.shared,
            })
        })
        .collect();
    // Unattributed-harness cells sort last so the readable lanes lead; the
    // rest stays kind/harness/provider/model ordered.
    out.sort_by(|x, y| {
        (x["harness"].as_str() == Some(UNATTRIBUTED))
            .cmp(&(y["harness"].as_str() == Some(UNATTRIBUTED)))
            .then_with(|| x["kind"].as_str().cmp(&y["kind"].as_str()))
            .then_with(|| x["harness"].as_str().cmp(&y["harness"].as_str()))
            .then_with(|| x["provider"].as_str().cmp(&y["provider"].as_str()))
            .then_with(|| x["model"].as_str().cmp(&y["model"].as_str()))
    });
    out
}

/// The coverage block: what share of records carry each lane axis and a
/// measured cost. The unattributed share is the harness number's gap, and
/// `dropped_unparsed` counts the nodes excluded because their session time
/// did not parse.
pub fn coverage(records: &[Value], dropped_unparsed: u64) -> Value {
    let total = records.len() as u64;
    let axis = |k: &str| -> u64 {
        records
            .iter()
            .filter(|r| {
                let v = r[k].as_str().unwrap_or_default();
                !v.is_empty() && v != UNATTRIBUTED && v != UNRECORDED && v != UNKNOWN
            })
            .count() as u64
    };
    json!({
        "nodes": total,
        "harness_pct": pct(axis("harness"), total),
        "provider_pct": pct(axis("provider"), total),
        "model_pct": pct(axis("model"), total),
        "cost_pct": pct(
            records
                .iter()
                .filter(|r| num_opt(r.get("cost_usd")).is_some())
                .count() as u64,
            total,
        ),
        "dropped_unparsed": dropped_unparsed,
    })
}

/// Per-PR fail-review counts from the journal, the feed behind
/// `review_findings_per_node`: one batched read over the same journal the
/// friction read walks. `review_coverage` rows map each PR to the heads its
/// rounds read; `review_attestation` rows give each (reviewer, head) pair's
/// verdict, and the pair's LATEST verdict decides - a reviewer who failed a
/// head and passed the same head after the answers landed counts one
/// resolved round, not findings. An unreadable store reports false so the
/// column reads null, never a measured zero.
pub fn review_fails_by_pr(journal: &std::path::Path) -> (HashMap<i64, u64>, bool) {
    let query = crate::event_store::EventQuery {
        types: vec![
            "review_coverage".to_string(),
            "review_attestation".to_string(),
        ],
        ..Default::default()
    };
    let rows = match crate::event_store::query_events(journal, &query) {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!(
                "fno backlog calibration: could not read the review event store ({err}); \
the review findings column reads as unknown rather than a measured zero."
            );
            return (HashMap::new(), false);
        }
    };
    let mut keyed: Vec<(i64, &str)> = rows.iter().map(|r| (r.ts_ms, r.line.as_str())).collect();
    keyed.sort_by_key(|(ts, _)| *ts);
    let mut heads_by_pr: HashMap<i64, HashSet<String>> = HashMap::new();
    let mut latest_by_reviewer_head: HashMap<(String, String), bool> = HashMap::new();
    for (_, line) in &keyed {
        let Ok(env) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &env["data"];
        match env["type"].as_str() {
            Some("review_coverage") => {
                if let (Some(pr), Some(head)) = (
                    data.get("pr").and_then(Value::as_i64),
                    data.get("head_sha").and_then(Value::as_str),
                ) {
                    heads_by_pr.entry(pr).or_default().insert(head.to_string());
                }
            }
            Some("review_attestation") => {
                if let (Some(reviewer), Some(head), Some(verdict)) = (
                    data.get("reviewer").and_then(Value::as_str),
                    data.get("head_sha").and_then(Value::as_str),
                    data.get("verdict").and_then(Value::as_str),
                ) {
                    // The same reviewer arrives with and without the leading
                    // slash the gate strips on both sides; key on one form or
                    // a pass and a fail split into two identities.
                    let name = reviewer.trim_start_matches('/');
                    latest_by_reviewer_head
                        .insert((name.to_string(), head.to_string()), verdict == "fail");
                }
            }
            _ => {}
        }
    }
    let mut fails_by_pr: HashMap<i64, u64> = HashMap::new();
    for (pr, heads) in heads_by_pr {
        let fails: u64 = latest_by_reviewer_head
            .iter()
            .filter(|((_, head), is_fail)| heads.contains(head) && **is_fail)
            .count() as u64;
        fails_by_pr.insert(pr, fails);
    }
    (fails_by_pr, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A merged node with one execute session; cost_sessions empty unless a
    /// cost is given, so the ledger fallback can fill it.
    fn node(id: &str, sessions: Value, difficulty: &str, cost: Option<f64>) -> Value {
        let mut n = json!({
            "id": id,
            "status": "done",
            "difficulty": difficulty,
            "merge_status": "merged",
            "completed_at": "2026-10-15T12:00:00Z",
            "sessions": sessions,
        });
        if let Some(c) = cost {
            n["cost_sessions"] = json!([{"session_id": "s1", "cost_usd": c}]);
        }
        n
    }

    /// Attaches an in-repo PR to a fixture node: the number plus a pr_url
    /// naming this repo, the shape the same-repo scope gate reads. The pair
    /// grants the CI, review and fix-of-fix surfaces.
    fn with_pr_url(mut n: Value, pr: i64) -> Value {
        n["pr_number"] = json!(pr);
        n["pr_url"] = json!(format!("https://github.com/bllshttng/fno/pull/{pr}"));
        n
    }

    /// One execute session ending `ended_at` (None: started but never
    /// ended); harness/model optional.
    fn session(
        harness: Option<&str>,
        model: Option<&str>,
        sid: &str,
        ended_at: Option<&str>,
    ) -> Value {
        let mut s = json!({
            "phase": "execute",
            "session_id": sid,
            "started_at": "2026-10-14T10:00:00Z",
        });
        if let Some(e) = ended_at {
            s["ended_at"] = json!(e);
        }
        if let Some(h) = harness {
            s["harness"] = json!(h);
        }
        if let Some(m) = model {
            s["observed_model"] = json!({"kind": "observed", "model": m});
        }
        s
    }

    fn ledger_row(
        sids: &[&str],
        provider: &str,
        model: &str,
        nid: Option<&str>,
        cost: f64,
    ) -> Value {
        let mut r = json!({
            "type": "execution",
            "sessions": sids,
            "provider": provider,
            "model": model,
            "cost_usd": cost,
        });
        if let Some(n) = nid {
            r["graph_node_id"] = json!(n);
        }
        r
    }

    const NOW: &str = "2026-10-20T12:00:00Z";

    fn no_feeds() -> OutcomeFeeds<'static> {
        OutcomeFeeds::default()
    }

    fn records(nodes: &[Value], ledger_rows: &[Value]) -> Vec<Value> {
        build_records(
            nodes,
            &LedgerIndex::build(ledger_rows),
            Some(&BTreeMap::new()),
            &no_feeds(),
            None,
            28,
            parse_local(NOW).unwrap(),
            None,
        )
        .records
    }

    #[test]
    fn grouping_and_cost_fallback() {
        // AC1-HP: two merged nodes on the same lane, costs 10 and 20, fold
        // into one cell reading cost_per_merged_usd 15.
        let s1 = "sess-1";
        let s2 = "sess-2";
        let nodes = vec![
            node(
                "x-aaaa",
                json!([session(
                    Some("claude"),
                    None,
                    s1,
                    Some("2026-10-15T10:00:00Z")
                )]),
                "medium",
                Some(10.0),
            ),
            node(
                "x-bbbb",
                json!([session(
                    Some("claude"),
                    None,
                    s2,
                    Some("2026-10-16T10:00:00Z")
                )]),
                "medium",
                Some(20.0),
            ),
        ];
        let ledger_rows = vec![
            ledger_row(&[s1], "anthropic", "sonnet", Some("x-aaaa"), 1.0),
            ledger_row(&[s2], "anthropic", "sonnet", Some("x-bbbb"), 2.0),
        ];
        // One `reverted` key anywhere switches the bounce read on, the same
        // gate the by-provider view applies.
        let mut w4_nodes = nodes;
        w4_nodes[0]["reverted"] = json!(false);
        let recs = records(&w4_nodes, &ledger_rows);
        assert_eq!(recs.len(), 2, "{recs:?}");
        let cells = fold_cells(&recs);
        assert_eq!(cells.len(), 1, "one lane cell: {cells:?}");
        let c = &cells[0];
        assert_eq!(c["kind"], "target/medium");
        assert_eq!(c["harness"], "claude");
        assert_eq!(c["provider"], "anthropic");
        assert_eq!(c["model"], "sonnet", "ledger model fills the axis: {c}");
        assert_eq!(c["nodes"], 2);
        assert_eq!(c["merged"], 2);
        assert_eq!(c["merged_pct"], 100);
        assert_eq!(c["spend_usd"], 30.0);
        assert_eq!(c["cost_per_merged_usd"], 15.0);
        // With a reverted flag anywhere in the graph the bounce read runs,
        // so the cell carries a real 0% rather than null.
        assert!(c["bounce_pct"].is_i64(), "w4 signal present: {c}");

        // Cost fallback: a node with no cost_sessions takes its cost from
        // the ledger row tagged with its graph node id.
        let s3 = "sess-3";
        let nodes = vec![node(
            "x-cccc",
            json!([session(
                Some("claude"),
                None,
                s3,
                Some("2026-10-17T10:00:00Z")
            )]),
            "medium",
            None,
        )];
        let ledger_rows = vec![ledger_row(
            &[s3],
            "anthropic",
            "sonnet",
            Some("x-cccc"),
            7.5,
        )];
        let recs = records(&nodes, &ledger_rows);
        assert_eq!(recs[0]["cost_usd"], 7.5, "ledger cost fills in: {recs:?}");
        let cov = coverage(&recs, 0);
        assert_eq!(cov["nodes"], 1);
        assert_eq!(cov["cost_pct"], 100);
    }

    #[test]
    fn unattributed_never_dropped() {
        // A node whose execute session row carries no harness lands in the
        // visible unattributed bucket, coverage.harness_pct drops, and the
        // unmeasured columns read null: no bounce signal, no friction
        // store, no cost anywhere.
        let nodes = vec![
            node(
                "x-dddd",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-4",
                    Some("2026-10-15T10:00:00Z")
                )]),
                "high",
                Some(5.0),
            ),
            node(
                "x-eeee",
                json!([session(None, None, "sess-5", Some("2026-10-16T10:00:00Z"))]),
                "high",
                None,
            ),
        ];
        let built = build_records(
            &nodes,
            &LedgerIndex::build(&[]),
            None,
            &no_feeds(),
            None,
            28,
            parse_local(NOW).unwrap(),
            None,
        );
        assert_eq!(built.records.len(), 2, "{:?}", built.records);
        let cells = fold_cells(&built.records);
        let unattr = cells
            .iter()
            .find(|c| c["harness"] == UNATTRIBUTED)
            .expect("unattributed bucket kept");
        assert_eq!(unattr["nodes"], 1);
        assert_eq!(
            unattr["provider"], UNRECORDED,
            "no ledger row, no provider: {unattr}"
        );
        assert_eq!(unattr["model"], UNKNOWN);
        assert_eq!(
            unattr["bounce_pct"],
            Value::Null,
            "no w4 fields, no fake 0%: {unattr}"
        );
        assert_eq!(
            unattr["ci_failure_per_node"],
            Value::Null,
            "unread friction store stays null: {unattr}"
        );
        assert_eq!(
            unattr["review_findings_per_node"],
            Value::Null,
            "unread review feed stays null: {unattr}"
        );
        assert_eq!(
            unattr["ci_red_pct"],
            Value::Null,
            "no PR-bearing node, no CI surface: {unattr}"
        );
        assert_eq!(
            unattr["ci_red_coverage_pct"], 0,
            "merged present, none measured: coverage reads 0"
        );
        // The priced cell keeps its columns even beside the unmeasured one.
        let priced = cells
            .iter()
            .find(|c| c["harness"] == "claude")
            .expect("priced bucket kept");
        assert_eq!(priced["spend_usd"], 5.0, "{priced}");
        let last = cells.last().unwrap();
        assert_eq!(
            last["harness"], UNATTRIBUTED,
            "unattributed sorted last: {cells:?}"
        );
        let cov = coverage(&built.records, built.dropped_unparsed);
        assert!(
            cov["harness_pct"].as_i64().unwrap() < 100,
            "harness_pct: {cov}"
        );
        assert_eq!(cov["dropped_unparsed"], 0);
    }

    #[test]
    fn window_keeps_only_recent_last_sessions() {
        // A last execute session 40 days old is outside a 28-day window; a
        // session started 10 days ago but never ended is inside, keyed on
        // its start, and reads no measured hours.
        let old = node(
            "x-ffff",
            json!([session(
                Some("claude"),
                None,
                "sess-6",
                Some("2026-09-05T10:00:00Z")
            )]),
            "low",
            None,
        );
        let recent = node(
            "x-0000",
            json!([session(Some("claude"), None, "sess-7", None)]),
            "low",
            None,
        );
        let built = build_records(
            &[old, recent],
            &LedgerIndex::build(&[]),
            Some(&BTreeMap::new()),
            &no_feeds(),
            None,
            28,
            parse_local(NOW).unwrap(),
            None,
        );
        assert_eq!(built.records.len(), 1, "only the in-window node");
        assert_eq!(built.records[0]["id"], "x-0000");
        assert_eq!(built.dropped_unparsed, 0);
        // A merged node with no measured cost anywhere reads null, not 0.
        assert_eq!(built.records[0]["cost_usd"], Value::Null);

        // A session whose timestamps do not parse counts in coverage
        // instead of vanishing.
        let junk = node(
            "x-bbbb",
            json!([{
                "phase": "execute",
                "session_id": "sess-8",
                "started_at": "not-a-timestamp",
                "ended_at": "also-not",
                "harness": "claude",
            }]),
            "low",
            None,
        );
        let built = build_records(
            &[junk],
            &LedgerIndex::build(&[]),
            Some(&BTreeMap::new()),
            &no_feeds(),
            None,
            28,
            parse_local(NOW).unwrap(),
            None,
        );
        assert_eq!(built.records.len(), 0);
        assert_eq!(built.dropped_unparsed, 1, "{:?}", built.records);
    }

    #[test]
    fn merged_ci95_is_the_wilson_interval_of_the_cell_counts() {
        // AC3-HP: 20 merged of 40 nodes lands near [0.35, 0.65], the closed
        // form the eval trend emits.
        let mut nodes = Vec::new();
        for i in 0..40 {
            let id = format!("x-w{i:04}");
            let mut n = node(
                &id,
                json!([session(
                    Some("claude"),
                    None,
                    &format!("sess-w{i}"),
                    Some("2026-10-15T10:00:00Z")
                )]),
                "medium",
                None,
            );
            // Every other node stays unmerged so the cell folds 20 of 40.
            if i % 2 == 1 {
                n["merge_status"] = json!("in_review");
            }
            nodes.push(n);
        }
        let recs = records(&nodes, &[]);
        assert_eq!(recs.len(), 40, "{recs:?}");
        let cells = fold_cells(&recs);
        assert_eq!(cells.len(), 1, "{cells:?}");
        let ci = cells[0]["merged_ci95"].as_array().expect("ci95 array");
        let lo = ci[0].as_f64().unwrap();
        let hi = ci[1].as_f64().unwrap();
        assert!(
            (0.34..=0.36).contains(&lo) && (0.64..=0.66).contains(&hi),
            "wilson(40, 20) about [0.35, 0.65]: {ci:?}"
        );
    }

    #[test]
    fn unread_outcome_feeds_stay_null_never_zero() {
        // AC3-EDGE shape: the node carries an in-repo PR, so every outcome
        // surface exists, but each feed is unread - no friction store, no
        // review feed, no fix walk. All three rates read null with coverage
        // 0, and the fix coverage stays null too: a walk that never ran is
        // not a walked zero. merged_ci95 still reads from the cell's own
        // counts.
        let n = with_pr_url(
            node(
                "x-nofeed",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-nofeed",
                    Some("2026-10-15T10:00:00Z")
                )]),
                "low",
                None,
            ),
            4207,
        );
        let mut built = build_records(
            &[n],
            &LedgerIndex::build(&[]),
            None,
            &no_feeds(),
            None,
            28,
            parse_local(NOW).unwrap(),
            None,
        );
        assert_eq!(built.records.len(), 1, "{:?}", built.records);
        assert_eq!(built.records[0]["pr"], 4207);
        let cells = fold_cells(&built.records);
        let c = &cells[0];
        assert_eq!(c["merged"], 1);
        assert_eq!(
            c["merged_ci95"].as_array().map(Vec::as_slice),
            Some(&[json!(0.2065), json!(1.0)][..]),
            "wilson(1, 1) is [0.2065, 1.0]: {c}"
        );
        assert_eq!(c["ci_red_pct"], Value::Null);
        assert_eq!(c["ci_red_coverage_pct"], 0);
        assert_eq!(c["review_findings_per_node"], Value::Null);
        assert_eq!(c["review_findings_coverage_pct"], 0);
        assert_eq!(c["fix_of_fix_pct"], Value::Null);
        assert_eq!(
            c["fix_of_fix_coverage_pct"],
            Value::Null,
            "no stamp pass ran, which is not a walked zero: {c}"
        );
        // A walk that ran but answers no fix PRs is the other shape: the
        // coverage reads its walked 0 while the rate stays null.
        stamp_fix_of_fix(&mut built.records, &HashMap::new());
        let cells = fold_cells(&built.records);
        assert_eq!(cells[0]["fix_of_fix_pct"], Value::Null);
        assert_eq!(
            cells[0]["fix_of_fix_coverage_pct"], 0,
            "walked, no fix PRs: a real coverage zero: {cells:?}"
        );
    }

    #[test]
    fn pr_keyed_feeds_fold_to_cell_rates() {
        // Two merged in-repo PR nodes on one lane: one carries a recorded
        // red head, a review-coverage row and 3 fail verdicts; one carries
        // only a measured-clean fix verdict, so its CI and review columns
        // stay unmeasured - the feeds never read its PR. A third node
        // carries a foreign repo's PR URL with a colliding number: it joins
        // nothing, record or cell.
        let friction = BTreeMap::from([(
            "x-red".to_string(),
            BTreeMap::from([("ci_failure".to_string(), 1_u32)]),
        )]);
        let red_node = with_pr_url(
            node(
                "x-red",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-red",
                    Some("2026-10-15T10:00:00Z")
                )]),
                "low",
                None,
            ),
            4201,
        );
        let clean_node = with_pr_url(
            node(
                "x-clean",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-clean",
                    Some("2026-10-16T10:00:00Z")
                )]),
                "low",
                None,
            ),
            4202,
        );
        let mut foreign = with_pr_url(
            node(
                "x-fn",
                json!([session(
                    Some("claude"),
                    None,
                    "sess-fn",
                    Some("2026-10-16T11:00:00Z")
                )]),
                "low",
                None,
            ),
            4201,
        );
        foreign["pr_url"] = json!("https://github.com/other/fno-books/pull/4201");
        let mut built = build_records(
            &[red_node, clean_node, foreign],
            &LedgerIndex::build(&[]),
            Some(&friction),
            &OutcomeFeeds {
                review_fails_by_pr: Some(&HashMap::from([(4201_i64, 3_u64)])),
            },
            Some("github.com/bllshttng/fno"),
            28,
            parse_local(NOW).unwrap(),
            None,
        );
        assert_eq!(built.records.len(), 3);
        // The foreign node's colliding number joins nothing: no PR surface,
        // no CI red, no review findings, no friction count.
        assert_eq!(built.records[2]["pr"], Value::Null, "{:?}", built.records);
        assert_eq!(built.records[2]["ci_red"], Value::Null);
        assert_eq!(built.records[2]["review_findings"], Value::Null);
        assert_eq!(built.records[2]["ci_failure"], Value::Null);
        // In scope: the red node measures red with 3 findings; the clean
        // node's CI and review stay unmeasured (no coverage row for its PR)
        // while its fix verdict lands.
        assert_eq!(built.records[0]["ci_red"], true);
        assert_eq!(built.records[0]["review_findings"], 3);
        assert_eq!(built.records[1]["ci_red"], Value::Null);
        assert_eq!(built.records[1]["review_findings"], Value::Null);
        stamp_fix_of_fix(
            &mut built.records,
            &HashMap::from([(4202_u64, Some(false))]),
        );
        let cells = fold_cells(&built.records);
        assert_eq!(cells.len(), 1, "{cells:?}");
        let c = &cells[0];
        assert_eq!(c["nodes"], 3);
        assert_eq!(c["merged"], 3);
        assert_eq!(
            c["ci_red_pct"], 100,
            "the one coverage-proven PR recorded a red: {c}"
        );
        assert_eq!(c["ci_red_coverage_pct"], 33);
        assert_eq!(
            c["review_findings_per_node"], 3.0,
            "3 fails over the one PR the review feed read: {c}"
        );
        assert_eq!(c["review_findings_coverage_pct"], 33);
        assert_eq!(c["fix_of_fix_pct"], 0, "measured clean, a real 0%: {c}");
        assert_eq!(c["fix_of_fix_coverage_pct"], 33, "one fix PR walked: {c}");
    }

    #[test]
    fn depth_three_reads_type_and_surface_from_the_parts_map() {
        let mut n = node(
            "n1",
            json!([session(
                Some("claude"),
                Some("sonnet"),
                "s1",
                Some("2026-10-14T11:00:00Z")
            )]),
            "medium",
            None,
        );
        n = with_pr_url(n, 4001);
        let mut recs = records(&[n], &[]);
        let mut parts = HashMap::new();
        parts.insert(4001_u64, Some(("fix", "rust")));
        stamp_kind_parts(&mut recs, &parts, 3);
        assert_eq!(recs[0]["kind"], json!("target/medium/fix-rust"));
        // The default depth is the stamp's no-op: the slice-1 kind survives.
        let mut untouched = records(
            &[with_pr_url(
                node(
                    "n2",
                    json!([session(
                        Some("claude"),
                        Some("sonnet"),
                        "s2",
                        Some("2026-10-14T11:00:00Z")
                    )]),
                    "medium",
                    None,
                ),
                4002,
            )],
            &[],
        );
        stamp_kind_parts(&mut untouched, &parts, 1);
        assert_eq!(untouched[0]["kind"], json!("target/medium"));
        // The type evidence ladder: a merge delivery's git subject is
        // GitHub's `Merge pull request #N ...` line, so a named type on the
        // PR title wins; a squash reads its own subject; a merge with a
        // plain title reads other, truthfully.
        assert_eq!(
            crate::stability_report::kind_commit_type(
                "Merge pull request #7 from o/b",
                Some("fix(mux): the thing"),
            ),
            "fix"
        );
        assert_eq!(
            crate::stability_report::kind_commit_type("fix: direct", None),
            "fix"
        );
        assert_eq!(
            crate::stability_report::kind_commit_type(
                "Merge pull request #7 from o/b",
                Some("Add widget")
            ),
            "other"
        );
        assert_eq!(crate::stability_report::commit_type("docs: note"), "docs");
        assert_eq!(
            crate::stability_report::surface_class(&["crates/a.rs"]),
            Some("rust")
        );
        assert_eq!(
            crate::stability_report::surface_class(&["docs/x.md", "crates/a.rs"]),
            Some("rust")
        );
        assert_eq!(
            crate::stability_report::surface_class(&["docs/x.md"]),
            Some("docs")
        );
        assert_eq!(
            crate::stability_report::surface_class(&["crates/a.rs", "cli/src/fno/b.py"]),
            Some("mixed")
        );
        assert_eq!(
            crate::stability_report::surface_class(&["cli/src/fno/agents/mux_server.py"]),
            Some("ui")
        );
    }

    #[test]
    fn blueprint_session_credits_its_lane_beside_the_target_cell() {
        let mut bp = session(
            Some("claude"),
            Some("claude-opus-5-5"),
            "bp1",
            Some("2026-10-14T09:30:00Z"),
        );
        bp["phase"] = json!("blueprint");
        let n = node(
            "n1",
            json!([
                bp,
                session(
                    Some("claude"),
                    Some("glm-5.3-flash"),
                    "s1",
                    Some("2026-10-14T11:00:00Z")
                )
            ]),
            "medium",
            None,
        );
        let ledger = vec![ledger_row(
            &["bp1"],
            "anthropic",
            "claude-opus-5-5",
            Some("n1"),
            5.0,
        )];
        let recs = records(&[n], &ledger);
        assert_eq!(recs.len(), 2, "{recs:?}");
        let by_kind: Vec<(&str, &str, &str)> = recs
            .iter()
            .map(|r| {
                (
                    r["kind"].as_str().unwrap_or_default(),
                    r["harness"].as_str().unwrap_or_default(),
                    r["model"].as_str().unwrap_or_default(),
                )
            })
            .collect();
        assert!(
            by_kind.contains(&("blueprint/medium", "claude", "claude-opus-5-5")),
            "{by_kind:?}"
        );
        assert!(
            by_kind.contains(&("target/medium", "claude", "glm-5.3-flash")),
            "{by_kind:?}"
        );
        // The credit mirrors the downstream outcome, merged included.
        assert!(recs.iter().all(|r| r["merged"] == json!(true)));
    }

    #[test]
    fn unknown_kind_parts_read_unknown_never_a_guess() {
        let n = node(
            "n1",
            json!([session(
                Some("claude"),
                Some("sonnet"),
                "s1",
                Some("2026-10-14T11:00:00Z")
            )]),
            "medium",
            None,
        );
        let mut recs = records(&[n], &[]);
        let parts: HashMap<u64, Option<(&'static str, &'static str)>> = HashMap::new();
        stamp_kind_parts(&mut recs, &parts, 3);
        assert_eq!(recs[0]["kind"], json!("target/medium/unknown-unknown"));
        // A wanted PR whose diff read failed maps to None and reads the
        // same visible unknown, not a guessed class.
        let mut n2 = with_pr_url(
            node(
                "n2",
                json!([session(
                    Some("claude"),
                    Some("sonnet"),
                    "s2",
                    Some("2026-10-14T11:00:00Z")
                )]),
                "medium",
                None,
            ),
            7,
        );
        n2["merge_status"] = json!("merged");
        let mut recs2 = records(&[n2], &[]);
        let parts2: HashMap<u64, Option<(&'static str, &'static str)>> =
            HashMap::from([(7_u64, None)]);
        stamp_kind_parts(&mut recs2, &parts2, 3);
        assert_eq!(recs2[0]["kind"], json!("target/medium/unknown-unknown"));
    }
}
