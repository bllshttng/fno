//! The numbered move picker's commit: where a digit or Enter becomes the
//! command the picked destination names. Moved out of `client.rs` (file
//! budget) when a session pin and a folder add joined the tab and pane moves.

use super::wire_version::{server_has_workspace_pins, workspace_pins_skew_notice};
use super::*;

/// Send the command that destination `idx` of `squads` names. A destination
/// that no longer exists, or an index past the list, is a BEL that sends
/// nothing.
pub(super) async fn commit(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
    src: MoveSrc,
    squads: &[u64],
    idx: usize,
) -> Result<(), String> {
    // The folder pick lists folders, not workspaces: its entries are indices
    // into the source's folder list.
    if let MoveSrc::Folder { squad, folders } = &src {
        let path = squads.get(idx).and_then(|&i| folders.get(i as usize));
        match path {
            Some(_) if !server_has_workspace_pins(view.server_proto) => {
                view.set_notice(workspace_pins_skew_notice())
            }
            Some(path) => write_msg(
                sock_w,
                &ClientMsg::Command(Command::AddWorkspaceFolder {
                    squad: *squad,
                    path: path.clone(),
                }),
            )
            .await
            .map_err(|e| format!("add-folder send failed: {e}"))?,
            None => {
                let _ = raw_out(b"\x07");
            }
        }
        return Ok(());
    }
    match squads.get(idx) {
        // The captured id must still name a live squad; the server refuses a
        // stale id regardless, but pre-validating saves a round-trip and
        // keeps the BEL local.
        Some(&sq) if view.layout.squads.iter().any(|s| s.id == sq) => match src {
            MoveSrc::Tab(tab) => {
                write_msg(
                    sock_w,
                    &ClientMsg::Command(Command::MoveTab { tab, squad: sq }),
                )
                .await
                .map_err(|e| format!("move-tab send failed: {e}"))?;
            }
            // A pane move needs an anchor pane in the destination: its
            // active tab's focus leaf (else the first leaf). MovePane
            // grafts the mover beside that anchor, de-recruiting it from
            // the source workspace (the `move_pane_cross_tab` path).
            // `target` is mandatory for a cross-tab move; without it the
            // server navigates from the mover, which lives in another
            // tab, and the move is refused.
            MoveSrc::Pane(pid) => {
                // Prefer the active tab's focus leaf; if that tab has no
                // panes (transient during a close, or an older server with
                // empty TabMeta.panes), fall back to the first leaf in any
                // tab so a valid anchor is not missed.
                let anchor = view
                    .layout
                    .squads
                    .iter()
                    .find(|s| s.id == sq)
                    .and_then(|s| {
                        s.tabs
                            .get(s.active_tab)
                            .and_then(|t| t.panes.first())
                            .or_else(|| s.tabs.iter().flat_map(|t| t.panes.first()).next())
                            .map(|p| p.id)
                    });
                match anchor {
                    Some(anchor) => {
                        write_msg(
                            sock_w,
                            &ClientMsg::Command(Command::MovePane {
                                mover: Some(pid),
                                target: Some(anchor),
                                dir: Dir::Right,
                            }),
                        )
                        .await
                        .map_err(|e| format!("move-pane send failed: {e}"))?;
                    }
                    None => view.set_notice("no pane in that workspace to anchor against".into()),
                }
            }
            // A session moved by hand: the server pins it on the destination.
            MoveSrc::Row {
                harness,
                harness_session_id,
            } => {
                if server_has_workspace_pins(view.server_proto) {
                    write_msg(
                        sock_w,
                        &ClientMsg::Command(Command::PinThread {
                            squad: sq,
                            harness,
                            harness_session_id,
                        }),
                    )
                    .await
                    .map_err(|e| format!("pin-thread send failed: {e}"))?;
                } else {
                    view.set_notice(workspace_pins_skew_notice());
                }
            }
            MoveSrc::Folder { .. } => {}
        },
        _ => {
            let _ = raw_out(b"\x07");
        }
    }
    Ok(())
}
