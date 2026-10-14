//! A waiting spawn's place in the queue and the reason it waits. A spawn that
//! waits in the gate holds one `gate-wait:<holder>` ticket in the gate's
//! global claims root. Its place is one plus the live tickets that joined
//! before it. A `--no-wait` pass takes no ticket: it reads the place it would
//! take at the back.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::claims;
use crate::spawn_gate::SlotReservation;

const TICKET_PREFIX: &str = "gate-wait:";
/// The holder pid keeps a ticket live; the lease only bounds a lost holder.
const TICKET_TTL_MS: i64 = 120_000;
const PRINT_EVERY: Duration = Duration::from_secs(30);

/// The waiting spawn's ticket and its print clock. Dropping it leaves the
/// queue.
pub(crate) struct Queue {
    root: Option<PathBuf>,
    key: String,
    holder: String,
    pid: u32,
    joined_ms: Option<i64>,
    started: Instant,
    printed: Option<Instant>,
}

impl Queue {
    pub(crate) fn new(root: Option<PathBuf>, holder: &str, pid: u32) -> Self {
        Self {
            root,
            key: format!("{TICKET_PREFIX}{holder}"),
            holder: holder.to_string(),
            pid,
            joined_ms: None,
            started: Instant::now(),
            printed: None,
        }
    }

    /// Joins the queue on the first wait and renews the ticket after. Returns
    /// the wait line on the first wait and every 30 seconds after it.
    pub(crate) fn tick(&mut self, waiting_on: &str) -> Option<String> {
        if self.printed.is_some_and(|at| at.elapsed() < PRINT_EVERY) {
            return None;
        }
        self.join();
        self.printed = Some(Instant::now());
        Some(wait_line(
            self.place(),
            waiting_on,
            self.started.elapsed().as_secs(),
        ))
    }

    /// Adds `queue_place`, `queue_size` and `waiting_on` to a refusal receipt.
    pub(crate) fn annotate(&self, receipt: &mut Value, waiting_on: &str) {
        let (place, size) = self.place();
        receipt["queue_place"] = json!(place);
        receipt["queue_size"] = json!(size);
        receipt["waiting_on"] = json!(waiting_on);
    }

    fn join(&mut self) {
        let joined = *self.joined_ms.get_or_insert_with(now_ms);
        let mut metadata = serde_json::Map::new();
        metadata.insert("joined_ms".into(), json!(joined));
        let opts = claims::AcquireOpts {
            ttl_ms: Some(TICKET_TTL_MS),
            pid: Some(self.pid),
            metadata: Some(metadata),
            root: self.root.clone(),
            ..Default::default()
        };
        if !matches!(
            claims::acquire(&self.key, &self.holder, opts),
            claims::AcquireOutcome::Acquired(_)
        ) {
            self.joined_ms = None;
        }
    }

    /// (place, size). Without a ticket the spawn reads the back of the queue.
    fn place(&self) -> (usize, usize) {
        let others: Vec<(i64, String)> = live_tickets(self.root.as_deref())
            .into_iter()
            .filter(|(_, key)| *key != self.key)
            .collect();
        let ahead = match self.joined_ms {
            Some(mine) => others
                .iter()
                .filter(|(at, key)| (*at, key.as_str()) < (mine, self.key.as_str()))
                .count(),
            None => others.len(),
        };
        (ahead + 1, others.len() + 1)
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        if self.joined_ms.is_some() {
            let _ = claims::release(&self.key, &self.holder, self.root.as_deref(), None);
        }
    }
}

/// The live tickets as (joined_ms, key). Unreadable claims read as none.
fn live_tickets(root: Option<&Path>) -> Vec<(i64, String)> {
    let Some(root) = root else {
        return Vec::new();
    };
    let records =
        crate::claim_store::records_in(&root.join(".fno/claims"), Some(TICKET_PREFIX), true)
            .unwrap_or_default();
    records
        .into_iter()
        .filter(|rec| {
            matches!(
                crate::claim_verbs::status_verdict(rec).0,
                claims::ClaimState::Live | claims::ClaimState::Suspect
            )
        })
        .map(|rec| {
            let joined = rec.metadata.get("joined_ms").and_then(Value::as_i64);
            (joined.unwrap_or(rec.acquired_at), rec.key)
        })
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The one line a waiting spawn prints. It starts `spawn queued:`, never the
/// `spawn-gate: ` verdict marker, because the spawn has not been refused.
pub(crate) fn wait_line(place: (usize, usize), waiting_on: &str, waited_s: u64) -> String {
    format!(
        "spawn queued: place {} of {} waiting; waiting on {waiting_on}; waited {waited_s}s",
        place.0, place.1
    )
}

/// `, place 3 of 3 waiting on max_live` from a receipt that carries a place,
/// for a refusal line that has no room for the receipt itself.
pub(crate) fn place_note(receipt: Option<&Value>) -> String {
    let field = |key: &str| receipt.and_then(|r| r.get(key));
    match (
        field("queue_place").and_then(Value::as_u64),
        field("queue_size").and_then(Value::as_u64),
        field("waiting_on").and_then(Value::as_str),
    ) {
        (Some(place), Some(size), Some(on)) => format!(", place {place} of {size} waiting on {on}"),
        _ => String::new(),
    }
}

/// The one slot-refusal sentence both print paths share, so a display can
/// never quote a count the gate did not measure. Pure text; the caller adds
/// its own tail (`refusing (--no-wait).`, or the queue line's advice). The
/// rows are named by the probe's `slot_rows` field (`fno agents gate-status`),
/// never by a second walk. When a reservation is counted, the sentence names
/// the release verb for the first suspect one; all-live saturation names none.
pub(crate) fn slot_refusal_line(
    slots: usize,
    learned: &crate::capacity::Effective,
    rows: usize,
    claims: &[SlotReservation],
    waiting: usize,
    tail: &str,
) -> String {
    let waiting_note = if waiting > 0 {
        format!("; {waiting} of the rows wait on an operator question")
    } else {
        String::new()
    };
    // Name the remedy only for a SUSPECT reservation: a live one still holds
    // its slot on purpose, and releasing it would leave the worker running
    // uncounted. All-live saturation names no release target.
    let remedy = match claims.iter().find(|r| r.state == "suspect") {
        Some(r) => format!(
            "; free a dead reservation: fno agents claim release worker:{} --force \
             --reason \"<why>\"",
            r.name
        ),
        None => String::new(),
    };
    format!(
        "{slots} live worker slots >= {cap_clause} ({rows} registry rows, {n} headless \
         reservations{waiting_note}); leads take no slot; every counted row: fno agents \
         gate-status, field slot_rows{remedy}; {tail}",
        cap_clause = learned.clause(),
        n = claims.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An unknown effective cap, the shape every fallback clause test wants.
    fn no_state() -> crate::capacity::Effective {
        crate::capacity::Effective {
            cap: 23,
            ceiling: 23,
            ..Default::default()
        }
    }

    /// AC5-TEXT: the shared refusal sentence names the probe field that lists
    /// every counted row, marks the operator-waiting share, and never again
    /// blames a population its own recommended reader cannot see.
    #[test]
    fn slot_refusal_line_names_the_probe_and_marks_waiting_rows() {
        let line = slot_refusal_line(3, &no_state(), 3, &[], 1, "refusing (--no-wait).");
        assert!(line.contains("fno agents gate-status"), "{line}");
        assert!(line.contains("slot_rows"), "{line}");
        assert!(
            line.contains("1 of the rows wait on an operator question"),
            "{line}"
        );
        assert!(!line.contains("--status quiet"), "{line}");
        assert!(!line.contains("fno agents top"), "{line}");

        let line = slot_refusal_line(3, &no_state(), 3, &[], 0, "refusing (--no-wait).");
        assert!(!line.contains("wait on an operator question"), "{line}");
    }

    /// Only 5 of 54 attempts launched in 4 hours, with no word. A waiting
    /// spawn names its place and what it waits on, and a --no-wait refusal
    /// carries the same place in its receipt.
    #[test]
    fn a_waiting_spawn_names_its_place_and_reason() {
        let _g = claims::test_env_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let root = std::env::temp_dir().join(format!("fno-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let me = std::process::id();
        let mut first = Queue::new(Some(root.clone()), "spawn-gate:1:first", me);
        let line = first.tick("max_live").expect("the first wait prints");
        assert!(
            line.starts_with("spawn queued: place 1 of 1 waiting"),
            "{line}"
        );
        assert!(line.contains("waiting on max_live"), "{line}");
        assert!(first.tick("max_live").is_none(), "the next line waits 30s");

        let mut second = Queue::new(Some(root.clone()), "spawn-gate:2:second", me);
        let line = second.tick("fleet_cpu_share").unwrap();
        assert!(line.contains("place 2 of 2 waiting"), "{line}");
        let back = Queue::new(Some(root.clone()), "spawn-gate:3:no-wait", me);
        let mut receipt = json!({"reason": "no_wait"});
        back.annotate(&mut receipt, "max_live");
        assert_eq!(
            (&receipt["queue_place"], &receipt["queue_size"]),
            (&json!(3), &json!(3))
        );
        assert_eq!(receipt["waiting_on"], "max_live");
        assert_eq!(
            place_note(Some(&receipt)),
            ", place 3 of 3 waiting on max_live"
        );

        drop(first);
        second.annotate(&mut receipt, "fleet_cpu_share");
        assert_eq!(
            receipt["queue_place"], 1,
            "a spawn that left frees its place"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&root);
    }
}
