//! The pane-output pump: the core loop's PTY drain, moved beside the
//! notification drain it now carries (file-budget ratchet). Feeds
//! keeper-forwarded pane bytes into the per-pane VT; since wire v110 it also
//! drains each pane's captured desktop notifications right after the feed,
//! so the queue never carries a live pane's notice across reads.

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{Core, PaneChunk};

/// Drain the keeper's pane-output channel into the panes, and report whether
/// anything was drained. Touched panes are broadcast and re-sync their
/// modes; captured notifications are re-emitted titled with the pane name
/// (see `super::notice`).
pub(super) fn drain_pty_output(
    core: &mut Core,
    out_rx: &mut tokio::sync::mpsc::Receiver<(u64, PaneChunk)>,
    first: Option<(u64, PaneChunk)>,
    e2e_first_out: &mut HashSet<u64>,
) -> bool {
    let mut drained = false;
    let mut touched = HashSet::new();
    let mut fired_notices: Vec<(u64, String)> = Vec::new();
    {
        let mut feed = |pid: u64, chunk: PaneChunk| {
            drained = true;
            match chunk {
                PaneChunk::Output(bytes) => {
                    if e2e_first_out.insert(pid) {
                        super::e2e_log(format_args!(
                            "core loop: first output from pane {pid} ({} bytes)",
                            bytes.len()
                        ));
                    }
                    if let Some(entry) = core.panes.get_mut(&pid) {
                        let t0 = Instant::now();
                        entry.vt.feed(&bytes);
                        // Notifications ride out of the pane at capture
                        // time: drained here so the queue never carries a
                        // live pane's notice across feeds, then re-emitted
                        // (titled with the pane name) after the borrow
                        // drops.
                        for notice in entry.vt.take_notices() {
                            fired_notices.push((pid, notice.body));
                        }
                        entry.last_output = t0;
                        entry
                            .stats
                            .bytes_in
                            .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        entry.stats.grid_updates.fetch_add(1, Ordering::Relaxed);
                        entry
                            .stats
                            .cpu_ns
                            .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
                        touched.insert(pid);
                    } else {
                        core.buffer_in_flight_output(pid, &bytes);
                    }
                    // A pane nobody focuses has no terminal to answer its
                    // palette probes; the mux does, from the pane's own
                    // ground. Focused panes ride the client loopback.
                    core.answer_color_queries(pid, &bytes);
                }
                PaneChunk::Resized(rows, cols) => {
                    // The keeper's resize round trip landed: apply the VT
                    // dimension change here, at its exact point in this
                    // pane's own ordered channel, never eagerly when the
                    // resize was issued (server.rs's push_layout). Any
                    // trailing pre-resize output is necessarily ahead
                    // of this marker in the same channel, so it is always
                    // fed before the resize lands.
                    if let Some(entry) = core.panes.get_mut(&pid) {
                        entry.vt.resize(rows, cols);
                        touched.insert(pid);
                    }
                }
            }
        };
        if let Some((pid, chunk)) = first {
            feed(pid, chunk);
        }
        while let Ok((pid, chunk)) = out_rx.try_recv() {
            feed(pid, chunk);
        }
    }
    for pid in touched {
        core.broadcast_pane(pid);
        core.note_pane_output(pid);
    }
    for (pid, body) in fired_notices {
        core.broadcast_pane_notice(pid, &body);
    }
    if drained {
        core.sync_focused_modes();
    }
    drained
}
