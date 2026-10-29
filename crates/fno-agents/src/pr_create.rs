//! `fno do pr create` (binary command `pr-create`) -- the duplicate-guarded
//! create. On 2026-10-04 three PRs fixed one bug on the same file within an
//! hour: the graph was down, so no claim or fold check could see the others.
//! This command reads open PRs from the GitHub REST API (no graph read: the
//! outage is exactly when the guard must work) and refuses when an open PR
//! touches the same changed files with an overlapping subject. The refusal
//! names the PR, its branch and its author; an explicit `--not-duplicate <n>`
//! passes it. On pass it execs `gh pr create --title --body-file`.
//! `--check-only` runs the scan and the gates without creating (exit 0 clean,
//! 3 twin, 5 or 6 a gate refusal) for callers that create through another
//! door, such as the worker and batch ship paths.
//!
//! Exit codes:
//! * `0` the PR was created (gh's output passes through), or the scan was
//!   clean under `--check-only`
//! * `3` a duplicate refused
//! * `2` usage or local read error
//! * `4` a GitHub read failed
//! * `5` the audited-owners gate refused: a code PR whose mapped owner test
//!   files went unaudited
//! * `6` the unknowns gate refused: the session's bound plan still lists an
//!   unchecked `- [ ]` line under `## Unknowns`
//! * `7` the sibling gate refused: another node of the same epic is on an
//!   open PR, or on one merged in the last two hours; a body line
//!   `Separate PR: <reason>` passes it
//! * otherwise gh's own create exit code

use crate::pr_push::{gh_api, run_labeled, READ_TIMEOUT};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CREATE_TIMEOUT: Duration = Duration::from_secs(300);
/// Open-PR pages read before the scan gives up expanding (a warning names the
/// bound); 100 rows per page.
const MAX_PAGES: usize = 3;
const USAGE: &str = "usage: fno do pr create --title <t> --body-file <path> [--base <branch>] [--not-duplicate <pr>]... | --check-only --title <t> [--base <branch>]";

#[derive(Debug)]
struct Args {
    title: String,
    body_file: String,
    base: String,
    not_duplicates: Vec<i64>,
    check_only: bool,
    cwd: PathBuf,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        title: String::new(),
        body_file: String::new(),
        base: "main".to_string(),
        not_duplicates: Vec::new(),
        check_only: false,
        cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let mut i = 0;
    while i < argv.len() {
        let take = |name: &str| -> Result<String, String> {
            argv.get(i + 1)
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argv[i].as_str() {
            "--title" => {
                a.title = take("--title")?;
                i += 1;
            }
            "--body-file" => {
                a.body_file = take("--body-file")?;
                i += 1;
            }
            "--base" => {
                a.base = take("--base")?;
                i += 1;
            }
            "--not-duplicate" => {
                let raw = take("--not-duplicate")?;
                let n: i64 = raw
                    .trim()
                    .parse()
                    .map_err(|_| format!("--not-duplicate wants a PR number, got {raw}"))?;
                a.not_duplicates.push(n);
                i += 1;
            }
            // The scan-only posture the worker and batch create sites run
            // before their own `gh pr create`: refuse on a twin, never create.
            "--check-only" => {
                a.check_only = true;
            }
            // Test seam, same as pr-body-check's.
            "--cwd" => {
                a.cwd = PathBuf::from(take("--cwd")?);
                i += 1;
            }
            other => return Err(format!("unknown flag: {other}\n{USAGE}")),
        }
        i += 1;
    }
    if a.title.is_empty() {
        return Err(format!("--title is required\n{USAGE}"));
    }
    if !a.check_only && a.body_file.is_empty() {
        return Err(format!("--body-file is required\n{USAGE}"));
    }
    Ok(a)
}

fn git(cwd: &Path, args: &[&str]) -> Result<String, String> {
    let (ok, out, err) = run_labeled("pr-create", "git", args, cwd, READ_TIMEOUT)?;
    if ok {
        Ok(out.trim().to_string())
    } else {
        Err(if err.trim().is_empty() {
            out.trim().to_string()
        } else {
            err.trim().to_string()
        })
    }
}

/// Title words that decide an "overlapping subject": conventional-commit
/// prefix (`type(scope):`) stripped, lowercased, split on non-alphanumerics,
/// tokens under three characters dropped.
fn subject_tokens(title: &str) -> Vec<String> {
    let body = match title.find(": ") {
        Some(idx) if idx < 40 => &title[idx + 2..],
        _ => title,
    };
    body.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 3)
        .map(String::from)
        .collect()
}

fn subjects_overlap(a: &str, b: &str) -> bool {
    let (ta, tb) = (subject_tokens(a), subject_tokens(b));
    if ta.is_empty() || tb.is_empty() {
        return false;
    }
    let shared = ta.iter().filter(|t| tb.contains(t)).count();
    shared >= 2 && shared * 2 >= ta.len().min(tb.len())
}

struct OpenPr {
    number: i64,
    title: String,
    branch: String,
    author: String,
    body: String,
}

/// One GitHub REST read: a repo-relative path with its query. Prod wires
/// `pr_push::gh_api`; tests inject.
type Gh<'a> = &'a dyn Fn(&str) -> Result<String, String>;

fn open_prs(slug: &str, gh: Gh) -> Result<Vec<OpenPr>, String> {
    let mut rows = Vec::new();
    for page in 1..=MAX_PAGES {
        let raw = gh(&format!(
            "repos/{slug}/pulls?state=open&per_page=100&page={page}"
        ))?;
        let parsed: Value = serde_json::from_str(&raw)
            .map_err(|_| format!("gh api pulls list page {page} returned non-JSON"))?;
        let Some(items) = parsed.as_array() else {
            return Err(format!(
                "gh api pulls list page {page} returned a non-array"
            ));
        };
        for item in items {
            let number = item.get("number").and_then(Value::as_i64);
            let title = item.get("title").and_then(Value::as_str);
            let branch = item
                .get("head")
                .and_then(|h| h.get("ref"))
                .and_then(Value::as_str);
            let author = item
                .get("user")
                .and_then(|u| u.get("login"))
                .and_then(Value::as_str);
            let body = item.get("body").and_then(Value::as_str).unwrap_or("");
            match (number, title, branch, author) {
                (Some(n), Some(t), Some(b), Some(a)) => rows.push(OpenPr {
                    number: n,
                    title: t.to_string(),
                    branch: b.to_string(),
                    author: a.to_string(),
                    body: body.to_string(),
                }),
                _ => {
                    return Err(format!(
                        "gh api pulls list page {page} carried a malformed row"
                    ))
                }
            }
        }
        if items.len() < 100 {
            break;
        }
        if page == MAX_PAGES {
            eprintln!(
                "pr-create: open-PR scan stopped at {} pages ({} rows); later pages unread",
                MAX_PAGES,
                rows.len()
            );
        }
    }
    Ok(rows)
}

fn pr_files(slug: &str, number: i64, gh: Gh) -> Result<Vec<String>, String> {
    let raw = gh(&format!("repos/{slug}/pulls/{number}/files?per_page=100"))?;
    let parsed: Value =
        serde_json::from_str(&raw).map_err(|_| format!("gh api files for #{number} non-JSON"))?;
    let Some(items) = parsed.as_array() else {
        return Err(format!("gh api files for #{number} a non-array"));
    };
    Ok(items
        .iter()
        .filter_map(|f| f.get("filename").and_then(Value::as_str))
        .map(String::from)
        .collect())
}

/// The first open PR that shares changed files with an overlapping subject,
/// or None when the create may proceed.
fn find_duplicate(
    title: &str,
    my_files: &[String],
    branch: &str,
    not_duplicates: &[i64],
    gh: Gh,
    slug: &str,
) -> Result<Option<(OpenPr, Vec<String>)>, String> {
    for pr in open_prs(slug, gh)? {
        if pr.branch == branch || not_duplicates.contains(&pr.number) {
            continue;
        }
        if !subjects_overlap(title, &pr.title) {
            continue;
        }
        let their_files = pr_files(slug, pr.number, gh)?;
        let shared: Vec<String> = my_files
            .iter()
            .filter(|f| their_files.contains(f))
            .cloned()
            .collect();
        if !shared.is_empty() {
            return Ok(Some((pr, shared)));
        }
    }
    Ok(None)
}

pub fn run_pr_create_command(argv: &[String]) -> i32 {
    if let Some(first) = argv.first() {
        if first.trim_start().starts_with('{') {
            return run_stdin_check_only(first);
        }
    }
    let a = match parse_args(argv) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("pr-create: {msg}");
            return 2;
        }
    };
    run(&a, &|path| gh_api("gh", &a.cwd, path, &[]))
}

/// One JSON request on stdin (the argv form starts with `{`): the shape
/// `command_call("pr-create", {...})` sends. The twin scan, then the sibling
/// gate over the payload's `body`, so the worker ship path meets the same
/// one-PR-per-epic rule as the argv create leg. The owners and unknowns gates
/// stay create-leg gates.
fn run_stdin_check_only(payload: &str) -> i32 {
    let parsed: Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pr-create: unreadable payload: {e}");
            return 2;
        }
    };
    let title = parsed.get("title").and_then(Value::as_str).unwrap_or("");
    let base = parsed.get("base").and_then(Value::as_str).unwrap_or("main");
    let cwd = parsed
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
    let branch = git(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty() && b != "HEAD");
    let my_files: Vec<String> = git(
        &cwd,
        &[
            "diff",
            "--name-only",
            &format!("{}...HEAD", crate::pr_body_check::base_ref(base)),
        ],
    )
    .map(|f| f.lines().map(String::from).collect())
    .unwrap_or_default();
    let slug = crate::pr_list::origin_slug(&cwd, None).unwrap_or_default();
    let refusal = match find_duplicate(
        title,
        &my_files,
        branch.as_deref().unwrap_or(""),
        &[],
        &|path| gh_api("gh", &cwd, path, &[]),
        &slug,
    ) {
        Ok(Some((pr, shared))) => {
            let shown = shared
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if shared.len() > 5 {
                format!(" (and {} more)", shared.len() - 5)
            } else {
                String::new()
            };
            format!(
                "pr-create: REFUSED: open PR #{} already touches the same changed files with an overlapping subject\n  PR:     #{} {}\n  branch: {}\n  files:  {shown}{more}",
                pr.number, pr.number, pr.title, pr.branch
            )
        }
        _ => String::new(),
    };
    let body = parsed.get("body").and_then(Value::as_str).unwrap_or("");
    let refusal = match (refusal.is_empty(), branch.as_deref()) {
        (true, Some(b)) => {
            sibling_gate(&cwd, body, b, &slug, &|path| gh_api("gh", &cwd, path, &[]))
                .unwrap_or_default()
        }
        _ => refusal,
    };
    println!("{}", json!({"refusal": refusal, "title": title}));
    0
}

/// The audited-owners gate for the create leg: a code PR (a changed
/// non-test Python or Rust file) whose mapped owner test files went
/// unaudited is refused, so every session either prunes the tests that own
/// its changed files or records in the body why nothing was cut. Returns
/// `Some(5)` on refusal. A body or diff read failure is exit 2, never a
/// refusal: the gate refuses on evidence, not on a broken transport.
fn owners_gate_refusal(a: &Args) -> Option<i32> {
    let body = match std::fs::read_to_string(&a.body_file) {
        Ok(b) => b,
        Err(err) => {
            eprintln!(
                "pr-create: could not read the body file {}: {err}",
                a.body_file
            );
            return Some(2);
        }
    };
    let base = crate::pr_body_check::base_ref(&a.base);
    let diff = match crate::test_delta::diff_range("git", &a.cwd, &format!("{base}...HEAD")) {
        Ok(d) => d,
        Err(msg) => {
            eprintln!("pr-create: could not diff {base}...HEAD for the owners gate: {msg}");
            return Some(2);
        }
    };
    let census = crate::test_delta::census(&diff);
    let py_tests = crate::test_delta::python_test_files(&a.cwd);
    let mut owners = crate::test_delta::owners_of(&census, &py_tests, &a.cwd);
    owners.sort();
    owners.dedup();
    if let Err(msg) = crate::test_delta::owners_gate(&body, &owners, census.total_cuts()) {
        eprintln!("pr-create: REFUSED: {msg}");
        Some(5)
    } else {
        None
    }
}

/// The open unknowns in a plan: each unchecked `- [ ]` line under its
/// `## Unknowns` heading, up to the next `#` or `##` heading. A `###`
/// subheading stays inside the section. A plan with no such section has none,
/// so plans written before the section existed pass.
fn open_unknowns(plan: &str) -> Vec<String> {
    let mut inside = false;
    let mut open = Vec::new();
    for line in plan.lines() {
        if line.starts_with("# ") || line.starts_with("## ") {
            inside = line.trim_end() == "## Unknowns";
            continue;
        }
        if let Some(rest) = line.trim_start().strip_prefix("- [ ]").filter(|_| inside) {
            open.push(rest.trim().to_string());
        }
    }
    open
}

/// The unknowns gate: target settles every unknown the blueprint listed
/// before the PR opens. Returns `Some(6)` naming each open line. No manifest,
/// no bound plan, or an unreadable plan file passes: the gate refuses on
/// evidence only.
fn unknowns_gate_refusal(a: &Args) -> Option<i32> {
    let manifest = crate::target_orient::read_manifest(&a.cwd)?;
    let plan_path = manifest.get("plan_path").filter(|p| !p.is_empty())?;
    let cwd = a.cwd.to_string_lossy();
    let resolved = crate::backlog::promise::resolve_plan_path(plan_path, Some(&cwd))?;
    let index = crate::target_blast::resolve_plan_index(&resolved.to_string_lossy());
    let open = open_unknowns(&std::fs::read_to_string(&index).ok()?);
    if open.is_empty() {
        return None;
    }
    eprintln!(
        "pr-create: REFUSED: {} unknown(s) still open in {}",
        open.len(),
        index.display()
    );
    for u in &open {
        eprintln!("  - [ ] {u}");
    }
    eprintln!("  settle each one: measure it, then write `- [x] <unknown> - settled: <finding>` in the plan");
    Some(6)
}

/// How long a merged sibling PR still refuses a new one for its epic.
const SIBLING_WINDOW: chrono::Duration = chrono::Duration::minutes(120);

/// A PR the sibling gate weighs: open, or merged at `merged_at`.
struct SiblingPr {
    number: i64,
    title: String,
    branch: String,
    body: String,
    merged_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The ids that ship elsewhere in `node`'s epic: every other child of its
/// parent and the nodes contained in each. Nodes contained in `node` itself
/// ship with it and are left out. A decompose group child is a declared PR
/// unit, so a group child never counts another group child (or its leaves)
/// as a sibling. Empty when the node has no parent.
fn epic_sibling_ids(entries: &[Value], node: &str) -> (String, Vec<String>) {
    let text = |e: &Value, k: &str| e.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let Some(row) = entries.iter().find(|e| text(e, "id") == node) else {
        return (String::new(), Vec::new());
    };
    let parent = text(row, "parent");
    if parent.is_empty() {
        return (parent, Vec::new());
    }
    let is_group = !text(row, "group_slug").is_empty();
    let children: Vec<String> = entries
        .iter()
        .filter(|e| text(e, "parent") == parent && text(e, "contained_in") != node)
        .filter(|e| !(is_group && !text(e, "group_slug").is_empty()))
        .map(|e| text(e, "id"))
        .filter(|id| !id.is_empty() && id != node)
        .collect();
    let mut ids = children.clone();
    for e in entries {
        let owner = text(e, "contained_in");
        let id = text(e, "id");
        if children.contains(&owner) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    (parent, ids)
}

/// True when the body carries `Separate PR: <reason>` with a reason.
fn says_separate_pr(body: &str) -> bool {
    body.lines().any(|line| {
        line.trim()
            .strip_prefix("Separate PR:")
            .is_some_and(|reason| !reason.trim().is_empty())
    })
}

/// The refusal text when another PR already ships part of this node's epic:
/// an open PR on another branch, or one merged inside `SIBLING_WINDOW`,
/// whose closure lines name a sibling. None lets the create go on.
fn sibling_refusal(
    node: &str,
    entries: &[Value],
    body: &str,
    branch: &str,
    prs: &[SiblingPr],
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if says_separate_pr(body) {
        return None;
    }
    let (epic, siblings) = epic_sibling_ids(entries, node);
    if siblings.is_empty() {
        return None;
    }
    for pr in prs {
        if pr.branch == branch || pr.merged_at.is_some_and(|at| now - at > SIBLING_WINDOW) {
            continue;
        }
        let claimed = pr
            .body
            .lines()
            .flat_map(crate::org_board::pr_closure::line_ids)
            .find(|id| siblings.contains(id));
        if let Some(sibling) = claimed {
            let state = if pr.merged_at.is_some() {
                "merged in the last two hours"
            } else {
                "open"
            };
            return Some(format!(
                "pr-create: REFUSED: PR #{} ({state}) already ships {sibling}, a sibling of {node} in epic {epic}\n  PR:     #{} {}\n  branch: {}\n  add this work to that PR, or write a `Separate PR: <reason>` line in the body",
                pr.number, pr.number, pr.title, pr.branch
            ));
        }
    }
    None
}

/// The recently merged PRs: one page of closed PRs, newest update first,
/// keeping the merged ones.
fn merged_prs(slug: &str, gh: Gh) -> Result<Vec<SiblingPr>, String> {
    let raw = gh(&format!(
        "repos/{slug}/pulls?state=closed&sort=updated&direction=desc&per_page=50"
    ))?;
    let parsed: Value = serde_json::from_str(&raw)
        .map_err(|_| "gh api closed pulls returned non-JSON".to_string())?;
    let items = parsed
        .as_array()
        .ok_or_else(|| "gh api closed pulls returned a non-array".to_string())?;
    Ok(items
        .iter()
        .filter_map(|item| {
            let merged_at = item
                .get("merged_at")
                .and_then(Value::as_str)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())?
                .with_timezone(&chrono::Utc);
            Some(SiblingPr {
                number: item.get("number").and_then(Value::as_i64)?,
                title: item
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                branch: item
                    .get("head")
                    .and_then(|h| h.get("ref"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                body: item
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                merged_at: Some(merged_at),
            })
        })
        .collect())
}

/// The sibling gate: one PR per epic at a time. Returns the refusal text;
/// the argv door prints it and exits 7, the stdin door returns it as its
/// `refusal`. No manifest, no bound node, or any graph or GitHub read that
/// fails passes with a warning: the gate refuses on evidence only.
fn sibling_gate(cwd: &Path, body: &str, branch: &str, slug: &str, gh: Gh) -> Option<String> {
    let manifest = crate::target_orient::read_manifest(cwd)?;
    let node = manifest.get("graph_node_id").filter(|n| !n.is_empty())?;
    if says_separate_pr(body) {
        return None;
    }
    let entries = match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path())
    {
        Ok(e) => e,
        Err(e) => {
            eprintln!("pr-create: warning: sibling gate skipped, the graph read failed: {e}");
            return None;
        }
    };
    if epic_sibling_ids(&entries, node).1.is_empty() {
        return None;
    }
    let mut prs = match open_prs(slug, gh) {
        Ok(open) => open
            .into_iter()
            .map(|p| SiblingPr {
                number: p.number,
                title: p.title,
                branch: p.branch,
                body: p.body,
                merged_at: None,
            })
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("pr-create: warning: sibling gate skipped, the open-PR read failed: {e}");
            return None;
        }
    };
    match merged_prs(slug, gh) {
        Ok(merged) => prs.extend(merged),
        Err(e) => eprintln!("pr-create: warning: sibling gate read no merged PRs: {e}"),
    }
    sibling_refusal(node, &entries, body, branch, &prs, chrono::Utc::now())
}

fn run(a: &Args, gh: Gh) -> i32 {
    let branch = match git(&a.cwd, &["rev-parse", "--abbrev-ref", "HEAD"]) {
        Ok(b) if !b.is_empty() && b != "HEAD" => b,
        _ => {
            eprintln!("pr-create: could not read the head branch (detached HEAD?)");
            return 2;
        }
    };
    let base = crate::pr_body_check::base_ref(&a.base);
    let my_files = match git(&a.cwd, &["diff", "--name-only", &format!("{base}...HEAD")]) {
        Ok(f) if !f.is_empty() => f.lines().map(String::from).collect::<Vec<_>>(),
        Ok(_) => {
            eprintln!("pr-create: no changed files against {base}; nothing to guard");
            return 2;
        }
        Err(msg) => {
            eprintln!("pr-create: could not diff {base}...HEAD: {msg} (fetched the base ref?)");
            return 2;
        }
    };
    let slug = match crate::pr_list::origin_slug(&a.cwd, None) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("pr-create: could not resolve owner/repo: {msg}");
            return 2;
        }
    };
    match find_duplicate(&a.title, &my_files, &branch, &a.not_duplicates, gh, &slug) {
        Err(msg) => {
            eprintln!("pr-create: {msg}");
            4
        }
        Ok(Some((pr, shared))) => {
            let shown = shared
                .iter()
                .take(5)
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            let more = if shared.len() > 5 {
                format!(" (and {} more)", shared.len() - 5)
            } else {
                String::new()
            };
            eprintln!(
                "pr-create: REFUSED: open PR #{} already touches the same changed files with an overlapping subject",
                pr.number
            );
            eprintln!("  PR:     #{} {}", pr.number, pr.title);
            eprintln!("  branch: {}", pr.branch);
            eprintln!("  author: {}", pr.author);
            eprintln!("  files:  {shown}{more}");
            eprintln!(
                "  this:   {} (branch {branch}, {} files)",
                a.title,
                my_files.len()
            );
            eprintln!("  pass anyway: add --not-duplicate {}", pr.number);
            3
        }
        Ok(None) => {
            // The owners gate runs whenever a body is available: the create
            // leg always, --check-only when a body file was passed (the
            // worker paths that pre-check their own body before gh).
            if !a.body_file.is_empty() {
                if let Some(code) = owners_gate_refusal(&a) {
                    return code;
                }
            }
            if let Some(code) = unknowns_gate_refusal(&a) {
                return code;
            }
            let body = std::fs::read_to_string(&a.body_file).unwrap_or_default();
            if let Some(refusal) = sibling_gate(&a.cwd, &body, &branch, &slug, gh) {
                eprintln!("{refusal}");
                return 7;
            }
            if a.check_only {
                return 0;
            }
            let cmd: Vec<String> = vec![
                "gh".to_string(),
                "pr".to_string(),
                "create".to_string(),
                "--title".to_string(),
                a.title.clone(),
                "--body-file".to_string(),
                a.body_file.clone(),
                "--base".to_string(),
                a.base.clone(),
            ];
            match crate::org_board::budget::run_with_timeout(&cmd, &a.cwd, CREATE_TIMEOUT) {
                Ok(out) => {
                    print!("{}", String::from_utf8_lossy(&out));
                    // The sha origin holds is the PR head; local HEAD can
                    // carry commits that were never pushed.
                    let remote = format!("refs/heads/{branch}");
                    if let Ok(line) = git(&a.cwd, &["ls-remote", "origin", &remote]) {
                        let sha = line.split_whitespace().next().unwrap_or_default();
                        let run = crate::stacked_base_stamp::runner("git", "gh");
                        let base = a.base.trim_start_matches("origin/");
                        crate::stacked_base_stamp::stamp(&a.cwd, base, sha, &run);
                    }
                    0
                }
                Err(e) if e.over_budget() => {
                    eprintln!("pr-create: gh pr create timed out after 300s");
                    4
                }
                Err(e) => {
                    eprintln!("pr-create: gh pr create failed: {}", e.message());
                    2
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_second_branch_editing_one_file_refuses_and_names_the_first() {
        let gh = |path: &str| {
            if path.contains("/pulls?state=open") {
                Ok(json_array(vec![pr_json(
                    3050,
                    "fix(backlog): repair the graph door",
                    "graph-door-hotfix",
                    "bllshttng",
                )]))
            } else if path.contains("/pulls/3050/files") {
                Ok(json_array(vec![file_json(
                    "crates/fno-agents/src/backlog/entities.rs",
                )]))
            } else {
                Err(format!("unexpected gh read: {path}"))
            }
        };
        let dup = find_duplicate(
            "fix(backlog): repair the graph door",
            &["crates/fno-agents/src/backlog/entities.rs".to_string()],
            "feature/duplicate-guard",
            &[],
            &gh,
            "o/r",
        )
        .unwrap();
        let (pr, shared) = dup.expect("the open twin refuses");
        assert_eq!(pr.number, 3050);
        assert_eq!(pr.branch, "graph-door-hotfix");
        assert_eq!(pr.author, "bllshttng");
        assert_eq!(shared, vec!["crates/fno-agents/src/backlog/entities.rs"]);
    }

    #[test]
    fn a_failed_github_read_is_exit_4_and_a_usage_error_is_exit_2() {
        // Exit 4: a real repo whose gh read fails, so the guard's refusal
        // never silently degrades to a create.
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        std::fs::write(dir.path().join("f.txt"), "base\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["checkout", "-q", "-b", "feature/guard"]);
        std::fs::write(dir.path().join("f.txt"), "changed\n").unwrap();
        git(&["add", "f.txt"]);
        git(&["commit", "-q", "-m", "change"]);
        let mut repo_args = Args {
            title: "fix: the guard subject".into(),
            body_file: "b".into(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: dir.path().to_path_buf(),
        };
        let gh = |path: &str| {
            assert!(path.contains("/pulls?state=open"));
            Err("HTTP 502".to_string())
        };
        assert_eq!(run(&repo_args, &gh), 4);
        // --check-only never creates: a clean scan exits 0 without reaching
        // the create leg, and a twin exits 3 with the same refusal.
        repo_args.check_only = true;
        repo_args.body_file = String::new();
        let clean = |path: &str| {
            assert!(path.contains("/pulls?state=open"));
            Ok("[]".to_string())
        };
        assert_eq!(run(&repo_args, &clean), 0);
        let twin = |path: &str| {
            if path.contains("/pulls?state=open") {
                Ok(json_array(vec![pr_json(
                    9,
                    "fix: the guard subject",
                    "other",
                    "someone",
                )]))
            } else {
                assert!(path.contains("/pulls/9/files"));
                Ok(json_array(vec![file_json("f.txt")]))
            }
        };
        assert_eq!(run(&repo_args, &twin), 3);
        // Exit 2: a non-repo cwd fails the branch read before any gh call.
        let a = Args {
            title: "t".into(),
            body_file: "b".into(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: PathBuf::from("/"),
        };
        let gh = |_: &str| Err("HTTP 502".to_string());
        // `/` has no git repo: the branch read fails first (exit 2).
        assert_eq!(run(&a, &gh), 2);
        assert!(parse_args(&args(&["--nope"])).is_err());
        assert!(parse_args(&args(&["--title", "t"])).is_err());
        // --check-only drops the --body-file requirement.
        let only = parse_args(&args(&["--check-only", "--title", "t"])).unwrap();
        assert!(only.check_only);
        assert!(parse_args(&args(&[
            "--title",
            "t",
            "--body-file",
            "b",
            "--not-duplicate",
            "x",
        ]))
        .is_err());
        // The happy parse maps every flag, repeats --not-duplicate, and
        // expands a bare base to origin/<base>.
        let a = parse_args(&args(&[
            "--title",
            "t",
            "--body-file",
            "b",
            "--base",
            "release/9",
            "--not-duplicate",
            "7",
            "--not-duplicate",
            "8",
        ]))
        .unwrap();
        assert_eq!(a.base, "release/9");
        assert_eq!(a.not_duplicates, vec![7, 8]);
        assert_eq!(crate::pr_body_check::base_ref("release/9"), "release/9");
        assert_eq!(crate::pr_body_check::base_ref("main"), "origin/main");
        // The owners gate shares this file's exit-code contract, so its
        // refusal and pass paths run under the same declaration.
        checks_a_code_pr_that_audits_no_owner_test_file_refuses_and_names_it();
    }

    fn checks_a_code_pr_that_audits_no_owner_test_file_refuses_and_names_it() {
        let dir = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let ok = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        let src = dir.path().join("crates/x/src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn base_case() {}\n}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "base"]);
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["checkout", "-q", "-b", "feature/owners"]);
        std::fs::write(
            src.join("lib.rs"),
            "pub fn f() {}\npub fn g() {}\n\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn base_case() {}\n}\n",
        )
        .unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "extend"]);
        let body = dir.path().join("body.md");
        std::fs::write(&body, "## Summary\n\nadds g\n").unwrap();
        let mut a = Args {
            title: "feat: extend the owner surface".into(),
            body_file: body.to_string_lossy().into_owned(),
            base: "main".into(),
            not_duplicates: vec![],
            check_only: false,
            cwd: dir.path().to_path_buf(),
        };
        // The scan passes (no open PRs); the owners gate then refuses and
        // the create leg is never reached.
        let gh = |path: &str| -> Result<String, String> {
            assert!(path.contains("/pulls?state=open"));
            Ok("[]".to_string())
        };
        assert_eq!(run(&a, &gh), 5);
        // The authored line satisfies the gate; --check-only then returns
        // clean without ever reaching gh pr create.
        std::fs::write(
            &body,
            "x\nAudited owners: crates/x/src/lib.rs (no cut: kept, distinct contracts)\n",
        )
        .unwrap();
        a.check_only = true;
        assert_eq!(run(&a, &gh), 0);
    }

    fn epic_rows() -> Vec<Value> {
        vec![
            serde_json::json!({"id": "x-eeee", "parent": ""}),
            serde_json::json!({"id": "x-aaaa", "parent": "x-eeee"}),
            serde_json::json!({"id": "x-bbbb", "parent": "x-eeee"}),
            serde_json::json!({"id": "x-cccc", "parent": "x-bbbb", "contained_in": "x-bbbb"}),
            serde_json::json!({"id": "x-dddd", "parent": "x-eeee", "contained_in": "x-aaaa"}),
        ]
    }

    fn sibling_pr(number: i64, body: &str, merged_at: Option<&str>) -> SiblingPr {
        SiblingPr {
            number,
            title: "feat: the sibling".into(),
            branch: format!("feature/{number}"),
            body: body.into(),
            merged_at: merged_at.map(|t| {
                chrono::DateTime::parse_from_rfc3339(t)
                    .unwrap()
                    .with_timezone(&chrono::Utc)
            }),
        }
    }

    fn at(t: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(t)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn an_open_sibling_pr_of_the_epic_refuses() {
        let now = at("2026-10-24T12:00:00Z");
        let rows = epic_rows();
        // A contained descendant of the sibling counts as the sibling's.
        let open = [sibling_pr(40, "Fixes x-bbbb x-cccc", None)];
        let refusal = sibling_refusal("x-aaaa", &rows, "## Summary\n", "feature/me", &open, now)
            .expect("an open sibling PR refuses");
        assert!(
            refusal.contains("PR #40 (open) already ships x-bbbb"),
            "{refusal}"
        );
        assert!(refusal.contains("epic x-eeee"), "{refusal}");
        assert!(refusal.contains("Separate PR: <reason>"), "{refusal}");
        let contained = [sibling_pr(41, "Fixes x-cccc", None)];
        assert!(sibling_refusal("x-aaaa", &rows, "", "feature/me", &contained, now).is_some());
        // This node's own PR, and a node it contains, never refuse it.
        let mine = [sibling_pr(42, "Fixes x-bbbb", None)];
        assert!(sibling_refusal("x-aaaa", &rows, "", "feature/42", &mine, now).is_none());
        let own_leaf = [sibling_pr(43, "Fixes x-dddd", None)];
        assert!(sibling_refusal("x-aaaa", &rows, "", "feature/me", &own_leaf, now).is_none());
        // No parent, or no sibling on any PR: silent pass.
        assert!(sibling_refusal("x-eeee", &rows, "", "feature/me", &open, now).is_none());
        let unrelated = [sibling_pr(44, "Fixes x-1111", None)];
        assert!(sibling_refusal("x-aaaa", &rows, "", "feature/me", &unrelated, now).is_none());
        // Two decompose group children are each a declared PR: neither
        // refuses the other, but a plain sibling still refuses a group.
        let mut grouped = rows.clone();
        grouped[1]["group_slug"] = serde_json::json!("1");
        grouped[2]["group_slug"] = serde_json::json!("2");
        assert!(sibling_refusal("x-aaaa", &grouped, "", "feature/me", &open, now).is_none());
        grouped.push(serde_json::json!({"id": "x-2222", "parent": "x-eeee"}));
        let leaf = [sibling_pr(47, "Fixes x-2222", None)];
        assert!(sibling_refusal("x-aaaa", &grouped, "", "feature/me", &leaf, now).is_some());
    }

    #[test]
    fn a_separate_pr_line_or_an_old_merge_lets_the_sibling_gate_pass() {
        let now = at("2026-10-24T12:00:00Z");
        let rows = epic_rows();
        let open = [sibling_pr(40, "Fixes x-bbbb", None)];
        let body = "## Summary\n\nSeparate PR: the schema change must land first\n";
        assert!(sibling_refusal("x-aaaa", &rows, body, "feature/me", &open, now).is_none());
        // A bare label with no reason is not an escape.
        assert!(
            sibling_refusal("x-aaaa", &rows, "Separate PR:", "feature/me", &open, now).is_some()
        );
        let recent = [sibling_pr(45, "Fixes x-bbbb", Some("2026-10-24T11:00:00Z"))];
        let refusal = sibling_refusal("x-aaaa", &rows, "", "feature/me", &recent, now).unwrap();
        assert!(
            refusal.contains("merged in the last two hours"),
            "{refusal}"
        );
        let old = [sibling_pr(46, "Fixes x-bbbb", Some("2026-10-24T09:30:00Z"))];
        assert!(sibling_refusal("x-aaaa", &rows, "", "feature/me", &old, now).is_none());
    }

    // ── fixtures ────────────────────────────────────────────────────────────

    fn pr_json(number: i64, title: &str, branch: &str, author: &str) -> Value {
        serde_json::json!({
            "number": number,
            "title": title,
            "head": {"ref": branch},
            "user": {"login": author},
        })
    }

    fn file_json(name: &str) -> Value {
        serde_json::json!({"filename": name})
    }

    fn json_array(items: Vec<Value>) -> String {
        Value::Array(items).to_string()
    }
}
