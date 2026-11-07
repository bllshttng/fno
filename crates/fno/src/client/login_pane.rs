//! Login panes the operator can type into: the Accounts `r` login
//! used to spawn `fno mux pane run ...` as a subprocess and stay in the
//! Accounts view, so the browser's paste-this-code prompt ran in a pane the
//! operator never saw. The pane spawn rides the wire `PaneRun` instead (so
//! the fresh pane's id comes back), the Accounts modal closes, and the pane
//! is focused, which hands keystrokes to the login prompt.

use super::compositor::Compositor;
use super::{ClientMsg, Command, ServerMsg, View};
use crate::connections_view::{run_command_env, ActionResult};
use crate::mux_cli::{control_roundtrip_with_timeouts, ControlError};
use crate::proto::{socket_path, write_msg, ControlCommand, PanePlacement};
use std::time::Duration;

/// The control deadlines: a wedged server holds the login no longer.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

/// The conn_action job's runner: a login argv rides the wire PaneRun (so the
/// fresh pane's id comes back), anything else keeps the subprocess door.
pub(super) async fn run_or_command(
    session: &str,
    argv: Vec<String>,
    env: Vec<(String, String)>,
    is_login: bool,
) -> ActionResult {
    if is_login {
        run(session, argv).await
    } else {
        run_command_env(argv, env).await
    }
}

/// The landing decision, pure so tests can pin it: a pane id in the result
/// means the pane exists on the server (whatever the modal did in flight),
/// and the landing focuses it; anything else keeps the modal and its notice.
pub(super) enum Landing {
    FocusPane(u64),
    ModalNotice,
}

/// The one-shot door to a pane the Accounts view asks for: the CLI argv
/// carries the `mux pane run` prefix the reducer builds. Strips it and hands
/// back the pane argv; `None` for any other shape, which keeps the
/// subprocess door as the fallback.
fn pane_argv(argv: &[String]) -> Option<Vec<String>> {
    if argv.len() > 3 && argv[..3] == ["mux", "pane", "run"] {
        return Some(argv[3..].to_vec());
    }
    None
}

/// Spawn the login pane through the session's control socket and name the
/// fresh pane in the result, so the landing can focus it. A non-`mux pane
/// run` argv keeps the subprocess door.
pub(super) async fn run(session: &str, argv: Vec<String>) -> ActionResult {
    let Some(pane_argv) = pane_argv(&argv) else {
        return run_command_env(argv, Vec::new()).await;
    };
    let session = session.to_string();
    let sock = match socket_path(&session) {
        Ok(path) => path,
        Err(e) => return refused(argv, format!("login pane: {e}")),
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let command = ControlCommand::PaneRun {
        cwd: cwd.to_string_lossy().into_owned(),
        argv: pane_argv,
        cols: None,
        rows: None,
        claim: false,
        placement: PanePlacement::default(),
        worker: None,
        fno_id: None,
    };
    let reply = tokio::task::spawn_blocking(move || {
        control_roundtrip_with_timeouts(&sock, &session, command, CONTROL_TIMEOUT, CONTROL_TIMEOUT)
    })
    .await;
    match reply {
        Ok(Ok(ServerMsg::PaneSpawned { pane_id, .. })) => ActionResult {
            ok: true,
            msg: focus_notice(pane_id, &argv),
            pane: Some(pane_id),
        },
        Ok(Ok(other)) => refused(
            argv,
            format!("the login pane never started: unexpected server reply {other:?}"),
        ),
        Ok(Err(e)) => refused(argv, control_reason(&e)),
        Err(join) => refused(argv, format!("login pane task failed: {join}")),
    }
}

fn control_reason(e: &ControlError) -> String {
    e.to_string()
}

/// The notice the landing shows. A codex login still owes its register, so
/// the notice names the r step; the `use --login` and wizard panes finish
/// the whole job themselves.
fn focus_notice(pane_id: u64, argv: &[String]) -> String {
    if argv.len() > 4 && argv[3] == "codex" && argv[4] == "login" {
        return format!(
            "login pane {pane_id} focused - finish the login there, then press r in Accounts"
        );
    }
    format!("login pane {pane_id} focused - finish the login there")
}

fn refused(argv: Vec<String>, reason: String) -> ActionResult {
    ActionResult {
        ok: false,
        msg: format!("{}: {reason}", argv.join(" ")),
        pane: None,
    }
}

/// The landing for a finished login spawn. A live pane pulls the operator
/// out of the Accounts modal and into the pane: the modal closes, a main-view
/// notice names where the login runs, and the pane takes focus so keystrokes
/// reach the login prompt. A failure keeps the modal and shows the reason
/// there, gen-guarded like every other modal read.
pub(super) async fn land(
    view: &mut View,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
    gen: u64,
    result: ActionResult,
    compositor: &mut Compositor,
) -> Result<(), String> {
    match landing(&result) {
        Landing::FocusPane(pane) => {
            if view.connections.is_some() {
                view.close_connections();
            }
            view.set_notice(result.msg);
            write_msg(sock_w, &ClientMsg::Command(Command::FocusPane(pane)))
                .await
                .map_err(|e| format!("focus send failed: {e}"))?;
        }
        Landing::ModalNotice => {
            if gen == view.conn_gen {
                if let Some(cv) = view.connections.as_mut() {
                    cv.notice = Some(result.msg);
                }
                view.rearm_connections_read();
            }
        }
    }
    compositor
        .draw(&view.compose())
        .map_err(|e| format!("draw: {e}"))
}

/// A pane id in the result means the pane exists on the server, whatever the
/// modal did while the spawn was in flight: focus it. Anything else keeps
/// the modal and its notice.
fn landing(result: &ActionResult) -> Landing {
    match result {
        ActionResult {
            ok: true,
            pane: Some(pane),
            ..
        } => Landing::FocusPane(*pane),
        _ => Landing::ModalNotice,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pane_argv_strips_the_mux_pane_run_prefix() {
        let full = argv(&[
            "mux",
            "pane",
            "run",
            "fno",
            "config",
            "accounts",
            "use",
            "readyrule",
            "--scope",
            "global",
            "--login",
        ]);
        let stripped = pane_argv(&full).expect("prefix stripped");
        assert_eq!(stripped.len(), 8);
        assert_eq!(stripped[0], "fno");
        assert_eq!(stripped[7], "--login");
    }

    #[test]
    fn pane_argv_keeps_the_subprocess_door_for_other_shapes() {
        assert_eq!(
            pane_argv(&argv(&["fno", "config", "accounts", "list"])),
            None
        );
        assert_eq!(pane_argv(&argv(&["mux", "pane", "run"])), None);
        let stripped =
            pane_argv(&argv(&["mux", "pane", "run", "claude", "/login"])).expect("prefix stripped");
        assert_eq!(stripped, argv(&["claude", "/login"]));
    }
}
