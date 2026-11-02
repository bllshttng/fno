//! The read shape of `fno backlog provenance`: the birth and spawn edges with
//! their transcripts, the origin and related links, the spawned walk, the
//! runtime attempts, and the per-phase lifecycle roster. Read-only. A runtime
//! attempt is a read-time join over manifests and the node claim: it never
//! labels an inferred attempt as a confirmed `do` row.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::birth::resolve_transcript_detail;
use super::done_rich::read_external;
use super::node_ref::resolve_tiers;
use super::render::py_json_pretty;
use super::settings;
use super::workflows::{active_backend_name, text_at};
use crate::graph_store;

const SPAWNED_MAX_DEPTH: usize = 10;
const LIFECYCLE_PHASES: [&str; 5] = ["think", "blueprint", "execute", "review", "ship"];
const IDENTITY_FIELDS: [&str; 6] = [
    "harness",
    "harness_session_id",
    "fno_id",
    "graph_node_id",
    "target_claim_key",
    "target_claim_holder",
];

pub(super) const PROVENANCE_HELP: &str = "Usage: fno backlog provenance <id> [OPTIONS]

Show the provenance pointers of a node and resolve transcripts where possible.
Read-only: no graph mutation.

Options:
      --spawned          Also walk the origin edge in reverse: every node this one produced.
  -J, --json             Emit JSON.
      --stamp-launch     Record the node's first launch.
      --backfill MAP     Backfill launches from a receipted map. Dry run without --apply.
  -h, --help             Print help.
";

/// One transcript resolution in the shape the JSON edges carry.
fn resolve_edge(harness: Option<&str>, session: &str, cwd: Option<&str>) -> Value {
    let harness_word = harness.unwrap_or("");
    let found = resolve_transcript_detail(harness_word, session, cwd.unwrap_or(""));
    let path = found.path.as_ref().map(|p| p.display().to_string());
    let kind = if harness_word == "opencode" && path.is_some() {
        "opencode-db"
    } else {
        "jsonl"
    };
    json!({
        "harness": harness,
        "session_id": session,
        "cwd": cwd,
        "resolved": path.is_some(),
        "transcript_path": path,
        "reason": found.reason,
        "ambiguous": found.ambiguous,
        "kind": kind,
    })
}

fn edge_json(label: &str, result: Option<&Value>) -> Value {
    match result {
        None => json!({"edge": label, "session_id": null, "resolved": false}),
        Some(found) => {
            let mut out = found.clone();
            out["edge"] = json!(label);
            out
        }
    }
}

fn birth_edge(node: &Value) -> Option<Value> {
    let session = text_at(node, "source_session_id").filter(|s| !s.is_empty())?;
    // The originating SESSION cwd: transcript dirs slug by it. Legacy nodes
    // carry only the durable project cwd.
    let cwd = text_at(node, "source_cwd")
        .filter(|c| !c.is_empty())
        .or_else(|| text_at(node, "cwd"));
    Some(resolve_edge(text_at(node, "source_harness"), session, cwd))
}

fn spawn_edge(node: &Value) -> Option<Value> {
    let session = text_at(node, "spawned_by_session").filter(|s| !s.is_empty())?;
    Some(resolve_edge(
        text_at(node, "spawned_by_harness"),
        session,
        text_at(node, "spawned_by_cwd"),
    ))
}

fn pr_block(row: &Value) -> Value {
    json!({"number": row.get("pr_number"), "url": row.get("pr_url")})
}

fn render_pr_line(pr: &Value) -> String {
    let url = pr["url"].as_str().filter(|u| !u.is_empty());
    match (pr["number"].as_i64(), url) {
        (None, None) => "  pr: (none)".to_string(),
        (None, Some(url)) => format!("  pr: {url}"),
        (Some(n), None) => format!("  pr: #{n}"),
        (Some(n), Some(url)) => format!("  pr: #{n} {url}"),
    }
}

/// `harness_session_id` to registry status. None when the registry cannot be
/// read: liveness is then omitted rather than every row reading reaped.
fn registry_status_index() -> Option<HashMap<String, String>> {
    let registry = crate::paths::AgentsHome::shared_registry_json();
    let rows = crate::client_commands::load_registry_entries(&registry).ok()?;
    Some(
        rows.iter()
            .filter_map(|row| {
                let sid = text_at(row, "harness_session_id").filter(|s| !s.is_empty())?;
                let status = text_at(row, "status").filter(|s| !s.is_empty());
                Some((sid.to_string(), status.unwrap_or("unknown").to_string()))
            })
            .collect(),
    )
}

fn registry_status_of(
    session: Option<&str>,
    index: Option<&HashMap<String, String>>,
) -> Option<String> {
    let (index, session) = (index?, session.filter(|s| !s.is_empty())?);
    Some(
        index
            .get(session)
            .cloned()
            .unwrap_or_else(|| "reaped".to_string()),
    )
}

fn observed_model_of(row: &Value) -> Option<String> {
    let observed = row.get("observed_model")?;
    if text_at(observed, "kind") != Some("observed") {
        return None;
    }
    text_at(observed, "model")
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// A duration is honest only when both canonical names parse in order. Legacy
/// rows (claimed_at/at) hold stamp-fire time, so they never sum into a total,
/// and an inverted window is a backfill typo, not a phase duration.
fn honest_duration(row: &Value) -> Option<f64> {
    let parse = |key: &str| chrono::DateTime::parse_from_rfc3339(text_at(row, key)?).ok();
    let (start, end) = (parse("started_at")?, parse("ended_at")?);
    let ms = (end - start).num_milliseconds();
    (ms >= 0).then(|| ms as f64 / 1000.0)
}

fn fmt_duration(sec: f64) -> String {
    let sec = sec.round() as i64;
    match sec {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s => format!("{}h{}m", s / 3600, (s % 3600) / 60),
    }
}

/// The per-phase roster: human lines and the summary. Every phase renders, so
/// a roster never reads "nobody touched this" by omission.
fn lifecycle_roster(
    sessions: &[Value],
    index: Option<&HashMap<String, String>>,
) -> (Vec<String>, Value) {
    let mut lines = Vec::new();
    let mut phases = Vec::new();
    let mut total = 0.0;
    let mut with_window = 0;
    for phase in LIFECYCLE_PHASES {
        let rows: Vec<&Value> = sessions
            .iter()
            .filter(|s| text_at(s, "phase") == Some(phase))
            .collect();
        if rows.is_empty() {
            lines.push(format!("    {phase:<9} not recorded"));
            phases.push(json!({"phase": phase, "recorded": false}));
            continue;
        }
        let mut has_window = false;
        for row in rows {
            let start = text_at(row, "started_at").or_else(|| text_at(row, "claimed_at"));
            let end = text_at(row, "ended_at").or_else(|| text_at(row, "at"));
            let duration = honest_duration(row);
            let session = text_at(row, "session_id");
            let status = registry_status_of(session, index);
            let model = observed_model_of(row);
            let mut bits = Vec::new();
            if let Some(status) = &status {
                bits.push(format!("[{status}]"));
            }
            if let Some(model) = &model {
                bits.push(model.clone());
            }
            let annot = if bits.is_empty() {
                String::new()
            } else {
                format!(" {}", bits.join(" "))
            };
            let head = format!(
                "    {phase:<9} {}:{}",
                text_at(row, "harness").unwrap_or("?"),
                session.unwrap_or("?")
            );
            lines.push(match (duration, end, start) {
                (Some(sec), _, _) => {
                    total += sec;
                    has_window = true;
                    format!(
                        "{head} {} -> {} ({}){annot}",
                        text_at(row, "started_at").unwrap_or(""),
                        text_at(row, "ended_at").unwrap_or(""),
                        fmt_duration(sec)
                    )
                }
                (None, Some(end), _) => format!("{head} end only @ {end}{annot}"),
                (None, None, Some(start)) => {
                    format!("{head} in progress (since {start}){annot}")
                }
                (None, None, None) => format!("{}{annot}", head.trim_end()),
            });
            phases.push(json!({
                "phase": phase,
                "recorded": true,
                "harness": row.get("harness"),
                "session_id": row.get("session_id"),
                "start": start,
                "end": end,
                "duration_seconds": duration,
                "registry_status": status,
                "observed_model": model,
            }));
        }
        if has_window {
            with_window += 1;
        }
    }
    let phase_total = LIFECYCLE_PHASES.len();
    // Keyed on the window count, not the total: a zero-second window still
    // recorded a phase.
    lines.push(if with_window > 0 {
        format!(
            "    total     {} ({with_window} of {phase_total} phases recorded)",
            fmt_duration(total)
        )
    } else {
        format!("    total     {with_window} of {phase_total} phases recorded")
    });
    let summary = json!({
        "phases": phases,
        "total_duration_seconds": (with_window > 0).then_some(total),
        "phases_recorded": with_window,
        "phases_total": phase_total,
    });
    (lines, summary)
}

struct SpawnedWalk {
    rows: Vec<(usize, Value)>,
    cycle: bool,
    truncated: bool,
}

/// `source_node_id` in reverse, breadth-first, so depth is the shortest path.
/// A cycle truncates and keeps what was found.
fn spawned_walk(entries: &[Value], root_id: &str) -> SpawnedWalk {
    let mut by_source: HashMap<&str, Vec<&Value>> = HashMap::new();
    for e in entries {
        if let Some(src) = text_at(e, "source_node_id").filter(|s| !s.is_empty()) {
            by_source.entry(src).or_default().push(e);
        }
    }
    let mut rows = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([root_id.to_string()]);
    let mut frontier = vec![root_id.to_string()];
    let mut cycle = false;
    let mut depth = 0;
    while !frontier.is_empty() && depth < SPAWNED_MAX_DEPTH {
        depth += 1;
        let mut next = Vec::new();
        for parent in &frontier {
            let mut children = by_source.get(parent.as_str()).cloned().unwrap_or_default();
            children.sort_by_key(|c| text_at(c, "id").unwrap_or(""));
            for child in children {
                let Some(id) = text_at(child, "id") else {
                    continue;
                };
                if !seen.insert(id.to_string()) {
                    cycle = true;
                    continue;
                }
                rows.push((depth, child.clone()));
                next.push(id.to_string());
            }
        }
        frontier = next;
    }
    // Truncated only if something was cut: a chain ending exactly at the cap
    // leaves a frontier with nothing below it.
    let truncated = frontier.iter().any(|parent| {
        by_source.get(parent.as_str()).is_some_and(|children| {
            children
                .iter()
                .any(|c| text_at(c, "id").is_some_and(|id| !seen.contains(id)))
        })
    });
    SpawnedWalk {
        rows,
        cycle,
        truncated,
    }
}

/// `key: value` scalars, frontmatter first, first value wins. Never fails.
fn read_manifest(root: &Path) -> Option<Map<String, Value>> {
    let path = crate::paths::worktree_space_dir_opt(root)?.join("target-state.md");
    let text = std::fs::read_to_string(path).ok()?;
    let mut raw = Map::new();
    for line in text.lines() {
        let line = line.trim();
        if line == "---" || line.is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches(|c| c == '"' || c == '\'');
        if !value.is_empty() && value != "null" && !raw.contains_key(key.trim()) {
            raw.insert(key.trim().to_string(), json!(value));
        }
    }
    (!raw.is_empty()).then_some(raw)
}

fn manifest_identity(raw: &Map<String, Value>) -> Option<HashMap<&'static str, String>> {
    let mut ident = HashMap::new();
    for field in IDENTITY_FIELDS {
        ident.insert(field, raw.get(field)?.as_str()?.to_string());
    }
    Some(ident)
}

fn worktree_roots(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.to_path_buf()];
    for row in crate::worktree::status::list_worktrees(root) {
        let path = PathBuf::from(row.worktree_path);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// Commits ahead of origin/main (origin/master when main is absent). None is
/// unknowable, never a zero that would suppress the interrupted verdict.
fn commits_ahead(root: &Path, branch: Option<&str>) -> Option<i64> {
    let branch = branch?;
    for base in ["origin/main", "origin/master"] {
        let Ok(out) = Command::new("git")
            .args(["rev-list", "--count", &format!("{base}..{branch}")])
            .current_dir(root)
            .output()
        else {
            continue;
        };
        if out.status.success() {
            if let Ok(n) = String::from_utf8_lossy(&out.stdout).trim().parse() {
                return Some(n);
            }
        }
    }
    None
}

/// The conservative attempt classifier, separate from the raw claim state.
fn classify(claim_state: Option<&str>, has_work: bool, node_terminal: bool) -> &'static str {
    match claim_state {
        Some("live") => "live",
        Some("suspect") | Some("stale") | Some("free") | Some("corrupted") | None => {
            if has_work && !node_terminal {
                "interrupted"
            } else if claim_state == Some("suspect") {
                "suspect"
            } else {
                "stale"
            }
        }
        Some(_) => "suspect",
    }
}

fn attempt_rank(state: &str) -> u8 {
    match state {
        "live" => 4,
        "interrupted" => 3,
        "suspect" => 2,
        "stale" => 1,
        _ => 0,
    }
}

fn runtime_attempts(node_id: &str, node: &Value) -> Vec<Value> {
    let root = text_at(node, "cwd")
        .filter(|c| !c.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let (state, record) = crate::claims::status(&format!("node:{node_id}"), None);
    let live_state = format!("{state:?}").to_lowercase();
    let holder = record.as_ref().map(|r| r.holder.clone());
    let pid = record.as_ref().and_then(|r| r.pid);
    let terminal = matches!(text_at(node, "status"), Some("done") | Some("superseded"));
    let pr_number = node.get("pr_number").cloned().unwrap_or(Value::Null);
    let mut order: Vec<String> = Vec::new();
    let mut rows: HashMap<String, Value> = HashMap::new();
    for worktree in worktree_roots(&root) {
        let Some(raw) = read_manifest(&worktree) else {
            continue;
        };
        let Some(ident) = manifest_identity(&raw) else {
            continue;
        };
        if ident["graph_node_id"] != node_id {
            continue;
        }
        let branch = ["worktree_branch", "branch"]
            .iter()
            .find_map(|k| raw.get(*k).and_then(Value::as_str));
        let commits = commits_ahead(&worktree, branch);
        let has_work =
            commits.is_some_and(|n| n != 0) || pr_number.as_i64().is_some_and(|n| n != 0);
        // The node claim is singular, so it names at most one attempt's owner.
        // Every other manifest is a historical attempt and classifies from its
        // own work, never from a claim another session now holds.
        let owner = holder.as_deref() == Some(ident["target_claim_holder"].as_str());
        let (row_state, row_holder, row_pid) = if owner {
            (Some(live_state.as_str()), holder.clone(), pid)
        } else {
            (None, Some(ident["target_claim_holder"].clone()), None)
        };
        let attempt = classify(row_state, has_work, terminal);
        let fno_id = ident["fno_id"].clone();
        let known = rows.get(&fno_id);
        if known.is_some_and(|prev| {
            attempt_rank(prev["attempt_state"].as_str().unwrap_or("")) >= attempt_rank(attempt)
        }) {
            continue;
        }
        if known.is_none() {
            order.push(fno_id.clone());
        }
        rows.insert(
            fno_id.clone(),
            json!({
                "fno_id": fno_id,
                "harness": ident["harness"],
                "harness_session_id": ident["harness_session_id"],
                "node": node_id,
                "worktree": worktree.display().to_string(),
                "claim_state": row_state,
                "claim_holder": row_holder,
                "claim_pid": row_pid,
                "commits_ahead": commits,
                "pr_number": pr_number,
                "attempt_state": attempt,
                "lifecycle": "unconfirmed do",
            }),
        );
    }
    order.iter().filter_map(|id| rows.remove(id)).collect()
}

fn render_runtime(lines: &mut Vec<String>, runtime: &[Value]) {
    if runtime.is_empty() {
        return;
    }
    lines.push("  runtime:".to_string());
    for a in runtime {
        let mut work = Vec::new();
        if let Some(n) = a["commits_ahead"].as_i64().filter(|n| *n != 0) {
            work.push(format!("{n} commits"));
        }
        if let Some(n) = a["pr_number"].as_i64().filter(|n| *n != 0) {
            work.push(format!("PR #{n}"));
        }
        let work = if work.is_empty() {
            "(no work evidence)".to_string()
        } else {
            work.join(", ")
        };
        let word = |key: &str| a[key].as_str().unwrap_or("None").to_string();
        lines.push(format!(
            "    {:<11} {}:{}",
            word("attempt_state"),
            word("harness"),
            word("harness_session_id")
        ));
        lines.push(format!("      run:       {}", word("fno_id")));
        lines.push(format!("      worktree:  {}", word("worktree")));
        let pid = a["claim_pid"]
            .as_i64()
            .map_or("None".to_string(), |p| p.to_string());
        lines.push(format!(
            "      claim:     {} pid={pid}",
            word("claim_state")
        ));
        lines.push(format!("      work:      {work}"));
        lines.push(format!("      lifecycle: {}", word("lifecycle")));
    }
}

fn fmt_edge(lines: &mut Vec<String>, label: &str, result: Option<&Value>) {
    let Some(result) = result else {
        lines.push(format!("  {label}: (none)"));
        return;
    };
    lines.push(format!("  {label}:"));
    lines.push(format!(
        "    session:  {}",
        text_at(result, "session_id").unwrap_or("")
    ));
    lines.push(format!(
        "    harness:  {}",
        text_at(result, "harness").unwrap_or("(unknown)")
    ));
    match text_at(result, "transcript_path") {
        Some(path) => {
            let ambig = if result["ambiguous"].as_bool() == Some(true) {
                " [ambiguous match]"
            } else {
                ""
            };
            lines.push(format!("    transcript: {path}{ambig}"));
        }
        None => lines.push(format!(
            "    transcript: (unresolved - {})",
            text_at(result, "reason").unwrap_or("not-found")
        )),
    }
}

fn run_external(id: &str, spawned: bool, json_out: bool) -> i32 {
    let (node, sc) = match read_external(id) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            eprintln!("No node matching '{id}' (external backend; exact ids)");
            return 1;
        }
        Err(e) => {
            eprintln!("Error: the tracker read for {id} failed: {e}");
            return 1;
        }
    };
    let title_of = |nid: &str| -> Option<String> {
        let read = crate::tracker::run_door(&json!({"tracker": "read", "id": nid}));
        text_at(read.get("node")?, "title").map(str::to_string)
    };
    let node_id = text_at(&node, "id").unwrap_or(id).to_string();
    let birth = birth_edge(&sc);
    let spawn = spawn_edge(&sc);
    let mut walk: Vec<(usize, String)> = Vec::new();
    if spawned {
        let all = crate::tracker::run_door(&json!({"tracker": "sidecar-all"}));
        let mut by_source: HashMap<String, Vec<String>> = HashMap::new();
        if let Some(sidecars) = all.get("sidecars").and_then(Value::as_object) {
            for (nid, other) in sidecars {
                if let Some(src) = text_at(other, "source_node_id").filter(|s| !s.is_empty()) {
                    by_source
                        .entry(src.to_string())
                        .or_default()
                        .push(nid.clone());
                }
            }
        }
        let mut seen = HashSet::from([node_id.clone()]);
        let mut frontier = vec![node_id.clone()];
        let mut depth = 0;
        while !frontier.is_empty() && depth < SPAWNED_MAX_DEPTH {
            depth += 1;
            let mut next = Vec::new();
            for parent in &frontier {
                let mut children = by_source.get(parent).cloned().unwrap_or_default();
                children.sort();
                for child in children {
                    if seen.insert(child.clone()) {
                        walk.push((depth, child.clone()));
                        next.push(child);
                    }
                }
            }
            frontier = next;
        }
    }
    let sessions = sc
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if json_out {
        let mut output = json!({
            "node_id": node_id,
            "title": node.get("title"),
            "edges": [edge_json("node_birth", birth.as_ref()), edge_json("spawn", spawn.as_ref())],
            "pr": pr_block(&sc),
            "sessions": sessions,
            "lifecycle": null,
            "source_node_id": sc.get("source_node_id"),
            "source_node_title": text_at(&sc, "source_node_id").and_then(title_of),
            "source_plan_path": sc.get("source_plan_path"),
            "related": [],
        });
        if spawned {
            let nodes: Vec<Value> = walk
                .iter()
                .map(|(d, nid)| json!({"depth": d, "id": nid, "title": title_of(nid)}))
                .collect();
            output["spawned"] =
                json!({"nodes": nodes, "cycle_detected": false, "truncated_at_depth": null});
        }
        println!("{}", py_json_pretty(&output));
        return 0;
    }
    println!(
        "provenance for {node_id}: {}",
        text_at(&node, "title").unwrap_or("")
    );
    println!(
        "  node_birth: {}",
        text_at(&sc, "source_session_id").unwrap_or("(none)")
    );
    println!(
        "  spawn: {}",
        text_at(&sc, "spawned_by_session").unwrap_or("(none)")
    );
    println!("{}", render_pr_line(&pr_block(&sc)));
    println!("  sessions: {} row(s)", sessions.len());
    for (d, nid) in &walk {
        println!(
            "  spawned d{d}: {nid} ({})",
            title_of(nid).unwrap_or_default()
        );
    }
    0
}

pub fn run(tail: &[String]) -> i32 {
    let mut id: Option<&str> = None;
    let (mut spawned, mut json_out) = (false, false);
    for arg in tail {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{PROVENANCE_HELP}");
                return 0;
            }
            "--spawned" => spawned = true,
            "--json" | "-J" => json_out = true,
            other if other.starts_with('-') || id.is_some() => {
                eprint!("{PROVENANCE_HELP}");
                return 2;
            }
            other => id = Some(other),
        }
    }
    let Some(id) = id else {
        eprint!("{PROVENANCE_HELP}");
        return 2;
    };
    if active_backend_name() != "graph" {
        return run_external(id, spawned, json_out);
    }
    let graph = settings::graph_path();
    // A wedged graph must not read as "no node matching", which asserts the
    // node is absent.
    let entries: Vec<Value> = match graph_store::read_rows(&graph) {
        Ok(rows) => rows
            .into_iter()
            .filter(|r| r.get("archived_at").is_none_or(Value::is_null))
            .collect(),
        Err(e) => {
            eprintln!("fno backlog provenance: the graph could not be read: {e}");
            return super::get_cli::GRAPH_UNREADABLE_EXIT;
        }
    };
    let Some(node) = resolve_tiers(&entries, id) else {
        let message = format!(
            "No node matching '{id}' in {}",
            crate::backlog::database_path(&graph).display()
        );
        if json_out {
            println!("{}", json!({"ok": false, "error": message}));
        } else {
            eprintln!("{message}");
        }
        return 1;
    };
    let node_id = text_at(node, "id").unwrap_or(id);
    let birth = birth_edge(node);
    let spawn = spawn_edge(node);
    let by_id = |other: &str| entries.iter().find(|e| text_at(e, "id") == Some(other));
    let titled = |other: &str| match by_id(other) {
        Some(row) => format!("{other} ({})", text_at(row, "title").unwrap_or("")),
        None => format!("{other} (not in graph)"),
    };
    let origin = text_at(node, "source_node_id").filter(|s| !s.is_empty());
    let related: Vec<&str> = node
        .get("related")
        .and_then(Value::as_array)
        .map(|r| r.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let walk = if spawned {
        spawned_walk(&entries, node_id)
    } else {
        SpawnedWalk {
            rows: Vec::new(),
            cycle: false,
            truncated: false,
        }
    };
    let runtime = runtime_attempts(node_id, node);
    let sessions = node
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let index = registry_status_index();
    let (roster_lines, roster) = lifecycle_roster(&sessions, index.as_ref());
    if json_out {
        let mut output = json!({
            "node_id": node_id,
            "title": node.get("title"),
            "edges": [edge_json("node_birth", birth.as_ref()), edge_json("spawn", spawn.as_ref())],
            "pr": pr_block(node),
            "sessions": sessions,
            "lifecycle": roster,
            "source_node_id": origin,
            "source_node_title": origin.and_then(by_id).and_then(|o| o.get("title")),
            "source_plan_path": node.get("source_plan_path"),
            "related": related,
            "runtime_attempts": runtime,
        });
        if spawned {
            let nodes: Vec<Value> = walk
                .rows
                .iter()
                .map(|(d, n)| json!({"depth": d, "id": n.get("id"), "title": n.get("title")}))
                .collect();
            output["spawned"] = json!({
                "nodes": nodes,
                "cycle_detected": walk.cycle,
                "truncated_at_depth": walk.truncated.then_some(SPAWNED_MAX_DEPTH),
            });
        }
        println!("{}", py_json_pretty(&output));
        return 0;
    }
    let mut lines = vec![format!(
        "provenance for {node_id}: {}",
        text_at(node, "title").unwrap_or("")
    )];
    fmt_edge(&mut lines, "node-birth", birth.as_ref());
    // Rendered even when null: an omitted line reads as "this command does not
    // report origins".
    lines.push(format!(
        "  origin: {}",
        origin.map_or("(none)".to_string(), &titled)
    ));
    if let Some(plan) = text_at(node, "source_plan_path").filter(|p| !p.is_empty()) {
        lines.push(format!("    plan: {plan}"));
    }
    fmt_edge(&mut lines, "spawn", spawn.as_ref());
    lines.push(render_pr_line(&pr_block(node)));
    lines.push(if related.is_empty() {
        "  related: (none)".to_string()
    } else {
        "  related:".to_string()
    });
    for rid in &related {
        lines.push(format!("    {}", titled(rid)));
    }
    if spawned {
        lines.push(if walk.rows.is_empty() {
            "  spawned: (none)".to_string()
        } else {
            "  spawned:".to_string()
        });
        for (depth, row) in &walk.rows {
            lines.push(format!(
                "    {}d{depth} {}",
                "  ".repeat(depth - 1),
                titled(text_at(row, "id").unwrap_or(""))
            ));
        }
        if walk.cycle {
            lines.push("    note: cycle detected; walk truncated at the repeat".to_string());
        }
        if walk.truncated {
            lines.push(format!(
                "    note: depth cap {SPAWNED_MAX_DEPTH} reached; walk truncated"
            ));
        }
    }
    render_runtime(&mut lines, &runtime);
    lines.push("  lifecycle:".to_string());
    lines.extend(roster_lines);
    println!("{}", lines.join("\n"));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_roster_sums_only_honest_windows_and_names_every_phase() {
        let sessions = vec![
            json!({"phase": "execute", "harness": "claude", "session_id": "s1",
                   "started_at": "2026-01-01T00:00:00+00:00", "ended_at": "2026-01-01T00:02:00+00:00"}),
            json!({"phase": "review", "harness": "codex", "session_id": "s2", "at": "2026-01-01T00:03:00+00:00"}),
            json!({"phase": "ship", "harness": "claude", "session_id": "s3",
                   "started_at": "2026-01-01T00:09:00+00:00", "ended_at": "2026-01-01T00:04:00+00:00"}),
        ];
        let (lines, summary) = lifecycle_roster(&sessions, None);
        assert!(lines.iter().any(|l| l.contains("think     not recorded")));
        assert!(lines.iter().any(|l| l.contains("end only @")));
        assert_eq!(summary["total_duration_seconds"], 120.0);
        assert_eq!(summary["phases_recorded"], 1);
    }

    #[test]
    fn a_registry_miss_reads_reaped_only_when_the_registry_answered() {
        let index = HashMap::from([("s1".to_string(), "running".to_string())]);
        assert_eq!(
            registry_status_of(Some("s2"), Some(&index)).as_deref(),
            Some("reaped")
        );
        assert_eq!(registry_status_of(Some("s2"), None), None);
    }

    #[test]
    fn the_walk_keeps_descendants_through_a_cycle() {
        let rows = vec![
            json!({"id": "a", "source_node_id": "c"}),
            json!({"id": "b", "source_node_id": "a"}),
            json!({"id": "c", "source_node_id": "b"}),
        ];
        let walk = spawned_walk(&rows, "a");
        assert!(walk.cycle);
        let ids: Vec<&str> = walk
            .rows
            .iter()
            .map(|(_, r)| text_at(r, "id").unwrap())
            .collect();
        assert_eq!(ids, ["b", "c"]);
    }

    #[test]
    fn work_with_a_dead_owner_is_interrupted_until_the_node_is_terminal() {
        assert_eq!(classify(None, true, false), "interrupted");
        assert_eq!(classify(None, true, true), "stale");
        assert_eq!(classify(Some("suspect"), false, false), "suspect");
        assert_eq!(classify(Some("live"), false, true), "live");
    }
}
