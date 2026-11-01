//! The per-client socket writer, and the order it sends dirty panes in.

use super::*;
use tokio::net::unix::OwnedWriteHalf;

/// Take the dirty pane after `last` in id order, wrapping at the end.
///
/// A flooding pane is dirty again by the time its frame is on the socket. Hash
/// order would hand it the socket every time and hold a neighbour's echo until
/// the flood paused, so each dirty pane waits at most one write per other pane.
pub(super) fn take_next_dirty<F>(
    dirty: &mut HashMap<u64, F>,
    last: Option<u64>,
) -> Option<(u64, F)> {
    let key = dirty
        .keys()
        .copied()
        .filter(|pane| Some(*pane) > last)
        .min()
        .or_else(|| dirty.keys().copied().min())?;
    dirty.remove(&key).map(|frame| (key, frame))
}

/// The per-client writer: reliable messages FIRST (biased select - a Layout
/// is never stuck behind a frame burst), then the droppable dirty map. `Bye`
/// is the exception: it flushes the final dirty frames before ending the
/// stream. A write failure exits this half; the reader owns deregistration so
/// commands already on the socket stay ordered before its `Gone` message.
pub(super) async fn client_writer(
    mut w: OwnedWriteHalf,
    mut reliable_rx: mpsc::Receiver<ServerMsg>,
    dirty: DirtyMap,
    notify: Arc<Notify>,
    stats: PaneStats,
) {
    let mut last_pane = None;
    loop {
        tokio::select! {
            biased;
            msg = reliable_rx.recv() => {
                let Some(msg) = msg else { break }; // deregistered by the core
                match write_reliable(&mut w, &msg, &dirty, &stats).await {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => break,
                }
            }
            _ = notify.notified() => {
                // Drain the whole map; every frame is self-contained, and a
                // frame inserted mid-drain re-notifies, so nothing is lost.
                loop {
                    // Reliable messages queued mid-flood jump ahead of the
                    // frame stream (codex P2): continuous re-insertion could
                    // otherwise pin the writer inside this arm, and the
                    // biased select only prioritizes at the select point -
                    // not while an arm is running.
                    while let Ok(msg) = reliable_rx.try_recv() {
                        match write_reliable(&mut w, &msg, &dirty, &stats).await {
                            Ok(true) => return,
                            Ok(false) => {}
                            Err(_) => return,
                        }
                    }
                    let next = take_next_dirty(&mut dirty.lock().unwrap(), last_pane);
                    let Some((pane_id, frame)) = next else { break };
                    last_pane = Some(pane_id);
                    if write_msg(&mut w, &ServerMsg::Frame { pane_id, frame })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    count_frame_emitted(&stats, pane_id);
                }
            }
        }
    }
}
