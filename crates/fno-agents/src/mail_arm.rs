//! The mail arm: the receiver side of the cross-host bus.
//!
//! With a shared primary set, the daemon pulls the pending `fno_mail_bus`
//! rows addressed to this host in seq order and hands each payload to the
//! receiver's own local send, so lane selection, holds, and receipts stay
//! the local command's. The question bus (`question_bus.rs`) rides the same
//! beat. Two maintenance legs ride it too: the flush
//! that drains the sender's local outbox once the primary answers again
//! (edge 7), and the expire that flips rows past their TTL (edge 2). With
//! no primary set, the beat returns at once and opens no socket.

use crate::mail_bus::{self, BusRow};
use crate::mail_remote::{local_send_argv, outbox_path, seen_path};
use crate::store_remote::Remote;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The default pull cadence. `agents.remote.pull_interval_secs` overrides it
/// (operator ruling, 2026-10-10: 5 seconds per host is heavy for machines
/// at their limit).
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15);

/// The pure interval rule: a positive override wins, anything else keeps
/// the default (a typo degrades, it never stops mail).
pub(crate) fn interval_from_secs(value: Option<i64>) -> Duration {
    match value {
        Some(secs) if secs > 0 => Duration::from_secs(secs as u64),
        _ => DEFAULT_INTERVAL,
    }
}

/// The configured cadence, read once per beat. The key lives in the GLOBAL
/// config only, like `store.remote_url` (a project cannot set store keys).
pub fn pull_interval() -> Duration {
    interval_from_secs(
        crate::agents_config::config_lookup_global(&["agents", "remote", "pull_interval_secs"])
            .and_then(|v| v.as_integer()),
    )
}

#[derive(Default)]
pub struct Arm {
    last_tick: Mutex<Option<Instant>>,
    in_flight: Arc<AtomicBool>,
}

/// One tick. Mirrors `lease_heartbeat::maybe_tick`: gate on the interval
/// and a one-in-flight flag, then run the legs off-loop.
pub fn maybe_tick(arm: &Arm) {
    let remote = match crate::store_remote::configured() {
        Ok(Some(remote)) => remote,
        Ok(None) => return,
        Err(error) => {
            static WARNED: std::sync::Once = std::sync::Once::new();
            WARNED.call_once(|| eprintln!("mail-arm: {error}"));
            return;
        }
    };
    let interval = pull_interval();
    {
        let mut last = arm.last_tick.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|tick| tick.elapsed() < interval)
            || arm.in_flight.swap(true, Ordering::SeqCst)
        {
            return;
        }
        *last = Some(Instant::now());
    }
    let flag = Arc::clone(&arm.in_flight);
    tokio::task::spawn_blocking(move || {
        let _gate = crate::daemon::SweepGate(flag);
        match beat(&remote) {
            Ok(outcome) => {
                if outcome.delivered + outcome.flushed + outcome.expired as usize > 0 {
                    eprintln!(
                        "mail-arm: delivered {} flushed {} expired {}",
                        outcome.delivered, outcome.flushed, outcome.expired
                    );
                }
            }
            Err(error) => eprintln!("mail-arm: {error}"),
        }
    });
}

/// What one beat did, for the event line and the tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct PullOutcome {
    pub delivered: usize,
    pub flushed: usize,
    pub expired: u64,
}

/// One beat: pull, then flush, then expire. The legs are independent: a
/// pull failure does not stop the flush from draining the local outbox.
pub fn beat(remote: &Remote) -> Result<PullOutcome, String> {
    let host = mail_bus::normalize_host(&crate::claims::hostname());
    let mut outcome = PullOutcome {
        delivered: 0,
        flushed: 0,
        expired: 0,
    };
    // One DDL per process, not one per beat: the table outlives the daemon,
    // and a failed create is surfaced, never swallowed.
    static SCHEMA: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let schema = SCHEMA.get_or_init(|| mail_bus::ensure_schema(remote));
    if let Err(e) = schema {
        return Err(e.clone());
    }
    match mail_bus::fetch_pending(remote, &host) {
        Ok(rows) => outcome.delivered = pull_rows(remote, rows, &home_root()),
        Err(e) => eprintln!("mail-arm: pull: {e}"),
    }
    match flush(remote) {
        Ok(n) => outcome.flushed = n,
        Err(e) => eprintln!("mail-arm: flush: {e}"),
    }
    match mail_bus::expire_stale(remote) {
        Ok(n) => outcome.expired = n,
        Err(e) => eprintln!("mail-arm: expire: {e}"),
    }
    // Open questions ride the same beat to the other machines of the store.
    crate::question_bus::tick(remote);
    Ok(outcome)
}

fn home_root() -> PathBuf {
    let home = crate::paths::AgentsHome::from_env();
    let _ = home.ensure_root();
    home.root().to_path_buf()
}

/// Deliver pending rows in seq order. A row leaves the pending pool only
/// through a local send whose receiver owns the copy (exit 0, or the NOT
/// LANDED durable demotion - the durable copy is written) or a
/// style-refused body (below): any other failure stays pending, backs its
/// next attempt down the retry ladder ([`retry_backoff`]), and the TTL
/// bounds it. The seen ledger records msg_ids after a delivered send, so a
/// redelivered row never injects twice (at-least-once, edge 5).
pub(crate) fn pull_rows(remote: &Remote, rows: Vec<BusRow>, home: &std::path::Path) -> usize {
    pull_rows_with(remote, rows, home, &mut |row| inject_row(remote, home, row))
}

/// The row loop behind `pull_rows`, with the injection leg as a parameter so
/// the retry contract is testable without spawning a real send. A failed
/// row books its next attempt at now + [`retry_backoff`] into the retry
/// ledger (`mail_retry.jsonl`), and a row not yet due is skipped without a
/// send; a delivered row's entry is dropped.
pub(crate) fn pull_rows_with(
    remote: &Remote,
    rows: Vec<BusRow>,
    home: &std::path::Path,
    inject: &mut dyn FnMut(&BusRow) -> bool,
) -> usize {
    let mut retries = load_retries(home);
    let mut delivered = 0;
    for row in rows {
        if seen(remote, home, &row) {
            continue;
        }
        if !retry_due(&retries, &row.msg_id) {
            continue;
        }
        if let Err(reason) = sender_trusted(home, &row) {
            eprintln!("mail-arm: refused {}: {reason}", row.msg_id);
            if let Err(e) = mail_bus::mark(remote, &row.msg_id, mail_bus::REFUSED) {
                eprintln!("mail-arm: refused mark for {}: {e}", row.msg_id);
            }
            continue;
        }
        if inject(&row) {
            retries.remove(&row.msg_id);
            save_retries(home, &retries);
            delivered += 1;
        } else {
            let attempts = retries
                .get(&row.msg_id)
                .map_or(0, |(n, _)| *n)
                .saturating_add(1);
            let due = now_epoch_secs() + retry_backoff(attempts).as_secs();
            retries.insert(row.msg_id.clone(), (attempts, due));
            save_retries(home, &retries);
        }
    }
    delivered
}

/// Is this row's sender who it says it is? A signed row must verify, its
/// key must match the one pinned for its host (first use pins it), and the
/// host inside `from_id` (`<user>:<host>/<name>`), the sender a receiver
/// reads, must be the host the key signed for.
fn sender_trusted(home: &std::path::Path, row: &BusRow) -> Result<(), String> {
    let signed = row.pubkey.as_deref().zip(row.sig.as_deref());
    if signed.is_some() {
        let claimed = row
            .from_id
            .split('/')
            .next()
            .and_then(|head| head.rsplit([':', '@']).next())
            .map(mail_bus::normalize_host);
        if claimed.as_deref() != Some(row.from_host.as_str()) {
            return Err(format!(
                "from_id {:?} names a host other than the signing host {:?}",
                row.from_id, row.from_host
            ));
        }
    }
    crate::machine_key::check(
        &crate::machine_key::pins_path(home),
        &row.from_host,
        signed,
        &row.signed_bytes(),
    )
    .map(drop)
}

/// One pending row's retry memory: the attempt count the ladder reads and
/// the epoch second its next send is due. A row the ledger never mentions
/// is on its first attempt and always due.
type RetryMap = std::collections::HashMap<String, (u32, u64)>;

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the retry ledger, dropping entries whose next due passed more than
/// the bus TTL ago (the row itself expired; the memory should too).
fn load_retries(home: &std::path::Path) -> RetryMap {
    let stale_before = now_epoch_secs().saturating_sub(crate::mail_bus::MAIL_TTL_SECS as u64);
    std::fs::read_to_string(crate::mail_remote::retry_path(home))
        .map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
                .filter_map(|v| {
                    let id = v.get("msg_id")?.as_str()?.to_string();
                    let attempts = v.get("attempts")?.as_u64()? as u32;
                    let due = v.get("next_due")?.as_u64()?;
                    Some((id, (attempts, due)))
                })
                .filter(|(_, (_, due))| *due >= stale_before)
                .collect()
        })
        .unwrap_or_default()
}

/// Rewrite the ledger with one line per entry. A no-delivery beat with no
/// backing-off row writes nothing.
fn save_retries(home: &std::path::Path, map: &RetryMap) {
    if map.is_empty() {
        let _ = std::fs::remove_file(crate::mail_remote::retry_path(home));
        return;
    }
    let mut lines: Vec<String> = map
        .iter()
        .map(|(id, (attempts, due))| {
            serde_json::json!({"msg_id": id, "attempts": attempts, "next_due": due}).to_string()
        })
        .collect();
    lines.sort();
    let _ = std::fs::write(
        crate::mail_remote::retry_path(home),
        lines.join("\n") + "\n",
    );
}

/// A row is due on its first attempt, and past due once the clock reaches
/// the booked second.
fn retry_due(map: &RetryMap, msg_id: &str) -> bool {
    map.get(msg_id)
        .is_none_or(|(_, due)| now_epoch_secs() >= *due)
}

/// The send's stderr as one log-sized line: whitespace collapsed, then cut
/// on a char boundary, so a refusal's reason reaches the daemon log without
/// a multi-line splatter.
fn brief(text: &[u8], cap: usize) -> String {
    let joined = String::from_utf8_lossy(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = joined.chars().take(cap).collect();
    if cut.len() < joined.len() {
        format!("{cut}...")
    } else {
        cut
    }
}

/// What one local send's exit means for its bus row. `Delivered` ends the
/// row (the receiver's mail owns the copy), `Refused` marks the loss visible
/// to the sender, `Pending` leaves the row on the retry ladder.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SendVerdict {
    Delivered,
    Refused,
    Pending,
}

/// The exit reading, lifted out of [`inject_row`] so the table is testable:
/// exit 0 and the Python receipts' NOT LANDED (a durable demotion - the
/// durable copy is written, only the live inject went unconfirmed) both read
/// delivered; exit 1 with a confirming style door reads refused; every
/// other code, and a signal (`None`), stays pending. The door closure runs
/// only on the ambiguous exit 1, so a clean verdict never pays the consult.
pub(crate) fn send_verdict(code: Option<i32>, style_refused: impl FnOnce() -> bool) -> SendVerdict {
    match code {
        Some(0) | Some(crate::mail_receipt::NOT_LANDED_EXIT) => SendVerdict::Delivered,
        Some(1) if style_refused() => SendVerdict::Refused,
        _ => SendVerdict::Pending,
    }
}

/// The style door's receipt naming violations: the deterministic refusal.
/// Mirrors the mail gate's own clean rule: a non-empty `exception` makes an
/// excepted body clean whatever the violations carry, and a clean or
/// unparseable receipt reads transient, so the row stays pending for the
/// next beat.
pub(crate) fn door_confirms_refusal(stdout: &[u8]) -> bool {
    serde_json::from_str::<serde_json::Value>(&String::from_utf8_lossy(stdout))
        .ok()
        .is_some_and(|receipt| {
            let excepted = receipt
                .get("exception")
                .is_some_and(|exception| !exception.is_null());
            if excepted {
                return false;
            }
            receipt
                .get("violations")
                .and_then(|violations| violations.as_array())
                .is_some_and(|violations| !violations.is_empty())
        })
}

/// The retry ladder between a failed delivery and its next attempt. One
/// beat every 15 s re-ran a full python send for every pending row, and a
/// row whose recipient is unknown (exit 16, the registry's refusal) fails
/// identically until the recipient registers or the 7-day TTL expires -
/// two stuck rows measured 8-9 spawns a minute, 0.3-0.4 cores of daemon
/// child CPU. The ladder keeps the pending contract (a row delivers the
/// moment its recipient exists) and pays the send's cold start on a
/// doubling schedule instead of every beat.
pub(crate) const RETRY_BACKOFF_BASE_S: u64 = 15;

/// The ladder's cap. A row past the cap retries four times an hour, still
/// inside the at-least-once contract the TTL bounds.
pub(crate) const RETRY_BACKOFF_CAP_S: u64 = 900;

pub(crate) fn retry_backoff(attempts: u32) -> std::time::Duration {
    let shift = attempts.saturating_sub(1).min(10);
    std::time::Duration::from_secs(RETRY_BACKOFF_BASE_S)
        .checked_mul(1u32 << shift)
        .unwrap_or(std::time::Duration::from_secs(RETRY_BACKOFF_CAP_S))
        .min(std::time::Duration::from_secs(RETRY_BACKOFF_CAP_S))
}

/// The argv the front door forwards to its sibling `style-check`, with the
/// receiver's configured mail word cap so the consult answers the same
/// question the send's gate asked (the built-in default 80 applies when no
/// cap is configured).
pub(crate) fn style_door_argv(word_cap: Option<i64>) -> Vec<String> {
    let mut argv = vec![
        "doctor".to_string(),
        "lint".to_string(),
        "style".to_string(),
        "--surface".to_string(),
        "mail".to_string(),
        "--stdin".to_string(),
        "--json".to_string(),
    ];
    if let Some(cap) = word_cap {
        argv.push("--word-cap".to_string());
        argv.push(cap.to_string());
    }
    argv
}

/// The receiver's configured mail word cap, read the way the daemon reads
/// every config key: project config at the working directory falling back
/// to the global file. `None` keeps the checker's built-in default.
fn mail_word_cap() -> Option<i64> {
    crate::agents_config::config_lookup(
        &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
        &["style", "word_cap", "mail"],
    )
    .and_then(|value| value.as_integer())
}

/// Ask the receiver's own style door about the body, the same door the
/// local send consults by way of the front door's
/// `doctor lint style` forwarding (the daemon binary itself has no
/// `style-check` dispatch). Runs only on an exit-1 refusal, where the exit
/// alone is ambiguous: the style gate refuses for good, but so does a
/// budget window, a down door, or an internal error. The body rides a
/// writer thread, so a payload wider than the pipe buffer cannot deadlock
/// the read.
pub(crate) fn style_door_refuses(payload: &str) -> bool {
    ask_style_door(&style_door_argv(mail_word_cap()), payload)
        .map(|output| door_confirms_refusal(&output.stdout))
        .unwrap_or(false)
}

/// The sender-side consult: run the same door before a cross-host store
/// send writes its row, so a refused body dies at the sender instead of in
/// transit, where only the sender's `mail status` shows the loss.
/// `Some(report)` when the door names violations; `None` when the body is
/// clean or the door cannot run, which reads clean exactly as the pull
/// side does.
pub(crate) fn style_door_report(payload: &str) -> Option<String> {
    let output = ask_style_door(&style_door_report_argv(mail_word_cap()), payload)?;
    (output.status.code() == Some(1)).then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The report form of the door consult: the same argv without --json, so
/// exit 1 with the human report is the refusal and stdout rides to the
/// sender.
pub(crate) fn style_door_report_argv(word_cap: Option<i64>) -> Vec<String> {
    style_door_argv(word_cap)
        .into_iter()
        .filter(|word| word != "--json")
        .collect()
}

/// Spawn the door with `argv`, feed it the payload through a writer
/// thread, and return the output. `None` when the door cannot start or a
/// read fails - both read clean.
fn ask_style_door(argv: &[String], payload: &str) -> Option<std::process::Output> {
    let mut cmd = std::process::Command::new(crate::mail_remote::fno_binary());
    cmd.args(argv);
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let payload = payload.to_string();
    let mut pipe = child.stdin.take();
    std::thread::spawn(move || {
        if let Some(stdin) = pipe.as_mut() {
            let _ = stdin.write_all(payload.as_bytes());
        }
    });
    child.wait_with_output().ok()
}

/// True when the row was already delivered by an earlier beat.
fn seen(remote: &Remote, home: &std::path::Path, row: &BusRow) -> bool {
    let path = seen_path(home);
    let already = std::fs::read_to_string(&path)
        .map(|existing| {
            existing
                .lines()
                .any(|line| line.trim() == row.msg_id.as_str())
        })
        .unwrap_or(false);
    if already {
        let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::DELIVERED);
    }
    already
}

fn record_seen(home: &std::path::Path, msg_id: &str) -> Result<(), String> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(seen_path(home))
        .and_then(|mut f| writeln!(f, "{msg_id}"))
        .map_err(|e| e.to_string())
}

/// The delivery leg: the receiver's own local send answers. Its registry
/// resolves the name, and its holds apply untouched (edge 8). An exit 1
/// whose style door names the body's violations marks the row refused, so
/// the sender's `mail status` shows the loss instead of a pending row that
/// every retry refuses again. Every other refusal, the unknown recipient
/// among them (exit 16, the registry's "no live name" answer), stays
/// pending and backs its next attempt down the [`retry_backoff`] ladder -
/// a recipient that registers later still receives the row, and the beat
/// no longer pays a send's cold start for it every 15 s. Returns true when
/// the row is done (delivered or refused), false to leave it pending.
fn inject_row(remote: &Remote, home: &std::path::Path, row: &BusRow) -> bool {
    // Edge 6: a raw row is a command typed into a pane. Anyone on the
    // tailnet can write a bus row, and the sender-written from_id proves
    // nothing: the tailnet is the boundary, the allowlist is the second
    // check (operator ruling, 2026-10-10).
    if row.raw && !raw_sender_allowed(&row.from_id) {
        let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::REFUSED);
        return true;
    }
    let argv = local_send_argv(&row.to_name, &row.payload, row.raw, &row.from_id);
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    match cmd.output() {
        Ok(output) => {
            let code = output.status.code();
            // The receiver's own style gate refusing the body replays the
            // same refusal on every retry, so marking refused is what makes
            // the loss visible to the sender's `mail status`. The door
            // consult keeps a transient exit 1 (a budget window, a down
            // door, an internal error) riding pending to a beat that can
            // still deliver.
            match send_verdict(code, || style_door_refuses(&row.payload)) {
                SendVerdict::Delivered => {
                    if record_seen(home, &row.msg_id).is_ok() {
                        let _ = mail_bus::mark(remote, &row.msg_id, mail_bus::DELIVERED);
                        return true;
                    }
                    // The send landed but the ledger write failed: leave
                    // pending, so the next beat reconciles the row state.
                    // The cost is one replayed local send on the next beat
                    // (the ledger is the at-least-once dedup, so delivering
                    // without it would trade one duplicate for unbounded
                    // ones on a redelivered row).
                    false
                }
                SendVerdict::Refused => {
                    eprintln!(
                        "mail-arm: local send for {} refused (exit {code:?}): {}",
                        row.msg_id,
                        brief(&output.stderr, 300)
                    );
                    if let Err(e) = mail_bus::mark(remote, &row.msg_id, mail_bus::REFUSED) {
                        eprintln!("mail-arm: refused mark for {}: {e}", row.msg_id);
                    }
                    true
                }
                SendVerdict::Pending => {
                    // The only trace a failed pull attempt leaves: without
                    // this line a row retrying forever is silent (the
                    // measured incident's rows stayed pending with no log).
                    eprintln!(
                        "mail-arm: local send for {} exit {}: {}; the row stays pending",
                        row.msg_id,
                        code.map(|c| c.to_string())
                            .unwrap_or_else(|| "signal".to_string()),
                        brief(&output.stderr, 300)
                    );
                    false
                }
            }
        }
        Err(e) => {
            eprintln!("mail-arm: local send for {}: {e}", row.msg_id);
            false
        }
    }
}

/// The raw allowlist: `agents.remote.raw_senders`, a list of `user@host`
/// sender heads. Empty is the default and refuses every cross-host raw row.
/// The label writes `user:host`; the head reads back as `user@host`, so the
/// config spelling holds for both label forms.
fn raw_sender_allowed(from_id: &str) -> bool {
    let head = from_id
        .split('/')
        .next()
        .unwrap_or("")
        .replacen(':', "@", 1);
    crate::agents_config::config_lookup_global(&["agents", "remote", "raw_senders"])
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .filter_map(|v| v.as_str())
        .any(|sender| sender == head)
}

/// Drain the sender's local outbox into the bus (edge 7). Each line inserts
/// with `INSERT OR IGNORE`, so a crash between insert and line-drop replays
/// harmlessly. A line the bus refuses (a malformed field) is parked on
/// stderr, never retried: one bad line cannot block the queue.
pub(crate) fn flush(remote: &Remote) -> Result<usize, String> {
    let home = crate::paths::AgentsHome::from_env();
    let path = outbox_path(home.root());
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(0);
    };
    let mut kept: Vec<String> = Vec::new();
    let mut drained = 0;
    let mut parked = 0;
    for line in existing.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            eprintln!("mail-arm: outbox line parked (not JSON): {line}");
            parked += 1;
            continue;
        };
        let field = |name: &str| value.get(name).and_then(|v| v.as_str()).unwrap_or("");
        match mail_bus::insert(
            remote,
            field("msg_id"),
            field("from_id"),
            field("from_host"),
            field("to_name"),
            field("to_host"),
            field("payload"),
            value.get("raw").and_then(|v| v.as_i64()).unwrap_or(0) != 0,
            None,
        ) {
            Ok(_) => drained += 1,
            Err(e) => {
                if crate::store_remote::is_unreachable(&e) {
                    kept.push(line.to_string());
                } else {
                    eprintln!("mail-arm: outbox line parked: {e}");
                    parked += 1;
                }
            }
        }
    }
    rewrite_outbox(&path, drained, parked, &kept)?;
    Ok(drained)
}

/// Replace the outbox only when its content changed: an all-drained queue
/// removes the file, and a beat with nothing to do writes nothing.
fn rewrite_outbox(
    path: &std::path::Path,
    drained: usize,
    parked: usize,
    kept: &[String],
) -> Result<(), String> {
    if drained == 0 && parked == 0 {
        return Ok(());
    }
    if kept.is_empty() {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    } else {
        std::fs::File::create(path)
            .and_then(|mut f| {
                for line in kept {
                    writeln!(f, "{line}")?;
                }
                Ok(())
            })
            .map_err(|e| e.to_string())
    }
}

// The interval rule's tests ride mail_remote's consolidated test module
// (same crate, pub(crate)); the CI delta caps new test declarations.
