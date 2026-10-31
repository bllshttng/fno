//! `fno mux serve --snapshot`: compose one mux frame through the real
//! [`View::compose`] and write it as html, svg or png, with no terminal,
//! shell, scrollback or cursor in the picture.
//!
//! `--server <name>` names the server. The shot goes through the read-only
//! observer attach the web bridge uses: `rows: 0, cols: 0` never resizes a PTY
//! and the write half is dropped after the attach. A public shot comes from
//! `scripts/ops/mux-demo-snapshot.sh`, which builds a throwaway server with
//! invented work and shoots it here, so no real session text can leak.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::{theme_import_ui, LayoutView, View};
use crate::frame_html::{self, Theme};
use crate::popup::Anchor;
use crate::proto::{self, ClientMsg, Frame, ServerMsg, BUILD_VERSION, PROTO_VERSION};
use serde_json::Value;

const USAGE: &str =
    "usage: fno mux serve --snapshot --server <name> --out <path> [--squad <name>] \
[--theme dark|light|macchiato] [--format html|svg|png] [--size <cols>x<rows> [--fit]] [--peer <cols>x<rows>] \
[--font <family>] [--message <fmail-id>] \
[--view bell|bell-wide|bell-full|feed|row-menu|hover|cursor|tab-menu|sideline-menu|composer|split-menu|settings|keys|org-block|org-table|org-recent|org-overview|org-graph|org-health|org-seat|org-feed|org-system|messages-tree|messages-chats|messages-thread|messages-system|messages-archive|backlog-list|backlog-kanban|navigator-pr|navigator-model|navigator-menu|feed-detail|question-detail|connections|connections-use|yard|needs]";

#[derive(Debug, PartialEq)]
pub enum Format {
    Html,
    Svg,
    Png,
}

/// A client-local surface a snapshot opens before composing. These live in
/// the client's own state, so the server frame never carries them; the seam
/// flips the same state the live keys flip, then paints.
#[derive(Debug, PartialEq)]
pub enum ViewKind {
    /// The notifications bell panel, with one synchronous gather like the
    /// live panel's `maybe_kick`.
    Bell,
    /// The bell panel wide (the `w` toggle), on the Questions tab with the
    /// first question marked, so one shot shows width, marks and the
    /// marked-count footer together.
    BellWide,
    /// The bell panel full screen (the `F` toggle), covering the panes.
    BellFull,
    /// The activity feed panel, with one synchronous page fold like the
    /// live panel's run-loop kick.
    Feed,
    /// The sideline row context menu on the first agent row.
    RowMenu,
    /// The pointer resting on the third agent row, away from the chosen one.
    Hover,
    /// The selector cursor on the fourth agent row, away from the chosen one.
    Cursor,
    /// The tab-strip context menu on the first tab.
    TabMenu,
    /// The sideline menu popup.
    SidelineMenu,
    /// The new-agent composer sheet, with a seeded draft so the editor text
    /// and the real terminal cursor's cell are in the picture.
    Composer,
    /// The row context menu on the first PANELESS bg row: the Split
    /// Direction group with its pane|portal toggle, the branch the first
    /// agent row (pane-hosted) never renders.
    SplitMenu,
    /// The settings modal at its top level: the tab strip the tab cycle
    /// walks.
    Settings,
    /// The which-key table, opened FROM settings: the printed key labels
    /// (`ctrl+b`) live here.
    Keys,
    /// The expanded org fold block on the sideline (the prefix+C surface),
    /// with one inline machine fold so the lines carry real readings.
    OrgBlock,
    /// The org view at one of its five tabs over the whole fleet.
    OrgTable,
    OrgRecent,
    OrgOverview,
    OrgGraph,
    OrgHealth,
    /// The org view's first seat page, its FEED page or its SYSTEM page.
    OrgSeat,
    OrgFeed,
    OrgSystem,
    /// The Messages board with focus on column 1, 2 or 3.
    MessagesTree,
    MessagesChats,
    MessagesThread,
    /// The Messages board with column 2's System or Archive tab active.
    MessagesSystem,
    MessagesArchive,
    /// The Messages board on the You tab: the operator's typed-turn threads.
    MessagesYou,
    /// The board on the newest You thread's detail, pairs right and left.
    MessagesYouThread,
    /// The backlog board full screen in list view, details pane beside it.
    BacklogList,
    /// The backlog board full screen in kanban view, details pane beside it.
    BacklogKanban,
    /// The prefix+f navigator filtered by the first agent row's PR number.
    NavigatorPr,
    /// The prefix+f navigator filtered by the model name `opus`.
    NavigatorModel,
    /// The prefix+f navigator with the sideline's row menu (Ctrl-o) open on
    /// the first agent row.
    NavigatorMenu,
    /// The event provenance modal on the first feed row that is not a
    /// question, with a mouse selection dragged across its longest line.
    FeedDetail,
    /// The question page on the first open question, with a mouse selection
    /// dragged across its longest line.
    QuestionDetail,
    /// The connections overlay (prefix+A) on its Accounts tab, with the
    /// reads the live open kicks run inline.
    Connections,
    /// The connections overlay with the `u` confirm staged on the first
    /// account that is not active.
    ConnectionsUse,
    /// The yard overlay on its first citizen.
    Yard,
    /// The needs-me overlay with its first row picked, after the same
    /// questions gather the bell shots run.
    Needs,
}

/// The seeded demo mail id a Messages view opens on when no --message
/// names one (scripts/ops/mux-demo-snapshot.sh writes it).
const DEMO_MESSAGE_ID: &str = "fmail-demo-2";

/// Open the navigator the way prefix+f does, with `query` typed. A query
/// that matches no row would shoot `no matches`, which proves nothing.
fn open_navigator(view: &mut View, query: String) -> Result<(), String> {
    let nav = super::NavView {
        query,
        state_filter: None,
        cursor: 0,
    };
    if view.nav_filtered(&nav).is_empty() {
        return Err(format!("navigator query {:?} matched no rows", nav.query));
    }
    view.nav = Some(nav);
    view.nav_esc.clear();
    Ok(())
}

/// Select the longest line of the open modal's body, the way a drag does:
/// compose once so the frame is recorded, then select. The caller's compose
/// paints the selection.
fn select_modal_line(view: &mut View) -> Result<(), String> {
    let _ = view.compose();
    crate::modal_select::select_longest_line()
        .map(|_| ())
        .ok_or_else(|| "the modal painted no body text to select".to_string())
}

fn parse_view(v: &str) -> Option<ViewKind> {
    match v {
        "bell" => Some(ViewKind::Bell),
        "bell-wide" => Some(ViewKind::BellWide),
        "bell-full" => Some(ViewKind::BellFull),
        "feed" => Some(ViewKind::Feed),
        "row-menu" => Some(ViewKind::RowMenu),
        "hover" => Some(ViewKind::Hover),
        "cursor" => Some(ViewKind::Cursor),
        "tab-menu" => Some(ViewKind::TabMenu),
        "sideline-menu" => Some(ViewKind::SidelineMenu),
        "composer" => Some(ViewKind::Composer),
        "split-menu" => Some(ViewKind::SplitMenu),
        "settings" => Some(ViewKind::Settings),
        "keys" => Some(ViewKind::Keys),
        "org-block" => Some(ViewKind::OrgBlock),
        "org-table" => Some(ViewKind::OrgTable),
        "org-recent" => Some(ViewKind::OrgRecent),
        "org-overview" => Some(ViewKind::OrgOverview),
        "org-graph" => Some(ViewKind::OrgGraph),
        "org-health" => Some(ViewKind::OrgHealth),
        "org-seat" => Some(ViewKind::OrgSeat),
        "org-feed" => Some(ViewKind::OrgFeed),
        "org-system" => Some(ViewKind::OrgSystem),
        "messages-tree" => Some(ViewKind::MessagesTree),
        "messages-chats" => Some(ViewKind::MessagesChats),
        "messages-thread" => Some(ViewKind::MessagesThread),
        "messages-system" => Some(ViewKind::MessagesSystem),
        "messages-archive" => Some(ViewKind::MessagesArchive),
        "messages-you" => Some(ViewKind::MessagesYou),
        "messages-you-thread" => Some(ViewKind::MessagesYouThread),
        "backlog-list" => Some(ViewKind::BacklogList),
        "backlog-kanban" => Some(ViewKind::BacklogKanban),
        "navigator-pr" => Some(ViewKind::NavigatorPr),
        "navigator-model" => Some(ViewKind::NavigatorModel),
        "navigator-menu" => Some(ViewKind::NavigatorMenu),
        "feed-detail" => Some(ViewKind::FeedDetail),
        "question-detail" => Some(ViewKind::QuestionDetail),
        "connections" => Some(ViewKind::Connections),
        "connections-use" => Some(ViewKind::ConnectionsUse),
        "yard" => Some(ViewKind::Yard),
        "needs" => Some(ViewKind::Needs),
        _ => None,
    }
}

#[derive(Debug)]
pub struct SnapshotArgs {
    pub out: PathBuf,
    pub server: String,
    pub squad: Option<String>,
    pub theme: Theme,
    pub format: Format,
    /// `(rows, cols)` of the whole picture; live mode defaults to the server's.
    pub size: Option<(u16, u16)>,
    /// Attach as a sizing client, so the server lays its panes out at `size`.
    /// It resizes every pane, so it is for a throwaway server only.
    pub fit: bool,
    /// `(rows, cols)` of a second window held attached as a sizing client
    /// while the shot is taken, so the shot sees a tab two windows share.
    pub peer: Option<(u16, u16)>,
    /// The font family drawn first, ahead of the common monospace stack: an
    /// image has no terminal, so its font is named here or left to the stack.
    pub font: Option<String>,
    /// Open the Messages tab on this fmail id: the paint gate's shots of a
    /// client-local board (law d-9f18c4d5). The board gathers via a client
    /// subprocess, so the snapshot runs one gather synchronously; the id
    /// resolves to its thread and positions the cursor there.
    pub message: Option<String>,
    /// The client-local surface to open before composing, for shots of a
    /// bell panel or a menu no server frame ever carries.
    pub view: Option<ViewKind>,
}

/// True when a `serve` tail asks for a snapshot rather than the web bridge.
pub fn wants_snapshot(tail: &[OsString]) -> bool {
    tail.iter().any(|a| a == "--snapshot")
}

/// Parse the `serve --snapshot` tail. `Err` is the usage line (exit 2).
pub fn parse(tail: &[OsString]) -> Result<SnapshotArgs, String> {
    let mut out = None;
    let mut server = None;
    let mut squad = None;
    let mut theme = frame_html::theme_by_name("dark").expect("dark is a snapshot theme");
    let mut format = None;
    let mut size = None;
    let mut fit = false;
    let mut peer = None;
    let mut font = None;
    let mut message = None;
    let mut view = None;
    let mut it = tail.iter();
    while let Some(a) = it.next() {
        let a = a.to_str().ok_or_else(|| USAGE.to_string())?;
        let mut value = || -> Result<String, String> {
            it.next()
                .and_then(|v| v.to_str())
                .map(str::to_string)
                .ok_or_else(|| format!("fno mux serve --snapshot: {a} needs a value\n{USAGE}"))
        };
        match a {
            "--snapshot" => {}
            "--fit" => fit = true,
            "--font" => {
                let f = value()?;
                if f.is_empty() || f.contains(['\'', '"', '<', '>', '&']) {
                    return Err(format!(
                        "fno mux serve --snapshot: bad --font {f:?}\n{USAGE}"
                    ));
                }
                font = Some(f);
            }
            "--out" => out = Some(PathBuf::from(value()?)),
            "--message" => message = Some(value()?),
            "--view" => {
                let v = value()?;
                view = Some(parse_view(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: unknown view {v:?}; use bell, bell-wide, bell-full, feed, row-menu, tab-menu, sideline-menu, composer, split-menu, settings, keys, org-block, org-table, org-recent, org-overview, org-graph, org-health, org-seat, org-feed, org-system, messages-tree, messages-chats, messages-thread, messages-system, messages-archive, messages-you, messages-you-thread, backlog-list, backlog-kanban, navigator-pr, navigator-menu, navigator-model, feed-detail, question-detail, connections-use or connections")
                })?)
            }
            tok @ ("--server" | "--session") => {
                crate::mux_cli::note_server_flag(tok);
                server = Some(value()?)
            }
            "--squad" => squad = Some(value()?),
            "--theme" => {
                let v = value()?;
                theme = frame_html::theme_by_name(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: unknown theme {v:?}; use dark, light or macchiato")
                })?
            }
            "--format" => {
                format = Some(match value()?.as_str() {
                    "html" => Format::Html,
                    "svg" => Format::Svg,
                    "png" => Format::Png,
                    v => {
                        return Err(format!(
                            "fno mux serve --snapshot: unknown format {v:?}; use html, svg or png"
                        ))
                    }
                })
            }
            "--peer" => {
                let v = value()?;
                peer = Some(parse_size(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: --peer wants <cols>x<rows>, got {v:?}")
                })?)
            }
            "--size" => {
                let v = value()?;
                size = Some(parse_size(&v).ok_or_else(|| {
                    format!("fno mux serve --snapshot: --size wants <cols>x<rows>, got {v:?}")
                })?)
            }
            other => {
                return Err(format!(
                    "fno mux serve --snapshot: unknown flag {other:?}\n{USAGE}"
                ))
            }
        }
    }
    let out = out.ok_or_else(|| format!("fno mux serve --snapshot: --out is required\n{USAGE}"))?;
    let server = server.ok_or_else(|| {
        "fno mux serve --snapshot: --server is required; for a public shot with no real \
session text, run scripts/ops/mux-demo-snapshot.sh"
            .to_string()
    })?;
    if fit && size.is_none() {
        return Err(format!(
            "fno mux serve --snapshot: --fit needs --size\n{USAGE}"
        ));
    }
    // The extension names the format when the flag does not.
    let format = match format {
        Some(f) => f,
        None => match out.extension().and_then(|e| e.to_str()) {
            Some("html") => Format::Html,
            Some("png") => Format::Png,
            _ => Format::Svg,
        },
    };
    Ok(SnapshotArgs {
        out,
        server,
        squad,
        theme,
        format,
        size,
        fit,
        peer,
        font,
        message,
        view,
    })
}

fn parse_size(v: &str) -> Option<(u16, u16)> {
    let (c, r) = v.split_once('x')?;
    let (cols, rows) = (c.parse::<u16>().ok()?, r.parse::<u16>().ok()?);
    (cols >= 40 && rows >= 10).then_some((rows, cols))
}

pub fn run(args: SnapshotArgs) -> i32 {
    // The chrome paints the mux theme a live client in this ground would run,
    // so its chips and badges carry that theme's own pairs.
    let chrome = match args.theme.name {
        "dark" => crate::theme::theme_footnote_superscript(),
        "light" => crate::theme::theme_footnote_paper(),
        _ => crate::theme::Theme::default_theme(),
    };
    let frame = live_frame(
        &args.server,
        args.squad.as_deref(),
        args.size,
        args.fit,
        args.peer,
        chrome,
        args.message.as_deref(),
        args.view.as_ref(),
    );
    match frame.and_then(|f| write(&f, &args)) {
        Ok(()) => {
            println!("{}", args.out.display());
            0
        }
        Err(e) => {
            eprintln!("fno mux serve --snapshot: {e}");
            1
        }
    }
}

fn write(frame: &Frame, args: &SnapshotArgs) -> Result<(), String> {
    let font = |page: String| match &args.font {
        Some(f) => page.replace("'SF Mono',", &format!("'{f}','SF Mono',")),
        None => page,
    };
    let svg = || font(frame_html::frame_svg(frame, args.theme));
    let io = |e: std::io::Error| format!("cannot write {}: {e}", args.out.display());
    match args.format {
        Format::Html => {
            std::fs::write(&args.out, font(frame_html::screen_html(frame, args.theme))).map_err(io)
        }
        Format::Svg => std::fs::write(&args.out, svg()).map_err(io),
        Format::Png => {
            png(&svg(), frame, &args.out)?;
            if super::image_layer::painted() {
                stamp_no_image_protocol(&args.out)?;
            }
            Ok(())
        }
    }
}

/// The `tEXt` chunk a snapshot png carries when its frame painted an image.
/// The snapshot speaks no image protocol, so such a shot shows the half-block
/// path only. The question intake refuses on it (`question_intake.rs`).
const NO_IMAGE_PROTOCOL_STAMP: (&str, &str) = ("fno-snapshot", "no-image-protocol");

/// Insert the stamp as a `tEXt` chunk right after the png's `IHDR`.
fn stamp_no_image_protocol(out: &Path) -> Result<(), String> {
    let io = |e: std::io::Error| format!("stamp {}: {e}", out.display());
    let mut png = std::fs::read(out).map_err(io)?;
    // Signature (8) + IHDR: length (4), type (4), data (13), crc (4).
    let at = 33;
    if png.len() < at || &png[12..16] != b"IHDR" {
        return Err(format!(
            "stamp {}: not a png with IHDR first",
            out.display()
        ));
    }
    let (key, text) = NO_IMAGE_PROTOCOL_STAMP;
    let mut body = b"tEXt".to_vec();
    body.extend_from_slice(key.as_bytes());
    body.push(0);
    body.extend_from_slice(text.as_bytes());
    let mut chunk = ((body.len() - 4) as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(&body);
    chunk.extend_from_slice(&crc32(&body).to_be_bytes());
    png.splice(at..at, chunk);
    std::fs::write(out, png).map_err(io)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & (crc & 1).wrapping_neg());
        }
    }
    !crc
}

/// Rasterize through headless Chrome: the svg in a zero-margin page, a window
/// the exact size of the image, at 2x.
fn png(svg: &str, frame: &Frame, out: &Path) -> Result<(), String> {
    let chrome = find_chrome()
        .ok_or("png needs Chrome or Chromium: set FNO_CHROME to its binary, or use --format svg")?;
    let w = (frame.cols as f64 * frame_html::SVG_CELL_W).ceil() as u32;
    let h = (frame.rows as f64 * frame_html::SVG_CELL_H).ceil() as u32;
    let dir = std::env::temp_dir().join(format!("fno-snapshot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;
    let page = dir.join("shot.html");
    std::fs::write(&page, page_html(svg)).map_err(|e| format!("temp page: {e}"))?;
    let out_abs = std::path::absolute(out).map_err(|e| format!("{}: {e}", out.display()))?;
    // The png on disk is the success signal below, so an earlier shot at the
    // same path would read as this one landing and pass off the stale image.
    match std::fs::remove_file(&out_abs) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(format!("{}: {e}", out_abs.display()));
        }
        _ => {}
    }
    let mut cmd = std::process::Command::new(&chrome);
    // Chrome hangs on macOS under a HOME that is not the user's, and the demo
    // script sets one so paths read `~/...`. Hand it the passwd home back.
    if let Some(home) = crate::live_store_fence::passwd_home() {
        cmd.env("HOME", home);
    }
    let mut child = cmd
        .arg("--headless=new")
        // Without its own profile dir a headless shot exits 2 whenever the
        // user's own Chrome is running, which is most of the time.
        .arg(format!("--user-data-dir={}", dir.join("profile").display()))
        .arg("--disable-gpu")
        .arg("--hide-scrollbars")
        .arg("--force-device-scale-factor=2")
        .arg(format!("--window-size={w},{h}"))
        .arg(format!("--screenshot={}", out_abs.display()))
        .arg(format!("file://{}", page.display()))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", chrome.display()))?;
    // Headless chrome can write the screenshot and then hang forever under
    // load, wedging the whole shot run. The png on disk is the success
    // signal: a short grace after it lands, then kill. A render with no png
    // gets a hard two-minute bound.
    let hard = Instant::now() + Duration::from_secs(120);
    let mut grace: Option<Instant> = None;
    let status;
    loop {
        if out_abs.exists() && grace.is_none() {
            grace = Some(Instant::now() + Duration::from_secs(5));
        }
        let bound = grace.map_or(hard, |g| g.min(hard));
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s);
                break;
            }
            Ok(None) if Instant::now() >= bound => {
                let png_landed = out_abs.exists();
                let _ = child.kill();
                let _ = child.wait();
                let _ = std::fs::remove_dir_all(&dir);
                if png_landed {
                    return Ok(());
                }
                return Err(format!(
                    "{} hung past 120s and wrote no png",
                    chrome.display()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(250)),
            Err(e) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(format!("cannot wait for {}: {e}", chrome.display()));
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    match status.expect("loop breaks on exit, bound, or a wait error") {
        s if s.success() && out_abs.exists() => Ok(()),
        s => Err(format!("{} exited {s} and wrote no png", chrome.display())),
    }
}

/// The zero-margin page png() rasterizes. overflow:hidden: the last row's
/// glyph descenders poke a fraction of a px past the svg height, and an
/// exact-fit window then screenshots a composite whose bottom tile repeats
/// the page's first rows (the doubled chrome the iMac reported). Clamping
/// the page kills the repeat.
fn page_html(svg: &str) -> String {
    format!(
        "<!doctype html><style>html,body{{margin:0;overflow:hidden}}svg{{display:block}}</style>{svg}"
    )
}

fn find_chrome() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("FNO_CHROME").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let mac = Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
    if mac.exists() {
        return Some(mac.to_path_buf());
    }
    let path = std::env::var_os("PATH")?;
    ["google-chrome", "chromium", "chromium-browser"]
        .iter()
        .flat_map(|name| std::env::split_paths(&path).map(move |d| d.join(name)))
        .find(|p| p.is_file())
}

/// Everything one observer attach delivers before it goes quiet.
struct Observed {
    layout: LayoutView,
    frames: HashMap<u64, Frame>,
}

fn live_frame(
    server: &str,
    squad: Option<&str>,
    size: Option<(u16, u16)>,
    fit: bool,
    peer: Option<(u16, u16)>,
    chrome: crate::theme::Theme,
    message: Option<&str>,
    kind: Option<&ViewKind>,
) -> Result<Frame, String> {
    let socket = proto::socket_path(server)?;
    let runtime = tokio::runtime::Runtime::new().map_err(|e| format!("runtime: {e}"))?;
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A sizing client reports the content area, as a real client does.
    let dims = size
        .filter(|_| fit)
        .map(|t| View::new(t, server.into(), LayoutView::default()).content_dims());
    // The peer attaches first and stays until the shot is composed.
    let _peer = peer
        .map(|t| View::new(t, server.into(), LayoutView::default()).content_dims())
        .map(|d| runtime.block_on(hold_peer(&socket, cwd.clone(), d)))
        .transpose()?;
    let mut seen = runtime.block_on(observe(&socket, cwd.clone(), dims))?;
    if let Some(name) = squad {
        let target = seen
            .layout
            .squads
            .iter()
            .find(|s| s.name == name)
            .ok_or_else(|| {
                let names: Vec<&str> = seen.layout.squads.iter().map(|s| s.name.as_str()).collect();
                format!(
                    "server {server:?} has no squad {name:?}; it has {}",
                    names.join(", ")
                )
            })?;
        if target.id != seen.layout.active_squad {
            // The server picks the squad from the attach cwd.
            let cwd = target.canonical_cwd.clone();
            seen = runtime.block_on(observe(&socket, cwd, dims))?;
        }
    }
    let area = seen.layout.area;
    let mut view = View::new(area, server.into(), seen.layout);
    view.theme = chrome;
    // The snapshot paints what a live client in this cwd would, so the
    // card-graph config latches here the way attach_and_run does.
    view.card_graph = crate::digest_overlay::card_graph(Path::new(&cwd));
    view.frames = seen.frames;
    view.term = match size {
        Some(t) => t,
        None => {
            // Grow the picture by exactly the chrome, so the content area
            // matches the rects the server computed. Twice, because the
            // sideline width depends on the terminal width.
            for _ in 0..2 {
                let (cr, cc) = view.content_dims();
                view.term = (
                    view.term.0 + area.0.saturating_sub(cr),
                    view.term.1 + area.1.saturating_sub(cc),
                );
            }
            view.term
        }
    };
    runtime.block_on(super::backlog_board::fold_once(&mut view));
    // The Messages board gathers via a client-side subprocess and the
    // snapshot has no event loop, so the tab would paint empty. --message
    // opens the board on that fmail id and runs one gather synchronously;
    // the paint gate's shots come from here (law d-9f18c4d5).
    use crate::client::messages_view::{ChatTab, Col};
    // A Messages view: the column that holds focus and column 2's tab.
    let messages_shot = match kind {
        Some(ViewKind::MessagesTree) => Some((Col::Tree, ChatTab::Chats)),
        Some(ViewKind::MessagesChats) => Some((Col::Chats, ChatTab::Chats)),
        Some(ViewKind::MessagesThread) => Some((Col::Thread, ChatTab::Chats)),
        Some(ViewKind::MessagesSystem) => Some((Col::Chats, ChatTab::System)),
        Some(ViewKind::MessagesArchive) => Some((Col::Chats, ChatTab::Archive)),
        Some(ViewKind::MessagesYou) => Some((Col::Chats, ChatTab::Chats)),
        Some(ViewKind::MessagesYouThread) => Some((Col::Thread, ChatTab::Chats)),
        _ => None,
    };
    let message = message.or(messages_shot.map(|_| DEMO_MESSAGE_ID));
    if let Some(id) = message {
        crate::client::messages_view::open_message(&mut view, id.to_string());
        let gen = view
            .messages_board
            .as_ref()
            .map(|b| b.gen)
            .unwrap_or_default();
        let mail = runtime.block_on(crate::messages_model::gather());
        // A board with no threads paints a pane of chrome around nothing -
        // exactly the shot law d-9f18c4d5 exists to stop. Read the gather
        // before apply moves it, and refuse.
        let board_empty = mail
            .as_ref()
            .ok()
            .and_then(|p| p.get("threads"))
            .and_then(Value::as_array)
            .is_none_or(|t| t.is_empty());
        // The You views refuse the same way: a gather with no `you` rows
        // paints "(no You threads)" and proves nothing (law d-9f18c4d5).
        let you_empty = matches!(
            kind,
            Some(ViewKind::MessagesYou | ViewKind::MessagesYouThread)
        ) && mail
            .as_ref()
            .ok()
            .and_then(|p| p.get("you"))
            .and_then(Value::as_array)
            .is_none_or(|t| t.is_empty());
        crate::client::messages_view::apply_gather(&mut view, gen, mail, Err("no org tree".into()));
        if board_empty || you_empty {
            return Err(
                "messages board rendered empty: mail-threads returned no threads; \
shoot a root whose chats store has rows (scripts/ops/mux-demo-snapshot.sh seeds one)"
                    .into(),
            );
        }
        if let (Some((col, tab)), Some(b)) = (messages_shot, view.messages_board.as_mut()) {
            b.col = col;
            b.col2 = tab;
        }
        // The You views select the first agent with a Me row (the user's own
        // thread one step down); the thread view then opens that Me thread.
        let you_view = matches!(
            kind,
            Some(ViewKind::MessagesYou | ViewKind::MessagesYouThread)
        );
        if let (true, Some(b)) = (you_view, view.messages_board.as_mut()) {
            let rows = b.tree_rows();
            let pick = rows.iter().enumerate().find_map(|(i, r)| match r {
                crate::client::messages_view::TreeRow::Agent { key, .. }
                    if b.chat_rows(key).iter().any(|c| {
                        matches!(c, crate::client::messages_view::ChatRow::YouThread { .. })
                    }) =>
                {
                    Some((i, key.clone()))
                }
                _ => None,
            });
            if let Some((i, key)) = pick {
                b.cursors[0] = i;
                b.sel_agent = Some(key);
            }
        }
        if let (Some(ViewKind::MessagesYouThread), Some(b)) = (kind, view.messages_board.as_mut()) {
            if let Some(sid) = b.active_chat_rows().into_iter().find_map(|row| match row {
                crate::client::messages_view::ChatRow::YouThread { session_id, .. } => {
                    Some(session_id)
                }
                _ => None,
            }) {
                b.sel_thread = Some(format!("you:{sid}"));
                b.cursors[2] = b.you_items(&sid).len().saturating_sub(1);
            }
        }
    }
    // The messages seam above is the shape: flip the client state the live
    // keys flip, run one synchronous gather where the live panel would kick
    // one, then compose. Menus build from the layout the server sent, so
    // they need no gather.
    match kind {
        Some(ViewKind::Bell) | Some(ViewKind::BellWide) | Some(ViewKind::BellFull) => {
            let gen = crate::client::bell::open(&mut view);
            let projection = runtime.block_on(crate::messages_model::gather());
            // Same refusal as the messages board: an announcements-less
            // gather paints the bell as "nothing here", a shot with nothing
            // to judge (law d-9f18c4d5).
            let bell_empty = projection
                .as_ref()
                .ok()
                .and_then(|p| p.get("announcements"))
                .and_then(Value::as_array)
                .is_none_or(|a| a.is_empty());
            crate::client::bell::apply(&mut view, gen, projection);
            if bell_empty {
                return Err(
                    "bell view rendered empty: the messages projection has no announcements; \
shoot a root whose chats store has rows (scripts/ops/mux-demo-snapshot.sh seeds one)"
                        .into(),
                );
            }
            // The Questions tab reads the fold the live loop kicks, and the
            // snapshot has no loop: without this gather the tab paints its
            // loading skeleton and the wide shot has no row to mark.
            let questions = runtime.block_on(crate::needs_overlay::questions_now());
            view.apply_questions_fold(questions);
            match kind {
                Some(ViewKind::BellWide) => {
                    view.bell.size = super::bell::Size::Wide;
                    view.bell.tab = super::bell::Tab::Questions;
                    view.bell.selected = 0;
                    if let Some(id) = crate::client::bell::first_question_id(&view) {
                        view.bell.marks.insert(id);
                    }
                }
                Some(ViewKind::BellFull) => {
                    view.bell.size = super::bell::Size::Full;
                }
                _ => {}
            }
        }
        Some(ViewKind::Feed) | Some(ViewKind::FeedDetail) => {
            // The `e` key's state flip plus the fold the live panel's run
            // loop kicks, run inline: one Head page lands, newest first.
            view.feed = Some(super::feed_view::open_overlay(None, 0));
            if let Some(f) = view.feed.as_mut() {
                f.want = true;
            }
            let outcome = runtime.block_on(crate::feed_overlay::fetch_page(
                crate::feed_overlay::PageReq::Head,
                None,
            ));
            super::feed_view::apply_fold(&mut view, 0, outcome);
            // A fresh root has no feed rows, so the changed view rendered
            // with nothing to judge. Refuse the shot.
            if view.feed.as_ref().is_none_or(|f| f.win.items.is_empty()) {
                return Err("feed view rendered empty: this root has no feed rows; \
shoot a root seeded with questions, mail or backlog activity \
(scripts/ops/mux-demo-snapshot.sh seeds one)"
                    .into());
            }
            if let Some(ViewKind::FeedDetail) = kind {
                // A question row opens the question page, so the provenance
                // modal opens on the first row that is not one.
                let item = view
                    .feed
                    .as_ref()
                    .and_then(|f| f.win.items.iter().find(|i| i.kind != "question_asked"))
                    .cloned()
                    .ok_or("feed-detail: the feed holds only question rows to open")?;
                view.feed_detail = Some(super::feed_detail::modal(&view, item));
                select_modal_line(&mut view)?;
            }
        }
        Some(ViewKind::QuestionDetail) => {
            // The same gather the bell shots run, then the page the bell
            // opens on a row.
            let questions = runtime.block_on(crate::needs_overlay::questions_now());
            view.apply_questions_fold(questions);
            let id = super::bell::first_question_id(&view).ok_or(
                "question-detail: no open question to show; shoot a root seeded with \
questions (scripts/ops/mux-demo-snapshot.sh seeds one)",
            )?;
            view.open_detail_on(&id);
            if let Some(d) = &view.question_detail {
                d.wait_decoded(Duration::from_secs(10));
            }
            select_modal_line(&mut view)?;
        }
        Some(ViewKind::RowMenu) => {
            let i = view
                .display_rows()
                .iter()
                .position(|r| matches!(r, super::DisplayRow::Agent(_)))
                .ok_or("no agent rows to open a row menu on")?;
            view.open_row_menu(i, Anchor::Center);
        }
        Some(kind @ (ViewKind::Hover | ViewKind::Cursor)) => {
            let agents = view.display_rows().into_iter().enumerate();
            let mut agents = agents.filter(|(_, r)| matches!(r, super::DisplayRow::Agent(_)));
            let hover = matches!(kind, ViewKind::Hover);
            let i = agents.nth(if hover { 2 } else { 3 }).map(|(i, _)| i);
            if hover {
                view.hover_row = i;
            } else {
                view.selector = i;
            }
        }
        Some(ViewKind::SplitMenu) => {
            let i = view
                .display_rows()
                .iter()
                .position(|r| {
                    matches!(r, super::DisplayRow::Agent(a)
                        if a.pane_id.is_none() && a.attach_id.is_some() && !a.exited)
                })
                .ok_or("no paneless bg rows to open a split menu on")?;
            view.open_row_menu(i, Anchor::Center);
        }
        Some(ViewKind::Settings) => {
            view.lane.reset();
            theme_import_ui::reset(&mut view);
            view.aux = Some(view.build_settings_modal());
            view.aux_esc.clear();
        }
        Some(ViewKind::Keys) => {
            view.open_keys_modal();
        }
        Some(ViewKind::Yard) => {
            view.yard = Some(super::YardSel {
                sel: 0,
                opened_at: Instant::now(),
            });
        }
        Some(ViewKind::Needs) => {
            let questions = runtime.block_on(crate::needs_overlay::questions_now());
            view.apply_questions_fold(questions);
            view.answers = Some(0);
        }
        Some(kind @ (ViewKind::Connections | ViewKind::ConnectionsUse)) => {
            view.open_connections();
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
            runtime.block_on(crate::connections_view::load_all(tx, view.conn_gen));
            let cv = view
                .connections
                .as_mut()
                .ok_or("connections overlay did not open")?;
            // A throwaway root reaches no real account, so the live probe and
            // identity legs only fail. Paint the cached reading the root seeds.
            let mut cached = true;
            while let Ok((_, outcome)) = rx.try_recv() {
                use crate::connections_view::ReadOutcome as R;
                match outcome {
                    R::UsageFailed(_) | R::IdentityFailed(_) => {}
                    R::Usage(_) if !cached => {}
                    R::Usage(cells) => {
                        cached = false;
                        cv.apply_read(R::Usage(cells));
                    }
                    other => cv.apply_read(other),
                }
            }
            if cv.accounts.is_empty() {
                return Err(
                    "connections view rendered empty: this root registers no accounts; \
shoot a root whose config lists accounts (scripts/ops/mux-demo-snapshot.sh seeds them)"
                        .into(),
                );
            }
            if *kind == ViewKind::ConnectionsUse {
                cv.acct_sel = cv.accounts.iter().position(|a| !a.active).unwrap_or(0);
                cv.render_with(&view.layout.agents, usize::MAX);
                cv.on_key(b'u');
            }
        }
        Some(ViewKind::OrgBlock) => {
            // The prefix+C fold runs expanded-only in a reading: flip the
            // same state the key flips, then run the fold and the gate
            // inline where the live panel would spawn them. A fold with no
            // reading paints unknown lines - nothing to judge.
            view.org = crate::org_overlay::Panel::with_detail(true);
            view.org.toggle();
            let lanes = runtime.block_on(crate::org_overlay::fold_now());
            let read = lanes.is_some();
            view.org.apply(crate::org_overlay::FoldPart::Lanes(lanes));
            let gate = runtime.block_on(crate::org_overlay::gate_now());
            view.org.apply(crate::org_overlay::FoldPart::Gate(gate));
            if !read {
                return Err(
                    "org block rendered without a reading: fno doctor lanes returned none; \
shoot from a host where the lanes command answers"
                        .into(),
                );
            }
        }
        Some(
            ViewKind::OrgTable
            | ViewKind::OrgRecent
            | ViewKind::OrgOverview
            | ViewKind::OrgGraph
            | ViewKind::OrgHealth
            | ViewKind::OrgSeat
            | ViewKind::OrgFeed
            | ViewKind::OrgSystem,
        ) => {
            // The org view at one tab or page: open it the way the key
            // does, run one synchronous fleet read, then select what the
            // shot names. A fleet map with no seat refuses the shot.
            view.experimental_org_board = true;
            super::backlog_board::toggle_org_view(&mut view);
            view.board_full = true;
            let inputs = runtime.block_on(super::org_seats::gather(
                &crate::backlog_view::graph_path(),
                view.layout.agents.clone(),
            ));
            let gen = view.org_generation;
            super::org_board::apply_fold(&mut view, gen, inputs);
            let questions = runtime.block_on(crate::needs_overlay::questions_now());
            view.apply_questions_fold(questions);
            let Some(b) = view.org_board.as_mut() else {
                return Err("org view did not open".into());
            };
            let first = b.fleet.as_ref().and_then(|f| {
                f.machines.iter().find_map(|m| {
                    m.leads
                        .iter()
                        .find_map(|p| p.seats.first().map(|s| super::org_board::seat_key(m, p, s)))
                })
            });
            let Some(first) = first else {
                return Err("org view rendered empty: fleet top answered no seat; \
shoot a root whose fleet map has seats \
(scripts/ops/mux-demo-snapshot.sh seeds one)"
                    .into());
            };
            b.tab = match kind {
                Some(ViewKind::OrgRecent) => crate::view_store::OrgTab::Recent,
                Some(ViewKind::OrgOverview) => crate::view_store::OrgTab::Overview,
                Some(ViewKind::OrgGraph) => crate::view_store::OrgTab::Graph,
                Some(ViewKind::OrgHealth) => crate::view_store::OrgTab::Health,
                _ => crate::view_store::OrgTab::Table,
            };
            match kind {
                Some(ViewKind::OrgSeat) => b.select(&first),
                Some(ViewKind::OrgFeed) => b.select("feed"),
                Some(ViewKind::OrgSystem) => b.select("system"),
                _ => {}
            }
        }
        Some(ViewKind::BacklogList) | Some(ViewKind::BacklogKanban) => {
            // The V-cycle backlog board, full screen so the board and
            // details panes lay out side by side. Set the sideline field
            // directly: the shot never writes the view prefs.
            view.sideline_view = crate::view_store::SidelineView::Backlog;
            runtime.block_on(super::backlog_board::fold_once(&mut view));
            view.board_full = true;
            let b = view
                .backlog_board
                .as_mut()
                .ok_or("the backlog board did not open")?;
            b.query.view = match kind {
                Some(ViewKind::BacklogList) => crate::backlog_model::View::List,
                _ => crate::backlog_model::View::Kanban,
            };
            if !b.body.as_ref().is_some_and(|body| !body.lanes.is_empty()) {
                return Err(
                    "backlog view rendered empty: this root has no backlog nodes; \
shoot a root seeded with nodes (scripts/ops/mux-demo-snapshot.sh seeds one)"
                        .into(),
                );
            }
        }
        Some(ViewKind::TabMenu) => {
            let tid = view
                .layout
                .squads
                .iter()
                .flat_map(|s| s.tabs.iter())
                .next()
                .map(|t| t.id)
                .ok_or("no tabs to open a tab menu on")?;
            view.open_tab_menu_by_id(tid, Anchor::Center);
        }
        Some(ViewKind::NavigatorPr) => {
            let pr = view.layout.agents.iter().find_map(|a| a.pr).ok_or(
                "no agent row carries a PR; shoot a root whose claimed node has a \
pr_number (scripts/ops/mux-demo-snapshot.sh seeds one)",
            )?;
            open_navigator(&mut view, pr.to_string())?;
        }
        Some(ViewKind::NavigatorModel) => open_navigator(&mut view, "opus".into())?,
        Some(ViewKind::NavigatorMenu) => {
            open_navigator(&mut view, String::new())?;
            let at = view
                .nav_filtered(view.nav.as_ref().ok_or("navigator did not open")?)
                .iter()
                .position(|r| matches!(r.menu, super::row_menu::NavMenu::Agent(_)))
                .ok_or("no agent rows to open a navigator menu on")?;
            if let Some(n) = view.nav.as_mut() {
                n.cursor = at;
            }
            view.nav_open_menu();
        }
        Some(ViewKind::SidelineMenu) => view.open_sideline_menu(Anchor::Center),
        Some(ViewKind::Composer) => {
            super::agent_launcher::open(&mut view);
            if let Some(l) = view.launcher.as_mut() {
                l.draft.message = "fix the mux composer cursor and paste".to_string();
                l.draft.cursor_chars = "fix the mux composer ".chars().count();
            }
        }
        // The messages seam above already opened and positioned the board.
        Some(ViewKind::MessagesTree)
        | Some(ViewKind::MessagesChats)
        | Some(ViewKind::MessagesThread)
        | Some(ViewKind::MessagesSystem)
        | Some(ViewKind::MessagesArchive)
        | Some(ViewKind::MessagesYou)
        | Some(ViewKind::MessagesYouThread)
        | None => {}
    }
    crate::lattice::freeze_spin();
    Ok(view.compose())
}

/// Attach a second sizing client at `dims` and keep it attached until the
/// returned half drops. Its stream is drained so the server never stalls on it.
async fn hold_peer(
    socket: &Path,
    cwd: String,
    dims: (u16, u16),
) -> Result<tokio::net::unix::OwnedWriteHalf, String> {
    let stream = tokio::net::UnixStream::connect(socket)
        .await
        .map_err(|e| format!("peer cannot connect to {}: {e}", socket.display()))?;
    let (mut reader, mut writer) = stream.into_split();
    let attach = ClientMsg::Attach {
        proto: PROTO_VERSION,
        build: BUILD_VERSION.to_string(),
        rows: dims.0,
        cols: dims.1,
        cwd,
    };
    proto::write_msg(&mut writer, &attach)
        .await
        .map_err(|e| format!("peer attach write failed: {e}"))?;
    loop {
        let read = proto::read_msg::<_, ServerMsg>(&mut reader);
        match tokio::time::timeout(Duration::from_secs(10), read).await {
            Ok(Ok(ServerMsg::Layout { .. })) => break,
            Ok(Ok(ServerMsg::Bye { reason })) => {
                return Err(format!("server refused the peer attach: {reason}"))
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(format!("peer read failed: {e}")),
            Err(_) => return Err("peer saw no layout within 10s".into()),
        }
    }
    tokio::spawn(
        async move { while proto::read_msg::<_, ServerMsg>(&mut reader).await.is_ok() {} },
    );
    Ok(writer)
}

/// `dims` set attaches as a sizing client at that content area and reads
/// for a settle window, so panes can redraw at the new size.
async fn observe(socket: &Path, cwd: String, dims: Option<(u16, u16)>) -> Result<Observed, String> {
    let stream = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::net::UnixStream::connect(socket),
    )
    .await
    .map_err(|_| format!("connect to {} timed out", socket.display()))?
    .map_err(|e| {
        format!(
            "cannot connect to {}: {e}; list servers with `fno mux ls`",
            socket.display()
        )
    })?;
    let (mut reader, mut writer) = stream.into_split();
    proto::write_msg(
        &mut writer,
        &ClientMsg::Attach {
            proto: PROTO_VERSION,
            build: BUILD_VERSION.to_string(),
            // (0, 0) is the observer sentinel: it never resizes a PTY.
            rows: dims.map_or(0, |d| d.0),
            cols: dims.map_or(0, |d| d.1),
            cwd,
        },
    )
    .await
    .map_err(|e| format!("attach write failed: {e}"))?;
    // Read-only from here: release the write half without the half-close
    // that would make the server detach us.
    writer.forget();

    let mut layout = None;
    let mut frames = HashMap::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let settle = dims.map(|_| Instant::now() + Duration::from_millis(2500));
    loop {
        // Once the layout is in, wait only briefly for its panes' frames.
        let wait = match (&layout, settle) {
            (Some(_), Some(t)) => t.saturating_duration_since(Instant::now()),
            (None, _) => deadline.saturating_duration_since(Instant::now()),
            (Some(_), None) => Duration::from_millis(400),
        };
        let msg =
            match tokio::time::timeout(wait, proto::read_msg::<_, ServerMsg>(&mut reader)).await {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => return Err(format!("read failed: {e}")),
                Err(_) if layout.is_some() => break,
                Err(_) => return Err("server sent no layout within 10s".into()),
            };
        match msg {
            ServerMsg::Layout {
                squads,
                active_squad,
                panes,
                focus,
                area,
                agents,
                focus_node,
                ..
            } => {
                layout = Some(LayoutView {
                    squads,
                    active_squad,
                    panes,
                    focus,
                    area,
                    agents,
                    focus_node,
                })
            }
            ServerMsg::Frame { pane_id, frame } if frame.geometry_ok() => {
                frames.insert(pane_id, frame);
            }
            ServerMsg::Bye { reason } => {
                return Err(format!("server refused the attach: {reason}"))
            }
            _ => {}
        }
        if let (Some(l), None) = (&layout, settle) {
            if l.panes.iter().all(|(id, _)| frames.contains_key(id)) {
                break;
            }
        }
    }
    let layout = layout.expect("loop exits with a layout");
    frames.retain(|id, _| layout.panes.iter().any(|(p, _)| p == id));
    Ok(Observed { layout, frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_extra(extra: &[&str]) -> Result<SnapshotArgs, String> {
        let mut tail: Vec<OsString> = ["--snapshot", "--server", "demo", "--out", "/tmp/s.png"]
            .iter()
            .map(OsString::from)
            .collect();
        tail.extend(extra.iter().map(OsString::from));
        parse(&tail)
    }

    #[test]
    fn view_flag_names_every_kind_and_refuses_the_rest() {
        for (name, kind) in [
            ("bell", ViewKind::Bell),
            ("bell-wide", ViewKind::BellWide),
            ("bell-full", ViewKind::BellFull),
            ("feed", ViewKind::Feed),
            ("row-menu", ViewKind::RowMenu),
            ("hover", ViewKind::Hover),
            ("cursor", ViewKind::Cursor),
            ("tab-menu", ViewKind::TabMenu),
            ("sideline-menu", ViewKind::SidelineMenu),
            ("composer", ViewKind::Composer),
            ("split-menu", ViewKind::SplitMenu),
            ("settings", ViewKind::Settings),
            ("keys", ViewKind::Keys),
            ("org-block", ViewKind::OrgBlock),
            ("org-table", ViewKind::OrgTable),
            ("org-recent", ViewKind::OrgRecent),
            ("org-overview", ViewKind::OrgOverview),
            ("org-graph", ViewKind::OrgGraph),
            ("org-health", ViewKind::OrgHealth),
            ("org-seat", ViewKind::OrgSeat),
            ("org-feed", ViewKind::OrgFeed),
            ("org-system", ViewKind::OrgSystem),
            ("messages-tree", ViewKind::MessagesTree),
            ("messages-chats", ViewKind::MessagesChats),
            ("messages-thread", ViewKind::MessagesThread),
            ("messages-system", ViewKind::MessagesSystem),
            ("messages-archive", ViewKind::MessagesArchive),
            ("messages-you", ViewKind::MessagesYou),
            ("messages-you-thread", ViewKind::MessagesYouThread),
            ("backlog-list", ViewKind::BacklogList),
            ("backlog-kanban", ViewKind::BacklogKanban),
            ("navigator-pr", ViewKind::NavigatorPr),
            ("navigator-model", ViewKind::NavigatorModel),
            ("navigator-menu", ViewKind::NavigatorMenu),
            ("feed-detail", ViewKind::FeedDetail),
            ("question-detail", ViewKind::QuestionDetail),
            ("connections", ViewKind::Connections),
            ("connections-use", ViewKind::ConnectionsUse),
            ("yard", ViewKind::Yard),
            ("needs", ViewKind::Needs),
        ] {
            let args = parse_extra(&["--view", name]).expect(name);
            assert_eq!(args.view, Some(kind), "{name}");
        }
        // --peer holds a second window; a malformed size is a usage error.
        assert_eq!(
            parse_extra(&["--peer", "120x40"]).unwrap().peer,
            Some((40, 120))
        );
        let e = parse_extra(&["--peer", "wide"]).expect_err("malformed peer");
        assert!(e.contains("--peer wants <cols>x<rows>"), "{e}");
        let e = parse_extra(&["--view", "nonesuch"]).expect_err("unknown view names the options");
        assert!(
            e.contains(
                "bell, bell-wide, bell-full, feed, row-menu, tab-menu, sideline-menu, composer, split-menu, settings, keys, org-block, org-table, org-recent, org-overview, org-graph, org-health, org-seat, org-feed, org-system, messages-tree, messages-chats, messages-thread, messages-system, messages-archive, messages-you, messages-you-thread, backlog-list, backlog-kanban, navigator-pr, navigator-menu, navigator-model, feed-detail, question-detail, connections-use or connections"
            ),
            "{e}"
        );
    }

    #[test]
    fn raster_page_clamps_overflow() {
        // The unclamped page screenshots a composite whose bottom tile repeats
        // the first rows at an exact-fit window (the doubled chrome report).
        let page = page_html("<svg>x</svg>");
        assert!(page.contains("overflow:hidden"), "{page}");
        assert!(page.contains("<svg>x</svg>"), "{page}");
    }
}
