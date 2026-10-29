//! The `sent` front command: the outbound half of `mail unread`.
//!
//! Port of the Python `mail sent` front. Lists mail this session sent, with
//! the claim state the recipient's cursor implies and the landed tri-state
//! proven from bus records or the recipient transcript. `--unclaimed`
//! applies exactly the predicate the nag applies, so what this prints is
//! what that line is counting, never a differently-scoped set.

use crate::claude_drive::claude_projects_dir;
use crate::mail_control_drain::{landed_ids, withdrawn_ids};
use crate::mail_delivery_state::newest_state;
use crate::mail_header::ids_in_text;
use crate::paths::AgentsHome;
use crate::spawn_context::resolve_self_identity;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TYPED_DELIVERY: &str = "typed";
const HOSTED_DELIVERY: &str = "hosted";
const CROSS_SESSION_DELIVERY: &str = "cross-session";
/// Absent delivery metadata is the legacy durable shape.
const DURABLE_DELIVERY: &str = "durable";
const SEND_KIND: &str = "send";
const LANDED_KIND: &str = "landed";
/// Transcript-read budget for the unclaimed scan (`_LANDED_READ_BUDGET_S`).
const LANDED_READ_BUDGET: Duration = Duration::from_millis(500);
const DEFAULT_UNCLAIMED_TTL: i64 = 1800;
const DEFAULT_LANDED_ABANDON_TTL: i64 = 21600;
const CODEX_SESSIONS_DIR_ENV: &str = "FNO_CODEX_SESSIONS_DIR";

pub fn run_sent(args: &[String]) -> i32 {
    let mut unclaimed_only = false;
    let mut json_out = false;
    let mut from_name: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--unclaimed" | "-u" => unclaimed_only = true,
            "--json" | "-J" => json_out = true,
            "--from-name" => match it.next() {
                Some(v) => from_name = Some(v.clone()),
                None => {
                    eprintln!("error: --from-name wants a label");
                    return 2;
                }
            },
            other => {
                eprintln!("sent: unknown argument {other:?}");
                return 2;
            }
        }
    }

    // `stamp_from`, not the precedence-only resolver: this must be the SAME
    // handle the send path stamped into `from`, or the outbox lists mail this
    // session did not send and hides mail it did. Unlike hold, an
    // unprovable self is NOT a hard stop: Python floors to "fno" and lists.
    let handle = match from_name {
        Some(name) => name,
        None => {
            let home = AgentsHome::from_env();
            let ident = resolve_self_identity(&|name| std::env::var(name).ok(), None, None, &home);
            match (ident.session_id, ident.harness) {
                (Some(session_id), Some(_)) => crate::identity::canonical_handle(&session_id),
                _ => "fno".to_string(),
            }
        }
    };

    let home = AgentsHome::from_env();
    let bus = bus_dir(&home);
    let all = dedupe_last_wins(read_bus(&bus));
    let now = now_secs();

    let (msgs, unclaimed_ids) = if unclaimed_only {
        let msgs = sent_unclaimed(
            &all,
            &handle,
            inbox_ttl(&["inbox", "unclaimed_ttl"], DEFAULT_UNCLAIMED_TTL),
            now,
            &bus,
        );
        let ids: HashSet<String> = msgs.iter().map(|m| str_field(m, "id", "?")).collect();
        (msgs, ids)
    } else {
        let retracted = withdrawn_ids(&all);
        // TTL of -1, not 0: the age test is strict `>` and bus timestamps are
        // whole seconds, so a message sent this second has age 0.0 and a TTL
        // of 0 would classify it CLAIMED - reporting a just-sent, unread
        // message as picked up, the opposite of the truth this command exists
        // to tell.
        let unclaimed_ids: HashSet<String> = sent_unclaimed(&all, &handle, -1, now, &bus)
            .iter()
            .map(|m| str_field(m, "id", "?"))
            .collect();
        let msgs: Vec<Value> = all
            .iter()
            .filter(|m| {
                str_field(m, "kind", "") == SEND_KIND
                    && str_field(m, "from", "") == handle
                    && !retracted.contains(&str_field(m, "id", "?"))
            })
            .cloned()
            .collect();
        (msgs, unclaimed_ids)
    };

    // `claimed`: durable cursor read past this row. A hosted row has none,
    // so it reports null; a typed row can never be drained, so it reports
    // false even though it is excluded from the unclaimed scan.
    let claimed_flag = |m: &Value| -> Option<bool> {
        let d = delivery_of(m);
        if d == HOSTED_DELIVERY {
            None
        } else if d == TYPED_DELIVERY {
            Some(false)
        } else {
            Some(!unclaimed_ids.contains(&str_field(m, "id", "?")))
        }
    };
    let landed_flag = landed_states(&all, &msgs, None);

    if json_out || !std::io::stdout().is_terminal() {
        let rows: Vec<Value> = msgs
            .iter()
            .map(|m| {
                let id = str_field(m, "id", "?");
                let mut delivery_state = newest_state(&all, &id);
                // A transcript hit is the turn record too, not only a landed row.
                if !delivery_state.is_accepted() && landed_flag.get(&id) == Some(&Some(true)) {
                    delivery_state = crate::mail_delivery_state::DeliveryState::AcceptedNewTurn {
                        evidence: crate::mail_delivery_state::Evidence {
                            record: "landed".into(),
                            ..Default::default()
                        },
                    };
                }
                json!({
                    "id": id,
                    "state": delivery_state.word(),
                    "state_detail": crate::mail_delivery_state::detail(&delivery_state),
                    "to": str_field(m, "to", ""),
                    "to_kind": m.get("to_kind").cloned().unwrap_or(Value::Null),
                    "kind": str_field(m, "kind", ""),
                    "ts": str_field(m, "ts", ""),
                    "delivery": delivery_of(m).to_string(),
                    "claimed": claimed_flag(m),
                    // A typed row is neither claimed nor claimABLE: it is
                    // excluded from the unclaimed scan, withdraw refuses it,
                    // and the recipient cannot drain it.
                    "claimable": delivery_of(m) != TYPED_DELIVERY,
                    "landed": landed_flag.get(&id).cloned().flatten().map(Value::from).unwrap_or(Value::Null),
                    // The stored envelope, wrapper included: a sender judges a
                    // stale message by its words before withdrawing it.
                    "body": str_field(m, "body", ""),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&rows).unwrap_or_else(|_| "[]".into())
        );
        return 0;
    }

    if msgs.is_empty() {
        let scope = if unclaimed_only { "unclaimed " } else { "" };
        println!("no {scope}mail sent from {handle}");
        return 0;
    }
    for m in &msgs {
        let id = str_field(m, "id", "?");
        let delivery_state = newest_state(&all, &id);
        let state = match delivery_of(m) {
            HOSTED_DELIVERY => match landed_flag.get(&id).cloned().flatten() {
                Some(true) => "landed".to_string(),
                Some(false) => format!(
                    "handed {}m ago, not in transcript",
                    age_minutes(&str_field(m, "ts", ""), now)
                ),
                _ => "handed, landing unknown".to_string(),
            },
            // `typed`: a PTY prompt can discard bytes, so never claim it was
            // consumed.
            TYPED_DELIVERY => "typed (unconfirmed)".to_string(),
            _ => {
                if claimed_flag(m).unwrap_or(false) {
                    "claimed".to_string()
                } else {
                    "UNCLAIMED".to_string()
                }
            }
        };
        let lane = m
            .get("to_kind")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("?");
        println!(
            "{}  -> {}  [{lane}/{}]  {}  {} ({state})",
            id,
            str_field(m, "to", ""),
            delivery_of(m),
            str_field(m, "ts", ""),
            crate::mail_delivery_state::receipt(&delivery_state, ""),
        );
    }
    println!(
        "\nunclaimed means the recipient never read it. Wake it first - \
         fno agents peek <recipient> to check, fno agents resume <recipient> \
         to wake (the wake lane delivers); withdraw one only when stale: \
         fno agents mail withdraw <id>"
    );
    0
}

/// The nag predicate: sent by this handle, not retracted, not already proven
/// landed, drainless deliveries excluded, younger than the abandon window,
/// and (for non-hosted rows) still past the recipient's cursor, and older
/// than `ttl_seconds`. A transcript-proven landing is recorded on the bus so
/// later scans skip the read; a hosted row stays unclaimed only on a proven
/// miss, never on "unknown".
fn sent_unclaimed(
    all: &[Value],
    handle: &str,
    ttl_seconds: i64,
    now: i64,
    bus: &Path,
) -> Vec<Value> {
    let retracted = withdrawn_ids(all);
    let already_landed = landed_ids(all);
    let abandon_ttl = inbox_ttl(&["inbox", "landed_abandon_ttl"], DEFAULT_LANDED_ABANDON_TTL);
    let sent: Vec<&Value> = all
        .iter()
        .filter(|m| {
            let d = delivery_of(m);
            str_field(m, "kind", "") == SEND_KIND
                && str_field(m, "from", "") == handle
                && !retracted.contains(&str_field(m, "id", "?"))
                && !already_landed.contains(&str_field(m, "id", "?"))
                && d != TYPED_DELIVERY
                && d != CROSS_SESSION_DELIVERY
                && !age_exceeds(&str_field(m, "ts", ""), abandon_ttl, now)
        })
        .collect();
    if sent.is_empty() {
        return Vec::new();
    }

    let pos: HashMap<String, usize> = all
        .iter()
        .enumerate()
        .map(|(i, m)| (str_field(m, "id", "?"), i))
        .collect();
    let mut recipients: Vec<String> = sent
        .iter()
        .filter(|m| delivery_of(m) != HOSTED_DELIVERY)
        .map(|m| str_field(m, "to", ""))
        .collect();
    recipients.sort();
    recipients.dedup();
    let mut cursor_pos: HashMap<String, i64> = HashMap::new();
    for r in recipients {
        let cp = match read_cursor(bus, &r) {
            // The oracle's reader swallows every failure to None, so an
            // unreadable or unresolvable cursor claims nothing (-1).
            None => -1,
            Some(cid) => pos.get(&cid).copied().map_or(-1, |p| p as i64),
        };
        cursor_pos.insert(r, cp);
    }

    let mut candidates: Vec<&Value> = Vec::new();
    for m in &sent {
        if delivery_of(m) != HOSTED_DELIVERY {
            let cp = cursor_pos
                .get(str_field(m, "to", "").as_str())
                .copied()
                .unwrap_or(all.len() as i64);
            let mp = pos.get(&str_field(m, "id", "?")).copied().unwrap_or(0) as i64;
            if mp <= cp {
                continue; // durable: claimed via cursor
            }
        }
        if !age_exceeds(&str_field(m, "ts", ""), ttl_seconds, now) {
            continue; // still fresh (strict >)
        }
        candidates.push(m);
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    let stop = Instant::now().checked_add(LANDED_READ_BUDGET);
    let owned: Vec<Value> = candidates.iter().map(|m| (*m).clone()).collect();
    let states = landed_states(all, &owned, stop);
    let mut out = Vec::new();
    for m in candidates {
        let id = str_field(m, "id", "?");
        if states.get(&id).cloned().flatten() == Some(true) {
            // Persists past a transcript rotation; later scans skip the read.
            record_landed(bus, m);
            continue;
        }
        if delivery_of(m) == HOSTED_DELIVERY && states.get(&id).cloned().flatten() != Some(false) {
            continue; // hosted: unknown is not a proven miss
        }
        out.push((*m).clone());
    }
    out
}

/// Landed tri-state for `msgs`: durable proof first, then a transcript read.
fn landed_states(
    all: &[Value],
    msgs: &[Value],
    stop: Option<Instant>,
) -> HashMap<String, Option<bool>> {
    let already = landed_ids(all);
    let to_check: Vec<&Value> = msgs
        .iter()
        .filter(|m| !already.contains(&str_field(m, "id", "?")))
        .collect();
    let mut out: HashMap<String, Option<bool>> = msgs
        .iter()
        .map(|m| (str_field(m, "id", "?"), Some(true)))
        .collect();
    out.extend(landed_map(&to_check, stop));
    out
}

/// Transcript verdicts for rows without a bus record. Absence of evidence
/// (no routing meta, a self-send, an unreadable transcript) stays None, never
/// false: a transcript that cannot be read is not proof of a miss.
fn landed_map(msgs: &[&Value], stop: Option<Instant>) -> HashMap<String, Option<bool>> {
    let mut out: HashMap<String, Option<bool>> = msgs
        .iter()
        .map(|m| (str_field(m, "id", "?"), None))
        .collect();
    let mut groups: HashMap<(String, String), Vec<&Value>> = HashMap::new();
    for m in msgs {
        let meta = m.get("meta");
        let to_session = meta
            .and_then(|x| x.get("to_session"))
            .and_then(Value::as_str);
        let to_harness = meta
            .and_then(|x| x.get("to_harness"))
            .and_then(Value::as_str);
        let from_session = m.get("from_session").and_then(Value::as_str);
        if let (Some(ts), Some(fs)) = (to_session, from_session) {
            if !ts.is_empty() && ts == fs {
                continue; // self-send: the sender's own transcript proves nothing
            }
        }
        let (Some(to_harness), Some(to_session)) = (to_harness, to_session) else {
            continue;
        };
        let id = str_field(m, "id", "?");
        if !str_field(m, "body", "").contains(&format!("id=\"{id}\"")) {
            continue;
        }
        groups
            .entry((to_harness.to_string(), to_session.to_string()))
            .or_default()
            .push(m);
    }
    for ((harness, session), members) in groups {
        let ids = mail_ids_in_transcript(&harness, &session);
        for m in members {
            let id = str_field(m, "id", "?");
            let verdict = ids.as_ref().map(|set| set.contains(&id));
            out.insert(id, verdict);
        }
        if let Some(stop) = stop {
            if Instant::now() > stop {
                break;
            }
        }
    }
    out
}

/// Every delivered-mail id in `(harness, session)`'s own transcript, or None
/// when the transcript cannot be resolved or read (never evidence of
/// absence).
fn mail_ids_in_transcript(harness: &str, session: &str) -> Option<HashSet<String>> {
    let path = match harness {
        "claude" => {
            let base = claude_projects_dir();
            let mut dirs: Vec<PathBuf> = std::fs::read_dir(&base)
                .ok()?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect();
            dirs.sort();
            let name = format!("{session}.jsonl");
            dirs.into_iter()
                .find(|d| d.join(&name).is_file())
                .map(|d| d.join(name))?
        }
        "codex" => {
            let base = codex_sessions_dir();
            codex_transcript_path(&base, session)?
        }
        _ => return None,
    };
    let text = std::fs::read_to_string(path).ok()?;
    // The transcript is a JSONL carrier: un-escape the embedded text before
    // the id scan, exactly as the Python reader does.
    Some(
        ids_in_text(&text.replace("\\\"", "\""))
            .into_iter()
            .collect(),
    )
}

/// Codex rollouts embed the session id in the file name anywhere in the
/// sessions tree, so the scan is a recursive substring match, not an exact
/// name.
fn codex_transcript_path(base: &Path, session: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, session: &str, depth: u8) -> Option<PathBuf> {
        if depth > 6 {
            return None;
        }
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(dir)
            .ok()?
            .filter_map(Result::ok)
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path();
            if p.is_dir() {
                if let Some(hit) = walk(&p, session, depth + 1) {
                    return Some(hit);
                }
            } else {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".jsonl") && name.contains(session) {
                    return Some(p);
                }
            }
        }
        None
    }
    walk(base, session, 0)
}

fn codex_sessions_dir() -> PathBuf {
    if let Some(v) = std::env::var_os(CODEX_SESSIONS_DIR_ENV) {
        return PathBuf::from(v);
    }
    if let Some(v) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(v).join("sessions");
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".codex").join("sessions")
}

/// Persist a landing so later scans skip the transcript read. Uses the
/// appender without the sender guard: the Python recorder stamps the bare
/// fleet name and the Rust sender gate would refuse it.
fn record_landed(bus: &Path, m: &Value) {
    let id = crate::announce::new_msg_id();
    let env = json!({
        "v": 1,
        "id": id,
        "thread": id,
        "ts": crate::graph_store::now_isoformat(),
        "from": str_field(m, "from", ""),
        "to": str_field(m, "to", ""),
        "kind": LANDED_KIND,
        "body": "",
        "meta": {"landed": str_field(m, "id", "?")},
    });
    let live = bus.join("messages.jsonl");
    if let Err(e) = crate::announce::append_line_open(&live, &env, &crate::chats::chats_dir()) {
        eprintln!("landed record failed: {e}");
    }
}

/// The per-recipient read cursor, or None when absent, blank, corrupt, or
/// unreadable: the oracle's reader warns on stderr and swallows every
/// failure to None, so an unresolvable cursor claims nothing.
fn read_cursor(bus: &Path, name: &str) -> Option<String> {
    let path = bus.join("cursors").join(format!("{name}.json"));
    let label = path.file_name().and_then(|n| n.to_str()).unwrap_or(name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!(
                "bus cursor: ignoring corrupt cursor {label} ({}); rescanning retained segments",
                e.kind()
            );
            return None;
        }
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        eprintln!(
            "bus cursor: ignoring corrupt cursor {label} (JSONDecodeError); rescanning retained segments"
        );
        return None;
    };
    v.get("last_seen_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn delivery_of(m: &Value) -> &str {
    m.get("delivery")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(DURABLE_DELIVERY)
}

/// True iff `ts` is strictly older than `ttl`. Unparseable -> false.
fn age_exceeds(ts: &str, ttl: i64, now: i64) -> bool {
    crate::intel::ts_secs(ts).is_some_and(|t| now - (t as i64) > ttl)
}

fn age_minutes(ts: &str, now: i64) -> i64 {
    crate::intel::ts_secs(ts)
        .map(|t| (now - (t as i64)) / 60)
        .unwrap_or(0)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `config.inbox.<key>` from the resolved settings, or the Python default.
fn inbox_ttl(keys: &[&str], default: i64) -> i64 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    crate::agents_config::config_lookup(&cwd, keys)
        .and_then(|v| v.as_integer())
        .unwrap_or(default)
}

fn bus_dir(home: &AgentsHome) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let root = crate::agents_config::state_dir(&cwd).unwrap_or_else(|| home.root().to_path_buf());
    crate::intel::bus_dir_of(&root)
}

/// Every retained envelope oldest -> newest: the rotated `<live>.<N>`
/// segments highest N first, then the live segment, so a cursor keyed by
/// message-id resolves across rotations (bus/log.py `iter_messages`).
fn read_bus(bus: &Path) -> Vec<Value> {
    let live = bus.join("messages.jsonl");
    let mut segments = rotated_segments_oldest_first(&live);
    segments.push(live);
    let mut out: Vec<Value> = Vec::new();
    for seg in &segments {
        let Ok(text) = std::fs::read_to_string(seg) else {
            continue;
        };
        out.extend(
            text.lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok()),
        );
    }
    out
}

/// The rotated siblings of the live segment, oldest (highest N) first; the
/// live path itself is appended last (bus/log.py
/// `_segment_paths_oldest_first`).
fn rotated_segments_oldest_first(live: &Path) -> Vec<PathBuf> {
    let Some(parent) = live.parent() else {
        return Vec::new();
    };
    let Some(name) = live.file_name() else {
        return Vec::new();
    };
    let prefix = format!("{}.", name.to_string_lossy());
    let mut rotated: Vec<(u64, PathBuf)> = std::fs::read_dir(parent)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let entry = e.file_name().to_string_lossy().into_owned();
            let suffix = entry.strip_prefix(prefix.as_str())?;
            if suffix.is_empty() || !suffix.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some((suffix.parse::<u64>().ok()?, e.path()))
        })
        .collect();
    rotated.sort_by(|a, b| b.0.cmp(&a.0));
    rotated.into_iter().map(|(_, p)| p).collect()
}

fn str_field(m: &Value, key: &str, blank: &str) -> String {
    m.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(blank)
        .to_string()
}

/// One row per id, last wins: a staged body row and its receipt or
/// durable-fallback twin share an id, and the outbox reads the receipt row's
/// final state. First-seen position is kept.
fn dedupe_last_wins(msgs: Vec<Value>) -> Vec<Value> {
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<Value> = Vec::new();
    for m in msgs {
        let id = str_field(&m, "id", "?");
        match index.get(&id) {
            Some(i) => out[*i] = m,
            None => {
                index.insert(id, out.len());
                out.push(m);
            }
        }
    }
    out
}
