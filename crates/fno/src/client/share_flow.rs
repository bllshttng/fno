//! The mux's side of the pane share. A key or menu row starts a managed
//! `fno mux serve --share` on the focused pane and copies its join link. Each
//! knock from a viewer raises a popup on whatever pane the driver is on, and
//! Enter admits. The share ends when the driver stops it or the client exits:
//! the child reads its commands from our pipe, so closing the pipe ends it.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

/// What the child's output tells the run loop.
pub(crate) enum ShareMsg {
    Link(String),
    Knock(u64, String),
    Gone(u64),
    Ended(String),
}

/// A message tagged with the share that sent it, so a stopped share's last
/// words never touch the next one.
pub(crate) type ShareTx = tokio::sync::mpsc::UnboundedSender<(u64, ShareMsg)>;

/// What a share row or popup row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Act {
    Start,
    Copy,
    Stop,
    Knocks,
    Admit(u64),
    Deny(u64),
}

pub(crate) struct Share {
    gen: u64,
    pane: u64,
    link: Option<String>,
    knocks: Vec<(u64, String)>,
    stdin: tokio::process::ChildStdin,
    _child: tokio::process::Child,
}

impl Share {
    pub(crate) fn waiting(&self) -> usize {
        self.knocks.len()
    }
}

fn parse_line(line: &str) -> Option<ShareMsg> {
    let (verb, rest) = line.split_once(' ')?;
    match verb {
        "link" => Some(ShareMsg::Link(rest.to_string())),
        "knock" => {
            let (id, name) = rest.split_once(' ')?;
            Some(ShareMsg::Knock(id.parse().ok()?, name.to_string()))
        }
        "gone" => Some(ShareMsg::Gone(rest.parse().ok()?)),
        _ => None,
    }
}

/// Forward the child's protocol lines until it exits, then say why. A share
/// that never started explains itself on stderr, so its last line is the
/// reason when no `end` line came.
async fn read_child(
    gen: u64,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    tx: ShareTx,
) {
    let err = tokio::spawn(async move {
        let mut last = String::new();
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            last = line;
        }
        last
    });
    let mut reason = String::new();
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(end) = line.strip_prefix("end ") {
            reason = end.to_string();
        } else if let Some(msg) = parse_line(&line) {
            let _ = tx.send((gen, msg));
        }
    }
    if reason.is_empty() {
        reason = err.await.unwrap_or_default();
    }
    let _ = tx.send((gen, ShareMsg::Ended(reason)));
}

/// The key chord: start a share of the focused pane, or copy the link of the
/// one already running.
pub(crate) fn chord(view: &mut View) {
    if !view.experimental_pane_share {
        view.set_notice("pane share is off: sideline menu > experimental".into());
        return;
    }
    if view.share.is_some() {
        copy_link(view);
    } else {
        start(view);
    }
}

fn start(view: &mut View) {
    let Some(tx) = view.share_tx.clone() else {
        return;
    };
    let pane = view.layout.focus;
    let mut command = crate::process_admission::tokio_command(crate::server::fno_bin());
    command
        .args(["mux", "serve", "--share", "--managed", "--tunnel", "--pane"])
        .arg(pane.to_string())
        .arg("--server")
        .arg(&view.session)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match crate::process_admission::tokio_spawn(&mut command) {
        Ok(child) => child,
        Err(e) => {
            view.set_notice(format!("share failed to start: {e}"));
            return;
        }
    };
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        view.set_notice("share failed to start: no pipes".into());
        return;
    };
    let gen = NEXT_GEN.fetch_add(1, Ordering::Relaxed);
    tokio::spawn(read_child(gen, stdout, stderr, tx));
    view.share = Some(Share {
        gen,
        pane,
        link: None,
        knocks: Vec::new(),
        stdin,
        _child: child,
    });
    view.set_notice(format!("starting a share of pane {pane}"));
}

fn copy_link(view: &mut View) {
    match view.share.as_ref().and_then(|s| s.link.clone()) {
        Some(link) => super::feed_detail::copy_value(view, link),
        None => view.set_notice("the share has no link yet".into()),
    }
}

/// A message from the child landed.
pub(crate) fn apply(view: &mut View, gen: u64, msg: ShareMsg) {
    if view.share.as_ref().map(|s| s.gen) != Some(gen) {
        return;
    }
    match msg {
        ShareMsg::Link(link) => {
            if let Some(s) = view.share.as_mut() {
                s.link = Some(link);
            }
            copy_link(view);
        }
        ShareMsg::Knock(id, name) => {
            if let Some(s) = view.share.as_mut() {
                s.knocks.push((id, name));
            }
            open_knocks(view);
        }
        ShareMsg::Gone(id) => {
            if let Some(s) = view.share.as_mut() {
                s.knocks.retain(|(k, _)| *k != id);
            }
            let showing = view.aux.as_ref().is_some_and(|a| {
                a.actions
                    .iter()
                    .any(|x| matches!(x, AuxAction::Share(Act::Admit(_))))
            });
            if showing {
                open_knocks(view);
            }
        }
        ShareMsg::Ended(reason) => {
            view.share = None;
            view.set_notice(if reason.is_empty() {
                "share ended".to_string()
            } else {
                format!("share ended: {reason}")
            });
        }
    }
}

/// The popup for the oldest waiting knock, or no popup when none waits.
fn open_knocks(view: &mut View) {
    let Some(share) = view.share.as_ref() else {
        return;
    };
    let Some((id, name)) = share.knocks.first().cloned() else {
        view.aux = None;
        return;
    };
    let more = match share.knocks.len() {
        1 => String::new(),
        n => format!(" ({n} waiting)"),
    };
    let entry = |glyph: &str, label: &str| PopupRow::Entry {
        glyph: glyph.into(),
        label: label.into(),
        hint: String::new(),
        enabled: true,
    };
    let rows = vec![
        PopupRow::Header(format!("{name} asks to watch pane {}{more}", share.pane)),
        PopupRow::Rule,
        entry("✓", "admit"),
        entry("✗", "deny"),
    ];
    let actions = vec![
        AuxAction::Share(Act::Admit(id)),
        AuxAction::Share(Act::Deny(id)),
    ];
    view.aux = Some(AuxPopup {
        popup: Popup::new(rows, Anchor::Center),
        actions,
    });
    view.aux_esc.clear();
}

/// Answer one knock through the child's stdin, then show the next.
async fn answer(view: &mut View, id: u64, verb: char) {
    let sent = match view.share.as_mut() {
        Some(share) => {
            share.knocks.retain(|(k, _)| *k != id);
            share
                .stdin
                .write_all(format!("{verb} {id}\n").as_bytes())
                .await
                .is_ok()
        }
        None => false,
    };
    if !sent {
        view.set_notice("the share is gone".into());
    }
    open_knocks(view);
}

/// A share row or popup row was chosen.
pub(crate) async fn act(view: &mut View, act: Act) {
    match act {
        Act::Start => {
            view.aux = None;
            chord(view);
        }
        Act::Copy => {
            view.aux = None;
            copy_link(view);
        }
        Act::Stop => {
            view.aux = None;
            view.share = None;
            view.set_notice("share stopped".into());
        }
        Act::Knocks => open_knocks(view),
        Act::Admit(id) => answer(view, id, 'a').await,
        Act::Deny(id) => answer(view, id, 'd').await,
    }
}

/// The share rows of the sideline menu and the actions they pair with: one
/// start row while idle, three while sharing. `waiting` is `None` when no
/// share runs. Empty while the experimental toggle is off.
pub(crate) fn menu_entries(
    pane_share: bool,
    waiting: Option<usize>,
) -> (Vec<PopupRow>, Vec<AuxAction>) {
    let entry = |glyph: &str, label: String, act: Act| {
        (
            PopupRow::Entry {
                glyph: glyph.into(),
                label,
                hint: String::new(),
                enabled: true,
            },
            AuxAction::Share(act),
        )
    };
    let mut rows = Vec::new();
    if pane_share {
        match waiting {
            None => rows.push(entry("⇪", "share focused pane".into(), Act::Start)),
            Some(n) => {
                if n > 0 {
                    rows.push(entry(
                        "●",
                        format!("{n} waiting: admit or deny"),
                        Act::Knocks,
                    ));
                }
                rows.push(entry("⧉", "copy share link".into(), Act::Copy));
                rows.push(entry("■", "stop sharing".into(), Act::Stop));
            }
        }
    }
    rows.into_iter().unzip()
}
