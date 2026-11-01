//! The silence sweep: a worker past its cadence deadline is a report, never an act.
//!
//! The refusal path catches a worker whose own last turn carries a marker the
//! taxonomy recognises. This catches the rest: a harness can reword its refusal
//! tomorrow, and a cap can arrive as a hang, so only a clock closes that gap.
//!
//! Three constraints. It reads the full registry, because recovery's candidate
//! set drops every non-claude row, so a codex successor would be invisible to the
//! sweep that would catch its cap. It never acts: a wrong report is a false alarm,
//! a wrong act is lost work. An unknowable age emits nothing: absence of evidence
//! must not become a finding.

use crate::backlog::worked::node_id_from_worktree;
use crate::paths::AgentsHome;
use crate::state::RegistryEntry;
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

const DEFAULT_SILENCE_DEADLINE_S: i64 = 600;
/// Resolving a worker's transcript truth takes minutes on a large roster, and this
/// sweep runs on a bounded tick. Rows past the budget report unread.
pub(crate) const DEFAULT_SWEEP_BUDGET_S: f64 = 20.0;

/// One worker's verdict. Healthy rows are included on purpose, so an empty answer
/// cannot pass a gate that only greps for a key on the unhappy path.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SweepRow {
    pub handle: String,
    pub harness: String,
    pub deadline_s: i64,
    pub age_s: Option<i64>,
    pub silent: bool,
    pub node: Option<String>,
    pub last_message: Option<String>,
    pub state: Option<String>,
    /// Named rather than dropped: a row missing from the answer reads as a fleet
    /// with fewer workers.
    pub unread: bool,
}

fn configured_deadline() -> i64 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::config_lookup(&cwd, &["agents", "silence_deadline_seconds"])
        .and_then(|v| v.as_integer())
        .filter(|v| *v > 0)
        .unwrap_or(DEFAULT_SILENCE_DEADLINE_S)
}

/// One verdict per registered worker. Reads only. `truth_for` answers a row's
/// truth payload; rows past `budget_s` come back unread with no age.
pub(crate) fn sweep_rows(
    entries: &[RegistryEntry],
    deadline_s: i64,
    budget_s: f64,
    truth_for: &dyn Fn(&RegistryEntry) -> Value,
) -> Vec<SweepRow> {
    let started = Instant::now();
    let mut rows = Vec::with_capacity(entries.len());
    for entry in entries {
        let harness = entry.harness.clone().unwrap_or_default();
        if budget_s > 0.0 && started.elapsed().as_secs_f64() > budget_s {
            rows.push(SweepRow {
                handle: entry.name.clone(),
                harness,
                deadline_s,
                age_s: None,
                silent: false,
                node: None,
                last_message: None,
                state: Some("not-read".to_string()),
                unread: true,
            });
            continue;
        }
        let truth = truth_for(entry);
        let age_s = truth.get("last_activity_age_s").and_then(Value::as_i64);
        let field = |key: &str| truth.get(key).and_then(Value::as_str).map(str::to_string);
        rows.push(SweepRow {
            handle: entry.name.clone(),
            harness,
            deadline_s,
            age_s,
            // An unknowable age is never silent: it means the instrument did not
            // read, not that the worker was quiet.
            silent: age_s.is_some_and(|age| age > deadline_s),
            node: if entry.cwd.is_empty() {
                None
            } else {
                node_id_from_worktree(&entry.cwd)
            },
            last_message: field("last_message"),
            state: field("state"),
            unread: false,
        });
    }
    rows
}

/// `(rows, silent_count)`, emitting `worker_silent` on the transition. `dedup`
/// keeps the emit once per silent spell: a per-tick emit is six events an hour per
/// worker forever, which is not a "something changed" signal.
pub(crate) fn run_sweep(
    deadline_s: Option<i64>,
    budget_s: f64,
    source: &str,
    dedup: bool,
) -> (Vec<SweepRow>, i64) {
    let deadline_s = deadline_s.unwrap_or_else(configured_deadline);
    let home = AgentsHome::from_env();
    let entries = crate::state::load_registry(&home.registry_json())
        .map(|r| r.entries)
        .unwrap_or_default();
    let stores = crate::session_truth::Stores::ambient();
    let now_s = crate::watchdog::sweep::now_epoch_s();
    // Resolve the row to itself: the default resolver's discovery scan costs about
    // ten seconds per row on a real roster.
    let truth_for = |entry: &RegistryEntry| -> Value {
        let one = std::slice::from_ref(entry);
        crate::session_truth::resolve_payload(
            Some(one),
            &entry.name,
            now_s,
            &stores,
            crate::session_truth::cursor::global(),
        )
    };
    let rows = sweep_rows(&entries, deadline_s, budget_s, &truth_for);
    let seen: BTreeSet<String> = if dedup {
        silent_seen()
    } else {
        BTreeSet::new()
    };
    let emitter = crate::events::EventEmitter::new(
        crate::watchdog::sweep::state_root().join("events.jsonl"),
        source,
    );
    let mut silent = 0i64;
    let mut now_silent: BTreeSet<String> = BTreeSet::new();
    for row in rows.iter().filter(|r| r.silent) {
        silent += 1;
        now_silent.insert(row.handle.clone());
        if seen.contains(&row.handle) {
            continue;
        }
        // A lost event never breaks a read-only sweep.
        let _ = emitter.emit(
            "worker_silent",
            &json!({
                "handle": row.handle,
                "harness": row.harness,
                "age_s": row.age_s,
                "deadline_s": row.deadline_s,
                "node": row.node,
                "last_message": row.last_message,
            }),
        );
    }
    if dedup {
        // Only rows the sweep read can leave the set: a truncated sweep would
        // otherwise re-report every worker it did not reach next tick.
        let mut keep = now_silent;
        for row in rows.iter().filter(|r| r.unread) {
            if seen.contains(&row.handle) {
                keep.insert(row.handle.clone());
            }
        }
        crate::fleet_state::set_silent_seen(&keep);
    }
    (rows, silent)
}

fn silent_seen() -> BTreeSet<String> {
    // No memory means report, never suppress.
    crate::fleet_state::silent_seen()
}

/// `fno agents sweep`, nested under the census action.
pub fn run(args: &[String]) -> i32 {
    let mut json_out = false;
    let mut deadline: Option<i64> = None;
    let mut budget = DEFAULT_SWEEP_BUDGET_S;
    let mut tick = false;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" | "-J" => json_out = true,
            "--tick" => tick = true,
            "--deadline" => {
                index += 1;
                match args.get(index).and_then(|v| v.parse().ok()) {
                    Some(value) => deadline = Some(value),
                    None => {
                        eprintln!("fno agents sweep: --deadline needs whole seconds");
                        return 2;
                    }
                }
            }
            "--budget" => {
                index += 1;
                match args.get(index).and_then(|v| v.parse().ok()) {
                    Some(value) => budget = value,
                    None => {
                        eprintln!("fno agents sweep: --budget needs seconds");
                        return 2;
                    }
                }
            }
            other => {
                eprintln!("fno agents sweep: unknown argument {other}");
                return 2;
            }
        }
        index += 1;
    }
    // A hand-run report is not a daemon observation, and the dedup memo belongs to
    // the daemon's cadence: a human running this twice wants two answers.
    let (source, dedup) = if tick {
        ("daemon", true)
    } else {
        ("cli", false)
    };
    let (rows, silent) = run_sweep(deadline, budget, source, dedup);
    if tick {
        println!("{}", json!({"silent": silent, "rows": rows.len()}));
        return 0;
    }
    use std::io::IsTerminal as _;
    if json_out || !std::io::stdout().is_terminal() {
        println!(
            "{}",
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".to_string())
        );
        return 0;
    }
    if rows.is_empty() {
        println!("no registered workers");
        return 0;
    }
    for row in &rows {
        let age = row.age_s.map_or("unknown".to_string(), |a| format!("{a}s"));
        let mark = if row.silent { "SILENT" } else { "ok" };
        println!(
            "{}  [{}]  {mark:<6} age={age} deadline={}s",
            row.handle, row.harness, row.deadline_s
        );
    }
    // Never a silent cap: a truncation nobody can see reads as full coverage.
    let unread = rows.iter().filter(|r| r.unread).count();
    let tail = if unread > 0 {
        format!(", {unread} unread (budget)")
    } else {
        String::new()
    };
    println!("{silent} silent of {}{tail}", rows.len());
    0
}
