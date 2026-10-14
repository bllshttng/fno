//! `fno backlog task`: the task-grain surface (list + update).
//!
//! The wheel's `cmd_task_list` / `cmd_task_update` + the claims task ladder,
//! ported whole: plan-bound resolution, the Execution Strategy task ids (the
//! same parser the orchestrator's wave reader uses), row materialization under
//! the locked write, the claim-gated `in_progress` transition, the
//! holder-guarded `done`, the holder-only `pending` give-back, and `--takeover`
//! as the gone-holder escape for all three.

use serde_json::{json, Value};

use crate::wave;

const TASK_HELP: &str = "Usage: fno backlog task <node> <task-id> --status pending|in_progress|done [--owner NAME] [--takeover]
       fno backlog task list <node> [--json|-J]

The claim IS the transition: `--status in_progress` takes the task:<node>:<task> claim first (exit 3 naming the holder when a peer owns it, exit 4 when no per-worker identity is provable and none was passed), then writes the row. `--status done` writes the row and releases the claim. `--status pending` is the holder-only give-back for a blocked or failed task. --takeover is the gone-holder escape for all three transitions.
";

/// The named exits (the wheel's constants): unreadable graph 5, no grain 6.
const TASK_GRAPH_UNREADABLE_EXIT: i32 = 5;
const TASK_NO_GRAIN_EXIT: i32 = 6;

const TASK_STATUSES: [&str; 3] = ["pending", "in_progress", "done"];

/// Resolve NODE to (node_id, resolved plan path); the wheel's
/// _task_plan_or_exit. Exact rows use the native read; missing rows resolve
/// through the tiers. An unreadable graph exits 5, never peer-held.
fn task_plan_or_exit(node_token: &str) -> (String, std::path::PathBuf) {
    let graph = super::settings::graph_path();
    let store = super::api::Store::new(&graph);
    let rows = match super::api::rows(&store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!(
                "Could not read the graph cleanly, so '{}' cannot be resolved: {}",
                node_token, e.0
            );
            std::process::exit(TASK_GRAPH_UNREADABLE_EXIT);
        }
    };
    let resolved = super::node_ref::find_node(&rows, node_token)
        .or_else(|| super::node_ref::resolve_tiers(&rows, node_token));
    let Some(row) = resolved else {
        eprintln!("Error: node '{node_token}' does not resolve to a node");
        std::process::exit(1);
    };
    let plan_path = row
        .get("plan_path")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if plan_path.is_empty() {
        eprintln!(
            "no plan bound to {}; no task grain to guard",
            row.get("id").and_then(Value::as_str).unwrap_or("")
        );
        std::process::exit(TASK_NO_GRAIN_EXIT);
    }
    let resolved_path = {
        let root = super::collision::find_repo_root();
        super::collision::resolve_plan_path(&plan_path, &root)
    };
    if !resolved_path.is_file() {
        eprintln!(
            "plan file for {} is not readable: {plan_path}",
            row.get("id").and_then(Value::as_str).unwrap_or("")
        );
        std::process::exit(1);
    }
    (
        row.get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        resolved_path,
    )
}

/// Plan task ids, or the named refusals (the wheel's _task_ids_or_exit): a
/// broken strategy fence exits 1 like every other task-verb failure, and an
/// id-less strategy is the same "no grain here" as an unbound plan (exit 6).
fn task_ids_or_exit(plan_path: &std::path::Path) -> Vec<String> {
    let strategy = match wave::parse_strategy(plan_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("plan parse failed for {}: {}", plan_path.display(), e);
            std::process::exit(1);
        }
    };
    let ids: Vec<String> = strategy.tasks.keys().cloned().collect();
    if ids.is_empty() {
        eprintln!("no tasks declared by {}", plan_path.display());
        std::process::exit(TASK_NO_GRAIN_EXIT);
    }
    ids
}

/// Materialize the missing pending rows inside the caller's mutation (the
/// wheel's ensure_task_rows): existing rows stay verbatim, new ids gain
/// pending rows, rows that left the plan stay, an unreadable tasks key
/// materializes nothing, and a repeated id yields one row.
fn ensure_task_rows(entry: &mut Value, ids: &[String]) {
    let bad_type = !entry
        .get("tasks")
        .map(|t| t.is_array() || t.is_null())
        .unwrap_or(true);
    if bad_type {
        return;
    }
    let mut rows: Vec<Value> = entry
        .get("tasks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut known: std::collections::BTreeSet<String> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str))
        .map(String::from)
        .collect();
    for task_id in ids {
        if known.insert(task_id.clone()) {
            rows.push(json!({"id": task_id, "status": "pending", "owner": null}));
        }
    }
    entry["tasks"] = Value::Array(rows);
}

/// The dispatcher: the wheel's two real subcommands (`list`, `update`; the
/// callback exists only to keep Typer from collapsing a single-command app).
pub fn run_task(tail: &[String]) -> i32 {
    match tail.first().map(String::as_str) {
        Some("list") => run_task_list(&tail[1..]),
        Some("update") => run_task_update(&tail[1..]),
        Some("--help") | Some("-h") | None => {
            print!("{TASK_HELP}");
            0
        }
        Some(word) => {
            eprintln!("No such command '{word}'.");
            2
        }
    }
}

/// `task list`: the node's task rows, materializing any the plan adds (the
/// wheel's cmd_task_list). The first read of a node with a bound plan writes
/// the missing pending rows, so a peer never sees a node whose tasks are
/// invisible until someone starts one; a steady-state poll takes no lock.
fn run_task_list(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{TASK_HELP}");
        return 0;
    }
    let json_out = tail.iter().any(|a| a == "--json" || a == "-J");
    let Some(node) = tail.iter().find(|a| !a.starts_with('-')) else {
        eprintln!("Missing argument 'NODE'.");
        return 2;
    };
    let (node_id, plan_path) = task_plan_or_exit(node);
    let ids = task_ids_or_exit(&plan_path);

    let print_rows = |rows: &[Value]| {
        if json_out {
            println!(
                "{}",
                serde_json::to_string_pretty(&json!({"node": node_id, "tasks": rows}))
                    .unwrap_or_else(|_| "{}".to_string())
            );
        } else {
            for r in rows {
                let owner = r
                    .get("owner")
                    .filter(|o| !o.is_null())
                    .map(|o| o.to_string())
                    .unwrap_or_else(|| "-".to_string());
                println!(
                    "{}\t{}\t{}",
                    r.get("id").and_then(Value::as_str).unwrap_or(""),
                    r.get("status").and_then(Value::as_str).unwrap_or(""),
                    owner
                );
            }
        }
    };

    // Fast path: every plan id already has a row -> print without the lock.
    let graph = super::settings::graph_path();
    let store = super::api::Store::new(&graph);
    if let Ok(rows) = super::api::rows(&store) {
        if let Some(entry) = rows
            .iter()
            .find(|e| e.get("id").and_then(Value::as_str) == Some(node_id.as_str()))
        {
            let task_rows: Vec<Value> = entry
                .get("tasks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let known: std::collections::BTreeSet<String> = task_rows
                .iter()
                .filter_map(|r| r.get("id").and_then(Value::as_str))
                .map(String::from)
                .collect();
            if ids.iter().all(|i| known.contains(i)) {
                if !task_rows.is_empty() {
                    print_rows(&task_rows);
                    return 0;
                }
            }
        }
    }

    // Materialize under the lock, then re-read and print.
    let written = crate::graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            if let Some(idx) = super::node_ref::find_node_index(working, &node_id) {
                ensure_task_rows(&mut working[idx], &ids);
            }
            Ok(true)
        },
    );
    match written {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Error: {e}");
            return 1;
        }
    }
    let rows = match super::api::rows(&store) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Error: {}", e.0);
            return 1;
        }
    };
    match rows
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(node_id.as_str()))
    {
        Some(entry) => {
            let task_rows: Vec<Value> = entry
                .get("tasks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if task_rows.is_empty() {
                eprintln!("no tasks declared by {}", plan_path.display());
                return TASK_NO_GRAIN_EXIT;
            }
            print_rows(&task_rows);
            0
        }
        None => {
            eprintln!("no tasks declared by {}", plan_path.display());
            TASK_NO_GRAIN_EXIT
        }
    }
}

/// `task update`: the claim IS the transition (the wheel's cmd_task_update).
fn run_task_update(tail: &[String]) -> i32 {
    if tail.iter().any(|a| a == "--help" || a == "-h") {
        print!("{TASK_HELP}");
        return 0;
    }
    let mut positional: Vec<&String> = Vec::new();
    let mut owner: Option<&String> = None;
    let mut status: Option<&String> = None;
    let mut takeover = false;
    let mut i = 0;
    while i < tail.len() {
        let arg = &tail[i];
        i += 1;
        match arg.as_str() {
            "--owner" => {
                let Some(v) = tail.get(i) else {
                    eprintln!("Error: Option '--owner' requires an argument");
                    return 2;
                };
                owner = Some(v);
                i += 1;
            }
            "--takeover" => {
                takeover = true;
            }
            "--status" => {
                let Some(v) = tail.get(i) else {
                    eprintln!("Error: Option '--status' requires an argument");
                    return 2;
                };
                status = Some(v);
                i += 1;
            }
            _ if arg.starts_with('-') && arg.len() > 1 => {
                eprintln!("Error: No such option: {arg}");
                return 2;
            }
            _ => positional.push(arg),
        }
    }
    let Some(status) = status else {
        eprintln!("Missing argument '--status'.");
        return 2;
    };
    if !TASK_STATUSES.contains(&status.as_str()) {
        eprintln!("invalid --status '{status}'; one of pending, in_progress, done");
        return 2;
    }
    if owner.is_some() && takeover {
        eprintln!(
            "--owner names this caller as the holder; --takeover replaces a gone holder with the ambient identity. One flag, one meaning - pass one, never both"
        );
        return 2;
    }
    let Some(node) = positional.first() else {
        eprintln!("Missing argument 'NODE'.");
        return 2;
    };
    let Some(task_id) = positional.get(1) else {
        eprintln!("Missing argument 'TASK_ID'.");
        return 2;
    };
    let (node_id, plan_path) = task_plan_or_exit(node);
    let ids = task_ids_or_exit(&plan_path);
    if !ids.iter().any(|i| i.as_str() == *task_id) {
        eprintln!(
            "task '{task_id}' not in plan {}; plan tasks: {}",
            plan_path.display(),
            ids.join(", ")
        );
        return 2;
    }
    let key = format!("task:{node_id}:{task_id}");

    // The holder: --owner, or the resolver's ladder (env roster name, the
    // registry row bound to this session, the ambient session id). The
    // manifest's shared session id is a shared anchor, never an owned
    // identity, so it refuses and names the fix.
    let holder: String = match owner {
        Some(o) => o.to_string(),
        None => match resolve_task_holder() {
            Some(h) => h,
            None => {
                eprintln!(
                    "cannot prove a per-worker identity; set --owner <full-session-id> or spawn through fno agents spawn"
                );
                return 4;
            }
        },
    };

    /// The wheel's resolve_task_holder: the env roster name, else the registry
    /// row bound to this session id (the spawn-time name<->session binding that
    /// survives a daemon fork), else the ambient session id - unless the ONLY
    /// provable identity is the worktree manifest's shared value, which every
    /// fno process in the directory reads and no worker owns.
    fn resolve_task_holder() -> Option<String> {
        if let Ok(name) = std::env::var("FNO_WORKER_NAME") {
            let name = name.trim().to_string();
            if !name.is_empty() {
                return Some(name);
            }
        }
        let (session, _harness) = crate::claims::resolve_identity();
        let sid = session?;
        let home = crate::paths::AgentsHome::from_env();
        if let Ok(registry) = crate::state::load_registry(&home.registry_json()) {
            let names: Vec<&str> = registry
                .entries
                .iter()
                .filter(|e| e.harness_session_id.as_deref() == Some(sid.as_str()))
                .map(|e| e.name.as_str())
                .collect();
            if let [name] = names[..] {
                return Some(name.to_string());
            }
        }
        if manifest_names_session(&sid) {
            return None;
        }
        Some(sid)
    }

    /// The manifest answer: does the session manifest name this session id?
    /// One resolver, state_path - the space layout and the legacy checkout
    /// fallback are its job; a hand-built climb consults nothing.
    fn manifest_names_session(sid: &str) -> bool {
        if sid.is_empty() {
            return false;
        }
        let Ok(cwd) = std::env::current_dir() else {
            return false;
        };
        crate::state_path::resolve("target-state", &cwd)
            .and_then(|p| std::fs::read_to_string(p).ok())
            .is_some_and(|text| text.contains(sid))
    }

    // A live claim gates every path (the wheel's pre-read): a takeover is
    // the gone-holder escape, never a way to take from a running worker, and
    // the same holder identity under another live process is shared, not
    // owned. The non-takeover live case falls through to the acquire
    // ladder, whose idempotent re-acquire serves the same-process re-run.
    let (state, record) = crate::claims::status(&key, None);
    if state.as_str() == "live" {
        let holder_live = record
            .as_ref()
            .map(|r| r.holder.clone())
            .unwrap_or_default();
        let pid_live = record.as_ref().and_then(|r| r.pid);
        let same_holder = holder_live == holder;
        let escape = if same_holder {
            "--takeover is the escape for a holder that is gone, not a way to take a claim from a running worker"
        } else {
            "a running worker is never takeable"
        };
        if takeover {
            eprintln!("{key} is held LIVE by {holder_live} (pid={pid_live:?}); {escape}");
            return 3;
        }
        let (session_pid, _harness) =
            crate::spawn_context::session_identity_ambient(std::process::id());
        if same_holder && pid_live != session_pid.map(|p| p as i32) {
            eprintln!(
                "{key} is held LIVE by this holder identity under another live process (pid={pid_live:?}); the identity is shared, not owned. Re-run from the owning session, or spawn through fno agents spawn"
            );
            return 4;
        }
    }

    // The row transition under the lock. `apply` mutates the task row the
    // way the caller decided and answers whether it refused by name.
    fn set_row(
        node_id: &str,
        ids: &[String],
        task_id: &str,
        apply: &dyn Fn(&mut Value) -> Result<(), String>,
    ) -> Result<Value, String> {
        let graph = super::settings::graph_path();
        let mut found: Vec<Value> = Vec::new();
        let written = crate::graph_store::mutate_rows(
            &graph,
            std::time::Duration::from_secs(5),
            None,
            None,
            |working| {
                let Some(idx) = super::node_ref::find_node_index(working, node_id) else {
                    return Ok(false);
                };
                ensure_task_rows(&mut working[idx], ids);
                let Some(rows) = working[idx].get_mut("tasks").and_then(Value::as_array_mut) else {
                    return Ok(false);
                };
                for row in rows.iter_mut() {
                    if row.get("id").and_then(Value::as_str) == Some(task_id) {
                        // The apply contract speaks String refusals; the store
                        // closure speaks StoreError. Corrupt carries the text.
                        apply(row).map_err(crate::graph_store::StoreError::Corrupt)?;
                        found.push(row.clone());
                        break;
                    }
                }
                Ok(true)
            },
        );
        match written {
            Ok(_) => {}
            Err(e) => return Err(format!("graph write failed: {e}")),
        }
        found
            .into_iter()
            .next()
            .ok_or_else(|| format!("node {node_id} or task {task_id} vanished at write time"))
    }

    let release_note = |node: &str, task: &str, holder: &str| {
        let _ = crate::claims::release(&format!("task:{node}:{task}"), holder, None, None);
    };

    if status == "in_progress" {
        // The harness rides the ambient identity walk inside acquire; opts
        // carries only the pid and the task provenance.
        let (session_pid, _harness) =
            crate::spawn_context::session_identity_ambient(std::process::id());
        let mut opts = crate::claims::AcquireOpts::default();
        opts.pid = session_pid;
        opts.pid_unavailable = session_pid.is_none();
        opts.ttl_ms = if session_pid.is_some() {
            None
        } else {
            Some(7_200_000)
        };
        opts.reason = Some(format!("task {task_id} of node {node_id}"));
        let mut metadata = serde_json::Map::new();
        metadata.insert("node".into(), json!(node_id));
        metadata.insert("task".into(), json!(task_id));
        opts.metadata = Some(metadata);
        match crate::claims::acquire(&key, &holder, opts) {
            crate::claims::AcquireOutcome::Acquired(_) => {}
            crate::claims::AcquireOutcome::HeldByOther { holder, pid, .. } => {
                eprintln!("another holder owns {key} ({holder}, pid={pid:?})");
                return 3;
            }
            crate::claims::AcquireOutcome::Error(e) => {
                eprintln!("task claim contention on {key}: {}", e);
                return 3;
            }
        }
        let claimed_at = crate::graph_store::now_isoformat();
        // A done row is shipped work; re-offering it re-dispatches a
        // finished task, so the transition refuses and releases a claim
        // this call took (an idempotent re-acquire keeps its own claim).
        let reopen_refused = std::cell::Cell::new(false);
        let apply = |row: &mut Value| -> Result<(), String> {
            if row.get("status").and_then(Value::as_str) == Some("done") {
                reopen_refused.set(true);
                return Ok(());
            }
            if let Some(obj) = row.as_object_mut() {
                obj.insert("status".into(), json!("in_progress"));
                obj.insert("owner".into(), json!(holder));
                obj.insert("claimed_at".into(), json!(claimed_at));
            }
            Ok(())
        };
        match set_row(&node_id, &ids, task_id, &apply) {
            Err(message) => {
                release_note(&node_id, task_id, &holder);
                eprintln!("{message}");
                return 1;
            }
            Ok(_) if reopen_refused.get() => {
                release_note(&node_id, task_id, &holder);
                eprintln!("task {task_id} is done; re-offering shipped work is refused");
                return 3;
            }
            Ok(_) => {
                println!("{key} in_progress holder={holder}");
                return 0;
            }
        }
    }

    if status == "done" {
        // Holder-guarded like the give-back: a non-holder marking a live
        // task done leaves the peer working a task the board reports
        // finished. An UNowned row accepts done from anyone. --takeover
        // relaxes the owner match for a GONE holder; the live-claim guard
        // above already proved nothing live holds it.
        let refused_owner = std::cell::Cell::new(None);
        let apply = |row: &mut Value| -> Result<(), String> {
            let owner_now = row
                .get("owner")
                .and_then(Value::as_str)
                .filter(|o| !o.is_empty())
                .map(String::from);
            if owner_now.is_some() && owner_now != Some(holder.clone()) && !takeover {
                refused_owner.set(owner_now);
                return Ok(());
            }
            if let Some(obj) = row.as_object_mut() {
                obj.insert("status".into(), json!("done"));
            }
            Ok(())
        };
        match set_row(&node_id, &ids, task_id, &apply) {
            Err(message) => {
                eprintln!("{message}");
                return 1;
            }
            Ok(_) => {
                if let Some(owner_now) = refused_owner.take() {
                    eprintln!(
                        "task {task_id} held by {owner_now}; only the holder can mark it done. If that holder is gone, re-run with --takeover"
                    );
                    return 3;
                }
                release_note(&node_id, task_id, &holder);
                println!("{key} done");
                return 0;
            }
        }
    }

    // pending: the holder-only give-back. `done` leaves owner set, so the
    // holder's own later give-back would reopen shipped work, and an
    // unowned done row would accept a give-back from anyone - both refuse,
    // as the claim transition does.
    let giveback_done_refused = std::cell::Cell::new(false);
    let refused_owner = std::cell::Cell::new(None);
    let apply = |row: &mut Value| -> Result<(), String> {
        if row.get("status").and_then(Value::as_str) == Some("done") {
            giveback_done_refused.set(true);
            return Ok(());
        }
        let owner_now = row
            .get("owner")
            .and_then(Value::as_str)
            .filter(|o| !o.is_empty())
            .map(String::from);
        if owner_now.is_some() && owner_now != Some(holder.clone()) && !takeover {
            refused_owner.set(owner_now);
            return Ok(());
        }
        if let Some(obj) = row.as_object_mut() {
            obj.insert("status".into(), json!("pending"));
            obj.insert("owner".into(), Value::Null);
            obj.remove("claimed_at");
        }
        Ok(())
    };
    match set_row(&node_id, &ids, task_id, &apply) {
        Err(message) => {
            eprintln!("{message}");
            1
        }
        Ok(_) if giveback_done_refused.get() => {
            eprintln!("task {task_id} is done; giving shipped work back is refused");
            3
        }
        Ok(_) => {
            if let Some(owner_now) = refused_owner.take() {
                eprintln!(
                    "task {task_id} held by {owner_now}; only the holder can give it back. If that holder is gone, re-run with --takeover"
                );
                return 3;
            }
            println!("{key} pending");
            0
        }
    }
}
