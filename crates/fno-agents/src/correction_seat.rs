//! The seat a corrected session sat in: `lead:L<n>`, `lead` with no rung known,
//! or `worker`. A seat is a fact about the session, never the turn, so a worker
//! promoted later reads as a lead on its earlier rows too.
//!
//! The store is append-only, so a row written before the fold stamped a seat
//! gets its own `correction_seat` row. An unmeasured session writes nothing and
//! is read again next pass, because a later role vacate can still name it.

use crate::correction_readings::Reading;
use crate::event_store::{append_envelope, query_events, EventQuery};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;

/// About five hours to work through the 23,000 rows that predate the seat.
const MAX_ROWS_PER_PASS: usize = 5_000;
/// A `think` or `blueprint` row can name the lead that filed the node, so only
/// these phases show a session did the work.
const WORKER_PHASES: [&str; 3] = ["execute", "ship", "review"];

#[derive(Debug, Default)]
pub(crate) struct Roster {
    held: BTreeMap<String, u64>,
    vacated: BTreeMap<String, u64>,
    workers: HashSet<String>,
}

impl Roster {
    pub(crate) fn note_registry_row(&mut self, session: &str, row: &Value) {
        if let Some(level) = row.get("role_level").and_then(Value::as_u64) {
            self.held.insert(session.to_string(), level);
        }
    }

    pub(crate) fn note_node(&mut self, node: &Value) {
        let sessions = node.get("sessions").and_then(Value::as_array);
        for row in sessions.into_iter().flatten() {
            let phase = row.get("phase").and_then(Value::as_str).unwrap_or("");
            if let Some(id) = row.get("session_id").and_then(Value::as_str) {
                if WORKER_PHASES.contains(&phase) {
                    self.workers.insert(id.to_string());
                }
            }
        }
    }

    /// Every role a session gave up, oldest first, so its newest rung wins. Older
    /// rows carry the fields flat, with no `data` object.
    pub(crate) fn note_vacated(&mut self, journal: &Path) -> Result<(), String> {
        let query = EventQuery::of_types(&["agent_role_vacated", "agent_team_vacated"]);
        for row in query_events(journal, &query)? {
            let Ok(event) = serde_json::from_str::<Value>(&row.line) else {
                continue;
            };
            let data = event.get("data").unwrap_or(&event);
            let session = data.get("holder_session").and_then(Value::as_str);
            let level = data.get("level").and_then(Value::as_u64);
            if let (Some(session), Some(level)) = (session, level) {
                self.vacated.insert(session.to_string(), level);
            }
        }
        Ok(())
    }
}

fn loaded(skills: &[String], names: [&str; 2]) -> bool {
    skills.iter().any(|s| names.contains(&s.as_str()))
}

/// A lead fact outranks a worker fact, because a lead also appears on the nodes
/// it works. The rung sources outrank the skill, which names no rung.
pub(crate) fn seat_reading(
    session: &str,
    skills: &[String],
    roster: &Roster,
    now: &str,
) -> Reading {
    let at = || Some(now.to_string());
    if let Some(level) = roster.held.get(session) {
        return Reading::with_basis(format!("lead:L{level}"), at(), "registry");
    }
    if let Some(level) = roster.vacated.get(session) {
        return Reading::with_basis(format!("lead:L{level}"), at(), "role_vacated");
    }
    if loaded(skills, ["fno:lead", "lead"]) {
        return Reading::with_basis("lead".to_string(), at(), "skill");
    }
    if roster.workers.contains(session) {
        return Reading::with_basis("worker".to_string(), at(), "roster");
    }
    if loaded(skills, ["fno:target", "target"]) {
        return Reading::with_basis("worker".to_string(), at(), "skill");
    }
    Reading::unmeasured("no role row, no node session and no lead or target skill")
}

fn data_rows(journal: &Path, kind: &str) -> Result<Vec<Value>, String> {
    Ok(query_events(journal, &EventQuery::of_types(&[kind]))?
        .iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .map(|mut v| v["data"].take())
        .collect())
}

/// One backfill pass over rows with no seat of their own.
pub(crate) fn seat_pass(journal: &Path, roster: &Roster, now: i64) -> Result<usize, String> {
    let seated: HashSet<String> = data_rows(journal, "correction_seat")?
        .iter()
        .filter_map(|r| r.get("correction_id").and_then(Value::as_str))
        .map(str::to_string)
        .collect();
    let stamp = chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
    let mut appended = 0;
    for row in data_rows(journal, "agent_corrected")? {
        if appended >= MAX_ROWS_PER_PASS {
            break;
        }
        let text = |k: &str| row.get(k).and_then(Value::as_str);
        let (Some(id), Some(session)) = (text("correction_id"), text("session_id")) else {
            continue;
        };
        if row.get("seat").is_some() || seated.contains(id) {
            continue;
        }
        let skills: Vec<String> = row
            .get("skills_loaded")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
        let seat = seat_reading(session, &skills, roster, &stamp);
        if seat.value.is_none() {
            continue;
        }
        let envelope = json!({"ts": stamp, "type": "correction_seat", "source": "daemon",
            "session_id": session,
            "data": {"correction_id": id, "seat": seat.to_json()}});
        append_envelope(
            journal,
            &envelope.to_string(),
            Some(&format!("corrseat:{id}")),
        )?;
        appended += 1;
    }
    Ok(appended)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-02T00:00:00Z";

    fn seat(session: &str, skills: &[&str], roster: &Roster) -> (Option<String>, String) {
        let skills: Vec<String> = skills.iter().map(|s| s.to_string()).collect();
        let reading = seat_reading(session, &skills, roster, NOW);
        (reading.value, reading.basis)
    }

    #[test]
    fn a_lead_fact_outranks_a_worker_fact_and_a_bare_session_reads_unmeasured() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        for (id, line) in [
            (
                "v1",
                json!({"ts": NOW, "type": "agent_role_vacated", "source": "agents",
                "data": {"holder_session": "was-lead", "level": 2, "scope": "x", "cause": "done"}}),
            ),
            (
                "v2",
                json!({"ts": NOW, "type": "agent_role_vacated", "source": "agents",
                "data": {"holder_session": "was-head", "level": 1, "scope": "y", "cause": "done"}}),
            ),
        ] {
            append_envelope(&journal, &line.to_string(), Some(id)).unwrap();
        }
        let mut roster = Roster::default();
        roster.note_vacated(&journal).unwrap();
        roster.note_registry_row("holds", &json!({"role_level": 0}));
        roster.note_registry_row("no-role", &json!({"role_level": null}));
        roster.note_node(&json!({"sessions": [
            {"phase": "execute", "session_id": "was-lead"},
            {"phase": "execute", "session_id": "holds"},
            {"phase": "ship", "session_id": "builder"},
            {"phase": "think", "session_id": "filer"},
        ]}));

        let lead = |v: &str| Some(v.to_string());
        assert_eq!(
            seat("holds", &[], &roster),
            (lead("lead:L0"), "registry".to_string())
        );
        assert_eq!(
            seat("was-lead", &[], &roster),
            (lead("lead:L2"), "role_vacated".to_string())
        );
        assert_eq!(seat("was-head", &[], &roster).0, lead("lead:L1"));
        assert_eq!(
            seat("builder", &["fno:lead"], &roster),
            (lead("lead"), "skill".to_string()),
            "a lead skill outranks a node row"
        );
        assert_eq!(
            seat("builder", &[], &roster),
            (lead("worker"), "roster".to_string())
        );
        assert_eq!(
            seat("other", &["fno:target"], &roster),
            (lead("worker"), "skill".to_string())
        );
        assert_eq!(seat("filer", &[], &roster).0, None, "think is not work");
        assert_eq!(seat("no-role", &[], &roster).0, None);
    }

    #[test]
    fn the_backfill_seats_old_rows_once_and_leaves_stamped_and_unmeasured_rows() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        let add = |id: &str, session: &str, extra: Value| {
            let mut data = json!({"correction_id": id, "corrected_by": "user", "kind": "correct",
                "session_id": session, "rule_version": "v1"});
            for (k, v) in extra.as_object().unwrap() {
                data[k] = v.clone();
            }
            let row =
                json!({"ts": NOW, "type": "agent_corrected", "source": "daemon", "data": data});
            append_envelope(&journal, &row.to_string(), Some(id)).unwrap();
        };
        add("old", "s1", json!({"skills_loaded": ["fno:target"]}));
        add("bare", "s2", json!({}));
        add(
            "new",
            "s3",
            json!({"seat": {"value": "worker", "basis": "roster"}}),
        );
        let roster = Roster::default();
        assert_eq!(seat_pass(&journal, &roster, 1_793_000_000).unwrap(), 1);
        assert_eq!(seat_pass(&journal, &roster, 1_793_000_001).unwrap(), 0);
        let rows = data_rows(&journal, "correction_seat").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["correction_id"], "old");
        assert_eq!(rows[0]["seat"]["value"], "worker");
        // The bare session gets its seat once a later fact names it.
        let mut later = Roster::default();
        later.note_registry_row("s2", &json!({"role_level": 1}));
        assert_eq!(seat_pass(&journal, &later, 1_793_000_002).unwrap(), 1);
    }
}
