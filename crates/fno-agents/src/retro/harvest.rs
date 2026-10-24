//! Harvest left-out work for a merged PR from three independent sources:
//! the carve-out ledger, declined reviewer findings, and the COMPLETION.md
//! deferred findings. One failing source never sinks the others. A gh
//! failure empties the review source, names itself in a warning, and makes
//! the caller retain the trigger for retry.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;
use std::sync::OnceLock;

use chrono::{DateTime, FixedOffset, NaiveDateTime, TimeZone, Utc};
use regex::Regex;
use serde_json::{json, Value};

use super::classify::py_splitlines;
use super::{py_str, truthy, RawItem, KIND_CARVEOUT, KIND_DEFERRED, KIND_REVIEW};

pub const DEFAULT_SEVERITY: &str = "medium";

/// A gh runner takes argv without the leading `gh` and returns
/// `(rc, stdout, stderr)`.
pub type GhRunner = dyn Fn(&[String]) -> (i32, String, String);

/// The real runner: `gh <args>`, rc 127 when gh cannot start.
pub fn default_gh_runner(args: &[String]) -> (i32, String, String) {
    match std::process::Command::new("gh").args(args).output() {
        Ok(out) => (
            out.status.code().unwrap_or(1),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        ),
        Err(err) => (127, String::new(), err.to_string()),
    }
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("harvest regex"))
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// Python's `err.strip()[:n]`.
fn err_head(err: &str, n: usize) -> String {
    err.trim().chars().take(n).collect()
}

/// A recognized reviewer badge severity, or `None`.
pub fn extract_severity(body: &str) -> Option<String> {
    static GEMINI: OnceLock<Regex> = OnceLock::new();
    static CODEX: OnceLock<Regex> = OnceLock::new();
    static SIGMA: OnceLock<Regex> = OnceLock::new();
    if let Some(m) = re(&GEMINI, r"(?i)!\[(critical|high|medium|low)\]").captures(body) {
        return Some(m[1].to_lowercase());
    }
    if let Some(m) = re(&CODEX, r"(?i)!\[P([123])\b").captures(body) {
        return Some(
            match &m[1] {
                "1" => "high",
                "2" => "medium",
                _ => "low",
            }
            .to_string(),
        );
    }
    // Python's `^` without MULTILINE anchors at the start of the body only.
    if let Some(m) = re(
        &SIGMA,
        r"(?i)\A\s*-\s+\*\*(P[123]|critical|high|medium|low)\*\*\s*(?:-|:)",
    )
    .captures(body)
    {
        let raw = m[1].to_lowercase();
        return Some(match raw.as_str() {
            "p1" => "high".to_string(),
            "p2" => "medium".to_string(),
            "p3" => "low".to_string(),
            _ => raw,
        });
    }
    None
}

/// The badge severity; no badge reads as medium (node tier).
pub fn normalize_severity(body: &str) -> String {
    extract_severity(body).unwrap_or_else(|| DEFAULT_SEVERITY.to_string())
}

fn opt_str(v: Option<&Value>) -> Option<String> {
    match v {
        None | Some(Value::Null) => None,
        Some(other) => Some(py_str(Some(other))),
    }
}

/// Carve-out ledger rows as items. A malformed line warns and is skipped;
/// a `backfill` row is left for the ship ritual's backfill slot.
pub fn harvest_carveouts_text(
    text: &str,
    session_ids: Option<&[String]>,
    source_pr: Option<i64>,
    warnings: &mut Vec<String>,
) -> Vec<RawItem> {
    let want: Option<HashSet<&str>> = session_ids.map(|s| s.iter().map(String::as_str).collect());
    let mut items = Vec::new();
    for (idx, raw) in py_splitlines(text).into_iter().enumerate() {
        let lineno = idx + 1;
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let rec: Value = match serde_json::from_str(raw) {
            Ok(v) => v,
            Err(_) => {
                warnings.push(format!(
                    "carveouts.jsonl line {lineno}: malformed JSON, skipped"
                ));
                continue;
            }
        };
        if !rec.is_object() {
            warnings.push(format!(
                "carveouts.jsonl line {lineno}: not a JSON object, skipped"
            ));
            continue;
        }
        if let Some(want) = &want {
            let session = rec.get("session_id").and_then(Value::as_str);
            if !session.is_some_and(|s| want.contains(s)) {
                continue;
            }
        }
        if rec.get("kind").and_then(Value::as_str) == Some(super::ledger::BACKFILL_KIND) {
            continue;
        }
        items.push(RawItem {
            kind: KIND_CARVEOUT.to_string(),
            text: rec
                .get("description")
                .map_or_else(String::new, |v| py_str(Some(v))),
            source_pr,
            source_id: rec
                .get("id")
                .map_or_else(|| format!("carveout:{lineno}"), |v| py_str(Some(v))),
            priority: opt_str(rec.get("priority")),
            severity: opt_str(rec.get("severity")),
            title_hint: opt_str(rec.get("need")),
            subkind: opt_str(rec.get("kind")),
            ..RawItem::default()
        });
    }
    items
}

/// `harvest_carveouts_text` over the ledger under `root`; no ledger, no items.
pub fn harvest_carveouts(
    root: &Path,
    session_ids: Option<&[String]>,
    source_pr: Option<i64>,
    warnings: &mut Vec<String>,
) -> Vec<RawItem> {
    let path = super::ledger::ledger_path(root);
    match std::fs::read_to_string(&path) {
        Ok(text) => harvest_carveouts_text(&text, session_ids, source_pr, warnings),
        Err(_) => Vec::new(),
    }
}

/// Reviewer inline comments as declined-finding items. An addressed id
/// (resolved, or shown addressed by the commit signal) is dropped; when the
/// author's Skipped table also names it, fix evidence wins and a warning
/// records the discrepancy. The author's own replies and every reply
/// comment are never findings.
pub fn harvest_reviews(
    comments: &[Value],
    resolved_ids: &HashSet<String>,
    skipped_ids: &HashSet<String>,
    commit_dates: Option<&[String]>,
    author_login: Option<&str>,
    source_pr: Option<i64>,
    warnings: &mut Vec<String>,
) -> Vec<RawItem> {
    let mut resolved = resolved_ids.clone();
    if let Some(dates) = commit_dates {
        resolved.extend(addressed_ids_from_comments(comments, dates));
    }
    let author_lower = author_login
        .filter(|a| !a.is_empty())
        .map(str::to_lowercase);
    let mut items: Vec<RawItem> = Vec::new();
    for c in comments {
        let cid = c.get("id").map_or_else(String::new, |v| py_str(Some(v)));
        let reviewer = if truthy(c.get("reviewer")) {
            py_str(c.get("reviewer"))
        } else {
            String::new()
        };
        if author_lower
            .as_deref()
            .is_some_and(|a| reviewer.to_lowercase() == a)
        {
            continue;
        }
        if c.get("in_reply_to_id").is_some_and(|v| !v.is_null()) {
            continue;
        }
        if !cid.is_empty() && resolved.contains(&cid) {
            if skipped_ids.contains(&cid) {
                warnings.push(format!(
                    "review comment {cid}: author table says skipped but a fix commit addressed it - fix evidence wins (no node)"
                ));
            }
            continue;
        }
        let body = c.get("body").map_or_else(String::new, |v| py_str(Some(v)));
        items.push(RawItem {
            kind: KIND_REVIEW.to_string(),
            severity: Some(normalize_severity(&body)),
            text: body,
            source_pr,
            source_id: if cid.is_empty() {
                format!("review:{}", items.len())
            } else {
                cid
            },
            url: opt_str(c.get("url")),
            reviewer: if reviewer.is_empty() {
                None
            } else {
                Some(reviewer)
            },
            ..RawItem::default()
        });
    }
    items
}

fn pulls_path(repo: Option<&str>, pr: i64, tail: &str) -> String {
    match repo.filter(|r| !r.is_empty()) {
        Some(r) => format!("repos/{r}/{tail}").replace("{pr}", &pr.to_string()),
        None => format!("repos/:owner/:repo/{tail}").replace("{pr}", &pr.to_string()),
    }
}

/// `--paginate --slurp` output, flattened: an array of pages, each an array
/// of objects. A flat array of objects is tolerated too.
fn flatten_pages(raw: &Value) -> Vec<Value> {
    let mut flat = Vec::new();
    if let Value::Array(elems) = raw {
        for elem in elems {
            match elem {
                Value::Array(page) => flat.extend(page.iter().cloned()),
                Value::Object(_) => flat.push(elem.clone()),
                _ => {}
            }
        }
    }
    flat
}

fn parse_out(out: &str) -> Result<Value, ()> {
    if out.trim().is_empty() {
        return Ok(Value::Array(Vec::new()));
    }
    serde_json::from_str(out).map_err(|_| ())
}

/// A PR's inline review comments, normalized. Any gh failure returns empty
/// and warns, so the caller retains the trigger.
pub fn fetch_review_comments(
    pr: i64,
    repo: Option<&str>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> Vec<Value> {
    let path = pulls_path(repo, pr, "pulls/{pr}/comments");
    let (rc, out, err) = gh(&args(&["api", &path, "--paginate", "--slurp"]));
    if rc != 0 {
        warnings.push(format!(
            "review harvest: `gh api` failed (rc={rc}); skipping reviewer findings for PR #{pr} ({})",
            err_head(&err, 120)
        ));
        return Vec::new();
    }
    let Ok(raw) = parse_out(&out) else {
        warnings.push("review harvest: could not parse `gh api` output, skipping".to_string());
        return Vec::new();
    };
    flatten_pages(&raw)
        .iter()
        .map(|c| {
            let user = c.get("user").cloned().unwrap_or(Value::Null);
            let login = user
                .get("login")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            // A third-party bot can carry type "User" with a "[bot]" login.
            let is_bot =
                user.get("type").and_then(Value::as_str) == Some("Bot") || login.ends_with("[bot]");
            json!({
                "id": c.get("id").map_or_else(String::new, |v| py_str(Some(v))),
                "body": c.get("body").cloned().unwrap_or(Value::String(String::new())),
                "url": c.get("html_url").cloned().unwrap_or(Value::Null),
                "reviewer": if login.is_empty() { Value::Null } else { Value::String(login) },
                "in_reply_to_id": match c.get("in_reply_to_id") {
                    None | Some(Value::Null) => Value::Null,
                    Some(v) => Value::String(py_str(Some(v))),
                },
                "created_at": c.get("created_at").cloned().unwrap_or(Value::Null),
                "is_bot": is_bot,
            })
        })
        .collect()
}

/// Python's `datetime.fromisoformat` after mapping a trailing `Z` to UTC;
/// a naive stamp reads as UTC. `None` on any parse failure.
pub(crate) fn parse_ts(s: Option<&str>) -> Option<DateTime<FixedOffset>> {
    let s = s.filter(|s| !s.is_empty())?;
    let normalized = match s.strip_suffix('Z') {
        Some(head) => format!("{head}+00:00"),
        None => s.to_string(),
    };
    if let Ok(dt) = DateTime::parse_from_rfc3339(&normalized) {
        return Some(dt);
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%dT%H:%M%:z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(&normalized, fmt) {
            return Some(dt);
        }
    }
    for fmt in [
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(&normalized, fmt) {
            return Some(Utc.from_utc_datetime(&naive).fixed_offset());
        }
    }
    chrono::NaiveDate::parse_from_str(&normalized, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|naive| Utc.from_utc_datetime(&naive).fixed_offset())
}

/// Comment ids the PR's own history shows as addressed. A top-level
/// comment is addressed when it has work evidence (a commit strictly after
/// it, or a non-bot `wontfix:` reply) and clears the engagement gate. A
/// human finding needs a non-bot reply first, so an unrelated commit cannot
/// bury it. A bot finding skips that gate: an autonomous merge commits the
/// fix without replying or resolving.
pub fn addressed_ids_from_comments(comments: &[Value], commit_dates: &[String]) -> HashSet<String> {
    let mut replies: HashMap<String, Vec<(bool, String)>> = HashMap::new();
    for c in comments {
        if let Some(parent) = c.get("in_reply_to_id").filter(|v| !v.is_null()) {
            let is_bot = truthy(c.get("is_bot"));
            let body = if truthy(c.get("body")) {
                py_str(c.get("body"))
            } else {
                String::new()
            };
            replies
                .entry(py_str(Some(parent)))
                .or_default()
                .push((is_bot, body));
        }
    }
    let commits: Vec<DateTime<FixedOffset>> = commit_dates
        .iter()
        .filter_map(|d| parse_ts(Some(d)))
        .collect();
    let mut addressed = HashSet::new();
    for c in comments {
        if c.get("in_reply_to_id").is_some_and(|v| !v.is_null()) {
            continue;
        }
        let id = c.get("id").map_or_else(String::new, |v| py_str(Some(v)));
        if id.is_empty() {
            continue;
        }
        let non_bot: Vec<&String> = replies
            .get(&id)
            .map(|r| r.iter().filter(|(bot, _)| !bot).map(|(_, b)| b).collect())
            .unwrap_or_default();
        let finding_dt = parse_ts(c.get("created_at").and_then(Value::as_str));
        let commit_after = finding_dt.is_some_and(|f| commits.iter().any(|cd| *cd > f));
        let wontfix = non_bot
            .iter()
            .any(|b| b.to_lowercase().contains("wontfix:"));
        let engaged = truthy(c.get("is_bot")) || !non_bot.is_empty();
        if engaged && (commit_after || wontfix) {
            addressed.insert(id);
        }
    }
    addressed
}

/// The commit timestamps of a PR (`committer.date`, else `author.date`).
/// An enrichment signal only, so a failure warns and returns
/// `(empty, true)` without retaining the trigger.
pub fn fetch_pr_commit_dates(
    pr: i64,
    repo: Option<&str>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> (Vec<String>, bool) {
    let path = pulls_path(repo, pr, "pulls/{pr}/commits");
    let (rc, out, err) = gh(&args(&["api", &path, "--paginate", "--slurp"]));
    if rc != 0 {
        warnings.push(format!(
            "review harvest: `gh api` commit-dates failed (rc={rc}); skipping addressed-finding enrichment for PR #{pr} ({})",
            err_head(&err, 120)
        ));
        return (Vec::new(), true);
    }
    let Ok(raw) = parse_out(&out) else {
        warnings.push(
            "review harvest: could not parse commit-dates output, skipping enrichment".to_string(),
        );
        return (Vec::new(), true);
    };
    let dates = flatten_pages(&raw)
        .iter()
        .filter_map(|commit| {
            let c = commit.get("commit")?;
            let pick = |who: &str| {
                c.get(who)
                    .and_then(|w| w.get("date"))
                    .filter(|d| truthy(Some(d)))
                    .map(|d| py_str(Some(d)))
            };
            pick("committer").or_else(|| pick("author"))
        })
        .collect();
    (dates, false)
}

/// The PR author's login, best-effort.
pub fn fetch_pr_author(
    pr: i64,
    repo: Option<&str>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let path = pulls_path(repo, pr, "pulls/{pr}");
    let (rc, out, err) = gh(&args(&["api", &path, "--jq", ".user.login"]));
    if rc != 0 {
        warnings.push(format!(
            "review harvest: `gh api` pr-author failed (rc={rc}); skipping author-reply filter for PR #{pr} ({})",
            err_head(&err, 120)
        ));
        return None;
    }
    let login = out.trim();
    (!login.is_empty()).then(|| login.to_string())
}

const REVIEW_THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$cursor:String){repository(owner:$owner,name:$name){pullRequest(number:$number){reviewThreads(first:100,after:$cursor){pageInfo{hasNextPage endCursor}nodes{id isResolved isOutdated path comments(first:100){pageInfo{hasNextPage endCursor}nodes{databaseId}}}}}}}";

const THREAD_COMMENTS_QUERY: &str = "query($id:ID!,$cursor:String){node(id:$id){... on PullRequestReviewThread{comments(first:100,after:$cursor){pageInfo{hasNextPage endCursor}nodes{databaseId}}}}}";

fn split_repo(repo: Option<&str>) -> Option<(String, String)> {
    let r = repo?;
    let (owner, name) = r.split_once('/')?;
    (!owner.is_empty() && !name.is_empty()).then(|| (owner.to_string(), name.to_string()))
}

/// One review thread with its LATEST state.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Thread {
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub path: Option<String>,
    pub comment_ids: Vec<String>,
}

impl Thread {
    pub fn from_value(v: &Value) -> Self {
        Thread {
            is_resolved: truthy(v.get("is_resolved")),
            is_outdated: truthy(v.get("is_outdated")),
            path: v.get("path").and_then(Value::as_str).map(str::to_string),
            comment_ids: v
                .get("comment_ids")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|x| py_str(Some(x))).collect())
                .unwrap_or_default(),
        }
    }
}

fn database_ids(nodes: Option<&Value>) -> Vec<String> {
    nodes
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|c| c.get("databaseId").filter(|d| !d.is_null()))
                .map(|d| py_str(Some(d)))
                .collect()
        })
        .unwrap_or_default()
}

/// A PR's review threads with their LATEST resolved/outdated state, as
/// `(threads, unavailable)`. Any failure, a GraphQL error envelope, or an
/// unresolvable owner/repo answers unavailable: treating it as zero resolved
/// threads would re-file every implemented finding.
pub fn fetch_review_thread_state(
    pr: i64,
    repo: Option<&str>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> (Vec<Thread>, bool) {
    let Some((owner, name)) = split_repo(repo) else {
        warnings.push(format!(
            "review harvest: no owner/repo for PR #{pr}; cannot read resolved-thread state, retaining sentinel"
        ));
        return (Vec::new(), true);
    };
    let mut threads = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..50 {
        let mut argv = args(&["api", "graphql", "-f"]);
        argv.push(format!("query={REVIEW_THREADS_QUERY}"));
        argv.extend([
            "-f".to_string(),
            format!("owner={owner}"),
            "-f".to_string(),
            format!("name={name}"),
        ]);
        argv.extend(["-F".to_string(), format!("number={pr}")]);
        if let Some(c) = &cursor {
            argv.extend(["-f".to_string(), format!("cursor={c}")]);
        }
        let (rc, out, err) = gh(&argv);
        if rc != 0 {
            warnings.push(format!(
                "review harvest: `gh api graphql` reviewThreads failed (rc={rc}) for PR #{pr} ({})",
                err_head(&err, 120)
            ));
            return (Vec::new(), true);
        }
        let data: Value = if out.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&out) {
                Ok(v) => v,
                Err(_) => {
                    warnings.push(
                        "review harvest: could not parse `gh api graphql` reviewThreads output"
                            .to_string(),
                    );
                    return (Vec::new(), true);
                }
            }
        };
        let pr_node = data
            .pointer("/data/repository/pullRequest")
            .filter(|v| !v.is_null());
        if truthy(data.get("errors")) || pr_node.is_none() {
            let detail = if truthy(data.get("errors")) {
                "GraphQL errors"
            } else {
                "no pullRequest in response"
            };
            warnings.push(format!(
                "review harvest: `gh api graphql` reviewThreads returned {detail} for PR #{pr}; retaining sentinel"
            ));
            return (Vec::new(), true);
        }
        let conn = pr_node
            .and_then(|p| p.get("reviewThreads"))
            .cloned()
            .unwrap_or(json!({}));
        for node in conn
            .get("nodes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if !truthy(Some(&node)) {
                continue;
            }
            let comments = node.get("comments").cloned().unwrap_or(json!({}));
            let mut cids = database_ids(comments.get("nodes"));
            let cpage = comments.get("pageInfo").cloned().unwrap_or(json!({}));
            if truthy(cpage.get("hasNextPage")) {
                if let Some(tid) = node
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    let after = cpage
                        .get("endCursor")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let (more, unavailable) =
                        fetch_remaining_thread_comment_ids(tid, after, gh, warnings);
                    if unavailable {
                        return (Vec::new(), true);
                    }
                    cids.extend(more);
                }
            }
            threads.push(Thread {
                is_resolved: truthy(node.get("isResolved")),
                is_outdated: truthy(node.get("isOutdated")),
                path: node.get("path").and_then(Value::as_str).map(str::to_string),
                comment_ids: cids,
            });
        }
        let page = conn.get("pageInfo").cloned().unwrap_or(json!({}));
        match page
            .get("endCursor")
            .and_then(Value::as_str)
            .filter(|c| !c.is_empty())
        {
            Some(end) if truthy(page.get("hasNextPage")) => cursor = Some(end.to_string()),
            _ => break,
        }
    }
    (threads, false)
}

fn fetch_remaining_thread_comment_ids(
    thread_id: &str,
    after: Option<String>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut cursor = after;
    for _ in 0..50 {
        let Some(c) = cursor.clone().filter(|c| !c.is_empty()) else {
            break;
        };
        let mut argv = args(&["api", "graphql", "-f"]);
        argv.push(format!("query={THREAD_COMMENTS_QUERY}"));
        argv.extend([
            "-f".to_string(),
            format!("id={thread_id}"),
            "-f".to_string(),
            format!("cursor={c}"),
        ]);
        let (rc, body, err) = gh(&argv);
        if rc != 0 {
            warnings.push(format!(
                "review harvest: `gh api graphql` thread-comments page failed (rc={rc}) ({}); retaining sentinel",
                err_head(&err, 120)
            ));
            return (Vec::new(), true);
        }
        let data: Value = if body.trim().is_empty() {
            json!({})
        } else {
            match serde_json::from_str(&body) {
                Ok(v) => v,
                Err(_) => {
                    warnings.push(
                        "review harvest: could not parse thread-comments page output".to_string(),
                    );
                    return (Vec::new(), true);
                }
            }
        };
        let node = data.pointer("/data/node").filter(|v| !v.is_null());
        if truthy(data.get("errors")) || node.is_none() {
            warnings.push("review harvest: thread-comments page returned errors/null node; retaining sentinel".to_string());
            return (Vec::new(), true);
        }
        let comments = node
            .and_then(|n| n.get("comments"))
            .cloned()
            .unwrap_or(json!({}));
        out.extend(database_ids(comments.get("nodes")));
        let cpage = comments.get("pageInfo").cloned().unwrap_or(json!({}));
        cursor = if truthy(cpage.get("hasNextPage")) {
            cpage
                .get("endCursor")
                .and_then(Value::as_str)
                .map(str::to_string)
        } else {
            None
        };
    }
    (out, false)
}

/// Comment ids in a RESOLVED thread.
pub fn resolved_ids_from_threads(threads: &[Thread]) -> HashSet<String> {
    threads
        .iter()
        .filter(|t| t.is_resolved)
        .flat_map(|t| t.comment_ids.iter().cloned())
        .collect()
}

/// Comment ids in a thread the LATEST state shows addressed: resolved, or
/// outdated (the anchored hunk changed after the comment).
pub fn addressed_ids_from_threads(threads: &[Thread]) -> HashSet<String> {
    threads
        .iter()
        .filter(|t| t.is_resolved || t.is_outdated)
        .flat_map(|t| t.comment_ids.iter().cloned())
        .collect()
}

/// One row of the author's consolidated "Skipped" reply table.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SkippedRow {
    pub reviewer: String,
    pub file: String,
    pub issue: String,
    pub reason: String,
}

fn skipped_header(line: &str) -> bool {
    static HEADER: OnceLock<Regex> = OnceLock::new();
    re(&HEADER, r"(?i)^#{2,4}\s+skipped\b").is_match(line)
}

/// Rows of a "### Skipped" markdown table; no section or table reads empty.
pub fn parse_skipped_table(text: &str) -> Vec<SkippedRow> {
    let mut rows = Vec::new();
    let mut in_section = false;
    for line in py_splitlines(text) {
        let stripped = line.trim();
        if skipped_header(stripped) {
            in_section = true;
            continue;
        }
        if !in_section {
            continue;
        }
        if stripped.starts_with('#') {
            break;
        }
        if !stripped.starts_with('|') {
            continue;
        }
        let mut cells: Vec<String> = stripped
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_string())
            .collect();
        let joined: String = cells.concat();
        if joined.is_empty() || joined.chars().all(|ch| matches!(ch, '-' | ':' | ' ')) {
            continue;
        }
        if cells
            .first()
            .is_some_and(|c| c.to_lowercase() == "reviewer")
        {
            continue;
        }
        while cells.len() < 4 {
            cells.push(String::new());
        }
        rows.push(SkippedRow {
            reviewer: cells[0].clone(),
            file: cells[1].trim_matches('`').trim().to_string(),
            issue: cells[2].clone(),
            reason: cells[3].clone(),
        });
    }
    rows
}

/// Every Skipped-table row from the PR's issue comments. A cosmetic
/// cross-check only, so a failure warns without retaining the trigger.
pub fn fetch_skipped_rows(
    pr: i64,
    repo: Option<&str>,
    gh: &GhRunner,
    warnings: &mut Vec<String>,
) -> (Vec<SkippedRow>, bool) {
    let path = pulls_path(repo, pr, "issues/{pr}/comments");
    let (rc, out, err) = gh(&args(&["api", &path, "--paginate", "--slurp"]));
    if rc != 0 {
        warnings.push(format!(
            "review harvest: `gh api` issue-comments failed (rc={rc}) for PR #{pr}; skipping author Skipped-table cross-check ({})",
            err_head(&err, 80)
        ));
        return (Vec::new(), true);
    }
    let Ok(raw) = parse_out(&out) else {
        warnings.push("review harvest: could not parse issue-comments output".to_string());
        return (Vec::new(), true);
    };
    let mut rows = Vec::new();
    for c in flatten_pages(&raw) {
        let body = c.get("body").map_or_else(String::new, |v| py_str(Some(v)));
        if py_splitlines(&body)
            .iter()
            .any(|ln| skipped_header(ln.trim()))
        {
            rows.extend(parse_skipped_table(&body));
        }
    }
    (rows, false)
}

/// Skipped rows mapped to comment ids by file path: full path, or a
/// basename on a path boundary, so `a.py` never matches `schema.py`.
pub fn skipped_ids_from_rows(rows: &[SkippedRow], threads: &[Thread]) -> HashSet<String> {
    let mut out = HashSet::new();
    if rows.is_empty() || threads.is_empty() {
        return out;
    }
    let mut by_path: Vec<(String, BTreeSet<String>)> = Vec::new();
    for t in threads {
        let p = t.path.as_deref().unwrap_or("").trim().to_string();
        if p.is_empty() {
            continue;
        }
        match by_path.iter_mut().find(|(k, _)| *k == p) {
            Some((_, ids)) => ids.extend(t.comment_ids.iter().cloned()),
            None => by_path.push((p, t.comment_ids.iter().cloned().collect())),
        }
    }
    for row in rows {
        let rfile = row.file.trim_matches('`').trim();
        if rfile.is_empty() {
            continue;
        }
        for (p, ids) in &by_path {
            if p == rfile || p.ends_with(&format!("/{rfile}")) || rfile.ends_with(&format!("/{p}"))
            {
                out.extend(ids.iter().cloned());
            }
        }
    }
    out
}

/// The bullets under a COMPLETION.md "## Deferred Findings" section.
pub fn harvest_deferred_findings_text(text: &str, source_pr: Option<i64>) -> Vec<RawItem> {
    let mut items = Vec::new();
    let mut in_section = false;
    for line in py_splitlines(text) {
        if line
            .trim()
            .to_lowercase()
            .starts_with("## deferred findings")
        {
            in_section = true;
            continue;
        }
        if in_section && line.starts_with("## ") {
            break;
        }
        let lead = line.trim_start();
        if in_section && lead.starts_with("- ") {
            let finding = lead[2..].trim();
            if !finding.is_empty() {
                items.push(RawItem {
                    kind: KIND_DEFERRED.to_string(),
                    text: finding.to_string(),
                    source_pr,
                    source_id: format!("deferred:{}", items.len()),
                    ..RawItem::default()
                });
            }
        }
    }
    items
}

pub fn harvest_deferred_findings(completion_md: &Path, source_pr: Option<i64>) -> Vec<RawItem> {
    match std::fs::read_to_string(completion_md) {
        Ok(text) => harvest_deferred_findings_text(&text, source_pr),
        Err(_) => Vec::new(),
    }
}
