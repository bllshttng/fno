//! `evals-trend --mode stability`: the stability eval's first slice, native
//! under d-b6cc1a2a (all new code lands in crates). It rides the existing
//! evals-trend verb rather than growing the over-cap fno-agents root, so
//! the menu-caps lint sees no new action:
//! `fno-agents evals-trend --mode stability` computes, from one repo's git
//! history plus the backlog graph, the fix-of-fix rate and the product-path
//! vs fleet-machinery surface split for a window of merged PRs.
//!
//! Definitions the report owns (they travel with every output):
//!
//! - **Merged PRs**: first-parent commits whose subject delivers a PR, in
//!   either shape this repo produces: squash (`... (#123)`) or merge
//!   (`Merge pull request #123 from owner/branch`). A repeated delivery
//!   keeps its newest landing.
//! - **Kind ladder** (first hit wins): the conventional subject
//!   (`fix(scope): ...`; for merge deliveries the PR title on the first
//!   body line, which this repo's merge commits carry), then the backlog
//!   node kind joined through the node id in the branch leaf
//!   (`bugfix/x-aaaa-crash` carries `x-aaaa`), then the branch prefix
//!   (`bugfix|fix|hotfix`, `feature|feat`), then other. The backlog read is
//!   read-only and optional: an absent graph degrades to the git-only
//!   ladder and says so; an explicit `--graph` that fails to read is an
//!   error, not an opt-out.
//! - **Surface split**: per-file first-match prefix globs (product = the
//!   plugin surface a user installs and exercises; fleet = the runtime and
//!   CI that runs the agent fleet). A PR lands on the side holding more of
//!   its changed lines; ties and unmatched paths read fleet.
//! - **Fix-of-fix**: a fix-kind PR whose diff removes at least one line
//!   whose introducing commit is a PR delivery merged within
//!   `--fix-window-days` (default 7) before the fix itself. Introduced-at
//!   comes from `git blame --first-parent` on the fix's first parent, so a
//!   blame sha is always a mainline delivery commit. The rate's denominator
//!   is the measured fix PRs only: a fix whose diff or blame read failed is
//!   `unmeasured`, never clean; fix PRs with no removed lines are counted
//!   and reported apart.
//!
//! Output is a human summary, or the full JSON (per-PR rows included) under
//! `--json`. The 30-day baseline this shipped with is in the PR body.

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_ENV: i32 = 3;

const USAGE: &str = "usage: fno-agents evals-trend --mode stability [--repo <path>] [--days <30>] \
[--fix-window-days <7>] [--graph <path> | --no-backlog] [--json] [--now <rfc3339>]";

/// Product = what a plugin user installs and exercises: the CLI, the skill
/// and command surfaces, hooks, and their tests. Fleet = the runtime and CI
/// that runs the agent fleet. Unmatched paths read as fleet (conservative:
/// unknown churn never flatters the product surface).
const PRODUCT_GLOBS: &[&str] = &[
    "cli/src/fno/",
    "skills/",
    "commands/",
    "agents/",
    "hooks/",
    ".claude-plugin/",
    "cli/tests/",
    "tests/",
];
// Fleet globs document the intended split; every unmatched path (including
// the harness state dirs) reads fleet without naming them here, because the
// placement lint bars fresh state-dir path strings in crates source.
const FLEET_GLOBS: &[&str] = &["crates/", "scripts/", "docs/", ".github/"];

struct Args {
    repo: PathBuf,
    days: i64,
    fix_window_days: i64,
    graph: Option<PathBuf>,
    graph_explicit: bool,
    now: Option<DateTime<Utc>>,
    json: bool,
}

/// One merged PR delivery. `kind` is the report's three buckets;
/// `classified_by` names the ladder rung that decided it.
#[derive(Clone)]
struct Delivery {
    pr: u64,
    sha: String,
    time: DateTime<Utc>,
    kind: &'static str,
    classified_by: &'static str,
    subject: String,
}

/// One output row, one merged PR.
struct Row {
    delivery: Delivery,
    surface: &'static str,
    changed_lines: u64,
    /// None for non-fix rows and unmeasured fix rows; Some for measured ones.
    fix_of_fix: Option<bool>,
    young_lines: u64,
    /// Removed lines the fix diff carries; 0 means an add-only fix.
    removed_lines: u64,
}

struct Report {
    rows: Vec<Row>,
    backlog_path: PathBuf,
    blame_runs_failed: u64,
    diff_failures: u64,
}

pub fn run(args: &[String]) -> i32 {
    let parsed = match parse_args(args) {
        Ok(Some(parsed)) => parsed,
        Ok(None) => return EXIT_OK,
        Err(()) => {
            eprintln!("{USAGE}");
            return EXIT_USAGE;
        }
    };
    if !parsed.repo.is_dir() {
        eprintln!(
            "stability-report: no such directory: {}",
            parsed.repo.display()
        );
        return EXIT_ENV;
    }
    let now = parsed.now.unwrap_or_else(Utc::now);
    let node_kinds = match &parsed.graph {
        Some(path) => match load_node_kinds(path) {
            Ok(kinds) => kinds,
            Err(error) => {
                if parsed.graph_explicit {
                    // An explicit --graph that cannot read is an error, not
                    // an opt-out: the operator asked for the join.
                    eprintln!("stability-report: graph read failed: {error}");
                    return EXIT_ENV;
                }
                eprintln!(
                    "stability-report: default graph unreadable, degrading to the git-only ladder: {error}"
                );
                BTreeMap::new()
            }
        },
        None => BTreeMap::new(),
    };
    // The join is "on" only when nodes actually loaded: an empty graph
    // degrades to the git-only ladder either way.
    let backlog_loaded = !node_kinds.is_empty();
    let report = match compute(&parsed, now, &node_kinds) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("stability-report: {error}");
            return EXIT_ENV;
        }
    };
    if parsed.json {
        println!("{}", render_json(&parsed, now, &report, backlog_loaded));
    } else {
        print!("{}", render_text(&parsed, now, &report, backlog_loaded));
    }
    EXIT_OK
}

fn parse_args(args: &[String]) -> Result<Option<Args>, ()> {
    let mut parsed = Args {
        repo: PathBuf::from("."),
        days: 30,
        fix_window_days: 7,
        graph: default_graph_path(),
        graph_explicit: false,
        now: None,
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(None);
            }
            "--repo" => {
                i += 1;
                parsed.repo = PathBuf::from(args.get(i).ok_or(())?);
            }
            "--days" => {
                i += 1;
                parsed.days = args.get(i).ok_or(())?.parse().map_err(|_| ())?;
            }
            "--fix-window-days" => {
                i += 1;
                parsed.fix_window_days = args.get(i).ok_or(())?.parse().map_err(|_| ())?;
            }
            "--graph" => {
                i += 1;
                parsed.graph = Some(PathBuf::from(args.get(i).ok_or(())?));
                parsed.graph_explicit = true;
            }
            "--no-backlog" => {
                parsed.graph = None;
                parsed.graph_explicit = false;
            }
            "--now" => {
                i += 1;
                let raw = args.get(i).ok_or(())?;
                parsed.now = Some(
                    DateTime::parse_from_rfc3339(raw)
                        .map_err(|_| ())?
                        .with_timezone(&Utc),
                );
            }
            "--json" => parsed.json = true,
            _ => return Err(()),
        }
        i += 1;
    }
    if parsed.days <= 0 || parsed.fix_window_days < 0 {
        return Err(());
    }
    Ok(Some(parsed))
}

/// The operator's graph store, resolved the same way the backlog porcelain
/// resolves it. A missing file is fine: the caller degrades to git-only.
fn default_graph_path() -> Option<PathBuf> {
    let path = crate::backlog::settings::graph_path();
    path.is_file().then_some(path)
}

/// id -> kind for every node in the graph. The `kind` column is nullable:
/// NULL-kind rows skip the join. A read failure is an error, so the caller
/// can tell a refused read from an opt-out.
fn load_node_kinds(path: &std::path::Path) -> Result<BTreeMap<String, String>, String> {
    let mut kinds = BTreeMap::new();
    let connection = crate::store_conn::open_read(path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut statement = connection
        .prepare("SELECT id, kind FROM nodes")
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|error| format!("{}: {error}", path.display()))?;
    for row in rows.flatten() {
        if let Some(kind) = row.1 {
            kinds.insert(row.0, kind);
        }
    }
    Ok(kinds)
}

/// The node id embedded in a branch name, validated against the graph so a
/// branch word like `id-cannot` cannot masquerade: `bugfix/x-aaaa-slug`
/// carries `x-aaaa`. The prefix segments are stripped first, so the split
/// never sees a `/` inside a token.
fn branch_node_kind<'a>(branch: &str, node_kinds: &'a BTreeMap<String, String>) -> Option<&'a str> {
    let leaf = branch.rsplit('/').next().unwrap_or(branch);
    let tokens: Vec<&str> = leaf.split('-').collect();
    for window in tokens.windows(2) {
        let head = window[0];
        let tail = window[1];
        let alpha = !head.is_empty() && head.chars().all(|c| c.is_ascii_lowercase());
        let hex = !tail.is_empty() && tail.chars().all(|c| c.is_ascii_hexdigit());
        if alpha && hex {
            let id = format!("{head}-{tail}");
            if let Some(kind) = node_kinds.get(&id) {
                return Some(kind.as_str());
            }
        }
    }
    None
}

fn node_kind_bucket(kind: &str) -> &'static str {
    match kind {
        "bug" => "fix",
        "feature" => "feat",
        _ => "other",
    }
}

fn conventional_kind(subject: &str) -> Option<&'static str> {
    let head = subject.split(':').next().unwrap_or(subject);
    let kind = head.split('(').next().unwrap_or(head);
    match kind.trim() {
        "fix" | "bugfix" | "hotfix" => Some("fix"),
        "feat" | "feature" => Some("feat"),
        _ => None,
    }
}

fn branch_prefix_kind(branch: &str) -> Option<&'static str> {
    let head = branch.split('/').next().unwrap_or(branch);
    match head {
        "bugfix" | "fix" | "hotfix" => Some("fix"),
        "feature" | "feat" => Some("feat"),
        _ => None,
    }
}

/// `(#123)` at the end of a squash subject, or the number in a GitHub merge
/// subject. Returns (pr, branch-or-none).
fn delivery_pr(subject: &str) -> Option<(u64, Option<String>)> {
    if let Some(rest) = subject.strip_prefix("Merge pull request #") {
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        let pr: u64 = digits.parse().ok()?;
        let rest = rest.strip_prefix(digits.as_str())?;
        let branch = rest
            .strip_prefix(" from ")?
            .split_once('/')
            .map(|(_, branch)| branch.to_string());
        return Some((pr, branch));
    }
    let inner = subject.strip_suffix(')')?;
    let open = inner.rfind('(')?;
    let spec = &inner[open + 1..];
    let digits = spec.strip_prefix('#')?;
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((digits.parse().ok()?, None))
}

/// The PR title a merge-delivered PR carries: the first non-empty body line
/// that is not the subject, not a `* ` commit-list bullet, and not another
/// merge header. This repo's merge commits put the title there.
fn merge_title<'a>(body: &'a str, subject: &str) -> Option<&'a str> {
    let mut lines = body.lines().filter(|line| !line.trim().is_empty());
    while let Some(line) = lines.next() {
        if line.trim() == subject.trim() {
            continue;
        }
        if line.trim_start().starts_with("* ") {
            continue;
        }
        if line.trim().starts_with("Merge pull request") {
            continue;
        }
        return Some(line.trim());
    }
    None
}

fn classify(
    subject: &str,
    title: Option<&str>,
    branch: Option<&str>,
    node_kinds: &BTreeMap<String, String>,
) -> (&'static str, &'static str) {
    if let Some(kind) = conventional_kind(subject) {
        return (kind, "subject");
    }
    if let Some(title) = title {
        if let Some(kind) = conventional_kind(title) {
            return (kind, "merge-title");
        }
    }
    if let Some(branch) = branch {
        if let Some(kind) = branch_node_kind(branch, node_kinds) {
            return (node_kind_bucket(kind), "node-kind");
        }
    }
    if let Some(branch) = branch {
        if let Some(kind) = branch_prefix_kind(branch) {
            return (kind, "branch-prefix");
        }
    }
    ("other", "unknown")
}

fn git_out(repo: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|error| format!("git {}: {error}", args.first().unwrap_or(&"")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        Err(format!(
            "git {} failed: {}",
            args.first().unwrap_or(&""),
            stderr.trim()
        ))
    }
}

fn compute(
    parsed: &Args,
    now: DateTime<Utc>,
    node_kinds: &BTreeMap<String, String>,
) -> Result<Report, String> {
    let since = now - Duration::days(parsed.days);
    // Blame targets reach back before the window by the youth span, so a
    // young introducer is always inside the delivery map.
    let map_since = since - Duration::days(parsed.fix_window_days.max(1));
    let since_iso = map_since.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let log = git_out(
        &parsed.repo,
        &[
            "log",
            "--first-parent",
            &format!("--since={since_iso}"),
            "--format=%H%x1f%cI%x1f%P%x1f%s%x1f%b%x1e",
        ],
    )?;

    let mut deliveries: Vec<Delivery> = Vec::new();
    let mut by_sha: BTreeMap<String, (u64, DateTime<Utc>)> = BTreeMap::new();
    for record in log.split('\u{1e}') {
        let record = record.trim_start();
        if record.is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.split('\u{1f}').collect();
        let (sha, iso, subject, body) = match fields.as_slice() {
            [sha, iso, _parents, subject, body] => (*sha, *iso, *subject, *body),
            _ => continue,
        };
        let time = match DateTime::parse_from_rfc3339(iso) {
            Ok(time) => time.with_timezone(&Utc),
            Err(_) => continue,
        };
        let Some((pr, branch)) = delivery_pr(subject) else {
            continue;
        };
        if deliveries.iter().any(|d| d.pr == pr) {
            // git log walks newest-first, so the delivery already held is
            // the newest one; keep it.
            continue;
        }
        // The PR title rung is merge-delivery only: a squash's own body
        // lines are commit prose, not a PR title.
        let title = if branch.is_some() {
            merge_title(body, subject)
        } else {
            None
        };
        let (kind, classified_by) = classify(subject, title, branch.as_deref(), node_kinds);
        by_sha.insert(sha.to_string(), (pr, time));
        deliveries.push(Delivery {
            pr,
            sha: sha.to_string(),
            time,
            kind,
            classified_by,
            subject: subject.to_string(),
        });
    }
    deliveries.sort_by_key(|d| d.time);

    let mut report = Report {
        rows: Vec::new(),
        backlog_path: parsed.graph.clone().unwrap_or_default(),
        blame_runs_failed: 0,
        diff_failures: 0,
    };
    for delivery in &deliveries {
        if delivery.time < since {
            continue;
        }
        let parent = format!("{}^1", delivery.sha);
        let (surface, changed_lines) = match surface_split(&parsed.repo, &parent, &delivery.sha) {
            Ok(split) => split,
            Err(_) => {
                report.diff_failures += 1;
                // A failed numstat read is unmeasured, never a fleet score.
                ("unmeasured", 0)
            }
        };
        let (fix_of_fix, young_lines, removed_lines) = if delivery.kind == "fix" {
            match fix_of_fix(
                &parsed.repo,
                &parent,
                delivery,
                &by_sha,
                parsed.fix_window_days,
                &mut report.blame_runs_failed,
            ) {
                Ok(verdict) => verdict,
                Err(_) => {
                    report.diff_failures += 1;
                    (None, 0, 0)
                }
            }
        } else {
            (None, 0, 0)
        };
        report.rows.push(Row {
            delivery: delivery.clone(),
            surface,
            changed_lines,
            fix_of_fix,
            young_lines,
            removed_lines,
        });
    }
    Ok(report)
}

/// (side, changed lines) for one PR: per-file first-match globs, the PR sits
/// with the side holding more changed lines, ties read fleet.
fn surface_split(
    repo: &std::path::Path,
    parent: &str,
    sha: &str,
) -> Result<(&'static str, u64), String> {
    let numstat = git_out(repo, &["diff", "--no-renames", "--numstat", parent, sha])?;
    let mut product: u64 = 0;
    let mut fleet: u64 = 0;
    for line in numstat.lines() {
        let mut parts = line.splitn(3, '\t');
        let (Some(add), Some(del), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        let lines = add.parse::<u64>().unwrap_or(0) + del.parse::<u64>().unwrap_or(0);
        if PRODUCT_GLOBS.iter().any(|glob| path.starts_with(glob)) {
            product += lines;
        } else {
            fleet += lines; // fleet globs plus every unmatched path
        }
    }
    let side = if product > fleet { "product" } else { "fleet" };
    Ok((side, product + fleet))
}

/// The fix-of-fix verdict for one fix PR: any removed line whose introducing
/// mainline commit is a PR delivery merged within the youth window. Returns
/// (verdict, young lines, removed lines); the verdict is None (unmeasured)
/// when the diff read fails or any blame run fails - a partial blame read
/// must never read as clean.
fn fix_of_fix(
    repo: &std::path::Path,
    parent: &str,
    delivery: &Delivery,
    by_sha: &BTreeMap<String, (u64, DateTime<Utc>)>,
    fix_window_days: i64,
    blame_failures: &mut u64,
) -> Result<(Option<bool>, u64, u64), String> {
    let diff = git_out(repo, &["diff", "--unified=0", parent, &delivery.sha])?;
    let removed = removed_runs(&diff);
    let removed_lines: u64 = removed
        .iter()
        .map(|(_, runs)| runs.iter().map(|(start, end)| end - start + 1).sum::<u64>())
        .sum();
    let mut young_lines: u64 = 0;
    let mut failures: u64 = 0;
    for (path, runs) in &removed {
        for (start, end) in runs {
            let range = format!("{start},{end}");
            let blame = match git_out(
                repo,
                &[
                    "blame",
                    "-l",
                    "--first-parent",
                    "--root",
                    "-L",
                    &range,
                    parent,
                    "--",
                    path,
                ],
            ) {
                Ok(blame) => blame,
                Err(_) => {
                    failures += 1;
                    *blame_failures += 1;
                    continue;
                }
            };
            for line in blame.lines() {
                let sha = line.trim().split_whitespace().next().unwrap_or("");
                if sha.is_empty() || sha.starts_with('^') {
                    continue; // boundary: older than the whole map
                }
                let Some((intro_pr, intro_time)) = by_sha.get(sha) else {
                    continue; // a non-PR delivery: direct push, not a fix-of-fix
                };
                if intro_pr == &delivery.pr {
                    continue;
                }
                let age = delivery.time - *intro_time;
                if age > Duration::zero() && age <= Duration::days(fix_window_days) {
                    young_lines += 1;
                }
            }
        }
    }
    // A fix with no removed lines is measured clean and reported apart; a
    // fix whose blame partially failed is unmeasured, never clean.
    if removed.is_empty() {
        return Ok((Some(false), 0, 0));
    }
    if failures > 0 {
        return Ok((None, young_lines, removed_lines));
    }
    Ok((Some(young_lines > 0), young_lines, removed_lines))
}

/// Removed-line runs per pre-image path, from a unified diff. Removed lines
/// live in the `a/` path (the fix may rename the file). Hunks of one file
/// share the path, so runs are grouped once, at the section switch.
fn removed_runs(diff: &str) -> Vec<(String, Vec<(u64, u64)>)> {
    let mut out: Vec<(String, Vec<(u64, u64)>)> = Vec::new();
    let mut current: Option<String> = None;
    let mut removed: Vec<u64> = Vec::new();
    let mut old_ln: u64 = 0;
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("--- a/") {
            push_runs(&mut out, current.take(), &mut removed);
            current = Some(path.to_string());
        } else if line.starts_with("diff --git ")
            || line.starts_with("--- ")
            || line.starts_with("+++ ")
            || line.starts_with("index ")
        {
            continue;
        } else if let Some(range) = line.strip_prefix("@@ -") {
            let start = range
                .split(',')
                .next()
                .and_then(|n| n.parse::<u64>().ok())
                .unwrap_or(0);
            old_ln = start;
        } else if line.starts_with('-') {
            if old_ln > 0 {
                removed.push(old_ln);
            }
            old_ln += 1;
        } else if line.starts_with('+') || line.starts_with('\\') {
            // added lines and "\ No newline" markers do not advance the old side
        } else {
            old_ln += 1; // context
        }
    }
    push_runs(&mut out, current.take(), &mut removed);
    out
}

/// Group one file's removed line numbers into contiguous runs and park them.
fn push_runs(
    out: &mut Vec<(String, Vec<(u64, u64)>)>,
    path: Option<String>,
    removed: &mut Vec<u64>,
) {
    let Some(path) = path else {
        removed.clear();
        return;
    };
    removed.sort_unstable();
    removed.dedup();
    let mut runs: Vec<(u64, u64)> = Vec::new();
    for line in removed.drain(..) {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == line => *end = line,
            _ => runs.push((line, line)),
        }
    }
    if !runs.is_empty() {
        out.push((path, runs));
    }
}

struct Totals {
    total: u64,
    fix: u64,
    feat: u64,
    other: u64,
    product_prs: u64,
    product_lines: u64,
    fleet_prs: u64,
    fleet_lines: u64,
    /// PRs whose numstat read failed: neither product nor fleet.
    surface_unmeasured: u64,
    fix_of_fix: u64,
    /// Fix PRs blame/diff-measured; the rate's denominator.
    measured: u64,
    /// Fix PRs left out of the rate: diff or blame read failed.
    unmeasured: u64,
    /// Measured-clean fix PRs whose diff removed no lines.
    no_removed: u64,
}

fn aggregate(report: &Report) -> Totals {
    let mut t = Totals {
        total: 0,
        fix: 0,
        feat: 0,
        other: 0,
        product_prs: 0,
        product_lines: 0,
        fleet_prs: 0,
        fleet_lines: 0,
        surface_unmeasured: 0,
        fix_of_fix: 0,
        measured: 0,
        unmeasured: 0,
        no_removed: 0,
    };
    for row in &report.rows {
        t.total += 1;
        match row.delivery.kind {
            "fix" => {
                t.fix += 1;
                match row.fix_of_fix {
                    Some(true) => {
                        t.measured += 1;
                        t.fix_of_fix += 1;
                        if row.removed_lines == 0 {
                            t.no_removed += 1;
                        }
                    }
                    Some(false) => {
                        t.measured += 1;
                        if row.removed_lines == 0 {
                            t.no_removed += 1;
                        }
                    }
                    None => t.unmeasured += 1,
                }
            }
            "feat" => t.feat += 1,
            _ => t.other += 1,
        }
        match row.surface {
            "product" => {
                t.product_prs += 1;
                t.product_lines += row.changed_lines;
            }
            "fleet" => {
                t.fleet_prs += 1;
                t.fleet_lines += row.changed_lines;
            }
            _ => t.surface_unmeasured += 1,
        }
    }
    t
}

fn repo_label(parsed: &Args) -> String {
    std::fs::canonicalize(&parsed.repo)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| parsed.repo.display().to_string())
}

fn render_text(parsed: &Args, now: DateTime<Utc>, report: &Report, backlog: bool) -> String {
    let stats = aggregate(report);
    let since = now - Duration::days(parsed.days);
    let mut out = String::new();
    out.push_str(&format!("stability report: {}\n", repo_label(parsed)));
    out.push_str(&format!(
        "window: {}d ({} .. {})   fix youth window: {}d\n",
        parsed.days,
        since.format("%Y-%m-%d"),
        now.format("%Y-%m-%d"),
        parsed.fix_window_days
    ));
    out.push_str(&format!(
        "merged PRs: {}  fix {}  feat {}  other {}\n",
        stats.total, stats.fix, stats.feat, stats.other
    ));
    out.push_str(&format!(
        "surface: product {} PRs / {} lines   fleet {} PRs / {} lines   unmeasured {}\n",
        stats.product_prs,
        stats.product_lines,
        stats.fleet_prs,
        stats.fleet_lines,
        stats.surface_unmeasured
    ));
    if stats.fix > 0 {
        if stats.measured > 0 {
            out.push_str(&format!(
                "fix-of-fix: {}/{} measured = {:.1}%   unmeasured {}   no-removed-lines {}\n",
                stats.fix_of_fix,
                stats.measured,
                100.0 * stats.fix_of_fix as f64 / stats.measured as f64,
                stats.unmeasured,
                stats.no_removed
            ));
        } else {
            out.push_str(&format!(
                "fix-of-fix: n/a ({} fix PRs, none measured)   no-removed-lines {}\n",
                stats.fix, stats.no_removed
            ));
        }
    } else {
        out.push_str("fix-of-fix: no fix PRs in window\n");
    }
    out.push_str(&format!(
        "backlog join: {}   blame runs failed: {}   diff failures: {}\n",
        if backlog { "on" } else { "off" },
        report.blame_runs_failed,
        report.diff_failures
    ));
    out
}

fn render_json(parsed: &Args, now: DateTime<Utc>, report: &Report, backlog: bool) -> String {
    let stats = aggregate(report);
    let rate = if stats.measured > 0 {
        json!(stats.fix_of_fix as f64 / stats.measured as f64)
    } else {
        json!(null)
    };
    let rows: Vec<Value> = report
        .rows
        .iter()
        .map(|row| {
            json!({
                "pr": row.delivery.pr,
                "merged_at": row.delivery.time.to_rfc3339(),
                "kind": row.delivery.kind,
                "classified_by": row.delivery.classified_by,
                "surface": row.surface,
                "changed_lines": row.changed_lines,
                "fix_of_fix": row.fix_of_fix,
                "young_lines": row.young_lines,
                "removed_lines": row.removed_lines,
                "subject": row.delivery.subject,
            })
        })
        .collect();
    json!({
        "repo": repo_label(parsed),
        "days": parsed.days,
        "fix_window_days": parsed.fix_window_days,
        "window_since": (now - Duration::days(parsed.days)).to_rfc3339(),
        "generated_at": now.to_rfc3339(),
        "backlog": {
            "available": backlog,
            "graph": report.backlog_path.display().to_string(),
        },
        "totals": {
            "merged": stats.total,
            "fix": stats.fix,
            "feat": stats.feat,
            "other": stats.other,
        },
        "surface": {
            "product": {"prs": stats.product_prs, "changed_lines": stats.product_lines},
            "fleet": {"prs": stats.fleet_prs, "changed_lines": stats.fleet_lines},
            "unmeasured": stats.surface_unmeasured,
        },
        "fix_of_fix": {
            "fix_prs": stats.fix,
            "measured": stats.measured,
            "of_fix": stats.fix_of_fix,
            "unmeasured": stats.unmeasured,
            "no_removed_lines": stats.no_removed,
            "rate": rate,
            "window_days": parsed.fix_window_days,
        },
        "blame_runs_failed": report.blame_runs_failed,
        "diff_failures": report.diff_failures,
        "globs": {"product": PRODUCT_GLOBS, "fleet": FLEET_GLOBS},
        "rows": rows,
    })
    .to_string()
}

#[cfg(test)]
mod tests;
