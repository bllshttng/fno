//! `fno backlog batch <verb>` - the batch lane's state, ship and metrics
//! doors (the wheel backlog/batch.py). State lives in `<canonical>/.fno/
//! batches/<domain>.json` under one flock per domain; the daemon shells
//! `prepare` before dispatch and `ship-closeable` after each tick, and a
//! `/target batched` member opens/joins through the same files.
//!
//! Exit codes: open 3 exists / 1 invalid; join 2 no-batch / 4 full / 1
//! invalid; ship 2 abandoned / 3 noop / 0 shipped; prepare always 0
//! (fail-safe solo); the rest 0 or 1.
use serde_json::{json, Value};
use std::io::Write as _;
use std::path::{Path, PathBuf};

pub(crate) const BATCH_HELP: &str = include_str!("batch_help.txt");

const BATCHES_DIRNAME: &str = ".fno/batches";

/// `fno backlog batch <verb>`: dispatch one tail. An unknown verb prints the
/// usage to stderr and exits 2, the same face the wheel's typer app wore.
pub fn run_batch(tail: &[String]) -> i32 {
    let (verb, rest) = match tail.split_first() {
        Some((v, r)) => (v.as_str(), r),
        None => {
            print!("{BATCH_HELP}");
            return 0;
        }
    };
    match verb {
        "open" => batch_open(rest),
        "join" => batch_join(rest),
        "close" | "abandon" => batch_retired(rest, verb),
        "ship" => batch_ship(rest),
        "prepare" => batch_prepare(rest),
        "ship-closeable" => batch_ship_closeable(rest),
        "status" => batch_status(rest),
        "metrics" => batch_metrics(rest),
        "--help" | "-h" | "help" => {
            print!("{BATCH_HELP}");
            0
        }
        _ => {
            eprintln!("No such command '{verb}'.");
            eprint!("Try 'fno backlog batch --help' for help.\n");
            2
        }
    }
}

/// The tombstoned close/abandon words: the wheel retired them (tombstones.toml
/// names ship as the only close path the daemon takes); the native lane
/// refuses with the same redirect.
fn batch_retired(rest: &[String], verb: &str) -> i32 {
    let domain = flag_value(rest, "--domain").or_else(|| flag_value(rest, "-d"));
    match domain {
        Some(d) => {
            eprintln!(
                "'batch {verb}' is retired; `fno backlog batch ship --domain {d}` is the close path (it requeues members on failure)"
            );
            1
        }
        None => {
            eprintln!("'batch {verb}' is retired; `fno backlog batch ship` is the close path (it requeues members on failure)");
            1
        }
    }
}

fn now_z() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn safe_domain(domain: &str) -> Result<String, String> {
    let d = domain.trim();
    if d.is_empty() || d.contains('/') || d == "." || d == ".." {
        return Err(format!("invalid domain: {domain:?}"));
    }
    Ok(d.to_string())
}

fn batches_dir(root: &Path) -> PathBuf {
    root.join(BATCHES_DIRNAME)
}

fn batch_path(domain: &str, root: &Path) -> Result<PathBuf, String> {
    Ok(batches_dir(root).join(format!("{}.json", safe_domain(domain)?)))
}

/// The state root: explicit --root, else the CANONICAL repo root. Batch state
/// is cross-worktree coordination state (daemon, batched worker, and the
/// close tick must all see one open batch per domain), so a raw cwd default
/// would fragment it across linked worktrees.
fn state_root(explicit: Option<&str>) -> PathBuf {
    if let Some(r) = explicit {
        return PathBuf::from(r);
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::paths::canonical_repo_root(&cwd).unwrap_or(cwd)
}

fn atomic_write(path: &Path, data: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension(format!(
        "json.tmp.{}",
        std::process::id()
    ));
    let body = format!("{}\n", serde_json::to_string_pretty(data).map_err(|e| e.to_string())?);
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Read-modify-write under the domain's flock. `mutate` returns Err to refuse
/// (the message rides out, mapped by the caller) or Ok(false) for a silent
/// no-op (idempotent re-join).
fn locked_mutate<F>(domain: &str, root: &Path, mutate: F) -> Result<(Value, bool), (String, i32)>
where
    F: FnOnce(Value) -> Result<(Value, bool), String>,
{
    let path = batch_path(domain, root).map_err(|m| (m, 1))?;
    let _lock = crate::graph_store::BoundedLock::acquire(
        &path,
        crate::graph_store::DEFAULT_LOCK_TIMEOUT,
    )
    .map_err(|e| (e.to_string(), 1))?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str::<Value>(&text).map_err(|e| (e.to_string(), 1))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err((e.to_string(), 1)),
    };
    let batch = existing.unwrap_or_else(|| json!({}));
    let (next, wrote) = mutate(batch).map_err(|m| (m, 1))?;
    if wrote {
        atomic_write(&path, &next)?;
    }
    Ok((next, wrote))
}

fn read_batch(domain: &str, root: &Path) -> Result<Option<Value>, String> {
    let path = batch_path(domain, root)?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    serde_json::from_str(&text).map(Some).map_err(|e| e.to_string())
}

/// Every parseable batch record, sorted by file name. A corrupt file is named
/// on stderr rather than hidden: the mutation path errors on it, and status
/// must not lie.
fn list_batches(root: &Path) -> Vec<Value> {
    let dir = batches_dir(root);
    let mut out = Vec::new();
    let Ok(files) = std::fs::read_dir(&dir) else {
        return out;
    };
    let mut paths: Vec<PathBuf> = files
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    paths.sort();
    for p in paths {
        match std::fs::read_to_string(&p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
            Some(v) => out.push(v),
            None => eprintln!("batch: skipping unreadable batch file {}", p.display()),
        }
    }
    out
}

fn member_ids(batch: &Value) -> Vec<String> {
    batch
        .get("members")
        .and_then(Value::as_array)
        .map(|ms| {
            ms.iter()
                .filter_map(|m| m.get("node_id").and_then(Value::as_str))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn is_full(batch: &Value) -> bool {
    let cap = batch.get("max_nodes").and_then(Value::as_i64).unwrap_or(3);
    member_ids(batch).len() >= cap as usize
}

fn is_open(batch: Option<&Value>) -> bool {
    batch.is_some_and(|b| b.get("status").and_then(Value::as_str) == Some("open"))
}

/// A long-form `--flag value` reader for the dispatch tails (inline `=` form
/// included). Returns (value, remaining-tail-position) semantics via Option.
fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(inline) = a.strip_prefix(&format!("{name}=")) {
            return Some(inline);
        }
        if a == name {
            return it.next().map(String::as_str);
        }
    }
    None
}

fn has_flag(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn hex_token(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| format!("mint entropy failed: {e}"))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn open_batch(
    domain: &str,
    branch: &str,
    worktree: &str,
    max_nodes: i64,
    root: &Path,
) -> Result<Value, (String, i32)> {
    safe_domain(domain).map_err(|m| (m, 1))?;
    // max_nodes < 1 makes is_full() true from birth: no node could ever join.
    // Refuse at the primitive instead of minting a dead batch.
    if max_nodes < 1 {
        return Err((format!("max_nodes must be >= 1, got {max_nodes}"), 1));
    }
    let batch = json!({
        "batch_id": format!("batch-{}", hex_token(4)?),
        "domain": domain,
        "branch": branch,
        "worktree": worktree,
        "status": "open",
        "max_nodes": max_nodes,
        "created_at": now_z(),
        "closed_at": Value::Null,
        "pr_url": Value::Null,
        "members": [],
    });
    let path = batch_path(domain, root).map_err(|m| (m, 1))?;
    let _lock = crate::graph_store::BoundedLock::acquire(
        &path,
        crate::graph_store::DEFAULT_LOCK_TIMEOUT,
    )
    .map_err(|e| (e.to_string(), 1))?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str::<Value>(&text).map_err(|e| (e.to_string(), 1))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err((e.to_string(), 1)),
    };
    if is_open(existing.as_ref()) {
        return Err((
            format!("an open batch already exists for domain {domain:?}"),
            3,
        ));
    }
    atomic_write(&path, &batch).map_err(|m| (m, 1))?;
    Ok(batch)
}

fn join_batch(
    domain: &str,
    node_id: &str,
    summary: &str,
    root: &Path,
) -> Result<Value, (String, i32)> {
    safe_domain(domain).map_err(|m| (m, 1))?;
    locked_mutate(domain, root, |mut batch| {
        if !is_open(Some(&batch)) {
            return Err(format!("no open batch for domain {domain:?}"));
        }
        if member_ids(&batch).iter().any(|m| m == node_id) {
            return Ok((batch, false)); // idempotent re-join
        }
        if is_full(&batch) {
            let id = batch
                .get("batch_id")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let cap = batch.get("max_nodes").and_then(Value::as_i64).unwrap_or(3);
            return Err(format!("batch {id} is full ({cap} nodes)"));
        }
        if let Some(ms) = batch.get_mut("members").and_then(Value::as_array_mut) {
            ms.push(json!({"node_id": node_id, "summary": summary}));
        }
        Ok((batch, true))
    })
    .map_err(|(m, code)| (m, if m.starts_with("no open batch") { 2 } else if m.contains("is full") { 4 } else { code }))
}

fn close_batch(domain: &str, pr_url: Option<&str>, root: &Path) -> Result<Value, (String, i32)> {
    locked_mutate(domain, root, |mut batch| {
        if !is_open(Some(&batch)) {
            return Err(format!("no open batch for domain {domain:?}"));
        }
        if let Some(obj) = batch.as_object_mut() {
            obj.insert("status".into(), json!("closed"));
            obj.insert("closed_at".into(), json!(now_z()));
            if let Some(url) = pr_url {
                obj.insert("pr_url".into(), json!(url));
            }
        }
        Ok((batch, true))
    })
}

fn abandon_batch(domain: &str, root: &Path) -> Result<Value, (String, i32)> {
    locked_mutate(domain, root, |mut batch| {
        if !is_open(Some(&batch)) {
            return Err(format!("no open batch for domain {domain:?}"));
        }
        if let Some(obj) = batch.as_object_mut() {
            obj.insert("status".into(), json!("abandoned"));
            obj.insert("closed_at".into(), json!(now_z()));
        }
        Ok((batch, true))
    })
}

fn emit_json(value: &Value) {
    let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
    println!("{body}");
}

fn batch_open(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let Some(domain) = flag_value(rest, "--domain").or_else(|| flag_value(rest, "-d")) else {
        eprintln!("error: the following required arguments were not provided: --domain");
        return 2;
    };
    let Some(branch) = flag_value(rest, "--branch").or_else(|| flag_value(rest, "-b")) else {
        eprintln!("error: the following required arguments were not provided: --branch");
        return 2;
    };
    let Some(worktree) = flag_value(rest, "--worktree").or_else(|| flag_value(rest, "-w")) else {
        eprintln!("error: the following required arguments were not provided: --worktree");
        return 2;
    };
    let max_nodes: i64 = flag_value(rest, "--max-nodes")
        .and_then(|v| v.parse().ok())
        .unwrap_or(3);
    let root = state_root(flag_value(rest, "--root"));
    match open_batch(domain, branch, worktree, max_nodes, &root) {
        Ok(batch) => {
            emit_json(&batch);
            0
        }
        Err((message, code)) => {
            eprintln!("{message}");
            code
        }
    }
}

fn batch_join(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let Some(domain) = flag_value(rest, "--domain").or_else(|| flag_value(rest, "-d")) else {
        eprintln!("error: the following required arguments were not provided: --domain");
        return 2;
    };
    let Some(node) = flag_value(rest, "--node").or_else(|| flag_value(rest, "-n")) else {
        eprintln!("error: the following required arguments were not provided: --node");
        return 2;
    };
    let summary = flag_value(rest, "--summary").or_else(|| flag_value(rest, "-s")).unwrap_or("");
    let root = state_root(flag_value(rest, "--root"));
    match join_batch(domain, node, summary, &root) {
        Ok(batch) => {
            emit_json(&batch);
            0
        }
        Err((message, code)) => {
            eprintln!("{message}");
            code
        }
    }
}

fn batch_status(rest: &[String]) -> i32 {
    let root = state_root(flag_value(rest, "--root"));
    if let Some(domain) = flag_value(rest, "--domain").or_else(|| flag_value(rest, "-d")) {
        match read_batch(domain, &root) {
            Ok(Some(batch)) => emit_json(&batch),
            Ok(None) => emit_json(&json!({"domain": domain, "status": "none"})),
            Err(message) => {
                eprintln!("{message}");
                return 1;
            }
        }
        return 0;
    }
    let batches = list_batches(&root);
    emit_json(&json!({ "batches": batches }));
    0
}
