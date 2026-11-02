//! The link role: one passive client that may drive one pane.
//!
//! `fno mux serve --link` runs on the pane's own machine, and ssh carries only
//! terminal bytes, so this is the same person at another keyboard. It stays
//! `passive`: it births no squad, stays out of the tab size pick, and takes
//! frames through the observer tag beside this one. The observer tag grants no
//! write path. Only this table does, and only for the one pane it names.

use std::collections::BTreeMap;

use super::observers::clean_label;
use super::{Core, CoreMsg, Flow};
use crate::proto::{LinkHello, ObserverHello, ObserverVia};

/// Client id -> the pane it may drive.
#[derive(Debug, Default)]
pub(crate) struct Links(BTreeMap<u64, u64>);

impl Links {
    pub(super) fn pane(&self, client: u64) -> Option<u64> {
        self.0.get(&client).copied()
    }

    /// Whether a mutating message from passive client `id` passes the
    /// read-only gate. `Input` carries no pane because `handle_input` sends it
    /// to this client's own pane; every other message names its pane.
    pub(super) fn may_send(&self, id: u64, msg: &CoreMsg) -> bool {
        let Some(own) = self.pane(id) else {
            return false;
        };
        match msg {
            CoreMsg::Input { .. } => true,
            CoreMsg::Mouse { pane, .. }
            | CoreMsg::PaneScroll { pane, .. }
            | CoreMsg::PaneScrollEdge { pane, .. } => *pane == own,
            _ => false,
        }
    }
}

impl Core {
    /// The pane whose terminal modes this client's terminal follows: a link
    /// client's own pane, which its view's focus says nothing about.
    pub(super) fn modes_pane(&self, id: u64, view: (u64, super::TabId)) -> Option<u64> {
        let focus = self.viewed_tab(view).map(|tab| tab.focus);
        self.links.pane(id).or(focus)
    }

    /// Mail's fail-closed target check. A `name@host` address is another
    /// machine's row: this registry cannot answer for it, and the mail command
    /// routes it.
    pub(super) fn mail_target(&self, name: &str) -> Result<(), String> {
        if name.contains('@') {
            return Ok(());
        }
        self.resolve_lifecycle_target(name, None).map(|_| ())
    }

    /// Tag passive client `id` as the driver of one pane, after checking the
    /// pane still holds the child the far door read. A refused hello leaves the
    /// client an untagged observer that can write nothing, so the far door
    /// names the refusal from its own check rather than a reply here.
    pub(super) fn link(&mut self, id: u64, hello: LinkHello) -> Flow {
        let child = self
            .panes
            .get(&hello.pane)
            .and_then(|entry| entry.pty.child_pid());
        if !self.is_passive(id) || child.map(|pid| format!("pid:{pid}")) != Some(hello.identity) {
            eprintln!("fno mux: client {id} link to pane {} refused", hello.pane);
            return Flow::Continue;
        }
        let live: Vec<u64> = self.clients.iter().map(|c| c.id).collect();
        self.links.0.retain(|cid, _| live.contains(cid));
        self.links.0.insert(id, hello.pane);
        self.observe(
            id,
            ObserverHello {
                name: clean_label(&hello.name),
                pane: hello.pane,
                via: ObserverVia::Ssh,
                frames: true,
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::empty_core;

    #[test]
    fn mail_target_passes_a_remote_address_and_refuses_an_unknown_local_name() {
        let core = empty_core();
        assert_eq!(core.mail_target("ines@bb16"), Ok(()));
        assert!(core.mail_target("ines").is_err());
    }
}
