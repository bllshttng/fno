//! The `?` footer: the keys of the view in focus, laid out as a short grid
//! of key and label cells. One builder serves every view, so a view names
//! its own keys and the grid shape cannot drift between them. The full
//! keys overlay stays one key further, the `*` cell the grid always ends in.

use super::{Cell, StdinFlow, View};
use crate::text_style::TLine;

/// One cell of the grid: a key and what it does.
pub(crate) type KeyHint = (&'static str, &'static str);

/// The view whose keys the footer names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum KeyView {
    Agents,
    Backlog,
    Detail,
    Messages,
    Org,
}

/// The keys of one view, in reading order. The closing `* all keys` cell is
/// the builder's, not the view's.
pub(crate) fn hints(view: KeyView) -> &'static [KeyHint] {
    match view {
        KeyView::Agents => &[
            ("^B i", "new session"),
            ("^B N", "new workspace"),
            ("^B P", "menu"),
            ("^B V", "cycle views"),
            ("^B B", "backlog"),
            ("^B M", "messages"),
            ("^B O", "org"),
            ("^B d", "detach"),
        ],
        KeyView::Backlog => &[
            ("hjkl", "move"),
            ("[ ]", "lane"),
            ("L", "lanes"),
            ("Tab", "list/kanban"),
            ("/", "search"),
            ("f", "filter"),
            ("enter", "details"),
            ("c", "columns"),
            ("b", "blueprint"),
            ("t", "target"),
            ("A", "lead"),
            ("T/K/J", "rank"),
            ("F", "full screen"),
            ("esc", "close"),
        ],
        KeyView::Detail => &[
            ("j/k", "scroll"),
            ("tab", "next link"),
            ("enter", "open"),
            ("y", "copy id"),
            ("Y", "copy command"),
            ("b", "blueprint"),
            ("t", "target"),
            ("A", "lead"),
            ("c", "comment"),
            ("esc", "board"),
        ],
        KeyView::Messages => &[
            ("h/l", "columns"),
            ("j/k", "move"),
            ("enter", "open"),
            ("esc", "back"),
            ("a/b", "filter"),
            ("s", "sort"),
            ("d", "details"),
            ("r", "retry"),
            ("m", "id"),
            ("y", "copy"),
            ("q", "close"),
        ],
        KeyView::Org => &[
            ("j/k", "move"),
            ("h/l", "tab"),
            ("u", "go up"),
            ("enter", "open or attach"),
            ("space", "peek, stop, portal"),
            ("s", "start a seat"),
            ("r", "review PR"),
            ("W", "HQ web page"),
            ("F", "full screen"),
            ("V", "cycle views"),
            ("esc", "close"),
        ],
    }
}

/// The grid never grows past this many rows; a narrow pane drops the last
/// view keys before it drops the `* all keys` cell.
const MAX_ROWS: usize = 4;

/// The columns that fit `w`: four where each cell reads whole, fewer on a
/// narrow pane so a label never clips mid-word on a wide-enough terminal.
fn columns(w: usize) -> usize {
    match w {
        0..=39 => 1,
        40..=69 => 2,
        _ => 4,
    }
}

/// The grid rows for `view` at width `w`: cells read left to right, then
/// top to bottom, the key bold and the label plain. The last cell is
/// `* all keys`. Every row is padded to the same column width, and a cell
/// wider than its column is cut at the column edge.
pub(crate) fn grid_rows(view: KeyView, w: usize) -> Vec<TLine> {
    let cols = columns(w);
    let mut cells: Vec<KeyHint> = hints(view).to_vec();
    cells.truncate(cols * MAX_ROWS - 1);
    cells.push(("*", "all keys"));
    let col_w = (w / cols).max(1);
    cells
        .chunks(cols)
        .map(|chunk| {
            let mut row = TLine::plain(String::new());
            for (key, label) in chunk {
                let mut cell = TLine::head(*key);
                cell.push_line(TLine::plain(format!(" {label}")));
                row.push_line(cell.trunc(col_w.saturating_sub(1)).pad_to(col_w));
            }
            row.trim_end()
        })
        .collect()
}

/// Paint `grid` over the bottom rows of a `width`-wide surface, blanking
/// them first so the grid never lands on the view's own rows.
pub(super) fn paint_bottom(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    width: usize,
    grid: &[TLine],
) {
    let h = grid.len().min(rows.saturating_sub(1));
    let top = rows - h;
    for r in top..rows {
        for c in 0..width.min(cols) {
            cells[r * cols + c] = Cell::default();
        }
    }
    super::backlog_style::paint_panel(
        cells,
        rows,
        cols,
        top,
        width.min(cols),
        h,
        grid,
        None,
        &view.theme,
    );
}

/// Smallest terminal that has room for the footer grid; below it `^B ?`
/// opens the full key table modal instead.
const MIN_ROWS: u16 = 16;
const MIN_COLS: u16 = 40;

/// Every footer flag, the boards first and the agents list last.
fn flags(view: &mut View) -> [Option<&mut bool>; 4] {
    [
        view.backlog_board.as_mut().map(|b| &mut b.keys_footer),
        view.org_board.as_mut().map(|b| &mut b.keys_footer),
        view.messages_board.as_mut().map(|b| &mut b.keys_footer),
        Some(&mut view.keys_footer),
    ]
}

fn footer_is_open(view: &mut View) -> bool {
    flags(view).iter().flatten().any(|f| **f)
}

fn close_footers(view: &mut View) {
    flags(view).into_iter().flatten().for_each(|f| *f = false);
}

/// `^B ?`: the footer grid of the view in focus when the terminal has room,
/// else the full key table modal.
pub(super) fn toggle(view: &mut View) {
    if view.term.0 < MIN_ROWS || view.term.1 < MIN_COLS {
        view.open_keys_modal();
        return;
    }
    if footer_is_open(view) {
        close_footers(view);
    } else if let Some(open) = flags(view).into_iter().flatten().next() {
        *open = true;
    }
}

/// The agents-list footer, full width along the bottom, shown only while no
/// board covers the list.
pub(super) fn paint_agents(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    if view.keys_footer {
        paint_bottom(
            view,
            cells,
            rows,
            cols,
            cols,
            &grid_rows(KeyView::Agents, cols),
        );
    }
}

/// Keys while a footer is open: `^B ?`, `?` and Esc close it, `*` opens the
/// full key table, anything else closes it and goes on to its normal owner.
pub(super) fn footer_keys(view: &mut View, bytes: &[u8]) -> Option<Result<StdinFlow, String>> {
    if bytes.is_empty() || !footer_is_open(view) {
        return None;
    }
    close_footers(view);
    match bytes {
        [0x02, b'?'] | [b'?'] | [0x1b] => Some(Ok(StdinFlow::Continue)),
        [b'*'] => {
            view.open_keys_modal();
            Some(Ok(StdinFlow::Continue))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_is_four_wide_and_ends_in_the_full_overlay_cell() {
        let rows = grid_rows(KeyView::Messages, 100);
        assert_eq!(rows.len(), 3, "11 keys + the closing cell = 12 cells");
        assert!(rows[0].text.starts_with("h/l columns"));
        assert!(rows[2].text.trim_end().ends_with("* all keys"));
    }
}
