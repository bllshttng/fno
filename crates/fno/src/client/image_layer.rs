//! Terminal graphics for the file viewer: the protocol this terminal speaks,
//! an image encoded for one rect, and the one placement a frame carries.
//!
//! A `proto::Cell` holds one char, and kitty needs a placeholder plus
//! combining marks in every cell while iTerm2 and sixel put kilobytes in one.
//! So those three ride a layer: the viewer fills its rect with [`HOLE`] cells
//! and stages a [`Placement`], and the compositor writes the bytes after the
//! cell diff. Half-blocks are plain cells and go straight into the frame.

use std::cell::RefCell;
use std::io::IsTerminal;
use std::sync::{Arc, OnceLock};

use crossterm::{cursor, queue, style};
use image::DynamicImage;
use ratatui_core::buffer::{Buffer, CellDiffOption};
use ratatui_core::layout::{Rect as RtRect, Size};
use ratatui_core::style::Color as RtColor;
use ratatui_core::widgets::Widget;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::{Image, Resize};

use crate::proto::{Cell, Color};

/// A cell the viewer reserves for an escape-protocol image. U+FDD0 is a
/// Unicode noncharacter, so pane text never holds it, and the compositor
/// prints it as a space: it never reaches the terminal.
pub(super) const HOLE: Cell = Cell {
    c: '\u{FDD0}',
    fg: Color::Default,
    bg: Color::Default,
    flags: 0,
};

/// The kitty delete for every placement and its data. The mux client places
/// no other images, so all of them are ours.
pub(super) const KITTY_DELETE: &[u8] = b"\x1b_Ga=d,d=A,q=2\x1b\\";

static PICKER: OnceLock<Picker> = OnceLock::new();

/// Set once any compose in this process painted an image. The snapshot
/// stamps its png with it.
static PAINTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// True once this process painted an image.
pub(super) fn painted() -> bool {
    PAINTED.load(std::sync::atomic::Ordering::Relaxed)
}

/// Terminal cells: top-left corner and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Rect {
    pub row: u16,
    pub col: u16,
    pub w: u16,
    pub h: u16,
}

/// One image encoded for one rect.
pub(super) enum Rendered {
    /// Half-blocks: plain cells for the frame.
    Cells(Buffer),
    /// Kitty, iTerm2 or sixel: bytes positioned for the rect.
    Escape { bytes: Arc<[u8]>, kitty: bool },
}

/// The escape-protocol image one frame shows.
#[derive(Clone, Debug)]
pub(super) struct Placement {
    pub rect: Rect,
    pub bytes: Arc<[u8]>,
    pub kitty: bool,
}

impl Placement {
    /// The same encoded image at the same rect: already on the terminal.
    pub(super) fn same(&self, other: &Placement) -> bool {
        self.rect == other.rect && Arc::ptr_eq(&self.bytes, &other.bytes)
    }
}

thread_local! {
    /// What the current compose placed. The viewer clears and fills it inside
    /// `View::compose`, and `Compositor::draw` takes it in the same expression,
    /// with no await between, so both run on one thread.
    static STAGED: RefCell<Option<Placement>> = const { RefCell::new(None) };
}

/// Pick the protocol once, for this process's terminal. It must run before
/// anything reads stdin: the terminal's reply would otherwise reach a pane.
pub(super) fn detect() {
    // Forces kitty, iterm2, sixel or halfblocks. Any but halfblocks still
    // queries, for the font size.
    let forced = std::env::var("FNO_MUX_IMAGE_PROTOCOL").ok();
    let _ = PICKER.set(pick(forced.as_deref()));
}

fn pick(forced: Option<&str>) -> Picker {
    let forced = forced.and_then(protocol_named);
    if forced == Some(ProtocolType::Halfblocks)
        || !(std::io::stdin().is_terminal() && std::io::stdout().is_terminal())
    {
        return Picker::halfblocks();
    }
    // The query ends on a status reply every terminal sends, so its crate
    // default bound (2 s, restarted on each read) costs nothing. A shorter
    // one is a trap: on a timeout the crate's query thread stays blocked on
    // stdin, eats the next input, then restores cooked mode over our raw one.
    let mut picker = Picker::from_query_stdio().unwrap_or_else(|_| Picker::halfblocks());
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    picker
}

fn protocol_named(name: &str) -> Option<ProtocolType> {
    match name.trim().to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "iterm2" => Some(ProtocolType::Iterm2),
        "sixel" => Some(ProtocolType::Sixel),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    }
}

/// The picked protocol; half-blocks where nothing was detected (tests, the
/// snapshot renderer).
fn picker() -> &'static Picker {
    static FALLBACK: OnceLock<Picker> = OnceLock::new();
    PICKER
        .get()
        .unwrap_or_else(|| FALLBACK.get_or_init(Picker::halfblocks))
}

/// Encode `img` to fit `rect`. `None` when the rect is empty or the encoder
/// refuses the image.
pub(super) fn render(img: &DynamicImage, rect: Rect) -> Option<Rendered> {
    render_with(picker(), img, rect)
}

/// The terminal's cell size in device pixels, so a PDF page renders at the
/// panel's pixel size. The fallback picker (tests, snapshot renderer) reads
/// 10x20.
pub(super) fn font_px() -> (u16, u16) {
    let fs = picker().font_size();
    (fs.width, fs.height)
}

fn render_with(picker: &Picker, img: &DynamicImage, rect: Rect) -> Option<Rendered> {
    if rect.w == 0 || rect.h == 0 {
        return None;
    }
    let proto = picker
        .new_protocol(img.clone(), Size::new(rect.w, rect.h), Resize::Fit(None))
        .ok()?;
    let area = RtRect::new(0, 0, rect.w, rect.h);
    let mut buf = Buffer::empty(area);
    Image::new(&proto).render(area, &mut buf);
    Some(match picker.protocol_type() {
        ProtocolType::Halfblocks => Rendered::Cells(buf),
        p => Rendered::Escape {
            bytes: serialize(&buf, rect).into(),
            kitty: p == ProtocolType::Kitty,
        },
    })
}

/// The buffer as terminal bytes at `rect`. Skipped cells (under an iTerm2 or
/// sixel image) and blank cells print nothing: the compositor blanks the rect
/// before it writes these.
fn serialize(buf: &Buffer, rect: Rect) -> Vec<u8> {
    let mut out = Vec::new();
    let w = usize::from(buf.area.width.max(1));
    for (y, row) in buf.content.chunks(w).enumerate() {
        let mut moved = false;
        for (x, cell) in row.iter().enumerate() {
            let blank = cell.symbol() == " " && cell.fg == RtColor::Reset;
            if matches!(cell.diff_option, CellDiffOption::Skip) || blank {
                moved = false;
                continue;
            }
            if !moved {
                let _ = queue!(
                    out,
                    cursor::MoveTo(rect.col + x as u16, rect.row + y as u16)
                );
                moved = true;
            }
            let fg = match cell.fg {
                RtColor::Rgb(r, g, b) => style::Color::Rgb { r, g, b },
                _ => style::Color::Reset,
            };
            let _ = queue!(out, style::SetForegroundColor(fg));
            out.extend_from_slice(cell.symbol().as_bytes());
        }
    }
    let _ = queue!(out, style::SetAttribute(style::Attribute::Reset));
    out
}

/// Paint a rendered image into the frame at `rect`: half-blocks as cells, an
/// escape protocol as [`HOLE`] cells plus the staged placement.
pub(super) fn paint(r: &Rendered, rect: Rect, cells: &mut [Cell], cols: usize) {
    PAINTED.store(true, std::sync::atomic::Ordering::Relaxed);
    match r {
        Rendered::Cells(buf) => crate::ratatui_blit::blit_area(
            buf,
            usize::from(rect.row),
            usize::from(rect.col),
            cells,
            cols,
        ),
        Rendered::Escape { bytes, kitty } => {
            let (r0, c0) = (usize::from(rect.row), usize::from(rect.col));
            for y in r0..r0 + usize::from(rect.h) {
                for x in c0..(c0 + usize::from(rect.w)).min(cols) {
                    if let Some(cell) = cells.get_mut(y * cols + x) {
                        *cell = HOLE;
                    }
                }
            }
            STAGED.with(|s| {
                *s.borrow_mut() = Some(Placement {
                    rect,
                    bytes: bytes.clone(),
                    kitty: *kitty,
                })
            });
        }
    }
}

/// Forget the last compose's placement. The viewer calls it first on every
/// compose, so a closed viewer stages nothing.
pub(super) fn clear() {
    STAGED.with(|s| *s.borrow_mut() = None);
}

/// The placement the last compose staged, once.
pub(super) fn take() -> Option<Placement> {
    STAGED.with(|s| s.borrow_mut().take())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 40x40: four by two cells at the default font size, inside `RECT`.
    fn png() -> DynamicImage {
        DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            40,
            40,
            image::Rgba([200, 30, 30, 255]),
        ))
    }

    fn forced(p: ProtocolType) -> Picker {
        let mut picker = Picker::halfblocks();
        picker.set_protocol_type(p);
        picker
    }

    const RECT: Rect = Rect {
        row: 3,
        col: 5,
        w: 8,
        h: 4,
    };

    #[test]
    fn escape_protocols_render_bytes_positioned_at_the_rect() {
        let escape = |p| match render_with(&forced(p), &png(), RECT) {
            Some(Rendered::Escape { bytes, kitty }) => {
                (String::from_utf8_lossy(&bytes).into_owned(), kitty)
            }
            _ => panic!("{p:?} should render escape bytes"),
        };
        let (kitty, is_kitty) = escape(ProtocolType::Kitty);
        assert!(is_kitty);
        assert!(
            kitty.contains("\x1b_G") && kitty.contains("U=1"),
            "{kitty:?}"
        );
        assert!(
            kitty.contains("\x1b[4;6H"),
            "bytes start at the rect: {kitty:?}"
        );
        let (iterm, is_kitty) = escape(ProtocolType::Iterm2);
        assert!(!is_kitty);
        assert!(iterm.contains("\x1b]1337;File="), "{iterm:?}");
        let empty = Rect { w: 0, ..RECT };
        assert!(render_with(&forced(ProtocolType::Kitty), &png(), empty).is_none());
    }

    #[test]
    fn an_escape_paint_holes_its_rect_and_stages_one_placement() {
        let cols = 20;
        let mut cells = vec![Cell::default(); cols * 10];
        let r = render_with(&forced(ProtocolType::Kitty), &png(), RECT).expect("kitty renders");
        clear();
        paint(&r, RECT, &mut cells, cols);
        let holes = cells.iter().filter(|c| **c == HOLE).count();
        assert_eq!(holes, usize::from(RECT.w * RECT.h));
        let placed = take().expect("a placement is staged");
        assert_eq!(placed.rect, RECT);
        assert!(placed.kitty);
        assert!(take().is_none(), "take empties the stage");
        assert_eq!(protocol_named(" Kitty "), Some(ProtocolType::Kitty));
        assert_eq!(protocol_named("chafa"), None);
    }
}
