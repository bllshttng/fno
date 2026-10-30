//! `fno agents peek <handle>`: the read-only twin of `fno agents mail send`.
//! A read leaf with no action of its own: the router rewrites it into
//! `status --peek`, because the binary's action list is shrink-only.
//!
//! Peek resolves a handle the way a truth read does and tails the same
//! records through [`crate::session_truth::session_records`], so peek, truth
//! and the lead check-in read one transcript reader. A pane worker with no
//! session id yet reads its mux pane screen. Peek output is NOT a liveness
//! signal: for a verdict use `fno agents truth`. It writes nothing.

use crate::client_commands::py_repr_str;
use crate::session_truth::{
    record_parser, resolve_session, row_falsified, session_records, session_transcript,
    RecordsError, Stores, TruthRecord, TruthSession,
};
use crate::state::{MuxRef, RegistryEntry};
use crate::wake_read::python_json_default;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const EXIT_OK: i32 = 0;
/// A known peer with no reader arm: distinct from "not found".
const EXIT_UNSUPPORTED: i32 = 1;
/// Parity with mail send's unresolvable-handle exit.
const EXIT_NOT_FOUND: i32 = 13;

const STATUS_KINDS: [&str; 4] = ["task_started", "task_done", "blocked", "run_summary"];

/// Registry statuses that claim a running worker.
const LIVE_STATUSES: [&str; 6] = ["spawning", "ready", "idle", "busy", "live", "restarting"];

const HELP: &str = "\
Usage: fno agents peek [OPTIONS] [HANDLE]

  Observe a peer read-only.

  Tails the transcript (claude/codex), preferring normalized status events; a
  pane worker resolves through the registry's mux ref. Never writes anything
  the peer reads. Exit 13 unknown peer, 1 no reader, 0 observed (or \"no
  activity yet\").

Options:
  -n, --lines INTEGER  Show the last N transcript records (default 15; 0 for
                       none).
  -f, --follow         Stream new records as the peer emits them (read-only).
  --grep TEXT          Only records carrying this token, within the --lines
                       window.
  -A, --all            Search every registry session with a transcript instead
                       of one handle.
  -J, --json           Emit JSON-Lines rows instead of human lines.
  --help               Show this message and exit.
";

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

// SAFETY: the handler touches only an AtomicBool. No allocations, no locks.
extern "C" fn on_sigint(_: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

struct Args {
    handle: Option<String>,
    lines: usize,
    follow: bool,
    grep: Option<String>,
    all: bool,
    json: bool,
}

fn parse(args: &[String]) -> Result<Args, i32> {
    let args = crate::client_commands::expand_eq(args);
    let mut out = Args {
        handle: None,
        lines: 15,
        follow: false,
        grep: None,
        all: false,
        json: false,
    };
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Err(0);
            }
            "--follow" | "-f" => out.follow = true,
            "--all" | "-A" => out.all = true,
            "--json" | "-J" => out.json = true,
            "--lines" | "-n" | "--grep" => {
                let Some(value) = it.next() else {
                    eprintln!("fno agents peek: {arg} requires a value");
                    return Err(2);
                };
                if arg == "--grep" {
                    out.grep = Some(value);
                    continue;
                }
                match value.parse::<i64>() {
                    Ok(n) if n < 0 => {
                        eprintln!("--lines must be >= 0 (got {n})");
                        return Err(2);
                    }
                    Ok(n) => out.lines = n as usize,
                    Err(_) => {
                        eprintln!("fno agents peek: {value:?} is not a valid integer");
                        return Err(2);
                    }
                }
            }
            s if s.starts_with('-') && s.len() > 1 => {
                eprintln!("fno agents peek: no such option: {s}");
                return Err(2);
            }
            _ if out.handle.is_none() => out.handle = Some(arg),
            s => {
                eprintln!("fno agents peek: got unexpected extra argument ({s})");
                return Err(2);
            }
        }
    }
    if out.all && out.handle.is_some() {
        eprintln!("--all searches every session; drop the handle argument");
        return Err(2);
    }
    if !out.all && out.handle.is_none() {
        eprintln!("usage: fno agents peek <handle>  (or --all)");
        return Err(2);
    }
    if out.follow && (out.all || out.grep.is_some()) {
        eprintln!("--follow streams one peer; --all/--grep read a tail");
        return Err(2);
    }
    Ok(out)
}

/// `status --peek [HANDLE] [-n N] [-f] [--grep T] [-A] [-J]`.
pub fn run_peek(args: &[String]) -> i32 {
    let args = match parse(args) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let rows = load_rows();
    let stores = Stores::ambient();
    let mut out = std::io::stdout().lock();
    if args.all {
        return peek_all(rows.as_deref(), &stores, &args, &mut out);
    }
    let handle = args.handle.clone().unwrap_or_default();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let events = crate::agents_config::state_dir(&cwd)
        .map(|d| d.join("events.jsonl"))
        .filter(|p| p.exists());
    peek_one(
        &handle,
        rows.as_deref(),
        &stores,
        events.as_deref(),
        &args,
        &mut out,
    )
}

fn load_rows() -> Option<Vec<RegistryEntry>> {
    crate::state::load_registry(&crate::paths::AgentsHome::from_env().registry_json())
        .ok()
        .map(|r| r.entries)
}

fn emit(out: &mut impl Write, line: &str) {
    let _ = writeln!(out, "{line}");
}

fn emit_record(out: &mut impl Write, rec: &TruthRecord, json_out: bool) {
    if json_out {
        emit(
            out,
            &python_json_default(&json!({"role": rec.role, "text": rec.text})),
        );
    } else {
        emit(out, &format!("{}: {}", rec.role, rec.text));
    }
}

fn emit_no_activity(out: &mut impl Write, json_out: bool) {
    if json_out {
        emit(
            out,
            &python_json_default(&json!({"status": "no activity yet"})),
        );
    } else {
        emit(out, "no activity yet");
    }
}

/// Flip `user` records whose text IS a delivered mail turn to `peer`.
fn resolve_peer_roles(mut records: Vec<TruthRecord>) -> Vec<TruthRecord> {
    for rec in records.iter_mut().filter(|r| r.role == "user") {
        if crate::mail_header::classify(&rec.text) != crate::mail_header::Framing::Bare {
            rec.role = "peer".to_string();
        }
    }
    records
}

/// The registry row naming `handle` by name, alias or short id, when exactly
/// one does.
fn row_named<'a>(rows: Option<&'a [RegistryEntry]>, handle: &str) -> Option<&'a RegistryEntry> {
    let mut hits = rows?.iter().filter(|r| {
        r.name == handle
            || r.aliases.iter().any(|a| a == handle)
            || (!r.short_id.is_empty() && r.short_id == handle)
    });
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

/// The registry row whose stored name or session id (any handle tier)
/// matches `handle`, when exactly one does. No alias resolution: the
/// diagnostic must name the exact row asked for.
fn row_exact<'a>(rows: Option<&'a [RegistryEntry]>, handle: &str) -> Option<&'a RegistryEntry> {
    let mut hits = rows?.iter().filter(|r| {
        r.name == handle
            || r.harness_session_id
                .as_deref()
                .is_some_and(|sid| crate::identity::session_handle_tier(handle, sid).is_some())
    });
    let first = hits.next()?;
    hits.next().is_none().then_some(first)
}

fn pane_of(row: &RegistryEntry) -> Option<&MuxRef> {
    row.mux
        .as_ref()
        .filter(|m| !m.session.trim().is_empty() && m.pane_id >= 1)
}

/// The last `n` lines of a mux pane's scrollback, through the shared pane
/// reader; `None` when the mux does not answer or the pane is blank.
fn read_pane(mux: &MuxRef, n: usize) -> Option<String> {
    let bin = crate::scrape::fno_bin();
    let text = crate::scrape::mux_pane_read(&bin, Some(&mux.session), mux.pane_id)?;
    let lines: Vec<&str> = text.trim_end().lines().collect();
    let tail = lines[lines.len().saturating_sub(n)..].join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// A pane worker's screen text, read in process: the lead check-in's
/// fallback when a pane row has no transcript yet.
pub(crate) fn pane_texts(handle: &str, n: usize) -> Result<Vec<String>, String> {
    let rows = load_rows();
    let mux = row_named(rows.as_deref(), handle)
        .and_then(pane_of)
        .ok_or_else(|| format!("{handle} is no pane worker"))?;
    let text = read_pane(mux, n).ok_or_else(|| "mux pane read did not answer".to_string())?;
    Ok(vec![text])
}

/// Observe a pane-substrate worker, or `None` to let the caller fall
/// through. A pane the mux says is gone falls through to the row read.
fn try_mux_pane(
    handle: &str,
    row: Option<&RegistryEntry>,
    args: &Args,
    out: &mut impl Write,
) -> Option<i32> {
    let mux = row.and_then(pane_of)?;
    let Some(text) = read_pane(mux, args.lines) else {
        if crate::session_truth::pane_gone(mux) {
            eprintln!(
                "{handle}: pane {} is gone; the worker exited. Resume: fno agents resume {handle}",
                mux.pane_id
            );
            return None;
        }
        eprintln!(
            "{handle} is a pane worker (mux pane {}, session {}); the mux did not answer. \
             Read it directly: fno mux pane read {}",
            mux.pane_id, mux.session, mux.pane_id
        );
        return Some(EXIT_UNSUPPORTED);
    };
    if args.json {
        emit(
            out,
            &python_json_default(&json!({
                "pane": handle,
                "mux_session": mux.session,
                "pane_id": mux.pane_id,
                "text": text.trim_end(),
            })),
        );
    } else {
        let _ = write!(
            out,
            "peer {handle}: mux pane={}:{}\n{text}",
            mux.session, mux.pane_id
        );
        if !text.ends_with('\n') {
            let _ = writeln!(out);
        }
    }
    Some(EXIT_OK)
}

/// Rendered status lines for this session from the shared events file, or
/// empty to fall through to the transcript. Both envelopes parse: Python's
/// `{type, data}` and Rust's flat `{kind, ...}`.
fn status_events(events: Option<&Path>, short_id: &str, session_id: &str) -> Vec<String> {
    let Some(path) = events else {
        return Vec::new();
    };
    let mut wants: Vec<String> = [short_id, session_id]
        .iter()
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .collect();
    if !short_id.is_empty() {
        wants.push(format!("worker:{short_id}"));
    }
    // Nothing identifies this session, so nothing in a SHARED events file
    // can be attributed to it.
    if wants.is_empty() {
        return Vec::new();
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(obj) = rec.as_object() else {
            continue;
        };
        let kind = [obj.get("kind"), obj.get("type")]
            .into_iter()
            .flatten()
            .find(|v| v.as_str().is_some_and(|s| !s.is_empty()))
            .and_then(Value::as_str)
            .unwrap_or("");
        if !STATUS_KINDS.contains(&kind) {
            continue;
        }
        let data = obj.get("data").filter(|d| d.is_object()).unwrap_or(&rec);
        let ident = ["short_id", "session_id", "source", "worker"]
            .iter()
            .find_map(|f| {
                [data.get(*f), rec.get(*f)]
                    .into_iter()
                    .flatten()
                    .find_map(|v| v.as_str().filter(|s| !s.is_empty()))
            })
            .unwrap_or("");
        if !ident.is_empty()
            && !wants.iter().any(|w| w == ident)
            && !wants.iter().any(|w| ident.contains(w.as_str()))
        {
            continue;
        }
        lines.push(format!("  [{kind}] {ident}").trim_end().to_string());
    }
    lines
}

fn peek_one(
    handle: &str,
    rows: Option<&[RegistryEntry]>,
    stores: &Stores,
    events: Option<&Path>,
    args: &Args,
    out: &mut impl Write,
) -> i32 {
    // A resolved row with no session id names no conversation: a pane worker
    // before reconcile backfills it, or an incomplete identity.
    let mut session = resolve_session(rows, handle, stores).filter(|s| !s.session_id.is_empty());
    // A session read from a falsified registry row comes from durable state,
    // not from a live writer: never tail it.
    let mut row_derived = session
        .as_ref()
        .and_then(|s| s.row.as_ref())
        .is_some_and(row_falsified);
    if session.is_none() {
        if let Some(rc) = try_mux_pane(handle, row_named(rows, handle), args, out) {
            return rc;
        }
        let row = row_exact(rows, handle);
        if let Some(row) = row {
            let status = serde_json::to_value(row.status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let sid = row.harness_session_id.as_deref().unwrap_or("");
            if LIVE_STATUSES.contains(&status.as_str()) && sid.is_empty() {
                eprintln!(
                    "registry row {handle} exists with status={status} but harness_session_id \
                     is missing; worker identity is incomplete"
                );
                return EXIT_UNSUPPORTED;
            }
            if !sid.is_empty() {
                session = Some(TruthSession {
                    agent: row.harness.clone().unwrap_or_else(|| "claude".into()),
                    session_id: sid.to_string(),
                    transcript_path: row.transcript_path.as_deref().map(PathBuf::from),
                    row: Some(row.clone()),
                });
                row_derived = true;
            }
        }
    }
    let Some(session) = session else {
        eprintln!("peer not found in the registry: {handle}");
        eprintln!(
            "the registry is not the transcript. A reaped row leaves its transcript on disk."
        );
        eprintln!("try: fno agents adopt {handle}");
        return EXIT_NOT_FOUND;
    };
    let agent = session.agent.as_str();
    let session_id = session.session_id.as_str();
    let (short_id, cwd) = match session.row.as_ref() {
        Some(r) => (r.short_id.clone(), r.cwd.clone()),
        None => (session_id.chars().take(8).collect(), String::new()),
    };
    if !args.json {
        // `recorded_cwd`: the registry's launch-time snapshot, never a live pwd.
        emit(
            out,
            &format!("peer {handle}: agent={agent} short_id={short_id} recorded_cwd={cwd}"),
        );
    }

    let status = status_events(events, &short_id, session_id);
    if !status.is_empty() {
        for line in &status {
            if args.json {
                emit(out, &python_json_default(&json!({"status": line.trim()})));
            } else {
                emit(out, line);
            }
        }
        return EXIT_OK;
    }

    let records = match session_records(&session, stores, args.lines) {
        Ok(records) => resolve_peer_roles(records),
        Err(RecordsError::Unsupported(other)) => {
            eprintln!("observe not yet supported for {other}");
            return EXIT_UNSUPPORTED;
        }
        Err(RecordsError::NoTranscript | RecordsError::Read(_)) => Vec::new(),
    };

    if let Some(token) = args.grep.as_deref() {
        // A search reports its count: a zero with exit 0 is a measured
        // absence, never a silent one.
        let hits: Vec<&TruthRecord> = records
            .iter()
            .filter(|r| r.text.contains(token) || r.role.contains(token))
            .collect();
        eprintln!(
            "grep {}: {} matching record(s) in {agent} {session_id}",
            py_repr_str(token),
            hits.len()
        );
        for rec in &hits {
            emit_record(out, rec, args.json);
        }
        if hits.is_empty() {
            emit_no_activity(out, args.json);
        }
        return EXIT_OK;
    }

    if records.is_empty() && row_derived && args.lines > 0 {
        let resolves = agent == "opencode" || session_transcript(&session, stores).is_some();
        if !resolves {
            eprintln!(
                "registry row {handle} exists ({agent} session {session_id}), but no {agent} \
                 transcript resolved for it. The row outlived its store: the session was pruned, \
                 or its store is not mounted here."
            );
            return EXIT_NOT_FOUND;
        }
        eprintln!(
            "registry row {handle} exists and its {agent} transcript resolved, but no records \
             were read from it."
        );
        return EXIT_OK;
    }

    if records.is_empty() && !args.follow {
        emit_no_activity(out, args.json);
        return EXIT_OK;
    }
    for rec in &records {
        emit_record(out, rec, args.json);
    }
    if records.is_empty() {
        emit_no_activity(out, args.json);
    }
    if !args.follow {
        return EXIT_OK;
    }
    if row_derived {
        eprintln!(
            "--follow: {handle} resolved from durable state (a registry row or transcript store), \
             not from a live session, so there is no writer to tail; showed the tail only. For a \
             liveness verdict: fno agents truth"
        );
        return EXIT_OK;
    }
    let (Some(parse), Some(path)) = (record_parser(agent), session_transcript(&session, stores))
    else {
        if agent == "opencode" {
            eprintln!("--follow not supported for opencode; showed the tail only");
        } else {
            eprintln!("could not resolve a transcript to follow for {agent}");
        }
        return EXIT_OK;
    };
    let _ = out.flush();
    follow_records(&path, parse, args.json, out);
    EXIT_OK
}

/// Stream new parsed records as the transcript grows. Reads only complete
/// lines, so a mid-write partial waits for its completion. Leaves when the
/// file rotates or disappears, or on Ctrl-C.
fn follow_records(
    path: &Path,
    parse: fn(&Value) -> Option<TruthRecord>,
    json_out: bool,
    out: &mut impl Write,
) {
    use std::io::{Read, Seek, SeekFrom};
    use std::os::unix::fs::MetadataExt;
    // SAFETY: installs an async-signal-safe handler that only sets an atomic.
    unsafe {
        libc::signal(libc::SIGINT, on_sigint as *const () as libc::sighandler_t);
    }
    let gone = || eprintln!("transcript disappeared: {}", path.display());
    let Ok(initial) = std::fs::metadata(path) else {
        gone();
        return;
    };
    let Ok(mut file) = std::fs::File::open(path) else {
        gone();
        return;
    };
    let mut pos = file.seek(SeekFrom::End(0)).unwrap_or(0);
    let mut pending: Vec<u8> = Vec::new();
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            return;
        }
        let mut chunk = Vec::new();
        let _ = file.read_to_end(&mut chunk);
        if !chunk.is_empty() {
            pos += chunk.len() as u64;
            pending.extend_from_slice(&chunk);
            // Keep a partial trailing line until the writer finishes it.
            let cut = pending
                .iter()
                .rposition(|b| *b == b'\n')
                .map_or(0, |i| i + 1);
            let complete: Vec<u8> = pending.drain(..cut).collect();
            for line in String::from_utf8_lossy(&complete).lines() {
                let Ok(v) = serde_json::from_str::<Value>(line.trim()) else {
                    continue;
                };
                if let Some(rec) = parse(&v) {
                    emit_record(out, &rec, json_out);
                    let _ = out.flush();
                }
            }
            continue;
        }
        let Ok(now) = std::fs::metadata(path) else {
            gone();
            return;
        };
        if now.ino() != initial.ino() || now.len() < pos {
            eprintln!("transcript rotated: {}", path.display());
            return;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// `--all`: the tail (and `--grep`) over every registry session with a
/// transcript. The stderr summary names sessions READ, so zero matches across
/// a searched corpus never reads as an unsearched one.
fn peek_all(
    rows: Option<&[RegistryEntry]>,
    stores: &Stores,
    args: &Args,
    out: &mut impl Write,
) -> i32 {
    let Some(rows) = rows else {
        eprintln!("registry unreadable; --all has no sessions to enumerate");
        return EXIT_UNSUPPORTED;
    };
    let mut seen = std::collections::HashSet::new();
    let (mut sessions_read, mut matched_total) = (0usize, 0usize);
    for row in rows {
        let (Some(sid), Some(agent)) = (
            row.harness_session_id.as_deref().filter(|s| !s.is_empty()),
            row.harness.as_deref().filter(|s| !s.is_empty()),
        ) else {
            continue;
        };
        if !seen.insert((agent.to_string(), sid.to_string())) {
            continue;
        }
        let session = TruthSession {
            agent: agent.to_string(),
            session_id: sid.to_string(),
            transcript_path: row.transcript_path.as_deref().map(PathBuf::from),
            row: Some(row.clone()),
        };
        let records = match session_records(&session, stores, args.lines) {
            Ok(records) => resolve_peer_roles(records),
            Err(RecordsError::Unsupported(_)) => continue,
            Err(_) => Vec::new(),
        };
        sessions_read += 1;
        let hits: Vec<&TruthRecord> = match args.grep.as_deref() {
            Some(t) => records
                .iter()
                .filter(|r| r.text.contains(t) || r.role.contains(t))
                .collect(),
            None => records.iter().collect(),
        };
        matched_total += hits.len();
        if args.grep.is_some() && hits.is_empty() {
            continue;
        }
        if !args.json {
            emit(out, &format!("# {agent} {sid}"));
        }
        for r in hits {
            if args.json {
                emit(
                    out,
                    &python_json_default(&json!({
                        "session_id": sid, "agent": agent, "role": r.role, "text": r.text,
                    })),
                );
            } else {
                emit(out, &format!("{}: {}", r.role, r.text));
            }
        }
    }
    match args.grep.as_deref() {
        Some(t) => eprintln!(
            "grep {}: {matched_total} matching record(s) across {sessions_read} session(s)",
            py_repr_str(t)
        ),
        None => eprintln!("{sessions_read} session(s) read"),
    }
    EXIT_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("peek-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_empty_short_id_never_claims_other_workers_status() {
        let dir = tmp("status");
        let events = dir.join("events.jsonl");
        std::fs::write(
            &events,
            concat!(
                r#"{"kind":"task_done","source":"worker:abcd1234"}"#,
                "\n",
                r#"{"type":"blocked","data":{"short_id":"ffff0000"}}"#,
                "\n",
            ),
        )
        .unwrap();
        assert!(status_events(Some(&events), "", "").is_empty());
        assert_eq!(
            status_events(Some(&events), "abcd1234", "sid"),
            vec!["  [task_done] worker:abcd1234".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_delivered_mail_turn_reads_as_peer() {
        let records = vec![
            TruthRecord {
                role: "user".into(),
                text: "<cross-session-message from-name=\"lead\">\nhi\n</cross-session-message>"
                    .into(),
                timestamp: None,
            },
            TruthRecord {
                role: "user".into(),
                text: "plain prompt".into(),
                timestamp: None,
            },
        ];
        let roles: Vec<String> = resolve_peer_roles(records)
            .into_iter()
            .map(|r| r.role)
            .collect();
        assert_eq!(roles, vec!["peer", "user"]);
    }
}
