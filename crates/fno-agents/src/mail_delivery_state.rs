//! What a live delivery reached, in six words every lane reports.
//!
//! A lane reaches an accepted state only when the receiving harness's own
//! turn record holds the envelope id. A started turn is not that proof: it
//! may be an earlier queued prompt with our message still behind it. Text
//! in the composer is a draft. The bus keeps the history as append-only
//! `kind: state` rows (`meta.of` names the message), and a reader folds
//! them with [`newest_state`].

use serde_json::{json, Value};

pub const STATE_KIND: &str = "state";

/// Where a parked message waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Parked {
    /// Typed into the input box and not submitted.
    Composer,
    /// Taken into the harness's own queue behind the running turn.
    HarnessQueue,
}

impl Parked {
    pub fn word(self) -> &'static str {
        match self {
            Parked::Composer => "composer",
            Parked::HarnessQueue => "harness-queue",
        }
    }
}

/// The record that proves an acceptance.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Evidence {
    /// The record read: a transcript path, `acp:session/prompt`, `legacy:hosted`.
    pub record: String,
    pub turn_id: Option<String>,
    pub at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    /// On the bus; no live lane reached the recipient.
    Queued,
    /// Bytes written or RPC sent; no turn record holds the envelope yet.
    Injected,
    Parked {
        where_: Parked,
    },
    /// The harness's own turn record holds the envelope id, as a new turn.
    AcceptedNewTurn {
        evidence: Evidence,
    },
    /// Same record, folded into the turn that was running at inject.
    AcceptedSteer {
        evidence: Evidence,
    },
    Failed {
        reason: String,
    },
}

impl DeliveryState {
    pub fn word(&self) -> &'static str {
        match self {
            DeliveryState::Queued => "queued",
            DeliveryState::Injected => "injected",
            DeliveryState::Parked { .. } => "parked",
            DeliveryState::AcceptedNewTurn { .. } => "accepted-new-turn",
            DeliveryState::AcceptedSteer { .. } => "accepted-steer",
            DeliveryState::Failed { .. } => "failed",
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        DeliveryState::Failed {
            reason: reason.into(),
        }
    }

    pub fn accepted(steer: bool, evidence: Evidence) -> Self {
        if steer {
            DeliveryState::AcceptedSteer { evidence }
        } else {
            DeliveryState::AcceptedNewTurn { evidence }
        }
    }

    pub fn is_accepted(&self) -> bool {
        matches!(
            self,
            DeliveryState::AcceptedNewTurn { .. } | DeliveryState::AcceptedSteer { .. }
        )
    }

    /// The bytes sit with the recipient (accepted, parked, or typed with no
    /// record yet), so a durable copy would reach it twice.
    pub fn reached(&self) -> bool {
        !matches!(self, DeliveryState::Queued | DeliveryState::Failed { .. })
    }

    pub fn evidence(&self) -> Option<&Evidence> {
        match self {
            DeliveryState::AcceptedNewTurn { evidence }
            | DeliveryState::AcceptedSteer { evidence } => Some(evidence),
            _ => None,
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            DeliveryState::Failed { reason } => Some(reason),
            _ => None,
        }
    }

    pub fn parked_where(&self) -> Option<Parked> {
        match self {
            DeliveryState::Parked { where_ } => Some(*where_),
            _ => None,
        }
    }

    /// The JSON a lane prints or answers: `{state, where?, reason?, evidence?}`.
    pub fn to_json(&self) -> Value {
        let mut v = json!({"state": self.word()});
        if let Some(w) = self.parked_where() {
            v["where"] = json!(w.word());
        }
        if let Some(r) = self.reason() {
            v["reason"] = json!(r);
        }
        if let Some(e) = self.evidence() {
            v["evidence"] = json!({"record": e.record, "turn_id": e.turn_id, "at": e.at});
        }
        v
    }

    /// Parse [`DeliveryState::to_json`]; `None` when `state` is missing or unknown.
    pub fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str);
        let evidence = || {
            let e = v.get("evidence").cloned().unwrap_or(Value::Null);
            Evidence {
                record: e
                    .get("record")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                turn_id: e.get("turn_id").and_then(Value::as_str).map(str::to_string),
                at: e
                    .get("at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }
        };
        Some(match s("state")? {
            "queued" => DeliveryState::Queued,
            "injected" => DeliveryState::Injected,
            "parked" => DeliveryState::Parked {
                where_: match s("where") {
                    Some("composer") => Parked::Composer,
                    _ => Parked::HarnessQueue,
                },
            },
            "accepted-new-turn" => DeliveryState::AcceptedNewTurn {
                evidence: evidence(),
            },
            "accepted-steer" => DeliveryState::AcceptedSteer {
                evidence: evidence(),
            },
            "failed" => DeliveryState::failed(s("reason").unwrap_or("unknown")),
            _ => return None,
        })
    }

    /// The state a pre-state bus row implies from its `delivery` field.
    pub fn from_legacy(delivery: &str) -> Self {
        let legacy = |record: &str| Evidence {
            record: record.to_string(),
            ..Evidence::default()
        };
        match delivery {
            "typed" => DeliveryState::Injected,
            "hosted" => DeliveryState::AcceptedNewTurn {
                evidence: legacy("legacy:hosted"),
            },
            "cross-session" => DeliveryState::AcceptedNewTurn {
                evidence: legacy("legacy:cross-session"),
            },
            _ => DeliveryState::Queued,
        }
    }
}

/// A `kind: state` bus row recording that message `of` reached `state`.
pub fn state_row(of: &str, state: &DeliveryState, from: &str, to: &str) -> Value {
    let id = crate::announce::new_msg_id();
    let mut meta = state.to_json();
    meta["of"] = json!(of);
    json!({
        "v": 1,
        "id": id,
        "thread": id,
        "ts": crate::graph_store::now_isoformat(),
        "from": from,
        "to": to,
        "kind": STATE_KIND,
        "body": "",
        "meta": meta,
    })
}

/// Append a [`state_row`] to the live bus segment; a failure only warns,
/// because the staged message row already holds the body.
pub fn record_state(bus: &std::path::Path, of: &str, state: &DeliveryState, from: &str, to: &str) {
    let row = state_row(of, state, from, to);
    let live = bus.join("messages.jsonl");
    if let Err(e) = crate::announce::append_line_open(&live, &row, &crate::chats::chats_dir()) {
        eprintln!("delivery state row failed: {e}");
    }
}

/// [`record_state`] on the bus the mail readers use (`FNO_BUS_DIR`, then
/// the inbox root, then the state root); no resolvable bus records nothing.
pub fn record(of: &str, state: &DeliveryState, from: &str, to: &str) {
    if let Some(bus) = crate::mail_cli::ack::bus_dir() {
        record_state(&bus, of, state, from, to);
    }
}

/// The state of message `id` folded from `rows` in bus order. An accepted
/// row is terminal: a retry's later rows are audit and never lower it.
/// Among the others the newest wins. With no state row, a `landed` row
/// reads as accepted, else the message's own legacy `delivery` field.
pub fn newest_state(rows: &[Value], id: &str) -> DeliveryState {
    let of = |r: &Value, key: &str| {
        r.get("meta")
            .and_then(|m| m.get(key))
            .and_then(Value::as_str)
            == Some(id)
    };
    let mut newest: Option<DeliveryState> = None;
    let mut landed = false;
    let mut own_delivery: Option<&str> = None;
    for r in rows {
        match r.get("kind").and_then(Value::as_str) {
            Some(STATE_KIND) if of(r, "of") => {
                let Some(state) = r.get("meta").and_then(DeliveryState::from_json) else {
                    continue;
                };
                if newest.as_ref().is_some_and(DeliveryState::is_accepted) {
                    continue;
                }
                newest = Some(state);
            }
            Some("landed") if of(r, "landed") => landed = true,
            _ if r.get("id").and_then(Value::as_str) == Some(id) => {
                if let Some(d) = r.get("delivery").and_then(Value::as_str) {
                    own_delivery = Some(d);
                }
            }
            _ => {}
        }
    }
    if let Some(state) = newest {
        if !state.is_accepted() && landed {
            return DeliveryState::AcceptedNewTurn {
                evidence: Evidence {
                    record: "landed".into(),
                    ..Evidence::default()
                },
            };
        }
        return state;
    }
    if landed {
        return DeliveryState::AcceptedNewTurn {
            evidence: Evidence {
                record: "landed".into(),
                ..Evidence::default()
            },
        };
    }
    DeliveryState::from_legacy(own_delivery.unwrap_or("durable"))
}

/// The detail after the state word: where it is parked, why it failed, or
/// the record that proves an acceptance.
pub fn detail(state: &DeliveryState) -> String {
    match state {
        DeliveryState::Parked { where_ } => format!("({})", where_.word()),
        DeliveryState::Failed { reason } => format!("({reason})"),
        DeliveryState::AcceptedNewTurn { evidence } | DeliveryState::AcceptedSteer { evidence } => {
            match &evidence.turn_id {
                Some(t) => format!("({} turn {t})", evidence.record),
                None if evidence.record.is_empty() => String::new(),
                None => format!("({})", evidence.record),
            }
        }
        _ => String::new(),
    }
}

/// The receipt status for a live send: `<word> [<lane>] <detail>`.
pub fn receipt(state: &DeliveryState, lane: &str) -> String {
    let mut out = state.word().to_string();
    let lane = lane.trim();
    if !lane.is_empty() {
        out.push(' ');
        out.push_str(lane);
    }
    let d = detail(state);
    if !d.is_empty() {
        out.push(' ');
        out.push_str(&d);
    }
    out
}

/// The receipt words for a send whose bytes reached the recipient's harness.
const REACHED_WORDS: [&str; 4] = ["injected", "parked", "accepted-new-turn", "accepted-steer"];

/// Did a `fno agents mail send` receipt line say the bytes reached the
/// recipient (so a durable copy or a resend would duplicate them)? The line
/// is the JSON receipt or a plain `<id> <status>` line. An older front
/// binary prints the pre-state `delivered (...)` receipt words (hosted, woken),
/// which still read as reached.
pub fn receipt_reached(line: &str) -> bool {
    let status = serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|v| v.get("status").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| line.to_string());
    status
        .split_whitespace()
        .take(2)
        .any(|t| REACHED_WORDS.contains(&t))
        || status.contains("delivered (")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bus_row(id: &str, delivery: Option<&str>) -> Value {
        let mut m = json!({"id": id, "kind": "send", "from": "a", "to": "b", "body": "x"});
        if let Some(d) = delivery {
            m["delivery"] = json!(d);
        }
        m
    }

    fn st(id: &str, s: &DeliveryState) -> Value {
        let mut meta = s.to_json();
        meta["of"] = json!(id);
        json!({"kind": STATE_KIND, "id": "fmail-s", "meta": meta})
    }

    fn steer() -> DeliveryState {
        DeliveryState::AcceptedSteer {
            evidence: Evidence {
                record: "/t.jsonl".into(),
                turn_id: Some("t1".into()),
                at: "now".into(),
            },
        }
    }

    #[test]
    fn a_newer_state_row_outranks_the_legacy_delivery_field() {
        let rows = vec![bus_row("fmail-1", Some("typed")), st("fmail-1", &steer())];
        assert_eq!(newest_state(&rows, "fmail-1"), steer());
    }

    #[test]
    fn a_legacy_hosted_row_reads_accepted_new_turn() {
        let got = newest_state(&[bus_row("fmail-1", Some("hosted"))], "fmail-1");
        assert_eq!(got.word(), "accepted-new-turn");
        assert_eq!(got.evidence().unwrap().record, "legacy:hosted");
        assert_eq!(
            newest_state(&[bus_row("fmail-2", None)], "fmail-2"),
            DeliveryState::Queued
        );
    }

    #[test]
    fn an_acceptance_is_never_lowered_by_a_retry() {
        let accepted = DeliveryState::accepted(false, Evidence::default());
        let rows = vec![
            bus_row("fmail-1", None),
            st(
                "fmail-1",
                &DeliveryState::Parked {
                    where_: Parked::HarnessQueue,
                },
            ),
            st("fmail-1", &accepted),
            st("fmail-1", &DeliveryState::Injected),
            st("fmail-1", &DeliveryState::failed("io-error")),
        ];
        assert_eq!(newest_state(&rows, "fmail-1"), accepted);
        // Among non-accepted rows the newest wins.
        let rows = vec![
            st("fmail-2", &DeliveryState::Injected),
            st(
                "fmail-2",
                &DeliveryState::Parked {
                    where_: Parked::Composer,
                },
            ),
        ];
        assert_eq!(newest_state(&rows, "fmail-2").word(), "parked");
    }

    #[test]
    fn every_state_round_trips_and_no_receipt_says_delivered() {
        let all = [
            DeliveryState::Queued,
            DeliveryState::Injected,
            DeliveryState::Parked {
                where_: Parked::Composer,
            },
            DeliveryState::Parked {
                where_: Parked::HarnessQueue,
            },
            DeliveryState::accepted(false, Evidence::default()),
            steer(),
            DeliveryState::failed("no-confirm-source"),
        ];
        for s in all {
            assert_eq!(DeliveryState::from_json(&s.to_json()), Some(s.clone()));
            let r = receipt(&s, "[live codex session x]");
            assert!(!r.contains("delivered"), "{r}");
            assert!(r.starts_with(s.word()), "{r}");
        }
    }
}
