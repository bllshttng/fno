//! The reviewer lane's second GitHub identity: post a review verdict
//! to GitHub as `config.review.bot_identity` so a clean pass can carry APPROVE
//! and `reviewDecision` a real value. GitHub refuses an approving review from
//! the PR's author - the API accepts the call and silently records COMMENTED -
//! and one account authors every PR here, so the producer is the identity, not
//! the review pipeline.
//!
//! This is the port of the Python producer that previously lived at
//! `cli/src/fno/pr/_publish_review.py` (the file-budget gate asked for the
//! port; Python keeps the two doors and nothing else). One JSON payload on
//! stdin, one JSON answer on stdout, same contract as `spawn-axes`. The emit
//! chokepoint (`fno event emit -t review_attestation`) and the hidden
//! `fno do pr publish-review` verb are the two doors; the payload is the same
//! shape for both, with the verb door passing empty `verdict` to have the
//! default resolved from the newest head-pinned attestation for HEAD.
//!
//! Fail-closed by construction, in this order: config missing -> skipped;
//! token missing -> skipped; identity collision with the PR author -> refused;
//! stale head pin -> refused; unmappable verdict -> refused. Config is read
//! BEFORE any gh call, so an unconfigured install makes zero network calls per
//! attestation emit. Only after the gate chain does the POST fire, and the
//! answer carries the `reviewDecision` GitHub reports back rather than
//! trusting the POST receipt - a receipt saying "posted APPROVE" while GitHub
//! recorded COMMENTED is the exact lie this module exists to remove.
//!
//! Each gh call is unbounded here; the Python transport (`verb_call`) bounds
//! the whole verb, so a hung network stack costs one timeout and a
//! `skipped (mirror error ...)` receipt, never a wedged emit.
//!
//! `publish_round` is the second, identity-independent leg: it posts a
//! formal `pulls/{n}/reviews` review under the CALLER's own gh auth, always
//! - zero findings included - so a `review.bot_identity`-unconfigured
//! install (this project's own default: `posture = "self_review"` with no
//! bot identity) still produces a visible GitHub review for every attested
//! round. `event=COMMENT` is what makes an author-authored post land
//! correctly: GitHub only silently demotes an *approving* review from the
//! PR's author, never a comment-type one.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One `gh` invocation. `token` set means GH_TOKEN=token in the child env
/// ONLY: the caller's own gh auth must survive the call unchanged, and the
/// token must not leak into any other subprocess this process later spawns.
pub trait Gh {
    fn run(&self, args: &[String], token: Option<&str>, cwd: &Path) -> GhOut;
}

pub struct GhOut {
    pub ok: bool,
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub struct GhReal;

impl Gh for GhReal {
    fn run(&self, args: &[String], token: Option<&str>, cwd: &Path) -> GhOut {
        let mut cmd = Command::new("gh");
        cmd.args(args).current_dir(cwd);
        if let Some(t) = token {
            cmd.env("GH_TOKEN", t);
        }
        match crate::gh_budget::output(cmd) {
            Ok(out) => GhOut {
                ok: out.status.success(),
                code: out.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            },
            Err(e) => GhOut {
                ok: false,
                code: -1,
                stdout: String::new(),
                stderr: format!("gh spawn failed: {e}"),
            },
        }
    }
}

/// The full answer for one publish attempt. `status`:
/// - `posted`  - the POST succeeded and `review_decision` carries what GitHub
///               read back (never assumed from the POST alone)
/// - `skipped` - not configured / no token / no open PR / dry-run; posting was
///               not the right action, nothing is wrong
/// - `refused` - posting would manufacture a false verdict (identity
///               collision, stale head pin, unmappable verdict)
/// - `failed`  - gh errored or is missing; `stderr` carries the detail
pub struct Answer {
    pub status: &'static str,
    pub reason: String,
    pub event: Option<&'static str>,
    pub review_decision: Option<String>,
    pub stderr: Option<String>,
    pub receipt: String,
}

fn receipt(status: &str, reason: &str) -> String {
    // The posted line carries the event, identity, and readback inline (the
    // plan's documented shape); every other status wraps its reason.
    if status == "posted" {
        format!("bot-review: {reason}")
    } else {
        format!("bot-review: {status} ({reason})")
    }
}

impl Answer {
    fn done(status: &'static str, reason: impl Into<String>) -> Self {
        let reason = reason.into();
        Answer {
            status,
            receipt: receipt(status, &reason),
            reason,
            event: None,
            review_decision: None,
            stderr: None,
        }
    }

    /// The verb door's process exit: 0 posted or dry-run-skipped, 2 the
    /// no-attestation default miss, 1 everything else.
    pub fn exit(&self) -> i32 {
        match self.status {
            "posted" => 0,
            "skipped" if self.reason.starts_with("no head-pinned attestation") => 2,
            "skipped" if self.reason.starts_with("dry-run:") => 0,
            _ => 1,
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "reason": self.reason,
            "event": self.event,
            "review_decision": self.review_decision,
            "stderr": self.stderr,
            "receipt": self.receipt,
            "exit": self.exit(),
        })
    }
}

fn answer_with_event(mut a: Answer, event: &'static str) -> Answer {
    a.event = Some(event);
    a
}

/// Drop a trailing `[bot]` for comparison (GitHub appends it to app logins).
/// `str::get` keeps a multibyte login from panicking on a non-boundary slice.
fn strip_bot(login: &str) -> &str {
    match login.get(login.len().saturating_sub(5)..) {
        Some(tail) if tail.eq_ignore_ascii_case("[bot]") => &login[..login.len() - 5],
        _ => login,
    }
}

/// owner/repo parsed from a PR url (https://github.com/owner/repo/pull/N).
/// Anchored on the `pull` segment rather than fixed indices so a host with a
/// subpath still resolves; a non-PR url reads as "" (skip) not garbage.
fn slug_from_url(url: &str) -> String {
    let segs: Vec<&str> = url.split('/').filter(|s| !s.is_empty()).collect();
    if let Some(i) = segs.iter().position(|s| *s == "pull") {
        if i >= 2 && segs.len() > i {
            return format!("{}/{}", segs[i - 2], segs[i - 1]);
        }
    }
    String::new()
}

fn verdict_event(verdict: &str) -> Option<&'static str> {
    match verdict {
        "pass" => Some("APPROVE"),
        "fail" => Some("REQUEST_CHANGES"),
        _ => None,
    }
}

/// The two `[review]` keys, parsed from one config.toml's text. Absent keys
/// read None so the caller can layer project over global per key.
fn review_keys_from(text: &str) -> (Option<String>, Option<String>) {
    let parsed: Value = match toml::from_str(text) {
        Ok(v) => v,
        Err(_) => return (None, None),
    };
    let review = parsed.get("review");
    let read = |key: &str| {
        review
            .and_then(|r| r.get(key))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    (read("bot_identity"), read("bot_token_env"))
}

fn home_config() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))?;
    Some(Path::new(&home).join(".fno").join("config.toml"))
}

/// `bot_identity` and `bot_token_env`, resolved through the same worktree ->
/// canonical -> global candidate chain the config layer uses. A publish that
/// runs inside a linked feature worktree must find keys that live only in the
/// canonical checkout's project config, or the lane reports unconfigured and
/// silently never posts from exactly the checkouts all the work happens in.
fn review_config(cwd: &Path) -> (Option<String>, Option<String>) {
    let mut roots: Vec<PathBuf> = vec![crate::paths::worktree_repo_root(cwd)];
    if let Some(canonical) = crate::paths::canonical_repo_root(cwd) {
        roots.push(canonical);
    }
    let mut candidates: Vec<Option<PathBuf>> = roots
        .into_iter()
        .map(|root| Some(root.join(".fno").join("config.toml")))
        .collect();
    candidates.push(home_config());
    let mut merged = (None, None);
    for path in candidates {
        let Some(path) = path else { continue };
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let (identity, token_env) = review_keys_from(&text);
        if merged.0.is_none() {
            merged.0 = identity;
        }
        if merged.1.is_none() {
            merged.1 = token_env;
        }
    }
    merged
}

/// The newest `review_attestation` whose head_sha equals `head`, last-wins on
/// ts (a same-second re-emit is the newer verdict). Pure over the journal
/// paths: missing or malformed rows read as absent, never an error.
fn newest_head_attestation(journals: &[PathBuf], head: &str) -> Option<Value> {
    let mut newest: Option<(String, Value)> = None;
    for path in journals {
        // SQL authority: committed rows in commit order; the import pulls any
        // journal bytes a pre-cutover writer (or fixture) left behind.
        let Ok(text) =
            crate::loopcheck::event_lines_of(path, &["review_attestation"]).map(|l| l.join("\n"))
        else {
            continue;
        };
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if row.get("type").and_then(Value::as_str) != Some("review_attestation") {
                continue;
            }
            let Some(data) = row.get("data").filter(|d| d.is_object()) else {
                continue;
            };
            if data.get("head_sha").and_then(Value::as_str) != Some(head) {
                continue;
            }
            let ts = row
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let take = match &newest {
                Some((best_ts, _)) => ts >= *best_ts,
                None => true,
            };
            if take {
                newest = Some((ts, data.clone()));
            }
        }
    }
    newest.map(|(_, data)| data)
}

/// The newest `review_attestation` naming `branch`, last-wins on ts. The
/// held-findings store IS this row: a no-PR review (`--comment` with no PR)
/// already writes it keyed to branch and head, so the PR-open leg reads it
/// back instead of a second store. Rows predating the `branch` field carry no
/// branch and cannot be scoped to one PR, so they never match. Also read by
/// `merge_gates::review_posted_gate`, which must ask about the SAME round
/// this function would post, head-carried or not - an exact-head lookup
/// would go silent on a round law d-608344c1 still carries forward after a
/// rebase.
pub(crate) fn newest_branch_attestation(journals: &[PathBuf], branch: &str) -> Option<Value> {
    let mut newest: Option<(String, Value)> = None;
    for path in journals {
        let Ok(text) =
            crate::loopcheck::event_lines_of(path, &["review_attestation"]).map(|l| l.join("\n"))
        else {
            continue;
        };
        for line in text.lines() {
            let Ok(row) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if row.get("type").and_then(Value::as_str) != Some("review_attestation") {
                continue;
            }
            let Some(data) = row.get("data").filter(|d| d.is_object()) else {
                continue;
            };
            if data.get("branch").and_then(Value::as_str) != Some(branch) {
                continue;
            }
            let ts = row
                .get("ts")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let take = match &newest {
                Some((best_ts, _)) => ts >= *best_ts,
                None => true,
            };
            if take {
                newest = Some((ts, data.clone()));
            }
        }
    }
    newest.map(|(_, data)| data)
}

/// The one marker a round's review ends with; idempotency keys on it. Also
/// read by `merge_gates::review_posted_gate` to decide whether an attested
/// round at a given head has landed as a GitHub review yet.
pub(crate) fn round_marker(branch: &str, reviewed_head: &str) -> String {
    format!("<!-- fno-review-round branch={branch} head={reviewed_head} -->")
}

/// The review body: a one-line summary (level, round, finding count, verdict,
/// head) so a zero-finding clean pass still posts proof of itself, then one
/// bullet per finding (`file:line [category] summary`) when any exist, the
/// moved-head caveat when the PR advanced past the reviewed commit, and the
/// marker last.
fn round_comment_body(
    branch: &str,
    reviewed_head: &str,
    pr_head: &str,
    level: &str,
    round_label: &str,
    verdict: &str,
    findings: &[Value],
) -> String {
    let short_head: String = reviewed_head.chars().take(8).collect();
    let mut body = format!(
        "fno review: level={level} round={round_label} findings={} verdict={verdict} head={short_head}\n",
        findings.len()
    );
    if reviewed_head != pr_head {
        body.push_str(&format!(
            "\nreviewed at {reviewed_head}; PR head is {pr_head}; this review does not cover the newer commits\n"
        ));
    }
    for f in findings {
        let key = f.get("finding_key").and_then(Value::as_str).unwrap_or("");
        let category = f.get("category").and_then(Value::as_str).unwrap_or("");
        let suffix = format!(":{category}");
        let file_line = match key.strip_suffix(&suffix) {
            Some(head) => head,
            None => key,
        };
        let summary = f.get("summary").and_then(Value::as_str).unwrap_or("");
        body.push_str(&format!("\n- `{file_line}` [{category}] {summary}"));
    }
    body.push_str(&format!("\n\n{}", round_marker(branch, reviewed_head)));
    body
}

/// The round-review answer for one PR-open post attempt. Independent of the
/// verdict mirror: a lane with no bot identity still owes the author proof
/// the round happened, zero findings or not. `status`: posted | stale-posted
/// | already-posted | skipped | failed. `journals` is injected so callers
/// (and tests) pin the store.
pub struct RoundAnswer {
    pub status: &'static str,
    pub reviewed_head: String,
    pub pr_head: String,
    pub reason: String,
    pub findings: Vec<Value>,
    /// How many findings got their own anchored inline comment, best-effort,
    /// never blocking the review post above. None when there was nothing to
    /// anchor (zero findings, or the review post itself never landed).
    pub inline_posted: Option<usize>,
}

impl RoundAnswer {
    fn done(
        status: &'static str,
        reason: impl Into<String>,
        reviewed_head: &str,
        pr_head: &str,
    ) -> Self {
        RoundAnswer {
            status,
            reviewed_head: reviewed_head.to_string(),
            pr_head: pr_head.to_string(),
            reason: reason.into(),
            findings: Vec::new(),
            inline_posted: None,
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "status": self.status,
            "reviewed_head": self.reviewed_head,
            "pr_head": self.pr_head,
            "reason": self.reason,
            "findings": self.findings,
            "inline_posted": self.inline_posted,
        })
    }
}

/// `path`, 1-based `line` parsed from a `finding_key` of the shape
/// `path:line:category` (the shape every emitter writes - see
/// `round_comment_body`'s own strip above). `None` on anything that does
/// not parse; the caller treats that finding as text-only.
fn finding_location(key: &str, category: &str) -> Option<(String, u64)> {
    let suffix = format!(":{category}");
    let file_line = key.strip_suffix(&suffix).unwrap_or(key);
    let (path, line) = file_line.rsplit_once(':')?;
    let line: u64 = line.parse().ok()?;
    Some((path.to_string(), line))
}

/// Best-effort: one `pulls/{n}/comments` POST per finding that has a
/// parseable `path:line`, anchoring it to the diff. A single finding's POST
/// failing (the line sits outside the diff hunk, a stale head, ...) is
/// swallowed and counted, never retried, and never undoes the review post
/// that already landed - the formal review object is the thing this gate
/// exists for; inline anchoring is a bonus on top of it.
fn post_inline_comments(
    gh: &dyn Gh,
    cwd: &Path,
    slug: &str,
    number: u64,
    commit_id: &str,
    findings: &[Value],
) -> usize {
    let mut posted = 0usize;
    for f in findings {
        let key = f.get("finding_key").and_then(Value::as_str).unwrap_or("");
        let category = f.get("category").and_then(Value::as_str).unwrap_or("");
        let Some((path, line)) = finding_location(key, category) else {
            continue;
        };
        let summary = f.get("summary").and_then(Value::as_str).unwrap_or("");
        let body = format!("[{category}] {summary}");
        let post: Vec<String> = [
            "api",
            "-X",
            "POST",
            &format!("/repos/{slug}/pulls/{number}/comments"),
            "-f",
            &format!("body={body}"),
            "-f",
            &format!("commit_id={commit_id}"),
            "-f",
            &format!("path={path}"),
            "-F",
            &format!("line={line}"),
            "-f",
            "side=RIGHT",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if gh.run(&post, None, cwd).ok {
            posted += 1;
        }
    }
    posted
}

/// Read the PR's newest review round for its branch and post it as ONE
/// formal `pulls/{n}/reviews` review under the caller's own gh auth - a
/// zero-finding clean pass included. Idempotent by the marker: a review
/// already carrying this branch and reviewed head posts nothing. A reviewed
/// head behind the PR head still posts (post, never drop), naming both
/// shas; coverage stays head-pinned, so the newer commits still owe their
/// own round. `event=COMMENT` rides the caller's own identity even when that
/// identity is the PR's author: GitHub silently demotes only an *approving*
/// review from the author (see this module's doc comment), it does not
/// refuse a COMMENT-type one - which is what lets this post land with no
/// `review.bot_identity` configured at all.
pub fn publish_round(payload: &Value, gh: &dyn Gh, journals: &[PathBuf]) -> RoundAnswer {
    let cwd = Path::new(payload.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let pr_number = payload.get("pr_number").and_then(Value::as_u64);

    // One gh read: the PR to post on, its head, its branch, its slug.
    let mut view: Vec<String> = vec!["pr".to_string(), "view".to_string()];
    if let Some(n) = pr_number {
        view.push(n.to_string());
    }
    view.extend(
        ["--json", "number,headRefOid,headRefName,url"]
            .iter()
            .map(|s| s.to_string()),
    );
    let out = gh.run(&view, None, cwd);
    let fields: Value = if out.ok {
        serde_json::from_str(out.stdout.trim()).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if fields.is_null() {
        return RoundAnswer::done("skipped", "could not read PR", "", "");
    }
    let number = fields.get("number").and_then(Value::as_u64);
    let Some(number) = number else {
        return RoundAnswer::done("skipped", "could not read PR number", "", "");
    };
    let pr_head = fields
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let branch = fields
        .get("headRefName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if pr_head.is_empty() || branch.is_empty() {
        return RoundAnswer::done("skipped", "could not read PR head or branch", "", "");
    }
    let slug = slug_from_url(fields.get("url").and_then(Value::as_str).unwrap_or(""));
    if slug.is_empty() {
        return RoundAnswer::done("skipped", "could not resolve repo slug", "", &pr_head);
    }

    // The round record: the newest attestation for this branch, any head.
    // No findings-empty skip here - a clean pass is the common case this
    // gate exists to stop dropping.
    let Some(att) = newest_branch_attestation(journals, &branch) else {
        return RoundAnswer::done(
            "skipped",
            format!("no review attestation for branch {branch}"),
            "",
            &pr_head,
        );
    };
    let reviewed_head = att
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let findings: Vec<Value> = att
        .get("findings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let level = att
        .get("level")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("unset")
        .to_string();
    let round_label = att
        .get("review_round")
        .and_then(Value::as_i64)
        .map(|n| n.to_string())
        .unwrap_or_else(|| "unscoped".to_string());
    let verdict = att
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    // Idempotency: scan existing REVIEWS (not issue comments - this leg now
    // posts a formal review object) for this exact marker. --paginate walks
    // every page, so a busy PR cannot push its marker past the scan.
    let list: Vec<String> = [
        "api",
        "--paginate",
        &format!("/repos/{slug}/pulls/{number}/reviews?per_page=100"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = gh.run(&list, None, cwd);
    if out.ok && out.stdout.contains(&round_marker(&branch, &reviewed_head)) {
        return RoundAnswer::done(
            "already-posted",
            format!("round for {reviewed_head} already posted on #{number}"),
            &reviewed_head,
            &pr_head,
        )
        .with_findings(findings);
    }

    let body = round_comment_body(
        &branch,
        &reviewed_head,
        &pr_head,
        &level,
        &round_label,
        &verdict,
        &findings,
    );
    let post: Vec<String> = [
        "api",
        "-X",
        "POST",
        &format!("/repos/{slug}/pulls/{number}/reviews"),
        "-f",
        "event=COMMENT",
        "-f",
        &format!("commit_id={reviewed_head}"),
        "-f",
        &format!("body={body}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = gh.run(&post, None, cwd);
    if !out.ok {
        let mut a = RoundAnswer::done(
            "failed",
            format!("gh api POST review failed (exit {})", out.code),
            &reviewed_head,
            &pr_head,
        );
        a.findings = findings;
        a.reason = format!("{}: {}", a.reason, out.stderr.trim());
        return a;
    }
    let status = if reviewed_head == pr_head {
        "posted"
    } else {
        "stale-posted"
    };
    // Best-effort, after the review already landed: never let an inline
    // anchoring failure undo or mask the review post above.
    let inline_posted = if findings.is_empty() {
        None
    } else {
        Some(post_inline_comments(
            gh,
            cwd,
            &slug,
            number,
            &reviewed_head,
            &findings,
        ))
    };
    let mut answer = RoundAnswer::done(
        status,
        format!("round posted on #{number}"),
        &reviewed_head,
        &pr_head,
    )
    .with_findings(findings);
    answer.inline_posted = inline_posted;
    answer
}

impl RoundAnswer {
    fn with_findings(mut self, findings: Vec<Value>) -> Self {
        self.findings = findings;
        self
    }
}

fn git_head(cwd: &Path) -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() && !s.is_empty() {
        Some(s)
    } else {
        None
    }
}

/// The gate chain + POST, shared by both doors. Never panics on payload
/// shape: a malformed field reads as its empty default and lands in the same
/// refusals a real value would.
pub fn publish(payload: &Value, gh: &dyn Gh, env: &dyn Fn(&str) -> Option<String>) -> Answer {
    let cwd = Path::new(payload.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let dry_run = payload
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pr_number = payload.get("pr_number").and_then(Value::as_u64);

    // Default resolution (the verb door): verdict/head/reviewer come from the
    // newest head-pinned attestation for HEAD, never from memory.
    let mut verdict = payload
        .get("verdict")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut head_sha = payload
        .get("head_sha")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let mut reviewer = payload
        .get("reviewer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if verdict.is_empty() {
        let head = if head_sha.is_empty() {
            match git_head(cwd) {
                Some(h) => h,
                None => {
                    return Answer::done("skipped", "could not read git HEAD");
                }
            }
        } else {
            head_sha.clone()
        };
        // One resolver, one read: events_path subsumes the legacy checkout
        // journal by migrating it on first resolve, so no hand-built path
        // may sit beside it (the state-roots lint refuses exactly that).
        let journals = [crate::paths::events_path(cwd)];
        match newest_head_attestation(&journals, &head) {
            Some(att) => {
                verdict = att
                    .get("verdict")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                head_sha = att
                    .get("head_sha")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                reviewer = att
                    .get("reviewer")
                    .and_then(Value::as_str)
                    .unwrap_or("manual")
                    .to_string();
            }
            None => {
                return Answer::done(
                    "skipped",
                    "no head-pinned attestation for HEAD; pass --verdict explicitly",
                );
            }
        }
    }
    if reviewer.is_empty() {
        reviewer = "manual".to_string();
    }

    // 1. Config + token. Missing either is a skip, not an error: an
    //    unconfigured lane must behave identically to today apart from the
    //    receipt line, and the config check sits BEFORE any gh call.
    // An unreadable or absent config layer reads as unconfigured (skipped),
    // never an error: a garbage config.toml must not fail the emit.
    let (identity, token_env) = review_config(cwd);
    let Some(identity) = identity else {
        return Answer::done("skipped", "review.bot_identity unset");
    };
    let Some(token_env) = token_env else {
        return Answer::done("skipped", "review.bot_token_env unset");
    };
    // A fine-grained PAT in an env var is deliberately the whole token story:
    // a GitHub App installation token (JWT signing + hourly refresh) is the
    // upgrade path when more than this one repo needs the reviewer identity.
    // The env lookup is injected so the tests stay hermetic: process env is
    // shared by every parallel test thread in the binary, and a set/remove
    // race there reads as a flaky skip.
    let token = env(&token_env).unwrap_or_default();
    if token.is_empty() {
        return Answer::done("skipped", format!("${token_env} unset or empty"));
    }

    // 2-4. One combined gh read answers the PR to post on, its author, its
    //    head, and the repo slug (parsed from the PR url).
    let mut view: Vec<String> = vec!["pr".to_string(), "view".to_string()];
    if let Some(n) = pr_number {
        view.push(n.to_string());
    }
    view.extend(
        ["--json", "number,author,headRefOid,url"]
            .iter()
            .map(|s| s.to_string()),
    );
    let out = gh.run(&view, None, cwd);
    let fields: Value = if out.ok {
        serde_json::from_str(out.stdout.trim()).unwrap_or(Value::Null)
    } else {
        Value::Null
    };
    if fields.is_null() {
        let reason = match pr_number {
            Some(n) => format!("could not read #{n}"),
            None => "no open PR for HEAD".to_string(),
        };
        return Answer::done("skipped", reason);
    }
    let resolved_number = fields.get("number").and_then(Value::as_u64);
    let number = resolved_number.or(pr_number);
    let Some(number) = number else {
        return Answer::done("skipped", "could not read PR number");
    };
    let slug = slug_from_url(fields.get("url").and_then(Value::as_str).unwrap_or(""));

    // Identity collision. This is the whole defect the node exists to fix:
    // posting as the PR author would have GitHub accept the call and record
    // COMMENTED - manufacturing the exact empty-reviewDecision lie.
    let author = fields
        .get("author")
        .and_then(|a| a.get("login"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if author.is_empty() {
        return Answer::done("skipped", format!("could not read author of #{number}"));
    }
    if strip_bot(author) == strip_bot(&identity) {
        return Answer::done(
            "refused",
            format!("bot identity {identity} is the PR author"),
        );
    }

    // Head pin. The attestation is evidence about ONE commit; approving a PR
    // whose head has moved past it approves commits nobody reviewed. A stale
    // approval is worse than no approval.
    let pr_head = fields
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("");
    if pr_head.is_empty() {
        return Answer::done("skipped", format!("could not read head sha of #{number}"));
    }
    if pr_head != head_sha {
        let short_a: String = head_sha.chars().take(8).collect();
        let short_h: String = pr_head.chars().take(8).collect();
        return Answer::done(
            "refused",
            format!("stale pin: attested {short_a} but PR head is {short_h}"),
        );
    }

    // 5. Verdict map. Anything the attestation schema does not enumerate is a
    //    verdict no GitHub event exists for; posting it as a comment would be
    //    the empty-reviewDecision shape again.
    let Some(event) = verdict_event(&verdict) else {
        return Answer::done("refused", format!("unmappable verdict {verdict:?}"));
    };

    if dry_run {
        return answer_with_event(
            Answer::done(
                "skipped",
                format!("dry-run: would post {event} as {identity}"),
            ),
            event,
        );
    }

    // 6. The POST, authenticated as the bot in the subprocess env ONLY.
    if slug.is_empty() {
        return Answer::done("skipped", "could not resolve repo slug");
    }
    let body = format!("fno review mirror: reviewer={reviewer} verdict={verdict} head={head_sha}");
    let post: Vec<String> = [
        "api",
        "-X",
        "POST",
        &format!("/repos/{slug}/pulls/{number}/reviews"),
        "-f",
        &format!("event={event}"),
        "-f",
        &format!("commit_id={head_sha}"),
        "-f",
        &format!("body={body}"),
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = gh.run(&post, Some(&token), cwd);
    if !out.ok {
        let mut a = Answer::done(
            "failed",
            format!("gh api POST reviews failed (exit {})", out.code),
        );
        a.stderr = Some(out.stderr.trim().to_string());
        return answer_with_event(a, event);
    }

    // 7. Read back what GitHub actually recorded. Load-bearing, not
    //    decoration: the readback is the only honest answer to "did it land
    //    as APPROVED?".
    let readback: Vec<String> = [
        "pr",
        "view",
        &number.to_string(),
        "--json",
        "reviewDecision",
        "--jq",
        ".reviewDecision",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let out = gh.run(&readback, None, cwd);
    let raw = out.stdout.trim();
    let decision = if out.ok && !raw.is_empty() && raw != "null" {
        Some(raw.to_string())
    } else {
        // The POST receipt is not a positive marker: an approval nobody can
        // read back (transient gh failure, or a token GitHub silently
        // demoted) must read as failed, never as a clean posted.
        None
    };
    let Some(decision) = decision else {
        let mut a = Answer::done(
            "failed",
            format!(
                "posted {event} on #{number} but the reviewDecision readback was unreadable; re-run fno do pr publish-review --pr {number}"
            ),
        );
        a.event = Some(event);
        return a;
    };
    let mut a = Answer {
        status: "posted",
        review_decision: Some(decision.clone()),
        event: Some(event),
        stderr: None,
        receipt: String::new(),
        reason: format!("posted {event} as {identity} on #{number} (reviewDecision={decision})"),
    };
    a.receipt = receipt("posted", &a.reason);
    a
}

/// The verb entry: one JSON payload on stdin, one JSON answer on stdout,
/// always exit 0 (the answer carries the caller-facing exit under "exit";
/// only a transport failure - unreadable or malformed stdin - exits 2).
/// Two independent legs ride one answer: the verdict mirror under the bot
/// identity, and the held-findings post under the caller's own auth (`held`).
pub fn run_publish_review(_args: &[String]) -> i32 {
    use std::io::Read;

    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        eprint!("publish-review: cannot read payload\n");
        return 2;
    }
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            eprint!("publish-review: bad payload: {e}\n");
            return 2;
        }
    };
    let mut answer = publish(&parsed, &GhReal, &|name: &str| std::env::var(name).ok()).to_json();
    let cwd = Path::new(parsed.get("cwd").and_then(Value::as_str).unwrap_or("."));
    let journals = [crate::paths::events_path(cwd)];
    answer["round"] = publish_round(&parsed, &GhReal, &journals).to_json();
    println!("{answer}");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Scripted gh: pops one GhOut per call and records the argv so tests can
    /// assert both answers and the exact commands (no POST may fire on a
    /// refusal path - the call log proves the absence).
    struct GhFake {
        answers: std::sync::Mutex<Vec<GhOut>>,
        calls: std::sync::Mutex<Vec<Vec<String>>>,
    }

    impl GhFake {
        fn new(answers: Vec<GhOut>) -> Self {
            GhFake {
                answers: std::sync::Mutex::new(answers),
                calls: std::sync::Mutex::new(Vec::new()),
            }
        }

        fn calls(&self) -> Vec<Vec<String>> {
            self.calls.lock().unwrap().clone()
        }

        fn gh_env(token: &str) -> String {
            format!("gh-token:{token}")
        }
    }

    impl Gh for GhFake {
        fn run(&self, args: &[String], token: Option<&str>, _cwd: &Path) -> GhOut {
            self.calls.lock().unwrap().push(args.to_vec());
            // Fail closed on an exhausted script: an unexpected call reads as
            // a failing gh, never an index panic.
            let mut answers = self.answers.lock().unwrap();
            let mut out = if answers.is_empty() {
                GhOut {
                    ok: false,
                    code: 1,
                    stdout: String::new(),
                    stderr: "gh-fake: unexpected call".to_string(),
                }
            } else {
                answers.remove(0)
            };
            drop(answers);
            if let Some(t) = token {
                // Prove the token rode the child env, not the caller's.
                out.stdout = format!("{}\n{}", GhFake::gh_env(t), out.stdout);
            }
            out
        }
    }

    fn view_answer() -> GhOut {
        GhOut {
            ok: true,
            code: 0,
            stdout: r#"{"number": 931, "author": {"login": "bllshttng"}, "headRefOid": "abc123", "url": "https://github.com/bllshttng/footnote/pull/931"}"#.to_string(),
            stderr: String::new(),
        }
    }

    fn post_ok() -> GhOut {
        GhOut {
            ok: true,
            code: 0,
            stdout: r#"{"id": 1}"#.to_string(),
            stderr: String::new(),
        }
    }

    fn readback(decision: &str) -> GhOut {
        GhOut {
            ok: true,
            code: 0,
            stdout: format!("{decision}\n"),
            stderr: String::new(),
        }
    }

    fn write_config(dir: &Path, identity: &str, token_env: &str) {
        let dir = dir.join(".fno");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("config.toml"),
            format!("[review]\nbot_identity = \"{identity}\"\nbot_token_env = \"{token_env}\"\n"),
        )
        .unwrap();
    }

    /// Config paths derive from `cwd`; each test gets its own temp dir so the
    /// suite runs in parallel. HOME is not consulted: the project layer
    /// carries both keys.
    fn temp_repo(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fno-publish-review-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn scan_rows() {
        let dir = temp_repo("scan");
        let journal = dir.join("events.jsonl");
        std::fs::write(
            &journal,
            concat!(
                "{\"type\":\"review_attestation\",\"ts\":\"2026-09-11T10:00:00Z\",\"data\":{\"head_sha\":\"abc\",\"verdict\":\"fail\",\"reviewer\":\"r1\"}}\n",
                "{\"type\":\"other\",\"ts\":\"2026-09-11T11:00:00Z\",\"data\":{\"head_sha\":\"abc\"}}\n",
                "not json\n",
                "{\"type\":\"review_attestation\",\"ts\":\"2026-09-11T12:00:00Z\",\"data\":{\"head_sha\":\"other\",\"verdict\":\"pass\"}}\n",
                "{\"type\":\"review_attestation\",\"ts\":\"2026-09-11T13:00:00Z\",\"data\":{\"head_sha\":\"abc\",\"verdict\":\"pass\",\"reviewer\":\"r2\"}}\n",
            ),
        )
        .unwrap();
        let hit = newest_head_attestation(&[journal.clone()], "abc").unwrap();
        assert_eq!(hit.get("verdict").and_then(Value::as_str), Some("pass"));
        assert_eq!(hit.get("reviewer").and_then(Value::as_str), Some("r2"));
        assert!(newest_head_attestation(&[journal], "nope").is_none());
        assert!(newest_head_attestation(&[dir.join("absent.jsonl")], "abc").is_none());

        // A temp-dir cwd carries no project config; the test also assumes the
        // running environment's global config layer names no bot keys (true
        // on CI and on any machine that has not configured the lane).
        let dir = temp_repo("unconfigured");
        let fake = GhFake::new(vec![]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| None,
        );
        assert_eq!(answer.status, "skipped");
        assert!(answer.reason.contains("bot_identity"));
        // The config check sits BEFORE any gh call: the fake never ran.
        assert!(fake.calls().is_empty());

        let dir = temp_repo("no-token");
        write_config(&dir, "fno-review-bot", "FNO_PUBLISH_REVIEW_TEST_UNSET");
        let fake = GhFake::new(vec![]);
        // Hermetic: the token lookup is injected, so an unset name is a None
        // without touching the process env.
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| None,
        );
        assert_eq!(answer.status, "skipped");
        assert!(answer.reason.contains("FNO_PUBLISH_REVIEW_TEST_UNSET"));
        assert!(fake.calls().is_empty());

        let dir = temp_repo("posted");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer(), post_ok(), readback("APPROVED")]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "local_attestation", "cwd": dir.to_string_lossy(),
                    "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "posted");
        assert_eq!(answer.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(
            answer.receipt,
            "bot-review: posted APPROVE as fno-review-bot on #931 (reviewDecision=APPROVED)"
        );
        let calls = fake.calls();
        assert_eq!(calls.len(), 3);
        assert!(calls[1].iter().any(|a| a.contains("event=APPROVE")));
        assert!(calls[1].iter().any(|a| a.contains("commit_id=abc123")));
        assert!(calls[2].contains(&".reviewDecision".to_string()));

        let dir = temp_repo("fail");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![
            view_answer(),
            post_ok(),
            readback("CHANGES_REQUESTED"),
        ]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "fail",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "posted");
        assert_eq!(answer.event, Some("REQUEST_CHANGES"));
        assert_eq!(answer.review_decision.as_deref(), Some("CHANGES_REQUESTED"));
        assert!(answer
            .receipt
            .ends_with("(reviewDecision=CHANGES_REQUESTED)"));

        // The POST receipt is not a positive marker: a verdict nobody can
        // read back must read as failed, never as a clean posted with an
        // empty decision. This is the exact "token GitHub silently demoted"
        // shape - the POST lands, the approval does not exist.
        let dir = temp_repo("unreadable-readback");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer(), post_ok(), readback("")]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "failed");
        assert_eq!(answer.review_decision, None);
        assert!(answer.reason.contains("readback was unreadable"));
        assert!(answer.reason.contains("re-run fno do pr publish-review"));
        assert_eq!(answer.exit(), 1);

        // A real linked worktree: keys live ONLY in the canonical checkout's
        // project config; a publish running from the worktree must still see
        // them, or the lane reads unconfigured where all the work happens.
        let base = temp_repo("wt-base");
        write_config(&base, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let setups: &[&[&str]] = &[
            &["init", "-q", "-b", "main"],
            &["config", "user.email", "t@t"],
            &["config", "user.name", "t"],
        ];
        for args in setups {
            let status = std::process::Command::new("git")
                .args(*args)
                .current_dir(&base)
                .status()
                .unwrap();
            assert!(status.success());
        }
        std::fs::write(base.join("f.txt"), "x").unwrap();
        let commits: &[&[&str]] = &[&["add", "f.txt"], &["commit", "-qm", "x"]];
        for args in commits {
            let status = std::process::Command::new("git")
                .args(*args)
                .current_dir(&base)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let wt = temp_repo("wt-linked");
        let out = std::process::Command::new("git")
            .args(["-C", base.to_str().unwrap(), "worktree", "add", "-q"])
            .arg(&wt)
            .status()
            .unwrap();
        assert!(out.success());
        let fake = GhFake::new(vec![]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": wt.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| None,
        );
        // The canonical leg answered: the env-var name came through the
        // config chain, and the missing token then skipped before any gh call.
        assert_eq!(answer.status, "skipped");
        assert!(answer.reason.contains("GH_REVIEW_BOT_TOKEN unset or empty"));
        assert!(fake.calls().is_empty());
        let _ = std::fs::remove_dir_all(&wt);
    }

    #[test]
    fn refuse_rows() {
        let dir = temp_repo("collision");
        write_config(&dir, "bllshttng", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer()]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "refused");
        assert!(answer.reason.contains("is the PR author"));
        assert_eq!(fake.calls().len(), 1);

        let dir = temp_repo("stale");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer()]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "deadbeef", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "refused");
        assert!(answer.reason.contains("stale pin"));
        assert!(answer.reason.contains("attested deadbeef"));
        assert!(answer.reason.contains("PR head is abc123"));
        assert_eq!(fake.calls().len(), 1);

        let dir = temp_repo("bad-verdict");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer()]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "comment",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "refused");
        assert!(answer.reason.contains("unmappable verdict"));
        assert_eq!(fake.calls().len(), 1);

        let dir = temp_repo("dry");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![view_answer()]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": true}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "skipped");
        assert_eq!(answer.event, Some("APPROVE"));
        assert!(answer.reason.starts_with("dry-run: would post APPROVE"));
        assert_eq!(answer.exit(), 0);
        assert_eq!(fake.calls().len(), 1);

        let dir = temp_repo("post-fail");
        write_config(&dir, "fno-review-bot", "GH_REVIEW_BOT_TOKEN");
        let fake = GhFake::new(vec![
            view_answer(),
            GhOut {
                ok: false,
                code: 1,
                stdout: String::new(),
                stderr: "HTTP 403: Not Allowed".to_string(),
            },
        ]);
        let answer = publish(
            &json!({"pr_number": 931, "head_sha": "abc123", "verdict": "pass",
                    "reviewer": "r", "cwd": dir.to_string_lossy(), "dry_run": false}),
            &fake,
            &|_| Some("tok-123".to_string()),
        );
        assert_eq!(answer.status, "failed");
        assert_eq!(answer.stderr.as_deref(), Some("HTTP 403: Not Allowed"));
        assert_eq!(answer.exit(), 1);

        let posted = Answer::done(
            "posted",
            "posted APPROVE as b on #1 (reviewDecision=APPROVED)",
        );
        assert_eq!(posted.exit(), 0);
        let dry = Answer::done("skipped", "dry-run: would post APPROVE as b");
        assert_eq!(dry.exit(), 0);
        let no_att = Answer::done(
            "skipped",
            "no head-pinned attestation for HEAD; pass --verdict explicitly",
        );
        assert_eq!(no_att.exit(), 2);
        let refused = Answer::done("refused", "bot identity b is the PR author");
        assert_eq!(refused.exit(), 1);
    }
}
