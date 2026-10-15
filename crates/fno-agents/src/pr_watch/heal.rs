//! The `heal` verb: revive an enabled-but-dead watcher. Ported from the
//! `fno do pr watch heal` leaf in `cli/src/fno/pr_watch/cli.py` (the
//! SessionStart self-heal entrypoint) and doctor's `heal_watcher` dead-cure
//! in `cli/src/fno/pr_watch/_install.py`, whose bodies this change deletes.
//! The gates are the leaf's own: the disabled skip, the bounce-pending guard
//! (healing over a bounce that has not ticked yet re-arms the grace over the
//! same fault), and the `pr-watch:heal` single-flight. The cure rides
//! `refresh::refresh_watcher` - re-render + bounce, `force_bounce`, deferred
//! while a tick is in flight - exactly as the Python leaf called it.

use std::path::PathBuf;

use crate::flight_gate::{acquire_flight, FlightGate};
use crate::pr_watch::refresh;
use crate::pr_watch::status::{self, as_bool, cfg_lookup};

// The single-flight key, scope and TTL the Python leaf used, kept verbatim:
// a heal in flight from an older client and one from this verb are the same
// flight.
const FLIGHT_KEY: &str = "pr-watch:heal";
const FLIGHT_SCOPE: &str = "pr-watch SessionStart self-heal";
const FLIGHT_TTL_MS: i64 = 5 * 60 * 1000;

/// The leaf contract the Python `fno do pr watch heal` command forwards to.
/// Every skip is a quiet exit 0 (the SessionStart hook must never see a
/// traceback); only a bounce step that wedged exits nonzero, after printing
/// its receipt line.
pub(crate) fn leaf_output(args: &[String]) -> (String, i32) {
    let mut caller = "heal".to_string();
    let mut fno_binary: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--caller" if i + 1 < args.len() => {
                i += 1;
                caller = args[i].clone();
            }
            "--fno-binary" if i + 1 < args.len() => {
                i += 1;
                fno_binary = Some(args[i].clone());
            }
            _ => {
                return (
                    "usage: fno-agents pr-watch heal [--caller <name>] [--fno-binary <path>]\n"
                        .to_string(),
                    2,
                );
            }
        }
        i += 1;
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    if !as_bool(cfg_lookup(&cwd, &["pr_watch", "enabled"]), false) {
        return ("pr-watch heal: disabled; nothing to heal\n".to_string(), 0);
    }

    // The bounce-pending guard reads the same verdict the status verb
    // reports, in process: pending means a bounce is inside its
    // healthy-pending grace, and re-bouncing would hide the fault for
    // another 2x interval.
    let launch_agents_dir = status::launch_agents_dir();
    let inp = status::gather(&cwd, &launch_agents_dir);
    let marks = status::tick_watermarks(&inp.events);
    let leg = status::leg_facts(&inp);
    let verdict = status::verdict(&inp, &marks, &leg);
    if verdict.bounce_pending {
        return (
            format!(
                "pr-watch heal: a bounce is pending its first tick ({}); skipped. \
                 Run fno do pr watch refresh to bounce anyway.\n",
                verdict.detail
            ),
            0,
        );
    }

    // One heal per window: concurrent SessionStarts reinstall at most once.
    // A held gate (or a gate that cannot run) reads as someone else healing,
    // never as a reason to abort the hook.
    let holder = format!(
        "pr-watch-heal:{}:{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    let held_by = match acquire_flight(
        FLIGHT_KEY,
        &holder,
        FLIGHT_TTL_MS,
        FLIGHT_SCOPE,
        Some(std::process::id()),
        None,
        None,
    ) {
        FlightGate::Acquired(held_by) => held_by,
        FlightGate::Held | FlightGate::Unavailable(_) => {
            return (
                "pr-watch heal: another session is healing; skipped\n".to_string(),
                0,
            );
        }
    };

    let interval = cfg_lookup(&cwd, &["pr_watch", "interval_seconds"])
        .and_then(|v| v.as_integer())
        .filter(|n| *n > 0)
        .unwrap_or(600);
    let fno_binary =
        fno_binary.unwrap_or_else(|| crate::scrape::fno_py().to_string_lossy().into_owned());
    let (msg, rc) = refresh::refresh_watcher(
        &launch_agents_dir,
        &fno_binary,
        interval,
        true,
        &caller,
        true,
        &status::state_root(&cwd),
    );
    // The TTL plus the pid probe retire a lost holder; a failed release must
    // never mask the cure's outcome.
    let _ = crate::claims::release(FLIGHT_KEY, &held_by, None, None);
    (
        format!("pr-watch heal: {msg}\n"),
        if rc != 0 { 1 } else { 0 },
    )
}

/// `fno-agents pr-watch heal` dispatches here from `pr_watch::run`.
pub fn run(args: &[String]) -> i32 {
    let (out, code) = leaf_output(args);
    print!("{out}");
    code
}
