//! The lead board's budget-aware collector, in Rust.
//!
//! Ports `cli/src/fno/lead/board.py`'s `build_board` + `collect_inputs` with
//! the budget branch built in (the reference, `feature/`
//! bfd1a5e8e, which could not land because the file-budget gate refuses grown
//! Python): the caller hands in ONE whole-board budget, every per-source slice
//! derives from it, and as the budget runs out the board stops starting reads,
//! marks each unstarted source, and still emits the payload it has.
//!
//! The speed win is in-process reads. One graph read (the store's
//! `backlog::api::rows`, the same defaulted view the keeper serves) feeds
//! claimed-node lookups, PR binding, team scope, AND the unplanned
//! queue's ready selection (the same `backlog_ready::select` the command
//! serves). The claims merge is a directory scan. `needs` folds in-process
//! over the same sources `fno
//! agents needs` reads. Two source reads stay subprocesses: `gh pr list` (a
//! real network boundary) and `fno inbox outstanding` (measured 2026-09-04:
//! 1.12s wall at load 52, far under its 10s bar - the plan's change 2 keeps it
//! and records the measurement). Undispatched reuses the native observer over
//! the graph, claims, and worked rows already loaded for this board.
//!
//! Output keeps the Python JSON shape: `actionable`, `unreadable`, `queues`
//! (same names, same order, same row dicts), `warnings`, `exit_code` - plus a
//! per-source `sources` map carrying `ok`/`truncated` that the plan's
//! done-probe reads. `parse_org_board_value` in loopcheck.rs ignores the
//! extra key, so every existing reader is unchanged.
//!
//! Module layout: `budget` (the one whole-board budget + bounded
//! subprocess runner), `claims` (the merged lock scan), `classify`
//! (holder/driver selection), `prs` (one listing, binding,
//! mergeable filter), `scope` (config paths + team scope), `queues`
//! (the lane parser + the eleven-queue build). This parent holds the
//! shared value vocabulary, the options, the collection orchestration,
//! and the board-shape tests.

pub(crate) mod budget;
mod claims;
mod classify;
pub mod pr_closure;
pub mod pr_closure_produce;
pub(crate) mod prs;
pub(crate) mod queues;
pub(crate) mod scope;
mod stranded;

pub(crate) use queues::{not_read_status, unreadable_status};

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) use crate::territory::compile_scope_ids;
pub(crate) use budget::{fno_py_cmd, now_secs_board, run_json, Budget, HAND_RUN_BUDGET_MS};
pub(crate) use claims::read_claims;
pub(crate) use classify::{entry_by_id, node_has_pr, read_claimed_nodes};
pub(crate) use prs::{read_pr_gates, read_prs};
pub(crate) use queues::{
    build_board, parse_lane, queue_json, read_blocked_rows, BoardInputs, Queue,
};
pub(crate) use scope::{
    autonomous_merge_enabled, blocked_child_grace_minutes, foreign_owned_read, graph_json_path,
    home_dot_fno, operator_lane_path, parse_manifest, project_map,
};
pub(crate) use stranded::{read_stranded_trees, stranded_candidates, STRANDED_GRACE_MINUTES};

/// Priorities a lead treats as its own work. Lower bands are the operator's.
pub(crate) const LEAD_PRIORITIES: [&str; 2] = ["p0", "p1"];

/// Claim states that mean the lock outlived its holder.
pub(crate) const DEAD_CLAIM_STATES: [&str; 2] = ["stale", "corrupted"];

pub(crate) const TERMINAL_RUNGS: [&str; 2] = ["done", "superseded"];
pub(crate) const LEGACY_DEFER_PREFIX: &str = "deferred:";

/// The one terminal test every reader of a row agrees on: a terminal status
/// stamp, a supersession pointer, or a completed stamp (legacy deferrals
/// excepted). Gate, board, and PR classification share it so they can never
/// disagree about what is live.
pub(crate) fn is_terminal(entry: &Value) -> bool {
    s_str(entry, "status")
        .map(|s| TERMINAL_RUNGS.contains(&s))
        .unwrap_or(false)
        || entry.get("superseded_by").is_some_and(|v| !v.is_null())
        || entry
            .get("completed_at")
            .and_then(Value::as_str)
            .map(|c| !c.is_empty() && !c.starts_with(LEGACY_DEFER_PREFIX))
            .unwrap_or(false)
}

/// The literal commands a reader can re-run; they ARE the checkability
/// property, so they sit beside the readers (board.py spelled them identically).
pub(crate) const SRC_UNDISPATCHED: &str = "fno backlog undispatched --json";
/// The unplanned queue's ready source answers in-process now; the label
/// names the function, the way `agents claim list` labels its source.
pub(crate) const SRC_READY: &str = "backlog_ready::select (-A)";
pub(crate) const SRC_WORKED: &str = "backlog::worked::json_rows (-J)";
pub(crate) const SRC_CLAIMS: &str = "fno agents claim list -J --include-stale --prefix node:";
/// The driver feed: registry rows that target a node. In-process,
/// like SRC_READY; the label names the mechanism, not a command.
pub(crate) const SRC_DRIVERS: &str = "registry::load_registry (rows with node)";
/// The owner feed: nodes a deeper live team holds. In-process, like the
/// driver feed it shares a registry with.
pub(crate) const SRC_OWNERS: &str = "territory::owner_read (nodes of other live teams)";

/// The roster feed's cap, mirroring MAX_CLAIMED_NODE_READS: a retained
/// registry can outgrow what one board read may tax the probe with. The
/// truncated tail reads unmeasured (loud), never none.
pub(crate) const MAX_DRIVER_ROWS: usize = 24;
/// The open-PR listing's default read depth. 50 covers a 21-open-PR fleet
/// with headroom; `--max-pr-reads` still overrides it.
pub(crate) const DEFAULT_MAX_PR_READS: usize = 50;
pub(crate) const SRC_PRS: &str =
    "gh pr list --state open --json number,title,mergeable,statusCheckRollup,headRefName,isCrossRepository,url";
pub(crate) const SRC_PR_NODES: &str = "gh pr list --state open --json number,title,mergeable,statusCheckRollup,headRefName,isCrossRepository,url + fno backlog get <id>";
/// the mergeable_pr queue asks the gate, not just the listing.
pub(crate) const SRC_PR_GATE: &str =
    "fno do pr status <n> (ready + ready_blockers per mergeable candidate)";
pub(crate) const SRC_QUESTIONS: &str = "fno inbox outstanding --json";
pub(crate) const SRC_NEEDS: &str = "fno agents needs --json";
pub(crate) const SRC_DISTRESS: &str =
    "~/.fno/events.jsonl (blocked rows) + bus/messages.jsonl + fno agents distress-verdicts";
pub(crate) const SRC_STRANDED: &str = "git worktree list --porcelain (per repo) + per-tree probe";

fn worked_node_ids(read: &SourceRead) -> HashSet<String> {
    read.payload
        .as_ref()
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("id").and_then(Value::as_str).map(str::to_string))
        .collect()
}

fn undispatched_from_board(
    entries: Option<&[Value]>,
    claims: &SourceRead,
    worked: &SourceRead,
) -> SourceRead {
    let Some(entries) = entries else {
        return SourceRead::err("graph unreadable: no entries for undispatched selection");
    };
    if !claims.is_ok() {
        return claims.rewrap(
            claims
                .error
                .clone()
                .unwrap_or_else(|| "claims unreadable".to_string()),
        );
    }
    if !worked.is_ok() {
        return SourceRead::err(format!(
            "worked overlay unreadable: {}",
            worked.error.as_deref().unwrap_or("worked read failed")
        ));
    }
    let mut claim_rows = Vec::new();
    for claim in claims.rows() {
        let Some(key) = claim.get("key").and_then(Value::as_str) else {
            return SourceRead::err(
                crate::backlog::undispatched::CLAIMS_UNREADABLE_KEY_NOT_A_STRING,
            );
        };
        claim_rows.push(json!({"key": key, "state": Value::Null}));
    }
    for id in worked_node_ids(worked) {
        claim_rows.push(json!({
            "key": format!("node:{id}"),
            "state": "live-worker"
        }));
    }
    match crate::backlog::undispatched::classify_planned_unclaimed(
        entries,
        &claim_rows,
        None,
        None,
        None,
        None,
    ) {
        Ok(receipt) => receipt
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .map(|rows| SourceRead::ok(Value::Array(rows)))
            .unwrap_or_else(|| SourceRead::err("undispatched: native observer omitted its rows")),
        Err(reason) => SourceRead::err(reason),
    }
}

/// Split held nodes out of the ready feed: an open question whose `blocks`
/// names a node keeps it off the board's ready list and under the `held`
/// source with the question id. Pure so the split is testable; the
/// held map is `needs::held_nodes` over the question journals. A ready feed
/// that never answered passes through untouched - the split never reads an
/// unreadable source as held-free.
fn partition_held(
    ready: SourceRead,
    held: &std::collections::BTreeMap<String, String>,
) -> (SourceRead, Vec<Value>) {
    if held.is_empty() || !ready.is_ok() {
        return (ready, Vec::new());
    }
    let (mut kept, mut held_rows) = (Vec::new(), Vec::new());
    for mut row in ready.rows() {
        let id = row.get("id").and_then(Value::as_str).unwrap_or("");
        match held.get(id) {
            Some(qid) => {
                if let Some(obj) = row.as_object_mut() {
                    obj.insert("question_id".to_string(), json!(qid));
                }
                held_rows.push(row);
            }
            None => kept.push(row),
        }
    }
    (SourceRead::ok(Value::Array(kept)), held_rows)
}

/// Split dispatch-held nodes out of the undispatched feed: an active
/// `dispatch_hold` (plan frontmatter or the node row's own field, walked
/// through parents and contained owners) parks the node the same way an open
/// question does, so it never renders as the next undispatched actionable
/// row; it is named under `held` with its hold reason. The verdict is the
/// same one `undriven_pr` reads, so one hold answers every queue alike. A
/// feed that never answered passes through untouched, like `partition_held`.
fn partition_dispatch_held(
    undispatched: SourceRead,
    entries: Option<&[Value]>,
) -> (SourceRead, Vec<Value>) {
    if !undispatched.is_ok() {
        return (undispatched, Vec::new());
    }
    let by_id: std::collections::BTreeMap<String, Value> = entries
        .unwrap_or(&[])
        .iter()
        .filter_map(|e| {
            e.get("id")
                .and_then(Value::as_str)
                .map(|id| (id.to_string(), e.clone()))
        })
        .collect();
    let (mut kept, mut held_rows) = (Vec::new(), Vec::new());
    for mut row in undispatched.rows() {
        let hold_entry = row
            .get("id")
            .and_then(Value::as_str)
            .and_then(|id| by_id.get(id))
            .unwrap_or(&row);
        let receipt = crate::backlog_ready::hold_verdict_receipt(hold_entry, &by_id);
        if receipt.as_ref().is_some_and(|v| v.held) {
            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "hold_reason".to_string(),
                    json!(receipt
                        .as_ref()
                        .map(|v| v.reason.as_str())
                        .unwrap_or_default()),
                );
                obj.insert("held_by".to_string(), json!("dispatch_hold"));
            }
            held_rows.push(row);
        } else {
            kept.push(row);
        }
    }
    (SourceRead::ok(Value::Array(kept)), held_rows)
}

// ---------------------------------------------------------------------------
// SourceRead: one source's answer, or the reason there is no answer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub(crate) struct SourceRead {
    pub(crate) payload: Option<Value>,
    pub(crate) error: Option<String>,
    /// True when the read died at its budget slice, not at a source failure.
    pub(crate) over_budget: bool,
}

impl SourceRead {
    pub(crate) fn ok(payload: Value) -> Self {
        SourceRead {
            payload: Some(payload),
            error: None,
            over_budget: false,
        }
    }
    pub(crate) fn err(msg: impl Into<String>) -> Self {
        SourceRead {
            payload: None,
            error: Some(msg.into()),
            over_budget: false,
        }
    }
    pub(crate) fn over_budget(msg: impl Into<String>) -> Self {
        SourceRead {
            payload: None,
            error: Some(msg.into()),
            over_budget: true,
        }
    }
    /// Re-wrap a failed read under a new message, keeping its verdict: a
    /// composition site must not flatten a budget kill into a plain failure.
    pub(crate) fn rewrap(&self, msg: impl Into<String>) -> Self {
        SourceRead {
            payload: None,
            error: Some(msg.into()),
            over_budget: self.over_budget,
        }
    }
    pub(crate) fn is_ok(&self) -> bool {
        self.error.is_none()
    }
    /// No evidence either way: the source answered, or the board chose not
    /// to pay for the read. A fold whose failing inputs all read this saw
    /// nothing because the board stopped looking, never because a source
    /// failed, and must not render as unreadable.
    pub(crate) fn not_evidence(&self) -> bool {
        self.is_ok() || self.over_budget
    }
    pub(crate) fn rows(&self) -> Vec<Value> {
        match &self.payload {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        }
    }
    /// The nested-shape half of the degrade-not-crash promise (board.py
    /// `_as_dict`): a stream that changed shape degrades that stream, never
    /// the whole board.
    pub(crate) fn dict(&self) -> Map<String, Value> {
        match &self.payload {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        }
    }
}

pub(crate) fn s_str<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

pub(crate) fn s_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

/// node -> the node claim's acquired_at (epoch ms) for the distress
/// staleness compare. A row without a `node:`-prefixed key, or whose
/// acquired_at is absent or non-numeric, contributes nothing.
pub(crate) fn claim_acquired_at_by_node(claim_rows: &[Value]) -> HashMap<String, i64> {
    claim_rows
        .iter()
        .filter_map(|row| {
            let node = s_str(row, "key")?.strip_prefix("node:")?;
            Some((node.to_string(), s_i64(row, "acquired_at")?))
        })
        .collect()
}

/// Python `bool()` over a JSON value: null/false/empty-string/zero/empty
/// container are false. The classify port decides `completed`/`has_pr`/
/// `batch_owner` the way the Python `bool(entry.get(...))` did, so a legacy
/// empty-string `completed_at` or a zero `pr_number` reads as absent, exactly
/// as the retired module read it.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(st) => !st.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

pub(crate) fn as_int(v: &Value) -> i64 {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Collection
// ---------------------------------------------------------------------------

/// The `fno-agents board` entry point: parse flags, print the payload, exit
/// with the board's own exit_code. Kept beside the parity sets' reach (an
/// `==` arm in client.rs), so no advertised command is added.
///
/// `--user-command` is `fno inbox board`: with no `--state` the caller's own
/// role manifest bounds the board, and the payload prints as the text page
/// (`--max-rows` per queue) or, with `--json`, indented.
pub fn run_board(args: &[String]) -> i32 {
    let mut opts = BoardOpts::default();
    let mut user_command = false;
    let mut as_json = false;
    let mut max_rows = crate::lead_reads::DEFAULT_BOARD_ROWS;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" | "-J" => as_json = true,
            "--user-command" => user_command = true,
            "--max-rows" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(v) => max_rows = v,
                None => {
                    eprintln!("fno-agents board: --max-rows needs an integer");
                    return 2;
                }
            },
            "--budget-ms" => match it.next().and_then(|v| v.parse::<u64>().ok()) {
                Some(v) => opts.budget_ms = v,
                None => {
                    eprintln!("fno-agents board: --budget-ms needs a millisecond integer");
                    return 2;
                }
            },
            "--max-pr-reads" => match it.next().and_then(|v| v.parse::<usize>().ok()) {
                Some(v) => opts.max_pr_reads = v,
                None => {
                    eprintln!("fno-agents board: --max-pr-reads needs an integer");
                    return 2;
                }
            },
            "--state" => match it.next() {
                Some(v) => opts.state_path = Some(PathBuf::from(v)),
                None => {
                    eprintln!("fno-agents board: --state needs a path");
                    return 2;
                }
            },
            other => {
                eprintln!("fno-agents board: unknown flag {other}");
                return 2;
            }
        }
    }
    if user_command && opts.state_path.is_none() {
        opts.state_path = crate::lead_reads::board_default_state();
    }
    let payload = read_board(&opts);
    if !user_command {
        println!(
            "{}",
            serde_json::to_string(&payload).unwrap_or_else(|_| "{}".into())
        );
    } else if as_json {
        println!("{}", crate::backlog::render::py_json_pretty(&payload));
    } else {
        let (out, err) = crate::lead_reads::render_board(&payload, max_rows);
        print!("{out}");
        eprint!("{err}");
    }
    payload
        .get("exit_code")
        .and_then(Value::as_i64)
        .map(|c| c as i32)
        .unwrap_or(1)
}

pub struct BoardOpts {
    pub budget_ms: u64,
    pub max_pr_reads: usize,
    /// Lead manifest whose `scope` bounds the board.
    pub state_path: Option<PathBuf>,
    /// The directory every config-tier, claims-root, and project-journal
    /// resolution anchors on. `None` means the process cwd (the hand-run CLI
    /// case); an IN-PROCESS caller such as loopcheck passes its `--cwd` here,
    /// because the old subprocess board ran with the lead session's cwd and
    /// the calling process's cwd is not guaranteed to be the same directory.
    pub cwd: Option<PathBuf>,
    /// A registry the caller already loaded this fire (the stop gate's
    /// session-binding read). The driver feed reuses it instead of paying a
    /// second store read inside the board's budgeted slice; None reads the
    /// store as before.
    pub registry: Option<crate::state::Registry>,
}

impl Default for BoardOpts {
    fn default() -> Self {
        BoardOpts {
            budget_ms: HAND_RUN_BUDGET_MS,
            max_pr_reads: DEFAULT_MAX_PR_READS,
            state_path: None,
            cwd: None,
            registry: None,
        }
    }
}

/// Read the whole board. Never panics on a source; every failure lands in its
/// queue's error and the payload still answers.
pub fn read_board(opts: &BoardOpts) -> Value {
    let cwd = opts
        .cwd
        .clone()
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let mut budget = Budget::new(opts.budget_ms);
    let mut sources: Map<String, Value> = Map::new();
    let mut warnings: Vec<String> = Vec::new();

    let mark =
        |sources: &mut Map<String, Value>, name: &str, read: &SourceRead, truncated: bool| {
            sources.insert(
                name.to_string(),
                json!({
                    "ok": read.is_ok(),
                    "truncated": truncated,
                    "error": read.error.clone().unwrap_or_default(),
                }),
            );
        };

    // The graph: ONE read, shared by undispatched, claimed-node lookups, PR
    // binding, and team scope. It asks the store (`backlog::api::rows`),
    // never the file: the store applies the same defaults the Python board's
    // `read_graph_strict` runs, and a read failure surfaces as a warning the
    // same way an unreadable file did. It is also the one IN-PROCESS source,
    // so it takes its slice check up front like every subprocess source: with
    // no time left it is marked unreadable and skipped, never run unbudgeted
    // (the collector measured 40,776ms against a 30,000ms
    // budget, and an unbounded in-process read cannot honor a deadline).
    let graph_path = graph_json_path(&cwd);
    let store = crate::backlog::api::Store::new(&graph_path);
    let mut graph_error = None;
    let entries: Option<Vec<Value>> = match budget.source_deadline() {
        None => {
            let error = "board budget exhausted".to_string();
            warnings.push(format!("graph not read: {error}"));
            graph_error = Some(error);
            None
        }
        Some(_) => match crate::graph_store::read_rows_where(
            &store.graph,
            &crate::backlog::RowQuery {
                fields: Some(
                    crate::graph_store::SLIM_FIELDS
                        .iter()
                        .copied()
                        .chain([
                            "size",
                            "model",
                            "dispatch_command",
                            "dispatch_brief",
                            "mission_id",
                            "mission_wave",
                            "mission_slug",
                            "mission_from_msg_id",
                            "roadmap_id",
                            "sessions",
                            "additional_prs",
                            "dispatch_hold",
                            "contained_in",
                            "queued_at",
                            "queued_reason",
                            "deferred_at",
                            "superseded_by",
                            "supersession",
                            "encounters",
                            "orphan_ok",
                            "batch",
                            "tags",
                            "blocked_reason",
                            "children_total",
                            "children_done",
                            "discovery",
                            "pr_url",
                            "cost_sessions",
                        ])
                        .map(str::to_string)
                        .collect(),
                ),
                with_blockers: true,
                ..Default::default()
            },
        )
        .map_err(|error| crate::backlog::api::ApiError(error.to_string()))
        {
            Ok(e) => Some(e),
            Err(e) => {
                let error = format!("graph unreadable: {}", e.0);
                warnings.push(error.clone());
                graph_error = Some(error);
                None
            }
        },
    };

    let spent = |sources: &mut Map<String, Value>, name: &str, budget: &Budget| {
        let err = budget.spent_error();
        sources.insert(
            name.to_string(),
            json!({"ok": false, "truncated": true, "error": err}),
        );
    };

    // The reference charged slices sequentially; this collector charges them
    // in the same order but runs the four subprocess reads CONCURRENTLY, for
    // the reason the Python board ran its six sources on a thread pool:
    // sequential execution lets the slowest source (gh pr list under fleet
    // load, measured 28s here) starve every queue behind it. The deadline is
    // still the ONE bound: every slice derives from the same total, and the
    // slowest source now bounds the wall, not the sum.
    let s_undispatched = budget.start("backlog undispatched");
    let s_claims = budget.start("agents claim list");
    let s_prs = budget.start(SRC_PRS);
    let s_pr_gate = budget.start(SRC_PR_GATE);
    let s_stalled = budget.start("stalled_holder lookups");
    let s_ready = budget.start(SRC_READY);
    let s_worked = budget.start(SRC_WORKED);
    let s_outstanding = budget.start(SRC_QUESTIONS);
    let s_needs = budget.start(SRC_NEEDS);
    let s_truth = budget.start("truth probe");
    let s_blocked_child = budget.start(SRC_DISTRESS);
    let s_stranded = budget.start(SRC_STRANDED);

    // Mostly in-process: graph already read; claims scan, claimed-node lookups,
    // the needs fold, and the lane file.
    let claims = match s_claims {
        None => {
            spent(&mut sources, "claims", &budget);
            budget.spent_read()
        }
        Some(_) => {
            let read = read_claims(&cwd);
            mark(&mut sources, "claims", &read, false);
            read
        }
    };
    // Undispatched keeps its slice claim above. Its former subprocess moved
    // into the concurrent section to avoid blocking later sources under fleet
    // load; the native fold now runs there over inputs already read here.

    // Claimed nodes: from the locks to the rows, one graph read.
    let (claimed_nodes, mut holders, claimed_warnings) = match s_stalled {
        None => {
            spent(&mut sources, "claimed_nodes", &budget);
            (budget.spent_read(), Vec::new(), Vec::new())
        }
        Some(_) => {
            let (read, mut holders, w) = read_claimed_nodes(&claims, entries.as_deref());
            // The probe feed must also measure DEAD-stated claim holders:
            // the reordered readers ask the holder before honoring the clock,
            // and a holder nobody probed always reads inactive.
            for h in classify::dead_claim_holders(&claims) {
                if !holders.contains(&h) {
                    holders.push(h);
                }
            }
            mark(&mut sources, "claimed_nodes", &read, false);
            (read, holders, w)
        }
    };
    warnings.extend(claimed_warnings);

    // The driver feed: registry rows that target a node, read
    // in-place (missing file = empty roster; corrupt = loud, and every
    // node_driver queue then reads unreadable rather than silently clean).
    // Its tokens join the ONE batched truth probe, so the roster's drivers
    // get the same transcript measurement the claim holders get.
    let s_drivers = budget.start(SRC_DRIVERS);
    let (drivers, roster_tokens, driver_name_tokens) = match s_drivers {
        None => (budget.spent_read(), Vec::new(), HashMap::new()),
        Some(_) => {
            let (mut read, name_to_token) =
                read_driver_rows(entries.as_deref(), opts.registry.as_ref());
            let mut tokens: Vec<String> = Vec::new();
            if let Some(rows) = read.payload.as_mut().and_then(Value::as_array_mut) {
                // PR-bound non-terminal nodes ride first: the cap spends the
                // probe on rows a queue can act on. The tail keeps its rows in
                // the payload, so its nodes still read unmeasured (never none);
                // only their probe tokens are withheld.
                sort_driver_rows_pr_first(rows, entries.as_deref());
                if rows.len() > MAX_DRIVER_ROWS {
                    warnings.push(format!(
                        "drivers: capped at {MAX_DRIVER_ROWS} of {} node-stamped rows; the unprobed tail reads unmeasured",
                        rows.len()
                    ));
                }
                for row in rows.iter().take(MAX_DRIVER_ROWS) {
                    if let Some(t) = row.get("token").and_then(Value::as_str) {
                        tokens.push(t.to_string());
                    }
                }
            }
            mark(&mut sources, "drivers", &read, false);
            (read, tokens, name_to_token)
        }
    };
    for t in roster_tokens {
        if !holders.contains(&t) {
            holders.push(t);
        }
    }

    // Blocked child and stranded tree ride the SAME concurrent thread pool
    // as worked/prs/ready/outstanding/needs below (t_blocked_child,
    // t_stranded), not a sequential step ahead of it: an earlier version of
    // this fix ran them sequentially before that pool, which stopped them
    // starving but meant their own wall time (the distress journal read, a
    // `git worktree list` + per-tree probe per repo) now sat on the
    // critical path ahead of every other source, so a slow repo could just
    // as easily starve worked/prs/ready/outstanding/needs instead (P1,
    // reviewed). True concurrency - every source bounded by the one shared
    // deadline, none of them ordered ahead of or behind the others - is
    // what actually fixes it rather than reassigning it.

    // The reads that can take real wall time run concurrently: gh pr
    // list, `fno inbox outstanding`, the batched truth
    // probe, the ready selection (in-process; its plan-document disk reads
    // still cost the slice), and the needs fold (in-process, but its refused-worker leg batch
    // probes the whole registry and measured ~7s on a busy fleet). Their
    // slices were derived above in the reference's order.
    let entries_ref = entries.as_deref();
    let cwd_for_threads = cwd.clone();
    // The spent receipt is captured BEFORE the threads spawn: a source whose
    // deadline is already past at its own spawn marks itself spent with the
    // same wording the join-side gates use, and never spawns a child it
    // would kill at once.
    let spent_err = budget.spent_error();
    // Held nodes: ONE fold over the question journals, computed once
    // and read by the ready partition below. needs::held_map is the
    // fail-open door (an unreadable journal is an empty map, the same
    // posture the question scans elsewhere take).
    let held_map = if budget.source_deadline().is_none() {
        Default::default()
    } else {
        crate::needs::held_map(&home_dot_fno(), &cwd)
    };
    let (
        undispatched,
        prs,
        pr_nodes,
        pr_gates,
        pr_warnings,
        prs_truncated,
        worked,
        ready,
        outstanding,
        needs,
        holder_activity,
        holder_activity_error,
        holder_starved,
        blocked_child,
        stranded,
    ) = std::thread::scope(|s| {
        // The worked read answers in-process now: the authority is native
        // (backlog::worked), and the Python leg it used to shell out to is a
        // refusing tombstone. It still rides the concurrent section:
        // the roster fold is real work, and only the ready thread (its one
        // consumer) waits for the result.
        let t_worked = s_worked.map(|dl| {
            let spent_err = spent_err.clone();
            let undispatched_spent_err = spent_err.clone();
            let undispatched_dl = s_undispatched;
            let entries = entries_ref;
            let claims = &claims;
            s.spawn(move || {
                let bound = Budget::spawn_bound(dl);
                if bound.is_zero() {
                    let not_read = SourceRead::over_budget(spent_err);
                    return (not_read.clone(), not_read);
                }
                // The fold is in-process, so the subprocess read's kill at
                // the slice becomes a race: the fold runs on its own thread
                // and this one abandons it at the bound rather than joining
                // forever. The abandoned thread holds only read locks and
                // its result is dropped when it lands late.
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let _ = tx.send(crate::backlog::worked::json_rows());
                });
                let worked = match rx.recv_timeout(bound) {
                    Ok(Ok(rows)) => SourceRead::ok(Value::Array(rows)),
                    Ok(Err(reason)) => SourceRead::err(format!("worked: {reason}")),
                    Err(_) => SourceRead::err(format!(
                        "worked: killed at its {:.1}s slice of the board budget; the source did not fail",
                        bound.as_secs_f64()
                    )),
                };
                let undispatched = match undispatched_dl {
                    Some(dl) if !Budget::spawn_bound(dl).is_zero() => {
                        undispatched_from_board(entries, claims, &worked)
                    }
                    _ => SourceRead::over_budget(undispatched_spent_err),
                };
                (worked, undispatched)
            })
        });
        let t_prs = s_prs.map(|dl| {
            let cwd = cwd_for_threads.clone();
            let gate_dl = s_pr_gate;
            s.spawn(move || {
                let (prs, pr_nodes, mut w) = read_prs(&cwd, dl, opts.max_pr_reads, entries_ref);
                // The gate read rides the same thread: its input is the
                // listing's own narrowed candidates, so there is nothing to
                // overlap until the listing lands.
                let candidates: Vec<i64> = prs
                    .rows()
                    .iter()
                    .filter_map(|r| r.get("number").and_then(Value::as_i64))
                    .collect();
                let (pr_gates, gate_w) = read_pr_gates(&cwd, &candidates, gate_dl);
                w.extend(gate_w);
                let truncated = w.iter().any(|x| x.contains("hit its"));
                (prs, pr_nodes, w, truncated, pr_gates)
            })
        });
        let (mut worked, undispatched) = match t_worked {
            None => {
                let not_read = budget.spent_read();
                (not_read.clone(), not_read)
            }
            Some(h) => h.join().unwrap_or_else(|_| {
                let failed = SourceRead::err("worked: reader panicked");
                (
                    failed.clone(),
                    SourceRead::err("undispatched: reader panicked"),
                )
            }),
        };
        mark(&mut sources, "worked", &worked, false);
        let worked_ids = worked_node_ids(&worked);
        // The worked feed nominates candidates: map each listed row's worker
        // labels to registry probe tokens before the batch spawns, so the
        // probe, not the listing, answers whether the named worker is alive.
        let worked_probe_tokens = enrich_worked_rows(&mut worked, &driver_name_tokens);
        for t in worked_probe_tokens {
            if !holders.contains(&t) {
                holders.push(t);
            }
        }
        let t_ready = s_ready.map(|dl| {
            let entries = entries_ref.map(|e| e.to_vec());
            let cwd = cwd_for_threads.clone();
            let worked_ids = worked_ids.clone();
            let spent_err = spent_err.clone();
            s.spawn(move || {
                // In-process now: the admission decision lives in this
                // binary (backlog_ready::select) and reads the graph the
                // board already loaded, so the source costs plan-document
                // disk reads, not an interpreter cold start. The budget
                // deadline still gates the START: a thread scheduled after
                // the deadline marks itself spent instead of reading.
                if Budget::spawn_bound(dl).is_zero() {
                    return SourceRead::over_budget(spent_err);
                }
                let Some(entries) = entries else {
                    return SourceRead::err("graph unreadable: no entries for ready selection");
                };
                // Strict claims read: an unreadable root is unknown claim
                // state, surfaced as a source error rather than an empty
                // set that would re-dispatch held work.
                let claimed_records = crate::claims::list_strict(Some("node:"), None, false);
                let Ok(claim_records) = claimed_records else {
                    return SourceRead::err(format!(
                        "claims unreadable: {}",
                        claimed_records.err().unwrap_or_default()
                    ));
                };
                let mut claimed = std::collections::BTreeSet::new();
                for rec in claim_records {
                    if let Some(id) = rec.key.strip_prefix("node:") {
                        claimed.insert(id.to_string());
                    }
                }
                claimed.extend(worked_ids);
                let opts = crate::backlog_ready::ReadyOpts {
                    all: true,
                    repo_root: crate::paths::canonical_repo_root(&cwd)
                        .map(|p| p.display().to_string()),
                    staleness_days: crate::backlog_ready::configured_staleness_days(
                        &cwd.join(".fno"),
                    ),
                    claimed,
                    now_ms: crate::claims::now_ms(),
                    ..Default::default()
                };
                match crate::backlog_ready::select(&entries, &opts) {
                    Ok(reply) => SourceRead::ok(Value::Array(reply.rows)),
                    Err(_) => SourceRead::err("ready: parent scope did not resolve"),
                }
            })
        });
        let t_outstanding = s_outstanding.map(|dl| {
            let cwd = cwd_for_threads.clone();
            let spent_err = spent_err.clone();
            s.spawn(move || {
                let bound = Budget::spawn_bound(dl);
                if bound.is_zero() {
                    return SourceRead::over_budget(spent_err);
                }
                let mut cmd = fno_py_cmd();
                cmd.extend(
                    ["inbox", "outstanding", "--json"]
                        .iter()
                        .map(|s| s.to_string()),
                );
                run_json(cmd, &cwd, bound)
            })
        });
        // ONE batched truth probe for every holder the lead cares about (the
        // single-transcript-reader constraint; a probe per holder would pay
        // one interpreter cold start each). It rides the board deadline like
        // every other source: its own 20s-to-60s page bound is capped by what
        // remains, and a page reached after the deadline is unmeasured, never
        // no-evidence (the probe measured ~7s of overrun into every board).
        // The feed is also bounded per handle by the arithmetic the batch
        // prices with: the slice funds a measured PREFIX - claim-derived
        // holders ride first, and they are the set the queue warnings count -
        // while the withheld tail reads unmeasured by name instead of dying
        // as one empty page (measured 2026-09-28: a 33-holder feed timed out
        // whole, three beats in a row).
        let t_truth = match (holders.is_empty(), s_truth) {
            (true, _) | (_, None) => None,
            (false, Some(dl)) => {
                let tokens: Vec<String> = holders
                    .iter()
                    .map(|h| {
                        h.split_once(':')
                            .map(|(_, t)| t.to_string())
                            .unwrap_or_else(|| h.clone())
                    })
                    .collect();
                Some(s.spawn(move || {
                    crate::truth_probe::family1_truth_probe_many_measured_within(&tokens, Some(dl))
                }))
            }
        };
        // The needs fold rides a thread too: in-process, but its
        // refused-worker leg batch probes the whole registry and measured
        // ~7s on a busy fleet, which no longer sits on the critical path.
        let t_needs = s_needs.map(|dl| {
            let cwd = cwd_for_threads.clone();
            let spent_err = spent_err.clone();
            s.spawn(move || {
                // In-process, but the deadline still gates the START: the
                // refused-worker leg batch-probes the whole registry and
                // measured ~7s on a busy fleet.
                if Budget::spawn_bound(dl).is_zero() {
                    return SourceRead::over_budget(spent_err);
                }
                let home = crate::paths::AgentsHome::from_env();
                let (mut event_paths, default_ledger) = default_needs_sources(&home);
                // The canonical checkout's journal, exactly as `run_needs`
                // adds it: a question asked from a worktree writes the
                // CANONICAL .fno/events.jsonl, never the worktree's. The
                // project journal anchors on the BOARD's cwd (the caller's
                // --cwd for an in-process reader), never the process cwd.
                event_paths[0] = cwd.join(".fno").join("events.jsonl");
                if let Some(root) = crate::paths::canonical_repo_root(&cwd) {
                    let canonical_events = root.join(".fno").join("events.jsonl");
                    let cwd_events = cwd.join(".fno").join("events.jsonl");
                    if canonical_events != cwd_events && !event_paths.contains(&canonical_events) {
                        event_paths.push(canonical_events);
                    }
                }
                let since = now_secs_board().saturating_sub(crate::needs::DEFAULT_WINDOW_SECS);
                let items = crate::needs::collect_needs_items(
                    &home,
                    &event_paths,
                    &default_ledger,
                    since,
                    crate::needs::DEFAULT_FIRES_FLOOR,
                    &cwd,
                );
                SourceRead::ok(serde_json::to_value(&items).unwrap_or(json!([])))
            })
        });

        // Blocked child: read the global distress journal and resolve every
        // candidate's answered/unanswered state HERE, where claims, the
        // graph, and the verdict subprocess already live - build_board only
        // scope-filters and renders what this collects. No catch_unwind here:
        // `AgentsHome::from_env()` panicking under a test process with no
        // declared hermetic root (paths.rs) used to need one because this ran
        // inline on the main thread; now it runs on its own spawned thread,
        // and the join below already converts a panic there into
        // "blocked_child: reader panicked" - a second layer of the same
        // protection would be dead weight, not defense in depth.
        let t_blocked_child = s_blocked_child.map(|dl| {
            let claims = &claims;
            let entries = entries_ref;
            let cwd = cwd_for_threads.clone();
            let spent_err = spent_err.clone();
            s.spawn(move || {
                if Budget::spawn_bound(dl).is_zero() {
                    return SourceRead::over_budget(spent_err);
                }
                let home = crate::paths::AgentsHome::from_env();
                let journal = crate::daemon::global_events_path(&home);
                match read_blocked_rows(&journal) {
                    Err(e) => SourceRead::err(e),
                    Ok(rows) => {
                        let claim_rows = claims.rows();
                        let claim_state_by_node: HashMap<String, String> = claim_rows
                            .iter()
                            .filter_map(|row| {
                                let node = s_str(row, "key")?.strip_prefix("node:")?;
                                Some((
                                    node.to_string(),
                                    s_str(row, "state").unwrap_or("").to_string(),
                                ))
                            })
                            .collect();
                        let status_by_node: HashMap<String, String> = entries
                            .unwrap_or(&[])
                            .iter()
                            .filter_map(|n| {
                                Some((
                                    s_str(n, "id")?.to_string(),
                                    s_str(n, "status").unwrap_or("").to_string(),
                                ))
                            })
                            .collect();
                        let candidates = queues::resolve_blocked_child_candidates(
                            rows,
                            &claim_state_by_node,
                            &claim_acquired_at_by_node(&claim_rows),
                            &status_by_node,
                            blocked_child_grace_minutes(&cwd),
                            now_secs_board() as i64,
                        );
                        if candidates.is_empty() {
                            SourceRead::ok(Value::Array(Vec::new()))
                        } else {
                            // A row keyed by its run id is never bus-addressed;
                            // the node's live claim names the harness session
                            // the bus does address.
                            let claim_session_by_node: HashMap<String, String> = claim_rows
                                .iter()
                                .filter_map(|row| {
                                    let node = s_str(row, "key")?.strip_prefix("node:")?;
                                    Some((node.to_string(), s_str(row, "session_id")?.to_string()))
                                })
                                .collect();
                            let mail_candidates: Vec<(String, Option<String>, String)> = candidates
                                .iter()
                                .map(|(row, _)| {
                                    let holder = row
                                        .node
                                        .as_deref()
                                        .and_then(|n| claim_session_by_node.get(n))
                                        .cloned();
                                    (row.session.clone(), holder, row.ts.clone())
                                })
                                .collect();
                            let answered =
                                queues::mail_answered_since(&bus_live_log_path(), &mail_candidates);

                            let mut sessions: Vec<String> = Vec::new();
                            for (session, holder, _) in &mail_candidates {
                                for key in [Some(session.as_str()), holder.as_deref()]
                                    .into_iter()
                                    .flatten()
                                {
                                    if !sessions.iter().any(|s| s == key) {
                                        sessions.push(key.to_string());
                                    }
                                }
                            }
                            let mut cmd = fno_py_cmd();
                            cmd.extend([
                                "agents".to_string(),
                                "distress-verdicts".to_string(),
                                "--sessions".to_string(),
                                serde_json::to_string(&sessions)
                                    .unwrap_or_else(|_| "[]".to_string()),
                            ]);
                            let verdict_bound = Budget::spawn_bound(dl);
                            let verdict_payload = if verdict_bound.is_zero() {
                                SourceRead::err(spent_err.clone())
                            } else {
                                run_json(cmd, &cwd, verdict_bound)
                            };
                            let verdicts: HashMap<String, String> = mail_candidates
                                .iter()
                                .filter_map(|(session, holder, _)| {
                                    queues::verdict_for(
                                        verdict_payload.payload.as_ref(),
                                        session,
                                        holder.as_deref(),
                                    )
                                    .map(|v| (session.clone(), v))
                                })
                                .collect();
                            SourceRead::ok(Value::Array(queues::filter_unanswered_by_mail(
                                candidates, &answered, &verdicts,
                            )))
                        }
                    }
                }
            })
        });

        // The stranded-tree read: which linked worktrees hold uncommitted or
        // unpushed work under an open lead-priority node. Candidates come
        // from the one graph read; repos from the workspace map plus this
        // repo's canonical root, so a board run anywhere lands the same
        // tree set.
        let t_stranded = s_stranded.map(|dl| {
            let entries = entries_ref;
            let cwd = cwd_for_threads.clone();
            let spent_err = spent_err.clone();
            s.spawn(move || {
                if Budget::spawn_bound(dl).is_zero() {
                    return SourceRead::over_budget(spent_err);
                }
                let candidates = entries.map(stranded_candidates).unwrap_or_default();
                let repos = scope::project_repo_paths(&cwd);
                read_stranded_trees(&repos, &candidates, dl, &spent_err)
            })
        });

        let (prs, pr_nodes, pr_warnings, prs_truncated, pr_gates) = match t_prs {
            None => {
                let err = budget.spent_error();
                (
                    SourceRead::over_budget(err.clone()),
                    SourceRead::over_budget(err.clone()),
                    Vec::new(),
                    true,
                    SourceRead::over_budget(err),
                )
            }
            Some(h) => h.join().unwrap_or_else(|_| {
                (
                    SourceRead::err("prs: reader panicked"),
                    SourceRead::err("undriven_pr: reader panicked"),
                    Vec::new(),
                    false,
                    SourceRead::err("merge gate: reader panicked"),
                )
            }),
        };
        mark(&mut sources, "pr_gate", &pr_gates, false);
        let ready = match t_ready {
            None => budget.spent_read(),
            Some(h) => h
                .join()
                .unwrap_or(SourceRead::err("ready: reader panicked")),
        };
        let outstanding = match t_outstanding {
            None => budget.spent_read(),
            Some(h) => h
                .join()
                .unwrap_or(SourceRead::err("outstanding: reader panicked")),
        };
        let outstanding = if outstanding.is_ok() {
            SourceRead::ok(outstanding.payload.unwrap_or(json!({})))
        } else {
            outstanding
        };
        let needs = match t_needs {
            None => budget.spent_read(),
            // A panicked fold must read UNREADABLE, never ok-empty: an
            // empty needs stream and a stream that never answered are
            // different boards, and the queue's loudness is the only
            // place the difference survives.
            Some(h) => h
                .join()
                .unwrap_or_else(|_| SourceRead::err("needs: reader panicked")),
        };
        let blocked_child = match t_blocked_child {
            None => budget.spent_read(),
            Some(h) => h
                .join()
                .unwrap_or_else(|_| SourceRead::err("blocked_child: reader panicked")),
        };
        mark(&mut sources, "blocked_child", &blocked_child, false);
        let stranded = match t_stranded {
            None => budget.spent_read(),
            Some(h) => h
                .join()
                .unwrap_or_else(|_| SourceRead::err("stranded: reader panicked")),
        };
        mark(&mut sources, "stranded", &stranded, false);
        let (holder_activity, holder_activity_error, holder_starved): (
            HashMap<String, crate::truth_probe::TruthProbe>,
            Option<String>,
            bool,
        ) = match t_truth {
            // No deadline for a non-empty holder set is a spent board: the
            // probe never ran, which reads not-read (a budget kill), never
            // ok-empty and never a source failure.
            None if holders.is_empty() => (HashMap::new(), None, false),
            None => (HashMap::new(), Some(spent_err.clone()), true),
            Some(h) => match h.join() {
                Ok((map, crate::truth_probe::BatchOutcome::Measured)) => (map, None, false),
                Ok((map, crate::truth_probe::BatchOutcome::NotMeasured)) if !map.is_empty() => {
                    (map, None, false)
                }
                // A batch that outlived its board-derived bound is a budget
                // kill, not a source failure: the queues fold it over_budget,
                // a quiet board stays certifiable, and the receipt names the
                // read for the next wake to re-pay. A panicked reader stays
                // UNREADABLE (the branch below). A non-empty partial page
                // remains usable, and queues name its missing holders.
                Ok((_, crate::truth_probe::BatchOutcome::NotMeasured)) => (
                    HashMap::new(),
                    Some(format!(
                        "truth probe: batch of {} handles timed out",
                        holders.len()
                    )),
                    true,
                ),
                Err(_) => (
                    HashMap::new(),
                    Some("truth probe: reader panicked".to_string()),
                    false,
                ),
            },
        };
        mark(&mut sources, "undispatched", &undispatched, false);
        let undispatched = match undispatched
            .payload
            .as_ref()
            .and_then(|r| r.get("rows").and_then(Value::as_array).cloned())
        {
            Some(rows) => SourceRead::ok(Value::Array(rows)),
            None => undispatched,
        };
        // An open question whose blocks names the node holds it out of ready
        //; the held rows land under a `held` source beside `ready`.
        let (ready, mut held_rows) = partition_held(ready, &held_map);
        // A ruling dispatch_hold parks a node the same way: it leaves the
        // undispatched queue (never again the next actionable row) and is
        // named under held with its hold reason. The split runs after the
        // shape re-wrap above, on the canonical array payload rows() reads.
        let (undispatched, dispatch_held_rows) =
            partition_dispatch_held(undispatched, entries.as_deref());
        held_rows.extend(dispatch_held_rows);
        if !held_rows.is_empty() {
            sources.insert(
                "held".to_string(),
                json!({
                    "ok": true,
                    "truncated": false,
                    "error": "",
                    "count": held_rows.len(),
                    "rows": held_rows,
                }),
            );
        }
        (
            undispatched,
            prs,
            pr_nodes,
            pr_gates,
            pr_warnings,
            prs_truncated,
            worked,
            ready,
            outstanding,
            needs,
            holder_activity,
            holder_activity_error,
            holder_starved,
            blocked_child,
            stranded,
        )
    });
    warnings.extend(pr_warnings);
    let prs_truncated = prs_truncated || !prs.is_ok();
    mark(&mut sources, "prs", &prs, prs_truncated);
    mark(&mut sources, "ready", &ready, false);
    mark(&mut sources, "outstanding", &outstanding, false);
    sources.insert(
        "holder_activity".to_string(),
        json!({
            "ok": holder_activity_error.is_none(),
            "truncated": false,
            "error": holder_activity_error.clone().unwrap_or_default(),
        }),
    );

    // Lane: a file read; there is no command behind it.
    let lane_path = operator_lane_path(&cwd);
    let lane = match parse_lane(&lane_path) {
        Err(e) => SourceRead::err(e),
        Ok(items) => SourceRead::ok(Value::Array(
            items
                .iter()
                .map(|i| {
                    json!({
                        "text": i.text,
                        "node": i.node,
                        "parked": i.parked,
                        "done": i.done,
                        "line": i.line,
                    })
                })
                .collect(),
        )),
    };
    mark(&mut sources, "lane", &lane, false);

    // Scope.
    let mut scope_ids: Option<HashSet<String>> = None;
    let mut role_scope: Option<String> = None;
    let mut foreign_owned = SourceRead::ok(Value::Array(Vec::new()));
    if let Some(state_path) = &opts.state_path {
        let manifest = parse_manifest(state_path);
        let scope = manifest.get("scope").cloned().unwrap_or_default();
        if scope.is_empty() {
            return json!({
                "actionable": 1,
                "unreadable": 1,
                "queues": [queue_json(&Queue {
                    name: "scope",
                    source: state_path.display().to_string(),
                    status: "unreadable",
                    error: "lead manifest has no scope".to_string(),
                    count: -1,
                    rows: Vec::new(),
                    actionable: true,
                    note: String::new(),
                    command: "",
                })],
                "warnings": warnings,
                "exit_code": 1,
                "sources": Value::Object(sources),
            });
        }
        role_scope = Some(scope.clone());
        let projects = project_map(&cwd);
        let Some(entries) = entries.as_deref() else {
            return json!({
                "actionable": 1,
                "unreadable": 1,
                "queues": [queue_json(&Queue {
                    name: "scope",
                    source: format!("lead manifest scope {scope}"),
                    status: "unreadable",
                    error: format!(
                        "graph unreadable, so scope {scope} is unknown: {}",
                        graph_error.as_deref().unwrap_or("read failed")
                    ),
                    count: -1,
                    rows: Vec::new(),
                    actionable: true,
                    note: String::new(),
                    command: "",
                })],
                "warnings": warnings,
                "exit_code": 1,
                "sources": Value::Object(sources),
            });
        };
        match compile_scope_ids(&scope, entries, &projects) {
            Ok(ids) => {
                scope_ids = Some(ids);
                // One registry file and an in-memory compile: no subprocess to
                // bound, and a spent budget must not turn the unplanned queue
                // blind for a read that costs milliseconds.
                foreign_owned = foreign_owned_read(&scope, entries, &projects);
                mark(&mut sources, "owners", &foreign_owned, false);
            }
            Err(e) => {
                return json!({
                    "actionable": 1,
                    "unreadable": 1,
                    "queues": [queue_json(&Queue {
                        name: "scope",
                        source: format!("lead manifest scope {scope}"),
                        status: "unreadable",
                        error: e,
                        count: -1,
                        rows: Vec::new(),
                        actionable: true,
                        note: String::new(),
                        command: "",
                    })],
                    "warnings": warnings,
                    "exit_code": 1,
                    "sources": Value::Object(sources),
                });
            }
        }
    }

    // Mirrors `bus/log.py::bus_dir`'s env overrides only, not its
    // `config.paths.bus_dir` template override - a documented gap: an
    // operator who relocates the bus via config gets a mail-check that
    // never sees mail there, degrading to "still shows unanswered" rather
    // than a wrong answer.
    fn bus_live_log_path() -> PathBuf {
        if let Some(dir) = std::env::var("FNO_BUS_DIR").ok().filter(|v| !v.is_empty()) {
            return PathBuf::from(dir).join("messages.jsonl");
        }
        if let Some(root) = std::env::var("FNO_INBOX_ROOT")
            .ok()
            .filter(|v| !v.is_empty())
        {
            return PathBuf::from(root).join(".bus").join("messages.jsonl");
        }
        let home = crate::paths::AgentsHome::from_env();
        home.root()
            .parent()
            .unwrap_or_else(|| home.root())
            .join("bus")
            .join("messages.jsonl")
    }

    // The fleet-task read: a local questions.jsonl fold, sub-millisecond,
    // so it runs inline instead of taking a budget slice or a thread. An
    // unreadable store degrades the queue, never the board; catch_unwind
    // for the same reason held_nodes above wraps its fold - from_env
    // panics under a test process with no declared root.
    let tasks = std::panic::catch_unwind(|| {
        let home = crate::paths::AgentsHome::from_env();
        let store = crate::provider_cap::questions_path(&home);
        match crate::fleet_task::open_tasks(&store) {
            Ok(tasks) => SourceRead::ok(Value::Array(
                tasks
                    .iter()
                    .map(|t| {
                        json!({
                            "id": t.id,
                            "lane": t.lane,
                            "key": t.key,
                            "cwd": t.cwd,
                            "text": t.text,
                            "run": t.run,
                            "node": t.node,
                            "ts": t.ts,
                        })
                    })
                    .collect(),
            )),
            Err(e) => SourceRead::err(e),
        }
    })
    .unwrap_or_else(|_| SourceRead::err("tasks: reader panicked"));
    mark(&mut sources, "tasks", &tasks, false);

    let inputs = BoardInputs {
        ready,
        claims,
        worked,
        claimed_nodes,
        drivers,
        holder_activity,
        holder_activity_error,
        holder_activity_starved: holder_starved,
        prs,
        pr_nodes,
        pr_gates,
        outstanding,
        needs,
        tasks,
        lane,
        repo_root: cwd.to_string_lossy().into_owned(),
        undispatched,
        blocked_child,
        stranded,
        entries,
        held: held_map,
        warnings,
        autonomous_merge: autonomous_merge_enabled(&cwd),
        scope_ids,
        role_scope,
        foreign_owned,
    };
    let mut payload = build_board(&inputs);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("sources".to_string(), Value::Object(sources));
    }
    payload
}

/// The driver feed: registry rows that target a node. One in-place
/// read of the shared registry (`state::load_registry`); a missing file is an
/// empty roster (a store with no workers is a positive empty answer, not a
/// fault), a corrupt one is a failed read the consuming queues render loudly.
/// A row's node is its registry `node` stamp when it carries one, else the
/// graph join (`graph_session_nodes`) over the entries the board already
/// holds. Also returns a name-to-token map over EVERY registry entry,
/// whatever its status: the worked feed's worker labels are names, and an
/// orphaned row's transcript can still answer for the worker it names.
/// A caller that already loaded the registry this fire (the stop gate's
/// session-binding read) passes it in, so the board's budgeted slice pays
/// for no second store read.
fn read_driver_rows(
    entries: Option<&[Value]>,
    supplied: Option<&crate::state::Registry>,
) -> (SourceRead, HashMap<String, String>) {
    let fold = |registry: &crate::state::Registry| -> (SourceRead, HashMap<String, String>) {
        let session_nodes = graph_session_nodes(entries);
        let mut name_to_token: HashMap<String, String> = HashMap::new();
        for e in &registry.entries {
            name_to_token.entry(e.name.clone()).or_insert_with(|| {
                e.harness_session_id
                    .clone()
                    .unwrap_or_else(|| e.name.clone())
            });
        }
        let rows = registry
            .entries
            .iter()
            .filter(|e| is_live_driver_status(e.status))
            .filter_map(|e| {
                let node = e.node.as_deref().or_else(|| {
                    e.harness_session_id
                        .as_deref()
                        .and_then(|sid| session_nodes.get(sid))
                        .map(String::as_str)
                })?;
                let token = e
                    .harness_session_id
                    .clone()
                    .unwrap_or_else(|| e.name.clone());
                Some(json!({"name": e.name, "node": node, "token": token}))
            })
            .collect();
        (SourceRead::ok(Value::Array(rows)), name_to_token)
    };
    if let Some(registry) = supplied {
        return fold(registry);
    }
    let Some(home) = crate::paths::AgentsHome::from_env_opt() else {
        return (SourceRead::ok(json!([])), HashMap::new());
    };
    let path = home.registry_json();
    match crate::state::load_registry(&path) {
        Ok(registry) => fold(&registry),
        Err(e) => (
            SourceRead::err(format!("registry unreadable: {e}")),
            HashMap::new(),
        ),
    }
}

/// session id -> node id, joined from the graph entries the board already
/// holds. The registry `node` stamp is the fast path and it is mostly
/// absent: 6 of 31 rows carried it on 2026-09-17, so a join on it alone
/// leaves the documented primary driver signal blind for four rows in five.
///
/// An OPEN phase row means the session is working that node. A CLOSED one
/// means the session finished that PHASE, which is not the same claim as
/// "this PR has no driver": the measured specimen (2026-09-17) closed its do
/// row at 06:19Z and the same session drove PR 2126 for fourteen more hours.
/// So a closed row still names the node when the node carries a PR, and
/// never otherwise: a PR-less node whose planner closed its row must stay
/// dispatchable (the closed-planner ruling pinned by
/// cli/tests/unit/test_live_worked_nodes.py::test_xdead_a_closed_phase_row_skips_its_worker).
/// A ship row is a PR-link event, never occupancy, and the existing readers
/// skip it. The truth probe still decides Active vs Unmeasured downstream: a
/// recovered row that probes dead folds to nothing, so a genuinely parked PR
/// stays named.
fn graph_session_nodes(entries: Option<&[Value]>) -> HashMap<String, String> {
    let mut map: HashMap<String, String> = HashMap::new();
    let Some(entries) = entries else {
        return map;
    };
    // An OPEN phase row is positive occupancy and outranks a closed one: a
    // session that closed its do row on one node and opened one on another
    // must land on the open node, so open rows join first and the closed-row
    // inference only fills sessions no open row claims.
    let admissions = |want_open: bool, map: &mut HashMap<String, String>| {
        for node in entries {
            if TERMINAL_RUNGS.contains(&s_str(node, "status").unwrap_or("")) {
                continue;
            }
            let Some(node_id) = s_str(node, "id").map(str::to_string) else {
                continue;
            };
            let pr_bound = node_has_pr(node);
            for row in node
                .get("sessions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(phase) = row.get("phase").and_then(Value::as_str) else {
                    continue;
                };
                if phase == "ship" {
                    continue;
                }
                let Some(session_id) = row
                    .get("session_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
                else {
                    continue;
                };
                let open = crate::graph_store::is_open_phase_row(row, phase);
                if open == want_open && (open || pr_bound) {
                    map.entry(session_id.to_string())
                        .or_insert_with(|| node_id.clone());
                }
            }
        }
    };
    admissions(true, &mut map);
    admissions(false, &mut map);
    map
}

/// A registry row counts as a driver candidate only while its status sits in
/// the LIVE vocabulary (spawning/ready/idle/busy/live/restarting; the same
/// set spawn_gate.LIVE_STATUSES pins Python-side). A row whose child exited,
/// failed, or died is a CLOSED run: its transcript may still answer for a
/// while, and feeding it to the roster would let a finished worker suppress
/// its node's undriven-PR row - the same snapshot-vs-process fold this feed
/// exists to retire. The truth probe still decides Active vs Unmeasured
/// inside the live set; this filter only removes positively closed runs.
fn is_live_driver_status(status: crate::AgentStatus) -> bool {
    matches!(
        status,
        crate::AgentStatus::Spawning
            | crate::AgentStatus::Ready
            | crate::AgentStatus::Idle
            | crate::AgentStatus::Busy
            | crate::AgentStatus::Live
            | crate::AgentStatus::Restarting
    )
}

/// Stable PR-first order over node-stamped roster rows: rows whose node is
/// non-terminal and bound to a PR ride first, so the driver cap spends the
/// probe on the rows a queue can act on. The rest keep registry order.
fn sort_driver_rows_pr_first(rows: &mut [Value], entries: Option<&[Value]>) {
    let is_pr_bound = |row: &Value| {
        s_str(row, "node")
            .and_then(|id| {
                entry_by_id(entries.unwrap_or(&[]), id).map(|node| {
                    !TERMINAL_RUNGS.contains(&s_str(node, "status").unwrap_or(""))
                        && node.get("superseded_by").is_none()
                        && node_has_pr(node)
                })
            })
            .unwrap_or(false)
    };
    // Ascending sort: the NOT-PR rows carry the true (larger) key so the
    // PR-bound rows ride first, registry order preserved on both sides.
    rows.sort_by_key(|row| !is_pr_bound(row));
}

/// The worked feed nominates candidates, never decides. Each listed row's
/// worker labels name registry workers; map each name to its probe token (a
/// label's worker name is its first whitespace token) and attach the tokens
/// to the row, so node_driver runs the shared candidate fold over them
/// instead of trusting the listing. A name the registry never confirmed gets
/// no token and flags the row `unmapped`, which reads unmeasured - never
/// active. Returns the tokens to add to the probe batch.
pub(crate) fn enrich_worked_rows(
    worked: &mut SourceRead,
    name_to_token: &HashMap<String, String>,
) -> Vec<String> {
    let Some(rows) = worked.payload.as_mut().and_then(Value::as_array_mut) else {
        return Vec::new();
    };
    let mut fresh: Vec<String> = Vec::new();
    for row in rows {
        let names: Vec<&str> = row
            .get("workers")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(Value::as_str)
                    .map(|label| label.split_whitespace().next().unwrap_or(""))
                    .filter(|name| !name.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let mut row_tokens: Vec<String> = Vec::new();
        let mut unmapped = false;
        for name in &names {
            match name_to_token.get(*name) {
                Some(token) => {
                    if !fresh.contains(token) {
                        fresh.push(token.clone());
                    }
                    if !row_tokens.contains(token) {
                        row_tokens.push(token.clone());
                    }
                }
                None => unmapped = true,
            }
        }
        row["tokens"] = json!(row_tokens);
        if unmapped {
            row["unmapped"] = json!(true);
        }
    }
    fresh
}

/// The needs command's default sources (needs.default_sources): project + global
/// events + questions, and the ledger.
pub(crate) fn default_needs_sources(home: &crate::paths::AgentsHome) -> (Vec<PathBuf>, PathBuf) {
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let global_events = fno_dir.join("events.jsonl");
    let questions = fno_dir.join("questions.jsonl");
    let project_events = PathBuf::from(".fno").join("events.jsonl");
    let ledger = fno_dir.join("ledger.json");
    (vec![project_events, global_events, questions], ledger)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_read(payload: Value) -> SourceRead {
        SourceRead::ok(payload)
    }

    #[test]
    fn held_partition_moves_the_row_and_names_the_question() {
        //  acceptance: a ready node an open question blocks is absent
        // from ready and present under held with the question id.
        let ready = ok_read(json!([
            {"id": "x-free", "priority": "p1", "title": "free"},
            {"id": "x-hold", "priority": "p1", "title": "held one"}
        ]));
        let held: std::collections::BTreeMap<String, String> =
            [("x-hold".to_string(), "q-1".to_string())]
                .into_iter()
                .collect();
        let (ready, held_rows) = partition_held(ready, &held);
        let ids: Vec<String> = ready
            .rows()
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert_eq!(ids, vec!["x-free".to_string()]);
        assert_eq!(held_rows.len(), 1);
        assert_eq!(held_rows[0]["id"], json!("x-hold"));
        assert_eq!(held_rows[0]["question_id"], json!("q-1"));
    }

    #[test]
    fn held_partition_never_reads_an_unreadable_ready_as_held_free() {
        // A failed ready read must stay a failed read: the split never
        // manufactures an ok-empty feed out of an error.
        let ready = SourceRead::err("ready: reader panicked");
        let held: std::collections::BTreeMap<String, String> =
            [("x-hold".to_string(), "q-1".to_string())]
                .into_iter()
                .collect();
        let (ready, held_rows) = partition_held(ready, &held);
        assert!(!ready.is_ok());
        assert!(held_rows.is_empty());
    }

    #[test]
    fn dispatch_hold_partition_moves_the_row_and_names_the_reason() {
        // An active dispatch_hold parks the node: it leaves the undispatched
        // feed and is named under held with its hold reason.
        let dir = tempfile::tempdir().unwrap();
        let plan_path = write_hold_plan(
            dir.path(),
            "dispatch_hold:\n  reason: waiting on legal\n  release_when: legal clears\n  review_on: 2099-01-01\n  set_by: operator\n",
        );
        let entries = vec![json!({
            "id": "x-hold1",
            "priority": "p1",
            "status": "ready",
            "plan_path": plan_path,
        })];
        let feed = ok_read(json!([
            {"id": "x-free", "priority": "p1", "title": "free"},
            {"id": "x-hold1", "priority": "p1", "title": "held dispatch"}
        ]));
        let (kept, held_rows) = partition_dispatch_held(feed, Some(&entries));
        let ids: Vec<String> = kept
            .rows()
            .iter()
            .filter_map(|r| r.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert_eq!(ids, vec!["x-free".to_string()]);
        assert_eq!(held_rows.len(), 1);
        assert_eq!(held_rows[0]["id"], json!("x-hold1"));
        assert_eq!(held_rows[0]["held_by"], json!("dispatch_hold"));
        assert_eq!(held_rows[0]["hold_reason"], json!("waiting on legal"));
    }

    fn inputs_with(ready: Value, claims: Value, claimed_nodes: Value) -> BoardInputs {
        BoardInputs {
            tasks: SourceRead::ok(json!([])),
            repo_root: String::new(),
            ready: ok_read(ready),
            claims: ok_read(claims),
            worked: ok_read(Value::Array(Vec::new())),
            claimed_nodes: ok_read(claimed_nodes),
            drivers: ok_read(Value::Array(Vec::new())),
            holder_activity: HashMap::new(),
            holder_activity_error: None,
            holder_activity_starved: false,
            prs: ok_read(Value::Array(Vec::new())),
            pr_nodes: ok_read(Value::Array(Vec::new())),
            pr_gates: ok_read(Value::Array(Vec::new())),
            outstanding: ok_read(json!({})),
            needs: ok_read(Value::Array(Vec::new())),
            blocked_child: ok_read(Value::Array(Vec::new())),
            lane: ok_read(Value::Array(Vec::new())),
            undispatched: ok_read(Value::Array(Vec::new())),
            stranded: ok_read(Value::Array(Vec::new())),
            entries: None,
            held: Default::default(),
            warnings: Vec::new(),
            autonomous_merge: false,
            scope_ids: None,
            role_scope: None,
            foreign_owned: ok_read(Value::Array(Vec::new())),
        }
    }

    #[test]
    fn unplanned_queue_is_unreadable_when_worked_source_fails() {
        let mut inputs = inputs_with(
            json!([{"id": "x-live", "priority": "p0", "plan_path": null}]),
            json!([]),
            json!([]),
        );
        inputs.worked = SourceRead::err("roster timeout");

        let board = build_board(&inputs);
        let queue = board["queues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|queue| queue["name"] == "unplanned")
            .unwrap();
        assert_eq!(queue["status"], "unreadable");
        assert!(queue["rows"].as_array().unwrap().is_empty());
        assert_eq!(queue["error"], "roster timeout");
    }

    #[test]
    fn the_fleet_task_queue_lists_open_tasks_report_only() {
        // AC9-HP: open tasks list with their run commands, actionable false,
        // beside an operator_question queue that keeps its own rows.
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.outstanding = ok_read(json!({"questions": [
            {"id": "q-1", "question": "a real ruling", "ts": "t", "session_id": "s"}
        ]}));
        inputs.tasks = ok_read(json!([
            {"id": "ft-a", "lane": "heal", "cwd": "/x", "text": "rebase", "run": "fno do pr rebase 1", "node": "x-1", "ts": "t"},
            {"id": "ft-b", "lane": "pr-nudge", "cwd": "/x", "text": "nudge", "run": "fno agents resume s", "node": "", "ts": "t"}
        ]));
        let board = build_board(&inputs);
        let queues = board["queues"].as_array().unwrap();
        let task_q = queues
            .iter()
            .find(|q| q["name"] == "fleet_task")
            .unwrap_or_else(|| panic!("no fleet_task queue: {board}"));
        assert_eq!(task_q["status"], "ok");
        assert_eq!(task_q["actionable"], json!(false));
        assert_eq!(task_q["count"], json!(2));
        assert_eq!(task_q["rows"][0]["run"], json!("fno do pr rebase 1"));
        let op_q = queues
            .iter()
            .find(|q| q["name"] == "operator_question")
            .unwrap();
        assert_eq!(op_q["count"], json!(1));
    }

    fn write_hold_plan(dir: &std::path::Path, hold_frontmatter: &str) -> String {
        let plan = dir.join("plan.md");
        std::fs::write(
            &plan,
            format!("---\nstatus: ready\n{hold_frontmatter}---\n\n# Held\n"),
        )
        .unwrap();
        plan.display().to_string()
    }

    #[test]
    fn undriven_pr_skips_a_ruling_held_pr_node() {
        // A team dispatch_hold parks the node deliberately; it is not
        // driverless, so undriven_pr must not name it (AC2-HP).
        let dir = tempfile::tempdir().unwrap();
        let plan_path = write_hold_plan(
            dir.path(),
            "dispatch_hold:\n  reason: waiting on legal\n  release_when: legal clears\n  review_on: 2099-01-01\n  set_by: operator\n",
        );
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.entries = Some(vec![json!({
            "id": "x-pr2",
            "priority": "p2",
            "status": "in_review",
            "plan_path": plan_path,
        })]);
        inputs.pr_nodes = ok_read(json!([{
            "id": "x-pr2",
            "priority": "p2",
            "status": "in_review",
            "title": "held work",
            "pr_number": 1895,
        }]));
        let board = build_board(&inputs);
        let queue = board["queues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|queue| queue["name"] == "undriven_pr")
            .unwrap();
        assert_eq!(queue["status"], "ok");
        assert!(queue["rows"].as_array().unwrap().is_empty());
    }

    #[test]
    fn unplanned_note_names_the_batch_and_undispatched_names_the_target() {
        // a rule without a number is advice nobody applies; the
        // queue a lead dispatches from names the command, never the blueprint.
        let inputs = inputs_with(
            json!([{"id": "x-aaaa", "priority": "p0"}]),
            json!([]),
            json!([]),
        );
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let unplanned = queues.iter().find(|q| q["name"] == "unplanned").unwrap();
        let note = unplanned["note"].as_str().unwrap();
        assert!(!note.is_empty());
        assert!(note.to_lowercase().contains("per lead"));
        let undispatched = queues.iter().find(|q| q["name"] == "undispatched").unwrap();
        assert_eq!(undispatched["verb"], "/fno:target");
        assert!(!undispatched["note"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("blueprint"));
    }

    #[test]
    fn a_budget_kill_and_a_failed_exit_tally_apart_but_both_hold_exit_code_1() {
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        // entries: None reads as an unreadable queue of its own; an empty
        // list keeps the fixture's failure count at exactly the two legs.
        inputs.entries = Some(Vec::new());
        inputs.undispatched = SourceRead::over_budget(
            "fno backlog undispatched --json: killed at its 28.5s slice of the board budget; the source did not fail",
        );
        inputs.prs = SourceRead::err("exit 1: gh pr list failed");
        let board = build_board(&inputs);
        assert_eq!(board["over_budget"], 1);
        assert_eq!(board["unreadable"], 1);
        assert_eq!(board["exit_code"], 1);
        let queues = board["queues"].as_array().unwrap();
        let undispatched = queues.iter().find(|q| q["name"] == "undispatched").unwrap();
        assert_eq!(undispatched["status"], "over_budget");
        let prs = queues.iter().find(|q| q["name"] == "mergeable_pr").unwrap();
        assert_eq!(prs["status"], "unreadable");
        assert_eq!(prs["count"], Value::Null);
    }

    #[test]
    fn stalled_holder_excludes_done_nodes() {
        let node = json!({
            "id": "x-doen",
            "priority": "p0",
            "status": "done",
            "completed_at": "2026-08-21T03:16:00Z",
        });
        let claims = json!([{"key": "node:x-doen", "state": "live", "holder": "h"}]);
        let inputs = inputs_with(json!([]), claims, json!([node]));
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let stalled = queues
            .iter()
            .find(|q| q["name"] == "stalled_holder")
            .unwrap();
        assert_eq!(stalled["rows"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn stalled_holder_still_names_a_live_open_node() {
        let node = json!({"id": "x-open", "priority": "p0", "status": "in_progress"});
        let claims = json!([{"key": "node:x-open", "state": "live", "holder": "h"}]);
        let mut inputs = inputs_with(json!([]), claims, json!([node]));
        // The probe ANSWERED for the holder - `unknown` is the batch's
        // per-handle "resolves to nothing" shape - so the row reads stalled.
        // With no entry at all the holder reads unmeasured, and an unmeasured
        // holder belongs to no row.
        inputs.holder_activity.insert(
            "h".to_string(),
            crate::truth_probe::TruthProbe {
                state: "unknown".to_string(),
                provider_refusal: None,
                harness_title: None,
                reachability: None,
                basis: None,
                last_activity_age_s: Some(30.0),
                last_activity_basis: None,
                last_event_at: None,
                last_message: None,
                observed_model: Value::Null,
            },
        );
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let stalled = queues
            .iter()
            .find(|q| q["name"] == "stalled_holder")
            .unwrap();
        let ids: Vec<&str> = stalled["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["x-open"]);
        // AC8-HP: the row names the worker behind the marker, and the source
        // hands the reader the peek command for THAT worker, not the raw holder
        // string no resolver accepts.
        let row = &stalled["rows"].as_array().unwrap()[0];
        assert_eq!(row["worker"], "h");
        assert!(stalled["source"]
            .as_str()
            .unwrap()
            .contains("peek <worker>"));
    }

    #[test]
    fn unheld_progress_names_a_claim_free_in_progress_row() {
        // x-dddd sat in_progress 25 minutes with a free claim and a dead
        // worker while every queue read clean; the status stamp is never
        // revoked, so this queue is the only one that can carry it.
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.entries = Some(vec![json!({
            "id": "x-dddd",
            "priority": "p1",
            "status": "in_progress",
            "title": "dead handoff",
        })]);
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let unheld = queues
            .iter()
            .find(|q| q["name"] == "unheld_progress")
            .unwrap();
        let rows = unheld["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{unheld}");
        assert_eq!(rows[0]["id"], "x-dddd");
        assert!(rows[0].get("claim_state").is_none(), "{:?}", rows[0]);
    }

    #[test]
    fn stranded_tree_names_a_dirty_quiet_driverless_tree() {
        // AC3-HP: a p1 ready node whose tree holds 1 modified file, no live
        // claim or driver, and no write for 61 minutes is stranded, and the
        // row carries the tree, the dirty count and the resume flag.
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.entries = Some(vec![json!({
            "id": "x-dddd", "priority": "p1", "status": "ready",
            "title": "stranded tree",
        })]);
        inputs.stranded = ok_read(json!([
            {"id": "x-dddd", "tree": "/base/footnote/x-dddd", "branch": "feature/x-dddd",
             "dirty": 1, "unpushed": 0, "idle_minutes": 61, "resumable": true},
        ]));
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let stranded = queues
            .iter()
            .find(|q| q["name"] == "stranded_tree")
            .unwrap();
        let rows = stranded["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{stranded}");
        assert_eq!(rows[0]["id"], "x-dddd");
        assert_eq!(rows[0]["dirty"], 1);
        assert_eq!(rows[0]["resumable"], true);
    }

    #[test]
    fn unheld_progress_omits_an_epic_with_a_live_claimed_child() {
        // An epic goes in_progress because its children are worked. The epic
        // never takes a claim or opens a PR, so the leaf test flagged every
        // healthy epic forever. A live child claim is the epic's driver.
        let mut inputs = inputs_with(
            json!([]),
            json!([{"key": "node:x-child", "state": "live", "holder": "h"}]),
            json!([]),
        );
        inputs.entries = Some(vec![
            json!({"id": "x-epic", "priority": "p1", "status": "in_progress", "type": "epic"}),
            json!({"id": "x-child", "priority": "p1", "status": "in_progress", "parent": "x-epic"}),
            json!({"id": "x-worked", "priority": "p1", "status": "in_progress", "parent": "x-epic"}),
        ]);
        // A worked-feed listing holds the parent with no claim anywhere.
        inputs.worked = ok_read(json!([{"id": "x-worked"}]));
        inputs.holder_activity.insert(
            "h".to_string(),
            crate::truth_probe::TruthProbe {
                state: "working".to_string(),
                harness_title: None,
                reachability: None,
                basis: None,
                last_activity_age_s: Some(30.0),
                last_activity_basis: None,
                last_event_at: None,
                last_message: None,
                observed_model: Value::Null,
                provider_refusal: None,
            },
        );
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let unheld = queues
            .iter()
            .find(|q| q["name"] == "unheld_progress")
            .unwrap();
        assert_eq!(unheld["rows"].as_array().unwrap().len(), 0, "{unheld}");
    }

    #[test]
    fn a_panicked_truth_batch_reads_the_five_queues_unreadable() {
        // AC5-ERR: a REAL read error (the batch's reader panicked) must
        // arrive as an unreadable queue, never as rows about workers nobody
        // measured. One Err, five queues blind, exit code 1: the lead is
        // told the board cannot see, which is the honest answer. The
        // timeout receipt rides the starved flag instead.
        let node = json!({"id": "x-blind", "priority": "p0", "status": "in_progress"});
        let claims = json!([{"key": "node:x-blind", "state": "live", "holder": "h"}]);
        let mut inputs = inputs_with(json!([]), claims, json!([node]));
        inputs.holder_activity_error = Some("truth probe: reader panicked".to_string());
        let board = build_board(&inputs);
        assert_eq!(board["exit_code"], 1);
        let queues = board["queues"].as_array().unwrap();
        for name in [
            "stalled_holder",
            "stale_claim",
            "unheld_progress",
            "undriven_pr",
            "unplanned",
        ] {
            let q = queues.iter().find(|q| q["name"] == name).expect(name);
            assert_eq!(q["status"], "unreadable", "{name}");
            assert_eq!(q["rows"].as_array().unwrap().len(), 0, "{name}");
        }
    }

    #[test]
    fn an_unread_owner_read_makes_the_unplanned_queue_blind_not_clean() {
        let mut inputs = inputs_with(
            json!([{"id": "x-theirs", "priority": "p1", "title": "theirs"}]),
            json!([]),
            json!([]),
        );
        inputs.foreign_owned = SourceRead::err("territory: registry unreadable (boom)");
        let board = build_board(&inputs);
        let unplanned = board["queues"]
            .as_array()
            .unwrap()
            .iter()
            .find(|q| q["name"] == "unplanned")
            .unwrap();
        assert_eq!(unplanned["status"], "unreadable", "{unplanned}");
        assert!(
            unplanned["error"]
                .as_str()
                .unwrap()
                .contains("registry unreadable"),
            "{unplanned}"
        );
    }

    #[test]
    fn mergeable_pr_is_scoped_by_the_binding_node() {
        // On a scoped board a PR whose node cannot be resolved fails
        // CLOSED - unknown attribution is not every team's work. PR 1494 is
        // attributed outside the team (out_of_scope); PR 99 is bound by no
        // node at all, so it lands in no queue and the warning names the drop.
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.prs = ok_read(json!([
            {"number": 1494, "title": "foreign"},
            {"number": 99, "title": "unbound"},
        ]));
        inputs.pr_nodes = ok_read(json!([
            {"id": "x-out", "priority": "p1", "pr_number": 1494},
        ]));
        inputs.scope_ids = Some(["x-in"].into_iter().map(str::to_string).collect());
        inputs.role_scope = Some("x-team".to_string());
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let mergeable = queues.iter().find(|q| q["name"] == "mergeable_pr").unwrap();
        let numbers: Vec<i64> = mergeable["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["number"].as_i64())
            .collect();
        assert_eq!(numbers, Vec::<i64>::new(), "{mergeable}");
        let out_of_scope = queues.iter().find(|q| q["name"] == "out_of_scope").unwrap();
        let mut ids: Vec<&str> = out_of_scope["rows"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids, vec!["x-out"], "{out_of_scope}");
        let warnings = board["warnings"].as_array().unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.as_str().unwrap_or("").contains("unattributed")
                    && w.as_str().unwrap_or("").contains("mergeable_pr")),
            "{warnings:?}"
        );
    }

    #[test]
    fn ac1_blocked_child_names_the_node_session_reason_and_age() {
        // AC1-HP: an in-scope unanswered blocked row names node, session,
        // reason, and age when the board runs from the teamed session.
        let mut inputs = inputs_with(json!([]), json!([]), json!([]));
        inputs.blocked_child = ok_read(json!([{
            "id": "x-0000",
            "session": "cx-run-1",
            "reason": "worktree-init-blocked",
            "evidence": "Operation not permitted",
            "age_minutes": 45,
        }]));
        inputs.scope_ids = Some(["x-0000"].into_iter().map(str::to_string).collect());
        let board = build_board(&inputs);
        let queues = board.get("queues").and_then(Value::as_array).unwrap();
        let blocked = queues
            .iter()
            .find(|q| q["name"] == "blocked_child")
            .unwrap();
        let rows = blocked["rows"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "{blocked}");
        assert_eq!(rows[0]["id"], "x-0000");
        assert_eq!(rows[0]["session"], "cx-run-1");
        assert_eq!(rows[0]["reason"], "worktree-init-blocked");
        assert_eq!(rows[0]["age_minutes"], 45);
    }

    /// Serializes the tests that point process-global HOME at a temp dir:
    /// every other test in this binary reads HOME, so a concurrent reader can
    /// catch it mid-flip (the same ENV_LOCK shape client_tests uses).
    static HOME_LOCK: std::sync::LazyLock<&'static std::sync::Mutex<()>> =
        std::sync::LazyLock::new(crate::claims::test_env_lock);

    /// Restores the env values a test pinned, whatever way the body ends.
    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);
    impl EnvRestore {
        fn take(vars: &[&'static str]) -> Self {
            EnvRestore(vars.iter().map(|v| (*v, std::env::var_os(*v))).collect())
        }
    }
    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (k, v) in self.0.iter() {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    #[test]
    fn the_driver_feed_projects_live_rows_only() {
        // A closed run (exited/failed/permanent_dead) whose transcript still
        // answers must never be a driver candidate: it would let a finished
        // worker suppress its node's undriven-PR row. Mirrors the
        // spawn_gate.LIVE_STATUSES vocabulary.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Pins die with the body: a later test must never read a dropped
        // TempDir through a leaked env value.
        let _restore = EnvRestore::take(&["FNO_AGENTS_HOME", "FNO_SPACES_DIR", "HOME"]);
        let dir = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(dir.path());
        let agents_home = dir.path().join(".fno").join("agents");
        std::env::set_var("FNO_AGENTS_HOME", &agents_home);
        let path = agents_home.join("registry.json");
        let live_row = crate::state::RegistryEntry {
            name: "t-live-worker".to_string(),
            node: Some("x-live".to_string()),
            status: crate::AgentStatus::Live,
            harness: Some("claude".to_string()),
            harness_session_id: Some("uuid-live".to_string()),
            ..Default::default()
        };
        let exited_row = crate::state::RegistryEntry {
            name: "t-done-worker".to_string(),
            node: Some("x-done".to_string()),
            status: crate::AgentStatus::Exited,
            harness: Some("claude".to_string()),
            harness_session_id: Some("uuid-done".to_string()),
            ..Default::default()
        };
        crate::state::update_registry(&path, |registry| {
            registry.entries.push(live_row);
            registry.entries.push(exited_row);
        })
        .unwrap();
        let (read, name_to_token) = read_driver_rows(None, None);
        std::env::remove_var("FNO_AGENTS_HOME");
        assert!(read.is_ok(), "{read:?}");
        let rows = read.payload.unwrap().as_array().unwrap().clone();
        // The name map covers every registry entry, whatever its status: the
        // exited row names a transcript that can still answer for its node.
        assert_eq!(
            name_to_token.get("t-done-worker").map(String::as_str),
            Some("uuid-done")
        );
        let nodes: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("node").and_then(Value::as_str))
            .collect();
        assert_eq!(nodes, vec!["x-live"], "{rows:?}");
    }

    #[test]
    fn a_supplied_registry_feeds_the_driver_feed_without_a_store_read() {
        // The board's driver slice must not pay a second registry read when
        // the caller already loaded one this fire: a supplied registry feeds
        // the fold even where the store read would answer empty (a test
        // process with no declared home). That is the proof the store was
        // never consulted.
        let registry = crate::state::Registry {
            entries: vec![crate::state::RegistryEntry {
                name: "t-supplied-worker".to_string(),
                node: Some("x-supplied".to_string()),
                status: crate::AgentStatus::Live,
                harness: Some("claude".to_string()),
                harness_session_id: Some("uuid-supplied".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (read, name_to_token) = read_driver_rows(None, Some(&registry));
        assert!(read.is_ok(), "{read:?}");
        let rows = read.payload.unwrap().as_array().unwrap().clone();
        let nodes: Vec<&str> = rows
            .iter()
            .filter_map(|r| r.get("node").and_then(Value::as_str))
            .collect();
        assert_eq!(nodes, vec!["x-supplied"], "{rows:?}");
        assert_eq!(
            name_to_token.get("t-supplied-worker").map(String::as_str),
            Some("uuid-supplied")
        );
    }

    #[test]
    fn the_board_answers_inside_a_tight_budget_with_every_queue_present() {
        // An isolated HOME + cwd: no graph, no claims, no lane - the degraded
        // machine. The board must still answer with all fourteen queues
        // (plus nothing else), the unreadable actionable ones counted, and
        // exit 1.
        // A declared hermetic root: blocked_child's collection resolves
        // ~/.fno/agents via AgentsHome, which panics under test with no
        // declared root (paths.rs) - this test already pins HOME, so it also
        // pins FNO_AGENTS_HOME under the same tempdir to declare one.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Pins die with the body: a later test must never read a dropped
        // TempDir through a leaked env value.
        let _restore = EnvRestore::take(&["FNO_AGENTS_HOME", "FNO_SPACES_DIR", "HOME"]);
        let dir = tempfile::tempdir().unwrap();
        std::env::set_var("HOME", dir.path());
        std::env::set_var("FNO_AGENTS_HOME", dir.path().join(".fno").join("agents"));
        crate::paths::pin_test_claims_root(dir.path());
        let payload = read_board(&BoardOpts {
            budget_ms: 20_000,
            ..Default::default()
        });
        std::env::remove_var("FNO_AGENTS_HOME");
        let queues = payload.get("queues").and_then(Value::as_array).unwrap();
        assert_eq!(queues.len(), 16, "{payload}");
        assert_eq!(payload["exit_code"], 1, "{payload}");
        assert!(payload["unreadable"].as_i64().unwrap() > 0);
        let names: Vec<&str> = queues
            .iter()
            .filter_map(|q| q.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            vec![
                "operator_lane",
                "undispatched",
                "unplanned",
                "stalled_holder",
                "unheld_progress",
                "blocked_child",
                "undriven_pr",
                "stranded_tree",
                "mergeable_pr",
                "stale_claim",
                "operator_question",
                "fleet_task",
                "carveout_pending",
                "capture_pending",
                "failed_verdict",
                "unreachable_worker",
            ]
        );
        // The done-probe's contract: every named source carries its verdict.
        let sources = payload.get("sources").and_then(Value::as_object).unwrap();
        assert!(!sources.is_empty());
        for (_name, s) in sources {
            assert!(s.get("ok").is_some() && s.get("truncated").is_some(), "{s}");
        }
    }

    #[test]
    fn the_scope_error_queue_is_actionable_and_loud() {
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Pins die with the body: a later test must never read a dropped
        // TempDir through a leaked env value.
        let _restore = EnvRestore::take(&[
            "FNO_AGENTS_HOME",
            "FNO_SPACES_DIR",
            "FNO_CONFIG",
            "FNO_HOME",
            "HOME",
        ]);
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("lead.md");
        std::fs::write(&state, "---\nscope: not-a-real-thing\n---\n").unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(&config, "[work.workspaces]\n").unwrap();
        std::env::set_var("FNO_CONFIG", &config);
        std::env::set_var("HOME", dir.path());
        std::env::set_var("FNO_HOME", dir.path());
        crate::paths::pin_test_claims_root(dir.path());
        crate::graph_store::seed_rows(
            &graph_json_path(dir.path()),
            &[json!({"id": "x-existing", "type": "epic", "status": "ready"})],
        )
        .unwrap();
        let payload = read_board(&BoardOpts {
            budget_ms: 20_000,
            state_path: Some(state),
            ..Default::default()
        });
        let queues = payload.get("queues").and_then(Value::as_array).unwrap();
        assert_eq!(queues.len(), 1);
        assert_eq!(queues[0]["name"], "scope");
        assert_eq!(queues[0]["status"], "unreadable");
        assert_eq!(queues[0]["actionable"], true);
        assert_eq!(payload["exit_code"], 1);
        assert!(queues[0]["error"]
            .as_str()
            .unwrap()
            .contains("neither a configured project nor a backlog node"));

        let corrupt_home = dir.path().join("corrupt-state");
        std::env::set_var("FNO_HOME", &corrupt_home);
        let graph = graph_json_path(dir.path());
        let db = crate::backlog::database_path(&graph);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(db, b"not a database").unwrap();
        let payload = read_board(&BoardOpts {
            budget_ms: 20_000,
            state_path: Some(dir.path().join("lead.md")),
            ..Default::default()
        });
        let error = payload["queues"][0]["error"].as_str().unwrap();
        assert!(error.contains("graph unreadable"), "{error}");
        assert!(!error.contains("neither a configured project"), "{error}");
    }

    #[test]
    fn a_slow_source_is_killed_inside_the_whole_board_budget() {
        // A scripted `fno` that sleeps 5 seconds under a
        // 4,000ms board budget must be killed at its deadline-derived spawn
        // bound, so the collector returns inside ~4.75s and the over-budget
        // source reads as unreadable - never the measured 40,776ms-against-
        // 30,000ms overrun, and never the 4.9s the captured-slice shape
        // accepted. The budget leaves room for the pre-spawn in-process
        // reads on a loaded runner (measured: the sibling 2,000ms shape
        // expired before its slow source spawned), and the cap stays under
        // the 5s sleep so an unbounded read still trips it.
        let _guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Pins die with the body: a later test must never read a dropped
        // TempDir through a leaked env value.
        let _restore = EnvRestore::take(&["FNO_AGENTS_HOME", "FNO_SPACES_DIR", "HOME"]);
        let dir = tempfile::tempdir().unwrap();
        // Hermetic state roots: the board reads resolve through pinned env.
        std::env::set_var("FNO_AGENTS_HOME", dir.path().join("agents"));
        std::env::set_var("FNO_SPACES_DIR", dir.path().join("spaces"));
        std::env::set_var("HOME", dir.path());
        crate::paths::pin_test_claims_root(dir.path());
        let script =
            crate::write_exec_stub(dir.path(), "sleepy-fno-py", "#!/bin/sh\nexec sleep 5\n");
        // `gh` answers with an empty-open-PR listing: the prs reads must
        // never reach the network, whose discretionary GraphQL quota is
        // shared machine-wide and reads exit 75 under a lock (measured: the
        // quota refusal surfaced here as an unreadable undriven_pr).
        let stub_dir = dir.path().join("stub-bin");
        std::fs::create_dir_all(&stub_dir).unwrap();
        crate::write_exec_stub(&stub_dir, "gh", "#!/bin/sh\necho '[]'\n");
        let prev = std::env::var_os("FNO_PY");
        let prev_path = std::env::var_os("PATH");
        std::env::set_var("FNO_PY", &script);
        std::env::set_var(
            "PATH",
            format!(
                "{}:{}",
                stub_dir.display(),
                prev_path.as_deref().and_then(|p| p.to_str()).unwrap_or("")
            ),
        );
        let start = std::time::Instant::now();
        // The cwd too: from the crate dir the needs fold reads the canonical
        // checkout's live journal, which measured 20s in a debug build.
        let payload = read_board(&BoardOpts {
            budget_ms: 4_000,
            cwd: Some(dir.path().to_path_buf()),
            ..Default::default()
        });
        let elapsed = start.elapsed();
        match prev {
            Some(v) => std::env::set_var("FNO_PY", v),
            None => std::env::remove_var("FNO_PY"),
        }
        match prev_path {
            Some(v) => std::env::set_var("PATH", v),
            None => std::env::remove_var("PATH"),
        }
        // The kill bound is the SLEEP length: an unbounded read would blow
        // past 5s, and a spawn after the in-process work would land past the
        // budget; the deadline-derived bound must keep the whole board under
        // budget plus the serialization reserve.
        assert!(
            elapsed < std::time::Duration::from_millis(4_750),
            "board took {elapsed:?} against a 4,000ms budget with a 5s sleep source"
        );
        let parsed = crate::lead_termination::parse_org_board_value(&payload).expect("parses");
        // A budget kill is the board's own choice, not evidence about the
        // source: it must not block completion, but it must stay named. The
        // message names the offending queues so a real read failure (a live
        // `gh` under a quota lock, once measured here) reads in the failure
        // itself.
        let unreadable: Vec<String> = payload["queues"]
            .as_array()
            .map(|qs| {
                qs.iter()
                    .filter(|q| q["status"] == "unreadable")
                    .filter_map(|q| {
                        Some(format!(
                            "{}: {}",
                            q["name"].as_str()?,
                            q["error"].as_str().unwrap_or("?")
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !parsed.unreadable_sources,
            "the killed source must read as not-read, not unreadable; unreadable queues: {unreadable:?}"
        );
        assert!(
            !parsed.blind_queues.is_empty(),
            "the killed source must still be named"
        );
    }
}
