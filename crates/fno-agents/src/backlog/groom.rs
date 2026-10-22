//! Native daily grooming pass and its scheduler agent.
//!
//! ONE entry point owns the whole pass: the mechanical legs (archive,
//! reconcile, maintain, relatedness) run under the daily claim, then ONE
//! judgment worker is dispatched with a fixed allowlist of reversible levers
//! and mails a one-screen report. This module owns sequencing and daily dedup
//! only; every judgment call lives in the groom skill brief.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{NaiveDate, Utc};
use serde_json::{json, Map, Value};

use super::render::{py_json_compact, py_json_pretty};
use super::settings;
use crate::pr_watch::refresh;

const MODEL_DEFAULT: &str = "claude-sonnet-5";
const AGE_DEFAULT: i64 = 14;
const KEY_PREFIX: &str = "groom";
/// The marker must outlive the dispatching process: a TTL claim whose pid is
/// dead but whose clock has not lapsed reads SUSPECT, which acquire treats as
/// held. That is what keeps "once a day" true after this process is gone.
const TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// The headless worker is synchronous, so this bounds the whole judgment
/// pass. It rides `spawn --timeout` so the RUNNER bounds the worker: killing
/// our own child would reap only the spawn wrapper and leave the harness
/// grandchild mutating the graph. The outer bound is larger so the inner one
/// always fires first.
const WORKER_TIMEOUT_S: u64 = 1800;
const SPAWN_TIMEOUT_S: u64 = WORKER_TIMEOUT_S + 120;
const LEG_TIMEOUT_S: u64 = 600;
/// Exit 4 is a PARTIAL result, never "nothing to do": every site that raises
/// it in the backlog CLI is a degraded outcome.
const PARTIAL_EXIT: i32 = 4;
/// Past this the daily cadence is presumed dead. Deliberately > 24h so a
/// healthy agent that merely slipped a night is not treated as broken.
const STALE_HOURS: f64 = 48.0;
const LABEL: &str = "sh.fno.groom";
/// Local time; the UTC-keyed claim, not the clock, enforces once a day.
const HOUR_DEFAULT: i64 = 2;

const HELP: &str = "Usage: fno backlog groom [OPTIONS]

  Run today's grooming pass over the backlog (at most once a day).

  One pipeline: the mechanical legs (archive, reconcile, maintain,
  relatedness) run first under the daily claim, then ONE worker makes the
  judgment calls. Judgment is levers-only: the worker may supersede,
  defer/undefer, re-prioritize, promote, and file ideas, and mails a
  one-screen report of every mutation with its receipt. A second run on the
  same UTC day exits 0 with an `already-ran` receipt, running nothing at all.

Options:
  -m, --model TEXT   Model for the groom worker (default: sonnet).
  -N, --dry-run      Print the brief and day key without dispatching.
  --age INTEGER      Archive-leg age gate in days (default: 14).
  --install-agent    Install the daily LaunchAgent and exit (macOS).
  --refresh-agent    Re-render an installed LaunchAgent onto the current
                     binary and exit.
  --hour INTEGER     Local hour for --install-agent (default: 2).
  --check            Report grooming freshness and exit 0 only if a pass is
                     due. Runs nothing.
  -h, --help         Show this message and exit.";

const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!--
  Daily backlog grooming pass: mechanical legs then one Sonnet judgment worker.
  Firing twice is harmless - the UTC-day claim makes the second run a no-op.
-->
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>

  <key>ProgramArguments</key>
  <array>
    <string>{fno_binary}</string>
    <string>backlog</string>
    <string>groom</string>
  </array>

  <!-- launchd launches with a minimal PATH; capture install-time PATH so
       fno / gh / claude resolve without a login shell. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>{path}</string>
    <key>HOME</key>
    <string>{home}</string>
  </dict>

  <key>StartCalendarInterval</key>
  <dict>
    <key>Hour</key>
    <integer>{hour}</integer>
    <key>Minute</key>
    <integer>0</integer>
  </dict>

  <key>RunAtLoad</key>
  <false/>

  <key>ProcessType</key>
  <string>Standard</string>

  <!-- A repo root, not $HOME: maintain's validity sweep resolves its git
       evidence root from the cwd, and from a non-repo it records every symbol
       "unavailable" while the leg still reports ok. -->
  <key>WorkingDirectory</key>
  <string>{workdir}</string>

  <key>StandardOutPath</key>
  <string>{log_out}</string>

  <key>StandardErrorPath</key>
  <string>{log_err}</string>
</dict>
</plist>
"#;

// -- config + freshness -------------------------------------------------------

/// `config.groom.enabled`, default true (the spawner always ran before the
/// gate existed, so a garbage value fails safe to true), behind the
/// `config.autonomy.enabled` master switch.
fn groom_enabled() -> bool {
    if !super::advance_settings::autonomy_master_enabled(None) {
        return false;
    }
    let doc = super::advance_settings::load_merged(None);
    match doc.get("groom").and_then(|g| g.get("enabled")) {
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64() == Some(1),
        Some(Value::String(s)) => {
            matches!(
                s.trim().to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        }
        _ => true,
    }
}

fn day_key(day: NaiveDate) -> String {
    format!("{KEY_PREFIX}:{}", day.format("%Y-%m-%d"))
}

/// The three freshness states. `Unknown` is load-bearing: folding an
/// unreadable claims root into `Never` would fire the fallback on every
/// session.
#[derive(Debug, PartialEq)]
enum Freshness {
    Ran(f64),
    Never,
    Unknown,
}

/// Hours since the last pass, read from its own claim marker: the daily claim
/// IS the run receipt. Stale rows count too, since an expired marker still
/// dates the last pass until a reap retires it.
fn staleness(now_s: f64) -> Freshness {
    let Some(dir) = crate::claims::global_claims_dir() else {
        return Freshness::Unknown;
    };
    let rows = match crate::claims::list_in(&[dir], Some(&format!("{KEY_PREFIX}:")), true) {
        Ok(rows) => rows,
        Err(_) => return Freshness::Unknown,
    };
    match rows.iter().map(|r| r.acquired_at).max() {
        None => Freshness::Never,
        // Clock skew reads as fresh, never negative.
        Some(ms) => Freshness::Ran(((now_s - ms as f64 / 1000.0) / 3600.0).max(0.0)),
    }
}

/// An unknown freshness stays dormant on purpose: an unreadable root must
/// not be an invitation to run on every session.
fn is_due(freshness: &Freshness) -> bool {
    match freshness {
        Freshness::Never => true,
        Freshness::Ran(hours) => *hours > STALE_HOURS,
        Freshness::Unknown => false,
    }
}

// -- the pass -----------------------------------------------------------------

/// The worker seed. Thin on purpose: the skill carries the contract. The
/// mechanical outcomes ride along because the worker is the only thing that
/// reaches a human; the receipt goes to a scheduler log nobody reads.
fn brief(day: &str, mechanical: Option<&[(String, String)]>, unproven: &[String]) -> String {
    let legs = match mechanical {
        Some(legs) if !legs.is_empty() => {
            let itemized: Vec<String> = legs.iter().map(|(n, o)| format!("{n} {o}")).collect();
            format!(
                "\n\nToday's mechanical pass already ran: {}.\nReport these verbatim as the \
                 leading Mechanical line, naming every leg. Any leg that did not come back \
                 `ok` is an anomaly: say so plainly in the report - that line is the only \
                 signal an operator gets.",
                itemized.join(", ")
            )
        }
        _ => String::new(),
    };
    let flags = if unproven.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nSuperseded nodes whose scope may have left the board: the successor shipped \
             with coverage unproven, or the leftover that held the rest was parked. \
             List each in the report. Where the old node's done condition still fails, file \
             the scope left out with `fno backlog idea`:\n- {}",
            unproven.join("\n- ")
        )
    };
    format!(
        "Daily backlog grooming pass for {day}.{legs}{flags}\n\nLoad the `fno:groom` skill and follow \
         it end to end. Use ONLY the levers it allowlists - never edit graph files directly. \
         Anything the patterns do not support goes to the triage pile as a one-line question. \
         Finish by mailing the one-screen report."
    )
}

/// Read after the mechanical legs, so reconcile has already stamped every
/// supersession a merged PR proved. An unreadable store flags nothing: the
/// brief still dispatches and the worker reads the graph itself.
fn unproven_supersessions() -> Vec<String> {
    let rows = crate::graph_store::read_rows(&settings::graph_path()).unwrap_or_default();
    let text = |row: &Value, key: &str, none: &str| row[key].as_str().unwrap_or(none).to_string();
    let mut lines: Vec<String> = super::supersession::shipped_successor_unproven(&rows, &rows)
        .iter()
        .map(|row| {
            let why = if row["recorded"] == json!(true) {
                format!(
                    "reason: {}; uncovered: {}",
                    text(row, "reason", "no reason recorded"),
                    row["uncovered_surfaces"]
                )
            } else {
                "no reason or coverage was ever recorded".to_string()
            };
            format!(
                "{} (successor {} shipped; {why})",
                text(row, "predecessor", "?"),
                text(row, "successor", "?")
            )
        })
        .collect();
    lines.extend(super::supersession::dead_leftovers(&rows));
    lines
}

/// The mechanical pass, in dependency order. `relatedness build` runs LAST
/// so the map reflects the post-groom graph. `--no-validity` is load-bearing:
/// the validity sweep watermarks the pile it reviews, so running it here
/// would leave the worker's own `maintain` read with nothing to report.
fn mechanical_legs(age: i64) -> Vec<(&'static str, Vec<String>)> {
    let argv = |words: &[&str]| words.iter().map(|w| w.to_string()).collect::<Vec<_>>();
    vec![
        (
            "archive",
            argv(&["archive", "--apply", "--older-than-days", &age.to_string()]),
        ),
        ("reconcile", argv(&["reconcile"])),
        ("maintain", argv(&["maintain", "--apply", "--no-validity"])),
        ("relatedness", argv(&["relatedness", "build"])),
    ]
}

fn leading_count(line: &str, prefix: &str, suffix: &str) -> Option<i64> {
    let rest = line.strip_prefix(prefix)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || !rest[digits.len()..].starts_with(suffix) {
        return None;
    }
    digits.parse().ok()
}

/// Bare "ok" cannot tell a run that archived 500 nodes from one that
/// archived zero, so the archive leg names both counts.
fn archive_leg_outcome(stdout: &str) -> String {
    let moved = stdout
        .lines()
        .find_map(|l| leading_count(l, "Archived ", " terminal node(s)"))
        .unwrap_or(0);
    let held: i64 = stdout
        .lines()
        .filter_map(|l| {
            let rest = l.strip_prefix("  held back (")?;
            let (reason, count) = rest.split_once("): ")?;
            let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
            if reason.is_empty() || !reason.chars().all(word) {
                return None;
            }
            (!count.is_empty() && count.chars().all(|c| c.is_ascii_digit()))
                .then(|| count.parse::<i64>().ok())
                .flatten()
        })
        .sum();
    format!("ok (archived {moved}, held back {held})")
}

fn reconcile_leg_outcome(stdout: &str) -> String {
    let healed = stdout
        .lines()
        .find_map(|l| leading_count(l, "re-parented ", " stranded child"))
        .unwrap_or(0);
    format!("ok (re-parented {healed})")
}

/// Run one leg through this binary's own backlog door, captured and bounded:
/// the receipt must stay one JSON document, and one wedged leg must not cost
/// the night the other three.
fn run_leg(name: &str, args: &[String]) -> String {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => return format!("failed: OSError: {}", clip(&err.to_string(), 120)),
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("backlog").args(args);
    let started = Instant::now();
    let out = match crate::bounded_cmd::output_with_timeout_result(cmd, LEG_TIMEOUT_S) {
        Ok(out) => out,
        Err(err) => return format!("failed: OSError: {}", clip(&err.to_string(), 120)),
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    let Some(code) = out.status.code() else {
        if started.elapsed() >= Duration::from_secs(LEG_TIMEOUT_S) {
            return format!("failed: TimeoutExpired: timed out after {LEG_TIMEOUT_S} seconds");
        }
        return "failed: killed by a signal".to_string();
    };
    if code == 0 {
        return match name {
            "archive" => archive_leg_outcome(&stdout),
            "reconcile" => reconcile_leg_outcome(&stdout),
            _ => "ok".to_string(),
        };
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let raw = if stderr.is_empty() { stdout } else { stderr };
    let detail = raw.trim().replace('\n', " ");
    let label = if code == PARTIAL_EXIT {
        "partial"
    } else {
        "failed"
    };
    format!("{label}: {code}: {}", clip(&detail, 120))
}

fn run_mechanical(age: i64) -> Vec<(String, String)> {
    mechanical_legs(age)
        .into_iter()
        .map(|(name, args)| (name.to_string(), run_leg(name, &args)))
        .collect()
}

/// Prefix match, not equality: a green archive leg carries counts.
fn leg_trouble(mechanical: &[(String, String)]) -> Vec<String> {
    let mut names: Vec<String> = mechanical
        .iter()
        .filter(|(_, outcome)| !outcome.starts_with("ok"))
        .map(|(name, _)| name.clone())
        .collect();
    names.sort();
    names
}

fn clip(text: &str, n: usize) -> String {
    text.chars().take(n).collect()
}

/// Why a dispatch failed: `Launch` means no worker ran, so the day can be
/// handed back; `Ran` means the worker may have pulled levers already.
#[derive(Debug)]
enum SpawnError {
    Launch(String),
    Ran(String),
}

/// Launch the one-shot headless groom worker; return its spawn short_id.
/// The headless substrate is explicit: a one-shot pass wants no pane and no
/// placement prompt.
fn spawn_worker(brief: &str, cwd: &str, model: &str, day: &str) -> Result<String, SpawnError> {
    // A naming refusal degrades to the legacy literal rather than aborting
    // the pass the worker exists to run.
    let name = crate::naming::dispatch_agent_name(
        Some("gr"),
        "th",
        "backlog",
        Some(day),
        None,
        None,
        None,
    )
    .unwrap_or_else(|_| format!("groom-{day}"));
    let mut cmd = std::process::Command::new(crate::scrape::fno_bin());
    cmd.args([
        "agents",
        "spawn",
        "--harness",
        "claude",
        "--once",
        "--cwd",
        cwd,
        "--timeout",
        &WORKER_TIMEOUT_S.to_string(),
        "--name",
        &name,
        brief,
        // The model is claude's own flag past the fence: spawn refuses --model.
        "--",
        "--model",
        model,
    ]);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, SPAWN_TIMEOUT_S)
        .map_err(|e| SpawnError::Launch(e.to_string()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match out.status.code() {
        Some(0) => {}
        None => {
            return Err(SpawnError::Ran(format!(
                "fno agents spawn killed after {SPAWN_TIMEOUT_S}s"
            )))
        }
        Some(code) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let raw = if stderr.is_empty() { &stdout } else { &stderr };
            return Err(SpawnError::Ran(format!(
                "fno agents spawn exited {code}: {}",
                super::advance::gate_refusal_detail(raw)
            )));
        }
    }
    let short_id = stdout
        .lines()
        .filter(|l| l.contains("\"short_id\""))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|v| match v.get("short_id")? {
            Value::String(s) if !s.is_empty() => Some(s.clone()),
            Value::Number(n) => Some(n.to_string()),
            _ => None,
        });
    Ok(short_id.unwrap_or_else(|| "unknown".to_string()))
}

fn legs_object(legs: &[(String, String)]) -> Value {
    Value::Object(
        legs.iter()
            .map(|(name, outcome)| (name.clone(), json!(outcome)))
            .collect::<Map<String, Value>>(),
    )
}

struct GroomRun<'a> {
    cwd: &'a str,
    model: &'a str,
    day: NaiveDate,
    dry_run: bool,
    age: i64,
}

type Legs<'a> = &'a dyn Fn(i64) -> Vec<(String, String)>;
type Spawn<'a> = &'a dyn Fn(&str, &str, &str, &str) -> Result<String, SpawnError>;

/// Today's pass end to end, at most once per UTC day. The mechanical legs
/// run under the claim (so a same-day rerun skips them too) and BEFORE the
/// worker, which re-derives its proposals from the post-mechanical graph.
/// Never panics: a grooming pass is hygiene, so a failed dispatch reports.
fn groom_pass(run: &GroomRun<'_>, legs: Legs<'_>, spawn: Spawn<'_>) -> Value {
    let key = day_key(run.day);
    let day = run.day.format("%Y-%m-%d").to_string();
    if !run.dry_run && !groom_enabled() {
        return json!({"status": "disabled", "day": day, "key": key,
                      "detail": "config.groom.enabled is false"});
    }
    if run.dry_run {
        let pending: Vec<(String, String)> = mechanical_legs(run.age)
            .into_iter()
            .map(|(name, _)| (name.to_string(), "pending".to_string()))
            .collect();
        return json!({"status": "dry-run", "day": day, "key": key, "model": run.model,
                      "brief": brief(&day, None, &unproven_supersessions()),
                      "mechanical": legs_object(&pending)});
    }
    let holder = format!("groom:{}", std::process::id());
    // Read before acquiring: acquire is idempotent for the SAME holder, so a
    // second call from one process would re-acquire and re-dispatch.
    if matches!(
        crate::claims::status(&key, None).0,
        crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
    ) {
        return json!({"status": "already-ran", "day": day, "key": key});
    }
    let opts = crate::claims::AcquireOpts {
        ttl_ms: Some(TTL_MS),
        reason: Some(format!("daily grooming pass {day}")),
        ..Default::default()
    };
    match crate::claims::acquire(&key, &holder, opts) {
        crate::claims::AcquireOutcome::Acquired(_) => {}
        // A normal dedup race, not a claims fault: never a false "failed".
        crate::claims::AcquireOutcome::HeldByOther { .. } => {
            return json!({"status": "already-ran", "day": day, "key": key});
        }
        crate::claims::AcquireOutcome::Error(err) => {
            return json!({"status": "failed", "day": day,
                          "detail": format!("claim: {}", clip(&err, 200))});
        }
    }
    let mechanical = legs(run.age);
    let seed = brief(&day, Some(&mechanical), &unproven_supersessions());
    match spawn(&seed, run.cwd, run.model, &day) {
        Err(SpawnError::Launch(detail)) => {
            // No worker ran and no lever was pulled: hand the day back, and
            // report whether the handback happened. A leaked marker would
            // make every retry today exit 0 as already-ran.
            let mut receipt = json!({"status": "failed", "day": day,
                                     "detail": clip(&detail, 200),
                                     "mechanical": legs_object(&mechanical)});
            match crate::claims::release_with_receipt(&key, &holder, None, None) {
                Ok(Some(_)) => receipt["released"] = json!(true),
                Ok(None) => {
                    receipt["released"] = json!(false);
                    receipt["release_error"] = json!(format!("{key} is not held by {holder}"));
                }
                Err(err) => {
                    receipt["released"] = json!(false);
                    receipt["release_error"] = json!(clip(&err, 200));
                }
            }
            receipt
        }
        // A timeout or a non-zero exit both mean the worker may have RUN and
        // applied levers: hold the marker. Burning one day of hygiene is
        // cheaper than double-mutating the graph.
        Err(SpawnError::Ran(detail)) => json!({"status": "failed", "day": day,
                                               "detail": clip(&detail, 200),
                                               "released": false,
                                               "mechanical": legs_object(&mechanical)}),
        Ok(short_id) => {
            let trouble = leg_trouble(&mechanical);
            let mut receipt = json!({
                "status": if trouble.is_empty() { "dispatched" } else { "degraded" },
                "day": day,
                "short_id": short_id,
                "model": run.model,
                "mechanical": legs_object(&mechanical),
            });
            if !trouble.is_empty() {
                receipt["degraded_legs"] = json!(trouble);
            }
            // The worker launched but `fno agents logs` has no handle.
            if short_id == "unknown" {
                receipt["short_id_lost"] = json!(true);
            }
            receipt
        }
    }
}

// -- daily cadence (macOS LaunchAgent) ----------------------------------------

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn launch_agents_dir() -> PathBuf {
    home().join("Library").join("LaunchAgents")
}

fn which(name: &str) -> Option<String> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| {
            use std::os::unix::fs::PermissionsExt;
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
        .map(|p| p.display().to_string())
}

/// Render the daily groom LaunchAgent plist. No filesystem writes.
fn render_plist(fno_binary: &str, install_path: &str, hour: i64, workdir: Option<&str>) -> String {
    let home = home();
    let home_s = home.display().to_string();
    let logs = home.join(".fno").join("logs");
    let esc = refresh::xml_escape;
    PLIST
        .replace("{label}", &esc(LABEL))
        .replace("{fno_binary}", &esc(fno_binary))
        .replace("{path}", &esc(&refresh::augment_path(install_path)))
        .replace("{home}", &esc(&home_s))
        .replace("{workdir}", &esc(workdir.unwrap_or(&home_s)))
        .replace("{hour}", &hour.to_string())
        .replace(
            "{log_out}",
            &esc(&logs.join("groom.out.log").display().to_string()),
        )
        .replace(
            "{log_err}",
            &esc(&logs.join("groom.err.log").display().to_string()),
        )
}

/// The repo root the scheduled run works from: the `FNO_REPO_ROOT` pin, else
/// the checkout around the cwd.
fn repo_root() -> Option<String> {
    if let Some(pin) = std::env::var_os("FNO_REPO_ROOT").filter(|v| !v.is_empty()) {
        let pin = PathBuf::from(pin);
        return Some(pin.canonicalize().unwrap_or(pin).display().to_string());
    }
    let cwd = std::env::current_dir().ok()?;
    Some(crate::paths::worktree_repo_root(&cwd).display().to_string())
}

/// Write the plist and bootstrap it into launchd. Reuses the pr-watch bounce
/// rather than `launchctl load` (only a re-bootstrap clears the wedge an
/// update can leave), but WITHOUT its kickstart: a grooming tick mutates the
/// backlog and burns the day's claim, so forcing one would run a full pass
/// at install time against the plist's own `RunAtLoad=false`.
fn install_agent(
    agents_dir: &Path,
    fno_binary: Option<&str>,
    install_path: Option<&str>,
    hour: i64,
    workdir: Option<String>,
) -> Value {
    if !cfg!(target_os = "macos") {
        return json!({
            "status": "unsupported",
            "detail": "launchd is macOS-only; see docs/backlog-usage.md for the cron one-liner",
            "cron": format!("0 {hour} * * * {} backlog groom", which("fno").unwrap_or_else(|| "fno".into())),
        });
    }
    let fno_binary = fno_binary
        .map(String::from)
        .or_else(|| which("fno"))
        .unwrap_or_else(|| "fno".into());
    let install_path = install_path
        .map(String::from)
        .unwrap_or_else(|| refresh::default_agent_path(&fno_binary));
    let state = settings::state_dir().unwrap_or_else(|| home().join(".fno"));
    let _ = std::fs::create_dir_all(state.join("logs"));
    // Captured at install time: the scheduled run has no cwd of its own. A
    // refresh passes the installed value so it is not re-resolved against
    // whatever directory the update ran from.
    let workdir = workdir.or_else(repo_root);
    let plist_path = agents_dir.join(format!("{LABEL}.plist"));
    let rendered = render_plist(&fno_binary, &install_path, hour, workdir.as_deref());
    match refresh::write_if_changed(&plist_path, &rendered) {
        Ok(false) => {
            return json!({"status": "unchanged", "detail": "plist matches; not re-registered"})
        }
        Ok(true) => {}
        Err(err) => {
            return json!({"status": "failed",
                          "detail": format!("write {}: {err}", plist_path.display())})
        }
    }
    let (msg, rc) = refresh::bounce(&plist_path, LABEL, false, false, "unknown", &state);
    json!({
        "status": if rc == 0 { "installed" } else { "failed" },
        "plist": plist_path.display().to_string(),
        "hour": hour,
        "workdir": workdir,
        "detail": msg,
    })
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// The text of the first `<tag>...</tag>` after `from`, bounded by `until`.
fn element_after<'a>(text: &'a str, from: usize, tag: &str, until: usize) -> Option<&'a str> {
    let open = format!("<{tag}>");
    let start = text[from..until].find(&open)? + from + open.len();
    let end = text[start..until].find(&format!("</{tag}>"))? + start;
    Some(&text[start..end])
}

/// The installed plist's hour and working directory, so a refresh does not
/// silently reset an operator's chosen `--hour`. A hand-edited or corrupt
/// plist falls back to the defaults.
fn installed_agent_settings(plist_path: &Path) -> (Option<i64>, Option<String>) {
    let Ok(text) = std::fs::read_to_string(plist_path) else {
        return (None, None);
    };
    if !text.contains("<plist") {
        return (None, None);
    }
    let hour = text
        .find("<key>StartCalendarInterval</key>")
        .and_then(|at| {
            let dict_end = text[at..].find("</dict>").map_or(text.len(), |e| e + at);
            let key = text[at..dict_end].find("<key>Hour</key>")? + at;
            element_after(&text, key, "integer", dict_end)
        })
        .and_then(|n| n.trim().parse().ok());
    let workdir = text.find("<key>WorkingDirectory</key>").and_then(|at| {
        let next_key = text[at + 1..]
            .find("<key>")
            .map_or(text.len(), |e| e + at + 1);
        element_after(&text, at, "string", next_key).map(xml_unescape)
    });
    (hour, workdir)
}

/// Re-render the installed plist onto the current binary and re-bootstrap
/// it: the tail of `fno doctor update`. A no-op when no groom agent is
/// installed. Never fails: the update chain calls it best-effort.
fn refresh_agent(agents_dir: &Path) -> Value {
    let plist_path = agents_dir.join(format!("{LABEL}.plist"));
    if !cfg!(target_os = "macos") || !plist_path.exists() {
        return json!({"status": "skipped", "detail": "no groom agent installed"});
    }
    // Carry the installed schedule and cwd forward; only the binary and PATH
    // are meant to change on a refresh.
    let (hour, workdir) = installed_agent_settings(&plist_path);
    install_agent(
        agents_dir,
        None,
        None,
        hour.unwrap_or(HOUR_DEFAULT),
        workdir,
    )
}

// -- the verb -------------------------------------------------------------------

#[derive(Default)]
struct Args {
    model: Option<String>,
    dry_run: bool,
    age: Option<i64>,
    install_agent: bool,
    refresh_agent: bool,
    hour: Option<i64>,
    check: bool,
}

fn parse(tail: &[String]) -> Result<Option<Args>, String> {
    let mut args = Args::default();
    let mut i = 0;
    while i < tail.len() {
        let token = tail[i].as_str();
        let (flag, inline) = match token.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (token, None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            if let Some(v) = inline.clone() {
                return Ok(v);
            }
            i += 1;
            tail.get(i)
                .cloned()
                .ok_or_else(|| format!("Option '{name}' requires an argument."))
        };
        let int = |name: &str, raw: String| -> Result<i64, String> {
            raw.trim()
                .parse()
                .map_err(|_| format!("Invalid value for '{name}': '{raw}' is not a valid integer."))
        };
        match flag {
            "-h" | "--help" => return Ok(None),
            "-m" | "--model" => args.model = Some(value("--model")?),
            "-N" | "--dry-run" => args.dry_run = true,
            "--age" => args.age = Some(int("--age", value("--age")?)?),
            "--install-agent" => args.install_agent = true,
            "--refresh-agent" => args.refresh_agent = true,
            "--hour" => args.hour = Some(int("--hour", value("--hour")?)?),
            "--check" => args.check = true,
            other if other.starts_with('-') => return Err(format!("No such option: {other}")),
            other => return Err(format!("Got unexpected extra argument ({other})")),
        }
        i += 1;
    }
    Ok(Some(args))
}

/// `fno backlog groom`: the daily pass, its freshness probe, and its agent.
pub fn run_groom(tail: &[String]) -> i32 {
    let args = match parse(tail) {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{HELP}");
            return 0;
        }
        Err(message) => {
            eprintln!(
                "Usage: fno backlog groom [OPTIONS]\nTry 'fno backlog groom -h' for help.\n\nError: {message}"
            );
            return 2;
        }
    };
    if args.check {
        // The shell bridge for the SessionStart fallback: the exit code is
        // the answer; stdout is for a human reading the receipt.
        let now = Utc::now().timestamp_micros() as f64 / 1e6;
        let fresh = staleness(now);
        let (state, hours) = match &fresh {
            Freshness::Ran(h) => ("ran", json!(h)),
            Freshness::Never => ("never", Value::Null),
            Freshness::Unknown => ("unknown", Value::Null),
        };
        println!(
            "{}",
            py_json_compact(&json!({"state": state, "hours": hours}))
        );
        return if is_due(&fresh) { 0 } else { 1 };
    }
    if args.refresh_agent {
        // The tail of `fno doctor update`: never fails the update.
        println!("{}", py_json_pretty(&refresh_agent(&launch_agents_dir())));
        return 0;
    }
    if args.install_agent {
        let receipt = install_agent(
            &launch_agents_dir(),
            None,
            None,
            args.hour.unwrap_or(HOUR_DEFAULT),
            None,
        );
        println!("{}", py_json_pretty(&receipt));
        return i32::from(receipt["status"] == "failed");
    }
    // The pass owns graph state; --check and the agent flags above do not.
    if let Some(code) = super::workflows::refuse_non_graph_backend("groom") {
        return code;
    }
    let cwd = std::env::current_dir()
        .map(|d| d.display().to_string())
        .unwrap_or_else(|_| ".".into());
    let model = args.model.as_deref().unwrap_or(MODEL_DEFAULT);
    let run = GroomRun {
        cwd: &cwd,
        model,
        day: Utc::now().date_naive(),
        dry_run: args.dry_run,
        age: args.age.unwrap_or(AGE_DEFAULT),
    };
    let receipt = groom_pass(&run, &run_mechanical, &spawn_worker);
    println!("{}", py_json_pretty(&receipt));
    // `degraded` exits non-zero too: a scheduler log nobody reads is not a
    // signal.
    i32::from(matches!(
        receipt["status"].as_str(),
        Some("failed" | "degraded")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_due_only_past_the_threshold_and_never_when_unknown() {
        assert!(is_due(&Freshness::Never));
        assert!(!is_due(&Freshness::Ran(6.0)));
        assert!(!is_due(&Freshness::Ran(STALE_HOURS)));
        assert!(is_due(&Freshness::Ran(STALE_HOURS + 1.0)));
        assert!(!is_due(&Freshness::Unknown));
    }

    #[test]
    fn the_daily_claim_runs_once_and_a_failed_launch_hands_the_day_back() {
        let _lock = crate::claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().display().to_string();
        let _claims = crate::claims::EnvVarGuard::set("FNO_CLAIMS_ROOT", &root);
        let _events = crate::claims::EnvVarGuard::set(
            "FNO_EVENTS_PATH",
            &dir.path().join("events.jsonl").display().to_string(),
        );
        let _config = crate::claims::EnvVarGuard::set(
            "FNO_CONFIG",
            &dir.path().join("absent.toml").display().to_string(),
        );
        // The brief reads the store for unproven supersessions.
        let _state = crate::claims::EnvVarGuard::set("FNO_STATE_DIR", &root);
        let spawned = std::cell::Cell::new(0);
        let legs = |_: i64| vec![("archive".to_string(), "ok".to_string())];
        let ok = |_: &str, _: &str, _: &str, _: &str| {
            spawned.set(spawned.get() + 1);
            Ok("gr01".to_string())
        };
        let launch_fails = |_: &str, _: &str, _: &str, _: &str| {
            Err(SpawnError::Launch("No such file or directory".into()))
        };
        let run = |day: u32, dry_run: bool| GroomRun {
            cwd: "/tmp",
            model: MODEL_DEFAULT,
            day: NaiveDate::from_ymd_opt(2026, 7, day).unwrap(),
            dry_run,
            age: AGE_DEFAULT,
        };

        let dry = groom_pass(&run(19, true), &legs, &ok);
        assert_eq!(dry["mechanical"]["relatedness"], json!("pending"));
        assert_eq!(
            groom_pass(&run(19, false), &legs, &ok)["status"],
            json!("dispatched")
        );
        assert_eq!(
            groom_pass(&run(19, false), &legs, &ok)["status"],
            json!("already-ran")
        );
        assert_eq!(spawned.get(), 1, "the second run must not spawn");

        let failed = groom_pass(&run(20, false), &legs, &launch_fails);
        assert_eq!(
            (failed["status"].clone(), failed["released"].clone()),
            (json!("failed"), json!(true))
        );
        assert_eq!(
            groom_pass(&run(20, false), &legs, &ok)["status"],
            json!("dispatched")
        );
        assert!(matches!(
            staleness(Utc::now().timestamp() as f64),
            Freshness::Ran(_)
        ));
    }
}
