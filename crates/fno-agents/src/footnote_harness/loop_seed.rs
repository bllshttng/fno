//! The `/loop <interval> <prompt>` seed for footnote rows: fno owns the
//! clock, footnote stays a one-shot child. The first turn is the spawn's
//! create; every later turn is one fresh resume through the ask lane, so
//! the registry row reads Busy during a tick and Exited between ticks.

use std::time::{Duration, SystemTime};

use super::outcome;
use crate::claude_ask::AskOutcome;

/// The loop stops on its own after this long, so a forgotten row cannot
/// tick forever.
const MAX_LIFETIME: Duration = Duration::from_secs(7 * 24 * 3_600);

/// Failed ticks in a row tolerated before the loop gives up.
const MAX_FAILURE_STREAK: u32 = 3;

/// A parsed seed: the tick interval and the prompt every turn runs.
#[derive(Debug)]
pub struct LoopSeed {
    pub interval: Duration,
    pub prompt: String,
}

/// Parse a `/loop` seed. `None` unless the first whitespace token is
/// exactly `/loop`; `Err` carries the usage line when the interval is
/// missing, malformed or below the 60-second floor (the same minute
/// granularity Claude's `/loop` cron keeps), or the prompt is empty.
pub fn parse(message: &str) -> Option<Result<LoopSeed, String>> {
    let token = message.split_whitespace().next()?;
    if token != "/loop" {
        return None;
    }
    let usage =
        "footnote /loop needs an interval and a prompt: /loop <N>s|m|h|d <prompt>".to_string();
    let rest = message[token.len()..].trim_start();
    let Some((interval_token, prompt)) = rest.split_once(char::is_whitespace) else {
        return Some(Err(usage));
    };
    let prompt = prompt.trim();
    match (parse_interval(interval_token), prompt.is_empty()) {
        (Some(secs), false) => Some(Ok(LoopSeed {
            interval: Duration::from_secs(secs),
            prompt: prompt.to_string(),
        })),
        _ => Some(Err(usage)),
    }
}

/// `<N>s|m|h|d`, at least 60 seconds.
fn parse_interval(token: &str) -> Option<u64> {
    let (digits, unit_secs) = if let Some(digits) = token.strip_suffix('s') {
        (digits, 1)
    } else if let Some(digits) = token.strip_suffix('m') {
        (digits, 60)
    } else if let Some(digits) = token.strip_suffix('h') {
        (digits, 3_600)
    } else if let Some(digits) = token.strip_suffix('d') {
        (digits, 86_400)
    } else {
        return None;
    };
    let secs = digits.parse::<u64>().ok()?.checked_mul(unit_secs)?;
    (secs >= 60).then_some(secs)
}

/// Run `first`, then `tick` once per interval until the row is removed, the
/// caller interrupts, a tick is refused (exit 2: a live writer holds the
/// session, or the spec was refused), [`MAX_FAILURE_STREAK`] ticks fail in
/// a row, or the seven-day cap. Sleep, clock and row check are injected so
/// tests never wait; the tick count names the turns run, first included. A
/// nonzero first turn stops at once and returns it unchanged: a turn that
/// refused before the loop began has no loop to run.
pub fn drive(
    seed: &LoopSeed,
    first: impl FnOnce(&str) -> AskOutcome,
    mut tick: impl FnMut(&str) -> AskOutcome,
    row_alive: impl Fn() -> bool,
    mut sleep: impl FnMut(Duration),
    now: impl Fn() -> SystemTime,
) -> AskOutcome {
    let started = now();
    let first_out = first(&seed.prompt);
    print!("{}", first_out.stdout);
    eprint!("{}", first_out.stderr);
    if first_out.exit_code != 0 {
        // The text is already out; a bare code stops the caller from
        // printing the same refusal a second time.
        return outcome(first_out.exit_code, String::new(), String::new());
    }
    let mut ticks = 1u64;
    let mut failures = 0u32;
    loop {
        // A huge interval never out-sleeps the cap: each sleep is bound by
        // the lifetime the row has left.
        let elapsed = now().duration_since(started).unwrap_or_default();
        let Some(remaining) = MAX_LIFETIME.checked_sub(elapsed) else {
            return stopped(0, ticks, "7 days elapsed");
        };
        sleep(seed.interval.min(remaining));
        if !row_alive() {
            return stopped(0, ticks, "row removed");
        }
        let out = tick(&seed.prompt);
        ticks += 1;
        print!("{}", out.stdout);
        eprint!("{}", out.stderr);
        if crate::subprocess_ask::ask_interrupted() {
            return stopped(130, ticks, "interrupted");
        }
        if out.exit_code == 2 {
            return stopped(2, ticks, "refused");
        }
        failures = if out.exit_code == 0 { 0 } else { failures + 1 };
        if failures >= MAX_FAILURE_STREAK {
            return stopped(out.exit_code, ticks, "3 failed ticks in a row");
        }
    }
}

fn stopped(code: i32, ticks: u64, why: &str) -> AskOutcome {
    outcome(
        code,
        String::new(),
        format!("footnote /loop stopped after {ticks} ticks: {why}\n"),
    )
}
