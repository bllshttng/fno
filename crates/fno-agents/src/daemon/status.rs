//! The `agent.status` read: registry counts plus the daemon's own vitals.
//! Moved out of daemon.rs (shrink-only) so the phase-2 op arms pay for their
//! dispatch lines with this move, and so status-side additions land here,
//! never back in the daemon file.

use super::*;
use std::sync::{Arc, Mutex};

/// The arms fold, cached against the file facts that change when a tick
/// lands: per journal, the live file's size+mtime, its store's size+mtime,
/// the store's WAL size, and the journal dir's mtime (rotation). A tick
/// landing changes at least one fact, so a hit is exactly "no tick landed
/// since the fold", and a miss recomputes once per tick interval.
static FOLD_STATE: Mutex<Option<FoldLive>> = Mutex::new(None);

/// The change-facts key: the home path first, so homes in tests never share
/// an entry, then one row per journal.
#[derive(PartialEq)]
struct FoldKey(Vec<(PathBuf, u64, u64, u64, u64, u64, u64)>);

/// One file's `(size, mtime_ns)`, or zeros when absent: an absent store
/// reads like an empty one, so a fold answering "nothing" keeps its key.
fn file_facts(p: &Path) -> (u64, u64) {
    match std::fs::metadata(p) {
        Ok(m) => (
            m.len(),
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as u64 * 1_000_000_000 + d.subsec_nanos() as u64)
                .unwrap_or(0),
        ),
        Err(_) => (0, 0),
    }
}

/// One journal family's change facts: (live size, live mtime, store size,
/// store mtime, WAL size, dir mtime).
fn journal_facts(j: &Path) -> (u64, u64, u64, u64, u64, u64) {
    let live = crate::event_store::live_journal(j);
    let store = crate::event_store::store_path(j);
    let wal = PathBuf::from(format!("{}-wal", store.display()));
    let dir = live.parent();
    let (ls, lm) = file_facts(&live);
    let (ss, sm) = file_facts(&store);
    let (ws, _) = file_facts(&wal);
    let (_, dm) = file_facts(dir.unwrap_or(j));
    (ls, lm, ss, sm, ws, dm)
}

/// The key for `home`: the home path row, then one facts row per journal.
fn fold_key(home: &AgentsHome) -> FoldKey {
    let mut facts: Vec<_> = vec![(home.root().to_path_buf(), 0, 0, 0, 0, 0, 0)];
    facts.extend(
        crate::tick_ledger::journals(home)
            .iter()
            .map(|j| {
                let (ls, lm, ss, sm, ws, dm) = journal_facts(j);
                (j.clone(), ls, lm, ss, sm, ws, dm)
            })
            .collect::<Vec<_>>(),
    );
    FoldKey(facts)
}

/// One home's live incremental fold: the file-facts key of the last fold,
/// the UTC day of the last fold (one full reseed per day bounds map drift),
/// the per-store tail boundaries, and the fold itself (`None` marks a
/// text-path home: no fold is served and readers fold locally).
struct FoldLive {
    home: PathBuf,
    key: FoldKey,
    day: u64,
    last_seqs: Vec<(PathBuf, i64)>,
    state: Option<crate::tick_ledger::ArmFoldState>,
}

/// The fold for this status read. A fast path answers from the state when
/// the file facts and the day match; a changed key folds only the tail rows
/// past the last folded seq; a new home seeds from the full windowed read.
/// Runs in `spawn_blocking`: the refresh is sqlite I/O, and the lock is held
/// across it so two concurrent readers share one refresh.
async fn fold_served_cached(home: AgentsHome) -> Option<Arc<crate::tick_ledger::ArmFoldServed>> {
    let home_path = home.root().to_path_buf();
    tokio::task::spawn_blocking(move || {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // Poison recovery is safe here: a panic mid-fold leaves the maps
        // partially refreshed, and the next refresh tail-folds from the last
        // recorded seq, so replay is idempotent (newest-wins). A poisoned
        // lock that quietly disabled the fold forever would be worse than a
        // loud recovery note.
        let mut guard = match FOLD_STATE.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                eprintln!("fno-agents: fold state lock recovered from poison");
                poisoned.into_inner()
            }
        };
        let journals = crate::tick_ledger::journals(&home);
        let day = now / 86_400;
        let key = fold_key(&home);
        match guard.as_mut() {
            // Unchanged facts and day: answer from the fold in memory.
            Some(live) if live.home == home_path && live.day == day && live.key == key => {}
            // Changed facts, same home: fold the tail rows past the boundary.
            Some(live) if live.home == home_path => {
                crate::tick_ledger::refresh_arm_fold(
                    live.state.as_mut()?,
                    &journals,
                    &mut live.last_seqs,
                    |j| crate::event_store::store_path(j),
                );
                live.key = key;
                live.day = day;
            }
            // New home (or first call): seed from the full windowed read. A
            // text-path home stores `None` once, so it pays no scan per read.
            // The key stays the pre-seed facts: a tick landing mid-seed
            // changes the files, so the next read tail-refreshes rather than
            // trusting a key newer than the fold it names.
            _ => {
                let seeded = crate::tick_ledger::seed_arm_fold(&journals, now);
                let (state, last_seqs) = match seeded {
                    Some(pair) => (Some(pair.0), pair.1),
                    None => (None, Vec::new()),
                };
                *guard = Some(FoldLive {
                    home: home_path.clone(),
                    key,
                    day,
                    last_seqs,
                    state,
                });
            }
        }
        let live = guard.as_ref()?;
        let state = live.state.as_ref()?;
        let names = journals
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        Some(Arc::new(crate::tick_ledger::served_from_state(
            state, names,
        )))
    })
    .await
    .ok()?
}

pub(super) async fn handle_status(ctx: &Ctx, req: &Request) -> Response {
    // load_registry does blocking flock I/O; offload it from the async worker
    // thread (Gemini review). The drive-table read below stays async. A read
    // failure is an RPC error: `unwrap_or_default()` here published
    // zero-agent status counts over a broken registry.
    let registry = match load_registry_offloaded(ctx.home.registry_json()).await {
        Ok(reg) => reg,
        Err(e) => return registry_read_failed(req.id, e),
    };
    let mut by_status: Map<String, Value> = Map::new();
    let mut restarting: u64 = 0;
    let mut channels_registered: u64 = 0;
    for e in &registry.entries {
        let key = format!("{:?}", e.status).to_lowercase();
        let n = by_status.get(&key).and_then(|v| v.as_u64()).unwrap_or(0) + 1;
        by_status.insert(key, Value::Number(n.into()));
        if e.status == AgentStatus::Restarting {
            restarting += 1;
        }
        if e.mcp_channel_id.is_some() {
            channels_registered += 1;
        }
    }
    let mut payload = json!({
            "schema_version": 1,
            "daemon": {
                "state": DaemonState::Serving.as_str(),
                "pid": std::process::id(),
                "uptime_secs": ctx.started_at.elapsed().as_secs(),
                "version": env!("CARGO_PKG_VERSION"),
                // Drift signal, additive. Null when the daemon
                // could not fingerprint itself; a client then reads Unknown.
                "exe_path": ctx
                    .exe_fingerprint
                    .as_ref()
                    .map(|f| f.path.to_string_lossy().into_owned()),
                "exe_mtime": ctx.exe_fingerprint.as_ref().map(|f| f.mtime_nanos),
                "exe_size": ctx.exe_fingerprint.as_ref().map(|f| f.size),
                // The daemon's own process start time, for the `restart`
                // pid-reuse guard.
                "pid_start_time": ctx.pid_start_time,
            },
            "agents": {
                "total": registry.entries.len(),
                "by_status": by_status,
            },
            "restarts": {
                // queue_depth tracks agents currently restarting; the full
                // restart queue + consecutive-failure history is not yet
                // surfaced in the served status (Wave 5), so the max-seen
                // counter reports 0 until that subsystem is wired into Ctx.
                "queue_depth": restarting,
                "consecutive_failures_max_seen": 0,
            },
            "channels": { "registered": channels_registered },
    });
    // The arms fold rides the status payload, cached: a fresh fold costs the
    // daemon one ~2 s blocking scan per tick interval; every other status
    // read in that interval answers from the cache, and the reader drops its
    // own store scan entirely.
    if let Some(fold) = fold_served_cached(ctx.home.clone()).await {
        if let Ok(v) = serde_json::to_value(&*fold) {
            if let Some(obj) = payload.as_object_mut() {
                obj.insert("arms_fold".into(), v);
            }
        }
    }
    Response::ok(req.id, payload)
}
