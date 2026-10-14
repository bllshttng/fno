//! A session the operator moved into a workspace by hand. A paneless row has
//! no member to carry the placement, so the move is a pin on the workspace's
//! stored row, keyed by harness session id.

use super::*;
use crate::squad_store::{StoredSquad, StoredThreadPin};

/// Every stored pin, beside the identity (name, else durable key) of the row
/// that holds it. A row with neither name nor key cannot be found again, so
/// its pins are skipped.
pub(super) fn pins_from_store(squads: &[StoredSquad]) -> Vec<(String, StoredThreadPin)> {
    squads
        .iter()
        .filter(|s| !s.name.is_empty() || !s.key.is_empty())
        .flat_map(|s| {
            let identity = if s.name.is_empty() {
                s.key.clone()
            } else {
                s.name.clone()
            };
            s.thread_pins
                .iter()
                .map(move |pin| (identity.clone(), pin.clone()))
        })
        .collect()
}

impl Core {
    /// `Command::PinThread`: store the move before the live pin changes. A
    /// failed write leaves the live pins alone, so a restart cannot undo a
    /// move the operator watched happen.
    pub(super) fn handle_pin_thread(
        &mut self,
        client_id: u64,
        squad: u64,
        harness: String,
        harness_session_id: String,
    ) -> Flow {
        let identity_of = |core: &Self| {
            core.squad_identity(squad)
                .map(|(name, key)| if name.is_empty() { key } else { name })
        };
        let Some(mut identity) = identity_of(&*self) else {
            self.notice(client_id, "no such workspace");
            return Flow::Continue;
        };
        if identity.is_empty() {
            // An unnamed workspace gets its durable key at first persist.
            self.persist_squad(squad);
            identity = identity_of(&*self).unwrap_or_default();
        }
        if identity.is_empty() {
            self.notice(
                client_id,
                "this workspace has no name or key to store the move under",
            );
            return Flow::Continue;
        }
        let pin = StoredThreadPin {
            harness,
            harness_session_id,
        };
        let mut stored = crate::squad_store::pin_thread(&identity, &pin);
        if matches!(stored, Ok((false, _))) {
            // A workspace the store has not seen yet: write its row, then pin.
            self.persist_squad(squad);
            stored = crate::squad_store::pin_thread(&identity, &pin);
        }
        match stored {
            Ok((true, batch)) => {
                self.persist_result(Ok(batch));
                self.thread_pins.retain(|(_, held)| held != &pin);
                self.thread_pins.push((identity, pin));
                self.push_layout(false);
            }
            Ok((false, _)) => self.notice(
                client_id,
                "workspace move was not stored: the workspace has no stored row",
            ),
            Err(error) => self.notice(client_id, format!("workspace move was not stored: {error}")),
        }
        Flow::Continue
    }
}
