//! `fno-agents pr-park` -- a parked PR is a board fact, not a watcher fact.
//!
//! A parked record stopped polling and, after one OS notice, the PR was
//! invisible to merge scan, heal and the lead. Red CI is the state a working
//! session is in while it pushes fixes, so "retries exhausted" read as
//! "forgotten until a human noticed": six open PRs parked in one afternoon,
//! with 17 finished delivery records cluttering the list above them.
//! This command is the ONE reader that can change a park record: `list` buckets
//! every parked row (open / finished / foreign), `unpark` clears one or all
//! open rows, and `sweep` is the automatic lifter. The sweep re-checks each
//! open park by its cause every run: a `checks-red` park lifts when the
//! checks read green again, a `worktree_head_mismatch` or `worktree_dirty`
//! park lifts when its branch worktree reads clear at the current PR head,
//! a head that moved lifts any park, and a park whose own `parked_at` stamp
//! passed 24 hours lifts regardless. Cause and head lifts name themselves in
//! the `cause` field of the `pr_watch_unparked` event.
//!
//! The store stays the Python watcher's files, written with the same atomic
//! tmp+rename; there is no second schema. Un-parks emit `pr_watch_unparked`
//! into the same journal the watcher's `pr_watch_parked` rows live in.

use crate::authorized_merge::Probes;
use crate::events::EventEmitter;
use crate::org_board::scope::graph_json_path;
use crate::paths::AgentsHome;
use crate::tick_ledger::parse_rfc3339_unix;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A park older than this un-parks on the next sweep, head moved or not.
const PARK_MAX_AGE_HOURS: u64 = 24;

/// A `parked` value the sweep already processed; never listed again.
const HANDLED: &str = "handled";

/// Who the sweep names in `pr_watch_unparked`.
const BY_SWEEP: &str = "sweep";
const BY_MANUAL: &str = "manual";

/// Why the sweep lifted a park: the optional `cause` field on
/// `pr_watch_unparked`, one word per sweep arm.
const CAUSE_HEAD_MOVED: &str = "head-moved";
const CAUSE_MAX_AGE: &str = "max-age";
const CAUSE_CHECKS_CLEARED: &str = "checks-cleared";
const CAUSE_WORKTREE_CLEARED: &str = "worktree-cleared";

/// The files this command reads and writes. Paths are injectable so the Python
/// caller can pass its config-resolved ones; the daemon uses [`Paths::from_home`].
#[derive(Debug, Clone)]
pub(crate) struct Paths {
    pub state: PathBuf,
    pub delivery: PathBuf,
    pub events: PathBuf,
    pub err_log: PathBuf,
}

impl Paths {
    fn for_root(root: PathBuf) -> Paths {
        Paths {
            state: crate::state_layout::place(&root, "pr-watcher-state.json"),
            delivery: crate::state_layout::place(&root, "pr-watcher-state-delivery.json"),
            events: root.join("events.jsonl"),
            err_log: crate::state_layout::place(&root, "pr-watcher.err.log"),
        }
    }

    /// The durable defaults beside the state root: the same files the Python
    /// watcher writes (`~/.fno/state/pr-watcher-state.json` + its `-delivery`
    /// sidecar, `~/.fno/events.jsonl`, `~/.fno/logs/pr-watcher.err.log`),
    /// resolved through the layout ladder so a root the mover has not reached
    /// yet still reads its legacy files.
    pub fn from_home() -> Paths {
        let root = AgentsHome::from_env()
            .root()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".fno"));
        Paths::for_root(root)
    }

    /// The paths a configured `state_dir` moves. Only an explicit
    /// `state_dir` key diverges from [`Paths::from_home`], so a test env or
    /// default install resolves exactly where the watcher already writes.
    pub fn resolve(cwd: &Path) -> Paths {
        // The FNO_STATE_DIR carrier outranks config and HOME, the way
        // Python's paths.state_dir reads it: a sealed or test-isolated root
        // must move every state file together, not leave the watcher and the
        // parked read on the configured root while events land elsewhere.
        if let Some(raw) = std::env::var_os("FNO_STATE_DIR").filter(|v| !v.is_empty()) {
            let text = raw.to_string_lossy().to_string();
            let root = match text.strip_prefix("~/") {
                Some(rest) => match std::env::var_os("HOME") {
                    Some(home) => PathBuf::from(home).join(rest),
                    None => return Paths::from_home(),
                },
                None if Path::new(&text).is_absolute() => PathBuf::from(&text),
                None => cwd.join(&text),
            };
            return Paths::for_root(root);
        }
        let configured = crate::agents_config::config_lookup(cwd, &["state_dir"])
            .and_then(|v| v.as_str().map(str::to_string));
        match configured {
            Some(raw) if !raw.trim().is_empty() => {
                let root = match raw.strip_prefix("~/") {
                    Some(rest) => match std::env::var_os("HOME") {
                        Some(home) => PathBuf::from(home).join(rest),
                        None => return Paths::from_home(),
                    },
                    None if Path::new(&raw).is_absolute() => PathBuf::from(raw),
                    None => cwd.join(&raw),
                };
                Paths::for_root(root)
            }
            _ => Paths::from_home(),
        }
    }
}

/// Everything the command needs from the world, injectable so the sweep is
/// testable without a network and the list without a graph.
pub(crate) struct Ctx {
    pub paths: Paths,
    /// `owner/repo` of `cwd`, from the git remote.
    pub slug: String,
    /// The backlog graph rows, for PR-to-node resolution.
    pub entries: Vec<Value>,
    /// One PR facts probe: `(head_sha, head_ref, pr_state)` or the error.
    pub head: Box<dyn Fn(u64) -> Result<(String, String, String), String>>,
    /// Does PR `pr` read red right now? `Ok(false)` is the answer a
    /// checks-red park lifts on.
    pub checks_red: Box<dyn Fn(u64) -> Result<bool, String>>,
    /// Is the branch worktree clean at `pr_head` with nothing in flight?
    /// `Ok(true)` is the answer a worktree park lifts on.
    pub worktree_clear: Box<dyn Fn(&str, &str) -> Result<bool, String>>,
}

impl Ctx {
    pub fn live(cwd: &Path, paths: Paths) -> Ctx {
        // The store key is `owner/repo#N`, so the cwd side resolves the full
        // identity and drops the host segment; slug_from_git_remote alone
        // would read "footnote" and make every row read foreign.
        let slug = crate::finalize::repo_identity_from_git_remote(cwd)
            .and_then(|id| id.split_once('/').map(|(_, rest)| rest.to_string()))
            .unwrap_or_default();
        let entries = graph_rows(cwd);
        let probe_cwd = cwd.to_path_buf();
        Ctx {
            paths,
            slug,
            entries,
            head: {
                let probe_cwd = probe_cwd.clone();
                Box::new(move |pr| {
                    let facts =
                        crate::authorized_merge::RealProbes.pr_facts(&probe_cwd, Some(pr))?;
                    Ok((facts.head_sha, facts.head_ref, facts.state))
                })
            },
            checks_red: {
                let probe_cwd = probe_cwd.clone();
                Box::new(move |pr| {
                    let checks =
                        Probes::checks_read(&crate::authorized_merge::RealProbes, &probe_cwd, pr);
                    Ok(checks.verdict == "red")
                })
            },
            worktree_clear: {
                let probe_cwd = probe_cwd.clone();
                Box::new(move |branch, pr_head| {
                    let reading =
                        crate::pr_status::reviews::review_activity(&probe_cwd, branch, pr_head);
                    Ok(reading
                        .get("blocker")
                        .and_then(Value::as_str)
                        .map_or(true, |b| b.is_empty()))
                })
            },
        }
    }
}

fn graph_rows(cwd: &Path) -> Vec<Value> {
    let graph_path = graph_json_path(cwd);
    crate::graph_store::read_rows_where(
        &graph_path,
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "status",
                    "pr_number",
                    "pr_url",
                    "additional_prs",
                    "cwd",
                ]
                .into_iter()
                .map(str::to_string)
                .collect(),
            ),
            with_blockers: true,
            ..Default::default()
        },
    )
    .map_err(|error| crate::backlog::api::ApiError(error.to_string()))
    .unwrap_or_default()
}

/// One parked row as listed.
#[derive(Debug, Clone)]
pub(crate) struct Row {
    pub key: String,
    pub pr: u64,
    pub slug: String,
    /// The `parked` value: why polling stopped.
    pub reason: String,
    /// The stored failure detail, recovered from the journal or the err log.
    pub reason_detail: String,
    pub node: String,
    pub node_status: String,
    pub pr_state: String,
    /// Hours since the park stamp (`parked_at`, else `last_polled_at`); `-1`
    /// when neither stamp is readable.
    pub age_hours: i64,
    pub parked_head: String,
    pub bucket: &'static str,
}

/// One parked entry as physically stored, before bucketing.
struct Stored {
    key: String,
    pr: u64,
    slug: String,
    entry: Value,
    from_delivery: bool,
}

/// Read both store files, delivery rows overlaying the observed cache the way
/// the Python status page's `dict.update` did. Rows already `handled` are
/// invisible from here on.
fn parked_rows(paths: &Paths) -> Vec<Stored> {
    let mut out: BTreeMap<String, Stored> = BTreeMap::new();
    for (path, from_delivery) in [(&paths.state, false), (&paths.delivery, true)] {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let Ok(Value::Object(data)) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        for (key, entry) in data {
            let Some(reason) = entry.get("parked").and_then(Value::as_str) else {
                continue;
            };
            if reason.is_empty() || reason == HANDLED {
                continue;
            }
            let Some((slug, pr)) = key
                .split_once('#')
                .and_then(|(s, n)| n.parse::<u64>().ok().map(|n| (s.to_lowercase(), n)))
            else {
                continue;
            };
            out.insert(
                key.clone(),
                Stored {
                    key,
                    pr,
                    slug,
                    entry,
                    from_delivery,
                },
            );
        }
    }
    out.into_values().collect()
}

fn parse_key(key: &str) -> Option<(String, u64)> {
    let (slug, num) = key.split_once('#')?;
    let pr = num.parse::<u64>().ok()?;
    Some((slug.to_lowercase(), pr))
}

/// Split `owner/repo#7`.
fn split_key(key: &str) -> Option<(&str, u64)> {
    let (slug, num) = key.split_once('#')?;
    Some((slug, num.parse().ok()?))
}

fn entry_str(entry: &Value, key: &str) -> String {
    entry
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn node_for(entries: &[Value], slug: &str, pr: u64) -> (String, String) {
    for e in entries {
        if crate::graph_keeper::node_carries_pr(e, pr as i64, Some(slug)) {
            return (
                e.get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                entry_str(e, "status"),
            );
        }
    }
    (String::new(), String::new())
}

/// The newest `merge_grant_execution` row for this PR in the store, or the
/// newest err-log line naming it, or empty. Both reads are bounded: the
/// keyed store query is indexed, and the lead reads this list every beat.
fn reason_detail(paths: &Paths, pr: u64) -> String {
    if let Some(detail) = grant_detail(&paths.events, pr, |data| {
        let mut bits = Vec::new();
        if let Some(p) = data.get("phase").and_then(Value::as_str) {
            bits.push(p.to_string());
        }
        if let Some(r) = data.get("reason").and_then(Value::as_str) {
            bits.push(r.to_string());
        }
        if let Some(code) = data.get("exit_code") {
            bits.push(format!("exit_code {code}"));
        }
        bits.join("; ")
    }) {
        return detail;
    }
    tail_detail(&paths.err_log, pr, |data| {
        data.get("reason")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    })
    .unwrap_or_default()
}

/// The newest committed `merge_grant_execution` row for `pr`, mapped through
/// `pick`; the first non-empty detail wins (newest first).
fn grant_detail(events: &Path, pr: u64, pick: impl Fn(&Value) -> String) -> Option<String> {
    let q = crate::event_store::EventQuery {
        types: vec!["merge_grant_execution".into()],
        pr_number: Some(pr as i64),
        ..Default::default()
    };
    let rows = crate::event_store::query_events(events, &q).ok()?;
    for row in rows.iter().rev() {
        let Ok(Value::Object(entry)) = serde_json::from_str::<Value>(&row.line) else {
            continue;
        };
        let data = entry.get("data").cloned().unwrap_or(entry.clone().into());
        let detail = pick(&data);
        if !detail.is_empty() {
            return Some(detail);
        }
    }
    None
}

/// The newest JSON line naming `pr` in the err-log tail, mapped through
/// `pick`. Reads at most the last 1 MiB.
fn tail_detail(path: &Path, pr: u64, pick: impl Fn(&Value) -> String) -> Option<String> {
    let Ok(meta) = std::fs::metadata(path) else {
        return None;
    };
    let read_from = meta.len().saturating_sub(1024 * 1024);
    let mut file = std::fs::File::open(path).ok()?;
    use std::io::{Read as _, Seek, SeekFrom};
    let _ = file.seek(SeekFrom::Start(read_from));
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    for line in text.lines().rev() {
        let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let data = row.get("data").cloned().unwrap_or(row.clone().into());
        if data.get("pr").and_then(Value::as_u64) != Some(pr) {
            continue;
        }
        let detail = pick(&data);
        if !detail.is_empty() {
            return Some(detail);
        }
    }
    None
}

fn bucket(stored: &Stored, slug: &str, pr_state: &str) -> &'static str {
    if stored.slug != slug {
        return "foreign";
    }
    if stored.from_delivery || pr_state == "MERGED" || pr_state == "CLOSED" {
        return "finished";
    }
    "open"
}

/// Every parked row with its bucket, node and recovered reason detail.
pub(crate) fn list_rows(ctx: &Ctx) -> Vec<Row> {
    parked_rows(&ctx.paths)
        .into_iter()
        .map(|stored| {
            let pr_state = entry_str(&stored.entry, "last_seen_state");
            let (node, node_status) = node_for(&ctx.entries, &stored.slug, stored.pr);
            // Park age, on the same stamp the sweep ages on.
            let stamp = entry_str(&stored.entry, "parked_at");
            let stamp = if stamp.is_empty() {
                entry_str(&stored.entry, "last_polled_at")
            } else {
                stamp
            };
            let age_hours = parse_rfc3339_unix(&stamp)
                .map(|t| (now_secs() as i64 - t as i64) / 3600)
                .unwrap_or(-1);
            let b = bucket(&stored, &ctx.slug, &pr_state);
            Row {
                key: stored.key.clone(),
                pr: stored.pr,
                slug: stored.slug,
                reason: entry_str(&stored.entry, "parked"),
                reason_detail: reason_detail(&ctx.paths, stored.pr),
                node,
                node_status,
                pr_state,
                age_hours,
                parked_head: entry_str(&stored.entry, "parked_head"),
                bucket: b,
            }
        })
        .collect()
}

fn row_json(row: &Row) -> Value {
    json!({
        "key": row.key,
        "pr": row.pr,
        "slug": row.slug,
        "reason": row.reason,
        "reason_detail": row.reason_detail,
        "node": row.node,
        "node_status": row.node_status,
        "pr_state": row.pr_state,
        "age_hours": row.age_hours,
        "parked_head": row.parked_head,
        "bucket": row.bucket,
    })
}

/// Clear `parked`, reset `retries`, clear `parked_at`, and emit one
/// `pr_watch_unparked` row per changed entry, writing each touched file
/// through the same tmp+rename the Python store uses. `pred` selects whole
/// entries by key; `cause` names the sweep arm that lifted the row and rides
/// the event (a manual unpark names none).
fn unpark_where(
    paths: &Paths,
    pred: &dyn Fn(&str, &Value, bool) -> bool,
    by: &str,
    cause: Option<&str>,
) -> Result<usize, String> {
    let mut changed = 0usize;
    for path in [&paths.state, &paths.delivery] {
        // One flock per file across the whole read-modify-write: the watcher
        // tick's next full-dict `set()` must land after this rewrite, not on
        // top of a snapshot taken before it.
        let _lock = crate::gh_budget::FileLock::try_acquire(&crate::gh_budget::lock_path(path))?;
        let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
        let mut data: Map<String, Value> = match serde_json::from_str(&text) {
            Ok(Value::Object(m)) => m,
            _ => Map::new(),
        };
        let mut touched = 0usize;
        let mut unparked_prs = Vec::new();
        for (key, entry) in data.iter_mut() {
            let is_parked = entry
                .get("parked")
                .and_then(Value::as_str)
                .is_some_and(|r| !r.is_empty() && r != HANDLED);
            let from_delivery = *path == paths.delivery;
            if !is_parked || !pred(key, entry, from_delivery) {
                continue;
            }
            entry["parked"] = Value::Null;
            entry["retries"] = json!(0);
            entry["parked_at"] = Value::Null;
            touched += 1;
            if let Some(pr) = split_key(key).map(|(_, n)| n) {
                unparked_prs.push(pr);
            }
        }
        if touched > 0 {
            atomic_write(path, &Value::Object(data).to_string())?;
            changed += touched;
            // The event follows the durable write: a failed store write must
            // not leave a journal row claiming an unpark that never landed.
            for pr in unparked_prs {
                let mut fields = Map::new();
                fields.insert("pr".to_string(), json!(pr));
                fields.insert("by".to_string(), json!(by));
                if let Some(cause) = cause {
                    fields.insert("cause".to_string(), json!(cause));
                }
                let _ = EventEmitter::new(&paths.events, "pr-park")
                    .emit_fields("pr_watch_unparked", fields);
            }
        }
    }
    Ok(changed)
}

/// `unpark <key>` / `unpark --all-open`. `--all-open` clears every open row
/// of this repo; a bare key clears exactly that row.
pub(crate) fn unpark(ctx: &Ctx, key: Option<&str>, all_open: bool) -> Result<usize, String> {
    if key.is_none() && !all_open {
        return Err("unpark needs a store key or --all-open".to_string());
    }
    let slug = ctx.slug.clone();
    let want = key.map(parse_key);
    unpark_where(
        &ctx.paths,
        &move |k, _e, from_delivery| {
            if from_delivery {
                return false;
            }
            match &want {
                Some(Some((s, n))) => k.to_lowercase() == format!("{s}#{n}"),
                Some(None) => false,
                None => {
                    // --all-open: only rows of THIS repo can be re-observed.
                    split_key(k)
                        .map(|(s, _)| s.to_lowercase() == slug)
                        .unwrap_or(false)
                }
            }
        },
        BY_MANUAL,
        None,
    )
}

/// The sweep's counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub unparked: usize,
    pub handled: usize,
    pub total: usize,
}

/// Un-park every open row whose PR head moved since the park baseline or
/// whose park passed 24 hours; mark finished rows handled. A row parked
/// before this shipped has no baseline and takes the 24-hour rule; the
/// first sweep records the head it sees as the baseline.
pub(crate) fn sweep(ctx: &Ctx) -> Result<SweepReport, String> {
    let rows = parked_rows(&ctx.paths);
    let mut report = SweepReport {
        total: rows.len(),
        ..SweepReport::default()
    };

    // Finished rows leave the list forever; one write per file that has any.
    let finished_keys: Vec<String> = rows
        .iter()
        .filter(|s| {
            let state = entry_str(&s.entry, "last_seen_state");
            bucket(s, &ctx.slug, &state) == "finished"
        })
        .map(|s| s.key.clone())
        .collect();
    if !finished_keys.is_empty() {
        report.handled = mark_handled(&ctx.paths, &finished_keys)?;
    }

    for stored in &rows {
        let pr_state = entry_str(&stored.entry, "last_seen_state");
        if bucket(stored, &ctx.slug, &pr_state) != "open" {
            continue;
        }
        // The park's own clock governs the age arm. Rows parked before the
        // stamp shipped carry none and keep the poll stamp, which is the age
        // main measured.
        let stamp = entry_str(&stored.entry, "parked_at");
        let stamp = if stamp.is_empty() {
            entry_str(&stored.entry, "last_polled_at")
        } else {
            stamp
        };
        let age_hours = parse_rfc3339_unix(&stamp)
            .map(|t| now_secs().saturating_sub(t) / 3600)
            .unwrap_or(0);
        if age_hours >= PARK_MAX_AGE_HOURS as u64 {
            do_unpark(&ctx.paths, &stored.key, BY_SWEEP, Some(CAUSE_MAX_AGE))?;
            report.unparked += 1;
            continue;
        }
        let parked_head = entry_str(&stored.entry, "parked_head");
        // The head and the cause probes read the row's world once per sweep.
        let Ok((head, branch, state)) = (ctx.head)(stored.pr) else {
            // An unreadable probe measures nothing; stay parked.
            continue;
        };
        if state != "OPEN" {
            // It merged or closed while parked: a finished row, not a resume.
            mark_handled(&ctx.paths, &[stored.key.clone()])?;
            report.handled += 1;
            continue;
        }
        // A park whose cause cleared lifts here, before the head arms: the
        // cause is the thing that parked the row, so its answer decides
        // first. A probe error lifts nothing, the rule the head probe keeps.
        match entry_str(&stored.entry, "parked").as_str() {
            "checks-red" => match (ctx.checks_red)(stored.pr) {
                Ok(false) => {
                    do_unpark(
                        &ctx.paths,
                        &stored.key,
                        BY_SWEEP,
                        Some(CAUSE_CHECKS_CLEARED),
                    )?;
                    report.unparked += 1;
                    continue;
                }
                Ok(true) | Err(_) => {}
            },
            "worktree_head_mismatch" | "worktree_dirty" => {
                match (ctx.worktree_clear)(&branch, &head) {
                    Ok(true) => {
                        do_unpark(
                            &ctx.paths,
                            &stored.key,
                            BY_SWEEP,
                            Some(CAUSE_WORKTREE_CLEARED),
                        )?;
                        report.unparked += 1;
                        continue;
                    }
                    Ok(false) | Err(_) => {}
                }
            }
            _ => {}
        }
        if parked_head.is_empty() {
            // Baseline round: record the head, then judge from the next
            // sweep on.
            record_parked_head(&ctx.paths, &stored.key, &head)?;
            continue;
        }
        if head != parked_head {
            do_unpark(&ctx.paths, &stored.key, BY_SWEEP, Some(CAUSE_HEAD_MOVED))?;
            report.unparked += 1;
        }
    }
    Ok(report)
}

fn do_unpark(paths: &Paths, key: &str, by: &str, cause: Option<&str>) -> Result<(), String> {
    unpark_where(paths, &|k, _e, _from_delivery| k == key, by, cause)?;
    Ok(())
}

fn record_parked_head(paths: &Paths, key: &str, head: &str) -> Result<(), String> {
    for path in [&paths.state, &paths.delivery] {
        let _lock = crate::gh_budget::FileLock::try_acquire(&crate::gh_budget::lock_path(path))?;
        let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
        let mut data: Map<String, Value> = match serde_json::from_str(&text) {
            Ok(Value::Object(m)) => m,
            _ => continue,
        };
        let Some(entry) = data.get_mut(key) else {
            continue;
        };
        entry["parked_head"] = json!(head);
        atomic_write(path, &Value::Object(data).to_string())?;
        return Ok(());
    }
    Ok(())
}

fn mark_handled(paths: &Paths, keys: &[String]) -> Result<usize, String> {
    let mut changed = 0usize;
    for path in [&paths.state, &paths.delivery] {
        let _lock = crate::gh_budget::FileLock::try_acquire(&crate::gh_budget::lock_path(path))?;
        let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
        let mut data: Map<String, Value> = match serde_json::from_str(&text) {
            Ok(Value::Object(m)) => m,
            _ => continue,
        };
        let mut touched = 0usize;
        for (key, entry) in data.iter_mut() {
            if keys.contains(key)
                && entry
                    .get("parked")
                    .and_then(Value::as_str)
                    .is_some_and(|r| !r.is_empty() && r != HANDLED)
            {
                entry["parked"] = json!(HANDLED);
                touched += 1;
            }
        }
        if touched > 0 {
            atomic_write(path, &Value::Object(data).to_string())?;
            changed += touched;
        }
    }
    Ok(changed)
}

/// Same shape as the Python store's persist: temp file beside the target,
/// then rename. A partial write can never be the file a reader sees.
fn atomic_write(path: &Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or_else(|| "no parent dir".to_string())?;
    let tmp = dir.join(format!(
        ".{}.tmp.{}",
        path.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "pr-park".to_string()),
        std::process::id()
    ));
    std::fs::write(&tmp, text)
        .and_then(|_| std::fs::rename(&tmp, path))
        .map_err(|e| format!("store write failed: {e}"))
}

pub(crate) fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

/// `fno-agents pr-park list|unpark|sweep`. Binary-direct behind
/// `fno do pr watch`, like `pr-heal`: not a routable `fno agents` command.
pub fn run(args: &[String]) -> i32 {
    let json_out = crate::json_output::requested(args);
    let mut all_open = false;
    let mut cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut key: Option<String> = None;
    let mut paths = Paths::from_home();
    let mut sub = "";
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "list" | "unpark" | "sweep" if sub.is_empty() => sub = a.as_str(),
            "--all-open" => all_open = true,
            "--cwd" => cwd = it.next().map(PathBuf::from).unwrap_or(cwd),
            "--state" => {
                if let Some(v) = it.next() {
                    paths.state = PathBuf::from(v)
                }
            }
            "--delivery" => {
                if let Some(v) = it.next() {
                    paths.delivery = PathBuf::from(v)
                }
            }
            "--events" => {
                if let Some(v) = it.next() {
                    paths.events = PathBuf::from(v)
                }
            }
            "--err-log" => {
                if let Some(v) = it.next() {
                    paths.err_log = PathBuf::from(v)
                }
            }
            other if key.is_none() && !other.starts_with('-') => key = Some(other.to_string()),
            _ => {}
        }
    }
    if sub.is_empty() {
        eprintln!("usage: pr-park list [--json] | unpark <key>|--all-open | sweep [--cwd <dir>]");
        return 2;
    }
    let ctx = Ctx::live(&cwd, paths);
    match sub {
        "list" => {
            let rows = list_rows(&ctx);
            if json_out {
                println!(
                    "{}",
                    json!({"rows": rows.iter().map(row_json).collect::<Vec<_>>()})
                );
            } else if rows.is_empty() {
                println!("parked: none");
            } else {
                for row in &rows {
                    let age = if row.age_hours < 0 {
                        "?".to_string()
                    } else {
                        format!("{}h", row.age_hours)
                    };
                    println!(
                        "{} [{}] {} ({}, {}) node={} {}",
                        row.key,
                        row.bucket,
                        row.reason,
                        age,
                        row.reason_detail,
                        row.node,
                        if row.node_status.is_empty() {
                            String::new()
                        } else {
                            format!("status={}", row.node_status)
                        },
                    );
                }
            }
            0
        }
        "unpark" => match unpark(&ctx, key.as_deref(), all_open) {
            Ok(n) => {
                println!("unparked {n}");
                0
            }
            Err(e) => {
                eprintln!("pr-park: {e}");
                1
            }
        },
        _ => match sweep(&ctx) {
            Ok(r) => {
                if json_out {
                    println!(
                        "{}",
                        json!({"unparked": r.unparked, "handled": r.handled, "total": r.total})
                    );
                } else {
                    println!(
                        "sweep: unparked {} handled {} of {}",
                        r.unparked, r.handled, r.total
                    );
                }
                0
            }
            Err(e) => {
                eprintln!("pr-park: {e}");
                1
            }
        },
    }
}

#[cfg(test)]
mod tests;
