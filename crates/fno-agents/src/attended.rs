//! The attended-session guard: a typed user turn inside the
//! window holds a session against every reap and stop. The age instrument
//! can read a live transcript quiet (an account store the probe misses, a
//! resumed session id); the turn record itself is the witness that cannot
//! miss, classified by the same provenance rules `fno-agents intel` folds
//! with. A done ledger never overrides an attended session.

use crate::provenance::{
    classify_turn, is_user_turn, turn_text, turn_ts_epoch, BusIndex, HarnessKind, Provenance,
};
use crate::state::RegistryEntry;
use serde_json::Value;
use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// How recent a typed turn must be to hold a session: 15 minutes.
pub(crate) const WINDOW_SECS: i64 = 15 * 60;

/// A person spoke: the witnessed operator turn, the unbound typed turn,
/// and a hand-run slash command (the operator acting) all count; keepalive
/// pings, mail and relay envelopes, and every synthetic harness shape do
/// not.
fn typed(p: Provenance) -> bool {
    matches!(
        p,
        Provenance::Operator
            | Provenance::Unknown
            | Provenance::Harness(HarnessKind::CommandInvocation)
    )
}

/// First `open`..`close` block body in `text`, trimmed.
fn xml_block<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = &text[start..];
    let body = match rest.find(close) {
        Some(end) => &rest[..end],
        None => rest,
    };
    Some(body.trim())
}

/// True when a CommandInvocation turn's text is the row's recorded spawn
/// payload: the machine's seed, never a person's turn. Two shapes on the
/// wire - the raw payload (harnesses that log the prompt verbatim) and
/// claude's rendering of a seeded slash command, where the payload's first
/// token becomes `<command-name>` and the remainder lands inside
/// `<command-args>`. Compared on the reminder-stripped text the classifier
/// reads; anything subtler fails open, keeping today's hold rather than
/// exempting a turn a person typed.
fn is_spawn_seed_turn(text: &str, seed: &str) -> bool {
    let text = crate::provenance::system_reminder_re()
        .replace_all(text, "")
        .to_string();
    let text = text.trim();
    let seed = seed.trim();
    if text.is_empty() || seed.is_empty() {
        return false;
    }
    if text == seed {
        return true;
    }
    let Some(name) = xml_block(text, "<command-name>", "</command-name>") else {
        return false;
    };
    let args = xml_block(text, "<command-args>", "</command-args>").unwrap_or("");
    let mut seed_parts = seed.splitn(2, char::is_whitespace);
    let verb = seed_parts.next().unwrap_or("");
    let rest = seed_parts.next().unwrap_or("").trim();
    name == verb && args == rest
}

/// The bus index both arms classify with, read off the state root the way
/// `liveness_sweep` reads it.
fn bus_for(home: &crate::paths::AgentsHome) -> BusIndex {
    BusIndex::load(&crate::intel::bus_log_path(
        home.root().parent().unwrap_or(home.root()),
    ))
}

/// The newest typed turn in ONE transcript, as an age in seconds; `None`
/// when the file holds none. A row without a parseable timestamp cannot
/// testify; a future stamp reads as now. This is the RECENCY read the
/// 15-minute window hold and the stop refusal use: a person spoke a
/// moment ago, whatever machine turn followed.
fn typed_age_in_file(
    path: &PathBuf,
    bus: &BusIndex,
    session: &str,
    now: i64,
    spawn_payload: Option<&str>,
) -> Option<i64> {
    let Ok(file) = std::fs::File::open(path) else {
        return None;
    };
    let mut newest: Option<i64> = None;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(obj) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if !is_user_turn(&obj) {
            continue;
        }
        let prov = classify_turn(&obj, bus, session);
        if matches!(prov, Provenance::Harness(HarnessKind::CommandInvocation))
            && spawn_payload.is_some_and(|seed| is_spawn_seed_turn(&turn_text(&obj), seed))
        {
            continue;
        }
        if !typed(prov) {
            continue;
        }
        let Some(ts) = turn_ts_epoch(&obj) else {
            continue;
        };
        let age = (now - ts as i64).max(0);
        newest = Some(newest.map_or(age, |n: i64| n.min(age)));
    }
    newest
}

/// The LAST readable turn in ONE transcript: the newest user-shaped row
/// whose reminder-stripped text is non-empty, as `(ts, Some(age))` when it
/// typed and `(ts, None)` when it did not; `None` when the file holds no
/// readable turn. Empty rows are the harness delivering tool results -
/// machinery, never a turn a person made, and never the turn that answers
/// "did the person speak last". A row without a parseable timestamp cannot
/// testify; a future stamp reads as now.
fn last_turn_in_file(
    path: &PathBuf,
    bus: &BusIndex,
    session: &str,
    now: i64,
    spawn_payload: Option<&str>,
) -> Option<(f64, Option<i64>)> {
    let Ok(file) = std::fs::File::open(path) else {
        return None;
    };
    // The newest readable turn so far. `>=` so the row written later wins
    // a shared stamp - append order is event order.
    let mut last: Option<(f64, Option<i64>)> = None;
    for line in std::io::BufReader::new(file).split(b'\n') {
        let Ok(line) = line else { continue };
        let Ok(obj) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        if !is_user_turn(&obj) {
            continue;
        }
        // The readable-turn cut mirrors the classifier's own no_user_text
        // shape: a turn whose reminder-stripped text is empty is the
        // harness delivering tool results, not a turn that decides.
        let cleaned_empty = crate::provenance::system_reminder_re()
            .replace_all(turn_text(&obj).trim(), "")
            .trim()
            .is_empty();
        if cleaned_empty {
            continue;
        }
        let Some(ts) = turn_ts_epoch(&obj) else {
            continue;
        };
        if last.is_some_and(|(prev, _)| ts < prev) {
            continue;
        }
        let prov = classify_turn(&obj, bus, session);
        let seed_command = matches!(prov, Provenance::Harness(HarnessKind::CommandInvocation))
            && spawn_payload.is_some_and(|seed| is_spawn_seed_turn(&turn_text(&obj), seed));
        let verdict = if !seed_command && typed(prov) {
            Some((now - ts as i64).max(0))
        } else {
            None
        };
        last = Some((ts, verdict));
    }
    last
}

/// Age in seconds of the newest typed user turn across `paths`, `None`
/// when no transcript holds one. A row without a parseable timestamp
/// cannot testify; a future stamp reads as now. `spawn_payload` is the
/// row's recorded seed: a CommandInvocation turn matching it is the
/// machine's spawn payload, never a person, and never holds.
pub(crate) fn newest_typed_turn_age_s(
    paths: &[PathBuf],
    bus: &BusIndex,
    session: &str,
    now: i64,
    spawn_payload: Option<&str>,
) -> Option<i64> {
    let mut newest: Option<i64> = None;
    for path in paths {
        // Transcripts are append-only JSONL: a file quiet past the window
        // cannot hold an in-window typed turn, so the full read is skipped.
        if let Ok(mtime) = std::fs::metadata(path).and_then(|m| m.modified()) {
            let m = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0) as i64;
            if now - m > WINDOW_SECS {
                continue;
            }
        }
        if let Some(age) = typed_age_in_file(path, bus, session, now, spawn_payload) {
            newest = Some(newest.map_or(age, |n: i64| n.min(age)));
        }
    }
    newest
}

/// The LAST-turn read with NO window and NO mtime skip: whether the
/// session's newest readable turn was typed, whenever it was written. This
/// is the fact the retirement policy reads, and the mtime short-circuit
/// cannot answer it - the incident row sat quiet 17 minutes when the sweep
/// retired it, past every window here. Across files, the newest readable
/// turn decides, wherever it lives: a resumed session's CURRENT transcript
/// answers, not whichever file happens to end on a person. The read's one
/// process-lifetime memo, keyed by the file's stat trio (path, len, mtime
/// secs) PLUS the recorded seed the read exempts by: two rows can resolve
/// to one transcript with different payloads, and a memo keyed without it
/// would answer one row with another row's exemption. An appended
/// transcript misses (new len or mtime) and rescans; an unchanged one
/// never pays the full read twice in this process - without it, a held
/// row's transcript is read end-to-end on every sweep pass.
fn scan_memo(
) -> &'static Mutex<HashMap<(PathBuf, u64, i64, Option<String>), Option<(f64, Option<i64>)>>> {
    static MEMO: OnceLock<
        Mutex<HashMap<(PathBuf, u64, i64, Option<String>), Option<(f64, Option<i64>)>>>,
    > = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn last_turn_typed_age_ever(
    paths: &[PathBuf],
    bus: &BusIndex,
    session: &str,
    now: i64,
    spawn_payload: Option<&str>,
) -> Option<i64> {
    // Across files the NEWEST readable turn decides: the winner's
    // typed-ness is the session's answer, so a resumed session whose
    // current transcript ends on a machine turn reads machine whatever an
    // older file ends on. Ties keep the later-scanned file's row (append
    // order is event order), matching the in-file rule.
    let mut best: Option<(f64, Option<i64>)> = None;
    for path in paths {
        // A file that cannot stat cannot form a key: it reads fresh and
        // answers None on the open failure, never from a stale entry.
        let stat = std::fs::metadata(path).and_then(|m| {
            m.modified().map(|t| {
                (
                    m.len(),
                    t.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0) as i64,
                )
            })
        });
        let Ok((len, mtime)) = stat else {
            continue;
        };
        let key = (path.clone(), len, mtime, spawn_payload.map(str::to_string));
        let file_fact = if let Some(hit) = scan_memo()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
        {
            *hit
        } else {
            let fact = last_turn_in_file(path, bus, session, now, spawn_payload);
            let mut memo = scan_memo().lock().unwrap_or_else(|e| e.into_inner());
            if memo.len() >= 1024 {
                memo.clear();
            }
            memo.insert(key, fact);
            fact
        };
        let Some((ts, verdict)) = file_fact else {
            continue;
        };
        if best.is_none_or(|(prev, _)| ts >= prev) {
            best = Some((ts, verdict));
        }
    }
    best.and_then(|(_, verdict)| verdict)
}

/// The sweep's one-row gate: `Some(hold)` when the row's transcript
/// carries a typed turn inside the window. A row the age seam already
/// reads fresh (`quiet`) skips the read - `kept_active` holds it.
pub(crate) fn sweep_hold(
    id: &str,
    home: &crate::paths::AgentsHome,
    hits: Option<Vec<PathBuf>>,
    sid: &str,
    quiet: bool,
    now: i64,
    hold_age_s: Option<i64>,
    hold_age_basis: &'static str,
    spawn_payload: Option<&str>,
) -> Option<crate::gc_sweep::Hold> {
    if quiet {
        return None;
    }
    let hits = hits?;
    let typed_age = newest_typed_turn_age_s(&hits, &bus_for(home), sid, now, spawn_payload)?;
    (typed_age <= WINDOW_SECS).then(|| crate::gc_sweep::Hold {
        id: id.to_string(),
        reason: "attended",
        detail: format!("typed user turn {typed_age}s ago"),
        age_s: hold_age_s,
        age_basis: hold_age_basis,
        escalated: false,
    })
}

/// The all-time read a retirement sweep pays for: the row's store-resolved
/// transcripts, keyed on the row's harness session id. The fact is the
/// LAST readable turn: `None` answers "last turn was machine" AND "store
/// unresolved" alike - the sweep treats both as no witness, and an
/// unresolved store never invented quiet either.
pub(crate) fn last_turn_typed_age_for_row(
    hits: Option<Vec<PathBuf>>,
    home: &crate::paths::AgentsHome,
    sid: &str,
    now: i64,
    spawn_payload: Option<&str>,
) -> Option<i64> {
    let hits = hits?;
    last_turn_typed_age_ever(&hits, &bus_for(home), sid, now, spawn_payload)
}

/// The stop-verb refusal: `Some(why)` when the row's store-resolved
/// transcript carries a typed turn inside the window. The bus join keys on
/// the row's harness session id, the same key `fno-agents intel` joins on.
/// The caller names its own error shape.
pub(crate) fn stop_refusal(home: &crate::paths::AgentsHome, e: &RegistryEntry) -> Option<String> {
    let mut index = crate::gc_inventory::HarnessStoreIndex::default();
    let hits = index.matches(e)?;
    let now = crate::daemon::now_epoch_secs();
    let sid = e.harness_session_id.as_deref().unwrap_or("");
    let bus = bus_for(home);
    let typed_age = newest_typed_turn_age_s(&hits, &bus, sid, now, e.spawn_payload.as_deref())?;
    (typed_age <= WINDOW_SECS).then(|| format!("typed user turn {typed_age}s ago"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The 2026-10-21 ruling's classifier half: the LAST readable turn
    /// decides, not the newest typed turn at any age. Empty rows are
    /// machinery, never a turn that decides; a machine shape after the
    /// person's turn answers None; a person's turn after machinery
    /// answers its age.
    #[test]
    fn the_last_readable_turn_decides_the_all_time_fact() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::daemon::now_epoch_secs();
        let ts = |age: i64| {
            chrono::DateTime::from_timestamp(now - age, 0)
                .unwrap()
                .to_rfc3339()
        };
        let bus = BusIndex::empty();
        // A person typed at 120s; lead mail arrived at 30s. The old fact
        // answered Some(120) forever; the last readable turn is the mail,
        // so the all-time fact answers None.
        let path = dir.path().join("mail_last.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type":"user","timestamp":ts(120),"message":{"role":"user","content":"keep the notes coming"}}),
                json!({"type":"user","timestamp":ts(30),"message":{"role":"user","content":"`@lead · fmail-a1b2c3d4e5f6`\nLead here, on x-1"}}),
            ),
        )
        .unwrap();
        assert_eq!(
            last_turn_typed_age_ever(&[path], &bus, "s", now, None),
            None,
            "the mail is the last readable turn; a machine turn never testifies"
        );
        // A person typed at 30s; empty rows (tool-result deliveries)
        // followed at 20s and 10s. The empty rows never decide, so the
        // person's turn is the last readable one.
        let path = dir.path().join("empty_tail.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"user","timestamp":ts(30),"message":{"role":"user","content":"go on"}}),
                json!({"type":"user","timestamp":ts(20),"message":{"role":"user","content":""}}),
                json!({"type":"user","timestamp":ts(10),"message":{"role":"user","content":""}}),
            ),
        )
        .unwrap();
        assert_eq!(
            last_turn_typed_age_ever(&[path], &bus, "s2", now, None),
            Some(30),
            "empty tail rows are machinery; the person's turn still decides"
        );
        // A machine shape after the person's turn flips the answer.
        let path = dir.path().join("task_after.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                json!({"type":"user","timestamp":ts(60),"message":{"role":"user","content":"run it"}}),
                json!({"type":"user","timestamp":ts(20),"message":{"role":"user","content":"<task-notification><task-id>x</task-id></task-notification>"}}),
            ),
        )
        .unwrap();
        assert_eq!(
            last_turn_typed_age_ever(&[path], &bus, "s3", now, None),
            None,
            "a machine turn after the person's turn is the last turn"
        );
        // A resumed session: the older file ends on a person turn, the
        // CURRENT transcript ends on a machine turn. The newest readable
        // turn decides, wherever it lives, so the answer is None - and it
        // stays None whichever order the files arrive in.
        let older = dir.path().join("resume-a.jsonl");
        std::fs::write(
            &older,
            format!(
                "{}\n",
                json!({"type":"user","timestamp":ts(300),"message":{"role":"user","content":"finish the thought"}})
            ),
        )
        .unwrap();
        let current = dir.path().join("resume-b.jsonl");
        std::fs::write(
            &current,
            format!(
                "{}\n",
                json!({"type":"user","timestamp":ts(60),"message":{"role":"user","content":"<task-notification><task-id>x</task-id></task-notification>"}})
            ),
        )
        .unwrap();
        assert_eq!(
            last_turn_typed_age_ever(&[older.clone(), current.clone()], &bus, "s4", now, None),
            None,
            "the current transcript's machine tail decides over the older file's person tail"
        );
        assert_eq!(
            last_turn_typed_age_ever(&[current, older], &bus, "s4b", now, None),
            None,
            "the answer never depends on the order the files arrive in"
        );
    }

    #[test]
    fn typed_turn_age_reads_the_person_and_skips_the_machines() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::daemon::now_epoch_secs();
        let ts = |age: i64| {
            chrono::DateTime::from_timestamp(now - age, 0)
                .unwrap()
                .to_rfc3339()
        };
        let path = dir.path().join("s1.jsonl");
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n{}\n",
                json!({"type":"user","timestamp":ts(60),"message":{"role":"user","content":"[cache-keepalive] Ping 1/4"}}),
                json!({"type":"user","timestamp":ts(30),"message":{"role":"user","content":"<fno_mail from=\"a\" to=\"b\" id=\"m\">run the sweep</fno_mail>"}}),
                json!({"type":"user","timestamp":ts(120),"message":{"role":"user","content":"keep the notes coming"}}),
            ),
        )
        .unwrap();
        let bus = BusIndex::empty();
        assert_eq!(
            newest_typed_turn_age_s(&[path.clone()], &bus, "s1", now, None),
            Some(120),
            "the newest TYPED turn decides; the keepalive and the mail never hold"
        );
        assert_eq!(newest_typed_turn_age_s(&[], &bus, "s1", now, None), None);
        let machine_only = dir.path().join("s2.jsonl");
        std::fs::write(
            &machine_only,
            format!(
                "{}\n",
                json!({"type":"user","timestamp":ts(10),"message":{"role":"user","content":"[cache-keepalive] Ping 2/4"}}),
            ),
        )
        .unwrap();
        assert_eq!(
            newest_typed_turn_age_s(&[machine_only], &bus, "s2", now, None),
            None,
            "machine-only transcript: nothing testifies"
        );
        // The append-only short-circuit: a file quiet past the window is
        // skipped even when its rows carry an in-window stamp.
        let aged = dir.path().join("s3.jsonl");
        std::fs::write(
            &aged,
            format!(
                "{}\n",
                json!({"type":"user","timestamp":ts(120),"message":{"role":"user","content":"typing now"}})
            ),
        )
        .unwrap();
        let old =
            std::time::SystemTime::now() - std::time::Duration::from_secs(WINDOW_SECS as u64 + 600);
        std::fs::File::options()
            .write(true)
            .open(&aged)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(old))
            .unwrap();
        assert_eq!(
            newest_typed_turn_age_s(&[aged], &bus, "s3", now, None),
            None,
            "a file quiet past the window is skipped"
        );
    }

    #[test]
    fn the_spawn_seed_command_never_holds_but_another_command_still_does() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::daemon::now_epoch_secs();
        let ts = |age: i64| {
            chrono::DateTime::from_timestamp(now - age, 0)
                .unwrap()
                .to_rfc3339()
        };
        // A spawn seeded with a slash command renders one user turn of
        // claude command XML, the args swallowing the brevity block that
        // trailed the payload.
        let seed =
            "/fno:target 42\n\n<fno_relay_compression>\nkeep it short\n</fno_relay_compression>";
        let rendered = format!(
            "{}\n",
            json!({"type":"user","timestamp":ts(60),"message":{"role":"user","content":
                "<command-message>fno:target</command-message>\n<command-name>/fno:target</command-name>\n<command-args>42\n\n<fno_relay_compression>\nkeep it short\n</fno_relay_compression>"}})
        );
        let path = dir.path().join("seeded.jsonl");
        std::fs::write(&path, &rendered).unwrap();
        let bus = BusIndex::empty();
        assert_eq!(
            newest_typed_turn_age_s(&[path.clone()], &bus, "w", now, Some(seed)),
            None,
            "the recorded seed's own command turn is the machine's, never a person"
        );
        assert_eq!(
            newest_typed_turn_age_s(&[path.clone()], &bus, "w", now, None),
            Some(60),
            "without a recorded payload the command still holds, as before"
        );
        // A raw (unrendered) single-line transcript row matches by exact
        // text: a bare command is the one shape that classifies
        // CommandInvocation on a harness that logs the prompt verbatim.
        let raw_path = dir.path().join("raw.jsonl");
        std::fs::write(
            &raw_path,
            format!(
                "{}\n",
                json!({"type":"user","timestamp":ts(45),"message":{"role":"user","content":"/fno:target 42"}})
            ),
        )
        .unwrap();
        assert_eq!(
            newest_typed_turn_age_s(&[raw_path], &bus, "w", now, Some("/fno:target 42")),
            None,
            "the raw seed row is the same machine turn"
        );
        // A DIFFERENT command a person typed later still holds the row.
        let person_path = dir.path().join("person.jsonl");
        std::fs::write(
            &person_path,
            format!(
                "{}\n{}\n",
                rendered.trim_end(),
                json!({"type":"user","timestamp":ts(30),"message":{"role":"user","content":"<command-message>fno:review</command-message>\n<command-name>/fno:review</command-name>\n<command-args>medium</command-args>"}})
            ),
        )
        .unwrap();
        assert_eq!(
            newest_typed_turn_age_s(&[person_path], &bus, "w", now, Some(seed)),
            Some(30),
            "a hand-run command that is not the seed still counts as the operator"
        );
    }
}
