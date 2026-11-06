//! The tick's two PR legs: the sweep (discover, re-read the tracked cache,
//! decide, dispatch, receipt) and the durable-grant merge queue. Every outside
//! effect sits behind [`Seams`], so the decision flow tests without gh.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Map, Value};

use super::clock::{self, DoorError};
use super::discover::{self, Candidate, Observation, Verdict};
use super::state::{delivery_path, make_key, parse_key, Normalization, Store};

const TICK_CLAIM_KEY: &str = "pr-watch:tick";
const FIRE_SEAM_ENV: &str = "PR_WATCH_FIRE_CMD";
/// Worker ceiling for the headless review fire: a launchd tick never overlaps,
/// so an unbounded child would wedge every future tick.
const CHECK_TIMEOUT_S: f64 = 180.0;
const SPAWN_GRACE_S: f64 = 30.0;
/// Room for one more rich read plus the final persist.
const READ_FLOOR_S: f64 = 15.0;
const FIRE_FLOOR_S: f64 = 30.0;
/// One ritual completed at 96s against 62 timeouts (2026-09-20/21).
const RITUAL_FLOOR_S: f64 = 100.0;
const RITUAL_TIMEOUT_CAP_S: f64 = 135.0;
/// A real merge took about 120s; head attempts ran 115-128s under load.
pub(crate) const MERGE_FLOOR_S: f64 = 150.0;
const READ_BUDGET_S: f64 = 30.0;
/// A fleet budget hold at or above this pauses the sweep: every read would
/// answer refused. The merge leg holds at the same threshold.
const GH_HOLD_SKIP_S: f64 = 10.0;

#[derive(Debug, Default)]
pub(crate) struct TickResult {
    pub open_prs: i64,
    pub acted: i64,
    pub skipped: i64,
    pub lock_held: bool,
    pub lock_holder: String,
    pub disabled: bool,
    pub sweep_failures: i64,
    pub quota_skip: bool,
    pub quota_remaining: Option<i64>,
    pub quota_reset: Option<String>,
    pub quota_unknown: bool,
    /// The fleet budget hold that paused the sweep, seconds left.
    pub budget_hold_s: Option<f64>,
}

#[derive(Debug, Clone)]
pub(crate) struct Fire {
    pub ok: bool,
    /// False with `ok` false means an admission refusal, not a failed attempt.
    pub is_error: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct RitualAnswer {
    pub outcome: String,
    pub detail: Option<String>,
}

/// The sweep's and the queue's outside world.
pub(crate) trait Seams {
    fn emit(&self, kind: &str, data: Value) -> bool;
    fn notify(&self, message: &str);
    fn acquire_lock(&self, key: &str, holder: &str) -> Result<(), String>;
    fn release_lock(&self, key: &str, holder: &str);
    fn is_node_live(&self, node: &str) -> bool;
    fn reviewers_for(&self, repo: &Path) -> Vec<String>;
    fn read_tracked(
        &self,
        keys: &BTreeSet<String>,
    ) -> Result<(BTreeMap<String, String>, i64), DoorError>;
    fn read_pr(
        &self,
        cand: &Candidate,
        reviewers: &[String],
        timeout_s: f64,
    ) -> Result<Observation, DoorError>;
    fn graphql_remaining(&self) -> (Option<i64>, Option<String>);
    fn merge_ready(&self, repo: &Path) -> Result<bool, DoorError>;
    fn fire_check(&self, cand: &Candidate) -> Result<Fire, DoorError>;
    fn dispatch_ritual(
        &self,
        cand: &Candidate,
        obs: &Observation,
    ) -> Result<RitualAnswer, DoorError>;
    fn flip_draft(&self, cand: &Candidate) -> Result<(), DoorError>;
    fn finish_queue_merge(&self, repo: &Path, pr: i64) -> Result<(), DoorError>;
    fn ensure_merge_cleanup(&self, cand: &Candidate, state: &str) -> Result<(), DoorError>;
    fn freeze_refusal(&self, pr: i64) -> Result<Option<String>, DoorError>;
    fn gh_backoff_left(&self) -> Result<f64, DoorError>;
    fn merge(&self, pr: i64, cwd: &Path) -> Result<Value, DoorError>;
}

pub(crate) struct SweepOpts {
    pub store_path: PathBuf,
    pub now_iso: String,
    pub max_age_days: i64,
    pub max_retries: i64,
    pub graphql_min_remaining: i64,
    pub dispatch_deadline: Option<Instant>,
    /// Rich reads skip a row polled within this window; 0 turns the floor off.
    pub min_repoll_s: i64,
}

/// The `last_polled_at` cutoff a repoll floor of `floor_s` implies, or `None`
/// when `now_iso` does not parse (the floor then stays off for this tick).
fn repoll_cutoff(now_iso: &str, floor_s: i64) -> Option<String> {
    if floor_s <= 0 {
        return None;
    }
    let now = chrono::DateTime::parse_from_rfc3339(now_iso).ok()?;
    let stamp = now - chrono::Duration::seconds(floor_s);
    Some(stamp.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn ritual_timeout() -> f64 {
    clock::left().map_or(RITUAL_TIMEOUT_CAP_S, |left| {
        RITUAL_TIMEOUT_CAP_S.min(left - 10.0)
    })
}

fn retries_of(entry: &Value) -> i64 {
    match entry.get("retries") {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

fn truthy_str(entry: &Value, key: &str) -> bool {
    match entry.get(key) {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

fn set_field(entry: &mut Value, key: &str, value: Value) {
    if let Some(map) = entry.as_object_mut() {
        map.insert(key.to_string(), value);
    }
}

fn is_terminal(state: &str) -> bool {
    matches!(state, "MERGED" | "CLOSED")
}

/// A terminal memo: without it every tick re-reads the same merged candidates.
fn mark_handled(delivery: &mut Store, key: &str, state: &str) {
    let mut row = delivery
        .get(key)
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    set_field(&mut row, "handled", json!(state));
    delivery.put(key, row);
}

fn drop_terminal(store: &mut Store, dropped: &mut Vec<Value>, key: &str, state: &str) {
    if store.remove(key) {
        dropped.push(json!({"key": key, "reason": state.to_lowercase(), "state": state}));
    }
}

fn notify_parked(seams: &dyn Seams, pr: i64, slug: &str, retries: i64, why: &str) {
    seams.notify(&format!(
        "PR #{pr} ({slug}) parked after {retries} failed {why} attempts"
    ));
}

fn compact_keys<'a>(keys: impl IntoIterator<Item = &'a String>) -> Value {
    let mut grouped: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    let mut loose = BTreeSet::new();
    for key in keys {
        match parse_key(key) {
            Some((repo, number)) => {
                grouped.entry(repo).or_default().insert(number);
            }
            None => {
                loose.insert(key.clone());
            }
        }
    }
    let mut out: Map<String, Value> = grouped
        .into_iter()
        .map(|(repo, numbers)| (repo, json!(numbers)))
        .collect();
    if !loose.is_empty() {
        out.insert("_unqualified".into(), json!(loose));
    }
    Value::Object(out)
}

fn compact_drops(dropped: &[Value]) -> Value {
    let mut by_reason: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for row in dropped {
        let reason = row
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let key = row.get("key").and_then(Value::as_str).unwrap_or("");
        by_reason
            .entry(reason.to_string())
            .or_default()
            .push(key.to_string());
    }
    Value::Object(
        by_reason
            .into_iter()
            .map(|(reason, keys)| (reason, compact_keys(&keys)))
            .collect(),
    )
}

/// Flatten an oversized receipt into exact, independently chunkable facts.
fn receipt_items(receipt: &Value) -> Vec<Value> {
    let mut items = Vec::new();
    let spell = |repo: &str, number: &Value| match (repo, number) {
        ("_unqualified", Value::String(s)) => s.clone(),
        ("_unqualified", other) => other.to_string(),
        (repo, number) => format!("{repo}#{number}"),
    };
    for (repo, numbers) in receipt["swept"].as_object().into_iter().flatten() {
        for number in numbers.as_array().into_iter().flatten() {
            items.push(json!({"action": "swept", "key": spell(repo, number)}));
        }
    }
    for (reason, compact) in receipt["dropped"].as_object().into_iter().flatten() {
        for (repo, numbers) in compact.as_object().into_iter().flatten() {
            for number in numbers.as_array().into_iter().flatten() {
                items.push(
                    json!({"action": "dropped", "key": spell(repo, number), "reason": reason}),
                );
            }
        }
    }
    for row in receipt["normalized"].as_array().into_iter().flatten() {
        items.push(json!({"action": "normalized", "key": row["from"], "target": row["to"]}));
    }
    for key in receipt["failed"].as_array().into_iter().flatten() {
        items.push(json!({"action": "failed", "key": key}));
    }
    items
}

/// Pack exact facts below the payload ceiling with ample envelope room.
fn chunk_items(items: Vec<Value>, limit: usize) -> Vec<Vec<Value>> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    let mut bytes = 256;
    for item in items {
        let size = item.to_string().len() + 1;
        if !current.is_empty() && bytes + size > limit {
            chunks.push(std::mem::take(&mut current));
            bytes = 256;
        }
        current.push(item);
        bytes += size;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn emit_receipt(seams: &dyn Seams, receipt: Value) -> Result<(), String> {
    let maximum = crate::events_limits::max_data_bytes();
    let limit = maximum - maximum / 4;
    if receipt.to_string().len() <= limit {
        return if seams.emit("pr_watch_tick", receipt) {
            Ok(())
        } else {
            Err("pr-watch tick receipt emission failed".to_string())
        };
    }
    let receipt_id = crate::plugins::random_hex(16);
    let chunks = chunk_items(receipt_items(&receipt), limit);
    let count = chunks.len();
    for (index, items) in chunks.into_iter().enumerate() {
        let data = json!({"receipt_id": receipt_id, "chunk_index": index + 1, "chunk_count": count,
                          "item_count": items.len(), "items": items});
        if !seams.emit("pr_watch_sweep_chunk", data) {
            return Err(format!(
                "pr-watch sweep receipt chunk {}/{count} emission failed",
                index + 1
            ));
        }
    }
    let mut summary = receipt;
    for (key, empty) in [
        ("swept", json!({})),
        ("dropped", json!({})),
        ("normalized", json!([])),
        ("failed", json!([])),
    ] {
        set_field(&mut summary, key, empty);
    }
    set_field(&mut summary, "receipt_id", json!(receipt_id));
    set_field(&mut summary, "receipt_chunks", json!(count));
    if seams.emit("pr_watch_tick", summary) {
        Ok(())
    } else {
        Err("pr-watch tick receipt summary emission failed".to_string())
    }
}

/// Take the tick mutex and sweep. A held lock emits nothing, so a wedged
/// predecessor cannot write one row every interval; the caller prints why.
pub(crate) fn sweep(
    seams: &dyn Seams,
    entries: Vec<Value>,
    opts: &SweepOpts,
) -> Result<TickResult, DoorError> {
    let holder = format!("pr-watch:{}", std::process::id());
    clock::set_phase("lock");
    if let Err(reason) = seams.acquire_lock(TICK_CLAIM_KEY, &holder) {
        return Ok(TickResult {
            lock_held: true,
            lock_holder: reason,
            ..Default::default()
        });
    }
    // A fleet budget hold pauses the whole sweep: every listing and rich read
    // would answer refused, and a refused read would poison the row it
    // touched. Rows keep their last-known states; the next tick retries.
    let hold = match seams.gh_backoff_left() {
        Err(DoorError::Cut) => {
            seams.release_lock(TICK_CLAIM_KEY, &holder);
            return Err(DoorError::Cut);
        }
        Err(DoorError::Failed(_)) => 0.0,
        Ok(left) => left,
    };
    if hold > GH_HOLD_SKIP_S {
        seams.release_lock(TICK_CLAIM_KEY, &holder);
        return Ok(TickResult {
            budget_hold_s: Some(hold),
            ..Default::default()
        });
    }
    let result = run_sweep(seams, entries, opts, &holder);
    seams.release_lock(TICK_CLAIM_KEY, &holder);
    result
}

fn run_sweep(
    seams: &dyn Seams,
    entries: Vec<Value>,
    opts: &SweepOpts,
    holder: &str,
) -> Result<TickResult, DoorError> {
    let now = opts.now_iso.as_str();
    clock::set_phase("sweep:discover");
    let mut candidates = discover::discover_open_prs(&entries, now, opts.max_age_days);
    let mut store = Store::load(opts.store_path.clone());
    let candidate_keys: BTreeSet<String> = candidates
        .iter()
        .filter_map(|c| make_key(c.repo_slug.as_deref(), c.pr))
        .collect();
    let Normalization {
        normalized,
        dropped,
    } = store.normalize_keys();
    let mut dropped = dropped;
    let mut delivery = Store::load(delivery_path(&opts.store_path));
    // Prune only on a healthy discovery: zero candidates is the graph-hiccup
    // signature, and pruning on it would wipe every terminal PR's retry facts.
    if !candidate_keys.is_empty() {
        let stale: Vec<String> = delivery
            .data()
            .keys()
            .filter(|k| !candidate_keys.contains(*k))
            .cloned()
            .collect();
        for key in stale {
            delivery.remove(&key);
            dropped.push(json!({"key": key, "reason": "delivery-pruned"}));
        }
    }
    let mut swept = BTreeSet::new();
    let mut failed = BTreeSet::new();

    // Discovery is not a retention oracle. Batch every record that cannot need
    // a transition dispatch; only the actionable few pay the rich read.
    let batch_keys: BTreeSet<String> = store
        .data()
        .iter()
        .filter(|(key, entry)| {
            !candidate_keys.contains(*key)
                || truthy_str(entry, "merge_dispatched")
                || truthy_str(entry, "parked")
        })
        .map(|(key, _)| key.clone())
        .collect();
    let mut batch_terminal = BTreeSet::new();
    let mut batch_baselined = BTreeSet::new();
    let query_keys: BTreeSet<String> = batch_keys.union(&candidate_keys).cloned().collect();
    let mut sweep_failures = 0;
    let mut batch_states = BTreeMap::new();
    clock::set_phase("sweep:listing");
    if !query_keys.is_empty() {
        match seams.read_tracked(&query_keys) {
            Ok((states, failures)) => {
                batch_states = states;
                sweep_failures = failures;
            }
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(reason)) => {
                eprintln!("pr-watch: tracked-state sweep failed: {reason}");
                sweep_failures = 1;
            }
        }
        // Only positively observed states count as swept: an UNKNOWN can mean
        // the instrument never ran, and a dead cache must not pass as completed.
        for key in &query_keys {
            if !batch_states.contains_key(key) {
                failed.insert(key.clone());
            }
        }
        for (key, current) in &batch_states {
            if matches!(current.as_str(), "OPEN" | "CLOSED" | "MERGED" | "NOT_OPEN") {
                swept.insert(key.clone());
            } else if query_keys.contains(key) {
                failed.insert(key.clone());
            }
        }
        for key in &batch_keys {
            let current = batch_states.get(key).map_or("UNKNOWN", String::as_str);
            if matches!(current, "MERGED" | "CLOSED" | "NOT_OPEN") {
                drop_terminal(&mut store, &mut dropped, key, current);
                batch_terminal.insert(key.clone());
            } else if let Some(mut entry) = store.get(key).cloned() {
                set_field(&mut entry, "last_seen_state", json!(current));
                store.put(key, entry);
                if current == "UNKNOWN" {
                    failed.insert(key.clone());
                }
            }
        }
        for (key, current) in &batch_states {
            if current == "OPEN" && store.get(key).is_none() {
                // Stamp the baseline clock, not null: a null review watermark
                // fires on any pre-existing activity at the next rich read.
                store.put(
                    key,
                    json!({"last_review_ts": now, "last_seen_state": "OPEN",
                           "merge_dispatched": false, "retries": 0, "parked": null}),
                );
                swept.insert(key.clone());
                batch_baselined.insert(key.clone());
            }
        }
        // Both open-count readers count last_seen_state, so a row frozen at an
        // old OPEN read would inflate the count forever. decide() never reads it.
        for key in &candidate_keys {
            let Some(current) = batch_states
                .get(key)
                .filter(|c| matches!(c.as_str(), "OPEN" | "NOT_OPEN"))
            else {
                continue;
            };
            if let Some(mut row) = store.get(key).filter(|r| r.is_object()).cloned() {
                set_field(&mut row, "last_seen_state", json!(current));
                store.put(key, row);
            }
        }
    }

    // OPEN candidates lead, then healthy rows by least-recently-read, then
    // read-failed rows WITHIN their OPEN-ness group. Discovery order breaks ties.
    let order = |index: usize, cand: &Candidate| -> (i32, i32, String, usize) {
        let Some(key) = make_key(cand.repo_slug.as_deref(), cand.pr) else {
            return (2, 1, String::new(), index);
        };
        let head = i32::from(batch_states.get(&key).map(String::as_str) != Some("OPEN"));
        let row = store.get(&key);
        let rec = delivery.get(&key);
        let unknown = row
            .and_then(|r| r.get("last_seen_state"))
            .and_then(Value::as_str)
            == Some("UNKNOWN");
        let read_failed = rec
            .and_then(|r| r.get("last_read_failed"))
            .and_then(Value::as_bool)
            == Some(true);
        let stamp = [row, rec]
            .iter()
            .filter_map(|v| {
                v.and_then(|r| r.get("last_polled_at"))
                    .and_then(Value::as_str)
            })
            .max()
            .unwrap_or("")
            .to_string();
        (head, i32::from(unknown || read_failed), stamp, index)
    };
    let mut ranked: Vec<_> = candidates
        .drain(..)
        .enumerate()
        .map(|(i, c)| (order(i, &c), c))
        .collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0));
    let candidates: Vec<Candidate> = ranked.into_iter().map(|(_, c)| c).collect();

    let (mut acted, mut skipped, mut scanned, mut read_failures) = (0, 0, 0, 0);
    let cutoff = repoll_cutoff(now, opts.min_repoll_s);

    // The dispatch pass bills the shared GraphQL pool; a drained bucket stalls
    // each query for its full timeout. An unreadable instrument PROCEEDS. A
    // skip persists nothing and mints no receipt: no sweep completed.
    let (quota_remaining, quota_reset) = seams.graphql_remaining();
    let quota_unknown = quota_remaining.is_none();
    if quota_remaining.is_some_and(|q| q < opts.graphql_min_remaining) {
        return Ok(TickResult {
            sweep_failures,
            quota_skip: true,
            quota_remaining,
            quota_reset,
            ..Default::default()
        });
    }

    clock::set_phase("sweep:dispatch");
    let total = candidates.len();
    for cand in &candidates {
        let pr = cand.pr;
        let slug = cand.repo_slug.clone().unwrap_or_default();
        let Some(key) = make_key(cand.repo_slug.as_deref(), pr) else {
            seams.emit(
                "pr_watch_skipped",
                json!({"pr": pr, "reason": "unresolvable-repo"}),
            );
            skipped += 1;
            continue;
        };
        if batch_terminal.contains(&key) {
            continue;
        }
        if batch_baselined.contains(&key) {
            let reason = if cand.repo_dir.is_none() {
                Some("no-checkout")
            } else if seams.is_node_live(&cand.node_id) {
                Some("claimed")
            } else {
                None
            };
            if let Some(reason) = reason {
                seams.emit("pr_watch_skipped", json!({"pr": pr, "reason": reason}));
                skipped += 1;
            }
            continue;
        }
        if batch_keys.contains(&key) && store.get(&key).is_some_and(|e| truthy_str(e, "parked")) {
            continue;
        }
        // Terminal memo: a merged PR never reopens, so its memo holds even when
        // the listing cannot answer.
        let rec = delivery.get(&key);
        let handled = rec
            .and_then(|r| r.get("handled"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let rec_parked = rec.is_some_and(|r| truthy_str(r, "parked"));
        let listing_terminal = matches!(
            batch_states.get(&key).map(String::as_str),
            Some("NOT_OPEN" | "MERGED" | "CLOSED")
        );
        if handled.as_deref() == Some("MERGED")
            || ((handled.as_deref() == Some("CLOSED") || rec_parked) && listing_terminal)
        {
            skipped += 1;
            continue;
        }
        // The repoll floor: the listing leg already answered this row's state,
        // so a rich read inside the window buys nothing. The LRU order above
        // spreads the stale rows across ticks; this is what makes the order a
        // bound instead of a hint.
        if let Some(cutoff) = &cutoff {
            let stamp = [store.get(&key), delivery.get(&key)]
                .into_iter()
                .flatten()
                .filter_map(|row| row.get("last_polled_at").and_then(Value::as_str))
                .max();
            if stamp.is_some_and(|s| s >= cutoff.as_str()) {
                skipped += 1;
                continue;
            }
        }
        if opts.dispatch_deadline.is_some_and(|d| {
            d.saturating_duration_since(Instant::now()).as_secs_f64() < READ_FLOOR_S
        }) {
            seams.emit(
                "pr_watch_skipped",
                json!({"pr": pr, "reason": "tick-budget"}),
            );
            skipped += 1;
            break;
        }
        let pr_lock = format!(
            "pr-watch:{}:{pr}",
            if slug.is_empty() { "unknown" } else { &slug }
        );
        if seams.acquire_lock(&pr_lock, holder).is_err() {
            skipped += 1;
            continue;
        }
        let mut scan = ScanCounts {
            scanned,
            read_failures,
            total,
        };
        let step = one_candidate(
            seams,
            cand,
            &key,
            &slug,
            opts,
            &mut store,
            &mut delivery,
            &mut dropped,
            &mut swept,
            &mut failed,
            &mut scan,
            &mut acted,
            &mut skipped,
        );
        scanned = scan.scanned;
        read_failures = scan.read_failures;
        // Delivery retries persist per candidate: a mid-tick crash must not
        // reset a terminal PR's retry count and re-arm unbounded attempts.
        if let Err(err) = delivery.persist() {
            eprintln!("pr-watch: delivery state persist failed: {err}");
        }
        seams.release_lock(&pr_lock, holder);
        if step? {
            clock::note("sweep", "budget hold: read pass ended early".to_string());
            break;
        }
    }

    store.persist().map_err(DoorError::Failed)?;
    delivery.persist().map_err(DoorError::Failed)?;
    let open_prs = store
        .data()
        .values()
        .filter(|e| e.get("last_seen_state").and_then(Value::as_str) == Some("OPEN"))
        .count() as i64;
    let receipt = json!({
        "open_prs": open_prs,
        "acted": acted,
        "swept_count": swept.len(),
        "swept": compact_keys(&swept),
        "dropped_count": dropped.len(),
        "dropped": compact_drops(&dropped),
        "normalized_count": normalized.len(),
        "normalized": normalized,
        "failed_count": failed.len(),
        "failed": failed,
        "sweep_failures": sweep_failures,
        "listing_api": "rest",
        // This tick LOOKED: rides a completed sweep only, zero included.
        "merge_scan": {"completed": true, "scanned": scanned},
    });
    emit_receipt(seams, receipt).map_err(DoorError::Failed)?;
    Ok(TickResult {
        open_prs,
        acted,
        skipped,
        sweep_failures,
        quota_unknown,
        ..Default::default()
    })
}

struct ScanCounts {
    scanned: i64,
    read_failures: i64,
    total: usize,
}

impl ScanCounts {
    fn publish(&self) {
        let mut note = format!("scanned={} of {}", self.scanned, self.total);
        if self.read_failures > 0 {
            note.push_str(&format!(" read_failed={}", self.read_failures));
        }
        clock::note("sweep", note);
    }
}

#[allow(clippy::too_many_arguments)]
fn one_candidate(
    seams: &dyn Seams,
    cand: &Candidate,
    key: &str,
    slug: &str,
    opts: &SweepOpts,
    store: &mut Store,
    delivery: &mut Store,
    dropped: &mut Vec<Value>,
    swept: &mut BTreeSet<String>,
    failed: &mut BTreeSet<String>,
    scan: &mut ScanCounts,
    acted: &mut i64,
    skipped: &mut i64,
) -> Result<bool, DoorError> {
    let pr = cand.pr;
    let now = opts.now_iso.as_str();
    let mut rec = delivery
        .get(key)
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    set_field(&mut rec, "last_polled_at", json!(now));
    delivery.put(key, rec.clone());

    let reviewers = cand
        .repo_dir
        .as_deref()
        .map(|d| seams.reviewers_for(d))
        .unwrap_or_default();
    // Never outlive the slice: an overrun would cut the post-loop persist.
    let budget = READ_BUDGET_S.min(ritual_timeout().max(1.0));
    let obs = match seams.read_pr(cand, &reviewers, budget) {
        Ok(obs) => obs,
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(reason)) => {
            // The instrument is down, not the PR: every later read fails the
            // same way, and a poisoned row would retry first forever. The
            // entry stamp above already throttles the retry through the floor.
            if reason.contains("budget_refused") || reason.contains("API rate limit exceeded") {
                eprintln!("pr-watch: gh budget holds at PR #{pr}; ending the read pass");
                seams.emit(
                    "pr_watch_skipped",
                    json!({"pr": pr, "reason": "budget-hold"}),
                );
                scan.publish();
                return Ok(true);
            }
            eprintln!("pr-watch: gh query failed for PR #{pr}: {reason}");
            failed.insert(key.to_string());
            scan.read_failures += 1;
            scan.publish();
            if let Some(mut stale) = store.get(key).filter(|v| v.is_object()).cloned() {
                set_field(&mut stale, "last_seen_state", json!("UNKNOWN"));
                store.put(key, stale);
            }
            set_field(&mut rec, "last_read_failed", json!(true));
            delivery.put(key, rec);
            return Ok(false);
        }
    };
    if let Some(map) = rec.as_object_mut() {
        map.remove("last_read_failed");
    }
    delivery.put(key, rec);
    swept.insert(key.to_string());
    failed.remove(key);
    scan.scanned += 1;
    scan.publish();

    // An observed draft goes back to ready; the door journals the flip.
    if obs.state == "OPEN" && obs.is_draft {
        match seams.flip_draft(cand) {
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(reason)) => {
                eprintln!("pr-watch: draft flip failed for PR #{pr}: {reason}")
            }
            Ok(()) => {}
        }
    }

    // A corrupt non-object entry reads as absent and re-baselines.
    let mut entry = store.get(key).filter(|v| v.is_object()).cloned();
    if let Some(row) = entry.as_mut() {
        set_field(row, "last_polled_at", json!(now));
        store.put(key, row.clone());
    }

    let skip_reason = if cand.repo_dir.is_none() {
        Some("no-checkout")
    } else if seams.is_node_live(&cand.node_id) {
        Some("claimed")
    } else {
        None
    };
    if let Some(reason) = skip_reason {
        seams.emit("pr_watch_skipped", json!({"pr": pr, "reason": reason}));
        *skipped += 1;
        if let Some(mut row) = entry {
            set_field(&mut row, "last_seen_state", json!(obs.state));
            store.put(key, row);
            if is_terminal(&obs.state) {
                drop_terminal(store, dropped, key, &obs.state);
            }
        }
        // No checkout means no tick can ever act; a live claim is transient.
        if reason == "no-checkout" && is_terminal(&obs.state) {
            mark_handled(delivery, key, &obs.state);
        }
        return Ok(false);
    }

    let mut entry = match entry {
        Some(row) => row,
        None if !is_terminal(&obs.state) => {
            // First sight records without firing.
            let baseline = json!({"last_review_ts": obs.latest_review_ts, "last_seen_state": obs.state,
                                  "merge_dispatched": false, "retries": 0, "parked": null,
                                  "last_polled_at": now});
            store.set(key, baseline).map_err(DoorError::Failed)?;
            return Ok(false);
        }
        None if obs.state == "MERGED" => {
            // Terminal rows never live in the cache, but a merged candidate
            // must still retry an idempotent ritual after a transient failure.
            let pending = delivery.get(key).cloned().unwrap_or_else(|| json!({}));
            json!({"last_review_ts": obs.latest_review_ts, "last_seen_state": "MERGED",
                   "merge_dispatched": false, "retries": pending.get("retries").cloned().unwrap_or(json!(0)),
                   "parked": pending.get("parked").cloned().unwrap_or(Value::Null), "last_polled_at": now})
        }
        None => {
            mark_handled(delivery, key, "CLOSED");
            return Ok(false);
        }
    };

    // An outside merge mints no request; the watch mints it before the parked check.
    match seams.ensure_merge_cleanup(cand, &obs.state) {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(reason)) => {
            eprintln!("pr-watch: merge-cleanup mint for PR #{pr} failed: {reason}")
        }
        Ok(()) => {}
    }
    if truthy_str(&entry, "parked") {
        return Ok(false);
    }
    let repo_dir = cand.repo_dir.clone().unwrap_or_default();
    let merge_ready = obs.state == "MERGED"
        && match seams.merge_ready(&repo_dir) {
            Ok(ready) => ready,
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(reason)) => {
                eprintln!("pr-watch: post_merge_readiness failed for PR #{pr}: {reason}");
                false
            }
        };
    let verdict = discover::decide(
        &obs,
        &entry,
        &reviewers,
        merge_ready,
        now,
        opts.max_age_days,
    );
    let terminal = is_terminal(&obs.state);
    match verdict {
        Verdict::Noop(reason) => {
            // First sight ends a merged candidate's watch: the ritual ran, or
            // readiness (repo-level config) cannot flip per PR.
            if matches!(reason, "merge-already-dispatched" | "merge-not-ready") && terminal {
                mark_handled(delivery, key, &obs.state);
            }
        }
        Verdict::Park(reason) => {
            set_field(&mut entry, "parked", json!(reason));
            set_field(&mut entry, "parked_at", json!(now));
            if !terminal {
                store.set(key, entry.clone()).map_err(DoorError::Failed)?;
            } else if reason == "closed" {
                mark_handled(delivery, key, &obs.state);
            }
            seams.emit("pr_watch_parked", json!({"pr": pr, "reason": reason}));
        }
        Verdict::Merge | Verdict::Review => {
            let is_merge = verdict == Verdict::Merge;
            let floor = if is_merge {
                RITUAL_FLOOR_S
            } else {
                FIRE_FLOOR_S
            };
            if ritual_timeout() < floor {
                // Skip the dispatch only: the scan continues and the next tick
                // re-decides with a fresh cursor.
                seams.emit(
                    "pr_watch_skipped",
                    json!({"pr": pr, "reason": "fire-budget"}),
                );
                *skipped += 1;
            } else {
                let ok;
                let mut refused = false;
                let mut route = None;
                if is_merge {
                    match seams.finish_queue_merge(&repo_dir, pr) {
                        Err(DoorError::Cut) => return Err(DoorError::Cut),
                        Err(DoorError::Failed(reason)) => {
                            eprintln!("pr-watch: queue-merge cleanup for PR #{pr} failed: {reason}")
                        }
                        Ok(()) => {}
                    }
                    let answer = match seams.dispatch_ritual(cand, &obs) {
                        Ok(answer) => Some(answer),
                        Err(DoorError::Cut) => return Err(DoorError::Cut),
                        Err(DoorError::Failed(reason)) => {
                            eprintln!("pr-watch: ritual dispatch for PR #{pr} failed: {reason}");
                            None
                        }
                    };
                    let outcome = answer.as_ref().map_or("", |a| a.outcome.as_str());
                    if outcome == "already-dispatched" {
                        // lock-contention: another detector holds the lock now
                        // and may still fail before its marker, so only the
                        // marker branch records the hand-off.
                        let contended = answer.as_ref().and_then(|a| a.detail.as_deref())
                            == Some("lock-contention");
                        if contended {
                            seams.emit(
                                "pr_watch_skipped",
                                json!({"pr": pr, "reason": "dispatch-in-flight"}),
                            );
                        } else {
                            mark_handled(delivery, key, &obs.state);
                            seams.emit(
                                "pr_watch_skipped",
                                json!({"pr": pr, "reason": "already-dispatched"}),
                            );
                        }
                        *skipped += 1;
                        drop_terminal(store, dropped, key, "MERGED");
                        return Ok(false);
                    }
                    if outcome == "disabled" {
                        // auto_run opt-in is off: a deliberate no-op. Park so the
                        // tick stops re-deciding across the discovery window.
                        delivery.put(
                            key,
                            json!({"retries": entry.get("retries").cloned().unwrap_or(json!(0)),
                                   "parked": "auto-run-disabled", "handled": obs.state}),
                        );
                        seams.emit(
                            "pr_watch_skipped",
                            json!({"pr": pr, "reason": "auto-run-disabled"}),
                        );
                        *skipped += 1;
                        drop_terminal(store, dropped, key, "MERGED");
                        return Ok(false);
                    }
                    ok = matches!(outcome, "dispatched" | "routed-warm");
                    if ok {
                        route = Some(if outcome == "routed-warm" {
                            "warm"
                        } else {
                            "cold"
                        });
                    }
                } else {
                    let fire = match seams.fire_check(cand) {
                        Ok(fire) => fire,
                        Err(DoorError::Cut) => return Err(DoorError::Cut),
                        Err(DoorError::Failed(reason)) => {
                            eprintln!("pr-watch: fire for PR #{pr} failed: {reason}");
                            Fire {
                                ok: false,
                                is_error: true,
                            }
                        }
                    };
                    ok = fire.ok;
                    refused = !fire.ok && !fire.is_error;
                }
                let kind = if is_merge { "merge" } else { "review" };
                if ok {
                    *acted += 1;
                    if is_merge {
                        set_field(&mut entry, "merge_dispatched", json!(true));
                    } else {
                        set_field(&mut entry, "last_review_ts", json!(obs.latest_review_ts));
                    }
                    set_field(&mut entry, "retries", json!(0));
                    if terminal {
                        mark_handled(delivery, key, &obs.state);
                    } else {
                        store.set(key, entry.clone()).map_err(DoorError::Failed)?;
                    }
                    let mut data = json!({"kind": kind, "pr": pr});
                    if let Some(route) = route {
                        set_field(&mut data, "route", json!(route));
                    }
                    seams.emit("pr_watch_dispatched", data);
                } else if refused {
                    // Not an attempt: no retry burned.
                    seams.emit(
                        "pr_watch_skipped",
                        json!({"pr": pr, "reason": "admission-refused"}),
                    );
                    *skipped += 1;
                    if terminal {
                        mark_handled(delivery, key, &obs.state);
                    }
                } else {
                    let retries = retries_of(&entry) + 1;
                    set_field(&mut entry, "retries", json!(retries));
                    seams.emit(
                        "pr_watch_dispatch_failed",
                        json!({"pr": pr, "retries": retries}),
                    );
                    if terminal {
                        mark_handled(delivery, key, &obs.state);
                    } else {
                        if retries >= opts.max_retries {
                            set_field(&mut entry, "parked", json!("retries-exhausted"));
                            set_field(&mut entry, "parked_at", json!(now));
                        }
                        store.set(key, entry.clone()).map_err(DoorError::Failed)?;
                        if retries >= opts.max_retries {
                            seams.emit(
                                "pr_watch_parked",
                                json!({"pr": pr, "reason": "retries-exhausted"}),
                            );
                            notify_parked(seams, pr, slug, retries, "dispatch");
                        }
                    }
                }
            }
        }
    }
    set_field(&mut entry, "last_seen_state", json!(obs.state));
    if terminal {
        drop_terminal(store, dropped, key, &obs.state);
    } else if store.get(key).is_some() {
        store.put(key, entry);
    }
    Ok(false)
}

/// One granted row of the durable-grant queue.
pub(crate) struct QueueRow {
    pub cand: Candidate,
    pub key: String,
    pub grant: Map<String, Value>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct QueueCounts {
    pub executed: i64,
    pub held: i64,
    pub failed: i64,
    pub skipped: i64,
    pub budget: i64,
}

/// The reason past a leading `<outcome>: ` prefix the merge renderer writes.
fn reason_after_outcome(reason: &str) -> &str {
    match reason.split_once(": ") {
        Some((word, rest))
            if matches!(
                word,
                "merged"
                    | "armed"
                    | "authorized"
                    | "held"
                    | "refused"
                    | "head_changed"
                    | "unknown"
                    | "failed"
            ) =>
        {
            rest
        }
        _ => reason,
    }
}

/// Drain granted rows; the counts sum to the queue length unless a cut stops
/// the walk. The attempt is spent BEFORE the merge call, so a cut mid-call
/// counts as one failed attempt.
pub(crate) fn run_execute_queue(
    seams: &dyn Seams,
    queue: &[QueueRow],
    store_path: &Path,
    max_retries: i64,
    now_iso: &str,
    counts: &mut QueueCounts,
) -> Result<(), DoorError> {
    let holder = format!("pr-watch-merge:{}", std::process::id());
    let mut store = Store::load(store_path.to_path_buf());
    let mut slowest = 0.0_f64;
    for row in queue {
        let pr = row.cand.pr;
        if let Some(why) = seams.freeze_refusal(pr)? {
            grant_row(seams, "held", row, &[("reason", json!(why))]);
            seams.emit(
                "pr_watch_skipped",
                json!({"pr": pr, "reason": "merge-freeze"}),
            );
            counts.held += 1;
            continue;
        }
        let slug = row.cand.repo_slug.clone().unwrap_or_default();
        let lock = format!(
            "pr-watch:{}:{pr}",
            if slug.is_empty() { "unknown" } else { &slug }
        );
        clock::set_phase("merge:prepare");
        if seams.acquire_lock(&lock, &holder).is_err() {
            seams.emit("pr_watch_skipped", json!({"pr": pr, "reason": "locked"}));
            counts.skipped += 1;
            continue;
        }
        let step = one_merge(
            seams,
            row,
            &slug,
            &mut store,
            max_retries,
            now_iso,
            &mut slowest,
            counts,
        );
        seams.release_lock(&lock, &holder);
        step?;
    }
    Ok(())
}

fn grant_row(seams: &dyn Seams, phase: &str, row: &QueueRow, extra: &[(&str, Value)]) {
    let mut data = Map::new();
    data.insert("phase".into(), json!(phase));
    data.insert("actor".into(), json!("pr-watch"));
    data.insert("pr".into(), json!(row.cand.pr));
    data.insert("node_id".into(), json!(row.cand.node_id));
    for (key, value) in extra {
        data.insert((*key).to_string(), value.clone());
    }
    data.extend(row.grant.clone());
    seams.emit("merge_grant_execution", Value::Object(data));
}

fn one_merge(
    seams: &dyn Seams,
    row: &QueueRow,
    slug: &str,
    store: &mut Store,
    max_retries: i64,
    now_iso: &str,
    slowest: &mut f64,
    counts: &mut QueueCounts,
) -> Result<(), DoorError> {
    let pr = row.cand.pr;
    let Some(mut entry) = store.get(&row.key).filter(|v| v.is_object()).cloned() else {
        seams.emit(
            "pr_watch_skipped",
            json!({"pr": pr, "reason": "no-watermark"}),
        );
        counts.skipped += 1;
        return Ok(());
    };
    let spent = clock::left().is_some_and(|left| left < MERGE_FLOOR_S.max(*slowest));
    let why = if truthy_str(&entry, "merge_dispatched") {
        Some("merged")
    } else if truthy_str(&entry, "parked") {
        Some("parked")
    } else if entry.get("last_seen_state").and_then(Value::as_str) == Some("NOT_OPEN") {
        Some("not-open")
    } else if spent {
        Some("execute-budget")
    } else {
        None
    };
    if let Some(why) = why {
        seams.emit("pr_watch_skipped", json!({"pr": pr, "reason": why}));
        if why == "execute-budget" {
            counts.budget += 1;
        } else {
            counts.skipped += 1;
        }
        return Ok(());
    }
    let backoff = seams.gh_backoff_left()?;
    if backoff > 10.0 {
        grant_row(
            seams,
            "held",
            row,
            &[(
                "reason",
                json!(format!("gh budget backoff {backoff:.0}s left")),
            )],
        );
        counts.held += 1;
        return Ok(());
    }
    let prior = retries_of(&entry);
    grant_row(seams, "reserved", row, &[]);
    set_field(&mut entry, "retries", json!(prior + 1));
    store
        .set(&row.key, entry.clone())
        .map_err(DoorError::Failed)?;
    clock::set_phase("merge:execute");
    let started = Instant::now();
    let cwd = row.cand.repo_dir.clone().unwrap_or_default();
    // The door answers exit 0 with the flow receipt even when the merge held
    // or failed; the flow's own exit rides the payload.
    let (rc, receipt) = match seams.merge(pr, &cwd) {
        Ok(receipt) => (
            receipt.get("exit").and_then(Value::as_i64).unwrap_or(0),
            receipt,
        ),
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(reason)) => {
            eprintln!("pr-watch: durable-grant merge for PR #{pr} crashed: {reason}");
            (1, json!({"reason": format!("crashed: {reason}")}))
        }
    };
    *slowest = slowest.max(started.elapsed().as_secs_f64());
    let reason: String = receipt
        .get("reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("no reason printed")
        .chars()
        .take(300)
        .collect();
    match rc {
        0 => {
            counts.executed += 1;
            set_field(&mut entry, "merge_dispatched", json!(true));
            set_field(&mut entry, "retries", json!(0));
            store.set(&row.key, entry).map_err(DoorError::Failed)?;
            grant_row(seams, "executed", row, &[]);
        }
        2 => {
            // Retryable with no failure budget; a terminal PR never retries.
            counts.held += 1;
            set_field(&mut entry, "retries", json!(prior));
            let bare = reason_after_outcome(&reason);
            if bare.starts_with("PR already ")
                || bare.contains("already merged")
                || bare.contains("already closed")
            {
                set_field(&mut entry, "last_seen_state", json!("NOT_OPEN"));
            }
            store
                .set(&row.key, entry.clone())
                .map_err(DoorError::Failed)?;
            grant_row(seams, "held", row, &[("reason", json!(reason))]);
            let red = bare.starts_with("checks are red");
            if red
                || bare.starts_with("worktree_head_mismatch")
                || bare.starts_with("worktree_dirty")
            {
                let park = if red {
                    "checks-red"
                } else {
                    bare.split(':').next().unwrap_or(bare)
                };
                set_field(&mut entry, "parked", json!(park));
                set_field(&mut entry, "parked_at", json!(now_iso));
                set_field(
                    &mut entry,
                    "parked_head",
                    receipt.get("observed_head").cloned().unwrap_or(Value::Null),
                );
                store.set(&row.key, entry).map_err(DoorError::Failed)?;
                seams.emit("pr_watch_parked", json!({"pr": pr, "reason": park}));
                notify_parked(seams, pr, slug, prior, "durable-grant merge");
            }
        }
        _ => {
            counts.failed += 1;
            grant_row(
                seams,
                "failed",
                row,
                &[("exit_code", json!(rc)), ("reason", json!(reason))],
            );
            if prior + 1 >= max_retries {
                set_field(&mut entry, "parked", json!("retries-exhausted"));
                set_field(&mut entry, "parked_at", json!(now_iso));
                store.set(&row.key, entry).map_err(DoorError::Failed)?;
                seams.emit(
                    "pr_watch_parked",
                    json!({"pr": pr, "reason": "retries-exhausted"}),
                );
                notify_parked(seams, pr, slug, prior + 1, "durable-grant merge");
            }
        }
    }
    Ok(())
}

/// The review fire's verdict from one spawn's exit and stdout. SUCCESS needs
/// exit 0 AND a JSON envelope without `is_error`; a code from the admission
/// gate is a durable stop answering, so no retry burns.
pub(crate) fn fire_verdict(code: i32, stdout: &str) -> Fire {
    use crate::spawn_gate::{EXIT_FLEET_STOP, EXIT_FLEET_STOP_UNAVAILABLE};
    if code == EXIT_FLEET_STOP || code == EXIT_FLEET_STOP_UNAVAILABLE {
        return Fire {
            ok: false,
            is_error: false,
        };
    }
    if code != 0 {
        return Fire {
            ok: false,
            is_error: true,
        };
    }
    let is_error = match serde_json::from_str::<Value>(stdout) {
        Ok(Value::Object(envelope)) => !matches!(
            envelope.get("is_error"),
            None | Some(Value::Bool(false)) | Some(Value::Null)
        ),
        _ => true,
    };
    Fire {
        ok: !is_error,
        is_error,
    }
}

/// argv for the review fire, or `None` when the PR binds no graph node: the
/// autonomous spawn refuses rather than naming a fake node after the number.
pub(crate) fn fire_argv(cand: &Candidate, repo: &Path) -> Option<Vec<String>> {
    if cand.node_id.is_empty() {
        return None;
    }
    if let Ok(seam) = std::env::var(FIRE_SEAM_ENV) {
        if !seam.is_empty() {
            return Some(vec![seam]);
        }
    }
    let name = crate::naming::dispatch_agent_name(
        Some("pw"),
        "r",
        &cand.node_id,
        None,
        Some(&format!("pr-{}", cand.pr)),
        None,
        None,
    )
    .ok()?;
    Some(vec![
        crate::scrape::fno_bin().to_string_lossy().into_owned(),
        "agents".into(),
        "spawn".into(),
        "--once".into(),
        "--harness".into(),
        "claude".into(),
        "--timeout".into(),
        (CHECK_TIMEOUT_S as i64).to_string(),
        "--cwd".into(),
        repo.display().to_string(),
        "--name".into(),
        name,
        "--output-format".into(),
        "json".into(),
        format!("/fno:ship pr check {}", cand.pr),
    ])
}

pub(crate) fn fire_timeout() -> f64 {
    CHECK_TIMEOUT_S + SPAWN_GRACE_S
}

pub(crate) fn ritual_bound() -> f64 {
    ritual_timeout()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake {
        events: RefCell<Vec<(String, Value)>>,
        notices: RefCell<Vec<String>>,
        tracked: BTreeMap<String, String>,
        obs: Option<Observation>,
        read_fails: bool,
        read_fail_reason: Option<String>,
        reads: RefCell<usize>,
        hold: f64,
        live: bool,
        reviewers: Vec<String>,
        fire: Option<Fire>,
        ritual: Option<RitualAnswer>,
        quota: Option<i64>,
        merge_receipt: Option<Value>,
        frozen: Option<String>,
        lock_refused: bool,
    }

    impl Seams for Fake {
        fn emit(&self, kind: &str, data: Value) -> bool {
            self.events.borrow_mut().push((kind.to_string(), data));
            true
        }
        fn notify(&self, message: &str) {
            self.notices.borrow_mut().push(message.to_string());
        }
        fn acquire_lock(&self, _: &str, _: &str) -> Result<(), String> {
            if self.lock_refused {
                Err("lock held by other".to_string())
            } else {
                Ok(())
            }
        }
        fn release_lock(&self, _: &str, _: &str) {}
        fn is_node_live(&self, _: &str) -> bool {
            self.live
        }
        fn reviewers_for(&self, _: &Path) -> Vec<String> {
            self.reviewers.clone()
        }
        fn read_tracked(
            &self,
            _: &BTreeSet<String>,
        ) -> Result<(BTreeMap<String, String>, i64), DoorError> {
            Ok((self.tracked.clone(), 0))
        }
        fn read_pr(&self, _: &Candidate, _: &[String], _: f64) -> Result<Observation, DoorError> {
            if self.read_fails {
                *self.reads.borrow_mut() += 1;
                let why = self
                    .read_fail_reason
                    .clone()
                    .unwrap_or_else(|| "gh down".to_string());
                return Err(DoorError::Failed(why));
            }
            *self.reads.borrow_mut() += 1;
            Ok(self.obs.clone().unwrap())
        }
        fn graphql_remaining(&self) -> (Option<i64>, Option<String>) {
            (self.quota, None)
        }
        fn merge_ready(&self, _: &Path) -> Result<bool, DoorError> {
            Ok(true)
        }
        fn fire_check(&self, _: &Candidate) -> Result<Fire, DoorError> {
            Ok(self.fire.clone().unwrap())
        }
        fn dispatch_ritual(
            &self,
            _: &Candidate,
            _: &Observation,
        ) -> Result<RitualAnswer, DoorError> {
            Ok(self.ritual.clone().unwrap())
        }
        fn flip_draft(&self, _: &Candidate) -> Result<(), DoorError> {
            Ok(())
        }
        fn finish_queue_merge(&self, _: &Path, _: i64) -> Result<(), DoorError> {
            Ok(())
        }
        fn ensure_merge_cleanup(&self, _: &Candidate, _: &str) -> Result<(), DoorError> {
            Ok(())
        }
        fn freeze_refusal(&self, _: i64) -> Result<Option<String>, DoorError> {
            Ok(self.frozen.clone())
        }
        fn gh_backoff_left(&self) -> Result<f64, DoorError> {
            Ok(self.hold)
        }
        fn merge(&self, _: i64, _: &Path) -> Result<Value, DoorError> {
            Ok(self.merge_receipt.clone().unwrap())
        }
    }

    struct Rig {
        dir: tempfile::TempDir,
        repo: PathBuf,
    }

    fn rig() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        Rig { dir, repo }
    }

    impl Rig {
        fn store(&self) -> PathBuf {
            self.dir.path().join("state.json")
        }
        fn seed(&self, value: Value) {
            std::fs::write(self.store(), value.to_string()).unwrap();
        }
        fn disk(&self) -> Value {
            serde_json::from_str(&std::fs::read_to_string(self.store()).unwrap()).unwrap()
        }
        fn entries(&self) -> Vec<Value> {
            vec![json!({"id": "n1", "cwd": self.repo, "pr_number": 7,
                        "pr_url": "https://github.com/o/r/pull/7"})]
        }
        fn opts(&self) -> SweepOpts {
            SweepOpts {
                store_path: self.store(),
                now_iso: "2026-11-01T00:00:00Z".to_string(),
                max_age_days: 14,
                max_retries: 2,
                graphql_min_remaining: 200,
                dispatch_deadline: None,
                min_repoll_s: 1200,
            }
        }
    }

    fn open_obs(ts: Option<&str>) -> Observation {
        Observation {
            state: "OPEN".into(),
            latest_review_ts: ts.map(str::to_string),
            ..Default::default()
        }
    }

    fn kinds(fake: &Fake) -> Vec<String> {
        fake.events
            .borrow()
            .iter()
            .map(|(k, _)| k.clone())
            .collect()
    }

    const KEY: &str = "o/r#7";

    #[test]
    fn a_first_seen_pr_baselines_without_firing() {
        let rig = rig();
        let fake = Fake {
            obs: Some(open_obs(Some("2026-10-30T00:00:00Z"))),
            reviewers: vec!["bot".into()],
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.acted, 0);
        assert_eq!(
            rig.disk()[KEY]["last_review_ts"],
            json!("2026-10-30T00:00:00Z")
        );
        assert_eq!(kinds(&fake), vec!["pr_watch_tick"]);
    }

    #[test]
    fn a_listing_baseline_suppresses_activity_that_predates_tracking() {
        let rig = rig();
        let tracked = BTreeMap::from([(KEY.to_string(), "OPEN".to_string())]);
        let fake = Fake {
            tracked,
            ..Default::default()
        };
        sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(
            rig.disk()[KEY]["last_review_ts"],
            json!("2026-11-01T00:00:00Z")
        );
    }

    #[test]
    fn new_review_activity_fires_and_advances_the_watermark() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_review_ts": "2026-10-01T00:00:00Z", "retries": 1, "last_seen_state": "OPEN"}}));
        let fake = Fake {
            obs: Some(open_obs(Some("2026-10-30T00:00:00Z"))),
            reviewers: vec!["bot".into()],
            fire: Some(Fire {
                ok: true,
                is_error: false,
            }),
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.acted, 1);
        assert_eq!(
            rig.disk()[KEY]["last_review_ts"],
            json!("2026-10-30T00:00:00Z")
        );
        assert_eq!(rig.disk()[KEY]["retries"], json!(0));
        assert!(kinds(&fake).contains(&"pr_watch_dispatched".to_string()));
    }

    #[test]
    fn failed_fires_park_at_the_cap_and_refusals_burn_nothing() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_review_ts": "2026-10-01T00:00:00Z", "retries": 1, "last_seen_state": "OPEN"}}));
        let refused = Fake {
            obs: Some(open_obs(Some("2026-10-30T00:00:00Z"))),
            reviewers: vec!["bot".into()],
            fire: Some(Fire {
                ok: false,
                is_error: false,
            }),
            ..Default::default()
        };
        sweep(&refused, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(rig.disk()[KEY]["retries"], json!(1));

        let failing = Fake {
            obs: Some(open_obs(Some("2026-10-30T00:00:00Z"))),
            reviewers: vec!["bot".into()],
            fire: Some(Fire {
                ok: false,
                is_error: true,
            }),
            ..Default::default()
        };
        // The repoll floor treats a same-instant second sweep as the same
        // tick; park-at-cap needs the next one, two floors later.
        let next_opts = SweepOpts {
            now_iso: "2026-11-01T02:00:00Z".to_string(),
            ..rig.opts()
        };
        sweep(&failing, rig.entries(), &next_opts).unwrap();
        assert_eq!(rig.disk()[KEY]["parked"], json!("retries-exhausted"));
        assert_eq!(failing.notices.borrow().len(), 1);
    }

    #[test]
    fn a_merged_pr_hands_off_once_and_memoizes() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_seen_state": "OPEN", "retries": 0}}));
        let fake = Fake {
            obs: Some(Observation {
                state: "MERGED".into(),
                ..Default::default()
            }),
            ritual: Some(RitualAnswer {
                outcome: "routed-warm".into(),
                detail: None,
            }),
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.acted, 1);
        assert!(
            rig.disk().get(KEY).is_none(),
            "a terminal row leaves the cache"
        );
        let delivery: Value =
            serde_json::from_str(&std::fs::read_to_string(delivery_path(&rig.store())).unwrap())
                .unwrap();
        assert_eq!(delivery[KEY]["handled"], json!("MERGED"));
        let dispatched = fake
            .events
            .borrow()
            .iter()
            .find(|(k, _)| k == "pr_watch_dispatched")
            .unwrap()
            .1
            .clone();
        assert_eq!(dispatched["route"], json!("warm"));
        // The memo answers the next tick without a rich read.
        let again = Fake {
            read_fails: true,
            ..Default::default()
        };
        let second = sweep(&again, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(second.skipped, 1);
    }

    #[test]
    fn lock_contention_on_the_ritual_writes_no_handled_memo() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_seen_state": "OPEN"}}));
        let fake = Fake {
            obs: Some(Observation {
                state: "MERGED".into(),
                ..Default::default()
            }),
            ritual: Some(RitualAnswer {
                outcome: "already-dispatched".into(),
                detail: Some("lock-contention".into()),
            }),
            ..Default::default()
        };
        sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        let delivery: Value =
            serde_json::from_str(&std::fs::read_to_string(delivery_path(&rig.store())).unwrap())
                .unwrap();
        assert!(delivery[KEY].get("handled").is_none());
    }

    #[test]
    fn a_read_failure_marks_unknown_and_stays_out_of_swept() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_seen_state": "OPEN"}}));
        let fake = Fake {
            read_fails: true,
            ..Default::default()
        };
        sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(rig.disk()[KEY]["last_seen_state"], json!("UNKNOWN"));
        let receipt = fake
            .events
            .borrow()
            .iter()
            .find(|(k, _)| k == "pr_watch_tick")
            .unwrap()
            .1
            .clone();
        assert_eq!(receipt["failed"], json!([KEY]));
        assert_eq!(receipt["merge_scan"]["scanned"], json!(0));
    }

    #[test]
    fn a_fresh_polled_row_skips_its_rich_read_inside_the_repoll_floor() {
        let rig = rig();
        rig.seed(
            json!({KEY: {"last_seen_state": "OPEN", "last_review_ts": "2026-10-01T00:00:00Z",
                              "retries": 0, "parked": null, "merge_dispatched": false,
                              "last_polled_at": "2026-11-01T00:00:00Z"}}),
        );
        let fake = Fake {
            obs: Some(open_obs(Some("2026-11-01T00:30:00Z"))),
            reviewers: vec!["bot".into()],
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.acted, 0);
        assert_eq!(*fake.reads.borrow(), 0);
        assert!(!kinds(&fake).contains(&"pr_watch_dispatched".to_string()));
        assert_eq!(
            rig.disk()[KEY]["last_review_ts"],
            json!("2026-10-01T00:00:00Z")
        );
    }

    #[test]
    fn a_stale_row_pays_its_rich_read_when_the_floor_has_passed() {
        let rig = rig();
        rig.seed(
            json!({KEY: {"last_seen_state": "OPEN", "last_review_ts": "2026-10-01T00:00:00Z",
                              "retries": 0, "parked": null, "merge_dispatched": false,
                              "last_polled_at": "2026-10-31T20:00:00Z"}}),
        );
        let fake = Fake {
            obs: Some(open_obs(Some("2026-11-01T00:30:00Z"))),
            reviewers: vec!["bot".into()],
            fire: Some(Fire {
                ok: true,
                is_error: false,
            }),
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.acted, 1);
        assert_eq!(*fake.reads.borrow(), 1);
        assert!(kinds(&fake).contains(&"pr_watch_dispatched".to_string()));
    }

    #[test]
    fn a_zero_floor_turns_the_repoll_bound_off() {
        let rig = rig();
        rig.seed(
            json!({KEY: {"last_seen_state": "OPEN", "last_review_ts": "2026-10-01T00:00:00Z",
                              "retries": 0, "parked": null, "merge_dispatched": false,
                              "last_polled_at": "2026-11-01T00:00:00Z"}}),
        );
        let opts = SweepOpts {
            min_repoll_s: 0,
            ..rig.opts()
        };
        let fake = Fake {
            obs: Some(open_obs(Some("2026-11-01T00:30:00Z"))),
            reviewers: vec!["bot".into()],
            fire: Some(Fire {
                ok: true,
                is_error: false,
            }),
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &opts).unwrap();
        assert_eq!(result.acted, 1);
        assert_eq!(*fake.reads.borrow(), 1);
    }

    #[test]
    fn a_budget_hold_pauses_the_sweep_before_any_read() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_seen_state": "OPEN"}}));
        let fake = Fake {
            hold: 45.0,
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.budget_hold_s, Some(45.0));
        assert_eq!(*fake.reads.borrow(), 0);
        assert!(kinds(&fake).is_empty());
        assert_eq!(rig.disk()[KEY]["last_seen_state"], json!("OPEN"));
    }

    #[test]
    fn a_budget_refused_read_ends_the_pass_and_keeps_the_row_clean() {
        let rig = rig();
        rig.seed(json!({
            "o/r#7": {"last_seen_state": "OPEN"},
            "o/r#8": {"last_seen_state": "OPEN"},
        }));
        let fake = Fake {
            read_fails: true,
            read_fail_reason: Some("pr-status gh outcome=budget_refused: held".to_string()),
            ..Default::default()
        };
        let entries = vec![
            json!({"id": "n1", "cwd": rig.repo, "pr_number": 7,
                    "pr_url": "https://github.com/o/r/pull/7"}),
            json!({"id": "n2", "cwd": rig.repo, "pr_number": 8,
                    "pr_url": "https://github.com/o/r/pull/8"}),
        ];
        sweep(&fake, entries, &rig.opts()).unwrap();
        assert_eq!(*fake.reads.borrow(), 1);
        assert!(kinds(&fake).contains(&"pr_watch_tick".to_string()));
        assert_eq!(rig.disk()["o/r#7"]["last_seen_state"], json!("OPEN"));
    }

    #[test]
    fn a_live_claim_skips_and_a_low_quota_mints_no_receipt() {
        let rig = rig();
        rig.seed(json!({KEY: {"last_seen_state": "OPEN"}}));
        let claimed = Fake {
            obs: Some(open_obs(None)),
            live: true,
            ..Default::default()
        };
        let result = sweep(&claimed, rig.entries(), &rig.opts()).unwrap();
        assert_eq!(result.skipped, 1);

        let starved = Fake {
            quota: Some(3),
            ..Default::default()
        };
        let result = sweep(&starved, rig.entries(), &rig.opts()).unwrap();
        assert!(result.quota_skip);
        assert!(kinds(&starved).is_empty());
    }

    #[test]
    fn a_held_lock_does_no_work_and_says_so() {
        let rig = rig();
        let fake = Fake {
            lock_refused: true,
            ..Default::default()
        };
        let result = sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert!(result.lock_held);
        assert!(kinds(&fake).is_empty());
    }

    #[test]
    fn a_parked_row_whose_pr_closed_leaves_the_cache_with_a_receipt() {
        let rig = rig();
        rig.seed(json!({KEY: {"parked": "max-age", "last_seen_state": "OPEN"}}));
        let tracked = BTreeMap::from([(KEY.to_string(), "NOT_OPEN".to_string())]);
        let fake = Fake {
            tracked,
            ..Default::default()
        };
        sweep(&fake, rig.entries(), &rig.opts()).unwrap();
        assert!(rig.disk().get(KEY).is_none());
        let receipt = fake
            .events
            .borrow()
            .iter()
            .find(|(k, _)| k == "pr_watch_tick")
            .unwrap()
            .1
            .clone();
        assert_eq!(receipt["dropped_count"], json!(1));
    }

    fn queue_row(rig: &Rig) -> QueueRow {
        QueueRow {
            cand: Candidate {
                node_id: "n1".into(),
                pr: 7,
                repo_dir: Some(rig.repo.clone()),
                repo_slug: Some("o/r".into()),
                ..Default::default()
            },
            key: KEY.to_string(),
            grant: Map::new(),
        }
    }

    fn drain(rig: &Rig, fake: &Fake) -> QueueCounts {
        let mut counts = QueueCounts::default();
        run_execute_queue(
            fake,
            &[queue_row(rig)],
            &rig.store(),
            2,
            "2026-11-05T00:00:00Z",
            &mut counts,
        )
        .unwrap();
        counts
    }

    #[test]
    fn a_merge_that_lands_marks_dispatched_and_clears_retries() {
        let rig = rig();
        rig.seed(json!({KEY: {"retries": 1}}));
        let fake = Fake {
            merge_receipt: Some(json!({"exit": 0})),
            ..Default::default()
        };
        assert_eq!(drain(&rig, &fake).executed, 1);
        assert_eq!(rig.disk()[KEY]["merge_dispatched"], json!(true));
        assert_eq!(rig.disk()[KEY]["retries"], json!(0));
    }

    #[test]
    fn a_red_hold_parks_on_the_observed_head_and_keeps_its_retries() {
        let rig = rig();
        rig.seed(json!({KEY: {"retries": 1}}));
        let fake = Fake {
            merge_receipt: Some(
                json!({"exit": 2, "reason": "held: checks are red", "observed_head": "abc"}),
            ),
            ..Default::default()
        };
        assert_eq!(drain(&rig, &fake).held, 1);
        assert_eq!(rig.disk()[KEY]["parked"], json!("checks-red"));
        assert_eq!(rig.disk()[KEY]["parked_head"], json!("abc"));
        assert!(
            !rig.disk()[KEY]["parked_at"].is_null(),
            "the park stamps its own clock"
        );
        assert_eq!(rig.disk()[KEY]["retries"], json!(1));
    }

    #[test]
    fn a_terminal_hold_reads_not_open_and_a_failure_parks_at_the_cap() {
        let rig = rig();
        rig.seed(json!({KEY: {"retries": 0}}));
        let held = Fake {
            merge_receipt: Some(json!({"exit": 2, "reason": "held: PR already merged"})),
            ..Default::default()
        };
        drain(&rig, &held);
        assert_eq!(rig.disk()[KEY]["last_seen_state"], json!("NOT_OPEN"));

        rig.seed(json!({KEY: {"retries": 1}}));
        let failed = Fake {
            merge_receipt: Some(json!({"exit": 1, "reason": "boom"})),
            ..Default::default()
        };
        assert_eq!(drain(&rig, &failed).failed, 1);
        assert_eq!(rig.disk()[KEY]["parked"], json!("retries-exhausted"));
        assert_eq!(rig.disk()[KEY]["retries"], json!(2));
    }

    #[test]
    fn a_freeze_holds_before_any_attempt_is_spent() {
        let rig = rig();
        rig.seed(json!({KEY: {"retries": 0}}));
        let fake = Fake {
            frozen: Some("freeze".into()),
            ..Default::default()
        };
        assert_eq!(drain(&rig, &fake).held, 1);
        assert_eq!(rig.disk()[KEY]["retries"], json!(0));
    }

    #[test]
    fn an_oversized_receipt_splits_into_exact_chunks() {
        let keys: Vec<String> = (1..2000).map(|n| format!("owner/repo#{n}")).collect();
        let receipt =
            json!({"swept": compact_keys(&keys), "dropped": {}, "normalized": [], "failed": []});
        let items = receipt_items(&receipt);
        assert_eq!(items.len(), 1999);
        let chunks = chunk_items(items, 4096);
        assert!(chunks.len() > 1);
        assert_eq!(chunks.iter().map(Vec::len).sum::<usize>(), 1999);
        assert!(chunks[0][0]["key"]
            .as_str()
            .unwrap()
            .starts_with("owner/repo#"));
    }

    #[test]
    fn a_fire_reads_the_envelope_and_not_only_the_exit() {
        assert!(fire_verdict(0, r#"{"is_error": false}"#).ok);
        assert!(fire_verdict(0, r#"{"is_error": true}"#).is_error);
        assert!(fire_verdict(0, "not json").is_error);
        assert!(fire_verdict(1, "").is_error);
        let refused = fire_verdict(crate::spawn_gate::EXIT_FLEET_STOP, "");
        assert!(!refused.ok && !refused.is_error);
    }

    #[test]
    fn a_pr_with_no_node_refuses_the_fire() {
        assert!(fire_argv(&Candidate::default(), Path::new("/")).is_none());
    }
}
