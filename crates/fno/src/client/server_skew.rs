//! The server-skew surface: one popup that answers "the attached mux server
//! is too old for the command just asked: what now?" (law d-2bd8976d: the
//! task finishes inside the mux). Every wire-version gate funnels here
//! instead of a dead-end notice, and the post-restart receipt lives beside
//! it so the fresh attach reports which kept panes came back.

use super::*;

/// Build the skew popup. Split from [`offer_server_restart`] so the
/// paint-shot snapshot can open it against an invented version pair.
pub(super) fn build_skew_popup(server_proto: Option<u32>, wanted: &str) -> AuxPopup {
    let server = match server_proto {
        Some(v) => format!("server v{v}"),
        None => "server never announced its version (pre-v97)".into(),
    };
    let rows = vec![
        PopupRow::Header("the mux server is too old".into()),
        PopupRow::Rule,
        PopupRow::Text(format!("this command needs newer mux code: {wanted}")),
        PopupRow::Text(format!(
            "{server}; this client v{}",
            crate::proto::PROTO_VERSION
        )),
        PopupRow::Rule,
        PopupRow::Text(
            "restart keeps every pane with a keeper. a session holding a keeperless pane \
             refuses the restart and nothing is killed."
                .into(),
        ),
        PopupRow::Text("every live mux session on this machine restarts.".into()),
        PopupRow::Rule,
        PopupRow::Entry {
            glyph: "↻".into(),
            label: "restart the mux server now (keeps panes)".into(),
            hint: String::new(),
            enabled: true,
        },
    ];
    AuxPopup {
        popup: Popup::new(rows, Anchor::Center)
            .title("server too old")
            .footer("esc close")
            .width_cap(usize::MAX),
        actions: vec![AuxAction::RestartAgents],
    }
}

/// The one skew refusal: open the popup whose only entry runs the update
/// modal's restart tap (detach, `fno agents restart --mux` in the foreground,
/// reattach). Replaces the notice-only dead end at every wire-version gate.
pub(super) fn offer_server_restart(view: &mut View, wanted: &str) {
    view.aux = Some(build_skew_popup(view.server_proto, wanted));
    view.aux_esc.clear();
}
