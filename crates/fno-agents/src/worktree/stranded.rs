//! The stranded-worktree classifier, ported from the deleted
//! `cli/src/fno/worktree_stranded.py`: the pure classify join, the
//! deterministic node resolution tiers, the sweep driver, and the recovery
//! acts. Characterized by the `stranded_classify_*`, `resolve_node_*`,
//! `sweep_*` and `apply_*` goldens.

use super::{git_bounded, git_out, sorted_json, OWNERSHIP_LIVE_STATUSES};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

const FETCH_TIMEOUT_SECS: u64 = 15;
const PUSH_TIMEOUT_SECS: u64 = 30;
const QUIET_TERMINAL_STATUSES: [&str; 2] = ["superseded", "deferred"];

/// The class vocabulary. CLEAN and the quiet classes are informational;
/// only STRANDED is ever acted on; UNKNOWN is fail-open.
pub const CLEAN: &str = "CLEAN";
pub const UNKNOWN: &str = "UNKNOWN";
pub const SHIPPED: &str = "SHIPPED";
pub const ABANDONED: &str = "ABANDONED";
pub const LIVE: &str = "LIVE";
pub const PR_OPEN: &str = "PR_OPEN";
pub const STRANDED: &str = "STRANDED";

pub struct Row {
    pub klass: String,
    pub node: Option<String>,
    pub unpushed: i64,
    pub age: String,
    pub facts: serde_json::Map<String, Value>,
}

/// First match wins. See the module docstring of the deleted Python leg for
/// the shape of the join; the goldens carry the table.
pub fn classify(
    path: &str,
    branch: Option<&str>,
    unpushed: i64,
    unpushed_ok: bool,
    node: Option<&str>,
    node_entry: Option<&Value>,
    graph_ok: bool,
    registry_status: Option<&str>,
    registry_ok: bool,
    age: &str,
    has_remote: Option<bool>,
) -> Row {
    let mut facts = serde_json::Map::new();
    facts.insert("path".to_string(), Value::String(path.to_string()));
    facts.insert(
        "branch".to_string(),
        branch
            .map(|b| Value::String(b.to_string()))
            .unwrap_or(Value::Null),
    );
    facts.insert(
        "has_remote".to_string(),
        has_remote.map(Value::Bool).unwrap_or(Value::Null),
    );
    facts.insert(
        "pr_number".to_string(),
        node_entry
            .and_then(|e| e.get("pr_number"))
            .cloned()
            .unwrap_or(Value::Null),
    );
    facts.insert(
        "live".to_string(),
        Value::Bool(
            registry_status
                .map(|s| OWNERSHIP_LIVE_STATUSES.contains(&s))
                .unwrap_or(false),
        ),
    );
    let row = Row {
        klass: String::new(),
        node: node.map(|n| n.to_string()),
        unpushed,
        age: age.to_string(),
        facts,
    };
    classify_tiers(
        row,
        unpushed,
        unpushed_ok,
        graph_ok,
        registry_status,
        registry_ok,
        node_entry,
    )
}

#[allow(clippy::too_many_arguments)]
fn classify_tiers(
    mut row: Row,
    unpushed: i64,
    unpushed_ok: bool,
    graph_ok: bool,
    registry_status: Option<&str>,
    registry_ok: bool,
    node_entry: Option<&Value>,
) -> Row {
    // A failed read answers UNKNOWN before "node unresolved": the graph
    // failure also empties the id map, so reporting node-unresolved would
    // hide the real failure.
    if unpushed == 0 {
        row.klass = CLEAN.to_string();
        return row;
    }
    if !(unpushed_ok && graph_ok && registry_ok) {
        let failed: Vec<&str> = [
            ("git", unpushed_ok),
            ("graph", graph_ok),
            ("fleet", registry_ok),
        ]
        .iter()
        .filter(|(_, ok)| !ok)
        .map(|(name, _)| *name)
        .collect();
        row.klass = UNKNOWN.to_string();
        row.facts.insert(
            "reason".to_string(),
            Value::String(format!("read failed: {}", failed.join(","))),
        );
        return row;
    }
    if row.node.is_none() || node_entry.is_none() {
        row.klass = UNKNOWN.to_string();
        row.facts.insert(
            "reason".to_string(),
            Value::String("node unresolved".to_string()),
        );
        return row;
    }
    if let Some(status) = registry_status {
        if OWNERSHIP_LIVE_STATUSES.contains(&status) {
            row.klass = LIVE.to_string();
            return row;
        }
    }
    let entry = node_entry.unwrap_or(&Value::Null);
    let status = entry.get("status").and_then(Value::as_str).unwrap_or("");
    if status == "done" {
        row.klass = SHIPPED.to_string();
        return row;
    }
    if QUIET_TERMINAL_STATUSES.contains(&status) {
        row.klass = ABANDONED.to_string();
        return row;
    }
    if entry.get("pr_number").map(is_truthy).unwrap_or(false) {
        row.klass = PR_OPEN.to_string();
        return row;
    }
    row.klass = STRANDED.to_string();
    row
}

/// Python truthiness for the pr_number read: a nonzero number or a
/// non-empty string.
fn is_truthy(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(false),
        Value::String(s) => !s.is_empty(),
        Value::Bool(b) => *b,
        Value::Null => false,
        _ => true,
    }
}

/// Worktree-to-node resolution, first hit wins: the directory basename, then
/// the branch's last `/`-segment, then the manifest's own graph_node_id line
/// (which must name a row in the map).
pub fn resolve_node_id(
    path: &str,
    branch: Option<&str>,
    rows: &[Value],
) -> Option<(String, Value)> {
    let basename = Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let branch_tail = branch.and_then(|b| b.rsplit('/').next().map(str::to_string));
    for candidate in [basename, branch_tail].into_iter().flatten() {
        if let Some(hit) = resolve_node(&candidate, rows) {
            return Some(hit);
        }
    }
    state_file_arm(path, rows)
}

/// The `graph_node_id:` line in `<path>/.fno/target-state.md`, when it names
/// a row in the map.
fn state_file_arm(path: &str, rows: &[Value]) -> Option<(String, Value)> {
    let manifest = Path::new(path).join(".fno").join("target-state.md");
    let text = std::fs::read_to_string(manifest).ok()?;
    let wanted = manifest_line(&text)?;
    let by_id: std::collections::HashMap<&str, &Value> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(|i| (i, r)))
        .collect();
    let row = by_id.get(wanted.as_str())?;
    Some((wanted, (*row).clone()))
}

// --- the sweep driver -------------------------------------------------------

static REMOTE_REFS_FRESH: AtomicBool = AtomicBool::new(false);
static REMOTE_REFS_STALE: AtomicBool = AtomicBool::new(false);

/// (count, ok); ok false means the read itself is untrustworthy. FAIL TOWARD
/// KEEP: only a literal 0 says "nothing unique"; a failed fetch caches too.
fn wt_unpushed_count(path: &str) -> (i64, bool) {
    if path.is_empty() || !Path::new(path).is_dir() {
        return (1, false);
    }
    if !REMOTE_REFS_FRESH.load(Ordering::SeqCst) {
        if REMOTE_REFS_STALE.load(Ordering::SeqCst) {
            return (1, false);
        }
        let fetch = git_bounded(
            Path::new(path),
            &["fetch", "--all", "--prune"],
            FETCH_TIMEOUT_SECS,
        );
        let ok = fetch.map(|o| o.status.success()).unwrap_or(false);
        if !ok {
            REMOTE_REFS_STALE.store(true, Ordering::SeqCst);
            return (1, false);
        }
        REMOTE_REFS_FRESH.store(true, Ordering::SeqCst);
    }
    let rev = git_out(
        Path::new(path),
        &["rev-list", "--count", "HEAD", "--not", "--remotes"],
    );
    match rev {
        Some(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            match text.trim().parse::<i64>() {
                Ok(n) => (n, true),
                Err(_) => (1, false),
            }
        }
        _ => (1, false),
    }
}

/// Some(true/false) resolving/absent; None when the probe itself fails.
fn has_remote(branch: Option<&str>, path: &str) -> Option<bool> {
    let branch = branch?;
    let out = git_out(
        Path::new(path),
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/remotes/origin/{branch}"),
        ],
    )?;
    match out.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

/// git log -1 --format=%cr stripped, or "unknown".
fn commit_age(path: &str) -> String {
    match git_out(Path::new(path), &["log", "-1", "--format=%cr"]) {
        Some(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if text.is_empty() {
                "unknown".to_string()
            } else {
                text
            }
        }
        _ => "unknown".to_string(),
    }
}

/// Classify every worktree registered to `repo`. The graph read is strict:
/// an unreadable store fails the leg, never silently zeroes it.
pub fn sweep(repo: &Path) -> Vec<Row> {
    let listing = match git_out(repo, &["worktree", "list", "--porcelain"]) {
        Some(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
        _ => String::new(),
    };
    let worktrees = super::parse_porcelain_rows(&listing);
    // The layout table's anchor, not a bare <state>/graph.db: a migrated
    // install keeps the store at <state>/db/graph.db, and a strict read of
    // the legacy spelling would fail the leg on every normal installation
    // and classify every ahead worktree UNKNOWN.
    let mut entries: Vec<Value> = Vec::new();
    let mut graph_ok = true;
    match crate::graph_store::read_rows_strict(&crate::state_layout::graph_anchor(
        &super::state_root(),
    )) {
        Ok(rows) => entries = rows,
        Err(_) => graph_ok = false,
    }
    let (join_map, registry_ok) = super::status::registry_join();
    sweep_over(worktrees, entries, graph_ok, join_map, registry_ok)
}

fn sweep_over(
    worktrees: Vec<(Option<String>, String)>,
    entries: Vec<Value>,
    graph_ok: bool,
    join_map: super::status::JoinMap,
    registry_ok: bool,
) -> Vec<Row> {
    let mut rows = Vec::new();
    for (branch, path) in worktrees {
        let (unpushed, unpushed_ok) = wt_unpushed_count(&path);
        let remote = has_remote(branch.as_deref(), &path);
        let age = commit_age(&path);
        let Some((node_id, node_entry)) = resolve_node_id(&path, branch.as_deref(), &entries)
        else {
            let registry_status = join_map.get(&path).map(|(_, s)| s.clone());
            let row = classify(
                &path,
                branch.as_deref(),
                unpushed,
                unpushed_ok,
                None,
                None,
                graph_ok,
                registry_status.as_deref(),
                registry_ok,
                &age,
                remote,
            );
            rows.push(row);
            continue;
        };
        let registry_status = join_map.get(&path).map(|(_, s)| s.clone());
        let row = classify(
            &path,
            branch.as_deref(),
            unpushed,
            unpushed_ok,
            Some(node_id.as_str()),
            Some(&node_entry),
            graph_ok,
            registry_status.as_deref(),
            registry_ok,
            &age,
            remote,
        );
        rows.push(row);
    }
    rows
}

// --- recovery acts (STRANDED only) + UNKNOWN recording ----------------------

/// The in-process event append, judged by the store like every writer.
fn emit_sweep_event(row: &Row, sha: Option<&str>, acts: Vec<String>) -> bool {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::AutoSi, false);
    let data = json!({
        "path": row.facts.get("path").cloned().unwrap_or(Value::Null),
        "branch": row.facts.get("branch").cloned().unwrap_or(Value::Null),
        "class": row.klass,
        "unpushed": row.unpushed,
        "sha": sha.map(|s| Value::String(s.to_string())).unwrap_or(Value::Null),
        "acts": acts,
        "reason": row.facts.get("reason").cloned().unwrap_or(Value::Null),
        "node": row.node,
    });
    let envelope = json!({
        "type": "stranded_sweep",
        "source": "daemon",
        "ts": now,
        "data": data,
    });
    let journal = super::policy::events_journal();
    crate::event_store::append_envelope(&journal, &envelope.to_string(), None).is_ok()
}

pub fn act_on_stranded(row: &Row) -> Value {
    let path = row.facts.get("path").and_then(Value::as_str).unwrap_or("");
    let branch = row.facts.get("branch").and_then(Value::as_str);
    let node = row.node.clone().unwrap_or_default();
    let sha = git_out(Path::new(path), &["rev-parse", "HEAD"])
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let (push_branch, push) = match branch {
        Some(br) => (
            br.to_string(),
            git_bounded(
                Path::new(path),
                &["push", "-u", "origin", br],
                PUSH_TIMEOUT_SECS,
            ),
        ),
        None => {
            let recovered = format!("recovered/{node}");
            (
                recovered.clone(),
                git_bounded(
                    Path::new(path),
                    &["push", "origin", &format!("HEAD:refs/heads/{recovered}")],
                    PUSH_TIMEOUT_SECS,
                ),
            )
        }
    };
    let push_ok = push.as_ref().map(|o| o.status.success()).unwrap_or(false);
    let detail = push
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stderr).trim().to_string();
            text.chars().take(500).collect::<String>()
        })
        .unwrap_or_default();
    let acts = vec![json!({
        "act": "push",
        "branch": push_branch,
        "ok": push_ok,
        "detail": detail,
    })];
    if !push_ok {
        return json!({
            "node": row.node,
            "class": row.klass,
            "acts": acts,
            "stopped_at": "push",
        });
    }
    let ev_ok = emit_sweep_event(row, Some(&sha), vec!["push".to_string()]);
    json!({
        "node": row.node,
        "class": row.klass,
        "acts": [
            acts[0].clone(),
            {"act": "event_emit", "ok": ev_ok},
        ],
        "stopped_at": if ev_ok { Value::Null } else { Value::String("event_emit".into()) },
    })
}

fn record_unknown(row: &Row) -> Value {
    let ev_ok = emit_sweep_event(row, None, vec![]);
    json!({
        "node": row.node,
        "class": row.klass,
        "acts": [{"act": "event_emit", "ok": ev_ok}],
        "stopped_at": if ev_ok { Value::Null } else { Value::String("event_emit".into()) },
    })
}

/// The `worktree-stranded` op: `--repo R [--json] [--apply] [--no-wake]`.
///
/// The deleted leg's contract: the default is classify-and-print only, so
/// without `--apply` nothing acts and `outcomes` stays empty. `--apply`
/// implies the wake (push + file for STRANDED, record for UNKNOWN) unless
/// `--no-wake` names the report posture explicitly - the shape the pr-watch
/// leg sends, and the reason the flag survives a wake-implying apply.
pub fn run_client(args: &[String]) -> i32 {
    let mut repo = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut as_json = false;
    let mut apply = false;
    let mut no_wake = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--repo" if i + 1 < args.len() => {
                repo = PathBuf::from(&args[i + 1]);
                i += 2;
            }
            a if crate::json_output::is_flag(a) => {
                as_json = true;
                i += 1;
            }
            "--apply" => {
                apply = true;
                i += 1;
            }
            "--no-wake" => {
                no_wake = true;
                i += 1;
            }
            other => {
                eprintln!("fno-agents worktree-stranded: unexpected argument {other}");
                return 2;
            }
        }
    }
    let wake = apply && !no_wake;
    run_stranded_print(&repo, as_json, apply, wake)
}

fn run_stranded_print(repo: &Path, as_json: bool, apply: bool, wake: bool) -> i32 {
    let rows = sweep(repo);
    // Nothing acts or records without --apply: the deleted leg's
    // classify-and-print default, and the reason a plain `stranded --json`
    // stays a read.
    let mut outcomes: Vec<Value> = Vec::new();
    if apply {
        for row in &rows {
            if row.klass == STRANDED {
                if wake {
                    outcomes.push(act_on_stranded(row));
                }
            } else if row.klass == UNKNOWN {
                outcomes.push(record_unknown(row));
            }
        }
    }
    if as_json {
        let payload = json!({"rows": rows_payload(&rows), "outcomes": outcomes});
        println!("{}", sorted_json(&payload));
    } else {
        for r in &rows {
            println!(
                "  {:<10} | {:<12} | {:>4} unpushed | {:<15} | {}",
                r.klass,
                r.node.clone().unwrap_or_else(|| "-".to_string()),
                r.unpushed,
                r.age,
                r.facts.get("path").and_then(Value::as_str).unwrap_or(""),
            );
        }
        if apply {
            let failed = outcomes
                .iter()
                .filter(|o| o.get("stopped_at").and_then(Value::as_str).is_some())
                .count();
            println!(
                "applied: {} rows acted on, {} stopped early",
                outcomes.len(),
                failed
            );
        }
    }
    0
}

fn rows_payload(rows: &[Row]) -> Vec<Value> {
    rows.iter()
        .map(|r| {
            json!({
                "class": r.klass,
                "node": r.node,
                "unpushed": r.unpushed,
                "age": r.age,
                "facts": r.facts,
            })
        })
        .collect()
}

/// The `worktree-resolve-node` op: `--path P --branch B`.
pub fn resolve_node_client(args: &[String]) -> i32 {
    let mut path: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--path" if i + 1 < args.len() => {
                path = Some(args[i + 1].clone());
                i += 2;
            }
            "--branch" if i + 1 < args.len() => {
                branch = Some(args[i + 1].clone());
                i += 2;
            }
            other => {
                eprintln!("fno-agents worktree-resolve-node: unexpected argument {other}");
                return 2;
            }
        }
    }
    let Some(path) = path else {
        eprintln!("fno-agents worktree-resolve-node: --path is required");
        return 2;
    };
    let path_str = super::resolve_lenient(Path::new(&path))
        .to_string_lossy()
        .into_owned();
    let rows = crate::graph_store::read_rows_strict(&super::state_root().join("graph.db"))
        .unwrap_or_default();
    let hit = resolve_node_id(&path_str, branch.as_deref(), &rows);
    let payload = match hit {
        Some((id, _)) => json!({"node": id}),
        None => json!({"node": null}),
    };
    println!("{}", sorted_json(&payload));
    0
}

/// The body line `graph_node_id: <id>` of a target manifest, anchored at
/// column 0 like the Python multiline regex.
fn manifest_line(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("graph_node_id:") {
            let id = rest.trim();
            if !id.is_empty() {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// The deterministic resolution tiers: exact id, exact slug
/// (case-insensitive), bare 4-8 lowercase-hex re-prefix. Never fuzzy.
pub fn resolve_node(query: &str, rows: &[Value]) -> Option<(String, Value)> {
    let q = query.trim();
    if q.is_empty() || q.starts_with('-') {
        return None;
    }
    for row in rows {
        if row.get("id").and_then(Value::as_str) == Some(q) {
            return Some((q.to_string(), row.clone()));
        }
    }
    let q_lc = q.to_lowercase();
    for row in rows {
        let slug = row.get("slug").and_then(Value::as_str);
        if slug == Some(q_lc.as_str()) && !q_lc.is_empty() {
            let id = row.get("id").and_then(Value::as_str)?.to_string();
            return Some((id, row.clone()));
        }
    }
    bare_hex_tier(q, rows)
}

fn bare_hex_tier(q: &str, rows: &[Value]) -> Option<(String, Value)> {
    let hex_ok = (4..=8).contains(&q.len())
        && q.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    if !hex_ok {
        return None;
    }
    let configured = super::policy::node_id_prefix(&std::env::current_dir().unwrap_or_default());
    let mut prefixes = vec![configured, "ab-".to_string()];
    prefixes.dedup();
    let by_id: std::collections::HashMap<&str, &Value> = rows
        .iter()
        .filter_map(|r| r.get("id").and_then(Value::as_str).map(|i| (i, r)))
        .collect();
    for p in prefixes {
        let cand = format!("{p}{q}");
        if let Some(row) = by_id.get(cand.as_str()) {
            return Some((cand, (*row).clone()));
        }
    }
    None
}
