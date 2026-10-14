//! Portal open and close become events. One row on the first
//! entry of an index into the portals map (`portal_opened`) and one on its
//! removal (`portal_closed`), so an incident like the draft-loss portal can
//! be answered from the journal alone: which door opened it, and what took
//! it. A retune emits a close-and-open pair, which is the honest shape: the
//! channel moved, the window stayed.

use super::*;

/// The `portal_opened` row: an index entered the portals map. Pure so tests
/// can assert the envelope, the same contract [`super::pane_close`] holds.
pub(super) fn portal_opened_row(
    mux_session: &str,
    portal: u8,
    row_key: &str,
    seat: u64,
) -> serde_json::Value {
    serde_json::json!({
        "ts": crate::review_invocation::review_invocation_timestamp(),
        "type": "portal_opened",
        "source": "daemon",
        "data": {
            "mux_session": mux_session,
            "portal": portal,
            "row_key": row_key,
            "seat": seat,
        }
    })
}

/// The `portal_closed` row: an index left the portals map. `cause` is the
/// door's word: `operator` (a deliberate close), `viewer_died` (the death
/// path whose parked screen could not take the seat), `displaced` (an
/// ordinary attach took the seat over), `stale_seat` (a half-created pane
/// was reaped), or `retune` (a reach re-tuned the index).
pub(super) fn portal_closed_row(
    mux_session: &str,
    portal: u8,
    row_key: &str,
    cause: &str,
) -> serde_json::Value {
    serde_json::json!({
        "ts": crate::review_invocation::review_invocation_timestamp(),
        "type": "portal_closed",
        "source": "daemon",
        "data": {
            "mux_session": mux_session,
            "portal": portal,
            "row_key": row_key,
            "cause": cause,
        }
    })
}

/// The attach budget: an attach-mechanism open slower than this says so.
const SLOW_OPEN_MS: u64 = 1_000;

/// The `portal_reached` row: one open landed. `mechanism` names how the
/// viewer came to be (`attach-direct`, `attach-plan`, `relaunch`, `follow`,
/// `locate`); `plan_ms` is the re-entry plan's time and `spawn_ms` the
/// viewer spawn's, so the click-to-pane time splits by stage.
pub(super) fn portal_reached_row(
    mux_session: &str,
    portal: u8,
    row_key: &str,
    mechanism: &str,
    plan_ms: u64,
    spawn_ms: u64,
) -> serde_json::Value {
    serde_json::json!({
        "ts": crate::review_invocation::review_invocation_timestamp(),
        "type": "portal_reached",
        "source": "daemon",
        "data": {
            "mux_session": mux_session,
            "portal": portal,
            "row_key": row_key,
            "mechanism": mechanism,
            "plan_ms": plan_ms,
            "spawn_ms": spawn_ms,
        }
    })
}

/// The notice an attach owes when it overran the budget; `None` inside it.
pub(super) fn slow_open_notice(
    portal: u8,
    row_name: &str,
    mechanism: &str,
    plan_ms: u64,
    spawn_ms: u64,
) -> Option<String> {
    let total = plan_ms + spawn_ms;
    (mechanism.starts_with("attach") && total > SLOW_OPEN_MS).then(|| {
        format!("portal {portal}: {row_name} took {total} ms to open (plan {plan_ms}, spawn {spawn_ms})")
    })
}

impl Core {
    /// Journal one landed open and flag a slow attach. The spawn time is
    /// measured from the moment the viewer's spawn was queued.
    pub(super) fn journal_portal_reached(
        &mut self,
        client_id: u64,
        idx: u8,
        row: &RegistryAgent,
        key: &str,
        mechanism: &str,
        (plan_ms, queued): (u64, std::time::Instant),
    ) {
        let spawn_ms = u64::try_from(queued.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.append_portal_event(portal_reached_row(
            &self.session_name,
            idx,
            key,
            mechanism,
            plan_ms,
            spawn_ms,
        ));
        if let Some(text) = slow_open_notice(idx, &row.name, mechanism, plan_ms, spawn_ms) {
            self.notice(client_id, text);
        }
    }

    /// Insert + journal: emits `portal_opened` only when the index was
    /// absent, so a fill or a reseat of a live portal stays silent.
    pub(super) fn journal_portal_open(&mut self, idx: u8, portal: Portal) {
        let fresh = !self.portals.contains_key(&idx);
        // A restore guard binds an index to the row it was captured for; a
        // portal that changes hands must not inherit it. The restore re-arm
        // inserts its guard after this call.
        self.portal_session_guards.remove(&idx);
        let row_key = portal.row_key.clone();
        let seat = portal.seat;
        self.portals.insert(idx, portal);
        if fresh {
            self.append_portal_event(portal_opened_row(&self.session_name, idx, &row_key, seat));
        }
    }

    /// Remove + journal `portal_closed` with the door's cause word. A
    /// missing index removes nothing and emits nothing; the caller that
    /// needs the slot keeps it (the retune door).
    pub(super) fn journal_portal_take(&mut self, idx: u8, cause: &str) -> Option<Portal> {
        let slot = self.portals.remove(&idx);
        self.portal_session_guards.remove(&idx);
        if let Some(portal) = &slot {
            self.append_portal_event(portal_closed_row(
                &self.session_name,
                idx,
                &portal.row_key,
                cause,
            ));
        }
        slot
    }

    /// The close half of a retune whose caller already removed the slot:
    /// emits `portal_closed` for the channel the index used to show, with
    /// no map mutation. The caller journals the open half on the success
    /// path, so a focus or a failed retune never reads as a closure.
    pub(super) fn journal_portal_channel_left(&self, idx: u8, previous_row_key: &str, cause: &str) {
        self.append_portal_event(portal_closed_row(
            &self.session_name,
            idx,
            previous_row_key,
            cause,
        ));
    }

    fn append_portal_event(&self, event: serde_json::Value) {
        crate::pane_send_audit::queue_agents_event(
            &crate::pane_send_audit::pane_send_audit_events_path(),
            event,
            None,
        );
    }
}
