//! The `list` operation behind the existing `fno-agents pr-heal` action:
//! open, closed, or all PRs of one GitHub repo, with each open row's binding.
//!
//! The repo defaults to the origin of the cwd. A job scratch dir can sit inside
//! a different checkout, so `[]` from there is a true answer about the wrong
//! repo. Every successful listing therefore ends with one stderr receipt that
//! names the repo it read and where that choice came from. A failed read never
//! prints the receipt and exits 4, so an empty repo and a failed read differ.

use crate::org_board::budget::run_with_timeout;
use crate::org_board::prs::pr_binding_verdicts;
use serde_json::{json, Map, Value};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 20;

type Runner<'a> = &'a dyn Fn(&[String], &Path) -> Result<Vec<u8>, String>;
type GraphRead<'a> = &'a dyn Fn() -> Result<Vec<Value>, String>;

struct Outcome {
    code: i32,
    stdout: String,
    stderr: Vec<String>,
}

pub fn run_pr_list(args: &[String]) -> i32 {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => {
            println!(
                "{}",
                json!({"error": format!("could not resolve owner/repo: {e}")})
            );
            return 4;
        }
    };
    let gh = |cmd: &[String], cwd: &Path| {
        run_with_timeout(cmd, cwd, Duration::from_secs(30)).map_err(|e| {
            if e.over_budget() {
                "timed out after 30s".to_string()
            } else {
                e.message().to_string()
            }
        })
    };
    let graph = || {
        let path = crate::org_board::scope::graph_json_path(&cwd);
        let store = crate::backlog::api::Store::new(&path);
        crate::graph_store::read_rows_where(
            &store.graph,
            &crate::backlog::RowQuery {
                fields: Some(
                    [
                        "id",
                        "slug",
                        "status",
                        "completed_at",
                        "superseded_by",
                        "pr_number",
                        "pr_url",
                        "additional_prs",
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
        .map_err(|e| e.0)
    };
    let out = list(args, &cwd, None, &gh, &graph);
    println!("{}", out.stdout);
    for line in out.stderr {
        eprintln!("{line}");
    }
    out.code
}

fn fail(code: i32, error: String) -> Outcome {
    Outcome {
        code,
        stdout: json!({ "error": error }).to_string(),
        stderr: Vec::new(),
    }
}

fn list(
    args: &[String],
    cwd: &Path,
    git_ceiling: Option<&Path>,
    gh: Runner,
    graph: GraphRead,
) -> Outcome {
    let mut state = "open".to_string();
    let mut repo: Option<String> = None;
    let mut since: Option<String> = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match (arg.as_str(), it.next()) {
            ("--state", Some(v)) => state = v.clone(),
            ("--repo", Some(v)) => repo = Some(v.clone()),
            ("--since", Some(v)) => since = Some(v.clone()),
            _ => {
                return fail(
                    2,
                    format!(
                    "usage: pr-heal list [--state open|closed|all] [--repo owner/repo] [--since YYYY-MM-DD]; got {arg}"
                ),
                )
            }
        }
    }
    if !matches!(state.as_str(), "open" | "closed" | "all") {
        return fail(2, "--state must be open, closed, or all".to_string());
    }
    if let Some(day) = &since {
        if chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() {
            return fail(2, format!("--since must be YYYY-MM-DD; got {day}"));
        }
    }
    let (slug, source) = match repo {
        Some(r) => (r, "--repo".to_string()),
        None => match origin_slug(cwd, git_ceiling) {
            Ok(s) => (
                s,
                format!(
                    "origin of {}; pass --repo owner/repo to choose",
                    home_relative(cwd)
                ),
            ),
            Err(reason) => return fail(4, format!("could not resolve owner/repo: {reason}")),
        },
    };
    let mut rows: Vec<Value> = Vec::new();
    let mut full_pages = 0;
    // Newest first, so a `--since` read stops at the first older page
    // instead of walking every page of the repo's history.
    let mut past_since = false;
    for page in 1..=MAX_PAGES {
        let cmd: Vec<String> = [
            "gh".to_string(),
            "api".to_string(),
            format!("repos/{slug}/pulls?state={state}&sort=created&direction=desc&per_page={PAGE_SIZE}&page={page}"),
        ]
        .to_vec();
        let raw = match gh(&cmd, cwd) {
            Ok(raw) => raw,
            Err(e) => return fail(4, format!("gh api pulls list page {page} failed: {e}")),
        };
        let parsed: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => {
                return fail(
                    4,
                    format!("gh api pulls list page {page} returned non-JSON"),
                )
            }
        };
        let Some(items) = parsed.as_array() else {
            return fail(
                4,
                format!("gh api pulls list page {page} returned a non-array"),
            );
        };
        for item in items {
            match summary(item) {
                // ISO-8601 UTC stamps order as strings, and a bare day
                // sorts before every stamp on that day.
                Some(row)
                    if since
                        .as_deref()
                        .is_some_and(|day| row["createdAt"].as_str() < Some(day)) =>
                {
                    past_since = true;
                }
                Some(row) => rows.push(row),
                None => {
                    return fail(
                        4,
                        format!("gh api pulls list page {page} carried a malformed row"),
                    )
                }
            }
        }
        if items.len() < PAGE_SIZE || past_since {
            break;
        }
        full_pages += 1;
    }
    let mut stderr = Vec::new();
    if full_pages == MAX_PAGES {
        stderr.push(format!(
            "pr list: listing possibly truncated after {} rows",
            rows.len()
        ));
    }
    bind(&mut rows, graph);
    for row in rows.iter_mut() {
        if let Some(obj) = row.as_object_mut() {
            obj.shift_remove("body");
        }
    }
    stderr.push(format!("pr list: {slug} ({source}), {} rows", rows.len()));
    Outcome {
        code: 0,
        stdout: Value::Array(rows).to_string(),
        stderr,
    }
}

/// The listing row: `number, state, title, headRefName, url`, the three
/// `createdAt`/`closedAt`/`mergedAt` stamps (null when unset), plus `body`
/// for the binding reader (removed before print). None on a malformed row.
fn summary(item: &Value) -> Option<Value> {
    let number = item.get("number")?.as_i64()?;
    let title = item.get("title")?.as_str()?;
    let head = item.get("head")?.get("ref")?.as_str()?;
    let url = item.get("html_url")?.as_str()?;
    let state = if item.get("merged_at").is_some_and(|v| !v.is_null()) {
        "MERGED"
    } else {
        match item.get("state").and_then(Value::as_str) {
            Some("open") => "OPEN",
            Some("closed") => "CLOSED",
            _ => "UNKNOWN",
        }
    };
    let mut row = Map::new();
    row.insert("number".into(), json!(number));
    row.insert("state".into(), json!(state));
    row.insert("title".into(), json!(title));
    row.insert("headRefName".into(), json!(head));
    // GitHub's own GraphQL name: true also for a deleted fork (a null
    // head repo), which reads outside everywhere the field is read.
    let head_repo = item.pointer("/head/repo/full_name").and_then(Value::as_str);
    let base_repo = item.pointer("/base/repo/full_name").and_then(Value::as_str);
    row.insert(
        "isCrossRepository".into(),
        json!(match (head_repo, base_repo) {
            (Some(h), Some(b)) => h != b,
            _ => true,
        }),
    );
    row.insert("url".into(), json!(url));
    for (key, field) in [
        ("createdAt", "created_at"),
        ("closedAt", "closed_at"),
        ("mergedAt", "merged_at"),
    ] {
        row.insert(key.into(), item.get(field).cloned().unwrap_or(Value::Null));
    }
    row.insert(
        "body".into(),
        json!(item.get("body").and_then(Value::as_str).unwrap_or("")),
    );
    Some(Value::Object(row))
}

fn bind(rows: &mut [Value], graph: GraphRead) {
    let is_open = |r: &Value| r.get("state").and_then(Value::as_str) == Some("OPEN");
    let open: Vec<&Value> = rows.iter().filter(|row| is_open(row)).collect();
    if open.is_empty() {
        return;
    }
    let entries = match graph() {
        Ok(e) => e,
        Err(e) => {
            for row in rows.iter_mut().filter(|r| is_open(r)) {
                row["node_binding_error"] = json!(format!("graph binding read failed: {e}"));
            }
            return;
        }
    };
    for v in pr_binding_verdicts(open, &entries) {
        let Some(row) = rows
            .iter_mut()
            .find(|r| is_open(r) && r.get("number").and_then(Value::as_i64) == Some(v.number))
        else {
            continue;
        };
        row["node_id"] = json!(v.node_id);
        row["node_binding"] = json!(v.verdict);
        if let Some(detail) = v.detail {
            row["node_binding_detail"] = json!(detail);
        }
    }
}

pub(crate) fn origin_slug(cwd: &Path, git_ceiling: Option<&Path>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(["remote", "get-url", "origin"]).current_dir(cwd);
    if let Some(ceiling) = git_ceiling {
        cmd.env("GIT_CEILING_DIRECTORIES", ceiling);
    }
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        // Python `_repo_slug_reason`'s rule: the reason is git's own first
        // stderr line, userinfo-redacted, falling back to the bare sentence.
        // The old "no origin remote" substitution broke the parity the
        // docstring claimed.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let first = stderr
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let redacted = redact_userinfo(first);
        return Err(if redacted.is_empty() {
            "no origin remote".to_string()
        } else {
            redacted
        });
    }
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    parse_origin_slug(&url)
        .ok_or_else(|| format!("origin is not a github remote: {}", redact_userinfo(&url)))
}

/// `owner/repo` from a GitHub remote URL (_parse_origin_slug): lowercase,
/// strip scheme and `user@`, require host `github.com`, strip a trailing `/`
/// and `.git`, and require exactly `owner/repo`.
pub(crate) fn parse_origin_slug(url: &str) -> Option<String> {
    let re = regex::Regex::new(
        r"^(?:[a-z][a-z0-9+.-]*://)?(?:[^/@]*@)?github\.com(?::[0-9]+)?[:/](.+)$",
    )
    .ok()?;
    let lower = url.trim().to_lowercase();
    let rest = re.captures(&lower)?.get(1)?.as_str().trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (owner, repo) = rest.split_once('/')?;
    (!owner.is_empty() && !repo.is_empty() && !repo.contains('/')).then(|| rest.to_string())
}

fn redact_userinfo(url: &str) -> String {
    match (url.find("://"), url.find('@')) {
        (Some(scheme), Some(at)) if at > scheme && !url[scheme + 3..at].contains('/') => {
            format!("{}{}", &url[..scheme + 3], &url[at + 1..])
        }
        _ => url.to_string(),
    }
}

fn home_relative(path: &Path) -> String {
    let shown = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && home != "/" => {
            if shown == home {
                "~".to_string()
            } else if let Some(rest) = shown.strip_prefix(&format!("{home}/")) {
                format!("~/{rest}")
            } else {
                shown
            }
        }
        _ => shown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_slug_rejects_lookalike_hosts() {
        // A lookalike domain or a github.com path segment yields no slug (a
        // substring match would admit a foreign repo's node).
        let look = [
            "https://notgithub.com/o/r.git",
            "https://gitlab.com/mirrors/github.com/o/r.git",
            "/tmp/github.com/o/r.git",
            "https://github.com.evil.test/o/r.git",
            "git@gitlab.com:mirror/x.git",
        ];
        for url in look {
            assert_eq!(parse_origin_slug(url), None, "url {url}");
        }
    }

    fn git_checkout(origin: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [vec!["init", "-q"], vec!["remote", "add", "origin", origin]] {
            let ok = Command::new("git")
                .args(&args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        }
        dir
    }

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    fn no_graph() -> Result<Vec<Value>, String> {
        panic!("graph read with no open rows")
    }

    #[test]
    fn open_rows_surface_binding_verdicts_and_never_emit_body() {
        let gh = |_: &[String], _: &Path| {
            Ok(json!([
                {
                    "number": 7, "state": "open", "merged_at": null, "title": "bound",
                    "head": {"ref": "feature/x-aaaa", "repo": {"full_name": "o/r"}},
                    "base": {"repo": {"full_name": "o/r"}},
                    "html_url": "https://github.com/o/r/pull/7",
                    "body": "text",
                },
                {
                    "number": 8, "state": "open", "merged_at": null, "title": "missing",
                    "head": {"ref": "feature/x-bbbb", "repo": {"full_name": "o/r"}},
                    "base": {"repo": {"full_name": "o/r"}},
                    "html_url": "https://github.com/o/r/pull/8",
                    "body": "text",
                },
                {
                    "number": 9, "state": "open", "merged_at": null, "title": "untracked",
                    "head": {"ref": "chore/tidy-docs", "repo": {"full_name": "o/r"}},
                    "base": {"repo": {"full_name": "o/r"}},
                    "html_url": "https://github.com/o/r/pull/9",
                    "body": "Just a fix.",
                },
                {
                    "number": 110, "state": "open", "merged_at": null, "title": "ambiguous",
                    "head": {"ref": "feature/x-aaaa-x-bbbb", "repo": {"full_name": "o/r"}},
                    "base": {"repo": {"full_name": "o/r"}},
                    "html_url": "https://github.com/o/r/pull/110",
                    "body": "text",
                },
            ])
            .to_string()
            .into_bytes())
        };
        let graph = || {
            Ok(vec![
                json!({"id": "x-aaaa", "pr_number": 7, "pr_url": "https://github.com/o/r/pull/7"}),
                json!({"id": "x-bbbb", "status": "ready"}),
            ])
        };
        let out = list(&args(&["--repo", "o/r"]), Path::new("/"), None, &gh, &graph);
        assert_eq!(out.code, 0);
        let rows: Value = serde_json::from_str(&out.stdout).unwrap();
        let rows = rows.as_array().unwrap();
        let row = |number: i64| {
            rows.iter()
                .find(|row| row["number"].as_i64() == Some(number))
                .expect("listed row")
        };
        let bound = row(7).as_object().unwrap();
        let keys: Vec<&str> = bound.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec![
                "number",
                "state",
                "title",
                "headRefName",
                "isCrossRepository",
                "url",
                "createdAt",
                "closedAt",
                "mergedAt",
                "node_id",
                "node_binding"
            ]
        );
        assert!(bound["createdAt"].is_null());
        assert_eq!(bound["node_id"], "x-aaaa");
        assert_eq!(bound["node_binding"], "bound");
        assert_eq!(row(8)["node_binding"], "missing");
        assert_eq!(row(8)["node_id"], "x-bbbb");
        assert_eq!(row(9)["node_binding"], "untracked");
        assert!(row(9)["node_binding_detail"]
            .as_str()
            .unwrap()
            .contains("chore/tidy-docs"));
        assert_eq!(row(110)["node_binding"], "ambiguous");
        assert!(rows.iter().all(|row| row.get("body").is_none()));
        assert_eq!(
            out.stderr,
            vec!["pr list: o/r (--repo), 4 rows".to_string()]
        );
    }

    #[test]
    fn since_keeps_rows_created_that_day_and_stops_paging_at_older_ones() {
        let pages = std::cell::Cell::new(0);
        let gh = |cmd: &[String], _: &Path| {
            pages.set(pages.get() + 1);
            assert!(cmd[2].contains("sort=created&direction=desc"), "{}", cmd[2]);
            let pr = |number: i64, created: &str, merged: Value| {
                json!({
                    "number": number, "state": "closed", "merged_at": merged,
                    "created_at": created, "closed_at": created, "title": "t",
                    "head": {"ref": "chore/t", "repo": {"full_name": "o/r"}},
                    "base": {"repo": {"full_name": "o/r"}},
                    "html_url": format!("https://github.com/o/r/pull/{number}"),
                })
            };
            // A full page, so only the older row can stop the paging.
            let mut page: Vec<Value> = (0..PAGE_SIZE as i64)
                .map(|n| pr(900 - n, "2026-10-24T09:00:00Z", Value::Null))
                .collect();
            page[0] = pr(901, "2026-10-24T00:00:00Z", json!("2026-10-24T01:00:00Z"));
            page[PAGE_SIZE - 1] = pr(1, "2026-10-23T23:59:59Z", Value::Null);
            Ok(Value::Array(page).to_string().into_bytes())
        };
        let out = list(
            &args(&["--repo", "o/r", "--state", "all", "--since", "2026-10-24"]),
            Path::new("/"),
            None,
            &gh,
            &no_graph,
        );
        assert_eq!(out.code, 0, "{}", out.stdout);
        assert_eq!(pages.get(), 1);
        let rows: Value = serde_json::from_str(&out.stdout).unwrap();
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), PAGE_SIZE - 1);
        assert_eq!(rows[0]["state"], "MERGED");
        assert_eq!(rows[0]["mergedAt"], "2026-10-24T01:00:00Z");
        assert!(rows.iter().all(|r| r["number"] != 1));

        let bad = list(
            &args(&["--repo", "o/r", "--since", "yesterday"]),
            Path::new("/"),
            None,
            &gh,
            &no_graph,
        );
        assert_eq!(bad.code, 2);
        assert!(bad.stdout.contains("--since must be YYYY-MM-DD"));
    }

    #[test]
    fn a_failed_read_exits_4_with_a_named_error_and_no_receipt() {
        let gh = |_: &[String], _: &Path| Err("exit 1: HTTP 502".to_string());
        let out = list(
            &args(&["--repo", "o/r"]),
            Path::new("/"),
            None,
            &gh,
            &no_graph,
        );
        assert_eq!(out.code, 4);
        let err: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(
            err["error"],
            "gh api pulls list page 1 failed: exit 1: HTTP 502"
        );
        assert!(out.stderr.is_empty());
        for bad in [&b"not json"[..], &b"{}"[..], &br#"[{"number": 1}]"#[..]] {
            let gh = move |_: &[String], _: &Path| Ok(bad.to_vec());
            let out = list(
                &args(&["--repo", "o/r"]),
                Path::new("/"),
                None,
                &gh,
                &no_graph,
            );
            assert_eq!(out.code, 4, "{}", out.stdout);
            assert!(out.stderr.is_empty());
        }
    }

    #[test]
    fn no_checkout_or_a_foreign_origin_refuses_without_leaking_the_token() {
        let gh = |_: &[String], _: &Path| panic!("gh called without a slug");
        let bare = tempfile::tempdir().unwrap();
        let out = list(&[], bare.path(), Some(bare.path()), &gh, &no_graph);
        assert_eq!(out.code, 4);
        let err: Value = serde_json::from_str(&out.stdout).unwrap();
        assert!(err["error"]
            .as_str()
            .unwrap()
            .starts_with("could not resolve owner/repo"));

        let dir = git_checkout("https://x-access-token:secret@gitlab.com/a/b.git");
        let out = list(&[], dir.path(), Some(dir.path()), &gh, &no_graph);
        assert_eq!(out.code, 4);
        assert!(
            out.stdout.contains("origin is not a github remote"),
            "{}",
            out.stdout
        );
        assert!(!out.stdout.contains("secret"));
    }
}
