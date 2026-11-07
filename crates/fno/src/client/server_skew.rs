//! The server-skew surface: one popup that answers "the attached mux server
//! is too old for the command just asked" (law d-2bd8976d: the task finishes
//! inside the mux), plus the post-restart receipt the fresh attach reads.

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
        PopupRow::Text(format!("needs newer mux code: {wanted}")),
        PopupRow::Text(format!(
            "{server}; this client v{}",
            crate::proto::PROTO_VERSION
        )),
        PopupRow::Rule,
        PopupRow::Text("restart keeps every pane with a keeper.".into()),
        PopupRow::Text("a session with a keeperless pane refuses the restart;".into()),
        PopupRow::Text("nothing is killed. every live mux session restarts.".into()),
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
            .footer("esc close"),
        actions: vec![AuxAction::RestartAgents],
    }
}

/// Open the popup whose only entry runs the update modal's restart tap.
pub(super) fn offer_server_restart(view: &mut View, wanted: &str) {
    view.aux = Some(build_skew_popup(view.server_proto, wanted));
    view.aux_esc.clear();
}

/// One preserved pane: the row the kill measured with a keeper.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct PreservedPane {
    pub(super) pane_id: u64,
    #[serde(default)]
    pub(super) name: Option<String>,
}

/// What one restart did, written by the detaching client and read once by
/// the fresh one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct RestartReceipt {
    pub(super) exit_code: i32,
    pub(super) verdict: String,
    pub(super) preserved: Vec<PreservedPane>,
}

/// The receipt file for one mux session under the shared mux dir.
fn receipt_path(session: &str) -> std::path::PathBuf {
    crate::proto::mux_dir()
        .join("restart-receipts")
        .join(format!("{session}.json"))
}

/// Fold the restart's captured stdout (the one machine line) into a receipt;
/// unparseable output still yields exit_code plus an honest verdict.
pub(super) fn receipt_from_stdout(
    session: &str,
    exit_code: i32,
    stdout: Option<&str>,
) -> RestartReceipt {
    let parsed = stdout.and_then(|out| {
        out.lines()
            .find_map(|line| line.strip_prefix(MACHINE_PREFIX))
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
    });
    let verdict = parsed
        .as_ref()
        .and_then(|v| v.get("verdict"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            if exit_code == 0 {
                "restart summary unreadable; panes unverified".into()
            } else {
                "restart failed before a verdict".into()
            }
        });
    let mut preserved = Vec::new();
    let sessions = parsed
        .as_ref()
        .and_then(|v| v.get("mux"))
        .and_then(|m| m.get("sessions"))
        .and_then(|s| s.as_array());
    for s in sessions.into_iter().flatten() {
        if s.get("session").and_then(|v| v.as_str()) != Some(session) {
            continue;
        }
        let rows = s.get("preserved").and_then(|p| p.as_array());
        for row in rows.into_iter().flatten() {
            if let Ok(p) = serde_json::from_value::<PreservedPane>(row.clone()) {
                preserved.push(p);
            }
        }
    }
    RestartReceipt {
        exit_code,
        verdict,
        preserved,
    }
}

/// The machine line the restart prints on stdout in `--json` mode.
const MACHINE_PREFIX: &str = "fno agents restart: keepers ";

/// The notice for a receipt: refused, or the return with each missing pane
/// named. One line - a notice renders one physical line.
pub(super) fn restart_report(receipt: &RestartReceipt, live_pane_ids: &[u64]) -> String {
    if receipt.exit_code != 0 {
        return format!("restart refused: {}", receipt.verdict);
    }
    let kept = receipt.preserved.len();
    let back = receipt
        .preserved
        .iter()
        .filter(|p| live_pane_ids.contains(&p.pane_id))
        .count();
    let mut text = format!("restart: {back} of {kept} kept panes came back");
    for p in &receipt.preserved {
        if !live_pane_ids.contains(&p.pane_id) {
            let name = p.name.as_deref().unwrap_or("unnamed");
            text.push_str(&format!("; pane {} ({name}) did not come back", p.pane_id));
        }
    }
    text
}

/// Write the receipt the fresh attach will read; best-effort, never blocks
/// the reattach.
pub(super) fn write_restart_receipt(session: &str, exit_code: i32, stdout: Option<&str>) {
    let receipt = receipt_from_stdout(session, exit_code, stdout);
    let path = receipt_path(session);
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("fno: restart receipt not written: {e}");
            return;
        }
    }
    let body = match serde_json::to_string_pretty(&receipt) {
        Ok(body) => body,
        Err(e) => {
            eprintln!("fno: restart receipt not written: {e}");
            return;
        }
    };
    if let Err(e) = std::fs::write(&path, body) {
        eprintln!("fno: restart receipt not written: {e}");
    }
}

/// The fresh attach's receipt read: post the report once, delete the file.
/// A missing receipt is the common case: silent.
pub(super) fn report_restart_receipt(view: &mut View) {
    let path = receipt_path(&view.session);
    let Ok(raw) = std::fs::read(&path) else {
        return;
    };
    let _ = std::fs::remove_file(&path);
    let notice = match serde_json::from_slice::<RestartReceipt>(&raw) {
        Ok(receipt) => {
            let live: Vec<u64> = view.layout.panes.iter().map(|(id, _)| *id).collect();
            restart_report(&receipt, &live)
        }
        Err(_) => "restart receipt unreadable; kept panes unverified".into(),
    };
    view.set_notice(notice);
}
