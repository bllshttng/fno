//! Attribute one process-table snapshot to live agent sessions.
//!
//! This module deliberately works on borrowed census rows.  A machine tick
//! performs one process read and passes that same table to both the machine
//! counters and this attribution pass.

use crate::census::ProcRow;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRoot {
    pub session_id: String,
    pub harness: String,
    pub name: Option<String>,
    pub node: Option<String>,
    pub pids: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseRow {
    pub session_id: String,
    pub node: Option<String>,
    pub stage: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Attribution {
    pub sessions: Vec<Value>,
    pub unresolved: Vec<Value>,
    pub top_rss: Vec<Value>,
}

fn parent_map(table: &[ProcRow]) -> HashMap<u32, u32> {
    table.iter().map(|row| (row.pid, row.ppid)).collect()
}

fn root_for(pid: u32, parents: &HashMap<u32, u32>, roots: &HashMap<u32, usize>) -> Option<usize> {
    let mut current = pid;
    let mut seen = HashSet::new();
    for _ in 0..64 {
        if let Some(root) = roots.get(&current) {
            return Some(*root);
        }
        if !seen.insert(current) {
            return None;
        }
        current = *parents.get(&current)?;
        if current <= 1 {
            return None;
        }
    }
    None
}

fn basename(command: &str) -> &str {
    command
        .split_whitespace()
        .next()
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("")
}

fn bucket(row: &ProcRow) -> Option<&'static str> {
    if crate::test_run::is_cargo_row(row)
        || crate::test_run::is_compile(row)
        || crate::orphan_reap::is_deps_test_binary(&row.command)
    {
        return Some("cargo");
    }
    let argv: Vec<String> = row.command.split_whitespace().map(str::to_string).collect();
    if matches!(basename(&row.command), "pytest" | "py.test")
        || (crate::hook::test_run_guard::is_python(basename(&row.command))
            && crate::hook::test_run_guard::has_dash_m_module(&argv, "pytest"))
    {
        return Some("pytest");
    }
    None
}

fn stage_for(
    root: &SessionRoot,
    phases: &[PhaseRow],
    test_pids: &HashSet<u32>,
    queued_pids: &HashSet<u32>,
    owned: &[&ProcRow],
) -> (Option<String>, Option<String>) {
    let is_test = root.pids.iter().any(|pid| test_pids.contains(pid))
        || owned.iter().any(|row| test_pids.contains(&row.pid));
    if is_test {
        return (Some("test".into()), root.node.clone());
    }
    let is_queued = root.pids.iter().any(|pid| queued_pids.contains(pid))
        || owned.iter().any(|row| queued_pids.contains(&row.pid));
    if is_queued {
        return (Some("test-queued".into()), root.node.clone());
    }
    phases
        .iter()
        .rev()
        .find(|phase| phase.session_id == root.session_id)
        .map(|phase| (phase.stage.clone(), phase.node.clone()))
        .unwrap_or((None, root.node.clone()))
}

pub fn attribute(
    table: &[ProcRow],
    roots: &[SessionRoot],
    phases: &[PhaseRow],
    test_pids: &HashSet<u32>,
    queued_pids: &HashSet<u32>,
) -> Attribution {
    let parents = parent_map(table);
    let root_map: HashMap<u32, usize> = roots
        .iter()
        .enumerate()
        .flat_map(|(i, root)| root.pids.iter().copied().map(move |pid| (pid, i)))
        .collect();
    let mut owned: Vec<Vec<&ProcRow>> = (0..roots.len()).map(|_| Vec::new()).collect();
    let mut owners = HashMap::new();
    for row in table {
        if let Some(index) = root_for(row.pid, &parents, &root_map) {
            owned[index].push(row);
            owners.insert(row.pid, roots[index].session_id.clone());
        }
    }
    let mut sessions = Vec::new();
    let mut unresolved = Vec::new();
    for (index, root) in roots.iter().enumerate() {
        if owned[index].is_empty() {
            unresolved.push(json!({
                "session_id": root.session_id,
                "node": root.node,
                "stage": null,
                "reason": "root pid not in process table"
            }));
            continue;
        }
        let (stage, node) = stage_for(root, phases, test_pids, queued_pids, &owned[index]);
        let session_pids: Vec<u32> = owned[index].iter().map(|row| row.pid).collect();
        let (footprint, footprint_unread) = footprint_mb(&session_pids);
        let mut row = json!({
            "session_id": root.session_id,
            "name": root.name,
            "harness": root.harness,
            "node": node,
            "stage": stage,
            "procs": owned[index].len(),
            "rss_mb": owned[index].iter().map(|r| r.rss_kb).sum::<u64>() as f64 / 1024.0,
            "cpu_pct": owned[index].iter().map(|r| r.cpu_pct).sum::<f64>(),
            "top_command": owned[index]
                .iter()
                .max_by(|a, b| a.cpu_pct.partial_cmp(&b.cpu_pct).unwrap_or(std::cmp::Ordering::Equal))
                .map(|process| process.command.clone()),
            "footprint_mb": footprint,
            "footprint_unread": footprint_unread,
        });
        let mut buckets: MapBuckets = BTreeMap::new();
        for process in &owned[index] {
            if let Some(name) = bucket(process) {
                let item = buckets.entry(name).or_default();
                item.0 += 1;
                item.1 += process.rss_kb;
                item.2 += process.cpu_pct;
            }
        }
        if !buckets.is_empty() {
            let mut values = serde_json::Map::new();
            for (name, (procs, rss, cpu)) in buckets {
                values.insert(
                    name.into(),
                    json!({ "procs": procs, "rss_mb": rss as f64 / 1024.0, "cpu_pct": cpu }),
                );
            }
            row["buckets"] = Value::Object(values);
        }
        sessions.push(row);
    }
    Attribution {
        sessions,
        unresolved,
        top_rss: top_by_rss(table, &owners, 10),
    }
}

/// Every pid the fleet's own session roots own: the owner map's keys from
/// the same `root_for` walk `attribute` builds, exported so the machine
/// arm's outside-group read shares the one ownership answer.
pub fn owned_pids(table: &[ProcRow], roots: &[SessionRoot]) -> HashSet<u32> {
    let parents = parent_map(table);
    let root_map: HashMap<u32, usize> = roots
        .iter()
        .enumerate()
        .flat_map(|(i, root)| root.pids.iter().copied().map(move |pid| (pid, i)))
        .collect();
    table
        .iter()
        .filter(|row| root_for(row.pid, &parents, &root_map).is_some())
        .map(|row| row.pid)
        .collect()
}

type MapBuckets = BTreeMap<&'static str, (u64, u64, f64)>;

pub fn tree_rss(table: &[ProcRow], pids: &[u32]) -> BTreeMap<u32, u64> {
    let parents = parent_map(table);
    let roots: HashMap<u32, usize> = pids.iter().enumerate().map(|(i, pid)| (*pid, i)).collect();
    let mut totals: BTreeMap<u32, u64> = pids
        .iter()
        .filter(|pid| table.iter().any(|row| row.pid == **pid))
        .map(|pid| (*pid, 0))
        .collect();
    for row in table {
        if let Some(index) = root_for(row.pid, &parents, &roots) {
            if let Some(root) = pids.get(index) {
                *totals.entry(*root).or_default() += row.rss_kb / 1024;
            }
        }
    }
    totals
}

pub fn top_by_rss(table: &[ProcRow], owners: &HashMap<u32, String>, limit: usize) -> Vec<Value> {
    let mut rows: Vec<&ProcRow> = table.iter().collect();
    rows.sort_by(|a, b| b.rss_kb.cmp(&a.rss_kb).then(a.pid.cmp(&b.pid)));
    rows.into_iter()
        .take(limit)
        .map(|row| {
            json!({
                "name": basename(&row.command),
                "pid": row.pid,
                "rss_mb": row.rss_kb as f64 / 1024.0,
                "cpu_pct": row.cpu_pct,
                "session_id": owners.get(&row.pid),
            })
        })
        .collect()
}

pub fn footprint_mb(pids: &[u32]) -> (Option<f64>, u64) {
    if !cfg!(target_os = "macos") {
        return (None, 0);
    }
    let mut total_bytes = 0u64;
    let mut unread = 0u64;
    for pid in pids {
        match crate::memory_groups::phys_footprint(*pid) {
            Some((now, _)) => total_bytes = total_bytes.saturating_add(now),
            None => unread = unread.saturating_add(1),
        }
    }
    (Some(total_bytes as f64 / 1024.0 / 1024.0), unread)
}

pub fn session_roots(home: &crate::paths::AgentsHome, table: &[ProcRow]) -> Vec<SessionRoot> {
    let mut roots = BTreeMap::<String, SessionRoot>::new();
    let mut warnings = Vec::new();
    for entry in crate::spawn_gate::live_rows(&home.registry_json(), &mut warnings) {
        let Some(session_id) = entry.harness_session_id.or(entry.session_id) else {
            continue;
        };
        let Some(pid) = entry.pid else { continue };
        let root = roots.entry(session_id.clone()).or_insert(SessionRoot {
            session_id,
            harness: entry
                .harness
                .or(entry.provider)
                .unwrap_or_else(|| "unknown".into()),
            name: Some(entry.name),
            node: entry.node,
            pids: Vec::new(),
        });
        if !root.pids.contains(&pid) {
            root.pids.push(pid);
        }
    }
    if let Ok(roster) = crate::claude_roster::ClaudeRoster::load_default() {
        for worker in roster.workers_deduped() {
            let Some(pid) = worker.pid else { continue };
            if !crate::daemon::pid_is_ours(pid, worker.proc_start) {
                continue;
            }
            let root_pid = worker
                .repl_pid
                .filter(|candidate| table.iter().any(|row| row.pid == *candidate))
                .unwrap_or(pid);
            roots
                .entry(worker.session_id.clone())
                .or_insert(SessionRoot {
                    session_id: worker.session_id.clone(),
                    harness: "claude".into(),
                    name: None,
                    node: None,
                    pids: vec![root_pid],
                });
        }
    }
    for row in table.iter().filter(|row| row.ppid == 1) {
        let Some(socket) = crate::process_owner::mux_server_socket(&row.command) else {
            continue;
        };
        let crate::process_owner::OwnerRead::Owner(lease) =
            crate::process_owner::owner_lease_for_server(row.pid, Path::new(&socket))
        else {
            continue;
        };
        let root = roots
            .entry(lease.session.clone())
            .or_insert_with(|| SessionRoot {
                session_id: lease.session,
                harness: "unknown".into(),
                name: None,
                node: None,
                pids: Vec::new(),
            });
        if !root.pids.contains(&row.pid) {
            root.pids.push(row.pid);
        }
    }
    roots.into_values().collect()
}

fn phase_rows(home: &crate::paths::AgentsHome) -> Result<Vec<PhaseRow>, String> {
    let path = crate::gc_sweep::graph_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let rows = crate::graph_store::read_rows_where(
        &path,
        &crate::backlog::RowQuery {
            fields: Some(
                [
                    "id",
                    "session_id",
                    "node",
                    "phase",
                    "harness",
                    "started_at",
                    "ended_at",
                    "sessions",
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
    .map_err(|error| format!("{}: {}", path.display(), error.0))?;
    let mut phases = Vec::new();
    for row in rows {
        for phase in ["think", "blueprint", "execute", "review", "ship"] {
            if !crate::graph_store::is_open_phase_row(&row, phase) {
                continue;
            }
            let Some(session_id) = row.get("session_id").and_then(Value::as_str) else {
                continue;
            };
            phases.push(PhaseRow {
                session_id: session_id.to_string(),
                node: row.get("node").and_then(Value::as_str).map(str::to_string),
                stage: Some(phase.to_string()),
            });
        }
    }
    Ok(phases)
}

/// Pids holding a live test run: the recorded pid of every run-door claim
/// (`test:suite`, `test:cargo-run:<i>`), with the bash-era `test-run:` holder
/// parse kept for a claim that recorded none. `test:priority` is a lane
/// marker, not a run, and never counts.
fn test_pids() -> HashSet<u32> {
    let doors: HashSet<String> = test_run_door_keys().into_iter().collect();
    crate::claims::list(Some("test:"), None, false)
        .unwrap_or_default()
        .into_iter()
        .filter(|claim| doors.contains(&claim.key))
        .filter_map(|claim| match claim.pid {
            Some(pid) => u32::try_from(pid).ok(),
            None => claim
                .holder
                .strip_prefix("test-run:")?
                .split(':')
                .next()?
                .parse()
                .ok(),
        })
        .collect()
}

fn test_run_door_keys() -> Vec<String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::test_run::test_door_keys(&cwd)
}

/// Pids waiting at a test door: the waiter tickets of the priority, queue
/// and full lanes of the suite door and the first cargo run slot. A cap of
/// zero seats leaves no run slot to wait on, so only the suite door reads.
fn queued_test_pids() -> HashSet<u32> {
    let mut doors = vec![crate::test_run::SUITE_CLAIM_KEY.to_string()];
    doors.extend(
        test_run_door_keys()
            .into_iter()
            .filter(|key| key.starts_with("test:cargo-run:"))
            .take(1),
    );
    let mut out = HashSet::new();
    for key in doors {
        let Ok(claim_path) = crate::claims::claim_path(&key, None) else {
            continue;
        };
        for lane in ["priority", "queue", "full"] {
            for pid in crate::claim_queue::waiter_pids(&crate::claim_queue::lane_dir_for(
                &claim_path,
                lane,
            )) {
                out.insert(pid);
            }
        }
    }
    out
}

/// Test-run pids no session root owns: a hand-typed cargo test in a bare
/// shell, or a ticket stamped on another machine.
fn unattributed(pids: &HashSet<u32>, owned: &HashSet<u32>) -> usize {
    pids.iter().filter(|pid| !owned.contains(pid)).count()
}

pub fn price(home: &crate::paths::AgentsHome, table: &[ProcRow]) -> Result<Value, String> {
    let roots = session_roots(home, table);
    let phases = phase_rows(home)?;
    let test_pids = test_pids();
    let queued = queued_test_pids();
    let out = attribute(table, &roots, &phases, &test_pids, &queued);
    let memory = crate::memory_groups::reading_for(table, &roots);
    Ok(json!({
        "sessions": out.sessions,
        "unresolved": out.unresolved,
        "top_rss": out.top_rss,
        "memory": memory,
    }))
}

/// The test feed the spawn-gate probe serves: which managed sessions run or
/// queue tests right now, and the run pids no session owns.
pub fn test_stages(home: &crate::paths::AgentsHome) -> Result<Value, String> {
    let (table, _) = crate::census::process_table();
    let roots = session_roots(home, &table);
    let running = test_pids();
    let queued = queued_test_pids();
    let out = attribute(&table, &roots, &[], &running, &queued);
    let owned = owned_pids(&table, &roots);
    let sessions: Vec<Value> = out
        .sessions
        .into_iter()
        .filter(|row| matches!(row["stage"].as_str(), Some("test") | Some("test-queued")))
        .collect();
    Ok(json!({
        "sessions": sessions,
        "running": running.len(),
        "queued": queued.len(),
        "unattributed": {
            "running": unattributed(&running, &owned),
            "queued": unattributed(&queued, &owned),
        },
    }))
}

// --- The transcript cost extractor -----------------------------------------
//
// One JSONL line per content block; every line of the same API message
// repeats identical `message.usage`, so usage and message counts accumulate
// once per unique (message.id, requestId) pair. Lines missing either field
// count as-is: no false dedup, matching the community cost-report semantics.
// Resumed sessions copy history lines (with usage) into the new file, so
// callers summing multiple transcripts share one dedup set per logical sum.

type DedupKey = (String, String);

#[derive(Clone)]
pub(crate) struct SessionMetrics {
    session_id: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_create_tokens: u64,
    assistant_messages: u64,
    user_messages: u64,
    mail_messages: u64,
    subagent_messages: u64,
    compaction_count: u64,
    // First-seen order; the primary-model pick keeps the first maximum on
    // ties, which is Python max() over an insertion-ordered dict.
    models: Vec<(String, u64)>,
    first_timestamp: String,
    last_timestamp: String,
    cost_usd: f64,
    unpriced: bool,
}

impl SessionMetrics {
    fn new(session_id: &str) -> Self {
        SessionMetrics {
            session_id: session_id.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_create_tokens: 0,
            assistant_messages: 0,
            user_messages: 0,
            mail_messages: 0,
            subagent_messages: 0,
            compaction_count: 0,
            models: Vec::new(),
            first_timestamp: String::new(),
            last_timestamp: String::new(),
            cost_usd: 0.0,
            unpriced: false,
        }
    }

    fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_create_tokens
    }

    fn duration_minutes(&self) -> f64 {
        if self.first_timestamp.is_empty() || self.last_timestamp.is_empty() {
            return 0.0;
        }
        // Both sides slice to 19 chars before parsing, so a shorter stamp
        // fails the parse and reads zero with the same warning the ported
        // leg printed.
        let parse = |ts: &str| -> Result<i64, String> {
            let head = ts.get(..19).ok_or_else(|| "too short".to_string())?;
            chrono::NaiveDateTime::parse_from_str(head, "%Y-%m-%dT%H:%M:%S")
                .map(|dt| dt.and_utc().timestamp())
                .map_err(|e| e.to_string())
        };
        match (parse(&self.first_timestamp), parse(&self.last_timestamp)) {
            (Ok(a), Ok(b)) => (b - a) as f64 / 60.0,
            (Err(e), _) | (_, Err(e)) => {
                eprintln!("Warning: could not parse timestamps: {e}");
                0.0
            }
        }
    }

    fn primary_model(&self) -> String {
        let mut best: Option<(&str, u64)> = None;
        for (model, count) in &self.models {
            match best {
                Some((_, top)) if *count <= top => {}
                _ => best = Some((model.as_str(), *count)),
            }
        }
        best.map(|(m, _)| m.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn bump_model(&mut self, model: &str) {
        if let Some(entry) = self.models.iter_mut().find(|(m, _)| m == model) {
            entry.1 += 1;
        } else {
            self.models.push((model.to_string(), 1));
        }
    }

    fn merge(&mut self, other: &SessionMetrics) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_create_tokens += other.cache_create_tokens;
        self.assistant_messages += other.assistant_messages;
        self.user_messages += other.user_messages;
        self.mail_messages += other.mail_messages;
        self.subagent_messages += other.subagent_messages;
        self.compaction_count += other.compaction_count;
        for (model, count) in &other.models {
            if let Some(entry) = self.models.iter_mut().find(|(m, _)| m == model) {
                entry.1 += count;
            } else {
                self.models.push((model.clone(), *count));
            }
        }
        self.cost_usd += other.cost_usd;
        self.unpriced = self.unpriced || other.unpriced;
    }
}

/// Python truthiness for the transcript fields the fold tests: a missing
/// key, null, false, zero, and the empty string are all false.
fn py_truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Accumulate one transcript JSONL entry into `metrics`. Returns the user
/// text for a user-type row (caller batch-classifies user vs mail), `None`
/// for anything else. `prev_context_size` carries the last main-chain
/// context size for compaction detection (`None` disables the series, as
/// the branch breakdown's call does); `seen` dedups API messages.
fn accumulate_entry(
    obj: &Value,
    metrics: &mut SessionMetrics,
    mut prev_context_size: Option<&mut Option<u64>>,
    seen: &mut HashSet<DedupKey>,
) -> Option<String> {
    if let Some(ts) = obj.get("timestamp").and_then(Value::as_str) {
        if !ts.is_empty() {
            if metrics.first_timestamp.is_empty() {
                metrics.first_timestamp = ts.to_string();
            }
            metrics.last_timestamp = ts.to_string();
        }
    }

    match obj.get("type").and_then(Value::as_str) {
        Some("user") => {
            let msg = obj.get("message");
            let msg_content = match msg {
                Some(m) if m.is_object() => m.get("content"),
                _ => obj.get("content"),
            };
            let text = match msg_content {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|b| b.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            Some(text)
        }
        Some("assistant") => {
            let msg = obj.get("message").cloned().unwrap_or_else(|| json!({}));
            let msg_id = msg.get("id").and_then(Value::as_str);
            let request_id = obj.get("requestId").and_then(Value::as_str);
            if let (Some(id), Some(req)) = (msg_id, request_id) {
                let key: DedupKey = (id.to_string(), req.to_string());
                if seen.contains(&key) {
                    // Duplicate content-block line of an already-counted API
                    // message: usage and counts skip; the compaction series
                    // is unaffected because duplicates carry identical usage.
                    return None;
                }
                seen.insert(key);
            }
            metrics.assistant_messages += 1;
            if py_truthy(obj.get("isSidechain")) {
                metrics.subagent_messages += 1;
            }
            let model = msg
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            metrics.bump_model(model);

            let usage = msg.get("usage").cloned().unwrap_or_else(|| json!({}));
            let num = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
            let input_t = num("input_tokens");
            let output_t = num("output_tokens");
            let cache_read = num("cache_read_input_tokens");
            let cache_create = num("cache_creation_input_tokens");

            metrics.input_tokens += input_t;
            metrics.output_tokens += output_t;
            metrics.cache_read_tokens += cache_read;
            metrics.cache_create_tokens += cache_create;

            if let Some(prev) = prev_context_size.as_deref_mut() {
                if !py_truthy(obj.get("isSidechain")) {
                    let context_size = input_t + cache_read + cache_create;
                    if context_size > 0 {
                        if let Some(before) = *prev {
                            if before > 0 && (context_size as f64) / (before as f64) < 0.5 {
                                metrics.compaction_count += 1;
                            }
                        }
                        *prev = Some(context_size);
                    }
                }
            }
            None
        }
        _ => None,
    }
}

/// Batch-classify the collected user texts: anything the one shape
/// classifier reads as framed (header, legacy tag, cross-session relay) is
/// a mail turn, a bare turn is the operator.
fn count_user_vs_mail(metrics: &mut SessionMetrics, texts: &[String]) {
    for text in texts {
        let is_mail = crate::mail_header::classify(text) != crate::mail_header::Framing::Bare;
        metrics.mail_messages += is_mail as u64;
        metrics.user_messages += (!is_mail) as u64;
    }
}

/// An entry stamp as epoch seconds under the ported parse: RFC 3339 with
/// `Z` or an offset normalizes to UTC, a naive stamp reads as UTC, a
/// date-only stamp reads as midnight UTC. `None` when missing or junk.
fn entry_ts_epoch(obj: &Value) -> Option<f64> {
    let raw = obj.get("timestamp").and_then(Value::as_str)?.trim();
    if raw.is_empty() {
        return None;
    }
    let stripped = raw.trim_end_matches('Z').trim_end();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(stripped) {
        return Some(dt.timestamp_millis() as f64 / 1000.0);
    }
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(stripped, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(nd.and_utc().timestamp_millis() as f64 / 1000.0);
    }
    let date_only = chrono::NaiveDate::parse_from_str(stripped, "%Y-%m-%d").ok()?;
    Some(date_only.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis() as f64 / 1000.0)
}

/// `--since` under the same parse and normalize the entries get.
fn parse_since(raw: &str) -> Option<f64> {
    let stripped = raw.trim_end_matches('Z').trim_end();
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(stripped) {
        return Some(dt.timestamp_millis() as f64 / 1000.0);
    }
    if let Ok(nd) = chrono::NaiveDateTime::parse_from_str(stripped, "%Y-%m-%dT%H:%M:%S%.f") {
        return Some(nd.and_utc().timestamp_millis() as f64 / 1000.0);
    }
    Some(
        chrono::NaiveDate::parse_from_str(stripped, "%Y-%m-%d")
            .ok()?
            .and_hms_opt(0, 0, 0)?
            .and_utc()
            .timestamp_millis() as f64
            / 1000.0,
    )
}

fn parse_transcript(
    path: &Path,
    session_id: &str,
    branch_filter: Option<&str>,
    since: Option<f64>,
    seen: &mut HashSet<DedupKey>,
) -> SessionMetrics {
    let mut metrics = SessionMetrics::new(session_id);
    let mut prev_context_size: Option<u64> = None;
    let mut skipped_lines = 0u64;
    let mut unparseable_ts_count = 0u64;
    let mut user_texts: Vec<String> = Vec::new();
    let Ok(file) = std::fs::File::open(path) else {
        return metrics;
    };
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(obj) = serde_json::from_slice::<Value>(&line) else {
            skipped_lines += 1;
            continue;
        };
        let git_branch = obj
            .get("gitBranch")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if let Some(filter) = branch_filter {
            if !git_branch.contains(filter) && git_branch != filter {
                continue;
            }
        }
        if let Some(bound) = since {
            match entry_ts_epoch(&obj) {
                None => {
                    if py_truthy(obj.get("timestamp")) {
                        unparseable_ts_count += 1;
                    }
                    continue;
                }
                Some(ts) if ts < bound => continue,
                Some(_) => {}
            }
        }
        if let Some(text) = accumulate_entry(&obj, &mut metrics, Some(&mut prev_context_size), seen)
        {
            user_texts.push(text);
        }
    }
    count_user_vs_mail(&mut metrics, &user_texts);
    if skipped_lines > 0 {
        eprintln!(
            "Warning: {skipped_lines} malformed lines in {}",
            path.display()
        );
    }
    if unparseable_ts_count > 0 {
        eprintln!(
            "Warning: {unparseable_ts_count} entries with unparseable timestamps skipped under --since in {}",
            path.display()
        );
    }
    price_metrics(&mut metrics);
    metrics
}

/// Price through the one in-process price leg: models.dev rates keyed on
/// the primary model, no route. An unpriced session reads `cost_usd: null`
/// with its reason, never a fallback-tier guess.
fn price_metrics(metrics: &mut SessionMetrics) {
    if metrics.models.is_empty() {
        metrics.unpriced = true;
        return;
    }
    let Some(book) = crate::model_price::price_book(&crate::model_price::state_dir()) else {
        metrics.unpriced = true;
        return;
    };
    let Some(rates) = book.rates(&metrics.primary_model(), None) else {
        metrics.unpriced = true;
        return;
    };
    let tokens = crate::session_activity::Tokens {
        input: metrics.input_tokens,
        output: metrics.output_tokens,
        cache_read: metrics.cache_read_tokens,
        cache_write: metrics.cache_create_tokens,
    };
    match crate::model_price::cost(tokens, &rates) {
        Some(dollars) => metrics.cost_usd = dollars,
        None => metrics.unpriced = true,
    }
}

/// Parse a transcript and return metrics grouped by gitBranch, in
/// first-seen order. One dedup set across all branches: every content-block
/// line of an API message carries the same gitBranch, so the message lands
/// on exactly one branch and is counted once.
fn get_branch_breakdown(path: &Path, session_id: &str) -> Vec<(String, SessionMetrics)> {
    let mut order: Vec<String> = Vec::new();
    let mut branches: std::collections::HashMap<String, SessionMetrics> =
        std::collections::HashMap::new();
    let mut skipped_lines = 0u64;
    let mut branch_texts: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut seen: HashSet<DedupKey> = HashSet::new();
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(obj) = serde_json::from_slice::<Value>(&line) else {
            skipped_lines += 1;
            continue;
        };
        let git_branch = obj
            .get("gitBranch")
            .and_then(Value::as_str)
            .filter(|b| !b.is_empty())
            .unwrap_or("unknown")
            .to_string();
        if !branches.contains_key(&git_branch) {
            order.push(git_branch.clone());
            branches.insert(
                git_branch.clone(),
                SessionMetrics::new(&format!("{session_id}:{git_branch}")),
            );
        }
        let metrics = branches.get_mut(&git_branch).expect("just inserted");
        if let Some(text) = accumulate_entry(&obj, metrics, None, &mut seen) {
            branch_texts
                .entry(git_branch.clone())
                .or_default()
                .push(text);
        }
    }
    for (branch, texts) in &branch_texts {
        count_user_vs_mail(branches.get_mut(branch).expect("seen branch"), texts);
    }
    if skipped_lines > 0 {
        eprintln!(
            "Warning: {skipped_lines} malformed lines in {}",
            path.display()
        );
    }
    order
        .into_iter()
        .filter_map(|b| branches.remove(&b).map(|m| (b, m)))
        .map(|(b, mut m)| {
            price_metrics(&mut m);
            (b, m)
        })
        .collect()
}

/// Merge multiple session metrics into one combined view. The combined
/// session id joins each id's first 12 chars; timestamps take the min and
/// max over the nonempty stamps.
fn merge_metrics(all: &[SessionMetrics]) -> SessionMetrics {
    if all.len() == 1 {
        return all[0].clone();
    }
    let mut combined = SessionMetrics::new(
        &all.iter()
            .map(|m| m.session_id.chars().take(12).collect::<String>())
            .collect::<Vec<_>>()
            .join(", "),
    );
    for m in all {
        combined.merge(m);
    }
    combined.first_timestamp = all
        .iter()
        .map(|m| m.first_timestamp.clone())
        .filter(|t| !t.is_empty())
        .min()
        .unwrap_or_default();
    combined.last_timestamp = all
        .iter()
        .map(|m| m.last_timestamp.clone())
        .filter(|t| !t.is_empty())
        .max()
        .unwrap_or_default();
    combined
}

/// Python `round(x, n)` as a value: half-to-even at the digit, so the JSON
/// number a golden pins cannot drift on a price that lands exactly on a
/// half. The JSON writer's float printer (shortest round-trip) then matches
/// Python's repr, `0.0` included.
pub(crate) fn py_round(value: f64, digits: u32) -> f64 {
    let factor = 10f64.powi(digits as i32);
    let scaled = value * factor;
    let floor = scaled.floor();
    let diff = scaled - floor;
    let rounded = match diff.partial_cmp(&0.5) {
        Some(std::cmp::Ordering::Less) => floor,
        Some(std::cmp::Ordering::Equal) => {
            if (floor as i64) % 2 == 0 {
                floor
            } else {
                floor + 1.0
            }
        }
        _ => floor + 1.0,
    };
    rounded / factor
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn metrics_json(metrics: &SessionMetrics) -> Value {
    let mut models = serde_json::Map::new();
    for (model, count) in &metrics.models {
        models.insert(model.clone(), json!(count));
    }
    let mut payload = json!({
        "session_id": metrics.session_id,
        "cost_usd": if metrics.unpriced {
            Value::Null
        } else {
            json!(py_round(metrics.cost_usd, 2))
        },
        "tokens": {
            "input": metrics.input_tokens,
            "output": metrics.output_tokens,
            "cache_read": metrics.cache_read_tokens,
            "cache_create": metrics.cache_create_tokens,
            "total": metrics.total_tokens(),
        },
        "messages": {
            "user": metrics.user_messages,
            "mail": metrics.mail_messages,
            "assistant": metrics.assistant_messages,
            "subagent": metrics.subagent_messages,
        },
        "compactions": metrics.compaction_count,
        "duration_minutes": py_round(metrics.duration_minutes(), 1),
        "primary_model": metrics.primary_model(),
        "models": models,
    });
    if metrics.unpriced {
        payload["unpriced_model"] = json!(metrics.primary_model());
    }
    payload
}

fn print_metrics(metrics: &SessionMetrics, as_json: bool) {
    if as_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&metrics_json(metrics)).unwrap_or_default()
        );
        return;
    }
    let total = metrics.total_tokens();
    println!("{}", "─".repeat(50));
    println!("  Session:      {}", metrics.session_id);
    println!("  Model:        {}", metrics.primary_model());
    println!("  Duration:     {:.0} min", metrics.duration_minutes());
    if metrics.unpriced {
        println!("  Cost:         unpriced ({})", metrics.primary_model());
    } else {
        println!("  Cost:         ${:.2}", metrics.cost_usd);
    }
    println!("{}", "─".repeat(50));
    println!(
        "  Input:        {:>10}",
        format_tokens(metrics.input_tokens)
    );
    println!(
        "  Output:       {:>10}",
        format_tokens(metrics.output_tokens)
    );
    println!(
        "  Cache read:   {:>10}",
        format_tokens(metrics.cache_read_tokens)
    );
    println!(
        "  Cache create: {:>10}",
        format_tokens(metrics.cache_create_tokens)
    );
    println!("  Total:        {:>10}", format_tokens(total));
    println!("{}", "─".repeat(50));
    let mut msg_summary = format!(
        "  Messages:     {} assistant, {} user",
        metrics.assistant_messages, metrics.user_messages
    );
    if metrics.mail_messages > 0 {
        msg_summary.push_str(&format!(", {} mail", metrics.mail_messages));
    }
    println!("{msg_summary}");
    if metrics.subagent_messages > 0 {
        println!("  Subagents:    {}", metrics.subagent_messages);
    }
    println!("  Compactions:  {}", metrics.compaction_count);
    println!("{}", "─".repeat(50));
}

/// Recalculate costs for every ledger entry with branch-level attribution.
/// One dedup set per entry (the logical sum): resumed sessions copy history
/// lines into new transcript files, so per-file dedup alone would re-count
/// history across the entry's sessions.
fn backfill_tasks_json(dry_run: bool) -> i32 {
    let ledger = crate::paths_cli::ledger_json(&std::env::current_dir().unwrap_or_default());
    if !ledger.exists() {
        println!("No ledger.json found at {}", ledger.display());
        return 0;
    }
    let Ok(raw) = std::fs::read_to_string(&ledger) else {
        eprintln!("Error: ledger.json is unreadable: {}", ledger.display());
        return 1;
    };
    let Ok(mut data) = serde_json::from_str::<Value>(&raw) else {
        eprintln!("Error: ledger.json is corrupt: parse failed");
        return 1;
    };
    // An object without an entries key reads as an empty ledger, the same
    // `data.get("entries", [])` default the ported leg answered; the apply
    // write below then stores the document back unchanged.
    let mut fallback: Vec<Value> = Vec::new();
    let entries: &mut Vec<Value> = match data.as_array_mut() {
        Some(list) => list,
        None => match data.get_mut("entries").and_then(Value::as_array_mut) {
            Some(list) => list,
            None => &mut fallback,
        },
    };
    for entry in entries.iter_mut() {
        let sessions = entry
            .get("sessions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let branch = entry
            .get("branch")
            .and_then(Value::as_str)
            .map(str::to_string);
        let mut all_metrics: Vec<SessionMetrics> = Vec::new();
        let mut entry_seen: HashSet<DedupKey> = HashSet::new();
        for sid_value in &sessions {
            let Some(sid) = sid_value.as_str() else {
                continue;
            };
            let Some(path) = crate::claude_drive::find_transcript(sid) else {
                continue;
            };
            let mut m = None;
            if let Some(branch) = &branch {
                let mut trial_seen = entry_seen.clone();
                let trial = parse_transcript(&path, sid, Some(branch), None, &mut trial_seen);
                if trial.assistant_messages > 0 {
                    m = Some(trial);
                    entry_seen = trial_seen;
                }
            }
            let metrics =
                m.unwrap_or_else(|| parse_transcript(&path, sid, None, None, &mut entry_seen));
            all_metrics.push(metrics);
        }
        if all_metrics.is_empty() {
            println!(
                "  SKIP: {} — no transcripts found",
                entry
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("Untitled")
            );
            continue;
        }
        let combined = merge_metrics(&all_metrics);
        let attribution = match &branch {
            Some(name) => format!("branch:{name}"),
            None => "full session".to_string(),
        };
        println!(
            "\n  {}",
            entry
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Untitled")
        );
        println!("    Attribution: {attribution}");
        println!(
            "    Cost: ${:.2} (was: ${:.2})",
            combined.cost_usd,
            entry.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0)
        );
        println!(
            "    Tokens: {} | Compactions: {}",
            format_tokens(combined.total_tokens()),
            combined.compaction_count
        );
        if !dry_run {
            entry["cost_usd"] = if combined.unpriced {
                Value::Null
            } else {
                json!(py_round(combined.cost_usd, 2))
            };
            entry["tokens_total"] = json!(combined.total_tokens());
            entry["cache_read_tokens"] = json!(combined.cache_read_tokens);
            entry["compactions"] = json!(combined.compaction_count);
        }
    }
    if dry_run {
        println!("\n  Dry run — no changes written. Use --backfill without --dry-run to apply.");
        return 0;
    }
    // Atomic write: temp file then rename, the ledger dir as the temp home.
    let tmp_path = ledger.with_extension("json.tmp");
    let Ok(serialized) = serde_json::to_string_pretty(&data) else {
        eprintln!("Error: ledger.json rewrite failed to serialize");
        return 1;
    };
    if std::fs::write(&tmp_path, format!("{serialized}\n")).is_err() {
        eprintln!("Error: could not write the ledger temp file");
        return 1;
    }
    if std::fs::rename(&tmp_path, &ledger).is_err() {
        let _ = std::fs::remove_file(&tmp_path);
        eprintln!("Error: could not replace ledger.json");
        return 1;
    }
    println!("\n  Updated {}", ledger.display());
    0
}

/// Print a cost breakdown by provider_id from the ledger. Entries without a
/// provider_id bucket under "unattributed"; the bucket sorts last.
fn print_by_provider() -> i32 {
    let ledger = crate::paths_cli::ledger_json(&std::env::current_dir().unwrap_or_default());
    if !ledger.exists() {
        println!("No ledger.json found at {}", ledger.display());
        return 0;
    }
    let Ok(raw) = std::fs::read_to_string(&ledger) else {
        eprintln!("Error: ledger.json is unreadable");
        return 1;
    };
    let Ok(data) = serde_json::from_str::<Value>(&raw) else {
        eprintln!("Error: ledger.json is corrupt: parse failed");
        return 1;
    };
    let entries = match &data {
        Value::Array(items) => items.clone(),
        Value::Object(o) => o
            .get("entries")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut buckets: Vec<(String, usize, f64)> = Vec::new();
    let mut total_cost = 0.0f64;
    for entry in &entries {
        let key = entry
            .get("provider_id")
            .and_then(Value::as_str)
            .unwrap_or("unattributed");
        let cost = entry.get("cost_usd").and_then(Value::as_f64).unwrap_or(0.0);
        total_cost += cost;
        if let Some(bucket) = buckets.iter_mut().find(|(k, _, _)| k == key) {
            bucket.1 += 1;
            bucket.2 += cost;
        } else {
            buckets.push((key.to_string(), 1, cost));
        }
    }
    buckets.sort_by(|a, b| {
        let ka: &str = if a.0 == "unattributed" {
            "\u{ff}"
        } else {
            &a.0
        };
        let kb: &str = if b.0 == "unattributed" {
            "\u{ff}"
        } else {
            &b.0
        };
        ka.cmp(kb)
    });
    let count = entries.len();
    println!("{}", "─".repeat(60));
    println!("  Provider cost breakdown ({count} entries total)");
    println!("  {:<35} {:>7} {:>12}", "Provider", "Entries", "Cost");
    println!("  {}", "─".repeat(55));
    for (key, count, cost) in &buckets {
        let pct = if total_cost > 0.0 {
            cost / total_cost * 100.0
        } else {
            0.0
        };
        println!("  {key:<35} {count:>7} ${cost:>9.2} ({pct:.0}%)");
    }
    println!("  {}", "─".repeat(55));
    println!("  {:<35} {:>7} ${:>9.2}", "TOTAL", count, total_cost);
    println!("  {}", "─".repeat(60));
    0
}

/// The `fno whoami cost` verb's native body, reached as
/// `fno-agents context-run --session-cost [args]`. Flag names, modes, and
/// exit codes match the argparse CLI the Python leg answered; the JSON
/// contract (`--json`) is the byte-level one the doctor check reads.
/// The `--json <uuid>` leg as a library call, for finalize's two in-process
/// consumers (the ledger row and the handoff header): find the transcript,
/// parse, price, return the metrics object. `None` when no transcript
/// resolves - the caller records cost=null, never a guess.
pub(crate) fn cost_json_for_transcript(transcript_uuid: &str) -> Option<Value> {
    let path = match crate::claude_drive::find_transcript(transcript_uuid) {
        Some(p) => p,
        None => {
            eprintln!("Warning: transcript not found for {transcript_uuid}");
            return None;
        }
    };
    let mut seen: HashSet<DedupKey> = HashSet::new();
    let metrics = parse_transcript(&path, transcript_uuid, None, None, &mut seen);
    Some(metrics_json(&metrics))
}

pub fn run_session_cost(args: &[String]) -> i32 {
    let mut backfill = false;
    let mut branches_mode = false;
    let mut by_provider = false;
    let mut as_json = false;
    let mut dry_run = false;
    let mut branch_filter: Option<String> = None;
    let mut since: Option<f64> = None;
    let mut session_ids: Vec<String> = Vec::new();
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--backfill" => backfill = true,
            "--branches" => branches_mode = true,
            "--by-provider" => by_provider = true,
            "--json" => as_json = true,
            "--dry-run" => dry_run = true,
            "--branch" => match rest.next() {
                Some(v) => branch_filter = Some(v.to_string()),
                None => return usage_error("--branch needs a value"),
            },
            "--since" => match rest.next() {
                Some(v) => match parse_since(v) {
                    Some(epoch) => since = Some(epoch),
                    None => {
                        eprintln!("--since must be ISO8601 (e.g. 2026-05-13T22:00:00Z): {v:?}");
                        return 2;
                    }
                },
                None => return usage_error("--since needs a value"),
            },
            "-h" | "--help" => {
                print_usage();
                return 0;
            }
            other => {
                if other.starts_with('-') && other.len() > 1 {
                    eprintln!("unrecognized arguments: {other}");
                    return 2;
                }
                session_ids.push(other.to_string());
            }
        }
    }
    if backfill {
        return backfill_tasks_json(dry_run);
    }
    if by_provider {
        return print_by_provider();
    }
    if session_ids.is_empty() {
        return usage_error("provide at least one session ID");
    }
    if branches_mode {
        for sid in &session_ids {
            let Some(path) = crate::claude_drive::find_transcript(sid) else {
                eprintln!("Warning: transcript not found for {sid}");
                continue;
            };
            print_branch_breakdown(&path, sid);
        }
        return 0;
    }
    let mut shared_seen: HashSet<DedupKey> = HashSet::new();
    let mut all_metrics: Vec<SessionMetrics> = Vec::new();
    for sid in &session_ids {
        let Some(path) = crate::claude_drive::find_transcript(sid) else {
            eprintln!("Warning: transcript not found for {sid}");
            continue;
        };
        all_metrics.push(parse_transcript(
            &path,
            sid,
            branch_filter.as_deref(),
            since,
            &mut shared_seen,
        ));
    }
    if all_metrics.is_empty() {
        eprintln!("Error: no transcripts found");
        return 1;
    }
    if all_metrics.len() == 1 {
        print_metrics(&all_metrics[0], as_json);
        return 0;
    }
    if !as_json {
        for m in &all_metrics {
            print_metrics(m, false);
            println!();
        }
        println!("COMBINED:");
    }
    let combined = merge_metrics(&all_metrics);
    print_metrics(&combined, as_json);
    0
}

fn usage_error(message: &str) -> i32 {
    eprintln!("{message}");
    2
}

fn print_usage() {
    println!(
        "usage: fno-agents context-run --session-cost [--json] [--branch BRANCH] [--since ISO8601] [--branches] [--by-provider] [--backfill [--dry-run]] [SESSION_ID ...]"
    );
}

/// Print the per-branch cost table for one session.
fn print_branch_breakdown(path: &Path, session_id: &str) {
    let branches = get_branch_breakdown(path, session_id);
    let total_cost: f64 = branches.iter().map(|(_, m)| m.cost_usd).sum();
    println!("{}", "─".repeat(60));
    println!(
        "  Branch breakdown for {}...",
        &session_id[..12.min(session_id.len())]
    );
    println!("{}", "─".repeat(60));
    println!("  {:<45} {:>5} {:>10}", "Branch", "Msgs", "Cost");
    println!("  {}", "─".repeat(55));
    let mut ranked = branches;
    ranked.sort_by(|a, b| {
        b.1.cost_usd
            .partial_cmp(&a.1.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    for (branch_name, m) in &ranked {
        let pct = if total_cost > 0.0 {
            m.cost_usd / total_cost * 100.0
        } else {
            0.0
        };
        println!(
            "  {branch_name:<45} {:>5} ${:>8.2} ({pct:.0}%)",
            m.assistant_messages, m.cost_usd
        );
    }
    let total_msgs: u64 = ranked.iter().map(|(_, m)| m.assistant_messages).sum();
    println!("  {:<45} {:>5} ${:>8.2}", "TOTAL", total_msgs, total_cost);
    println!("{}", "─".repeat(60));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(pid: u32, ppid: u32, command: &str, rss_kb: u64) -> ProcRow {
        ProcRow {
            pid,
            ppid,
            state: 'S',
            elapsed_s: 1,
            cpu_pct: 1.0,
            rss_kb,
            command: command.into(),
        }
    }

    #[test]
    fn attributes_roots_and_descendants_without_crossing_cycles() {
        let mut table = vec![
            row(100, 1, "session", 1024),
            row(201, 100, "cargo test", 2048),
            row(202, 201, "rustc", 1024),
            row(300, 1, "fseventsd", 28 * 1024 * 1024),
        ];
        table[2].cpu_pct = 3.0;
        let roots = vec![SessionRoot {
            session_id: "a".into(),
            harness: "claude".into(),
            name: Some("worker-a".into()),
            node: Some("x-a".into()),
            pids: vec![100],
        }];
        let phases = &[PhaseRow {
            session_id: "a".into(),
            node: Some("x-a".into()),
            stage: Some("blueprint".into()),
        }];
        let no_pids: HashSet<u32> = HashSet::new();
        let out = attribute(&table, &roots, phases, &no_pids, &no_pids);
        assert_eq!(out.sessions.len(), 1);
        assert_eq!(out.sessions[0]["stage"], "blueprint");
        assert_eq!(out.sessions[0]["name"], "worker-a");
        assert_eq!(out.sessions[0]["top_command"], "rustc");
        assert_eq!(out.sessions[0]["buckets"]["cargo"]["procs"], 2);
        assert_eq!(out.top_rss[0]["name"], "fseventsd");

        // AC1-HP: a live cargo-run claim whose recorded pid (201) descends
        // from the session root marks that session `test` over the phase row.
        let running: HashSet<u32> = [201u32].into_iter().collect();
        let out = attribute(&table, &roots, phases, &running, &no_pids);
        assert_eq!(out.sessions[0]["stage"], "test");

        // AC1-QUEUE: a queued ticket pid owned by the root marks the session
        // `test-queued` while the running set stays empty.
        let queued: HashSet<u32> = [100u32].into_iter().collect();
        let out = attribute(&table, &roots, phases, &no_pids, &queued);
        assert_eq!(out.sessions[0]["stage"], "test-queued");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn footprint_mb_reads_the_live_process_footprint() {
        let pid = std::process::id();
        let mut info: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(
                pid as libc::c_int,
                libc::RUSAGE_INFO_V4,
                (&mut info as *mut libc::rusage_info_v4).cast(),
            )
        };
        assert_eq!(rc, 0);
        let direct = info.ri_phys_footprint as f64 / 1024.0 / 1024.0;
        let (footprint, unread) = footprint_mb(&[pid]);
        let footprint = footprint.expect("macOS reads a footprint");
        assert_eq!(unread, 0);
        assert!(
            footprint > direct / 4.0 && footprint < direct * 4.0,
            "footprint_mb read {footprint} MB, a direct read says {direct} MB"
        );
    }

    /// The ported `fno.cost._session_cost` parse, characterized end to end on
    /// a small transcript: usage accumulation, (message id, requestId) dedup,
    /// subagent and user-vs-mail classification, the context-size halving
    /// compaction rule, and the duration bookends.
    #[test]
    fn parse_transcript_accumulates_dedups_and_classifies() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let a1 = json!({
            "type": "assistant", "uuid": "u1", "requestId": "r1",
            "timestamp": "2026-06-07T10:00:00.000Z",
            "message": {"id": "msg_1", "model": "m1",
                        "usage": {"input_tokens": 100, "output_tokens": 10,
                                  "cache_read_input_tokens": 200,
                                  "cache_creation_input_tokens": 50}},
        });
        let dup = json!({
            "type": "assistant", "uuid": "u2", "requestId": "r1",
            "timestamp": "2026-06-07T10:01:00.000Z",
            "message": {"id": "msg_1", "model": "m1",
                        "usage": {"input_tokens": 100, "output_tokens": 10,
                                  "cache_read_input_tokens": 200,
                                  "cache_creation_input_tokens": 50}},
        });
        let side = json!({
            "type": "assistant", "uuid": "u3", "requestId": "r2",
            "timestamp": "2026-06-07T10:02:00.000Z", "isSidechain": true,
            "message": {"id": "msg_2", "model": "m1",
                        "usage": {"input_tokens": 900, "output_tokens": 5,
                                  "cache_read_input_tokens": 0,
                                  "cache_creation_input_tokens": 0}},
        });
        let halve = json!({
            "type": "assistant", "uuid": "u4", "requestId": "r3",
            "timestamp": "2026-06-07T10:03:00.000Z",
            "message": {"id": "msg_3", "model": "m2",
                        "usage": {"input_tokens": 40, "output_tokens": 5,
                                  "cache_read_input_tokens": 60,
                                  "cache_creation_input_tokens": 0}},
        });
        let user_turn = json!({
            "type": "user", "uuid": "u5", "timestamp": "2026-06-07T10:04:00.000Z",
            "message": {"content": "please look at this"},
        });
        let mail_turn = json!({
            "type": "user", "uuid": "u6", "timestamp": "2026-06-07T10:05:00.000Z",
            "message": {"content": "<fno_mail from=\"lead\">body</fno_mail>"},
        });
        let mut body = String::new();
        for line in [&a1, &dup, &side, &halve, &user_turn, &mail_turn] {
            body.push_str(&line.to_string());
            body.push('\n');
        }
        std::fs::write(&path, body).unwrap();

        let mut seen: HashSet<DedupKey> = HashSet::new();
        let metrics = parse_transcript(&path, "sid-x", None, None, &mut seen);
        // The duplicate content-block line skipped its usage and its count.
        assert_eq!(metrics.assistant_messages, 3, "deduped msg_1 twice-counted");
        assert_eq!(metrics.input_tokens, 100 + 900 + 40);
        assert_eq!(metrics.output_tokens, 10 + 5 + 5);
        assert_eq!(metrics.cache_read_tokens, 200 + 0 + 60);
        assert_eq!(metrics.cache_create_tokens, 50 + 0 + 0);
        assert_eq!(metrics.subagent_messages, 1);
        assert_eq!(metrics.user_messages, 1);
        assert_eq!(metrics.mail_messages, 1);
        // 360 (100+200+50) -> 100 (40+60) is a >2x drop: one compaction.
        assert_eq!(metrics.compaction_count, 1);
        assert_eq!(metrics.primary_model(), "m1");
        // 10:00 to 10:05.
        let out = metrics_json(&metrics);
        assert_eq!(out["duration_minutes"], json!(5.0));
        // Unpriced model: null cost, never a fallback tier.
        assert_eq!(out["cost_usd"], serde_json::Value::Null);
        assert_eq!(out["unpriced_model"], "m1");
    }
}
