//! `fno mux serve --link <key>`: open one session of this machine in the calling
//! terminal, live, and take input. Another machine's mux runs it over `ssh -t`,
//! so the host's own binary talks to the host's own server and ssh carries only
//! terminal bytes. A wire bump here crosses no machine pair.
//!
//! A row hosted in a pane takes the link role for that pane
//! (`crate::server::links`). A paneless thread has no pane to link, so the
//! caller runs the harness's own attach inline in this terminal. Either way the
//! host's layout stays as it was. The pty keeps the host's size, so a smaller
//! window crops and the paint keeps the cursor row in view.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use tokio::net::UnixStream;
use tokio::sync::{mpsc, watch};

use crate::mux_cli::{self, Resolution};
use crate::observe::{default_name, spawn_keys, Screen};
use crate::proto::{self, ClientMsg, Frame, LinkHello, MouseEvent, MouseKind, ServerMsg};
use crate::{band_remap, frame_ansi, mouse, share};

/// The far door's exits, the contract `fno-agents attach --dial` reads. Ssh
/// itself answers 255 for a lost connection, which the supervisor redials too.
pub const EXIT_LEFT: i32 = 0;
pub const EXIT_USAGE: i32 = 2;
pub const EXIT_GONE: i32 = 3;
pub const EXIT_RETRY: i32 = 4;

/// Bytes typed in the first moments of a session were typed during the drop:
/// ssh holds them until the channel opens, and a key sent late into an agent
/// is the one failure a redial must never cause.
const STALE_KEY_WINDOW: Duration = Duration::from_millis(300);
/// A hello the server refused leaves a tagless observer that gets no frames.
const FIRST_FRAME_DEADLINE: Duration = Duration::from_secs(5);
/// Wheel reports only; 1003 any-motion would flood a remote link.
const MOUSE_ON: &[u8] = b"\x1b[?1000h\x1b[?1006h";
const MODES_OFF: &[u8] = b"\x1b[?1000l\x1b[?1006l\x1b[?1l\x1b[?1004l\x1b[?2004l\x1b>\x1b[=0;1u";

/// Parsed `fno mux serve --link` arguments. `host` set means this process is
/// the local supervisor; unset means it is the far door itself.
#[derive(Debug, PartialEq, Eq)]
pub struct LinkArgs {
    pub key: String,
    pub host: Option<String>,
    pub session: Option<String>,
}

/// What the caller does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Door {
    Exit(i32),
    /// A paneless row: run `fno-agents attach --inline <name>` in this terminal.
    Attach(String),
}

/// The child a portal seat runs to show `remote` here: this binary's
/// supervisor for the far door, under the `env` wrapper that names the pane.
/// The name stays on this machine: ssh does not forward it. A host or key
/// with a leading `-` would read as an ssh option, so it is refused.
pub fn pane_argv(remote: &proto::RemoteRow) -> Result<Vec<String>, String> {
    let bad = |word: &str| word.is_empty() || word.starts_with('-');
    if bad(&remote.host) || bad(&remote.key) {
        return Err(format!("no remote row at {}/{}", remote.host, remote.key));
    }
    let exe = std::env::current_exe()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| "fno".into());
    let words = [
        "mux",
        "serve",
        "--link",
        &remote.key,
        "--host",
        &remote.host,
    ];
    Ok(
        ["env".into(), format!("FNO_AGENT_SELF={}", remote.name), exe]
            .into_iter()
            .chain(words.into_iter().map(String::from))
            .collect(),
    )
}

/// The far door: resolve the row, then link its pane or hand back its name.
pub fn serve(args: &LinkArgs) -> Door {
    const COMMAND: &str = "fno mux serve --link";
    let session = mux_cli::resolve_session(args.session.as_deref(), None);
    let (rows, now) = match mux_cli::registry_rows_or(COMMAND) {
        Ok(read) => read,
        Err(_) => return Door::Exit(EXIT_RETRY),
    };
    let row = match mux_cli::resolve_selector(&rows, &args.key, now) {
        Resolution::Found(row) => *row,
        Resolution::Ambiguous(_) => {
            eprintln!(
                "{COMMAND}: {:?} matches more than one row; open it by session id",
                args.key
            );
            return Door::Exit(EXIT_GONE);
        }
        Resolution::NotFound => {
            eprintln!("{COMMAND}: no row answers {:?} on this machine", args.key);
            return Door::Exit(EXIT_GONE);
        }
    };
    let pane = row
        .mux
        .clone()
        .filter(|_| !row.exited)
        .or_else(|| crate::server::lifecycle_target::pane_table_host(&row, &session));
    match pane {
        Some((host_session, pane)) => Door::Exit(link_pane(&host_session, pane)),
        None => Door::Attach(row.name),
    }
}

fn link_pane(session: &str, pane: u64) -> i32 {
    const COMMAND: &str = "fno mux serve --link";
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        eprintln!("{COMMAND} needs a terminal: over ssh, run it with `ssh -t host ...`");
        return EXIT_USAGE;
    }
    let socket = match proto::socket_path(session) {
        Ok(path) => path,
        Err(e) => {
            eprintln!("{COMMAND}: {e}");
            return EXIT_USAGE;
        }
    };
    let panes = match share::list_panes(&socket, session) {
        Ok(panes) => panes,
        Err(e) => {
            eprintln!("{COMMAND}: {e}");
            return EXIT_RETRY;
        }
    };
    let Some(identity) = panes
        .iter()
        .find(|p| p.pane_id == pane)
        .and_then(|p| p.child_pid)
        .map(|pid| format!("pid:{pid}"))
    else {
        eprintln!("{COMMAND}: pane {pane} is gone from session {session:?}");
        return EXIT_GONE;
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("{COMMAND}: cannot start runtime: {e}");
            return EXIT_USAGE;
        }
    };
    let (code, reason) = runtime.block_on(drive(socket, session.to_string(), pane, identity));
    runtime.shutdown_timeout(Duration::from_millis(500));
    eprintln!("{reason}");
    code
}

/// Attach passive and read to the first `Layout`. The server's generation
/// decides here, before the hello: an older server closes on the unknown
/// message, which a supervisor would read as a network drop.
async fn connect(
    socket: &Path,
    pane: u64,
    identity: String,
) -> Result<
    (
        tokio::net::unix::OwnedReadHalf,
        tokio::net::unix::OwnedWriteHalf,
    ),
    (i32, String),
> {
    let stream = UnixStream::connect(socket)
        .await
        .map_err(|e| (EXIT_RETRY, format!("cannot reach the mux server: {e}")))?;
    let (mut reader, mut writer) = stream.into_split();
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let attach = ClientMsg::Attach {
        proto: proto::PROTO_VERSION,
        build: proto::BUILD_VERSION.to_string(),
        rows: 0,
        cols: 0,
        cwd,
    };
    let lost = |e: String| (EXIT_RETRY, format!("the mux server went away: {e}"));
    proto::write_msg(&mut writer, &attach)
        .await
        .map_err(|e| lost(e.to_string()))?;
    loop {
        match proto::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(ServerMsg::Layout { proto: server, .. }) => {
                if server.unwrap_or(0) < proto::LINK_MIN_PROTO {
                    return Err((
                        EXIT_USAGE,
                        "this machine's mux server predates the link role: restart it with `fno mux kill-server`, then reopen".to_string(),
                    ));
                }
                break;
            }
            Ok(ServerMsg::Bye { reason }) => {
                return Err((EXIT_USAGE, format!("the mux server refused: {reason}")))
            }
            Ok(_) => {}
            Err(e) => return Err(lost(e.to_string())),
        }
    }
    let hello = LinkHello {
        name: default_name(),
        pane,
        identity,
    };
    proto::write_msg(&mut writer, &ClientMsg::Link(hello))
        .await
        .map_err(|e| lost(e.to_string()))?;
    Ok((reader, writer))
}

/// The terminal modes this door set, undone before [`Screen`] leaves.
struct Modes;

impl Drop for Modes {
    fn drop(&mut self) {
        emit(MODES_OFF);
    }
}

fn emit(bytes: &[u8]) {
    let mut out = std::io::stdout();
    let _ = out.write_all(bytes);
    let _ = out.flush();
}

/// Link until the user leaves, the pane closes or the server goes away.
async fn drive(socket: PathBuf, session: String, pane: u64, identity: String) -> (i32, String) {
    let (mut reader, mut writer) = match connect(&socket, pane, identity.clone()).await {
        Ok(halves) => halves,
        Err(failed) => return failed,
    };
    // Frames are read on their own task because a framed read is not
    // cancel-safe. The watch keeps only the newest, so a stalled ssh leg skips
    // screens instead of queueing them.
    let (frame_tx, mut frames) = watch::channel::<Option<Frame>>(None);
    let (control_tx, mut control) = mpsc::unbounded_channel::<ServerMsg>();
    tokio::spawn(async move {
        while let Ok(msg) = proto::read_msg::<_, ServerMsg>(&mut reader).await {
            match msg {
                ServerMsg::Frame { pane_id, mut frame } if pane_id == pane => {
                    if frame.geometry_ok() {
                        frame.cells.iter_mut().for_each(band_remap::remap);
                        frame_tx.send_replace(Some(frame));
                    }
                }
                ServerMsg::Frame { .. } => {}
                other => {
                    if control_tx.send(other).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let _screen = match Screen::enter() {
        Ok(screen) => screen,
        Err(e) => return (EXIT_USAGE, e),
    };
    let _modes = Modes;
    emit(MOUSE_ON);
    let mut keys = spawn_keys();
    let mut carry = Vec::new();
    let mut resize_tick = tokio::time::interval(Duration::from_millis(250));
    let mut pane_tick = tokio::time::interval(Duration::from_secs(1));
    let size = || crossterm::terminal::size().unwrap_or((80, 24));
    let mut term = size();
    let mut painted = Vec::new();
    let started = Instant::now();
    let mut first_frame: Option<Instant> = None;
    let mut recheck = false;
    loop {
        tokio::select! {
            changed = frames.changed() => {
                if changed.is_err() {
                    return (EXIT_RETRY, "the mux server went away".to_string());
                }
                if let Some(frame) = frames.borrow_and_update().as_ref() {
                    first_frame.get_or_insert_with(Instant::now);
                    emit(&link_paint(frame, term, &mut painted));
                }
            }
            msg = control.recv() => match msg {
                None => return (EXIT_RETRY, "the mux server went away".to_string()),
                Some(ServerMsg::Bye { reason }) => {
                    return (EXIT_RETRY, format!("the mux server ended the link: {reason}"))
                }
                Some(ServerMsg::Layout { .. }) => recheck = true,
                Some(ServerMsg::ModeSync { bytes }) => {
                    emit(&bytes);
                    emit(MOUSE_ON);
                }
                Some(_) => {}
            },
            typed = keys.recv() => {
                let Some(bytes) = typed else {
                    return (EXIT_LEFT, "terminal closed".to_string());
                };
                let fresh = first_frame.is_some_and(|at| at.elapsed() >= STALE_KEY_WINDOW);
                let (reports, pass) = mouse::extract_mouse(&mut carry, &bytes);
                let start = frames
                    .borrow()
                    .as_ref()
                    .map_or(0, |frame| window_start(frame, term.1));
                for msg in fresh
                    .then(|| link_messages(pane, start, &reports, pass))
                    .into_iter()
                    .flatten()
                {
                    if let Err(e) = proto::write_msg(&mut writer, &msg).await {
                        return (EXIT_RETRY, format!("the mux server went away: {e}"));
                    }
                }
            }
            _ = resize_tick.tick() => {
                let now = size();
                if now != term {
                    term = now;
                    painted.clear();
                    emit(b"\x1b[2J");
                    if let Some(frame) = frames.borrow().as_ref() {
                        emit(&link_paint(frame, term, &mut painted));
                    }
                }
            }
            _ = pane_tick.tick() => {
                if first_frame.is_none() && started.elapsed() > FIRST_FRAME_DEADLINE {
                    return (EXIT_GONE, format!("the mux server refused the link to pane {pane}"));
                }
                if std::mem::take(&mut recheck) {
                    let (sock, name) = (socket.clone(), session.clone());
                    let panes = tokio::task::spawn_blocking(move || share::list_panes(&sock, &name)).await;
                    if let Ok(Ok(panes)) = panes {
                        if !share::pane_still_shared(&panes, pane, &identity) {
                            return (EXIT_GONE, format!("pane {pane} closed"));
                        }
                    }
                }
            }
        }
    }
}

/// What one chunk of typed bytes sends: wheel ticks to the pane's own scroll or
/// app, every other byte as input. Clicks stay here: a click means selection
/// on the mux side, and a selection is every viewer's to see.
fn link_messages(
    pane: u64,
    start: u16,
    reports: &[mouse::MouseReport],
    pass: Vec<u8>,
) -> Vec<ClientMsg> {
    let mut out: Vec<ClientMsg> = reports
        .iter()
        .filter(|r| matches!(r.kind, MouseKind::WheelUp | MouseKind::WheelDown))
        .map(|r| ClientMsg::Mouse {
            pane,
            event: MouseEvent {
                row: r.row + start,
                col: r.col,
                kind: r.kind,
                alt: r.alt,
            },
        })
        .collect();
    if !pass.is_empty() {
        out.push(ClientMsg::Input(pass));
    }
    out
}

/// The frame row painted at the terminal's top. A grid taller than the window
/// shows its bottom, where an agent's composer sits, unless the cursor is above
/// that.
fn window_start(frame: &Frame, rows: u16) -> u16 {
    let over = frame.rows.saturating_sub(rows);
    if frame.cursor_visible && frame.cursor_row < over {
        frame.cursor_row.saturating_sub(rows / 2).min(over)
    } else {
        over
    }
}

/// The bytes that bring the terminal to `frame`: only rows that changed since
/// `painted`, then the cursor. An idle agent's spinner costs one row this way.
fn link_paint(frame: &Frame, size: (u16, u16), painted: &mut Vec<Vec<u8>>) -> Vec<u8> {
    let (cols, rows) = size;
    let start = window_start(frame, rows);
    let width = frame.cols.min(cols) as usize;
    painted.resize(rows as usize, Vec::new());
    let mut out = Vec::new();
    for screen_row in 0..rows {
        let source = start + screen_row;
        let mut line = Vec::new();
        if source < frame.rows {
            let begin = source as usize * frame.cols as usize;
            frame_ansi::paint_row(&mut line, &frame.cells[begin..begin + width]);
        }
        line.extend_from_slice(b"\x1b[0m\x1b[K");
        if painted[screen_row as usize] != line {
            out.extend_from_slice(format!("\x1b[{};1H", screen_row + 1).as_bytes());
            out.extend_from_slice(&line);
            painted[screen_row as usize] = line;
        }
    }
    if frame.scroll_offset > 0 && rows > 0 {
        let mark = format!("[+{}]", frame.scroll_offset);
        let at = cols.saturating_sub(mark.len() as u16) + 1;
        out.extend_from_slice(format!("\x1b[1;{at}H\x1b[7m{mark}\x1b[0m").as_bytes());
        painted[0].clear();
    }
    let visible = frame.cursor_visible
        && frame.cursor_row >= start
        && frame.cursor_row < start.saturating_add(rows)
        && frame.cursor_col < cols;
    if visible {
        let (row, col) = (frame.cursor_row - start + 1, frame.cursor_col + 1);
        out.extend_from_slice(format!("\x1b[{row};{col}H\x1b[?25h").as_bytes());
    } else {
        out.extend_from_slice(b"\x1b[?25l");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::Cell;

    fn frame(rows: u16, cols: u16, text: &[&str]) -> Frame {
        let mut cells = vec![Cell::default(); rows as usize * cols as usize];
        for (r, line) in text.iter().enumerate() {
            for (c, ch) in line.chars().enumerate() {
                cells[r * cols as usize + c].c = ch;
            }
        }
        Frame {
            rows,
            cols,
            cells,
            ..Frame::default()
        }
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    #[test]
    fn link_paint_sends_only_changed_rows_and_places_the_cursor() {
        let mut painted = Vec::new();
        let mut one = frame(2, 5, &["alpha", "bravo"]);
        one.cursor_visible = true;
        one.cursor_row = 1;
        one.cursor_col = 2;
        let first = text(&link_paint(&one, (5, 2), &mut painted));
        assert!(first.contains("alpha") && first.contains("bravo"));
        assert!(first.ends_with("\x1b[2;3H\x1b[?25h"), "{first:?}");

        let two = frame(2, 5, &["alpha", "bRavo"]);
        let second = text(&link_paint(&two, (5, 2), &mut painted));
        assert!(second.contains("bRavo"));
        assert!(!second.contains("alpha"), "an unchanged row is not resent");
    }
}
