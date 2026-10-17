//! `fno backlog freshness`: is a node's premise, or its plan, still true
//! against current main before a worker spends on it?
//!
//! Read-only and advisory: every verdict exits 0, nothing blocks, and the
//! one write is the best-effort `backlog_freshness_verdict` event the
//! false-positive rate is measured from (d-44800d59). v1 warns; turning a
//! verdict into a refusal is a measured follow-up, never this verb.
use crate::events::EventEmitter;
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

pub(crate) const EVENT_KIND: &str = "backlog_freshness_verdict";

fn parse_ts(raw: &str) -> Option<String> {
    let t = raw.trim();
    if chrono::DateTime::parse_from_rfc3339(t).is_ok() {
        return Some(t.to_string());
    }
    chrono::NaiveDateTime::parse_from_str(t, "%Y-%m-%dT%H:%M:%S%.f")
        .ok()
        .map(|n| n.and_utc().to_rfc3339())
}

fn is_path_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-')
}

/// Path-shaped tokens: `[\w.\-]+` segments joined by `/`, at least one
/// slash - maintain.py's `_PATH_TOKEN_RE` shape, read without a regex.
fn path_tokens(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !is_path_char(b[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < b.len() && (is_path_char(b[i]) || b[i] == b'/') {
            i += 1;
        }
        let mut end = i;
        while end > start && (b[end - 1] == b'/' || b[end - 1] == b'.') {
            end -= 1;
        }
        let tok = std::str::from_utf8(&b[start..end]).unwrap_or("");
        let slashes = tok.bytes().filter(|c| *c == b'/').count();
        if slashes >= 1 && tok.len() > slashes + 1 {
            out.push(tok.to_string());
        }
    }
    out.sort();
    out.dedup();
    out
}

/// The plan leg cites backticked tokens only: a plan is a long document and
/// its prose must not manufacture cites.
fn backticked_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for span in text.split('`').skip(1).step_by(2) {
        out.extend(path_tokens(span));
    }
    out.sort();
    out.dedup();
    out
}

fn git_out(repo: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn prs_in_subjects(subjects: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in subjects.split_whitespace() {
        if let Some(rest) = s.strip_prefix('#') {
            if rest.bytes().all(|c| c.is_ascii_digit()) && !out.contains(&s.to_string()) {
                out.push(s.to_string());
            }
        }
    }
    out
}

/// Split a `--ids` value on commas, trimming empties, so a trailing comma or
/// stray space costs nothing. Node ids are `<prefix>-<hex>`, so a comma
/// never appears inside an id.
fn split_ids(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn tracked_at(repo: &Path, rev: &str) -> BTreeSet<String> {
    git_out(repo, &["ls-tree", "-r", "--name-only", rev])
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// One changed path between the plan sha and main: the status letter, the
/// path as the diff named it, and the PRs whose first-parent merges on main
/// touched it since the sha.
struct PathChange {
    status: char,
    path: String,
    prs: Vec<String>,
}

fn diff_against_main(repo: &Path, sha: &str, paths: &[String]) -> Vec<PathChange> {
    let mut args: Vec<&str> = vec!["diff", "--name-status", "-M", sha, "origin/main", "--"];
    args.extend(paths.iter().map(String::as_str));
    let Some(raw) = git_out(repo, &args) else {
        return Vec::new();
    };
    raw.lines()
        .filter_map(|line| {
            let mut parts = line.split('\t');
            let status = parts.next()?.chars().next()?;
            let path = match status {
                'D' | 'R' | 'M' | 'A' | 'C' => parts.next()?.to_string(),
                _ => return None,
            };
            Some(PathChange {
                status,
                path,
                prs: Vec::new(),
            })
        })
        .collect()
}

fn prs_touching(repo: &Path, sha: &str, path: &str) -> Vec<String> {
    let subjects = git_out(
        repo,
        &[
            "log",
            "--first-parent",
            "--format=%s",
            &format!("{sha}..origin/main"),
            "--",
            path,
        ],
    )
    .unwrap_or_default();
    prs_in_subjects(&subjects)
}

fn cited_paths(row: &Value) -> String {
    ["title", "details", "dispatch_brief"]
        .iter()
        .filter_map(|k| row.get(*k).and_then(Value::as_str))
        .collect::<Vec<&str>>()
        .join(" ")
}

/// One first-parent commit of main, newest first in the stream: the date, the
/// subject, and the paths its diff against the first parent touched.
struct CommitFact {
    /// Commit date as unix seconds; 0 when unparsable (never matches since).
    ts: i64,
    subject: String,
    paths: Vec<String>,
}

/// The shared git facts every premise verdict reads. Built once per process
/// so a batch of rows pays three git reads total instead of three per row.
pub(crate) struct PremiseCtx {
    main: Option<String>,
    tracked: BTreeSet<String>,
    stream: Vec<CommitFact>,
}

/// Build the shared facts: resolve main, one `ls-tree -r`, and one
/// name-only log. `--no-renames` keeps the old side of a rename in the path
/// list, matching the pathspec semantics the per-node read used to answer
/// with, so ambient `diff.renames` config cannot drift the verdict.
pub(crate) fn premise_ctx(repo: &Path) -> PremiseCtx {
    let main = resolve_main(repo);
    let mut tracked = BTreeSet::new();
    let mut stream: Vec<CommitFact> = Vec::new();
    if let Some(m) = &main {
        tracked = tracked_at(repo, m);
        if let Some(raw) = git_out(
            repo,
            &[
                "log",
                "--first-parent",
                "--no-renames",
                "--name-only",
                "--format=@%cI%x00%s",
                m,
            ],
        ) {
            let mut current: Option<CommitFact> = None;
            for line in raw.lines() {
                // A header carries the NUL separator the format wrote; a
                // tracked path that merely starts with `@` does not, so it
                // still reads as a path.
                match line.strip_prefix('@').filter(|h| h.contains('\u{0}')) {
                    Some(head) => {
                        if let Some(c) = current.take() {
                            stream.push(c);
                        }
                        let mut parts = head.splitn(2, '\u{0}');
                        let date = parts.next().unwrap_or("");
                        let subject = parts.next().unwrap_or("").to_string();
                        let ts = chrono::DateTime::parse_from_rfc3339(date)
                            .map(|d| d.timestamp())
                            .unwrap_or(0);
                        current = Some(CommitFact {
                            ts,
                            subject,
                            paths: Vec::new(),
                        });
                    }
                    None => {
                        if let Some(c) = current.as_mut() {
                            if !line.is_empty() {
                                c.paths.push(line.to_string());
                            }
                        }
                    }
                }
            }
            if let Some(c) = current.take() {
                stream.push(c);
            }
        }
    }
    PremiseCtx {
        main,
        tracked,
        stream,
    }
}

/// The status legs that need no git at all, read first by every caller.
fn status_shortcut(row: &Value) -> Option<Value> {
    let status = row.get("status").and_then(Value::as_str).unwrap_or("");
    if matches!(status, "done" | "superseded") {
        return Some(json!({"verdict": "shipped", "evidence": format!("node status {status}")}));
    }
    if status == "in_review" {
        return Some(json!({"verdict": "partly_shipped", "evidence": "node status in_review"}));
    }
    None
}

/// The premise verdict for one node off a shared context, first match wins.
pub(crate) fn premise_verdict_in(ctx: &PremiseCtx, node_id: &str, row: &Value) -> Value {
    if let Some(v) = status_shortcut(row) {
        return v;
    }
    let Some(_main) = &ctx.main else {
        return json!({"verdict": "unmeasured:no origin/main", "evidence": "git could not resolve origin/main"});
    };
    // git's --grep matched `/{node_id}-` against first-parent subjects; the
    // ids are hex, so a plain substring read answers the same set.
    let needle = format!("/{node_id}-");
    let mut named: Vec<&str> = Vec::new();
    for fact in &ctx.stream {
        if fact.subject.contains(&needle) {
            named.push(&fact.subject);
        }
    }
    if !named.is_empty() {
        let joined = named.join(" ");
        let prs = prs_in_subjects(&joined).join(",");
        return json!({
            "verdict": "partly_shipped",
            "evidence": format!("a merge on main names {node_id}- ({prs})"),
        });
    }
    let mut cites: Vec<String> = path_tokens(&cited_paths(row))
        .into_iter()
        .filter(|t| ctx.tracked.contains(t))
        .collect();
    if cites.is_empty() {
        return json!({"verdict": "unmeasured:no cited repo path", "evidence": "no cited path exists on main"});
    }
    // git --since traversal stops at the first commit older than the cutoff
    // (revision.c: `commitdate < since`), so the boundary instant counts as
    // changed; the stream comparison keeps `>=`. The evidence names the
    // normalized created_at the per-node read used to name.
    let since = row
        .get("created_at")
        .and_then(Value::as_str)
        .and_then(parse_ts);
    let Some(since) = since else {
        return json!({"verdict": "unmeasured:created_at unreadable", "evidence": "no comparable created_at"});
    };
    let since_ts = chrono::DateTime::parse_from_rfc3339(&since)
        .map(|d| d.timestamp())
        .unwrap_or(i64::MAX);
    cites.sort();
    let mut changed: Vec<String> = Vec::new();
    for path in &cites {
        let mut subjects: Vec<&str> = Vec::new();
        for fact in &ctx.stream {
            // The stream is newest first; once a commit predates the cutoff
            // every later one does too.
            if fact.ts < since_ts {
                break;
            }
            if fact.paths.iter().any(|p| p == path) {
                subjects.push(&fact.subject);
            }
        }
        if !subjects.is_empty() {
            let joined = subjects.join(" ");
            let prs = prs_in_subjects(&joined).join(",");
            changed.push(if prs.is_empty() {
                path.clone()
            } else {
                format!("{path} ({prs})")
            });
        }
    }
    if changed.is_empty() {
        return json!({
            "verdict": "holds",
            "evidence": format!("{} cited path(s) exist, none changed since {since}", cites.len()),
        });
    }
    json!({
        "verdict": "changed",
        "evidence": format!("changed since {since}: {}", changed.join(", ")),
    })
}

/// The premise verdict for one node, first match wins.
pub fn premise_verdict(repo: &Path, node_id: &str, row: &Value) -> Value {
    if let Some(v) = status_shortcut(row) {
        return v;
    }
    premise_verdict_in(&premise_ctx(repo), node_id, row)
}

fn resolve_main(repo: &Path) -> Option<String> {
    git_out(repo, &["rev-parse", "--verify", "origin/main^{commit}"])
}

fn frontmatter_main_sha(text: &str) -> Option<String> {
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        if line == "---" {
            break;
        }
        // The key nests under `code_index:` in every real plan, so match the
        // trimmed line, not the raw indentation.
        if let Some(v) = line.trim().strip_prefix("main_sha:") {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// The plan verdict: did main move under the plan's cited paths since the
/// sha the blueprint recorded?
fn plan_verdict(repo: &Path, text: &str) -> Value {
    let Some(sha) = frontmatter_main_sha(text) else {
        return json!({"verdict": "unmeasured:no code_index.main_sha", "since": Value::Null, "evidence": "the plan records no main sha"});
    };
    let short: String = sha.chars().take(9).collect();
    if resolve_commit(repo, &sha).is_none() {
        return json!({"verdict": "unmeasured:sha not a local commit", "since": short, "evidence": format!("{sha} is not resolvable locally")});
    }
    let at_sha = tracked_at(repo, &sha);
    let cites: Vec<String> = backticked_tokens(text)
        .into_iter()
        .filter(|t| at_sha.contains(t))
        .collect();
    if cites.is_empty() {
        return json!({"verdict": "unmeasured:no cited path exists at the sha", "since": short, "evidence": "no backticked path matches the recorded tree"});
    }
    let mut changes = diff_against_main(repo, &sha, &cites);
    let void: Vec<String> = changes
        .iter()
        .filter(|c| c.status == 'D' || c.status == 'R')
        .map(|c| c.path.clone())
        .collect();
    if !void.is_empty() {
        return json!({
            "verdict": "void",
            "since": short,
            "evidence": format!("deleted or renamed on main: {}", void.join(", ")),
        });
    }
    let mut changed: Vec<String> = Vec::new();
    for c in changes.iter_mut() {
        c.prs = prs_touching(repo, &sha, &c.path);
        let prs = c.prs.join(",");
        changed.push(if prs.is_empty() {
            c.path.clone()
        } else {
            format!("{} ({})", c.path, prs)
        });
    }
    if changed.is_empty() {
        return json!({"verdict": "fresh", "since": short, "evidence": format!("no cited path changed since {short}")});
    }
    json!({
        "verdict": "drifted",
        "since": short,
        "evidence": format!("changed since {short}: {}", changed.join(", ")),
    })
}

fn resolve_commit(repo: &Path, sha: &str) -> Option<String> {
    git_out(
        repo,
        &["rev-parse", "--verify", &format!("{sha}^{{commit}}")],
    )
}

/// The full report for one node and optional plan. `Err` is a usage failure.
pub fn freshness_report(
    graph: &Path,
    repo: &Path,
    token: &str,
    plan: Option<&Path>,
) -> Result<Value, String> {
    if token.is_empty() {
        return Err("needs <node>".to_string());
    }
    let entries = crate::graph_store::read_rows_where(
        graph,
        &crate::backlog::RowQuery {
            filter: crate::backlog::api::NodeFilter {
                id_in: Some(vec![token.to_string()]),
                ..Default::default()
            },
            with_blockers: true,
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let row = crate::graph_get::find_entry(&entries, token);
    let node_id = row
        .and_then(|r| crate::graph_store::entry_id(r))
        .unwrap_or(token);
    let main = resolve_main(repo).unwrap_or_default();
    let premise = match row {
        Some(r) => premise_verdict(repo, &node_id, r),
        None => json!({"verdict": "unmeasured:node not found", "evidence": "no row in the graph"}),
    };
    let plan_v = match plan {
        Some(p) => match std::fs::read_to_string(p) {
            Ok(text) => {
                let mut v = plan_verdict(repo, &text);
                if let Ok(notes) = super::note_stale::stale_report(graph, token, p) {
                    v["notes"] = notes;
                }
                v
            }
            Err(e) => {
                json!({"verdict": format!("unmeasured:plan unreadable ({e})"), "since": Value::Null, "evidence": "the plan file could not be read"})
            }
        },
        None => Value::Null,
    };
    Ok(json!({
        "node": node_id,
        "main_sha": main,
        "premise": premise,
        "plan": plan_v,
    }))
}

/// The premise marks for rows already in hand: the board's displayed rows.
/// One shared git read for the whole slice, no events, no graph re-read.
pub(crate) fn premise_marks_rows(repo: &Path, rows: &[&Value]) -> BTreeMap<String, Value> {
    let ctx = premise_ctx(repo);
    let mut marks = BTreeMap::new();
    for row in rows {
        if let Some(id) = crate::graph_store::entry_id(row) {
            marks.insert(id.to_string(), premise_verdict_in(&ctx, id, row));
        }
    }
    marks
}

/// The premise marks for many ids off one shared git read. The batch is a
/// display read (the board and the list): it emits no verdict events, so a
/// glance never floods the measurement stream the explicit verb feeds.
pub fn premise_marks(graph: &Path, repo: &Path, ids: &[String]) -> Value {
    let entries = crate::graph_store::read_rows_where(
        graph,
        &crate::backlog::RowQuery {
            filter: crate::backlog::api::NodeFilter {
                id_in: Some(ids.to_vec()),
                ..Default::default()
            },
            with_blockers: true,
            ..Default::default()
        },
    )
    .unwrap_or_default();
    let ctx = premise_ctx(repo);
    let mut marks = BTreeMap::new();
    for token in ids {
        let row = crate::graph_get::find_entry(&entries, token);
        let node_id = row
            .and_then(|r| crate::graph_store::entry_id(r))
            .unwrap_or(token);
        let verdict = match row {
            Some(r) => premise_verdict_in(&ctx, node_id, r),
            None => {
                json!({"verdict": "unmeasured:node not found", "evidence": "no row in the graph"})
            }
        };
        marks.insert(node_id, verdict);
    }
    json!({ "marks": marks })
}

/// Best-effort verdict event: the false-positive rate is measured from
/// these rows, so every reading lands one (d-44800d59). Never raises.
fn emit_verdict(events: Option<&Path>, report: &Value) {
    let Some(path) = events else {
        return;
    };
    let mut data = Map::new();
    data.insert(
        "node".to_string(),
        report["node"].as_str().unwrap_or_default().into(),
    );
    data.insert(
        "main_sha".to_string(),
        report["main_sha"].as_str().unwrap_or_default().into(),
    );
    data.insert("premise".to_string(), report["premise"]["verdict"].clone());
    data.insert("plan".to_string(), report["plan"]["verdict"].clone());
    if let Err(e) = EventEmitter::new(path, "backlog").emit_fields(EVENT_KIND, data) {
        eprintln!("fno-agents backlog freshness: WARNING: event emit failed: {e}");
    }
}

fn line_of(v: &Value, head: &str) -> String {
    let verdict = v["verdict"].as_str().unwrap_or("unmeasured:no verdict");
    let evidence = v["evidence"].as_str().unwrap_or("");
    if evidence.is_empty() {
        format!("{head}: {verdict}")
    } else {
        format!("{head}: {verdict} - {evidence}")
    }
}

/// Print, emit, exit. Advisory: every reading exits 0; 2 is usage only.
pub fn run_report(
    graph: &Path,
    repo: &Path,
    token: &str,
    plan: Option<&Path>,
    json_out: bool,
    events: Option<&Path>,
) -> i32 {
    if crate::graph_get::external_backend_selected() {
        eprintln!(
            "fno-agents backlog freshness: the graph store is not the authoritative \
             backend under the selected external tracker"
        );
        return 2;
    }
    match freshness_report(graph, repo, token, plan) {
        Ok(report) => {
            emit_verdict(events, &report);
            if json_out {
                println!("{report}");
            } else {
                println!("{}", line_of(&report["premise"], "premise"));
                if !report["plan"].is_null() {
                    println!("{}", line_of(&report["plan"], "plan"));
                }
            }
            0
        }
        Err(e) => {
            eprintln!("fno-agents backlog freshness: {e}");
            2
        }
    }
}

const USAGE: &str = "usage: fno backlog freshness --ids <comma-separated-node-ids> [--graph <path>]\n       fno backlog freshness <node> [--plan <path>] [--json] [--graph <path>]";

/// Dispatch body for the `fno backlog freshness` native aux verb.
pub fn run(tail: &[String]) -> i32 {
    let mut node: Option<&String> = None;
    let mut ids: Option<Vec<String>> = None;
    let mut plan: Option<String> = None;
    let mut graph: Option<String> = None;
    let mut json_out = false;
    let mut it = tail.iter();
    while let Some(arg) = it.next() {
        let mut value = || it.next().cloned();
        match arg.as_str() {
            "--json" | "-J" => json_out = true,
            "--ids" => match value() {
                Some(v) => ids = Some(split_ids(&v)),
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            "--plan" => match value() {
                Some(v) => plan = Some(v),
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            "--graph" => match value() {
                Some(v) => graph = Some(v),
                None => {
                    eprintln!("{USAGE}");
                    return 2;
                }
            },
            other if !other.starts_with("--") && node.is_none() => node = Some(arg),
            _ => {
                eprintln!("{USAGE}");
                return 2;
            }
        }
    }
    if let Some(ids) = ids {
        if plan.is_some() {
            eprintln!("{USAGE}");
            return 2;
        }
        // The batch arm is a display read: no verdict event is emitted.
        if crate::graph_get::external_backend_selected() {
            eprintln!(
                "fno-agents backlog freshness: the graph store is not the authoritative \
                 backend under the selected external tracker"
            );
            return 2;
        }
        let graph_path = match graph {
            Some(g) => std::path::PathBuf::from(g),
            None => crate::graph_get::default_graph_path(),
        };
        let repo = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let marks = premise_marks(&graph_path, &repo, &ids);
        println!("{marks}");
        return 0;
    }
    let Some(node) = node else {
        eprintln!("{USAGE}");
        return 2;
    };
    let graph_path = match graph {
        Some(g) => std::path::PathBuf::from(g),
        None => crate::graph_get::default_graph_path(),
    };
    let repo = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let events: std::path::PathBuf = match std::env::var_os("FNO_EVENTS_PATH") {
        Some(p) => std::path::PathBuf::from(p),
        None => crate::paths::events_path(&repo),
    };
    run_report(
        &graph_path,
        &repo,
        node,
        plan.as_deref().map(Path::new),
        json_out,
        Some(events.as_path()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn git(dir: &Path, args: &[&str], env_date: Option<&str>) {
        let mut cmd = Command::new("git");
        cmd.arg("-C").arg(dir).args(args);
        if let Some(d) = env_date {
            cmd.env("GIT_COMMITTER_DATE", d).env("GIT_AUTHOR_DATE", d);
        }
        let out = cmd.output().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    fn init_repo(dir: &Path) {
        git(dir, &["init", "-q"], None);
        git(dir, &["config", "user.email", "t@example.com"], None);
        git(dir, &["config", "user.name", "t"], None);
    }

    fn commit(dir: &Path, path: &str, body: &str, msg: &str, date: &str) {
        let p = dir.join(path);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, body).unwrap();
        git(dir, &["add", path], None);
        git(dir, &["commit", "-q", "-m", msg], Some(date));
    }

    fn advance_main(dir: &Path) {
        git(dir, &["branch", "-f", "origin/main", "HEAD"], None);
    }

    fn head_sha(dir: &Path) -> String {
        git_out(dir, &["rev-parse", "HEAD"]).unwrap()
    }

    fn graph_with(dir: &Path, row: Value) -> PathBuf {
        let graph = dir.join("graph.json");
        crate::graph_store::seed_rows(&graph, &[row]).unwrap();
        graph
    }

    #[test]
    fn a_changed_cited_path_reads_drifted_and_records_the_event() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/a.rs",
            "one\n",
            "base",
            "2026-09-01T00:00:00Z",
        );
        git(repo_dir.path(), &["branch", "origin/main"], None);
        let plan_sha = head_sha(repo_dir.path());
        let plan_path = repo_dir.path().join("plan.md");
        std::fs::write(
            &plan_path,
            format!("---\ncode_index:\n  main_sha: {plan_sha}\n---\n\nedit `src/a.rs` now\n"),
        )
        .unwrap();
        commit(
            repo_dir.path(),
            "src/a.rs",
            "two\n",
            "touch",
            "2026-09-02T00:00:00Z",
        );
        advance_main(repo_dir.path());

        let store = tempfile::tempdir().unwrap();
        let graph = graph_with(
            store.path(),
            json!({"id": "x-1", "status": "ready", "created_at": "2026-09-01T00:00:00Z"}),
        );
        crate::paths::pin_test_claims_root(store.path());
        let events = store.path().join("events.jsonl");
        let code = run_report(
            &graph,
            repo_dir.path(),
            "x-1",
            Some(&plan_path),
            true,
            Some(&events),
        );
        assert_eq!(code, 0);
        let report = freshness_report(&graph, repo_dir.path(), "x-1", Some(&plan_path)).unwrap();
        assert_eq!(report["plan"]["verdict"], "drifted");
        assert!(report["plan"]["evidence"]
            .as_str()
            .unwrap()
            .contains("src/a.rs"));
        let _ = crate::event_store::import_all(&events);
        let rows =
            crate::event_store::query_events(&events, &crate::event_store::EventQuery::default())
                .unwrap_or_default();
        let verdict_row = rows
            .iter()
            .find(|r| r.line.contains(EVENT_KIND))
            .unwrap_or_else(|| panic!("no verdict event in {} rows", rows.len()));
        assert!(
            verdict_row.line.contains("\"node\":\"x-1\""),
            "row: {}",
            verdict_row.line
        );
        assert!(
            verdict_row.line.contains("\"premise\":\""),
            "row: {}",
            verdict_row.line
        );
    }

    #[test]
    fn a_deleted_cited_path_reads_void() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/gone.rs",
            "one\n",
            "base",
            "2026-09-01T00:00:00Z",
        );
        git(repo_dir.path(), &["branch", "origin/main"], None);
        let plan_sha = head_sha(repo_dir.path());
        let plan_path = repo_dir.path().join("plan.md");
        std::fs::write(
            &plan_path,
            format!("---\ncode_index:\n  main_sha: {plan_sha}\n---\n\nedit `src/gone.rs`\n"),
        )
        .unwrap();
        git(repo_dir.path(), &["rm", "-q", "src/gone.rs"], None);
        git(
            repo_dir.path(),
            &["commit", "-q", "-m", "drop"],
            Some("2026-09-02T00:00:00Z"),
        );
        advance_main(repo_dir.path());

        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(store.path(), json!({"id": "x-1", "status": "ready"}));
        let report = freshness_report(&graph, repo_dir.path(), "x-1", Some(&plan_path)).unwrap();
        assert_eq!(report["plan"]["verdict"], "void");
        assert!(report["plan"]["evidence"]
            .as_str()
            .unwrap()
            .contains("src/gone.rs"));
    }

    #[test]
    fn a_plan_without_a_main_sha_reads_unmeasured_and_exits_zero() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/a.rs",
            "one\n",
            "base",
            "2026-09-01T00:00:00Z",
        );
        advance_main(repo_dir.path());
        let plan_path = repo_dir.path().join("plan.md");
        std::fs::write(&plan_path, "---\nstatus: ready\n---\n\nedit `src/a.rs`\n").unwrap();

        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(store.path(), json!({"id": "x-1", "status": "ready"}));
        let report = freshness_report(&graph, repo_dir.path(), "x-1", Some(&plan_path)).unwrap();
        assert!(report["plan"]["verdict"]
            .as_str()
            .unwrap()
            .starts_with("unmeasured:"));
        assert_eq!(
            run_report(
                &graph,
                repo_dir.path(),
                "x-1",
                Some(&plan_path),
                false,
                None
            ),
            0
        );
    }

    #[test]
    fn a_commit_after_created_at_touching_a_cited_path_reads_changed() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/b.rs",
            "one\n",
            "base",
            "2026-01-01T00:00:00Z",
        );
        git(repo_dir.path(), &["branch", "origin/main"], None);
        commit(
            repo_dir.path(),
            "src/b.rs",
            "two\n",
            "touch",
            "2026-02-01T00:00:00Z",
        );
        advance_main(repo_dir.path());

        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(
            store.path(),
            json!({
                "id": "x-2", "status": "triage",
                "created_at": "2026-01-15T00:00:00Z",
                "title": "fix b",
                "details": "the defect sits in src/b.rs",
            }),
        );
        let report = freshness_report(&graph, repo_dir.path(), "x-2", None).unwrap();
        assert_eq!(report["premise"]["verdict"], "changed");
        assert!(report["premise"]["evidence"]
            .as_str()
            .unwrap()
            .contains("src/b.rs"));
    }

    #[test]
    fn a_done_node_reads_shipped_even_without_git() {
        let bare = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(store.path(), json!({"id": "x-3", "status": "done"}));
        let report = freshness_report(&graph, bare.path(), "x-3", None).unwrap();
        assert_eq!(report["premise"]["verdict"], "shipped");
        assert_eq!(run_report(&graph, bare.path(), "x-3", None, false, None), 0);
    }

    #[test]
    fn a_cited_path_untouched_since_created_at_reads_holds() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/keep.rs",
            "one\n",
            "base",
            "2026-01-01T00:00:00Z",
        );
        advance_main(repo_dir.path());

        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(
            store.path(),
            json!({
                "id": "x-4", "status": "triage",
                "created_at": "2026-01-15T00:00:00Z",
                "title": "keep working",
                "details": "the defect sits in src/keep.rs",
            }),
        );
        let report = freshness_report(&graph, repo_dir.path(), "x-4", None).unwrap();
        assert_eq!(report["premise"]["verdict"], "holds");
    }

    #[test]
    fn batch_marks_match_the_single_verb_on_one_shared_read() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        commit(
            repo_dir.path(),
            "src/a.rs",
            "one\n",
            "base",
            "2026-01-01T00:00:00Z",
        );
        commit(
            repo_dir.path(),
            "src/keep.rs",
            "one\n",
            "keep",
            "2026-01-01T00:00:00Z",
        );
        git(repo_dir.path(), &["branch", "origin/main"], None);
        commit(
            repo_dir.path(),
            "src/a.rs",
            "two\n",
            "touch src/a.rs (#12)",
            "2026-02-01T00:00:00Z",
        );
        advance_main(repo_dir.path());

        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = store.path().join("graph.json");
        crate::graph_store::seed_rows(
            &graph,
            &[
                json!({"id": "x-1", "status": "ready", "created_at": "2026-01-15T00:00:00Z",
                       "title": "fix a", "details": "the defect sits in src/a.rs"}),
                json!({"id": "x-2", "status": "ready", "created_at": "2026-01-15T00:00:00Z",
                       "title": "keep", "details": "the defect sits in src/keep.rs"}),
                json!({"id": "x-3", "status": "done"}),
            ],
        );
        let marks = premise_marks(
            &graph,
            repo_dir.path(),
            &["x-1".into(), "x-2".into(), "x-3".into()],
        );
        assert_eq!(marks["marks"]["x-1"]["verdict"], "changed");
        assert!(marks["marks"]["x-1"]["evidence"]
            .as_str()
            .unwrap()
            .contains("src/a.rs"));
        assert_eq!(marks["marks"]["x-2"]["verdict"], "holds");
        assert_eq!(marks["marks"]["x-3"]["verdict"], "shipped");

        // One probe, two readers: the shared-context core answers exactly
        // what the single-node wrapper answers on the same rows.
        let ctx = premise_ctx(repo_dir.path());
        for id in ["x-1", "x-2"] {
            let row = freshness_report(&graph, repo_dir.path(), id, None).unwrap();
            assert_eq!(marks["marks"][id]["verdict"], row["premise"]["verdict"]);
            assert_eq!(marks["marks"][id]["evidence"], row["premise"]["evidence"]);
            let fetched = crate::graph_get::find_entry(
                &crate::graph_store::read_rows_where(
                    &graph,
                    &crate::backlog::RowQuery {
                        filter: crate::backlog::api::NodeFilter {
                            id_in: Some(vec![id.to_string()]),
                            ..Default::default()
                        },
                        with_blockers: true,
                        ..Default::default()
                    },
                )
                .unwrap_or_default(),
                id,
            )
            .unwrap();
            assert_eq!(
                premise_verdict_in(&ctx, id, &fetched),
                premise_verdict(repo_dir.path(), id, &fetched)
            );
        }
    }

    #[test]
    fn an_id_absent_from_the_graph_reads_unmeasured_in_marks() {
        let bare = tempfile::tempdir().unwrap();
        let store = tempfile::tempdir().unwrap();
        crate::paths::pin_test_claims_root(store.path());
        let graph = graph_with(store.path(), json!({"id": "x-9", "status": "ready"}));
        let marks = premise_marks(&graph, bare.path(), &["x-9".into(), "nope-1".into()]);
        assert_eq!(
            marks["marks"]["x-9"]["verdict"],
            "unmeasured:no origin/main"
        );
        assert_eq!(
            marks["marks"]["nope-1"]["verdict"],
            "unmeasured:node not found"
        );
    }

    #[test]
    fn split_ids_takes_commas_and_drops_empties() {
        assert_eq!(
            split_ids(" x-1 ,x-2,,x-3"),
            vec!["x-1".to_string(), "x-2".to_string(), "x-3".to_string()]
        );
        assert!(split_ids("").is_empty());
    }
}
