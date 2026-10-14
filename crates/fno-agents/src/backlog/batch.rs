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
    let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(data).map_err(|e| e.to_string())?
    );
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
    let _lock =
        crate::graph_store::BoundedLock::acquire(&path, crate::graph_store::DEFAULT_LOCK_TIMEOUT)
            .map_err(|e| (e.to_string(), 1))?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(serde_json::from_str::<Value>(&text).map_err(|e| (e.to_string(), 1))?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err((e.to_string(), 1)),
    };
    let batch = existing.unwrap_or_else(|| json!({}));
    let (next, wrote) = mutate(batch).map_err(|m| (m, 1))?;
    if wrote {
        atomic_write(&path, &next).map_err(|m| (m, 1))?;
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
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| e.to_string())
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
        match std::fs::read_to_string(&p)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        {
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
        "batch_id": format!("batch-{}", hex_token(4).map_err(|m| (m, 1))?),
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
    let _lock =
        crate::graph_store::BoundedLock::acquire(&path, crate::graph_store::DEFAULT_LOCK_TIMEOUT)
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
            let id = batch.get("batch_id").and_then(Value::as_str).unwrap_or("?");
            let cap = batch.get("max_nodes").and_then(Value::as_i64).unwrap_or(3);
            return Err(format!("batch {id} is full ({cap} nodes)"));
        }
        if let Some(ms) = batch.get_mut("members").and_then(Value::as_array_mut) {
            ms.push(json!({"node_id": node_id, "summary": summary}));
        }
        Ok((batch, true))
    })
    .map_err(|(m, code)| {
        (
            m,
            if m.starts_with("no open batch") {
                2
            } else if m.contains("is full") {
                4
            } else {
                code
            },
        )
    })
    .map(|(batch, _)| batch)
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
    .map(|(batch, _)| batch)
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
    .map(|(batch, _)| batch)
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
    let summary = flag_value(rest, "--summary")
        .or_else(|| flag_value(rest, "-s"))
        .unwrap_or("");
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

// The policy engine (the wheel's decide_batch_action + should_close): pure
// over inputs so tests feed synthetic nodes and batches.

/// A node ships alone (never batched) when it is large or drop-everything: a
/// big or urgent change deserves its own reviewable PR.
fn ships_alone(node: &Value) -> Option<&'static str> {
    let size = node.get("size").and_then(Value::as_str).unwrap_or("");
    if size.eq_ignore_ascii_case("L") {
        return Some("size:L ships alone");
    }
    let prio = node.get("priority").and_then(Value::as_str).unwrap_or("");
    if prio.eq_ignore_ascii_case("p0") {
        return Some("p0 ships alone");
    }
    None
}

/// What to do with a candidate node at selection time.
struct BatchDecision {
    action: &'static str,
    domain: String,
    reason: String,
}

/// Decide whether a candidate node ships solo, joins, or starts a batch.
/// `enabled=false` always answers ship_solo: byte-for-byte the one-PR-per-node
/// behavior when config.batch.enabled is off.
fn decide_batch_action(node: &Value, enabled: bool, root: &Path) -> BatchDecision {
    let domain = node
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or("code")
        .to_string();
    if !enabled {
        return BatchDecision {
            action: "ship_solo",
            domain,
            reason: "batching disabled".into(),
        };
    }
    if let Some(solo) = ships_alone(node) {
        return BatchDecision {
            action: "ship_solo",
            domain,
            reason: solo.to_string(),
        };
    }
    // A corrupt batch file must not crash the live selection loop; ship solo.
    let batch = match read_batch(&domain, root) {
        Ok(b) => b,
        Err(e) => {
            return BatchDecision {
                action: "ship_solo",
                domain,
                reason: format!("error reading batch: {e}"),
            }
        }
    };
    let open_not_full = is_open(batch.as_ref()) && !is_full(batch.as_ref().unwrap());
    if open_not_full {
        let id = batch
            .as_ref()
            .unwrap()
            .get("batch_id")
            .and_then(Value::as_str)
            .unwrap_or("?");
        return BatchDecision {
            action: "join",
            domain,
            reason: format!("join open batch {id}"),
        };
    }
    BatchDecision {
        action: "start",
        domain,
        reason: "no joinable open batch".into(),
    }
}

/// Close the open batch when the first close condition trips. The domain
/// boundary is the important one: it caps blast radius and keeps the review
/// panel looking at a coherent diff. (config.batch.max_loc stays inert in v1:
/// the batch state tracks no cumulative diff size, so there is nothing to
/// compare - the leg can never wrongly close.)
fn should_close(batch: Option<&Value>, next_node: Option<&Value>) -> (bool, String) {
    let Some(batch) = batch.filter(|b| b.get("status").and_then(Value::as_str) == Some("open"))
    else {
        return (false, "no open batch".into());
    };
    if is_full(batch) {
        return (true, "max_nodes reached".into());
    }
    let Some(next) = next_node else {
        return (true, "no more ready nodes (drain)".into());
    };
    let batch_domain = batch
        .get("domain")
        .and_then(Value::as_str)
        .unwrap_or("code");
    let next_domain = next.get("domain").and_then(Value::as_str).unwrap_or("code");
    if next_domain != batch_domain {
        return (true, "next node is a different domain".into());
    }
    if let Some(solo) = ships_alone(next) {
        return (true, format!("next node {solo}"));
    }
    (false, "batch stays open".into())
}

/// Fetch a node dict through the native get door, or None on any failure.
fn get_node(node_id: &str) -> Option<Value> {
    let out = std::process::Command::new(std::env::current_exe().ok()?)
        .args(["backlog", "get", node_id])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice::<Value>(&out.stdout).ok()
}

/// The next ready node, or None on a genuine drain. ANY failure (nonzero exit,
/// unparseable output) is Err: should_close treats None as "drain -> close
/// every open batch", so a transient next hiccup mapped to None would ship
/// every open batch on one bad tick. The caller skips the tick on Err.
/// `mission` scopes the peek to the same candidate set the daemon dispatches.
fn peek_next(project: Option<&str>, mission: Option<&str>) -> Result<Option<Value>, String> {
    let mut cmd = std::process::Command::new(
        std::env::current_exe()
            .map_err(|_| "fno backlog next failed: no current binary".to_string())?,
    );
    cmd.args(["backlog", "next"]);
    if let Some(p) = project {
        cmd.args(["--project", p]);
    }
    if let Some(m) = mission {
        cmd.args(["--mission", m]);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("fno backlog next spawn failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "fno backlog next exited {}: {}",
            out.status.code().unwrap_or(-1),
            err.trim().chars().take(160).collect::<String>()
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() || text == "null" {
        return Ok(None);
    }
    let node: Value = serde_json::from_str(&text)
        .map_err(|e| format!("fno backlog next returned non-JSON: {e}"))?;
    if node.is_object() {
        Ok(Some(node))
    } else {
        Ok(None)
    }
}

/// Decide solo-vs-batched for a candidate node and (on batch) resolve the
/// shared worktree, opening a new batch if needed. Fail-safe: ANY error
/// degrades to solo, so a broken batch setup never blocks or mis-dispatches.
/// Always exit 0; the daemon reads `mode`.
fn prepare_batch(node_id: &str, repo: &str, root: &Path) -> Value {
    let Some(node) = get_node(node_id) else {
        return json!({"mode": "solo", "reason": "node lookup failed"});
    };
    let decision = decide_batch_action(&node, super::settings::batch_enabled(), root);
    if decision.action == "ship_solo" {
        return json!({"mode": "solo", "reason": decision.reason});
    }
    let domain = decision.domain;
    if decision.action == "start" {
        return prepare_start(&node_id.to_string(), &domain, repo, root);
    }
    // join: reuse the open batch's recorded worktree/branch.
    match read_batch(&domain, root) {
        Ok(b) if is_open(b.as_ref()) => {
            let b = b.unwrap();
            return json!({
                "mode": "batched",
                "domain": domain,
                "worktree": b.get("worktree").cloned().unwrap_or(Value::Null),
                "branch": b.get("branch").cloned().unwrap_or(Value::Null),
                "batch_id": b.get("batch_id").cloned().unwrap_or(Value::Null),
            });
        }
        _ => {
            return json!({"mode": "solo", "reason": "no open batch to join"});
        }
    }
}

/// The `start` leg: unique branch/worktree per batch, then open (a peer that
/// opened between decide and open is joined instead - its recorded
/// worktree/branch win).
fn prepare_start(node_id: &str, domain: &str, repo: &str, root: &Path) -> Value {
    let name = format!(
        "batch-{}.{}",
        safe_domain(domain).unwrap_or_else(|_| "code".into()),
        hex_token(3).unwrap_or_default()
    );
    let branch = format!("feature/{name}");
    // The launcher's own command builder resolves the ensure door the same
    // way every native caller does; the explicit --branch means a `never`
    // project refuses and degrades to solo below, never launches on canonical.
    let mut cmd = crate::org_board::budget::fno_py_cmd();
    cmd.extend([
        "workspace".to_string(),
        "worktree".to_string(),
        "ensure".to_string(),
        "--repo".to_string(),
        repo.to_string(),
        "--name".to_string(),
        name.clone(),
        "--branch".to_string(),
        branch.clone(),
        "--harness".to_string(),
        "claude".to_string(),
    ]);
    let we = crate::org_board::budget::run_with_timeout_full(
        &cmd,
        Path::new("."),
        std::time::Duration::from_secs(120),
    );
    let we = match we {
        Ok(w) => w,
        Err(e) => {
            return json!({"mode": "solo", "reason": format!("worktree ensure failed: {}", e.message())})
        }
    };
    let worktree = String::from_utf8_lossy(&we.stdout).trim().to_string();
    if worktree.is_empty() {
        let err = String::from_utf8_lossy(&we.stderr);
        return json!({
            "mode": "solo",
            "reason": format!(
                "worktree ensure failed: {}",
                err.trim().chars().take(160).collect::<String>()
            )
        });
    }
    let setup = Path::new(repo)
        .join("scripts")
        .join("setup")
        .join("setup-worktree.sh");
    if setup.exists() {
        // Ok already means exit 0 (the full runner refuses nonzero); a setup
        // failure lands in the Err arm and degrades to solo.
        let sr = crate::org_board::budget::run_with_timeout_full(
            &["bash".to_string(), setup.to_string_lossy().into_owned()],
            Path::new(&worktree),
            std::time::Duration::from_secs(300),
        );
        if let Err(e) = sr {
            return json!({
                "mode": "solo",
                "reason": format!("setup-worktree failed: {}", e.message())
            });
        }
    }
    match open_batch(
        domain,
        &branch,
        &worktree,
        super::settings::batch_max_nodes(),
        root,
    ) {
        Ok(b) => {
            return json!({
                "mode": "batched",
                "domain": domain,
                "worktree": b.get("worktree").cloned().unwrap_or(Value::Null),
                "branch": b.get("branch").cloned().unwrap_or(Value::Null),
                "batch_id": b.get("batch_id").cloned().unwrap_or(Value::Null),
            });
        }
        Err((message, _code)) => {
            if message.contains("already exists") {
                // A peer opened the batch between decide and open; join it
                // (its recorded worktree/branch win; this call's fresh
                // worktree is left unused - a rare single-dispatcher race).
                if let Ok(Some(b)) = read_batch(domain, root) {
                    if is_open(Some(&b)) {
                        return json!({
                            "mode": "batched",
                            "domain": domain,
                            "worktree": b.get("worktree").cloned().unwrap_or(Value::Null),
                            "branch": b.get("branch").cloned().unwrap_or(Value::Null),
                            "batch_id": b.get("batch_id").cloned().unwrap_or(Value::Null),
                        });
                    }
                }
                return json!({"mode": "solo", "reason": "batch vanished after race"});
            }
            json!({"mode": "solo", "reason": message})
        }
    }
}

fn batch_prepare(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let Some(node) = flag_value(rest, "--node").or_else(|| flag_value(rest, "-n")) else {
        eprintln!("error: the following required arguments were not provided: --node");
        return 2;
    };
    let Some(repo) = flag_value(rest, "--repo") else {
        eprintln!("error: the following required arguments were not provided: --repo");
        return 2;
    };
    let root = state_root(flag_value(rest, "--root"));
    emit_json(&prepare_batch(node, repo, &root));
    0
}

/// Ship every open batch whose close condition tripped (the daemon calls this
/// per tick). A peek FAILURE (distinct from a drain) skips the whole tick and
/// ships nothing.
fn ship_closeable(project: Option<&str>, mission: Option<&str>, root: &Path) -> Vec<Value> {
    let next_node = match peek_next(project, mission) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("batch ship-closeable: peek failed, skipping tick: {e}");
            return Vec::new();
        }
    };
    let mut results = Vec::new();
    for b in list_batches(root) {
        if b.get("status").and_then(Value::as_str) != Some("open") {
            continue;
        }
        let (close, _reason) = should_close(Some(&b), next_node.as_ref());
        if close {
            let domain = b.get("domain").and_then(Value::as_str).unwrap_or("code");
            results.push(ship_result_value(&ship_batch(domain, "main", None, root)));
        }
    }
    results
}

// ---------------------------------------------------------------------------
// Per-batch ship: one PR for the whole batch (the wheel's ship_batch).
// ---------------------------------------------------------------------------

struct ShipResult {
    action: String,
    domain: String,
    reason: String,
    pr_url: Option<String>,
    pr_number: Option<i64>,
    members: Vec<String>,
}

fn ship_result_value(r: &ShipResult) -> Value {
    json!({
        "action": r.action,
        "domain": r.domain,
        "reason": r.reason,
        "pr_url": r.pr_url,
        "pr_number": r.pr_number,
        "members": r.members,
    })
}

/// PR number from a GitHub URL (or a bare number).
fn extract_pr_number(url_or_output: &str) -> Option<i64> {
    let text = url_or_output.trim();
    if let Some(idx) = text.find("/pull/") {
        let rest = &text[idx + 6..];
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !digits.is_empty() {
            return digits.parse().ok();
        }
    }
    if text.chars().all(char::is_ascii_digit) && !text.is_empty() {
        return text.parse().ok();
    }
    None
}

/// The PR body listing the members + their one-line summaries.
fn batch_pr_body(batch: &Value) -> String {
    let id = batch.get("batch_id").and_then(Value::as_str).unwrap_or("?");
    let domain = batch.get("domain").and_then(Value::as_str).unwrap_or("?");
    let count = member_ids(batch).len();
    let mut out = format!(
        "Batch **{id}** (domain `{domain}`) coalesces {count} node(s) into one PR to cut CI runs.\n\n## Members\n"
    );
    for m in batch
        .get("members")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let node = m.get("node_id").and_then(Value::as_str).unwrap_or("?");
        let summary = m
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if summary.is_empty() {
            out.push_str(&format!("- `{node}`\n"));
        } else {
            out.push_str(&format!("- `{node}` - {summary}\n"));
        }
    }
    out
}

/// Record the shared batch PR ref on every member, in one locked mutation.
/// Members are NOT closed here (the PR is not merged yet); the ref lets
/// merge-time `fno backlog reconcile` close each member by its pr_number.
fn set_member_pr_refs(
    members: &[String],
    pr_url: &str,
    pr_number: Option<i64>,
) -> Result<(), String> {
    let graph = super::settings::graph_path();
    let ids: std::collections::BTreeSet<String> = members.iter().cloned().collect();
    crate::graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for node in working.iter_mut() {
                let Some(id) = node.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if !ids.contains(id) {
                    continue;
                }
                if let Some(obj) = node.as_object_mut() {
                    obj.insert("pr_url".into(), json!(pr_url));
                    if let Some(n) = pr_number {
                        obj.insert("pr_number".into(), json!(n));
                    }
                }
            }
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| format!("member pr-ref write failed: {e}"))
}

/// Clear the `batch` mark on every member so they requeue as individual PRs.
/// Deliberately does NOT release the members' node claims: only the walker or
/// the handoff may release a node claim, never a helper subprocess (the
/// requeue is correct but latency-bound by the claim TTL).
fn clear_member_batch_marks(members: &[String]) -> Result<(), String> {
    let graph = super::settings::graph_path();
    let ids: std::collections::BTreeSet<String> = members.iter().cloned().collect();
    crate::graph_store::mutate_rows(
        &graph,
        std::time::Duration::from_secs(5),
        None,
        None,
        |working| {
            for node in working.iter_mut() {
                let Some(id) = node.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if !ids.contains(id) {
                    continue;
                }
                if let Some(obj) = node.as_object_mut() {
                    obj.insert("batch".into(), Value::Null);
                }
            }
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| format!("member batch-mark clear failed: {e}"))
}

/// The v1 failure path: abandon the batch and clear member marks.
fn abandon_and_requeue(domain: &str, members: &[String], root: &Path) {
    let _ = abandon_batch(domain, root);
    if let Err(e) = clear_member_batch_marks(members) {
        eprintln!("batch: member requeue degraded: {e}");
    }
}

// The stale-base guard (the wheel's check_stale_base, codes and fail-open
// ladder preserved): (0, None) clean, (0, Some(msg)) skipped-with-a-word,
// (3, msg) stale, (4, msg) unrelated.
fn check_stale_base(base: &str, cwd: &Path) -> (i32, Option<String>) {
    if std::env::var("FNO_PR_BASE_OK").as_deref() == Ok("stale-acknowledged") {
        return (0, None);
    }
    let compare = if base.contains('/') {
        base.to_string()
    } else {
        format!("origin/{base}")
    };
    // The fetch advances the remote-tracking ref, so it takes TWO words
    // (remote + ref), never the `remote/ref` spelling the comparison reads.
    let (remote, refname) = compare.split_once('/').unwrap_or(("origin", base));
    let fetch = std::process::Command::new("git")
        .args(["fetch", remote, refname, "--quiet"])
        .current_dir(cwd)
        .output();
    match fetch {
        Err(_) => {
            return (
                0,
                Some("could not run git; stale-base check skipped".into()),
            )
        }
        Ok(out) if !out.status.success() => {
            return (
                0,
                Some(format!(
                    "could not refresh {compare}; stale-base check skipped"
                )),
            )
        }
        _ => {}
    }
    let run_git = |args: &[&str]| -> Option<std::process::Output> {
        std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .ok()
    };
    let mb = run_git(&["merge-base", "HEAD", &compare]);
    let mb_sha = mb
        .as_ref()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if mb.is_none() || !mb.as_ref().is_some_and(|o| o.status.success()) || mb_sha.is_empty() {
        return (
            4,
            Some(format!(
                "base {compare} shares no history with HEAD; cannot compute a merge base"
            )),
        );
    }
    let Some(behind) = run_git(&["rev-list", "--count", &format!("{mb_sha}..{compare}")]) else {
        return (
            0,
            Some("could not determine behind count; stale-base check skipped".into()),
        );
    };
    if !behind.status.success() {
        return (
            0,
            Some("could not determine behind count; stale-base check skipped".into()),
        );
    }
    if String::from_utf8_lossy(&behind.stdout).trim() == "0" {
        return (0, None);
    }
    // Behind: stale iff the base tip has moved further than MAX_HOURS past
    // the merge base (the wheel's 24h window).
    let committer_epoch = |sha: &str| -> Option<i64> {
        let out = run_git(&["show", "-s", "--format=%ct", sha])?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    };
    let tip_ts = committer_epoch(&compare);
    let mb_ts = committer_epoch(&mb_sha);
    let (Some(tip_ts), Some(mb_ts)) = (tip_ts, mb_ts) else {
        return (
            0,
            Some("could not read commit dates; stale-base check skipped".into()),
        );
    };
    let span_hours = (tip_ts - mb_ts) as f64 / 3600.0;
    if span_hours > 24.0 {
        return (
            3,
            Some(format!(
                "base {compare} moved {span_hours:.0}h past the merge base; rebase before shipping (FNO_PR_BASE_OK=stale-acknowledged overrides)"
            )),
        );
    }
    (0, None)
}

/// Open ONE PR for the open batch, record the shared ref on members.
/// Idempotent on the PR (an existing PR for the branch is reused). Any
/// failure to open the PR abandons the batch and requeues its members as
/// individual PRs (v1 failure policy).
fn ship_batch(domain: &str, base: &str, title: Option<&str>, root: &Path) -> ShipResult {
    let members_of = |b: &Value| member_ids(b);
    let batch = match read_batch(domain, root) {
        Ok(b) => b,
        Err(e) => {
            return ShipResult {
                action: "noop".into(),
                domain: domain.into(),
                reason: e,
                pr_url: None,
                pr_number: None,
                members: Vec::new(),
            }
        }
    };
    let Some(batch) = batch.filter(|b| is_open(Some(b))) else {
        return ShipResult {
            action: "noop".into(),
            domain: domain.into(),
            reason: "no open batch".into(),
            pr_url: None,
            pr_number: None,
            members: Vec::new(),
        };
    };
    let members = members_of(&batch);
    if members.is_empty() {
        // An empty open batch has nothing to ship; abandon it so it does not
        // linger and block a fresh batch for the domain.
        let _ = abandon_batch(domain, root);
        return ShipResult {
            action: "abandoned".into(),
            domain: domain.into(),
            reason: "empty batch".into(),
            pr_url: None,
            pr_number: None,
            members,
        };
    }
    let worktree = batch
        .get("worktree")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let branch = batch
        .get("branch")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if worktree.is_empty() || branch.is_empty() {
        abandon_and_requeue(domain, &members, root);
        return ShipResult {
            action: "abandoned".into(),
            domain: domain.into(),
            reason: "batch missing worktree/branch".into(),
            pr_url: None,
            pr_number: None,
            members,
        };
    }
    let pr_title = title
        .map(String::from)
        .unwrap_or_else(|| format!("batch({domain}): {} nodes", members.len()));
    let wt = PathBuf::from(&worktree);
    // Idempotency: reuse an existing PR for the branch before creating one.
    let mut pr_url: Option<String> = None;
    let mut pr_number: Option<i64> = None;
    if let Ok(lst) = std::process::Command::new("gh")
        .args(["pr", "list", "--head", &branch, "--json", "number,url"])
        .current_dir(&wt)
        .output()
    {
        if lst.status.success() {
            let text = String::from_utf8_lossy(&lst.stdout);
            if let Ok(Ok(items)) = serde_json::from_str::<Value>(text.trim())
                .map(|v| serde_json::from_value::<Vec<Value>>(v))
            {
                if let Some(first) = items.first() {
                    pr_url = first.get("url").and_then(Value::as_str).map(String::from);
                    pr_number = first.get("number").and_then(Value::as_i64);
                }
            }
        }
    }
    if pr_url.is_none() {
        // Guard precedes the push so a stale branch is never pushed. Fail-open
        // (skipped guard) is announced, never read as a clean pass.
        let (base_code, base_msg) = check_stale_base(base, &wt);
        if let Some(msg) = &base_msg {
            if base_code == 0 {
                eprintln!("batch.ship: {msg}");
            }
        }
        if base_code != 0 {
            abandon_and_requeue(domain, &members, root);
            return ShipResult {
                action: "abandoned".into(),
                domain: domain.into(),
                reason: base_msg.unwrap_or_else(|| "stale base: refused to open batch PR".into()),
                pr_url: None,
                pr_number: None,
                members,
            };
        }
        // The duplicate guard rides the native pr-create check-only door.
        let dup = std::process::Command::new(std::env::current_exe().unwrap_or_default())
            .args([
                "pr-create",
                "--check-only",
                "--title",
                &pr_title,
                "--base",
                &format!("origin/{base}"),
                "--cwd",
                &worktree,
            ])
            .output();
        if let Ok(dup) = dup {
            if !dup.status.success() {
                let detail = String::from_utf8_lossy(&dup.stderr).trim().to_string();
                abandon_and_requeue(domain, &members, root);
                return ShipResult {
                    action: "abandoned".into(),
                    domain: domain.into(),
                    reason: if detail.is_empty() {
                        "duplicate PR for this title".into()
                    } else {
                        detail
                    },
                    pr_url: None,
                    pr_number: None,
                    members,
                };
            }
        }
        // The ensure pass creates only a LOCAL branch, so push explicitly
        // before `gh pr create --head` (which does not push).
        let pushed = std::process::Command::new("git")
            .args(["push", "-u", "origin", &branch])
            .current_dir(&wt)
            .output();
        match pushed {
            Ok(p) if p.status.success() => {}
            Ok(p) => {
                let detail = if p.stderr.is_empty() {
                    String::from_utf8_lossy(&p.stdout).trim().to_string()
                } else {
                    String::from_utf8_lossy(&p.stderr).trim().to_string()
                };
                abandon_and_requeue(domain, &members, root);
                return ShipResult {
                    action: "abandoned".into(),
                    domain: domain.into(),
                    reason: format!(
                        "git push failed: {}",
                        detail.chars().take(200).collect::<String>()
                    ),
                    pr_url: None,
                    pr_number: None,
                    members,
                };
            }
            Err(e) => {
                abandon_and_requeue(domain, &members, root);
                return ShipResult {
                    action: "abandoned".into(),
                    domain: domain.into(),
                    reason: format!("git push failed: {e}"),
                    pr_url: None,
                    pr_number: None,
                    members,
                };
            }
        }
        let created = std::process::Command::new("gh")
            .args([
                "pr",
                "create",
                "--title",
                &pr_title,
                "--body",
                &batch_pr_body(&batch),
                "--base",
                base,
                "--head",
                &branch,
            ])
            .current_dir(&wt)
            .output();
        let created = match created {
            Ok(c) => c,
            Err(e) => {
                abandon_and_requeue(domain, &members, root);
                return ShipResult {
                    action: "abandoned".into(),
                    domain: domain.into(),
                    reason: format!("gh pr create failed: {e}"),
                    pr_url: None,
                    pr_number: None,
                    members,
                };
            }
        };
        if !created.status.success() {
            let detail = if created.stderr.is_empty() {
                String::from_utf8_lossy(&created.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&created.stderr).trim().to_string()
            };
            abandon_and_requeue(domain, &members, root);
            return ShipResult {
                action: "abandoned".into(),
                domain: domain.into(),
                reason: format!(
                    "gh pr create failed: {}",
                    detail.chars().take(200).collect::<String>()
                ),
                pr_url: None,
                pr_number: None,
                members,
            };
        }
        let url = String::from_utf8_lossy(&created.stdout).trim().to_string();
        pr_number = extract_pr_number(&url);
        pr_url = Some(url);
    }
    let Some(url) = pr_url.clone().filter(|u| !u.is_empty()) else {
        abandon_and_requeue(domain, &members, root);
        return ShipResult {
            action: "abandoned".into(),
            domain: domain.into(),
            reason: "no PR url from gh".into(),
            pr_url: None,
            pr_number: None,
            members,
        };
    };
    // Record the shared ref on members BEFORE closing the batch: a failed
    // graph write leaves the batch open, so a later ship-closeable tick
    // reuses the PR (idempotent) and retries. Closing first would strand
    // members batch-marked with no pr_number.
    if let Err(e) = set_member_pr_refs(&members, &url, pr_number) {
        return ShipResult {
            action: "noop".into(),
            domain: domain.into(),
            reason: e,
            pr_url: Some(url),
            pr_number,
            members,
        };
    }
    match close_batch(domain, Some(&url), root) {
        Ok(_) => ShipResult {
            action: "shipped".into(),
            domain: domain.into(),
            reason: String::new(),
            pr_url: Some(url),
            pr_number,
            members,
        },
        Err((message, _)) => ShipResult {
            action: "noop".into(),
            domain: domain.into(),
            reason: message,
            pr_url: Some(url),
            pr_number,
            members,
        },
    }
}

// ---------------------------------------------------------------------------
// Wave-4-trigger metrics: does batching save CI runs net of abandonment waste?
// ---------------------------------------------------------------------------

const BATCH_EVENT_KINDS: [&str; 2] = ["active_backlog_batch_ship", "active_backlog_batch_abandon"];

/// Parse batch ship/abandon envelopes from an events.jsonl file; skips
/// unparseable lines (append-only journals accumulate junk). `since` is an
/// ISO-8601 UTC lower bound compared lexically - safe because the journal's
/// `ts` is always `YYYY-MM-DDTHH:MM:SSZ`.
fn read_batch_events(events_path: &Path, since: Option<&str>) -> Vec<Value> {
    let Ok(text) = std::fs::read_to_string(events_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = ev.get("type").and_then(Value::as_str).unwrap_or("");
        if !BATCH_EVENT_KINDS.contains(&kind) {
            continue;
        }
        if let Some(since) = since {
            let ts = ev.get("ts").and_then(Value::as_str).unwrap_or("");
            if ts < since {
                continue;
            }
        }
        out.push(ev);
    }
    out
}

#[derive(Default)]
struct DomainStats {
    batches_shipped: i64,
    batches_abandoned: i64,
    runs_saved: i64,
    runs_wasted: i64,
}

impl DomainStats {
    fn to_value(&self) -> Value {
        json!({
            "batches_shipped": self.batches_shipped,
            "batches_abandoned": self.batches_abandoned,
            "runs_saved": self.runs_saved,
            "runs_wasted": self.runs_wasted,
        })
    }
    fn finish(&self) -> Value {
        let opened = self.batches_shipped + self.batches_abandoned;
        let net = self.runs_saved - self.runs_wasted;
        let rate = if opened > 0 {
            self.batches_abandoned as f64 / opened as f64
        } else {
            0.0
        };
        json!({
            "batches_shipped": self.batches_shipped,
            "batches_abandoned": self.batches_abandoned,
            "runs_saved": self.runs_saved,
            "runs_wasted": self.runs_wasted,
            "net": net,
            "abandon_rate": (rate * 1000.0).round() / 1000.0,
            "verdict": verdict(self),
        })
    }
}

/// The binary economic comparison: does batching save CI runs net of waste?
fn verdict(s: &DomainStats) -> &'static str {
    let opened = s.batches_shipped + s.batches_abandoned;
    if opened == 0 {
        return "no-data";
    }
    let net = s.runs_saved - s.runs_wasted;
    let abandon_rate = s.batches_abandoned as f64 / opened as f64;
    if net < 0 && abandon_rate > 0.5 {
        return "disable-batching";
    }
    if s.runs_wasted > 0 && (net <= 0 || s.runs_wasted as f64 > 0.4 * s.runs_saved as f64) {
        return "build-wave4";
    }
    "keep-v1"
}

/// Pure rollup of batch ship/abandon events into per-domain stats + verdict.
/// Event shapes: active_backlog_batch_ship carries data.stdout (the
/// ship-closeable JSON); active_backlog_batch_abandon counts only detail=ok.
fn compute_metrics(events: &[Value]) -> Value {
    let mut domains: std::collections::BTreeMap<String, DomainStats> = Default::default();
    for ev in events {
        let data = match ev.get("data") {
            Some(d) if d.is_object() => d,
            _ => continue,
        };
        match ev.get("type").and_then(Value::as_str) {
            Some("active_backlog_batch_ship") => {
                let shipped = data
                    .get("stdout")
                    .and_then(Value::as_str)
                    .and_then(|s| serde_json::from_str::<Value>(s).ok())
                    .and_then(|v| v.get("shipped").and_then(Value::as_array).cloned())
                    .unwrap_or_default();
                for r in &shipped {
                    let action = r.get("action").and_then(Value::as_str).unwrap_or("");
                    let members = r
                        .get("members")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    let domain = r
                        .get("domain")
                        .and_then(Value::as_str)
                        .unwrap_or("code")
                        .to_string();
                    let s = domains.entry(domain).or_default();
                    if action == "shipped" {
                        s.batches_shipped += 1;
                        s.runs_saved += 0.max(members as i64 - 1);
                    } else if action == "abandoned" {
                        s.batches_abandoned += 1;
                        s.runs_wasted += members as i64;
                    }
                }
            }
            Some("active_backlog_batch_abandon") => {
                if data.get("detail").and_then(Value::as_str) != Some("ok") {
                    continue;
                }
                let s = domains
                    .entry(
                        data.get("domain")
                            .and_then(Value::as_str)
                            .unwrap_or("code")
                            .to_string(),
                    )
                    .or_default();
                s.batches_abandoned += 1;
                s.runs_wasted += data
                    .get("member_count")
                    .and_then(Value::as_i64)
                    .unwrap_or(0)
                    .max(0);
            }
            _ => {}
        }
    }
    let mut totals = DomainStats::default();
    for s in domains.values() {
        totals.batches_shipped += s.batches_shipped;
        totals.batches_abandoned += s.batches_abandoned;
        totals.runs_saved += s.runs_saved;
        totals.runs_wasted += s.runs_wasted;
    }
    let top_verdict = verdict(&totals);
    let domains_json: serde_json::Map<String, Value> = domains
        .iter()
        .map(|(d, s)| (d.clone(), s.finish()))
        .collect();
    json!({
        "domains": domains_json,
        "totals": totals.finish(),
        "verdict": top_verdict,
    })
}

fn batch_metrics(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let root = state_root(flag_value(rest, "--root"));
    let since = flag_value(rest, "--since").map(String::from);
    let events = read_batch_events(&root.join(".fno").join("events.jsonl"), since.as_deref());
    let m = compute_metrics(&events);
    if has_flag(rest, "--json") || has_flag(rest, "-J") {
        emit_json(&m);
        return 0;
    }
    let verdict = m
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("no-data");
    if verdict == "no-data" {
        println!("batch metrics: no batch events yet (batching off or never ran)");
        println!("verdict: no-data");
        return 0;
    }
    if let Some(ds) = m.get("domains").and_then(Value::as_object) {
        for (d, s) in ds {
            println!(
                "{d}: shipped={} abandoned={} runs_saved={} runs_wasted={} net={} abandon_rate={} -> {}",
                s.get("batches_shipped").and_then(Value::as_i64).unwrap_or(0),
                s.get("batches_abandoned").and_then(Value::as_i64).unwrap_or(0),
                s.get("runs_saved").and_then(Value::as_i64).unwrap_or(0),
                s.get("runs_wasted").and_then(Value::as_i64).unwrap_or(0),
                s.get("net").and_then(Value::as_i64).unwrap_or(0),
                s.get("abandon_rate").and_then(Value::as_f64).unwrap_or(0.0),
                s.get("verdict").and_then(Value::as_str).unwrap_or("?"),
            );
        }
    }
    let t = m.get("totals");
    println!(
        "totals: shipped={} abandoned={} runs_saved={} runs_wasted={} net={} abandon_rate={}",
        t.and_then(|v| v.get("batches_shipped"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        t.and_then(|v| v.get("batches_abandoned"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        t.and_then(|v| v.get("runs_saved"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        t.and_then(|v| v.get("runs_wasted"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        t.and_then(|v| v.get("net"))
            .and_then(Value::as_i64)
            .unwrap_or(0),
        t.and_then(|v| v.get("abandon_rate"))
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    );
    println!("verdict: {verdict}");
    0
}

fn batch_ship(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let Some(domain) = flag_value(rest, "--domain").or_else(|| flag_value(rest, "-d")) else {
        eprintln!("error: the following required arguments were not provided: --domain");
        return 2;
    };
    let base = flag_value(rest, "--base").unwrap_or("main");
    let title = flag_value(rest, "--title");
    let root = state_root(flag_value(rest, "--root"));
    let result = ship_batch(domain, base, title, &root);
    emit_json(&ship_result_value(&result));
    if result.action == "abandoned" {
        return 2;
    }
    if result.action == "noop" {
        return 3;
    }
    0
}

fn batch_ship_closeable(rest: &[String]) -> i32 {
    if has_flag(rest, "--help") || has_flag(rest, "-h") {
        print!("{BATCH_HELP}");
        return 0;
    }
    let root = state_root(flag_value(rest, "--root"));
    let project = flag_value(rest, "--project").or_else(|| flag_value(rest, "-p"));
    let mission = flag_value(rest, "--mission");
    let shipped = ship_closeable(project, mission, &root);
    emit_json(&json!({ "shipped": shipped }));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ship_events_earn_saved_runs_and_abandons_waste_them() {
        let ship = serde_json::json!({
            "type": "active_backlog_batch_ship",
            "data": {"stdout": "{\"shipped\": [{\"action\": \"shipped\", \"domain\": \"code\", \"members\": [\"a\", \"b\", \"c\"]}]}"}
        });
        let events = vec![ship];
        let m = compute_metrics(&events);
        let totals = m.get("totals").unwrap();
        assert_eq!(
            totals.get("runs_saved").and_then(Value::as_i64),
            Some(2),
            "members - 1 saved runs"
        );
    }

    #[test]
    fn a_failed_abandon_call_counts_nothing() {
        let ev = serde_json::json!({
            "type": "active_backlog_batch_abandon",
            "data": {"detail": "error", "domain": "code", "member_count": 3}
        });
        let m = compute_metrics(std::slice::from_ref(&ev));
        assert_eq!(
            m.get("verdict").and_then(Value::as_str),
            Some("no-data"),
            "detail != ok abandoned nothing"
        );
    }

    #[test]
    fn the_verdict_ladder_matches_the_wheel_thresholds() {
        let mut s = DomainStats::default();
        assert_eq!(verdict(&s), "no-data");
        s.runs_saved = 10;
        s.runs_wasted = 8;
        s.batches_abandoned = 6;
        s.batches_shipped = 2;
        assert_eq!(verdict(&s), "disable-batching", "net<0 and rate>0.5");
        let mut s = DomainStats::default();
        s.runs_saved = 4;
        s.runs_wasted = 3;
        s.batches_shipped = 2;
        s.batches_abandoned = 1;
        assert_eq!(verdict(&s), "build-wave4", "waste > 0.4 * saved");
        let mut s = DomainStats::default();
        s.runs_saved = 10;
        s.runs_wasted = 1;
        s.batches_shipped = 4;
        assert_eq!(verdict(&s), "keep-v1");
    }

    #[test]
    fn the_pr_number_reader_takes_urls_and_bare_numbers() {
        assert_eq!(
            extract_pr_number("https://github.com/o/r/pull/4123"),
            Some(4123)
        );
        assert_eq!(extract_pr_number("4123"), Some(4123));
        assert_eq!(extract_pr_number("no number here"), None);
    }

    #[test]
    fn the_domain_guard_refuses_paths_and_traversal() {
        assert!(safe_domain("code").is_ok());
        assert!(safe_domain("a/b").is_err());
        assert!(safe_domain("..").is_err());
        assert!(safe_domain("").is_err());
    }
}
