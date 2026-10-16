//! The post-merge ritual (`fno do pr ritual`), ported from
//! `cli/src/fno/pr/_ritual.py`. One idempotent verb runs the mechanical legs
//! in order and prints one receipt line per leg:
//!
//!     step=<name> status=<ok|skipped|failed|deferred> detail=<...>
//!
//! exit 1 when any leg failed. Legs that shell another family's verb go
//! through ONE helper (`run_leg` / `stream_leg`) over `crate::scrape::fno_bin()`
//! - the seam-crossing site the baseline names. Facts the ritual reads live
//! in Rust already: config, claims, the cleanup mint, the origin slug, the
//! graph scan.

use serde_json::{json, Value};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Per-leg default bound so a single hung verb cannot wedge the whole ritual.
const LEG_TIMEOUT_SECS: u64 = 120;
/// The advance leg: bounded + streamed; killing mid-dispatch is safe (advance's
/// reservations are TTL claims that survive the kill, so a re-run dedups).
const ADVANCE_TIMEOUT_SECS: u64 = 180;
/// Headless judgment worker; matched by the outer subprocess bound.
const JUDGMENT_TIMEOUT_SECS: u64 = 600;
/// The reconcile leg runs above reconcile's close-probe budget, same as the
/// merge verb (`POST_MERGE_RECONCILE_TIMEOUT_S` in cli/src/fno/pr/_merge.py).
const RECONCILE_TIMEOUT_SECS: u64 = 300;
/// The mutex TTL: 15m bounds a run that finishes in 1-3 min; the TTL is the
/// crash backstop.
const CLAIM_TTL: &str = "15m";

const OK: &str = "ok";
const SKIPPED: &str = "skipped";
const FAILED: &str = "failed";
/// Distinct from SKIPPED: the work is still owed, and a later sweep does it.
const DEFERRED: &str = "deferred";

/// One receipt line, 200-char detail cap kept from the Python `Receipt`.
fn receipt_line(step: &str, status: &str, detail: &str) -> String {
    let flat: String = detail.replace('\n', " ").trim().to_string();
    let d: String = if flat.chars().count() > 200 {
        let head: String = flat.chars().take(197).collect();
        format!("{head}...")
    } else {
        flat
    };
    format!("step={step} status={status} detail={d}")
}

/// `git rev-parse` text, empty on any failure (the ported `_git_text`).
fn git_text(argv: &[&str], cwd: &Path) -> String {
    let out = Command::new("git").args(argv).current_dir(cwd).output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// The CANONICAL checkout, never the worktree this may run from: a lane
/// worktree can carry a per-worktree `parking_lot_path` override, and the
/// durable parking-lot write must land where archive-worktree.sh cannot
/// delete it. `--git-common-dir` points at the shared `.git` whose parent IS
/// canonical; it may be relative, so resolve before taking the parent.
pub(crate) fn canonical_root(cwd: &Path) -> Option<PathBuf> {
    let gcd = git_text(&["rev-parse", "--git-common-dir"], cwd);
    if !gcd.is_empty() {
        let p = PathBuf::from(&gcd);
        let p = if p.is_absolute() { p } else { cwd.join(p) };
        if let Ok(resolved) = p.canonicalize() {
            if resolved.exists() {
                return resolved.parent().map(Path::to_path_buf);
            }
        }
        if p.exists() {
            return p.parent().map(Path::to_path_buf);
        }
    }
    let top = git_text(&["rev-parse", "--show-toplevel"], cwd);
    if top.is_empty() {
        None
    } else {
        Some(PathBuf::from(top))
    }
}

/// The checkout containing `cwd`, for worktree-local config reads.
fn worktree_root(cwd: &Path) -> PathBuf {
    let top = git_text(&["rev-parse", "--show-toplevel"], cwd);
    if top.is_empty() {
        return cwd.to_path_buf();
    }
    PathBuf::from(top)
}

/// Runner-unique, stable-per-runner mutex holder: a shared constant would read
/// as an idempotent re-acquire and silently defeat the mutex. Session id when
/// the harness exports one, else the pid.
fn session_holder() -> String {
    let mut sid = std::env::var("CLAUDE_CODE_SESSION_ID")
        .or_else(|_| std::env::var("FNO_HARNESS_SESSION_ID"))
        .unwrap_or_default();
    if sid.is_empty() {
        sid = std::process::id().to_string();
    }
    format!("postmerge:pr-holder:{sid}")
}

/// The post_merge config table: `config.post_merge` merged across the config
/// tiers, read once. Missing table = the config leg's failure case.
struct PostMergeConfig {
    enabled: bool,
    model: Option<String>,
    maintainer_marker: Option<String>,
    parking_lot_path: Option<String>,
    self_reap: bool,
    sync_command_set: bool,
}

fn truthy(v: Option<&toml::Value>) -> bool {
    match v {
        Some(toml::Value::Boolean(b)) => *b,
        Some(toml::Value::String(s)) => matches!(
            s.trim().to_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

fn truthy_json(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(Value::String(s)) => matches!(
            s.trim().to_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        _ => false,
    }
}

fn post_merge_config(canon: &Path) -> Option<PostMergeConfig> {
    let table = crate::agents_config::config_table_merged(canon, &["post_merge"])?;
    let pm = table.get("post_merge")?;
    Some(PostMergeConfig {
        enabled: match pm.get("enabled") {
            Some(v) => truthy(Some(v)),
            None => true,
        },
        model: pm
            .get("model")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        maintainer_marker: pm
            .get("maintainer_marker")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        parking_lot_path: pm
            .get("parking_lot_path")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        self_reap: truthy(pm.get("self_reap")),
        sync_command_set: pm
            .get("sync_command")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
    })
}

/// The parking lot resolves against the CANONICAL root, never a worktree cwd
/// that may carry a stale override. Defense-in-depth: the schema rejects
/// absolute / `..` paths, but a stale installed fno might not.
fn parking_lot_path(canon: Option<&Path>, pm: &PostMergeConfig) -> Option<PathBuf> {
    let rel = pm.parking_lot_path.as_deref()?;
    let canon = canon?;
    if rel.starts_with('/') || rel.starts_with('~') || rel.split('/').any(|seg| seg == "..") {
        return None;
    }
    Some(canon.join(rel))
}

/// One bounded child run: exit code, streams, and how it ended. A hang reads
/// as a timeout, never as a wedge - the Python legs' whole point.
pub(crate) struct LegRun {
    pub(crate) code: i32,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
    pub(crate) timed_out: bool,
    pub(crate) spawn_error: Option<String>,
}

impl LegRun {
    pub(crate) fn ok(&self) -> bool {
        !self.timed_out && self.spawn_error.is_none() && self.code == 0
    }
    fn last_line(&self) -> String {
        let text = if self.stdout.is_empty() {
            self.stderr.clone()
        } else {
            self.stdout.clone()
        };
        text.trim().lines().last().unwrap_or("").trim().to_string()
    }
}

/// The ONE leg-runner seam: every fno-family verb this ritual shells goes
/// through here, over `fno_bin()`. Detached legs start their own session so a
/// detached sync survives the slice kill upstream.
fn run_leg(argv: &[String], cwd: &Path, timeout: Duration, detached: bool) -> LegRun {
    let mut cmd = Command::new(crate::scrape::fno_bin());
    cmd.args(argv)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if detached {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            unsafe {
                cmd.pre_exec(|| {
                    libc::setsid();
                    Ok(())
                });
            }
        }
    }
    bounded(cmd, timeout)
}

/// The bounded-run core: spawn, drain both pipes on reader threads, wait
/// under the bound, kill at the deadline. A hang reads as `timed_out`, never
/// as a wedge - the Python legs' whole point.
pub(crate) fn bounded(mut cmd: Command, timeout: Duration) -> LegRun {
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return LegRun {
                code: 127,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: false,
                spawn_error: Some(format!("spawn-error: {e}")),
            };
        }
    };
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let out_buf = Arc::new(Mutex::new(Vec::new()));
    let err_buf = Arc::new(Mutex::new(Vec::new()));
    let spawn_reader = |pipe: Option<Box<dyn Read + Send>>, buf: Arc<Mutex<Vec<u8>>>| {
        std::thread::spawn(move || {
            let Some(mut pipe) = pipe else {
                return;
            };
            let mut chunk = [0u8; 65536];
            loop {
                match pipe.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if let Ok(mut b) = buf.lock() {
                            b.extend_from_slice(&chunk[..n]);
                        }
                    }
                }
            }
        })
    };
    let _out_thread = spawn_reader(
        out_pipe.map(|p| Box::new(p) as Box<dyn Read + Send>),
        Arc::clone(&out_buf),
    );
    let _err_thread = spawn_reader(
        err_pipe.map(|p| Box::new(p) as Box<dyn Read + Send>),
        Arc::clone(&err_buf),
    );
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let code;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Readers drain to EOF on exit; give them a beat to flush.
                std::thread::sleep(Duration::from_millis(50));
                code = status.code().unwrap_or(1);
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    code = 124;
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {
                code = 127;
                break;
            }
        }
    }
    let stdout = out_buf
        .lock()
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    let stderr = err_buf
        .lock()
        .map(|b| String::from_utf8_lossy(&b).to_string())
        .unwrap_or_default();
    LegRun {
        code,
        stdout,
        stderr,
        timed_out,
        spawn_error: None,
    }
}

// ---------------------------------------------------------------------------
// The ritual
// ---------------------------------------------------------------------------

struct Ritual {
    pr: i64,
    pr_supplied: bool,
    autonomous: bool,
    cwd: PathBuf,
    canon: Option<PathBuf>,
    project: String,
    lane_project: String,
    pm: Option<PostMergeConfig>,
    parking_lot: Option<PathBuf>,
    holder: String,
    owns_claim: bool,
    node_ids: Vec<String>,
    any_failed: bool,
    merge_state: Option<(Option<String>, Option<String>, Option<String>)>,
}

impl Ritual {
    fn new(pr: i64, autonomous: bool, cwd: &Path) -> Ritual {
        let canon = canonical_root(cwd);
        let pm = canon.as_deref().and_then(post_merge_config);
        let project = canon
            .as_deref()
            .and_then(|c| crate::agents_config::config_table_merged(c, &["project"]))
            .and_then(|t| {
                t.get("project")
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let lane_root = worktree_root(cwd);
        let lane_project = crate::agents_config::config_table_merged(&lane_root, &["project"])
            .and_then(|t| {
                t.get("project")
                    .and_then(|p| p.get("id"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let pr_supplied = pr > 0;
        let resolved = if pr_supplied {
            Some(pr)
        } else {
            resolve_most_recent_merge(canon.as_deref().unwrap_or(cwd))
        };
        let parking_lot = pm
            .as_ref()
            .and_then(|cfg| parking_lot_path(canon.as_deref(), cfg));
        Ritual {
            pr: resolved.unwrap_or(0),
            pr_supplied,
            autonomous,
            cwd: cwd.to_path_buf(),
            lane_project,
            project,
            canon,
            pm,
            parking_lot,
            holder: session_holder(),
            owns_claim: false,
            node_ids: Vec::new(),
            any_failed: false,
            merge_state: None,
        }
    }

    fn canon_path(&self) -> PathBuf {
        self.canon.clone().unwrap_or_else(|| self.cwd.clone())
    }

    fn emit(&mut self, step: &str, status: &str, detail: &str) {
        println!("{}", receipt_line(step, status, detail));
        if status == FAILED {
            self.any_failed = true;
        }
        if step == "archive" {
            let home = crate::paths::AgentsHome::from_env();
            let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "agents");
            let _ = emitter.emit_fields(
                "post_merge_archive",
                [
                    ("pr".to_string(), json!(self.pr)),
                    ("project".to_string(), json!(self.project)),
                    ("outcome".to_string(), json!(status)),
                    ("detail".to_string(), json!(detail)),
                ]
                .into_iter()
                .collect(),
            );
        }
    }

    /// One best-effort leg: map the exit to ok/failed, never raise.
    fn leg(&mut self, step: &str, argv: &[String], timeout: Duration, detached: bool) -> LegRun {
        let cwd = self.canon_path();
        let run = run_leg(argv, &cwd, timeout, detached);
        if let Some(err) = &run.spawn_error {
            self.emit(step, FAILED, err);
            return run;
        }
        if run.timed_out {
            self.emit(step, FAILED, "timeout");
            return run;
        }
        let last = run.last_line();
        if run.ok() {
            let detail = if last.is_empty() {
                format!("exit={}", run.code)
            } else {
                last
            };
            self.emit(step, OK, &detail);
        } else {
            let detail = if last.is_empty() {
                format!("exit={}", run.code)
            } else {
                format!("exit={} {last}", run.code)
            };
            self.emit(step, FAILED, &detail);
        }
        run
    }

    fn gh(&self, argv: &[&str], timeout: Duration) -> LegRun {
        let mut cmd = Command::new("gh");
        cmd.args(argv)
            .current_dir(self.canon_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        bounded(cmd, timeout)
    }

    /// (state, headRefName, mergedAt) from ONE gh call, memoized: two legs
    /// ask, and one PR view per ritual is enough. The state can only travel
    /// toward MERGED during a run, so a cached OPEN refuses more, never less.
    fn merged_state(&mut self) -> (Option<String>, Option<String>, Option<String>) {
        if self.merge_state.is_none() {
            self.merge_state = Some(self.merged_state_read());
        }
        self.merge_state.clone().unwrap()
    }

    fn merged_state_read(&self) -> (Option<String>, Option<String>, Option<String>) {
        let run = self.gh(
            &[
                "pr",
                "view",
                &self.pr.to_string(),
                "--json",
                "state,headRefName,mergedAt",
            ],
            Duration::from_secs(30),
        );
        if run.spawn_error.is_some() || !run.ok() {
            return (None, None, None);
        }
        let obj: Value = serde_json::from_str(run.stdout.trim()).unwrap_or(json!({}));
        let s = |k: &str| {
            obj.get(k)
                .and_then(Value::as_str)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        (s("state"), s("headRefName"), s("mergedAt"))
    }

    /// True when gh confirms MERGED. Fails CLOSED: an unreadable state refuses
    /// too - "I could not check" must never spend the same as "I checked and
    /// it is merged".
    fn refuse_unless_merged(&mut self, step: &str) -> bool {
        let (state, _, _) = self.merged_state();
        if state.as_deref() == Some("MERGED") {
            return true;
        }
        let detail = match state {
            Some(s) => format!("not-merged (state={s})"),
            None => "merge-state unreadable".to_string(),
        };
        self.emit(step, SKIPPED, &detail);
        false
    }
}

/// The repo's most recent merged PR, when the caller supplied no number. A
/// silent pick decides which worktree gets archived, so the receipt carries
/// where the number came from (`source=most-recent-merge`).
fn resolve_most_recent_merge(cwd: &Path) -> Option<i64> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--state",
            "merged",
            "--json",
            "number,mergedAt",
            "--limit",
            "1",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let rows: Value = serde_json::from_slice(&out.stdout).ok()?;
    let rows = rows.as_array()?;
    let last = rows.last()?;
    last.get("number").and_then(Value::as_i64)
}

// -- mutex ---------------------------------------------------------------

impl Ritual {
    /// Step 0.5: refuse if another runner owns this PR's ritual. A claims
    /// hiccup fails open (the marker is the backstop); a live foreign claim
    /// stops the run with a skipped receipt.
    fn acquire_mutex(&mut self) -> bool {
        let key = format!("reconcile:pr-{}", self.pr);
        let opts = crate::claims::AcquireOpts {
            pid: Some(std::process::id()),
            ttl_ms: Some(900_000),
            reason: Some("post-merge ritual".to_string()),
            ..Default::default()
        };
        match crate::claims::acquire(&key, &self.holder, opts) {
            crate::claims::AcquireOutcome::Acquired(_) => {
                self.owns_claim = true;
                self.emit("mutex", OK, "acquired");
                true
            }
            crate::claims::AcquireOutcome::HeldByOther { .. } => {
                self.emit("mutex", SKIPPED, "already-held by another runner");
                false
            }
            crate::claims::AcquireOutcome::Error(e) => {
                self.emit("mutex", SKIPPED, &format!("acquire-error: {e}"));
                true
            }
        }
    }

    fn release_mutex(&mut self) {
        if !self.owns_claim {
            return;
        }
        let key = format!("reconcile:pr-{}", self.pr);
        let _ = crate::claims::release(&key, &self.holder, None, None);
        self.owns_claim = false;
    }
}

// -- legs ----------------------------------------------------------------

impl Ritual {
    /// Step 2: close the merged node(s), reconcile plan status, stamp ship.
    /// `--repo` is passed explicitly when resolvable so a same-numbered PR in
    /// another repo can never be mismatched; reconcile's own scoping still
    /// fails closed on a bare-number match when this resolution comes up empty.
    fn leg_stamp(&mut self) {
        let mut argv: Vec<String> = [
            "backlog",
            "reconcile",
            "--pr-number",
            &self.pr.to_string(),
            "--json",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let repo = crate::pr_list::origin_slug(&self.canon_path(), None).ok();
        match repo {
            Some(slug) => {
                argv.push("--repo".to_string());
                argv.push(slug);
            }
            None => {
                eprintln!(
                    "warning: could not resolve this repo's slug for PR #{}; reconcile will scope to exact trailer claims only, skipping any bare-number backfill match",
                    self.pr
                );
            }
        }
        let cwd = self.canon_path();
        let r = run_leg(
            &argv,
            &cwd,
            Duration::from_secs(RECONCILE_TIMEOUT_SECS),
            true,
        );
        if let Some(err) = &r.spawn_error {
            self.emit("reconcile", FAILED, err);
            return;
        }
        if r.timed_out {
            self.emit("reconcile", FAILED, "timeout");
            return;
        }
        if !r.ok() {
            self.emit("reconcile", FAILED, &format!("exit={}", r.code));
            return;
        }
        let obj: Value = serde_json::from_str(r.stdout.trim()).unwrap_or(json!({}));
        let closed: Vec<String> = obj
            .get("closed")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|c| {
                c.get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect();
        self.node_ids.extend(closed.iter().cloned());
        let unmet_rows = obj.get("promise_unmet").cloned().unwrap_or(json!([]));
        let unknown_rows = obj.get("promise_unknown").cloned().unwrap_or(json!([]));
        let held: Vec<String> = [unmet_rows, unknown_rows]
            .iter()
            .flat_map(|rows| rows.as_array().cloned().unwrap_or_default())
            .filter_map(|h| {
                h.get("node_id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect();
        let errs = obj
            .get("contained_errors")
            .and_then(Value::as_array)
            .map(|a| a.len())
            .unwrap_or(0);
        let sync_outcome = obj
            .get("sync_catchup")
            .and_then(|s| s.get("outcome"))
            .and_then(Value::as_str);
        let closure_refused = obj.get("closure_refused");
        if obj.get("held").is_some_and(|v| truthy_json(Some(v))) {
            // A held sweep (one-in-flight gate): another reconcile was still
            // running, so this one stood down without scanning. DEFERRED keeps
            // the work visibly owed rather than reading as a no-drift run.
            let detail = format!(
                "held, a sweep was already in flight (requests={}, holder={})",
                obj.get("requests")
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                obj.get("holder").map(|v| v.to_string()).unwrap_or_default(),
            );
            self.emit("reconcile", DEFERRED, &detail);
        } else if !held.is_empty() {
            // Held open, not clean: the PR merged but the promise gate did not
            // clear the close. Unmet and unknown count apart: an unmet promise
            // owes the operator work, an unknown is a read outage.
            let ids: Vec<String> = held.iter().take(5).cloned().collect();
            let more = if held.len() > 5 {
                format!(" +{}", held.len() - 5)
            } else {
                String::new()
            };
            let detail = format!(
                "closed={} held_open={} (unmet={} unknown={}): {}{}",
                closed.len(),
                held.len(),
                unmet_rows.as_array().map(|a| a.len()).unwrap_or(0),
                unknown_rows.as_array().map(|a| a.len()).unwrap_or(0),
                ids.join(", "),
                more,
            );
            self.emit("reconcile", DEFERRED, &detail);
        } else if closure_refused.is_some_and(|v| truthy_json(Some(v))) {
            // The trailer-claimed nodes never got bound: a flaky gh query,
            // cross-repo mismatch, or unmerged PR refusal. Never silently
            // readable as "no-drift".
            self.emit(
                "reconcile",
                DEFERRED,
                &format!("closure binding refused: {closure_refused:?}"),
            );
        } else if errs > 0 || sync_outcome == Some("failed") {
            // reconcile exits 0 here (only unresolvable PRs exit 4), so the
            // leg is not failed - but cascade-close failures and a failed
            // canonical sync must not read as a clean run.
            self.emit(
                "reconcile",
                OK,
                &format!(
                    "closed={} contained-errors={} sync={}",
                    closed.len(),
                    errs,
                    sync_outcome.unwrap_or("not-run"),
                ),
            );
        } else if !closed.is_empty()
            || obj
                .get("candidates")
                .and_then(Value::as_array)
                .is_some_and(|a| !a.is_empty())
        {
            self.emit("reconcile", OK, &format!("closed={}", closed.len()));
        } else if ["contained_closed", "healed_epics", "reverted"]
            .iter()
            .any(|k| obj.get(k).is_some_and(|v| v.as_bool() == Some(true)))
            || matches!(sync_outcome, Some("synced") | Some("marked"))
        {
            // Drift was found and healed without a new close. NOT no-drift -
            // the run mutated state.
            self.emit("reconcile", OK, "healed-only");
        } else {
            self.emit("reconcile", OK, "no-drift");
        }
        // Step 2a: plan frontmatter. Idempotent.
        let argv: Vec<String> = ["do", "plan", "reconcile-status", "--apply"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        self.leg(
            "plan-reconcile",
            &argv,
            Duration::from_secs(LEG_TIMEOUT_SECS),
            false,
        );
    }

    fn leg_harvest(&mut self) {
        let mut argv: Vec<String> = ["retro", "run", "--pr-number", &self.pr.to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if self.autonomous {
            argv.push("--keep-going".to_string());
        }
        self.leg("retro", &argv, Duration::from_secs(LEG_TIMEOUT_SECS), false);
    }

    /// Step 3b: merge-triggered next dispatch, bounded + streamed. No
    /// --closed here: the reconcile call already dispatched dependents per
    /// closed record; this leg is a plain next-selection pass.
    fn leg_advance(&mut self) {
        let mut argv: Vec<String> = ["backlog", "advance", "-J", "--verbose", "--source", "ac"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if !self.lane_project.is_empty() {
            argv.push("--project".to_string());
            argv.push(self.lane_project.clone());
        }
        self.stream_leg("advance", &argv, Duration::from_secs(ADVANCE_TIMEOUT_SECS));
    }

    /// Step 3c: close the skill-diff loop.
    fn leg_skill_diff(&mut self) {
        let argv: Vec<String> = [
            "doctor",
            "skill-diff",
            "reconcile",
            "--pr-number",
            &self.pr.to_string(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        self.leg(
            "skill-diff",
            &argv,
            Duration::from_secs(LEG_TIMEOUT_SECS),
            false,
        );
    }

    /// Step 3d: the guard chain lives in the native verb (sync_canonical.rs).
    fn leg_sync_canonical(&mut self) {
        let Some(pm) = &self.pm else {
            return;
        };
        if !pm.sync_command_set {
            self.emit("sync-canonical", SKIPPED, "not configured");
            return;
        }
        // detached: a sync in its own session survives the slice kill upstream.
        let argv: Vec<String> = [
            "do",
            "pr",
            "sync-canonical",
            "--pr-number",
            &self.pr.to_string(),
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        self.leg("sync-canonical", &argv, Duration::from_secs(900), true);
    }

    // -- archive -------------------------------------------------------------

    /// `owner/repo` for this checkout: git remote first, then gh fallback.
    /// git remote needs no network, so an offline run still scopes the scan.
    fn resolve_origin_slug(&self) -> Option<String> {
        let cwd = self.canon_path();
        if let Ok(slug) = crate::pr_list::origin_slug(&cwd, None) {
            return Some(slug);
        }
        let run = self.gh(
            &[
                "repo",
                "view",
                "--json",
                "nameWithOwner",
                "-q",
                ".nameWithOwner",
            ],
            Duration::from_secs(30),
        );
        if !run.ok() {
            return None;
        }
        let s = run.stdout.trim().to_lowercase();
        let parts: Vec<&str> = s.split('/').collect();
        if parts.len() == 2 && parts.iter().all(|p| !p.is_empty()) {
            return Some(s);
        }
        None
    }

    /// Sidecar-derived node id(s) for this PR when reconcile closed nothing:
    /// the dominant path closes + stamps the node at the ship gate, so
    /// `.closed[]` is empty and the scan recovers repo-scoped.
    fn recover_node_for_pr(&self) -> Vec<String> {
        let home = crate::paths::AgentsHome::from_env();
        let slug = self.resolve_origin_slug();
        crate::merge_reap::pr_node_ids(&home, self.pr, slug.as_deref())
    }

    /// Persist one merge-triggered cleanup request before deferring: the
    /// shared helper `fno do pr merge` also mints, one fold key for both.
    fn register_cleanup_request(&mut self, branch: &str, why: &str) -> String {
        if self.node_ids.is_empty() {
            // Recover BEFORE the envelope keys on it: the reap-rows leg runs
            // the same recovery several legs later, too late for this mint.
            self.node_ids = self.recover_node_for_pr();
        }
        let home = crate::paths::AgentsHome::from_env();
        let node_ids: Vec<String> = self.node_ids.iter().map(String::clone).collect();
        let merged_at = self.merged_state().2;
        let request_id = crate::merge_reap::emit_merge_cleanup_requested(
            &home,
            &self.canon_path().display().to_string(),
            &self.project,
            self.pr,
            branch,
            None,
            &node_ids,
            None,
            None,
            merged_at.as_deref(),
            None,
            None,
        );
        // Minting makes the next idle tick the request's first payment window.
        let stamp = home.root().join("worktree-sweep.stamp");
        let _ = std::fs::remove_file(stamp);
        format!("cleanup-requested request_id={request_id}; {why}")
    }

    /// Step 4: mint the daemon cleanup request after a confirmed merge.
    fn leg_archive(&mut self) {
        let (state, branch, _) = self.merged_state();
        let Some(state) = state else {
            self.emit("archive", SKIPPED, "gh-unavailable");
            return;
        };
        if state != "MERGED" {
            self.emit("archive", SKIPPED, &format!("not-merged (state={state})"));
            return;
        }
        let Some(branch) = branch else {
            self.emit("archive", SKIPPED, "no-branch");
            return;
        };
        let order = self.register_cleanup_request(&branch, "merged-pr");
        let order_written =
            order.contains("cleanup-requested") && !order.contains("reap-order-unwritten");
        let status = if order_written { DEFERRED } else { FAILED };
        self.emit(
            "archive",
            status,
            &format!("{order}; daemon resolves the tree from {branch}"),
        );
    }

    // -- judgment ------------------------------------------------------------

    /// Step 3e input (a): this PR's deferral-born nodes still open.
    fn deferral_nodes(&self) -> i64 {
        let argv: Vec<String> = ["backlog", "find", "deferred from PR", "-J"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let cwd = self.canon_path();
        let r = run_leg(&argv, &cwd, Duration::from_secs(LEG_TIMEOUT_SECS), false);
        if !r.ok() {
            return 0;
        }
        let rows: Value = serde_json::from_str(r.stdout.trim()).unwrap_or(json!([]));
        let needle = format!("#{}", self.pr);
        let mut count = 0i64;
        for row in rows.as_array().into_iter().flatten() {
            let status = row
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase();
            if matches!(status.as_str(), "done" | "superseded" | "cancelled") {
                continue;
            }
            let details = row.get("details").and_then(Value::as_str).unwrap_or("");
            let parent = row.get("parent").and_then(Value::as_str).unwrap_or("");
            if details.contains(&needle)
                || (!self.node_ids.is_empty() && self.node_ids.iter().any(|n| n == parent))
            {
                count += 1;
            }
        }
        count
    }

    /// Step 6 input (b): (changed_files, additions+deletions).
    fn diff_stat(&self) -> (i64, i64) {
        let run = self.gh(
            &[
                "pr",
                "view",
                &self.pr.to_string(),
                "--json",
                "additions,deletions,changedFiles",
            ],
            Duration::from_secs(30),
        );
        if !run.ok() {
            return (0, 0);
        }
        let obj: Value = serde_json::from_str(run.stdout.trim()).unwrap_or(json!({}));
        let files = obj.get("changedFiles").and_then(Value::as_i64).unwrap_or(0);
        let lines = obj.get("additions").and_then(Value::as_i64).unwrap_or(0)
            + obj.get("deletions").and_then(Value::as_i64).unwrap_or(0);
        (files, lines)
    }

    /// Compute judgment inputs; spawn one headless leg (autonomous only).
    fn leg_judgment(&mut self) {
        let deferred = self.deferral_nodes();
        let (files, lines) = self.diff_stat();
        let pl_set = self.parking_lot.is_some();
        // The bar gates on diff size alone, not parking-lot availability: a
        // maintainer-only item has a default home (.fno/tasks/user.md) even
        // without a shared lot.
        let above_bar = files >= 1 && lines >= 10;
        let inputs = format!(
            "deferred={deferred} files={files} lines={lines} parking_lot={} bar={}",
            if pl_set { "set" } else { "unset" },
            if above_bar { "above" } else { "below" },
        );
        let has_inputs = deferred > 0 || above_bar;
        if !self.autonomous {
            // An attended session does the judgment itself (skill body).
            self.emit(
                "judgment",
                OK,
                &format!("deferred-to-skill (attended); {inputs}"),
            );
            return;
        }
        if !has_inputs {
            let reason = if !above_bar && deferred == 0 {
                "no-inputs"
            } else {
                "diff-below-bar"
            };
            self.emit("judgment", SKIPPED, &format!("reason={reason}; {inputs}"));
            return;
        }
        let spawned = self.spawn_judgment(deferred, files, lines);
        if spawned {
            self.emit("judgment", OK, &format!("spawned headless; {inputs}"));
        } else {
            self.emit("judgment", FAILED, &format!("spawn-failed; {inputs}"));
        }
    }

    /// ONE headless one-shot carrying only the two judgment steps. The prompt
    /// is the sole positional; `pm-r-<node>-pr-<n>` rides --name. A merged PR
    /// with no recovered node binding refuses the spawn rather than
    /// substituting the PR number as a fake node.
    fn spawn_judgment(&self, deferred: i64, files: i64, lines: i64) -> bool {
        let node_ids: Vec<String> = self
            .node_ids
            .iter()
            .filter(|n| !n.is_empty())
            .cloned()
            .collect();
        let Some(node0) = node_ids.first() else {
            eprintln!(
                "post-merge judgment: skipped, PR {} binds no node; no pm-r worker spawned ",
                self.pr
            );
            return false;
        };
        let qualifier = format!("pr-{}", self.pr);
        let name = match crate::naming::bridge_name(
            None,
            node0,
            None,
            Some(&qualifier),
            None,
            Some("pm"),
            Some("r"),
            None,
        ) {
            Ok(n) => n,
            Err(_) => {
                // A stale/missing binary must fail this leg, not abort the
                // legs after it.
                eprintln!(
                    "post-merge judgment: skipped, worker name unmintable; no pm-r worker spawned"
                );
                return false;
            }
        };
        let prompt = self.judgment_prompt(deferred, files, lines);
        let mut argv: Vec<String> = vec![
            "agents".to_string(),
            "spawn".to_string(),
            "--substrate".to_string(),
            "headless".to_string(),
            "--timeout".to_string(),
            JUDGMENT_TIMEOUT_SECS.to_string(),
            "-c".to_string(),
            self.canon_path().display().to_string(),
        ];
        // Un-routed site: no --role, so config.post_merge.model IS the model.
        // --harness pins the binary WITH the model (the groom worker's
        // pairing); deliberately NOT `--role post-merge`, which auto-routes
        // to the secondary provider and this leg renders a judgment.
        let model = self.pm.as_ref().and_then(|pm| pm.model.clone());
        if let Some(model) = model {
            argv.push("--harness".to_string());
            argv.push("claude".to_string());
            argv.push("--model".to_string());
            argv.push(model);
        }
        argv.push("--name".to_string());
        argv.push(name);
        argv.push("--".to_string());
        argv.push(prompt);
        let cwd = self.canon_path();
        let r = run_leg(
            &argv,
            &cwd,
            Duration::from_secs(JUDGMENT_TIMEOUT_SECS + 60),
            false,
        );
        r.ok()
    }

    fn judgment_prompt(&self, deferred: i64, files: i64, lines: i64) -> String {
        let pr = self.pr;
        let marker = self
            .pm
            .as_ref()
            .and_then(|pm| pm.maintainer_marker.clone())
            .unwrap_or_default();
        let (dest, content) = if let Some(lot) = &self.parking_lot {
            // Shared/vault destination: narrative AND maintainer-only items
            // both live in the parking-lot section, the latter tagged only
            // when a marker is configured.
            let tag = if marker.is_empty() {
                String::new()
            } else {
                format!(" tagged `{marker}`")
            };
            (
                lot.display().to_string(),
                format!(
                    "narrative plus maintainer-only items (a decision/sign-off/manual setup){tag}"
                ),
            )
        } else {
            // Default: no shared lot, so ONLY maintainer-only items get a
            // home. Narrative must NOT go in this file; file real follow-up
            // work as a backlog node instead.
            (
                ".fno/tasks/user.md (under the canonical repo root)".to_string(),
                "maintainer-only items only (a decision/sign-off/manual setup), no tag - do NOT write narrative here".to_string(),
            )
        };
        format!(
            "Post-merge judgment for PR #{pr} (autonomous; do not prompt). \
(a) Triage {deferred} deferral-born node(s) filed from this PR: run \
`fno backlog find 'deferred from PR #{pr}'` and for each open node decide \
promote/keep/defer/supersede via the matching `fno backlog` verb, logging \
undecided ones. \
(b) Follow-up capture: read `gh pr diff {pr}`, and if the diff (files={files}, \
lines={lines}) carries genuine follow-up context, append ONE dated section \
keyed `<!-- post-merge:pr-{pr} -->` to {dest} via an append-only redirect \
(never overwrite): {content}. Skip if below the bar. \
Self-end when done; never spawn further work."
        )
    }

    // -- row reap ------------------------------------------------------------

    /// Step 8a: reap the merged node's lingering build-worker rows.
    fn leg_reap_rows(&mut self) {
        if !self.refuse_unless_merged("reap-rows") {
            return;
        }
        if self.node_ids.is_empty() {
            self.node_ids = self.recover_node_for_pr();
        }
        if self.node_ids.is_empty() {
            self.emit("reap-rows", SKIPPED, "no closed node ids");
            return;
        }
        let reap_on = self.pm.as_ref().map(|pm| pm.self_reap).unwrap_or(false);
        let rows = self.dead_target_rows();
        if rows.is_empty() {
            self.emit("reap-rows", OK, "no lingering rows");
            return;
        }
        if !reap_on {
            let cmds: Vec<String> = rows.iter().map(|n| format!("fno agents rm {n}")).collect();
            self.emit(
                "reap-rows",
                OK,
                &format!(
                    "{} row(s) linger; self_reap off - clear: {}",
                    rows.len(),
                    cmds.join("; ")
                ),
            );
            return;
        }
        // rm alone (law d-81c6da7e): the daemon's rm ends a live row's
        // process itself, so the reap leg runs no stop.
        let mut removed = 0usize;
        for name in &rows {
            let argv: Vec<String> = vec!["agents".to_string(), "rm".to_string(), name.clone()];
            let cwd = self.canon_path();
            let r = run_leg(&argv, &cwd, Duration::from_secs(30), false);
            if r.ok() {
                removed += 1;
            }
        }
        self.emit("reap-rows", OK, &format!("reaped {removed}/{}", rows.len()));
    }

    /// Non-live rows for the nodes this ritual closed, by canonical parse with
    /// the legacy `target-<node>-` fallback: the shared candidate read, then
    /// only non-live rows.
    fn dead_target_rows(&self) -> Vec<String> {
        let home = crate::paths::AgentsHome::from_env();
        let candidates: std::collections::HashSet<String> =
            crate::merge_reap::rows_for_cleanup(&home, None, &self.node_ids)
                .into_iter()
                .collect();
        let argv: Vec<String> = vec![
            "agents".to_string(),
            "list".to_string(),
            "--json".to_string(),
        ];
        let cwd = self.canon_path();
        let r = run_leg(&argv, &cwd, Duration::from_secs(LEG_TIMEOUT_SECS), false);
        if !r.ok() {
            return Vec::new();
        }
        let obj: Value = serde_json::from_str(r.stdout.trim()).unwrap_or(json!({}));
        let mut out = Vec::new();
        for a in obj
            .get("agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = a.get("name").and_then(Value::as_str).unwrap_or("");
            if !candidates.contains(name) {
                continue;
            }
            if a.get("status").and_then(Value::as_str) == Some("live") {
                continue;
            }
            out.push(name.to_string());
        }
        out
    }

    // -- streaming leg -------------------------------------------------------

    /// Run a verb, echoing each stdout line as progress; bounded by timeout.
    fn stream_leg(&mut self, step: &str, argv: &[String], timeout: Duration) {
        let mut cmd = Command::new(crate::scrape::fno_bin());
        cmd.args(argv)
            .current_dir(self.canon_path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.emit(step, FAILED, &format!("spawn-error: {e}"));
                return;
            }
        };
        let Some(mut out) = child.stdout.take() else {
            let _ = child.kill();
            self.emit(step, FAILED, "spawn-error: no stdout pipe");
            return;
        };
        let progress = Arc::new(Mutex::new(0usize));
        let reader_progress = Arc::clone(&progress);
        let step_owned = step.to_string();
        let reader = std::thread::spawn(move || {
            let mut pending = String::new();
            let mut chunk = [0u8; 65536];
            loop {
                match out.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        pending.push_str(&String::from_utf8_lossy(&chunk[..n]));
                        while let Some(pos) = pending.find('\n') {
                            let line: String = pending.drain(..pos + 1).collect();
                            let line = line.trim_end();
                            if !line.is_empty() {
                                println!("  {step_owned}: {line}");
                                if let Ok(mut p) = reader_progress.lock() {
                                    *p += 1;
                                }
                            }
                        }
                    }
                }
            }
        });
        let deadline = Instant::now() + timeout;
        let mut timed_out = false;
        let code;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    code = status.code().unwrap_or(1);
                    break;
                }
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        timed_out = true;
                        code = 124;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(_) => {
                    code = 127;
                    break;
                }
            }
        }
        let _ = reader.join();
        let lines = progress.lock().map(|p| *p).unwrap_or(0);
        if timed_out {
            self.emit(
                step,
                FAILED,
                &format!(
                    "timeout after {}s; {lines} progress lines",
                    timeout.as_secs()
                ),
            );
            return;
        }
        let status = if code == 0 { OK } else { FAILED };
        let detail = if lines > 0 {
            format!("exit={code} ({lines} progress lines)")
        } else {
            format!("exit={code}")
        };
        self.emit(step, status, &detail);
    }

    // -- driver --------------------------------------------------------------

    fn run(&mut self) -> i32 {
        if self.pr == 0 {
            self.emit("resolve", FAILED, "no merged PR found; pass a PR number");
            return 1;
        }
        let Some(pm) = &self.pm else {
            self.emit(
                "config",
                FAILED,
                "post_merge config unreadable (not a repo / bad config)",
            );
            return 1;
        };
        if !pm.enabled {
            // config.post_merge.enabled is the ritual's off switch; a repo
            // that sets it false must not acquire the mutex or run any leg.
            self.emit(
                "config",
                SKIPPED,
                "post_merge.enabled is false; ritual disabled",
            );
            return 0;
        }
        if !crate::backlog::advance_settings::autonomy_master_enabled(self.canon.as_deref()) {
            // The master panic switch outranks post_merge.enabled too - checked
            // separately so the receipt names WHICH one fired.
            self.emit(
                "config",
                SKIPPED,
                "autonomy.enabled is false; ritual disabled",
            );
            return 0;
        }
        self.emit(
            "resolve",
            OK,
            &format!(
                "pr={} source={}",
                self.pr,
                if self.pr_supplied {
                    "caller"
                } else {
                    "most-recent-merge"
                }
            ),
        );
        if !self.acquire_mutex() {
            return 0; // another runner owns it
        }
        self.leg_stamp();
        self.leg_harvest();
        self.leg_advance();
        self.leg_skill_diff();
        self.leg_sync_canonical();
        self.leg_archive();
        self.leg_judgment();
        self.leg_reap_rows();
        self.release_mutex();
        i32::from(self.any_failed)
    }
}

/// The `pr-watch ritual` arm: `<pr> [--autonomous]`, POST_MERGE_NONINTERACTIVE
/// implies --autonomous. Exit code IS the ritual's exit code.
pub fn run(args: &[String]) -> i32 {
    let mut pr: i64 = 0;
    let mut autonomous = false;
    for arg in args {
        match arg.as_str() {
            "--autonomous" => autonomous = true,
            other => match other.parse::<i64>() {
                Ok(n) => pr = n,
                Err(_) => {
                    eprintln!("pr-watch ritual: unrecognized argument: {other}");
                    return 2;
                }
            },
        }
    }
    if std::env::var("POST_MERGE_NONINTERACTIVE").as_deref() == Ok("1") {
        autonomous = true;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let mut ritual = Ritual::new(pr, autonomous, &cwd);
    ritual.run()
}

#[cfg(test)]
mod tests {
    /// AC7-ORDER: reaping must follow the retro. Reaping before the retro
    /// destroys the evidence the retro reads, and archiving after the row
    /// drop orphans the tree with nothing pointing at it. The ported
    /// test_worktree_reap_ordering.py source pins.
    #[test]
    fn retro_precedes_every_removal_leg() {
        let src = include_str!("ritual.rs");
        let run_at = src
            .find("fn run(&mut self)")
            .expect("the run driver exists");
        let run_body = &src[run_at..];
        let end = run_body
            .find("/// The `pr-watch ritual` arm")
            .expect("the arm follows run()");
        let run_body = &run_body[..end];
        let retro = run_body
            .find("self.leg_harvest();")
            .expect("the retro leg stays in run()");
        for removal in ["self.leg_archive();", "self.leg_reap_rows();"] {
            let at = run_body.find(removal).expect(removal);
            assert!(at > retro, "{removal} must run after the retro");
        }
        let archive = run_body.find("self.leg_archive();").expect("archive leg");
        let reap = run_body
            .find("self.leg_reap_rows();")
            .expect("reap-rows leg");
        assert!(archive < reap, "archive the worktree, then drop the row");
    }
}
