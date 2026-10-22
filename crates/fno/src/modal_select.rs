//! Mouse text selection for every framed modal, built once in the shared
//! frame.
//!
//! The painters ([`crate::chrome::blit`], [`crate::popup::draw`]) report each
//! body row they put on screen through [`paint_line`]. The recorder keeps the
//! rows of the last frame as blocks of cells, so a drag works in the modal's
//! own coordinates: the border, the title and the footer are never part of a
//! selection. A selection paints with the theme's `sel` surface from the same
//! call, and [`release`] returns the text to copy.
//!
//! A soft wrap is not a line break. A row the layout marked as a
//! continuation joins back onto the row above: with nothing between when the
//! wrap split a word (a long URL broken mid-token), with one space when it
//! stood at a space. So a wrapped URL pastes whole.
//!
//! State is thread-local, like the close-chip recorder beside it: the client
//! composes and handles mouse on one thread, and each test gets its own.

use crate::chrome::{BodySpan, Join};
use crate::proto::Cell;
use crate::theme::{cell_style, Role, Theme};
use std::cell::RefCell;
use std::time::{Duration, Instant};

/// Two presses on one cell inside this window select the word under them.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// One painted body cell: its screen column, its char, and its width.
type BodyCell = (usize, char, usize);

#[derive(Debug, Clone)]
struct Line {
    row: usize,
    cells: Vec<BodyCell>,
    /// The body's screen columns, `lo..hi`.
    lo: usize,
    hi: usize,
    join: Join,
}

impl Line {
    fn first_text_col(&self) -> Option<usize> {
        self.cells
            .iter()
            .find(|(_, ch, _)| !ch.is_whitespace())
            .map(|&(c, _, _)| c)
    }
}

/// The body rows of one framed block, top to bottom.
#[derive(Debug, Clone, Default)]
struct Block {
    /// `(first body row, left column)`: the block's identity across frames.
    key: (usize, usize),
    lines: Vec<Line>,
}

impl Block {
    fn sig(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for l in &self.lines {
            l.row.hash(&mut h);
            for &(c, ch, _) in &l.cells {
                (c, ch).hash(&mut h);
            }
        }
        h.finish()
    }

    fn line_at(&self, row: usize) -> Option<&Line> {
        let first = self.lines.first()?.row;
        self.lines.get(row.checked_sub(first)?)
    }
}

#[derive(Debug, Clone)]
struct Sel {
    key: (usize, usize),
    sig: u64,
    anchor: (usize, usize),
    head: (usize, usize),
    /// A drag moved off the press cell (or a word was selected): the gesture
    /// is a selection, not a click.
    live: bool,
    /// The text already went to the clipboard (a double-click copies on the
    /// press), so the release has nothing left to copy.
    copied: bool,
}

#[derive(Default)]
struct State {
    building: Option<Vec<Block>>,
    last: Vec<Block>,
    sel: Option<Sel>,
    last_press: Option<(Instant, (usize, usize))>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

/// Start recording the body rows a frame paints.
pub fn frame_begin() {
    STATE.with(|s| s.borrow_mut().building = Some(Vec::new()));
}

/// Keep what the frame painted. A selection whose block is gone, or whose
/// text changed under it (a scroll, a refresh), is dropped: its cells would
/// no longer name what the reader selected.
pub fn frame_end() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(blocks) = s.building.take() else {
            return;
        };
        // Only a live selection names cells; a press still waiting on its
        // release holds no text, so a refresh in between must not eat the click.
        let stale = s.sel.as_ref().is_some_and(|sel| {
            sel.live
                && !blocks
                    .iter()
                    .any(|b| b.key == sel.key && b.sig() == sel.sig)
        });
        if stale {
            s.sel = None;
        }
        s.last = blocks;
    });
}

/// Record one painted line and, when a selection covers it, repaint its
/// selected cells with the theme's selection surface. A line with no body
/// span (a border, the title, the footer) is not text a drag can take.
pub fn paint_line(
    cells: &mut [Cell],
    (rows, cols): (usize, usize),
    theme: &Theme,
    (row, c0): (usize, usize),
    text: &str,
    body: Option<BodySpan>,
) {
    let Some(span) = body else {
        return;
    };
    if row >= rows {
        return;
    }
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let State { building, sel, .. } = &mut *s;
        let Some(blocks) = building.as_mut() else {
            return;
        };
        let mut body_cells: Vec<BodyCell> = Vec::new();
        let mut col = c0;
        for (j, ch) in text.chars().enumerate() {
            let w = crate::chrome::char_cols(ch);
            if j >= span.start && j < span.end && col < cols {
                body_cells.push((col, ch, w));
            }
            col += w;
        }
        let (Some(&(lo, _, _)), Some(&(last, _, lw))) = (body_cells.first(), body_cells.last())
        else {
            return;
        };
        let line = Line {
            row,
            cells: body_cells,
            lo,
            hi: last + lw,
            join: span.join,
        };
        let extends = blocks
            .last()
            .is_some_and(|b| b.key.1 == c0 && b.lines.last().is_some_and(|l| l.row + 1 == row));
        if !extends {
            blocks.push(Block {
                key: (row, c0),
                lines: Vec::new(),
            });
        }
        let block = blocks.last_mut().expect("a block was just ensured");
        if let Some(sel) = sel.as_ref().filter(|s| s.key == block.key && s.live) {
            let (from, to) = ordered(sel.anchor, sel.head);
            if row >= from.0 && row <= to.0 {
                let a = if row == from.0 { from.1 } else { line.lo };
                let b = if row == to.0 { to.1 } else { line.hi - 1 };
                let (fg, bg, flags) = cell_style(Role::BodySel, theme);
                for &(c, _, w) in &line.cells {
                    if c + w > a && c <= b {
                        for k in c..(c + w).min(cols) {
                            let cell = &mut cells[row * cols + k];
                            cell.fg = fg;
                            cell.bg = bg;
                            cell.flags =
                                (cell.flags & crate::proto::cell_flags::WIDE_SPACER) | flags;
                        }
                    }
                }
            }
        }
        block.lines.push(line);
    });
}

fn ordered(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

/// The top block of the last frame, when it holds the body cell.
fn hit(state: &State, row: usize, col: usize) -> Option<&Block> {
    let block = state.last.last()?;
    block
        .line_at(row)
        .filter(|l| col >= l.lo && col < l.hi)
        .map(|_| block)
}

/// Select the longest logical line of the top block, as a drag across it
/// would, and return its text. The snapshot views use it to shoot a
/// selection; a wrapped line spans several rows, so it makes the best
/// picture of the join.
pub fn select_longest_line() -> Option<String> {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let block = s.last.last()?.clone();
        let mut best: Option<(usize, usize)> = None;
        let mut start = 0;
        for i in 1..=block.lines.len() {
            if i == block.lines.len() || block.lines[i].join == Join::Line {
                let len = i - start;
                if best.is_none_or(|(_, l)| len > l) {
                    best = Some((start, len));
                }
                start = i;
            }
        }
        let (at, len) = best?;
        let first = &block.lines[at];
        let last = &block.lines[at + len - 1];
        let from = (first.row, first.first_text_col()?);
        let end = last
            .cells
            .iter()
            .rev()
            .find(|(_, ch, _)| !ch.is_whitespace())
            .map(|&(c, _, w)| c + w - 1)?;
        let to = (last.row, end);
        let text = extract(&block, from, to);
        s.sel = Some(Sel {
            key: block.key,
            sig: block.sig(),
            anchor: from,
            head: to,
            live: true,
            copied: true,
        });
        Some(text)
    })
}

/// Drop any selection.
pub fn clear() {
    STATE.with(|s| s.borrow_mut().sel = None);
}

/// Whether a selection is on screen.
pub fn active() -> bool {
    STATE.with(|s| s.borrow().sel.is_some())
}

/// What a left press did.
#[derive(Debug, PartialEq, Eq)]
pub enum Press {
    /// Not on a modal's body text: the caller handles the press as before.
    Pass,
    /// A drag may start here. The caller holds the press back: a release on
    /// the same cell replays it as a click.
    Armed,
    /// A double-click selected the word under it; the text is ready to copy.
    Word(String),
}

/// A left press at a screen cell.
pub fn press(row: usize, col: usize, now: Instant) -> Press {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.sel = None;
        let Some(block) = hit(&s, row, col).cloned() else {
            s.last_press = None;
            return Press::Pass;
        };
        let doubled = s
            .last_press
            .is_some_and(|(t, cell)| cell == (row, col) && now.duration_since(t) <= DOUBLE_CLICK);
        s.last_press = Some((now, (row, col)));
        if doubled {
            if let Some((a, b)) = word_at(&block, row, col) {
                let text = extract(&block, a, b);
                s.sel = Some(Sel {
                    key: block.key,
                    sig: block.sig(),
                    anchor: a,
                    head: b,
                    live: true,
                    copied: true,
                });
                s.last_press = None;
                return Press::Word(text);
            }
        }
        s.sel = Some(Sel {
            key: block.key,
            sig: block.sig(),
            anchor: (row, col),
            head: (row, col),
            live: false,
            copied: false,
        });
        Press::Armed
    })
}

/// A left-button drag to a screen cell. Returns whether a selection gesture
/// owns the pointer. The head clamps to the block, so a drag that leaves the
/// modal still extends the selection to its edge.
pub fn drag(row: usize, col: usize) -> bool {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let State { last, sel, .. } = &mut *s;
        let Some(sel) = sel.as_mut() else {
            return false;
        };
        let Some(block) = last.last().filter(|b| b.key == sel.key) else {
            return false;
        };
        let (Some(first), Some(end)) = (block.lines.first(), block.lines.last()) else {
            return false;
        };
        let row = row.clamp(first.row, end.row);
        let line = block.line_at(row).expect("row is inside the block");
        let head = (row, col.clamp(line.lo, line.hi - 1));
        if head != sel.anchor {
            sel.live = true;
        }
        sel.head = head;
        true
    })
}

/// What a left release did.
#[derive(Debug, PartialEq, Eq)]
pub enum Release {
    /// No gesture was armed: the caller handles the release as before.
    Pass,
    /// The press never moved: replay it as a click at this cell.
    Click(usize, usize),
    /// A drag selected this text: copy it.
    Copy(String),
}

/// A left release. A selection stays painted until the next press.
pub fn release() -> Release {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(sel) = s.sel.clone() else {
            return Release::Pass;
        };
        if !sel.live {
            s.sel = None;
            return Release::Click(sel.anchor.0, sel.anchor.1);
        }
        if sel.copied {
            return Release::Pass;
        }
        let Some(block) = s.last.last().filter(|b| b.key == sel.key) else {
            return Release::Pass;
        };
        let (a, b) = ordered(sel.anchor, sel.head);
        Release::Copy(extract(block, a, b))
    })
}

/// The text of the cells from `from` to `to` (inclusive, reading order).
fn extract(block: &Block, from: (usize, usize), to: (usize, usize)) -> String {
    struct Piece {
        text: String,
        join: Join,
        /// The segment began at the line's left edge, so its leading
        /// whitespace is the modal's own indent, not the reader's selection.
        from_edge: bool,
    }
    let mut pieces: Vec<Piece> = Vec::new();
    for row in from.0..=to.0 {
        let Some(line) = block.line_at(row) else {
            continue;
        };
        let a = if row == from.0 { from.1 } else { line.lo };
        let b = if row == to.0 { to.1 } else { line.hi - 1 };
        let text: String = line
            .cells
            .iter()
            .filter(|&&(c, _, w)| c + w > a && c <= b)
            .map(|&(_, ch, _)| ch)
            .collect();
        let first_text = line.first_text_col();
        pieces.push(Piece {
            text,
            join: if row == from.0 { Join::Line } else { line.join },
            from_edge: a <= first_text.unwrap_or(a),
        });
    }
    // The modal's own indent: the smallest lead among whole-line segments.
    let indent = pieces
        .iter()
        .filter(|p| p.from_edge && p.join == Join::Line && !p.text.trim().is_empty())
        .map(|p| p.text.len() - p.text.trim_start().len())
        .min()
        .unwrap_or(0);
    let mut out = String::new();
    for (i, p) in pieces.iter().enumerate() {
        let body = p.text.trim_end();
        if i == 0 {
            out.push_str(if p.from_edge {
                &body[indent.min(lead(body))..]
            } else {
                body
            });
            continue;
        }
        match p.join {
            Join::Seam => out.push_str(body.trim_start()),
            Join::Space => {
                out.push(' ');
                out.push_str(body.trim_start());
            }
            Join::Line => {
                out.push('\n');
                out.push_str(if p.from_edge {
                    &body[indent.min(lead(body))..]
                } else {
                    body
                });
            }
        }
    }
    out.trim_end().to_string()
}

fn lead(s: &str) -> usize {
    s.len() - s.trim_start().len()
}

/// The word under a cell as `(first cell, last cell)`: a run of non-space
/// chars, followed across soft wraps so a wrapped URL selects whole.
fn word_at(block: &Block, row: usize, col: usize) -> Option<((usize, usize), (usize, usize))> {
    let at = block.lines.iter().position(|l| l.row == row)?;
    let mut first = at;
    while block.lines[first].join != Join::Line && first > 0 {
        first -= 1;
    }
    let mut last = at;
    while block
        .lines
        .get(last + 1)
        .is_some_and(|l| l.join != Join::Line)
    {
        last += 1;
    }
    // The logical line as one char stream: (line, cell) per char, with a
    // `None` gap where a word-boundary wrap stands for a space.
    let mut chars: Vec<Option<(usize, usize)>> = Vec::new();
    for (li, line) in block.lines.iter().enumerate().take(last + 1).skip(first) {
        if line.join == Join::Space {
            chars.push(None);
        }
        // A continuation's leading indent belongs to no word.
        let skip = if line.join != Join::Line {
            line.cells
                .iter()
                .take_while(|c| c.1.is_whitespace())
                .count()
        } else {
            0
        };
        for (ci, cell) in line.cells.iter().enumerate().skip(skip) {
            chars.push((!cell.1.is_whitespace()).then_some((li, ci)));
        }
    }
    let click = chars.iter().position(|c| {
        c.is_some_and(|(li, ci)| {
            block.lines[li].row == row && {
                let (c0, _, w) = block.lines[li].cells[ci];
                col >= c0 && col < c0 + w
            }
        })
    })?;
    let mut lo = click;
    while lo > 0 && chars[lo - 1].is_some() {
        lo -= 1;
    }
    let mut hi = click;
    while chars.get(hi + 1).is_some_and(|c| c.is_some()) {
        hi += 1;
    }
    let cell = |k: usize, end: bool| {
        let (li, ci) = chars[k].expect("a word char");
        let line = &block.lines[li];
        let (c0, _, w) = line.cells[ci];
        (line.row, if end { c0 + w - 1 } else { c0 })
    };
    Some((cell(lo, false), cell(hi, true)))
}

#[cfg(test)]
#[path = "modal_select_tests.rs"]
mod tests;
