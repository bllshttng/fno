//! The review-hold verb family, ported onto the `claim` verb.
//!
//! Ports `cli/src/fno/pr/_review_hold.py` (deleted in the same change) and
//! the verb body `pr/cli.py review_hold` carried. The hold is a TTL claim on
//! `review:branch:<branch>` plus the derived worktree probe: layer 1 sees
//! the window between "review dispatched" and "first edit", layer 2 covers
//! every review footnote never dispatched. Absence is never the clear
//! answer by itself: a missing hold clears only when the worktree probe
//! also RAN and answered. A dead instrument blocks.
//!
//! Sub-token of `fno-agents claim` (d-fe66560a: no new top-level verb):
//! `claim review-hold check|acquire|release|metadata|round`, with argv
//! flags mirroring the typer door one flag each, plus a stdin JSON door
//! (no argv) so `verb_call(["claim", "review-hold"], {...})` reaches every
//! action. Goldens: tests/golden/review_hold/ (Change 1).

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// A review is unbounded, so the TTL is a wedge bound rather than an
/// estimate: long enough that a real review never ages out under itself,
/// short enough that a crashed reviewer does not hold the lane for a
/// working day.
pub(crate) const DEFAULT_HOLD_TTL_MINUTES: i64 = 90;

/// Spelled by `fno::review_invocation::VERIFY_FIXES`; the cap gate exempts
/// a scoped fix-verification, which is the one review-shaped act the cap
/// still funds after the round cap is spent.
pub(crate) const VERIFY_FIXES_FLAG: &str = "--verify-fixes";

/// Minutes -> a TTL inside the claim bounds. Coerces, never raises: a
/// misconfigured TTL must not make the guard unreadable, and the recovery
/// from a clamped TTL is a config edit.
pub(crate) fn resolve_ttl_ms(minutes: Option<f64>) -> i64 {
    let minutes = minutes.unwrap_or(DEFAULT_HOLD_TTL_MINUTES as f64);
    let ms = if minutes.is_finite() {
        (minutes * 60_000.0) as i64
    } else {
        DEFAULT_HOLD_TTL_MINUTES * 60_000
    };
    crate::claims::MIN_TTL_MS.max(ms.min(crate::claims::MAX_TTL_MS))
}

/// One config leaf read twice: global `$HOME/.fno/config.toml` first, then
/// the project-local file's populated field wins. Fail-open (None) on any
/// unreadable file, matching the Python door's fallbacks.
fn config_leaf<T: Default + Copy>(
    cwd: &Path,
    pick: impl Fn(&crate::loopcheck::settings::Settings) -> Option<T>,
) -> Option<T> {
    let read = |path: &Path| -> Option<T> {
        let content = std::fs::read_to_string(path).ok()?;
        crate::loopcheck::settings::parse_settings_result(&content)
            .ok()
            .and_then(|settings| pick(&settings))
    };
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let global = read(&PathBuf::from(home).join(".fno/config.toml"));
    let local = read(&cwd.join(".fno/config.toml"));
    local.or(global)
}

/// `config.review.hold_ttl_minutes`, clamped. Falls back on any failure: a
/// config read must not gate a review.
pub(crate) fn configured_ttl_ms(cwd: &Path) -> i64 {
    resolve_ttl_ms(config_leaf(cwd, |s| s.hold_ttl_minutes_opt()))
}

/// The carried interdiff budget: `review.carry_interdiff_lines`, default
/// 100, read with the same global-then-local overlay.
fn configured_carry_interdiff(cwd: &Path) -> i64 {
    config_leaf(cwd, |s| s.carry_interdiff_opt()).unwrap_or(100)
}

// ── the chain read the round-cap surfaces share ─────────────────────────────

/// The attestation-chain answer, shared with the `review-coverage op chain`
/// server so the verb door and the merge gate read ONE chain implementation
/// (d-e11b2b3e: twins retire first; no second copy).
pub(crate) fn chain_answer(cwd: &Path, branch: &str, head: &str) -> Value {
    crate::loopcheck::coverage_gate::chain_answer(cwd, branch, head)
}

// ── the cap refusal and the advisory ────────────────────────────────────────

/// Why a NEW hunting round may not start, or `""` when it may. Carveouts,
/// both the law's own: `--verify-fixes` is a scoped fix-verification (not a
/// round), and a rebase delta at or over `review.carry_interdiff_lines`;
/// an unmeasurable interdiff fails open.
pub(crate) fn review_invocation_refusal(
    cwd: &Path,
    branch: &str,
    head: &str,
    flags: Option<&[String]>,
) -> String {
    let answer = chain_answer(cwd, branch, head);
    let rounds_used = answer
        .get("rounds_used")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let max_rounds = answer
        .get("max_rounds")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    if rounds_used < max_rounds {
        return String::new();
    }
    let exempt = flags.unwrap_or(&[]).iter().any(|f| f == VERIFY_FIXES_FLAG);
    if exempt {
        return String::new();
    }
    let line = refusal_after_last_row(cwd, head, answer.get("last"), rounds_used, max_rounds);
    let _ = branch;
    line
}

/// The rebase-carry carveout, spelled once. A head that moved off the last
/// reviewed commit stays refused only while the carried interdiff is
/// under budget; unmeasurable fails open.
fn refusal_after_last_row(
    cwd: &Path,
    head: &str,
    last: Option<&Value>,
    rounds_used: i64,
    max_rounds: i64,
) -> String {
    let last = last.cloned().unwrap_or(Value::Null);
    let last_head = last.get("head_sha").and_then(Value::as_str).unwrap_or("");
    if last_head.is_empty() || head.is_empty() || last_head == head {
        return cap_refusal_line(rounds_used, max_rounds);
    }
    let base = last
        .get("reviewed_base_sha")
        .and_then(Value::as_str)
        .unwrap_or("");
    if base.is_empty() {
        // No base to measure a carry against: the head move alone cannot
        // fund a fresh round, so the refusal stands (the Python leg's
        // fall-through).
        return cap_refusal_line(rounds_used, max_rounds);
    }
    let carry = configured_carry_interdiff(cwd);
    let measured =
        crate::review_freshness::interdiff_lines_between("git", cwd, base, last_head, head);
    if measured.is_none() || measured.is_some_and(|n| (n as i64) >= carry) {
        return String::new();
    }
    cap_refusal_line(rounds_used, max_rounds)
}

fn cap_refusal_line(rounds_used: i64, max_rounds: i64) -> String {
    format!(
        "review refused: the configured round cap is spent for this head ({rounds_used} of {max_rounds} rounds used); \
a new hunting round may not start. The review phase is complete: merge on green CI - open findings stay \
in the PR conversation. A scoped fix-verification with {VERIFY_FIXES_FLAG} is the one review-shaped act the cap still funds."
    )
}

/// The nudge a driver needs exactly when the default flow misleads: two
/// careful drivers both fixed findings and re-ran a full pass, spending the
/// round the law reserved for the verify. Advisory only - it never refuses
/// and never classifies the pass.
pub(crate) fn verify_fixes_advisory(cwd: &Path, branch: &str, head: &str) -> String {
    let answer = chain_answer(cwd, branch, head);
    let last = match answer.get("last") {
        Some(l) if l.is_object() => l.clone(),
        _ => return String::new(),
    };
    let last_head = last.get("head_sha").and_then(Value::as_str).unwrap_or("");
    if last_head.is_empty() || last_head == head {
        return String::new();
    }
    let blocking = last.get("blocking").and_then(Value::as_i64).unwrap_or(0);
    let detail = if blocking > 0 {
        format!("raised {blocking} blocking finding(s)")
    } else if last.get("findings_truncated").and_then(Value::as_bool) == Some(true) {
        "raised blocking finding(s) the record could not carry".to_string()
    } else {
        return String::new();
    };
    let short = last_head.get(..8).unwrap_or(last_head);
    format!(
        "review-hold: the last round at {short} {detail} and the head has moved.\n\
If this pass verifies those fixes, invoke it with {VERIFY_FIXES_FLAG}: a scoped \
fix-verification declares the round it verifies and does not spend a new one."
    )
}

/// The round a scoped fix-verification at `head` verifies. Floored at 1: a
/// verify with no round behind it still names a round.
pub(crate) fn scoped_verify_round(cwd: &Path, branch: &str, head: &str) -> i64 {
    let answer = chain_answer(cwd, branch, head);
    answer
        .get("rounds_used")
        .and_then(Value::as_i64)
        .unwrap_or(0)
        .max(1)
}

/// Acquire the hold. Never blocks a review from starting: an error prints
/// the not-registered receipt and the worktree layer still covers it.
fn acquire_hold(
    branch: &str,
    holder: &str,
    head: &str,
    ttl_ms: i64,
    metadata: serde_json::Map<String, Value>,
) -> bool {
    let reason = format!("review in flight at {}", head.get(..12).unwrap_or(head));
    let opts = crate::claims::AcquireOpts {
        ttl_ms: Some(ttl_ms),
        reason: Some(reason),
        metadata: Some(metadata),
        ..Default::default()
    };
    let key = format!("review:branch:{branch}");
    match crate::claims::acquire(&key, holder, opts) {
        crate::claims::AcquireOutcome::Acquired(_) => true,
        other => {
            let detail = match other {
                crate::claims::AcquireOutcome::HeldByOther { holder, .. } => {
                    format!("held by {holder}")
                }
                crate::claims::AcquireOutcome::Error(e) => e,
                _ => String::new(),
            };
            eprintln!("review-hold: could not register hold on {branch}: {detail}");
            false
        }
    }
}

/// Clear the branch's hold. Returns whether one was actually there: a lane
/// lock, not an ownership assertion. The status read (not the release
/// receipt) decides "was there one" - the honest answer on the one recovery
/// command there is.
fn release_hold(branch: &str, holder: Option<&str>) -> bool {
    let key = format!("review:branch:{branch}");
    let (state, record) = crate::claims::status(&key, None);
    match state {
        crate::claims::ClaimState::Free => false,
        crate::claims::ClaimState::Corrupted => {
            matches!(
                crate::claim_store::force_release(&key, "corrupted review hold", None, false),
                Ok(_)
            )
        }
        _ => {
            let holder = holder
                .map(str::to_string)
                .or_else(|| record.as_ref().map(|r| r.holder.clone()))
                .unwrap_or_default();
            crate::claims::release(&key, &holder, None, None).is_ok()
        }
    }
}

/// The gh REST read for a PR's head ref, oid and state. Tests pin
/// FNO_REVIEW_HOLD_GH; production resolves plain `gh`.
fn pr_head_ref_and_oid(cwd: &Path, pr: &str) -> Option<(String, String, String)> {
    let gh = std::env::var("FNO_REVIEW_HOLD_GH").unwrap_or_else(|_| "gh".to_string());
    let pr_json = crate::loopcheck::gh_read::read_pr_view(
        &gh,
        cwd,
        Some(pr),
        std::time::Duration::from_secs(10),
    )
    .ok()??;
    let branch = pr_json
        .get("headRefName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let head = pr_json
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let state = pr_json
        .get("state")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if branch.is_empty() || head.is_empty() {
        return None;
    }
    Some((branch, head, state.to_uppercase()))
}

// ── the verb body ────────────────────────────────────────────────────────────

/// The parsed argv of one `claim review-hold` invocation.
struct Args {
    action: String,
    pr: Option<String>,
    branch: Option<String>,
    head: String,
    holder: Option<String>,
    verb: String,
    invocation_id: String,
    args_raw: String,
    level: String,
    level_source: String,
    flags_json: Option<String>,
    ttl_minutes: Option<f64>,
    cwd: PathBuf,
}

impl Args {
    fn parse(rest: &[String], default_cwd: PathBuf) -> Self {
        let mut a = Args {
            action: String::new(),
            pr: None,
            branch: None,
            head: String::new(),
            holder: None,
            verb: String::new(),
            invocation_id: String::new(),
            args_raw: String::new(),
            level: String::new(),
            level_source: String::new(),
            flags_json: None,
            ttl_minutes: None,
            cwd: default_cwd,
        };
        let mut it = rest.iter();
        // The action word leads; positional PR number may follow.
        if let Some(first) = it.next() {
            if !first.starts_with('-') {
                a.action = first.clone();
            }
        }
        while let Some(tok) = it.next() {
            match tok.as_str() {
                "--branch" => a.branch = it.next().cloned(),
                "--head" => a.head = it.next().cloned().unwrap_or_default(),
                "--holder" => a.holder = it.next().cloned(),
                "--verb" => a.verb = it.next().cloned().unwrap_or_default(),
                "--invocation-id" => a.invocation_id = it.next().cloned().unwrap_or_default(),
                "--args-raw" => a.args_raw = it.next().cloned().unwrap_or_default(),
                "--level" => a.level = it.next().cloned().unwrap_or_default(),
                "--level-source" => a.level_source = it.next().cloned().unwrap_or_default(),
                "--flags-json" => a.flags_json = it.next().cloned(),
                "--ttl-minutes" => a.ttl_minutes = it.next().and_then(|v| v.parse().ok()),
                "--repo" => {
                    if let Some(dir) = it.next() {
                        a.cwd = PathBuf::from(dir);
                    }
                }
                other if !other.starts_with('-') => a.pr = Some(other.to_string()),
                _ => {}
            }
        }
        a
    }
}

/// `(exit, stdout, stderr)` for one `claim review-hold ...` run. The client
/// entry `run` prints and exits; tests capture.
pub fn run_capture(args: &[String]) -> (i32, String, String) {
    if args.is_empty() {
        return run_stdin_door();
    }
    let parsed = Args::parse(
        args,
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    );
    match parsed.action.as_str() {
        "check" => check_arm(&parsed),
        "release" => release_arm(&parsed),
        "metadata" => metadata_arm(&parsed),
        "round" => round_arm(&parsed),
        "acquire" => acquire_arm(&parsed),
        other => (
            1,
            String::new(),
            format!("unknown review-hold action: {other}\n"),
        ),
    }
}

/// Print stdout/stderr and return the exit code. The `claim` sub-token entry.
pub fn run(args: &[String]) -> i32 {
    let (code, stdout, stderr) = run_capture(args);
    if !stdout.is_empty() {
        print!("{stdout}");
    }
    if !stderr.is_empty() {
        eprint!("{stderr}");
    }
    code
}

fn check_arm(a: &Args) -> (i32, String, String) {
    let cwd = a.cwd.clone();
    let Some(pr) = &a.pr else {
        return (
            1,
            String::new(),
            "review-hold check needs a PR number\n".into(),
        );
    };
    let Some((branch, head, _state)) = pr_head_ref_and_oid(&cwd, pr) else {
        return (
            4,
            String::new(),
            format!(
                "PR {pr}: could not resolve the head branch; no in-flight review could be ruled out\n"
            ),
        );
    };
    let activity = crate::pr_status::reviews::review_activity(&cwd, &branch, &head);
    if activity
        .get("blocker")
        .and_then(Value::as_str)
        .is_some_and(|b| !b.is_empty())
    {
        let blocker = activity
            .get("blocker")
            .and_then(Value::as_str)
            .unwrap_or("");
        let detail = activity.get("detail").and_then(Value::as_str).unwrap_or("");
        return (3, String::new(), format!("{blocker}: {detail}\n"));
    }
    (
        0,
        format!("PR {pr}: no review in flight on {branch}\n"),
        String::new(),
    )
}

fn release_arm(a: &Args) -> (i32, String, String) {
    let Some(branch) = &a.branch else {
        return (
            1,
            String::new(),
            "review-hold release needs --branch\n".into(),
        );
    };
    let released = release_hold(branch, a.holder.as_deref());
    let line = if released {
        format!("review-hold: released {branch}\n")
    } else {
        format!("review-hold: no hold on {branch}\n")
    };
    (0, line, String::new())
}

fn metadata_arm(a: &Args) -> (i32, String, String) {
    let Some(branch) = &a.branch else {
        return (
            1,
            String::new(),
            "review-hold metadata needs --branch\n".into(),
        );
    };
    let key = format!("review:branch:{branch}");
    let (state, record) = crate::claims::status(&key, None);
    let mut payload = match &record {
        Some(r) => serde_json::to_value(r).unwrap_or_else(|_| json!({})),
        None => json!({}),
    };
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("key".into(), json!(key));
        obj.insert("state".into(), json!(state.as_str()));
    }
    (0, format!("{payload}\n"), String::new())
}

fn round_arm(a: &Args) -> (i32, String, String) {
    if a.head.is_empty() {
        return (1, String::new(), "review-hold round needs --head\n".into());
    }
    let Some(branch) = &a.branch else {
        return (
            1,
            String::new(),
            "review-hold round needs --branch\n".into(),
        );
    };
    // The instrument must have RUN: an empty chain is an absence with more
    // than one explanation, and journals missing on both mirrors means no
    // read ever answered. Fail closed so the caller stamps nothing.
    if !review_journals_present(&a.cwd) {
        return (1, String::new(), String::new());
    }
    let round = scoped_verify_round(&a.cwd, branch, &a.head);
    (0, format!("{round}\n"), String::new())
}

fn acquire_arm(a: &Args) -> (i32, String, String) {
    if a.branch.is_none() && a.pr.is_none() {
        return (
            1,
            String::new(),
            "review-hold acquire needs --branch\n".into(),
        );
    }
    if a.holder.is_none() {
        return (
            1,
            String::new(),
            "review-hold acquire needs --holder\n".into(),
        );
    }
    // A PR-number acquire resolves the branch from GitHub - never on whatever
    // branch this checkout stands on. No hold rather than a guessed one.
    let (branch, head) = match (&a.branch, &a.pr) {
        (Some(b), _) => (b.clone(), a.head.clone()),
        (None, Some(pr)) => {
            let Some((b, h, state)) = pr_head_ref_and_oid(&a.cwd, pr) else {
                return (
                    0,
                    String::new(),
                    format!(
                        "review-hold: not registered on PR {pr}: the PR read failed; no hold rather than a guessed branch\n"
                    ),
                );
            };
            if state != "OPEN" {
                return (
                    0,
                    String::new(),
                    format!(
                        "review-hold: not registered on PR {pr}: {state}; no hold rather than a guessed branch\n"
                    ),
                );
            }
            (b, h)
        }
        (None, None) => unreachable!("checked above"),
    };
    let flags = a.flags_json.as_deref().and_then(parse_flags_json);
    // The cap gate: the prefixed line lets the hook surface the denial;
    // policy, not infra, so the one acquire failure that blocks.
    let refusal = review_invocation_refusal(&a.cwd, &branch, &head, flags.as_deref());
    if !refusal.is_empty() {
        return (
            3,
            format!("review-invocation-refused: {refusal}\n"),
            String::new(),
        );
    }
    let advisory = verify_fixes_advisory(&a.cwd, &branch, &head);
    let stderr = if advisory.is_empty() {
        String::new()
    } else {
        format!("{advisory}\n")
    };
    let ttl = match a.ttl_minutes {
        Some(m) => resolve_ttl_ms(Some(m)),
        None => configured_ttl_ms(&a.cwd),
    };
    let ok = acquire_hold(
        &branch,
        a.holder.as_deref().unwrap_or(""),
        &head,
        ttl,
        hold_metadata(a, &flags),
    );
    if !ok {
        return (
            0,
            String::new(),
            format!("review-hold: not registered on {branch}\n"),
        );
    }
    let shown = if head.is_empty() {
        "an unrecorded head"
    } else {
        &head
    };
    (
        0,
        format!("review-hold: holding {branch} at {shown}\n"),
        stderr,
    )
}

/// The acquire metadata row: head plus every optional telemetry leaf the
/// caller passed, in the Python writer's exact key set.
fn hold_metadata(a: &Args, flags: &Option<Vec<String>>) -> serde_json::Map<String, Value> {
    let mut metadata = serde_json::Map::new();
    metadata.insert("head_sha".into(), json!(a.head));
    for (key, value) in [
        ("verb", &a.verb), // verb-wording: allow (event metadata key)
        ("invocation_id", &a.invocation_id),
        ("args_raw", &a.args_raw),
        ("level", &a.level),
        ("level_source", &a.level_source),
    ] {
        if !value.is_empty() {
            metadata.insert(key.into(), json!(value));
        }
    }
    if let Some(flags) = flags {
        metadata.insert("flags".into(), json!(flags));
    }
    metadata
}

fn parse_flags_json(raw: &str) -> Option<Vec<String>> {
    let parsed: Value = serde_json::from_str(raw).ok()?;
    let items = parsed.as_array()?;
    // One non-string refuses the whole payload, as the Python twin's
    // all(isinstance(flag, str)) did; a silent drop would widen the door.
    let mut flags = Vec::with_capacity(items.len());
    for item in items {
        flags.push(item.as_str()?.to_string());
    }
    Some(flags)
}

/// Whether either chain journal exists: the round action fails closed when
/// no read ever answered.
fn review_journals_present(cwd: &Path) -> bool {
    let inputs =
        crate::loopcheck::review_inputs::resolve_review_inputs(cwd, None, None, None, None, None);
    inputs.project_events.exists() || inputs.global_events.exists()
}

/// One JSON request on stdin, one JSON answer on stdout: the shape
/// `verb_call(["claim", "review-hold"], {...})` expects. The answer carries
/// the action result; nonzero exits surface through the raised VerbUnavailable
/// with the same code, exactly as every other stdin door behaves.
fn run_stdin_door() -> (i32, String, String) {
    use std::io::Read;
    let mut payload = String::new();
    if std::io::stdin().read_to_string(&mut payload).is_err() {
        return (
            2,
            String::new(),
            "review-hold: stdin did not answer\n".into(),
        );
    }
    let parsed: Value = match serde_json::from_str(&payload) {
        Ok(v) => v,
        Err(e) => {
            return (
                2,
                String::new(),
                format!("review-hold: unreadable payload: {e}\n"),
            )
        }
    };
    let action = parsed.get("action").and_then(Value::as_str).unwrap_or("");
    let cwd = PathBuf::from(parsed.get("cwd").and_then(Value::as_str).unwrap_or("."));
    match action {
        "acquire" => {
            let branch = parsed.get("branch").and_then(Value::as_str).unwrap_or("");
            let head = parsed.get("head").and_then(Value::as_str).unwrap_or("");
            let holder = parsed.get("holder").and_then(Value::as_str).unwrap_or("");
            let verb = parsed.get("verb").and_then(Value::as_str).unwrap_or(""); // verb-wording: allow (door payload key)
            if branch.is_empty() || holder.is_empty() {
                return (
                    1,
                    String::new(),
                    "review-hold acquire needs --branch and --holder\n".into(),
                );
            }
            let flags: Option<Vec<String>> = parsed
                .get("flags")
                .and_then(Value::as_array)
                .and_then(|items| {
                    // Same refusal as parse_flags_json: one non-string
                    // refuses the payload rather than dropping it.
                    let mut flags = Vec::with_capacity(items.len());
                    for item in items {
                        flags.push(item.as_str()?.to_string());
                    }
                    Some(flags)
                })
                .filter(|f: &Vec<String>| !f.is_empty());
            let refusal = review_invocation_refusal(&cwd, branch, head, flags.as_deref());
            if !refusal.is_empty() {
                return (
                    3,
                    format!("review-invocation-refused: {refusal}\n"),
                    String::new(),
                );
            }
            let advisory = verify_fixes_advisory(&cwd, branch, head);
            let stderr = if advisory.is_empty() {
                String::new()
            } else {
                format!("{advisory}\n")
            };
            let ttl = configured_ttl_ms(&cwd);
            let mut metadata = serde_json::Map::new();
            metadata.insert("head_sha".into(), json!(head));
            if !verb.is_empty() {
                metadata.insert("verb".into(), json!(verb)); // verb-wording: allow (event metadata key)
            }
            if let Some(flags) = &flags {
                metadata.insert("flags".into(), json!(flags));
            }
            if acquire_hold(branch, holder, head, ttl, metadata) {
                (
                    0,
                    format!("{{\"registered\": true, \"branch\": \"{branch}\"}}\n"),
                    stderr,
                )
            } else {
                (
                    0,
                    format!("{{\"registered\": false, \"branch\": \"{branch}\"}}\n"),
                    format!("review-hold: not registered on {branch}\n"),
                )
            }
        }
        other => (
            1,
            String::new(),
            format!("review-hold: unknown action {other:?}\n"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttl_clamps_into_claim_bounds() {
        assert_eq!(resolve_ttl_ms(None), 90 * 60_000);
        assert_eq!(resolve_ttl_ms(Some(0.0)), crate::claims::MIN_TTL_MS);
        assert_eq!(resolve_ttl_ms(Some(1.0)), 60_000);
        assert_eq!(resolve_ttl_ms(Some(1_000_000.0)), crate::claims::MAX_TTL_MS);
        assert_eq!(
            resolve_ttl_ms(Some(f64::NAN)),
            DEFAULT_HOLD_TTL_MINUTES * 60_000
        );
    }

    #[test]
    fn flags_json_parses_and_refuses_non_strings() {
        assert_eq!(
            parse_flags_json(r#"["--verify-fixes"]"#),
            Some(vec!["--verify-fixes".to_string()])
        );
        assert_eq!(parse_flags_json(r#"["--verify-fixes", 1]"#), None);
        assert_eq!(parse_flags_json("not json"), None);
    }
}
