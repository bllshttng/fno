//! The `pr-watch tick` command: the LaunchAgent's entry. One slice per phase, so
//! a slow phase loses its turn instead of aborting the phases after it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use super::clock::{self, Call, DoorError};
use super::discover::{self, Candidate, Observation};
use super::dispatch::{
    self, Fire, QueueCounts, QueueRow, RitualAnswer, Seams, SweepOpts, TickResult,
};
use super::state;
use super::status::{as_bool, cfg_lookup};

/// EX_TEMPFAIL: launchd logs the exit but never respawns a StartInterval job
/// early, so a timed-out tick surfaces without suppressing its successor.
const TIMEOUT_EXIT: i32 = 75;
const ENV_TICK_TIMEOUT: &str = "FNO_PR_WATCH_TICK_TIMEOUT";
const SCHEDULER: &str = "launchd:sh.fno.pr-watcher";
/// One batched fetch plus a rev-list per worktree; skipping costs nothing,
/// because the next run sweeps from scratch.
const STRANDED_FLOOR_S: f64 = 10.0;
/// Belt over a wedged spawn, not a run bound: `--detach` answers in
/// milliseconds. At 30s one wedged root spent the whole heal cap.
const HEAL_DRIVE_TIMEOUT_S: f64 = 5.0;
const GRANT_QUEUE_READ_TIMEOUT_S: f64 = 90.0;
/// Merge's cap reserves the sweep's 150s after the worst settings and wake
/// caps on a 480s tick, with room for the queue read and the merge floor.
const MERGE_CAP_S: f64 = 245.0;
const SWEEP_CAP_S: f64 = 150.0;
/// Past the ceiling, this much slack before the backstop calls the tick hung:
/// the phases already sum under the ceiling, so reaching it means a hang.
const BACKSTOP_GRACE_S: f64 = 10.0;
/// The sweep's repoll floor, in ticks: a rich read no more often than every
/// Nth interval. The listing leg answers a row's state for free; only real
/// activity needs the rich read, and the least-recently-read order spreads
/// the pool across the window. Without the floor a ~50-row OPEN pool repays
/// its whole cost inside one 150s slice and starves every phase after sweep.
const MIN_REPOLL_TICKS: i64 = 2;

fn phase_cap(name: &str, merge_cap: f64) -> Option<f64> {
    Some(match name {
        "settings" => 10.0,
        "sweep" => SWEEP_CAP_S,
        // The arm over every catch-up root: 23.1s measured over 12 roots.
        "notify_watch" => 30.0,
        "heal" => 10.0,
        "stranded" => 60.0,
        "recovery" => 90.0,
        "watchdog" => 30.0,
        "merge" => merge_cap,
        _ => return None,
    })
}

fn merge_budget_for_load(load_per_core: f64, drain_floor_s: f64) -> (f64, f64) {
    let timeout = (MERGE_CAP_S + SWEEP_CAP_S - drain_floor_s - 35.0)
        .min(GRANT_QUEUE_READ_TIMEOUT_S * (load_per_core / 2.0).max(1.0));
    (timeout, MERGE_CAP_S.max(timeout + drain_floor_s + 5.0))
}

/// Env seam first, then config, then 0.85x the interval (min 60s). Config and
/// derived values stay BELOW the interval: launchd never runs the job
/// concurrently, so an overrun would suppress the successor tick.
fn resolve_deadline(interval: i64, configured: Option<i64>) -> i64 {
    if let Some(env) = env_timeout() {
        return env;
    }
    let ceiling = (interval - 5).max(1);
    match configured {
        Some(value) => value.min(ceiling),
        None => ((interval as f64 * 0.85) as i64).max(60).min(ceiling),
    }
}

fn env_timeout() -> Option<i64> {
    std::env::var(ENV_TICK_TIMEOUT)
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|v: &i64| *v > 0)
}

fn outcome_word(result: Option<&TickResult>, failed: bool) -> &'static str {
    match result {
        _ if failed => "error",
        None => "error",
        Some(r) if r.disabled => "disabled",
        Some(r) if r.lock_held => "lock_held",
        Some(r) if r.quota_skip || r.budget_hold_s.is_some() => "quota_skip",
        Some(r) if r.sweep_failures > 0 => "degraded",
        Some(_) => "ok",
    }
}

fn int_cfg(cwd: &Path, path: &[&str], default: i64) -> i64 {
    match cfg_lookup(cwd, path) {
        Some(toml::Value::Integer(i)) => i,
        _ => default,
    }
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Emitter {
    journal: PathBuf,
}

impl Emitter {
    fn emit(&self, kind: &str, data: Value) -> bool {
        let Value::Object(fields) = data else {
            return false;
        };
        let emitter = crate::events::EventEmitter::new(&self.journal, "daemon");
        match emitter.emit_fields(kind, fields) {
            Ok(()) => true,
            Err(err) => {
                eprintln!("pr-watch: emit {kind} failed: {err:?}");
                false
            }
        }
    }

    /// One arm row per phase outcome; the arms readout folds these.
    fn row(
        &self,
        arm: &str,
        interval_s: i64,
        acted: i64,
        skip: Option<&str>,
        detail: Option<&str>,
    ) {
        let mut data =
            json!({"arm": arm, "scheduler": SCHEDULER, "acted": acted, "interval_s": interval_s});
        if let Some(skip) = skip {
            data["skip_reason"] = json!(skip);
        }
        if let Some(detail) = detail {
            data["detail"] = json!(detail.chars().take(4000).collect::<String>());
        }
        self.emit("control_plane_tick", data);
    }
}

fn claims_root() -> Option<PathBuf> {
    std::env::var("FNO_CLAIMS_ROOT")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(PathBuf::from)
}

fn reviewers_from(value: Option<toml::Value>) -> Option<Vec<String>> {
    let array = value?;
    let rows = array.as_array()?;
    Some(
        rows.iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect(),
    )
}

/// The production world.
struct Live {
    emitter: Emitter,
    cwd: String,
}

impl Live {
    fn front_merge_ready(&self, repo: &Path) -> Result<bool, DoorError> {
        let program = crate::scrape::fno_bin();
        let args: Vec<String> = ["config", "doctor", "--post-merge", "--json"]
            .map(String::from)
            .to_vec();
        let out = clock::run(Call {
            program: &program,
            args: &args,
            stdin: None,
            cwd: Some(repo),
            // The door reads this pin before the cwd; an inherited one names
            // another repo.
            env: &[("FNO_REPO_ROOT", repo.display().to_string())],
            timeout_s: 30.0,
        })?;
        if out.code != 0 {
            return Err(DoorError::Failed(format!(
                "fno config doctor exited {}",
                out.code
            )));
        }
        let answer: Value =
            serde_json::from_str(&out.stdout).map_err(|e| DoorError::Failed(e.to_string()))?;
        Ok(answer.get("status").and_then(Value::as_str) == Some("ready"))
    }
}

impl Seams for Live {
    fn emit(&self, kind: &str, data: Value) -> bool {
        self.emitter.emit(kind, data)
    }

    fn notify(&self, message: &str) {
        crate::operator_notice::notify_operator("pr-watch", message, None);
    }

    fn acquire_lock(&self, key: &str, holder: &str) -> Result<(), String> {
        let opts = crate::claims::AcquireOpts {
            pid: Some(std::process::id()),
            root: claims_root(),
            ..Default::default()
        };
        match crate::claims::acquire(key, holder, opts) {
            crate::claims::AcquireOutcome::Acquired(_) => Ok(()),
            crate::claims::AcquireOutcome::HeldByOther { holder, pid, .. } => Err(match pid {
                Some(pid) => format!("lock held by {holder} (pid {pid})"),
                None => format!("lock held by {holder}"),
            }),
            // Not contention: calling this a held lock would disguise a hard
            // failure as routine skipping.
            crate::claims::AcquireOutcome::Error(err) => {
                Err(format!("tick lock unavailable: {err}"))
            }
        }
    }

    fn release_lock(&self, key: &str, holder: &str) {
        if let Err(err) = crate::claims::release(key, holder, claims_root().as_deref(), None) {
            eprintln!("pr-watch: failed to release {key}: {err}");
        }
    }

    /// A suspect claim (TTL-unexpired, dead pid) is a respawned worker's slot.
    /// An unreadable store reads live: never double-dispatch onto a session.
    fn is_node_live(&self, node: &str) -> bool {
        match crate::claims::status_checked(&format!("node:{node}"), claims_root().as_deref()) {
            Ok((state, _)) => matches!(state.as_str(), "live" | "suspect"),
            Err(err) => {
                eprintln!(
                    "pr-watch: claim status failed for node {node} ({err}); treating as live"
                );
                true
            }
        }
    }

    fn reviewers_for(&self, repo: &Path) -> Vec<String> {
        reviewers_from(cfg_lookup(repo, &["review", "github_apps"]))
            .or_else(|| reviewers_from(cfg_lookup(repo, &["review", "required_bots"])))
            .unwrap_or_default()
    }

    fn read_tracked(
        &self,
        keys: &BTreeSet<String>,
    ) -> Result<(std::collections::BTreeMap<String, String>, i64), DoorError> {
        discover::read_tracked_states(keys)
    }

    fn read_pr(
        &self,
        cand: &Candidate,
        reviewers: &[String],
        timeout_s: f64,
    ) -> Result<Observation, DoorError> {
        discover::read_pr_state(cand, reviewers, timeout_s)
    }

    /// An unreadable instrument answers no evidence of a low budget.
    fn graphql_remaining(&self) -> (Option<i64>, Option<String>) {
        let answer = clock::door(
            &["authorized-merge"],
            Some(&json!({"op": "status-quota-graphql", "cwd": self.cwd})),
            60.0,
        );
        match answer {
            Ok(answer) => (
                answer.get("remaining").and_then(Value::as_i64),
                answer
                    .get("reset_iso")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            ),
            Err(_) => (None, None),
        }
    }

    fn merge_ready(&self, repo: &Path) -> Result<bool, DoorError> {
        self.front_merge_ready(repo)
    }

    fn fire_check(&self, cand: &Candidate) -> Result<Fire, DoorError> {
        let repo = cand.repo_dir.clone().unwrap_or_default();
        let Some(mut argv) = dispatch::fire_argv(cand, &repo) else {
            eprintln!(
                "pr-watch: PR #{} binds no graph node; refusing the check fire",
                cand.pr
            );
            return Ok(Fire {
                ok: false,
                is_error: false,
            });
        };
        let program = argv.remove(0);
        let out = clock::run(Call {
            program: std::ffi::OsStr::new(&program),
            args: &argv,
            stdin: None,
            cwd: Some(&repo),
            env: &[],
            timeout_s: dispatch::fire_timeout(),
        })?;
        Ok(dispatch::fire_verdict(out.code, &out.stdout))
    }

    fn dispatch_ritual(
        &self,
        cand: &Candidate,
        obs: &Observation,
    ) -> Result<RitualAnswer, DoorError> {
        let bound = dispatch::ritual_bound();
        let out = clock::door(
            &["pr-watch", "post-merge-dispatch"],
            Some(&json!({
                "pr_number": cand.pr,
                "dedup_key": obs.merge_sha,
                "node_cwd": cand.repo_dir,
                "ship_session_id": cand.ship_session_id,
                "ship_harness": cand.ship_harness,
                "source_session_id": cand.source_session_id,
                "source_harness": cand.source_harness,
                "source_cwd": cand.source_cwd,
                "node_id": cand.node_id,
                "repo_slug": cand.repo_slug,
                "timeout_s": bound,
            })),
            bound + 30.0,
        )?;
        Ok(RitualAnswer {
            outcome: out
                .get("outcome")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            detail: out
                .get("detail")
                .and_then(Value::as_str)
                .map(str::to_string),
        })
    }

    fn flip_draft(&self, cand: &Candidate) -> Result<(), DoorError> {
        clock::door(
            &["graph-get"],
            Some(&json!({"pr_draft_ready": {"flip": {
                "pr": cand.pr, "repo": cand.repo_slug, "node": cand.node_id,
                "cwd": cand.repo_dir, "journal": self.emitter.journal,
            }}})),
            60.0,
        )
        .map(|_| ())
    }

    /// GitHub's auto-merge queue can land a merge with no fno process in the
    /// loop, so this detector pays the remote-delete step the merge command
    /// would have. Reached only from the MERGED arm; cleanup never blocks.
    fn finish_queue_merge(&self, repo: &Path, pr: i64) -> Result<(), DoorError> {
        if !as_bool(
            cfg_lookup(repo, &["auto_merge", "delete_branch_on_merge"]),
            true,
        ) {
            return Ok(());
        }
        let out = clock::door(
            &["authorized-merge"],
            Some(&json!({"op": "merge-remote-delete", "pr": pr, "cwd": repo})),
            120.0,
        )?;
        let cleanup = out.get("cleanup").and_then(Value::as_str).unwrap_or("");
        if !cleanup.is_empty() {
            eprintln!("pr-watch: PR #{pr} remote cleanup: {cleanup}");
            self.emit(
                "pr_watch_cleanup_failed",
                json!({"pr": pr, "cleanup": cleanup}),
            );
        }
        Ok(())
    }

    fn ensure_merge_cleanup(&self, cand: &Candidate, state: &str) -> Result<(), DoorError> {
        ensure_merge_cleanup(cand, state)
    }

    /// One freeze-check receipt answers; an unreadable one refuses fail closed.
    fn freeze_refusal(&self, pr: i64) -> Result<Option<String>, DoorError> {
        let payload = json!({"op": "freeze-check", "pr": pr}).to_string();
        let receipt = match clock::door(&["authorized-merge", &payload], None, 15.0) {
            Ok(receipt) => receipt,
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(err)) => {
                return Ok(Some(format!(
                    "merge-freeze check unavailable ({err}); the arm fails closed"
                )))
            }
        };
        let detail = receipt
            .get("detail")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        Ok(match receipt.get("outcome").and_then(Value::as_str) {
            Some("clear") => None,
            Some("frozen") => Some(format!(
                "a merge freeze holds ({}); PR {pr} is not on its allow-list",
                detail.unwrap_or("unnamed")
            )),
            _ => Some(format!(
                "merge-freeze record unreadable ({}); the arm fails closed",
                detail.unwrap_or("None")
            )),
        })
    }

    /// The budget protects the fleet, not the tick: unreadable reads free.
    fn gh_backoff_left(&self) -> Result<f64, DoorError> {
        match clock::door(&["fleet-incident"], Some(&json!({"op": "status"})), 5.0) {
            Ok(answer) => Ok(answer
                .get("backoff_remaining_s")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                .max(0.0)),
            Err(DoorError::Cut) => Err(DoorError::Cut),
            Err(DoorError::Failed(_)) => Ok(0.0),
        }
    }

    fn merge(&self, pr: i64, cwd: &Path) -> Result<Value, DoorError> {
        clock::door(
            &["authorized-merge"],
            Some(
                &json!({"op": "merge", "argv": [pr.to_string()], "authority": "durable_grant", "cwd": cwd}),
            ),
            900.0,
        )
    }
}

/// `(repo, project)` resolved the ritual's way, so both mints share one fold key.
fn cleanup_repo_and_project(cwd: &Path) -> (String, String) {
    let git = |args: &[&str]| -> Option<String> {
        let argv: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        let out = clock::run(Call {
            program: std::ffi::OsStr::new("git"),
            args: &argv,
            stdin: None,
            cwd: Some(cwd),
            env: &[],
            timeout_s: 10.0,
        })
        .ok()?;
        (out.code == 0)
            .then(|| out.stdout.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let common = git(&["rev-parse", "--git-common-dir"]).and_then(|raw| {
        let path = PathBuf::from(&raw);
        let path = if path.is_absolute() {
            path
        } else {
            cwd.join(path)
        };
        path.exists()
            .then(|| path.parent().map(|p| p.display().to_string()))
            .flatten()
    });
    let repo = common
        .or_else(|| git(&["rev-parse", "--show-toplevel"]))
        .unwrap_or_else(|| cwd.display().to_string());
    let project = match cfg_lookup(Path::new(&repo), &["project", "id"]) {
        Some(toml::Value::String(id)) => id,
        _ => String::new(),
    };
    (repo, project)
}

/// Mint the reap order for a merged PR with none standing, or the skip row the
/// journal contract requires: a request or a skip, never silence.
fn ensure_merge_cleanup(cand: &Candidate, state: &str) -> Result<(), DoorError> {
    let Some(repo_dir) = cand
        .repo_dir
        .as_deref()
        .filter(|d| state == "MERGED" && d.join(".git").exists())
    else {
        return Ok(());
    };
    let pr = cand.pr;
    let (repo, project) = cleanup_repo_and_project(repo_dir);
    let home = crate::paths::AgentsHome::from_env();
    let journal = home.root().join("events.jsonl");
    let minted = crate::event_store::journal_text(&journal, &["merge_cleanup_requested"]);
    for line in minted.lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &record["data"];
        if data["repo"].as_str() == Some(&repo) && data["pr"].as_i64() == Some(pr) {
            return Ok(());
        }
    }
    let skip = |reason: &str, detail: &str, branch: Option<&str>| {
        let fields = json!({"repo": repo, "project": project, "pr": pr, "reason": reason,
                            "branch": branch, "detail": detail, "session_id": null, "harness": null});
        if let Value::Object(fields) = fields {
            let _ = crate::events::EventEmitter::new(&journal, "rust")
                .emit_fields("merge_cleanup_skipped", fields);
        }
    };
    let argv: Vec<String> = [
        "pr",
        "view",
        &pr.to_string(),
        "--json",
        "state,headRefName,url,mergedAt",
    ]
    .map(String::from)
    .to_vec();
    let read = clock::run(Call {
        program: std::ffi::OsStr::new("gh"),
        args: &argv,
        stdin: None,
        cwd: Some(repo_dir),
        env: &[],
        timeout_s: 30.0,
    });
    let meta = match read {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(err)) => {
            skip(
                "gh-unavailable",
                &err.chars().take(200).collect::<String>(),
                None,
            );
            return Ok(());
        }
        Ok(out) if out.code != 0 => {
            skip(
                "gh-unavailable",
                &out.stderr.trim().chars().take(200).collect::<String>(),
                None,
            );
            return Ok(());
        }
        Ok(out) => match serde_json::from_str::<Value>(&out.stdout) {
            Ok(meta) if meta.is_object() => meta,
            _ => {
                skip("unparseable-pr-json", "ValueError", None);
                return Ok(());
            }
        },
    };
    let branch = meta
        .get("headRefName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let pr_state = meta.get("state").and_then(Value::as_str);
    if pr_state != Some("MERGED") {
        skip(
            "not-merged",
            &format!("state={}", pr_state.unwrap_or("None")),
            branch,
        );
        return Ok(());
    }
    let Some(branch) = branch else {
        skip("no-branch", "", None);
        return Ok(());
    };
    let (session_id, harness) = crate::claims::resolve_identity();
    let nodes: Vec<&str> = Some(cand.node_id.as_str())
        .filter(|n| !n.is_empty())
        .into_iter()
        .collect();
    clock::door(
        &["pr-watch", "merge-cleanup-mint"],
        Some(
            &json!({"pr": pr, "branch": branch, "repo": repo, "project": project,
                     "node_ids": nodes, "session_id": session_id, "harness": harness}),
        ),
        120.0,
    )?;
    Ok(())
}

fn catchup_roots() -> Vec<PathBuf> {
    let name = crate::tracker::backend_name(None);
    let sidecars = crate::tracker::backend(&name).and_then(|tracker| tracker.sidecar_all());
    let sidecars = match sidecars {
        Ok(rows) => rows,
        Err(err) => {
            eprintln!("pr-watch: could not read sidecars for catch-up roots: {err:?}");
            return Vec::new();
        }
    };
    let mut seen = BTreeSet::new();
    let mut roots = Vec::new();
    for (_, row) in sidecars {
        if let Some(cwd) = row
            .get("cwd")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if seen.insert(cwd.to_string()) && Path::new(cwd).is_dir() {
                roots.push(PathBuf::from(cwd));
            }
        }
    }
    roots
}

struct Tick {
    started: Instant,
    cwd: PathBuf,
    live: Live,
    interval: i64,
    max_age_days: i64,
    retries: i64,
    graphql_min: i64,
    enabled: bool,
    autonomy: bool,
    ceiling: Option<f64>,
    merge_cap: f64,
    grant_timeout: f64,
    roots: Option<Vec<PathBuf>>,
    result: Option<TickResult>,
    tick_failed: Option<String>,
    cut: Vec<String>,
    phase_s: Map<String, Value>,
    watchdog_interval: i64,
}

impl Tick {
    fn roots(&mut self) -> Vec<PathBuf> {
        self.roots.get_or_insert_with(catchup_roots).clone()
    }

    fn arm_interval(&self, arm: &str) -> i64 {
        match arm {
            "watchdog" => self.watchdog_interval,
            _ => self.interval,
        }
    }

    fn run_phase(
        &mut self,
        name: &str,
        arm: Option<&str>,
        cadence: i64,
        slot: i64,
        body: fn(&mut Tick, f64) -> Result<(), DoorError>,
    ) {
        if cadence > 1 {
            // Fleet tail: a phase above cadence 1 runs one tick in `cadence`,
            // on its slot. A skip mints a row naming its next tick and no cut,
            // so the outcome never reads timeout for a phase that never ran.
            let tick_no = now_secs() as i64 / self.interval.max(1);
            if tick_no.rem_euclid(cadence) != slot {
                self.phase_s.insert(name.to_string(), json!(0.0));
                clock::take_note(name);
                if let Some(arm) = arm {
                    let next = tick_no + (slot - tick_no).rem_euclid(cadence);
                    let detail = format!("fleet-tail cadence {cadence}: next tick {next}");
                    self.live.emitter.row(
                        arm,
                        self.arm_interval(arm),
                        0,
                        Some("off_cadence"),
                        Some(&detail),
                    );
                }
                return;
            }
        }
        // A stale note must never ride this run's receipt.
        clock::take_note(name);
        let ceiling = self.ceiling.expect("the settings phase sets the ceiling");
        let left = ceiling - self.started.elapsed().as_secs_f64();
        if left <= 0.0 {
            self.cut.push(name.to_string());
            self.phase_s.insert(name.to_string(), json!(0.0));
            if let Some(arm) = arm {
                // starved, not broken: timeout is a FAILURE_SKIPS token.
                let detail = format!("deadline exceeded before phase {name}");
                self.live.emitter.row(
                    arm,
                    self.arm_interval(arm),
                    0,
                    Some("starved"),
                    Some(&detail),
                );
            }
            return;
        }
        let cap = phase_cap(name, self.merge_cap);
        let slice = cap.map_or(left, |c| c.min(left)).max(1.0);
        // Which budget fired: a cap below the wall starves one phase.
        let wall_limited = cap.is_none_or(|c| c >= left);
        let phase_start = Instant::now();
        clock::set_phase(name);
        clock::set_deadline(Some(
            Instant::now() + std::time::Duration::from_secs_f64(slice),
        ));
        let outcome = body(self, slice);
        clock::set_deadline(None);
        let seconds = (phase_start.elapsed().as_secs_f64() * 10.0).round() / 10.0;
        self.phase_s.insert(name.to_string(), json!(seconds));
        match outcome {
            Ok(()) => {}
            Err(DoorError::Failed(reason)) => eprintln!("pr-watch: phase {name} failed: {reason}"),
            Err(DoorError::Cut) => {
                self.cut.push(name.to_string());
                if let Some(arm) = arm {
                    // Name the sub-step and hand back the partial work, so the
                    // row says "scanned 21 of 39", never a bare timeout.
                    let step = clock::phase();
                    let at = if step.starts_with(&format!("{name}:")) {
                        format!(" at {step}")
                    } else {
                        String::new()
                    };
                    let base = if wall_limited {
                        format!("deadline exceeded in phase {name} at {}s{at}", slice as i64)
                    } else {
                        format!("phase slice {}s spent{at}", slice as i64)
                    };
                    let detail = match clock::take_note(name) {
                        Some(note) if !note.is_empty() => format!("{base} {note}"),
                        _ => base,
                    };
                    self.live.emitter.row(
                        arm,
                        self.arm_interval(arm),
                        0,
                        Some("starved"),
                        Some(&detail),
                    );
                }
            }
        }
    }
}

fn phase_merge(t: &mut Tick, slice: f64) -> Result<(), DoorError> {
    let head = "merge ok";
    let interval = t.interval;
    if !t.enabled {
        let detail = format!("{head} pr_watch disabled");
        t.live.emitter.row(
            "pr_watch_merge",
            interval,
            0,
            Some("disabled"),
            Some(&detail),
        );
        return Ok(());
    }
    let roots = t.roots();
    let cwd = roots.first().cloned().unwrap_or_else(|| t.cwd.clone());
    // The read's bound travels as deadline_ms, so its git reads answer inside.
    let grant_timeout = t.grant_timeout.min((slice - 10.0).max(1.0));
    let read = clock::door(
        &["authorized-merge"],
        Some(
            &json!({"op": "grant-queue", "rotate": now_secs() as i64 / interval.max(1), "cwd": cwd,
                     "deadline_ms": (grant_timeout * 1000.0) as i64}),
        ),
        grant_timeout,
    );
    let unreadable = |t: &Tick, why: &str| {
        let reason = if why.to_lowercase().contains("timed out after") {
            "grant_queue_timeout"
        } else {
            "error"
        };
        let detail = format!("{head} grant queue unreadable ({why})");
        t.live
            .emitter
            .row("pr_watch_merge", interval, 0, Some(reason), Some(&detail));
    };
    let out = match read {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(why)) => {
            unreadable(t, &why);
            return Ok(());
        }
        Ok(out) => out,
    };
    if let Some(error) = out.get("error").filter(|e| !e.is_null()) {
        let why = error
            .as_str()
            .map_or_else(|| error.to_string(), str::to_string);
        unreadable(t, &why);
        return Ok(());
    }
    let mut queue = Vec::new();
    for row in out
        .get("queue")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let fields = (
            row.get("node_id").and_then(Value::as_str),
            row.get("pr").and_then(Value::as_i64),
            row.get("cwd").and_then(Value::as_str),
            row.get("repo_slug").and_then(Value::as_str),
        );
        let (Some(node), Some(pr), Some(row_cwd), Some(slug)) = fields else {
            unreadable(t, "a queue row is missing node_id, pr, cwd or repo_slug");
            return Ok(());
        };
        let Some(key) = state::make_key(Some(slug), pr) else {
            unreadable(t, &format!("invalid PR watermark key for {slug}#{pr}"));
            return Ok(());
        };
        queue.push(QueueRow {
            cand: Candidate {
                node_id: node.to_string(),
                pr,
                repo_dir: Some(PathBuf::from(row_cwd)),
                repo_slug: Some(slug.to_string()),
                ..Default::default()
            },
            key,
            grant: row
                .get("grant")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        });
    }
    clock::note("merge", format!("queue={}", queue.len()));
    let mut counts = QueueCounts::default();
    let store = state::default_store_path(&t.cwd);
    dispatch::run_execute_queue(
        &t.live,
        &queue,
        &store,
        t.retries,
        &chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        &mut counts,
    )?;
    let starved = counts.budget > 0 && counts.executed + counts.held + counts.failed == 0;
    let verdicts = out.get("verdicts").cloned().unwrap_or(json!({}));
    let int = |v: Option<&Value>, d: i64| v.and_then(Value::as_i64).unwrap_or(d);
    let detail = format!(
        "{head} candidates={} granted={} executed={} held={} failed={} skipped={} budget={} read_ms={}",
        int(out.get("candidates"), 0),
        int(verdicts.get("granted"), 0),
        counts.executed,
        counts.held,
        counts.failed,
        counts.skipped,
        counts.budget,
        int(out.get("elapsed_ms"), -1),
    );
    let skip = starved.then_some("budget_spent");
    t.live.emitter.row(
        "pr_watch_merge",
        interval,
        counts.executed,
        skip,
        Some(&detail),
    );
    Ok(())
}

fn phase_sweep(t: &mut Tick, slice: f64) -> Result<(), DoorError> {
    let interval = t.interval;
    let graph = crate::backlog::settings::graph_path();
    if !t.enabled {
        t.result = Some(TickResult {
            disabled: true,
            ..Default::default()
        });
    } else {
        let opts = SweepOpts {
            store_path: state::default_store_path(&t.cwd),
            now_iso: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            max_age_days: t.max_age_days,
            max_retries: t.retries,
            graphql_min_remaining: t.graphql_min,
            dispatch_deadline: Some(Instant::now() + std::time::Duration::from_secs_f64(slice)),
            min_repoll_s: interval * MIN_REPOLL_TICKS,
        };
        clock::set_phase("sweep:discover");
        let entries = clock::bounded_read(slice, move || discover::graph_entries(&graph))?;
        match dispatch::sweep(&t.live, entries, &opts) {
            Ok(result) => t.result = Some(result),
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            // A dead events path must not stop the legs below; the exit code
            // fails at the end instead.
            Err(DoorError::Failed(reason)) => {
                eprintln!("pr-watch tick: failed: {reason}");
                t.live
                    .emitter
                    .row("pr_watch_sweep", interval, 0, Some("error"), Some(&reason));
                t.tick_failed = Some(reason);
                return Ok(());
            }
        }
    }
    let Some(result) = t.result.as_ref() else {
        return Ok(());
    };
    if result.disabled {
        let reason = if t.autonomy {
            "config.pr_watch.enabled"
        } else {
            "config.autonomy.enabled"
        };
        println!("pr-watch tick: {reason} is false - skipped");
    } else if result.lock_held {
        println!("pr-watch tick: {} - skipped", result.lock_holder);
    } else if let Some(hold) = result.budget_hold_s {
        println!("pr-watch tick: fleet gh budget held ({hold:.0}s left) - dispatch pass skipped");
    } else if result.quota_skip {
        let reset = result
            .quota_reset
            .as_ref()
            .map_or(String::new(), |r| format!(", resets {r}"));
        let degraded = if result.sweep_failures > 0 {
            format!(" (degraded: {} sweep failure(s))", result.sweep_failures)
        } else {
            String::new()
        };
        println!(
            "pr-watch tick: graphql remaining {} below floor - dispatch pass skipped{reset}{degraded}",
            result.quota_remaining.unwrap_or(0)
        );
    } else if result.sweep_failures > 0 {
        println!(
            "pr-watch tick: degraded: {} sweep failure(s)",
            result.sweep_failures
        );
    } else {
        println!(
            "pr-watch tick: open_prs={} acted={} skipped={}",
            result.open_prs, result.acted, result.skipped
        );
    }
    let skip = if result.disabled {
        Some("disabled")
    } else if result.lock_held {
        Some("lock_held")
    } else if result.budget_hold_s.is_some() {
        Some("budget_hold")
    } else if result.quota_skip {
        Some("quota_skip")
    } else {
        None
    };
    let detail = if result.budget_hold_s.is_some() {
        "fleet gh budget hold - dispatch pass skipped".to_string()
    } else {
        format!(
            "open_prs={} acted={} skipped={}",
            result.open_prs, result.acted, result.skipped
        )
    };
    t.live.emitter.row(
        "pr_watch_sweep",
        interval,
        result.acted,
        skip,
        Some(&detail),
    );
    Ok(())
}

/// The arm's receipt becomes the tick row; the row carries the arm's real
/// cadence, so the reader's stale bound stays honest about a healthy rotation.
/// A dead notice lane never raises out of the tick.
fn phase_notify(t: &mut Tick, slice: f64) -> Result<(), DoorError> {
    clock::set_phase("notify_watch:roots");
    let roots = t.roots();
    let interval = t.interval;
    clock::set_phase("notify_watch:arm");
    let mut argv = vec!["notify-watch".to_string(), "--json".to_string()];
    for root in &roots {
        argv.push("--root".to_string());
        argv.push(root.display().to_string());
    }
    let exe = std::env::current_exe().map_err(|e| DoorError::Failed(e.to_string()))?;
    let bound = (slice - 2.0).max(1.0);
    // launchd starts this daemon in `/`, where a board read would read nothing.
    let answer = clock::run(Call {
        program: exe.as_os_str(),
        args: &argv,
        stdin: None,
        cwd: roots.first().map(PathBuf::as_path),
        env: &[],
        timeout_s: bound,
    });
    let row = |skip: Option<&str>, acted: i64, detail: &str| {
        let detail: String = detail.chars().take(200).collect();
        t.live
            .emitter
            .row("notify_watch", interval, acted, skip, Some(&detail));
    };
    match answer {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        // A bound overrun is a budget cut; notify_failed would read FAIL.
        Err(DoorError::Failed(why)) if why.contains("timed out after") => {
            row(
                Some("starved"),
                0,
                &format!("arm pass exceeded its {bound:.0}s bound"),
            );
        }
        Err(DoorError::Failed(why)) => row(Some("notify_failed"), 0, &why),
        Ok(out) => {
            let text = if out.stdout.is_empty() {
                "{}"
            } else {
                out.stdout.as_str()
            };
            match serde_json::from_str::<Value>(text) {
                Ok(payload) => row(
                    payload.get("skip_reason").and_then(Value::as_str),
                    payload.get("acted").and_then(Value::as_i64).unwrap_or(0),
                    payload.get("detail").and_then(Value::as_str).unwrap_or(""),
                ),
                Err(err) => row(Some("notify_failed"), 0, &err.to_string()),
            }
        }
    }
    Ok(())
}

/// Nothing else calls the healer on a timer. The loop lives in `pr-heal`; this
/// is only the gate, and every gate answer lands as a row so status says why.
fn phase_heal(t: &mut Tick, _slice: f64) -> Result<(), DoorError> {
    clock::set_phase("heal");
    let answer = if !as_bool(cfg_lookup(&t.cwd, &["auto_heal", "enabled"]), false) {
        "unarmed"
    } else {
        let roots = t.roots();
        if roots.is_empty() {
            "no-roots"
        } else {
            let mut argv: Vec<String> = ["pr-heal", "--all", "--apply", "--detach"]
                .map(String::from)
                .to_vec();
            for root in &roots {
                argv.push("--cwd".to_string());
                argv.push(root.display().to_string());
            }
            match clock::door_raw(&argv, HEAL_DRIVE_TIMEOUT_S) {
                Err(DoorError::Cut) => return Err(DoorError::Cut),
                Err(DoorError::Failed(why)) => {
                    eprintln!("pr-watch: heal drive loop failed: {why}");
                    "failed"
                }
                // 0..3 are drive-loop verdicts; anything else names a stale
                // binary that would otherwise read as ran with no row.
                Ok(out) if (0..=3).contains(&out.code) => "ran",
                Ok(out) => {
                    eprintln!(
                        "pr-watch: heal drive loop exited {}; run `fno doctor`",
                        out.code
                    );
                    "failed"
                }
            }
        }
    };
    println!("pr heal: {answer}");
    if answer != "ran" {
        let skip = answer.replace('-', "_");
        let detail = format!("auto_heal gate: {answer}");
        t.live
            .emitter
            .row("heal", t.interval, 0, Some(&skip), Some(&detail));
    }
    Ok(())
}

/// Report, never reap: only STRANDED rows get pushed and filed; only UNKNOWN
/// rows get recorded. The sweep always feeds the board's provenance cache;
/// the lane's arming decides only whether it acts.
fn phase_stranded(t: &mut Tick, _slice: f64) -> Result<(), DoorError> {
    clock::set_phase("stranded");
    let arming = match clock::door(&["census", "watchdog-arming"], None, 15.0) {
        Ok(answer) => answer,
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(_)) => json!({}),
    };
    let on = |key: &str| arming.get(key).and_then(Value::as_bool) == Some(true);
    let wake = on("armed") && on("wake_armed");
    if clock::left().unwrap_or(0.0) < STRANDED_FLOOR_S {
        return Ok(());
    }
    let mut roots = t.roots();
    if !roots.is_empty() {
        // One run per three buckets, so consecutive RUNS start at consecutive
        // roots and a cap cut never replays the same prefix.
        let run = now_secs() as i64 / t.interval.max(1) / 3;
        let start = run.rem_euclid(roots.len() as i64) as usize;
        roots.rotate_left(start);
    }
    let (mut changed, mut stranded, mut unknown, mut acted, mut failed) = (false, 0, 0, 0, 0);
    for root in &roots {
        // Per root: a multi-repo tick could blow past the shared deadline
        // after the first root's own check passed.
        if clock::left().unwrap_or(0.0) < STRANDED_FLOOR_S {
            break;
        }
        let root_text = root.display().to_string();
        let mut args = vec![
            "worktree-stranded",
            "--repo",
            root_text.as_str(),
            "--json",
            "--apply",
        ];
        if !wake {
            // The report posture still records UNKNOWN rows.
            args.push("--no-wake");
        }
        let payload = match clock::door(&args, None, 120.0) {
            Ok(payload) => payload,
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(why)) => {
                eprintln!("pr-watch: stranded sweep failed for {root_text}: {why}");
                continue;
            }
        };
        let rows = payload
            .get("rows")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let outcomes = payload
            .get("outcomes")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        changed |= state::write_provenance_cache(root, &rows);
        let class = |name: &str| {
            rows.iter()
                .filter(|r| r["class"].as_str() == Some(name))
                .count()
        };
        stranded += class("STRANDED");
        unknown += class("UNKNOWN");
        acted += outcomes.len();
        failed += outcomes
            .iter()
            .filter(|o| !o["stopped_at"].is_null())
            .count();
    }
    println!(
        "stranded sweep ({}): stranded={stranded} unknown={unknown} acted={acted} failed={failed}",
        if wake { "wake" } else { "report" }
    );
    if changed {
        // The cache moves no store version, so the keeper's render pass would
        // never see the change.
        let argv = ["backlog", "render-views"].map(String::from).to_vec();
        clock::door_raw(&argv, 600.0)?;
    }
    Ok(())
}

fn phase_recovery(t: &mut Tick, _slice: f64) -> Result<(), DoorError> {
    clock::set_phase("recovery");
    if !as_bool(cfg_lookup(&t.cwd, &["recovery", "enabled"]), true) || !t.autonomy {
        return Ok(());
    }
    let (mut candidates, mut refused, mut silent, mut swept) = (0, 0, 0, false);
    match clock::door(
        &["census", "recovery-sweep"],
        None,
        clock::left().unwrap_or(5.0).max(5.0),
    ) {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(why)) => eprintln!("pr-watch: recovery sweep failed: {why}"),
        Ok(answer) => {
            candidates = answer
                .get("candidates")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            for event in answer
                .get("events")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
                if kind == "worker_refused" {
                    refused += 1;
                }
                t.live
                    .emit(kind, event.get("data").cloned().unwrap_or(json!({})));
            }
            // A disarmed sweep measured nothing, so it stamps no heartbeat.
            swept = answer.get("swept").and_then(Value::as_bool) == Some(true);
            println!("recovery sweep: candidates={candidates}");
        }
    }
    // The cadence-deadline backstop for a refusal the taxonomy does not
    // recognise; separate, so neither takes the other down.
    match clock::door(&["census", "sweep", "--tick"], None, 60.0) {
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(why)) => eprintln!("pr-watch: silence sweep failed: {why}"),
        Ok(answer) => {
            silent = answer.get("silent").and_then(Value::as_i64).unwrap_or(0);
            if silent > 0 {
                println!("silence sweep: silent={silent}");
            }
        }
    }
    // Only on a COMPLETED sweep: a failed one that stamped this watermark
    // would render as a healthy quiet fleet.
    if swept {
        let beat = json!({"candidates": candidates, "refused": refused, "silent": silent});
        match clock::door(
            &["fleet-incident", "fleet-state", "heartbeat"],
            Some(&beat),
            5.0,
        ) {
            Err(DoorError::Cut) => return Err(DoorError::Cut),
            Err(DoorError::Failed(why)) => {
                eprintln!("pr-watch: fleet heartbeat write failed: {why}")
            }
            Ok(_) => {}
        }
    }
    Ok(())
}

fn phase_watchdog(t: &mut Tick, _slice: f64) -> Result<(), DoorError> {
    clock::set_phase("watchdog");
    let interval = t.interval;
    t.watchdog_interval = interval;
    let left = clock::left().unwrap_or(0.0);
    let budget = format!("{left:.1}");
    let answer = match clock::door(
        &["census", "watchdog-tick", "--budget-s", &budget],
        None,
        left.max(5.0),
    ) {
        Ok(answer) => answer,
        Err(DoorError::Cut) => return Err(DoorError::Cut),
        Err(DoorError::Failed(why)) => {
            let detail: String = why.chars().take(200).collect();
            t.live
                .emitter
                .row("watchdog", interval, 0, Some("sweep_failed"), Some(&detail));
            return Ok(());
        }
    };
    let text = |key: &str| {
        answer
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    };
    if answer.get("armed").and_then(Value::as_bool) != Some(true) {
        // An unarmed lane still ticks: why it did nothing is the readout's job.
        t.live.emitter.row(
            "watchdog",
            interval,
            0,
            Some("watchdog_off"),
            Some(&text("off_detail")),
        );
        return Ok(());
    }
    if let Some(skip) = answer
        .get("skip_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let detail: String = text("detail").chars().take(200).collect();
        t.live
            .emitter
            .row("watchdog", interval, 0, Some(skip), Some(&detail));
        return Ok(());
    }
    let int = |key: &str| answer.get(key).and_then(Value::as_i64).unwrap_or(0);
    let (acted, counts, applied, remaining) = (
        int("acted"),
        text("counts"),
        int("recoverable_applied"),
        int("recoverable_remaining"),
    );
    println!("watchdog report: {counts} acted={acted} recoverable_applied={applied} recoverable_remaining={remaining}");
    let detail =
        format!("{counts} recoverable_applied={applied} recoverable_remaining={remaining}");
    t.live
        .emitter
        .row("watchdog", interval, acted, None, Some(&detail));
    Ok(())
}

/// The bounce that just sent this SIGTERM, from its receipt. A hand kill, or a
/// sidecar older than 15s from an earlier bounce, must not blame the wrong cure.
fn bounce_sender(state_root: &Path) -> String {
    let read = || -> Option<String> {
        let raw: Value = serde_json::from_str(
            &std::fs::read_to_string(state_root.join("pr-watch-bounce.json")).ok()?,
        )
        .ok()?;
        (now_secs() - raw.get("ts")?.as_f64()? <= 15.0).then(|| {
            let parent = raw.get("parent").and_then(Value::as_str).unwrap_or("");
            format!(
                "{} pid {} via {parent}",
                raw["caller"].as_str().unwrap_or(""),
                raw["pid"]
            )
            .trim_end()
            .to_string()
        })
    };
    read().unwrap_or_else(|| "unrecorded".to_string())
}

static SIGNAL_PIPE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_sigterm(_: libc::c_int) {
    let fd = SIGNAL_PIPE.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        // SAFETY: write(2) is async-signal-safe; one byte wakes the thread.
        unsafe { libc::write(fd, b"x".as_ptr().cast(), 1) };
    }
}

/// A bootout kills this process by signal; without this the tick dies with no
/// record. The handler only writes a byte; the record and the re-raise run on
/// a thread, because neither is async-signal-safe.
fn arm_sigterm(journal: PathBuf, state_root: PathBuf, interval: i64, started: Instant) {
    let mut fds = [0; 2];
    // SAFETY: fds is a live two-int array.
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return;
    }
    SIGNAL_PIPE.store(fds[1], std::sync::atomic::Ordering::SeqCst);
    // SAFETY: the handler is async-signal-safe.
    unsafe { libc::signal(libc::SIGTERM, on_sigterm as *const () as libc::sighandler_t) };
    std::thread::spawn(move || {
        let mut byte = [0u8; 1];
        // SAFETY: reads one byte from this function's own pipe.
        if unsafe { libc::read(fds[0], byte.as_mut_ptr().cast(), 1) } <= 0 {
            return;
        }
        clock::kill_active_child();
        let emitter = Emitter { journal };
        let (phase, sender) = (clock::phase(), bounce_sender(&state_root));
        emitter.emit(
            "pr_watch_tick_end",
            json!({"outcome": "error", "why": "killed",
                   "duration_s": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                   "phase": phase, "pid": std::process::id(), "sender": sender}),
        );
        let detail = format!("killed by a signal mid-tick; started and did not complete, phase={phase}; sender={sender}");
        emitter.row("pr_watch_merge", interval, 0, Some("error"), Some(&detail));
        // SAFETY: restores the default action and re-raises at this process.
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    });
}

/// The wall backstop: phases sum under the ceiling and children die with their
/// slice, so reaching this means a hang inside this process, which no slice can
/// cut. It closes the attempt record and exits, so launchd can run the next tick.
fn arm_backstop(journal: PathBuf, ceiling: f64, started: Instant) {
    std::thread::spawn(move || {
        let bound = std::time::Duration::from_secs_f64(ceiling + BACKSTOP_GRACE_S);
        std::thread::sleep(bound.saturating_sub(started.elapsed()));
        clock::kill_active_child();
        let phase = clock::phase();
        println!("pr-watch tick: deadline exceeded in phase {phase} - aborted");
        Emitter { journal }.emit(
            "pr_watch_tick_end",
            json!({"outcome": "timeout", "why": "deadline_exceeded",
                   "duration_s": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
                   "phase": phase, "pid": std::process::id(), "saturated": []}),
        );
        std::process::exit(TIMEOUT_EXIT);
    });
}

pub fn run(args: &[String]) -> i32 {
    if !args.is_empty() {
        eprintln!("usage: fno-agents pr-watch tick (no arguments)");
        return 2;
    }
    let started = Instant::now();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let state_root = super::status::state_root(&cwd);
    let journal = crate::pr_park::Paths::resolve(&cwd).events;
    let emitter = Emitter {
        journal: journal.clone(),
    };
    clock::set_phase("entry");
    // Before anything that can hang, so a tick that dies in bootstrap is still
    // attributable. NOT the liveness watermark: only a completed sweep mints it.
    emitter.emit(
        "pr_watch_tick_attempt",
        json!({"pid": std::process::id(), "phase": "entry"}),
    );

    clock::set_phase("settings");
    let settings_start = Instant::now();
    let interval = int_cfg(&cwd, &["pr_watch", "interval_seconds"], 600).max(1);
    let configured = match cfg_lookup(&cwd, &["pr_watch", "tick_timeout_seconds"]) {
        Some(toml::Value::Integer(i)) if i > 0 => Some(i),
        _ => None,
    };
    arm_sigterm(journal.clone(), state_root.clone(), interval, started);
    let autonomy = crate::backlog::advance_settings::autonomy_master_enabled(Some(&cwd));
    let ceiling = resolve_deadline(interval, configured) as f64;
    arm_backstop(journal.clone(), ceiling, started);
    let (mut grant_timeout, mut merge_cap) = (GRANT_QUEUE_READ_TIMEOUT_S, MERGE_CAP_S);
    let mut load = [0.0_f64; 1];
    // SAFETY: load is a live one-element array.
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
    if unsafe { libc::getloadavg(load.as_mut_ptr(), 1) } == 1 && ceiling >= 480.0 {
        (grant_timeout, merge_cap) =
            merge_budget_for_load(load[0] / cores, dispatch::MERGE_FLOOR_S + 15.0);
    }
    let mut tick = Tick {
        started,
        live: Live {
            emitter,
            cwd: cwd.display().to_string(),
        },
        interval,
        max_age_days: int_cfg(&cwd, &["pr_watch", "max_age_days"], 14),
        retries: int_cfg(&cwd, &["pr_watch", "retries"], 3),
        graphql_min: int_cfg(&cwd, &["pr_watch", "graphql_min_remaining"], 200),
        enabled: as_bool(cfg_lookup(&cwd, &["pr_watch", "enabled"]), false) && autonomy,
        autonomy,
        ceiling: Some(ceiling),
        merge_cap,
        grant_timeout,
        roots: None,
        result: None,
        tick_failed: None,
        cut: Vec::new(),
        phase_s: Map::new(),
        watchdog_interval: interval,
        cwd,
    };
    let seconds = (settings_start.elapsed().as_secs_f64() * 10.0).round() / 10.0;
    tick.phase_s.insert("settings".to_string(), json!(seconds));

    // Value order, not cost order: behind a fleet-loaded sweep the merge read
    // its queue and executed nothing. The merge reads durable state, so it runs
    // first; the sweep is the one arm that resumes per-PR across ticks.
    tick.run_phase("merge", Some("pr_watch_merge"), 1, 0, phase_merge);
    tick.run_phase("sweep", Some("pr_watch_sweep"), 1, 0, phase_sweep);
    tick.run_phase("notify_watch", Some("notify_watch"), 1, 0, phase_notify);
    tick.run_phase("heal", None, 1, 0, phase_heal);
    // The fleet tail: the PR lane never pays all three p90s in one tick.
    tick.run_phase("stranded", Some("stranded"), 3, 0, phase_stranded);
    tick.run_phase("recovery", Some("recovery"), 3, 1, phase_recovery);
    tick.run_phase("watchdog", Some("watchdog"), 3, 2, phase_watchdog);

    let outcome = outcome_word(tick.result.as_ref(), tick.tick_failed.is_some());
    let mut end = json!({
        "outcome": outcome,
        "duration_s": (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0,
        "phase": tick.cut.first().cloned().unwrap_or_else(clock::phase),
        "pid": std::process::id(),
    });
    if !tick.cut.is_empty() {
        end["cut"] = json!(tick.cut);
    }
    if !tick.phase_s.is_empty() {
        end["phase_s"] = Value::Object(tick.phase_s.clone());
    }
    // Saturated = the phase spent its slice; a phase cut before its body ran
    // spent zero, so cut names it and this does not.
    let spent = |name: &&String| {
        tick.phase_s
            .get(*name)
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
            > 0.0
    };
    end["saturated"] = json!(tick.cut.iter().filter(spent).collect::<Vec<_>>());
    if let Some(result) = &tick.result {
        end["sweep_failures"] = json!(result.sweep_failures);
        if result.quota_skip {
            end["quota_remaining"] = json!(result.quota_remaining);
            end["quota_reset"] = json!(result.quota_reset);
        }
        if result.quota_unknown {
            end["quota_unknown"] = json!(true);
        }
    }
    tick.live.emit("pr_watch_tick_end", end);
    i32::from(tick.tick_failed.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deadline_stays_under_the_interval_and_the_env_seam_is_unclamped() {
        let _guard = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_TICK_TIMEOUT);
        assert_eq!(resolve_deadline(600, None), 510);
        assert_eq!(resolve_deadline(600, Some(9999)), 595);
        assert_eq!(resolve_deadline(600, Some(100)), 100);
        assert_eq!(resolve_deadline(30, None), 25);
        std::env::set_var(ENV_TICK_TIMEOUT, "9000");
        assert_eq!(resolve_deadline(600, Some(100)), 9000);
        std::env::remove_var(ENV_TICK_TIMEOUT);
    }

    #[test]
    fn the_outcome_word_names_why_a_tick_did_no_work() {
        let held = TickResult {
            lock_held: true,
            ..Default::default()
        };
        let degraded = TickResult {
            sweep_failures: 2,
            ..Default::default()
        };
        assert_eq!(outcome_word(None, false), "error");
        assert_eq!(outcome_word(Some(&TickResult::default()), true), "error");
        assert_eq!(outcome_word(Some(&held), false), "lock_held");
        assert_eq!(outcome_word(Some(&degraded), false), "degraded");
        assert_eq!(outcome_word(Some(&TickResult::default()), false), "ok");
    }

    #[test]
    fn merge_budget_grows_with_load_inside_the_window() {
        let (timeout, cap) = merge_budget_for_load(1.0, 165.0);
        assert_eq!((timeout, cap), (90.0, 260.0));
        let (timeout, _) = merge_budget_for_load(100.0, 165.0);
        assert_eq!(timeout, 195.0);
    }

    #[test]
    fn a_cut_child_dies_and_reads_as_a_cut() {
        clock::set_deadline(Some(Instant::now() + std::time::Duration::from_millis(100)));
        let args = vec!["5".to_string()];
        let answer = clock::run(Call {
            program: std::ffi::OsStr::new("sleep"),
            args: &args,
            stdin: None,
            cwd: None,
            env: &[],
            timeout_s: 30.0,
        });
        clock::set_deadline(None);
        assert_eq!(answer.err(), Some(DoorError::Cut));
    }
}
