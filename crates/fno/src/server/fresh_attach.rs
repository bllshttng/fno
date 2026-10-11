//! How a fresh-squad attach lands its shell.
//!
//! [`Core::attach`]'s fresh-squad arm builds the new squad around a RESERVED
//! pane id and registers the client in the same turn, with the shell not yet
//! running: the keeper handshake that used to run synchronously on the core
//! loop (bounded at seconds per shell candidate) now runs on the blocking
//! pool, and [`Core::attach_shell_landed`] finishes the gesture on the loop -
//! frames on success, the same `cannot start a shell` refusal on failure.

use super::pane_spawn::AttachShellJob;
use super::*;

impl Core {
    /// The fresh squad's shell job: parked against the attaching client and
    /// spawned on the blocking pool. Runs at the end of `attach`, after the
    /// model, client, restore hook, first Layout, and cold-attach snapshot
    /// are already out.
    pub(super) fn spawn_attach_shell_after(
        &mut self,
        job: AttachShellJob,
        client: u64,
        sid: u64,
        tid: u64,
    ) {
        self.spawn_attach_shell_then(job, move |core, result| {
            core.attach_shell_landed(client, sid, tid, result)
        });
    }

    /// The fresh squad's shell landed. Success: the pane registers at its
    /// reserved id (the tree already names it), so a re-push carries its
    /// frames, and the reliable gift covers the droppable seed push_layout
    /// just wrote - a lost droppable seed never recovers (broadcast_pane only
    /// fires on output), the same reason the cold-attach snapshot exists.
    /// Failure: the placeholder squad tears down and the attach is refused
    /// with `cannot start a shell`, the same terminal case the synchronous
    /// path refused with, minus the stall.
    pub(super) fn attach_shell_landed(
        &mut self,
        client: u64,
        sid: u64,
        tid: u64,
        result: Result<u64, String>,
    ) -> Flow {
        match result {
            Ok(pid) => {
                self.push_layout(true);
                self.seed_reliable_frames(client, &[pid]);
                Flow::Continue
            }
            Err(e) => {
                // Remove the PLACEHOLDER tab by id, not index 0: the tab is
                // live and mutable during the window, and a client-added tab
                // may now hold index 0. The placeholder already gone (closed
                // by its own close cascade) leaves nothing to remove.
                let found = self
                    .session
                    .squad(sid)
                    .and_then(|sq| sq.tabs.iter().position(|t| t.id == tid));
                let mut squad_dead = false;
                if let Some(idx) = found {
                    let ident = self.squad_identity(sid);
                    let outcome = self.session.remove_tab(sid, idx);
                    squad_dead = matches!(
                        outcome,
                        RemoveOutcome::SquadRemoved | RemoveOutcome::SessionEmpty
                    );
                    if squad_dead {
                        self.squad_members.remove(&sid);
                        if let Some((name, key)) = ident {
                            self.persist_remove(&name, &key);
                        }
                    }
                    self.tab_areas.remove(&tid);
                }
                // Refuse every client attached onto the placeholder squad (the
                // attacher, plus anyone who joined it during the window). A
                // squad that survived (the user added a tab during the window)
                // keeps its clients; reanchor settles their views. A
                // SessionEmpty here is NOT CloseTab's end-of-session shutdown:
                // an attach refusal never ended the session, and the server
                // keeps serving.
                if squad_dead {
                    let victims: Vec<u64> = self
                        .clients
                        .iter()
                        .filter(|c| c.view.0 == sid)
                        .map(|c| c.id)
                        .collect();
                    self.reanchor_views();
                    let reason = format!("cannot start a shell: {e}");
                    for c in &self.clients {
                        if victims.contains(&c.id) {
                            let _ = c.reliable_tx.try_send(ServerMsg::Bye {
                                reason: reason.clone(),
                            });
                        }
                    }
                    self.clients.retain(|c| !victims.contains(&c.id));
                } else {
                    self.reanchor_views();
                }
                self.push_layout(true);
                Flow::Continue
            }
        }
    }

    /// One reliable `Frame` for `pid` to `client_id`, dropping the droppable
    /// seed it supersedes. The cold-attach snapshot's per-pane step, shared
    /// by `attach` (every visible pane) and the landing (the one pane that
    /// registered after its viewer's snapshot ran).
    pub(super) fn seed_reliable_frames(&mut self, client_id: u64, pids: &[u64]) {
        let Some(c) = self.clients.iter_mut().find(|c| c.id == client_id) else {
            return;
        };
        let mut sent_n = 0usize;
        for pid in pids {
            let Some(entry) = self.panes.get(pid) else {
                continue;
            };
            entry
                .stats
                .frames_composited
                .fetch_add(1, Ordering::Relaxed);
            let sent = c
                .reliable_tx
                .try_send(ServerMsg::Frame {
                    pane_id: *pid,
                    frame: entry.vt.frame(),
                })
                .is_ok();
            // Only drop the droppable seed push_layout wrote once the
            // reliable frame actually landed. A failed send means a wedged
            // client (unreachable at birth: fresh 256-cap channel, dead
            // clients already reaped by push_layout) - but if it ever
            // happens, keep the seed so the already-notified droppable path
            // stays the fallback rather than leaving the pane with no
            // delivery at all.
            if sent {
                sent_n += 1;
                c.dirty.lock().unwrap().remove(pid);
            }
        }
        e2e_log(format_args!(
            "attach client {client_id}: reliable frames for {sent_n}/{} visible panes",
            pids.len()
        ));
    }
}

#[cfg(test)]
mod fresh_attach_tests {
    use super::super::tests::empty_core;
    use super::*;

    fn attach_fresh(core: &mut Core, id: u64) -> mpsc::Receiver<ServerMsg> {
        let (tx, rx) = mpsc::channel::<ServerMsg>(64);
        core.attach(
            id,
            24,
            80,
            "/fresh".into(),
            "/fresh".into(),
            tx,
            Arc::default(),
            Arc::new(Notify::new()),
        );
        rx
    }

    #[test]
    fn fresh_attach_lands_its_shell_inline_under_test() {
        // Under cfg(test) the shell job runs inside the attach turn: the
        // fresh squad exists, its reserved pane is registered, and the
        // client is attached - the synchronous contract the unit fixtures
        // rely on, held by the placeholder-plus-inline-landing flow.
        let mut core = empty_core();
        core.shells = vec!["/bin/cat".into()];
        let _rx = attach_fresh(&mut core, 1);
        assert_eq!(core.session.squads.len(), 1, "the fresh squad exists");
        let sq = core.session.squad(1).expect("fresh squad");
        assert_eq!(sq.tabs.len(), 1);
        let pid = sq.tabs[0].focus;
        assert!(core.panes.contains_key(&pid), "the shell pane registered");
        assert_eq!(core.clients.len(), 1, "the attacher is attached");
        assert!(
            core.shell_rc_dirs.is_empty(),
            "an inline unit-fixture shell carries no rc dir"
        );
    }

    #[test]
    fn fresh_attach_spawn_failure_tears_the_placeholder_squad_down() {
        // The placeholder squad is live between the reserve and the landing;
        // a failed spawn must undo the whole gesture: squad (and its store
        // row) removed, the client Byed and dropped.
        let mut core = empty_core();
        core.shells = Vec::new(); // no candidate can spawn
        let mut rx = attach_fresh(&mut core, 1);
        assert!(
            core.session.squads.is_empty(),
            "the placeholder squad is torn down"
        );
        assert!(core.panes.is_empty());
        assert!(core.clients.is_empty(), "the refused client is dropped");
        let mut byes = 0;
        while let Ok(m) = rx.try_recv() {
            if matches!(m, ServerMsg::Bye { .. }) {
                byes += 1;
            }
        }
        assert_eq!(byes, 1, "the attach is refused with exactly one Bye");
    }
}
