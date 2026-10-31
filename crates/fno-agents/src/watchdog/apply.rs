//! Apply lanes: the watchdog owns the decision, never the mechanism.

use super::tail::{tail_facts, CONFIRM_RECORDS, TAIL_RECORDS};
use super::{branch_commit_count, sandbox_denial_text, Verdict, SANDBOX_BLOCKED, SILENCE, WAKE};
use std::path::Path;
use std::process::{Command, Output};
use std::time::Duration;

/// The bare resume word: a wake is an attach and a neutral resume, not a paste.
pub(crate) const WAKE_MESSAGE: &str = "continue";

/// Resume returns when the live state reads working, which is before the
/// injected turn is flushed to the transcript, so a one-shot read reports a
/// landed wake as refused.
const CONFIRM_ATTEMPTS: u32 = 40;
const CONFIRM_INTERVAL: Duration = Duration::from_millis(250);

pub(crate) const SKIPPED: &str = "skipped";
/// The action half landed: the fleet changed and the verdict did not finish.
pub(crate) const PARTIAL: &str = "partial";

/// The `events.jsonl` type for an apply outcome word. Anything not named here is
/// a true refusal: nothing changed.
pub(crate) fn outcome_event(outcome: &str) -> &'static str {
    match outcome {
        "applied" => "watchdog_applied",
        PARTIAL => "watchdog_partial",
        _ => "watchdog_refused",
    }
}

fn run_fno(args: &[&str], cwd: &str) -> std::io::Result<Output> {
    let mut command = Command::new(crate::scrape::fno_bin());
    command.args(args);
    if !cwd.is_empty() {
        command.current_dir(Path::new(cwd));
    }
    command.output()
}

fn last_output_line(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stderr.trim().is_empty() {
        stdout.trim().to_string()
    } else {
        stderr.trim().to_string()
    };
    text.lines().last().unwrap_or("").to_string()
}

/// Execute one verdict inside `lanes` ("wake" or "all"); only `skipped` is
/// silent. Cap moves belong to the provider-cap actor, so no reroute arm exists.
pub(crate) fn apply_verdict(
    v: &Verdict,
    lanes: &str,
    cwd: &str,
    agent: Option<&str>,
    node: Option<&str>,
) -> (String, String) {
    let in_lane = match lanes {
        "wake" => v.verdict == WAKE || v.verdict == SILENCE,
        "all" => v.verdict == WAKE || v.verdict == SILENCE || v.verdict == SANDBOX_BLOCKED,
        _ => false,
    };
    if !in_lane {
        return (
            SKIPPED.to_string(),
            format!("{} outside {lanes} lane", v.verdict),
        );
    }
    if v.verdict == SANDBOX_BLOCKED {
        return apply_sandbox_blocked(v, cwd, node);
    }
    apply_wake(v, cwd, agent.unwrap_or(&v.agent))
}

fn apply_sandbox_blocked(v: &Verdict, cwd: &str, node: Option<&str>) -> (String, String) {
    let Some(node) = node.filter(|n| !n.is_empty()) else {
        return (
            "refused".to_string(),
            "reap refused: node identity is unreadable".to_string(),
        );
    };
    let claim = match super::sweep::claim_view(node) {
        Ok(claim) => claim,
        Err(exc) => {
            return (
                "refused".to_string(),
                format!("reap refused: claim unreadable ({exc})"),
            )
        }
    };
    if claim.state != "free" {
        let state = if claim.state.is_empty() {
            "unknown"
        } else {
            claim.state.as_str()
        };
        return (
            "held".to_string(),
            format!("reap held: node claim is {state}"),
        );
    }
    match branch_commit_count(cwd) {
        None => {
            return (
                "refused".to_string(),
                "reap refused: branch commit count unreadable".to_string(),
            )
        }
        Some(count) if count != 0 => {
            return (
                "held".to_string(),
                format!("reap held: branch carries {count} commit(s)"),
            )
        }
        Some(_) => {}
    }
    let facts = tail_facts(&v.row_id, cwd, "codex", TAIL_RECORDS, &[]);
    if sandbox_denial_text(facts.as_ref()).is_none() {
        return (
            "refused".to_string(),
            "reap refused: current transcript no longer carries the distress".to_string(),
        );
    }
    let output = run_fno(
        &[
            "agents",
            "rm",
            &v.row_id,
            "--force",
            "--audit-reason",
            "codex-sandbox-blocked",
        ],
        cwd,
    );
    match output {
        Err(exc) => (
            "refused".to_string(),
            format!("{} action failed: {exc}", v.verdict),
        ),
        Ok(out) if !out.status.success() => (
            "refused".to_string(),
            format!(
                "reap exit {}: {}",
                out.status.code().unwrap_or(-1),
                last_output_line(&out)
            ),
        ),
        Ok(_) => (
            "applied".to_string(),
            format!("reaped {}; Codex sandbox blocked Git writes", v.name),
        ),
    }
}

fn apply_wake(v: &Verdict, cwd: &str, agent: &str) -> (String, String) {
    let before = tail_facts(&v.row_id, cwd, agent, TAIL_RECORDS, &[]);
    let before_epoch = before.and_then(|f| f.last_event_epoch);
    let output = run_fno(
        &[
            "agents",
            "resume",
            &v.row_id,
            "--message",
            WAKE_MESSAGE,
            "--from-name",
            "fno/watchdog",
        ],
        cwd,
    );
    let out = match output {
        Ok(out) => out,
        Err(exc) => {
            return (
                "refused".to_string(),
                format!("{} action failed: {exc}", v.verdict),
            )
        }
    };
    if !out.status.success() {
        return (
            "refused".to_string(),
            format!(
                "resume exit {}: {}",
                out.status.code().unwrap_or(-1),
                last_output_line(&out)
            ),
        );
    }
    if agent != "claude" {
        // Resume's exit code is the receipt on every non-claude harness; the
        // transcript marker is the claude contract, and re-checking it here
        // would read a lagging rollout write as a refusal on a delivered wake.
        return (
            "applied".to_string(),
            format!(
                "woke {}; resume exit 0 is the delivery receipt ({agent})",
                v.name
            ),
        );
    }
    if !confirm_wake_landed(&v.row_id, cwd, WAKE_MESSAGE, before_epoch, agent) {
        // Exit 0 is exactly the receipt that lied here, so its last line is what
        // the next operator needs.
        let tail = last_output_line(&out);
        let suffix = if tail.is_empty() {
            String::new()
        } else {
            format!(": {tail}")
        };
        return (
            "refused".to_string(),
            format!(
                "resume reported success but '{WAKE_MESSAGE}' is not in the transcript after the wake{suffix}"
            ),
        );
    }
    (
        "applied".to_string(),
        format!("woke {}; message confirmed in transcript", v.name),
    )
}

/// The message must appear after the pre-wake marker, as a record whose whole
/// text equals the message: a substring match reads "Let me continue with the
/// tests" as a landed wake.
fn confirm_wake_landed(
    row_id: &str,
    cwd: &str,
    message: &str,
    before_epoch: Option<f64>,
    agent: &str,
) -> bool {
    for attempt in 0..CONFIRM_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(CONFIRM_INTERVAL);
        }
        let Some(facts) = tail_facts(row_id, cwd, agent, CONFIRM_RECORDS, &[]) else {
            continue;
        };
        if confirm_in_records(&facts.records, message, before_epoch) {
            return true;
        }
    }
    false
}

/// A record with no timestamp of its own never confirms when a pre-wake stamp
/// exists: a torn old line carrying the word is presence, not a landing.
pub(crate) fn confirm_in_records(
    records: &[(Option<f64>, String)],
    message: &str,
    before_epoch: Option<f64>,
) -> bool {
    records.iter().any(|(epoch, text)| {
        text == message
            && match before_epoch {
                None => true,
                Some(before) => epoch.is_some_and(|e| e > before),
            }
    })
}
