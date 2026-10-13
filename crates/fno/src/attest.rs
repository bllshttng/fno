//! Best-effort primary mirror for committed `review_attestation` rows.
//!
//! The event store stays machine-local (multiplayer.md), but a
//! head-pinned attestation is gate evidence for a merge from a peer
//! machine: the merge read only the local journals and paid a second
//! review round. The commit path owns the funnel: every producer (the
//! Python emit surface, the emit-attestation recovery script, a declare
//! path) lands here, so one hook crosses every producer without each
//! caller learning the primary.
//!
//! Failure is named on stderr and never fails the commit: the local row is
//! the durable record, and a peer's gate read degrades to machine-local
//! coverage, the pre-fix behavior.

use crate::digest_overlay::fno_agents_bin;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Fail-open budget for the carrier shell-outs; a wedged child must not
/// hold an attestation emit or a gate read longer than a review round can
/// afford.
const PUSH_BUDGET: Duration = Duration::from_secs(10);

/// Push one committed envelope line to the primary when its type is a
/// review attestation. A suppressed receipt committed nothing, so it
/// pushes nothing.
pub(crate) fn maybe_push(ty: &str, line: &str, suppressed: bool) {
    if suppressed || ty != "review_attestation" {
        return;
    }
    let mut child = match Command::new(fno_agents_bin())
        .args(["attest", "push"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("attest push: skipped ({e})");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = stdin.write_all(line.as_bytes()) {
            eprintln!("attest push: skipped ({e})");
        }
    }
    // Dropping stdin hands the child EOF; the carrier answers in one
    // transaction round-trip, so the budget is generous and the commit
    // never waits on a wedged primary for longer than it must.
    let deadline = Instant::now() + PUSH_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    eprintln!("attest push: skipped (carrier exceeded {PUSH_BUDGET:?})");
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return,
        }
    }
}

/// The shared primary's attestation rows for this repo, as envelope lines,
/// for a gate read. Fail-open at every step: an unshared store, an
/// unresolvable carrier, or an unreachable primary yields an empty
/// segment, the pre-carrier behavior. The caller gates on the query's
/// types, so only a `review_attestation` read pays the shell-out.
pub(crate) fn pull_segment() -> Vec<String> {
    let mut child = match Command::new(fno_agents_bin())
        .args(["attest", "pull"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("attest pull: skipped ({e})");
            return Vec::new();
        }
    };
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut out) = stdout {
            use std::io::Read;
            let _ = out.read_to_string(&mut text);
        }
        text
    });
    let deadline = Instant::now() + PUSH_BUDGET;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let text = reader.join().unwrap_or_default();
                return text.lines().map(str::to_string).collect();
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    eprintln!("attest pull: skipped (carrier exceeded {PUSH_BUDGET:?})");
                    return Vec::new();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => return Vec::new(),
        }
    }
}
