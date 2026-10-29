//! The mux's side of the pane share. A key or menu row starts a managed
//! `fno mux serve --share` on the focused pane and copies its join link. Each
//! knock from a viewer raises a popup on whatever pane the driver is on;
//! Up then Enter admits. The share ends when the driver stops it or the client exits:
//! the child reads its commands from our pipe, so closing the pipe ends it.

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

static NEXT_GEN: AtomicU64 = AtomicU64::new(1);

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
    let (command, rest) = line.split_once(' ')?;
    match command {
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
        view.set_notice("pane share is off: menu > settings > experimental".into());
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
    let exe = std::env::current_exe().unwrap_or_else(|_| "fno".into());
    let mut command = crate::process_admission::tokio_command(&exe);
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
            // A second knock refreshes the count but leaves the cursor where
            // the driver put it.
            let sel = knock_popup_sel(view);
            open_knocks(view);
            if let (Some(sel), Some(aux)) = (sel, view.aux.as_mut()) {
                aux.popup.sel = sel;
            }
        }
        ShareMsg::Gone(id) => {
            if let Some(s) = view.share.as_mut() {
                s.knocks.retain(|(k, _)| *k != id);
            }
            if knock_popup_sel(view).is_some() {
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

/// The cursor row of the knock popup while one is showing.
fn knock_popup_sel(view: &View) -> Option<usize> {
    let aux = view.aux.as_ref()?;
    aux.actions
        .iter()
        .any(|x| matches!(x, AuxAction::Share(Act::Admit(_))))
        .then_some(aux.popup.sel)
}

/// The popup for the oldest waiting knock, or no popup when none waits. The
/// cursor starts on deny: the popup can appear while the driver is typing
/// into a pane, and a stray Enter must never let a stranger in.
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
        PopupRow::Text("Enter denies. Up, then Enter, admits.".into()),
        PopupRow::Rule,
        entry("✓", "admit"),
        entry("✗", "deny"),
    ];
    let actions = vec![
        AuxAction::Share(Act::Admit(id)),
        AuxAction::Share(Act::Deny(id)),
    ];
    let mut popup = Popup::new(rows, Anchor::Center);
    popup.sel = 1;
    view.aux = Some(AuxPopup { popup, actions });
    view.aux_esc.clear();
}

/// Answer one knock through the child's stdin, then show the next.
async fn answer(view: &mut View, id: u64, command: char) {
    let sent = match view.share.as_mut() {
        Some(share) => {
            share.knocks.retain(|(k, _)| *k != id);
            share
                .stdin
                .write_all(format!("{command} {id}\n").as_bytes())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn child_lines_parse_and_menu_rows_pair_with_actions() {
        assert!(matches!(
            parse_line("knock 3 Ms. Lee"),
            Some(ShareMsg::Knock(3, n)) if n == "Ms. Lee"
        ));
        assert!(matches!(parse_line("gone 3"), Some(ShareMsg::Gone(3))));
        assert!(matches!(
            parse_line("link https://x.example/#k=1"),
            Some(ShareMsg::Link(l)) if l == "https://x.example/#k=1"
        ));
        assert!(parse_line("sharing pane 3").is_none());
        assert!(parse_line("knock x Ms. Lee").is_none());

        let (rows, acts) = menu_entries(false, None);
        assert!(rows.is_empty() && acts.is_empty(), "toggle off: no rows");
        let (rows, acts) = menu_entries(true, None);
        assert_eq!((rows.len(), acts), (1, vec![AuxAction::Share(Act::Start)]));
        let (rows, acts) = menu_entries(true, Some(2));
        assert_eq!(rows.len(), 3);
        assert_eq!(
            acts,
            vec![
                AuxAction::Share(Act::Knocks),
                AuxAction::Share(Act::Copy),
                AuxAction::Share(Act::Stop),
            ]
        );
    }
}
