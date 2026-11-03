//! The reply dock: space on an agent row docks the peek under the list,
//! reply focused. It is the peek in another place: one implementation.

use super::overlay_paint::layout_lines_overlay_at;
use super::*;

/// The narrowest sideline the dock fits in; below it the peek centers.
const MIN_DOCK_W: u16 = 40;

/// The dock's height range in a column `avail` rows tall: the list keeps
/// two rows, the dock keeps four.
pub(super) fn dock_bounds(avail: usize) -> (usize, usize) {
    let hi = avail.saturating_sub(2).max(1);
    (avail.min(4).min(hi), hi)
}

/// Open the peek on `name` docked under the list, reply input focused.
pub(super) async fn open(
    view: &mut View,
    cur: usize,
    name: String,
    sock_w: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    fetch_peek(view, cur, name.clone(), sock_w).await?;
    view.peek_docked = true;
    view.peek_input = Some((name.clone(), super::composer_draft::load(&name)));
    view.peek_input_esc.clear();
    Ok(())
}

impl View {
    pub(super) fn peek_is_docked(&self) -> bool {
        self.peek_docked && self.peek.is_some() && self.panel_w() >= MIN_DOCK_W
    }

    /// Where the peek paints: the bottom half of the sideline column when
    /// docked, else centered in the content viewport. The draw and the hit
    /// test both read this one layout.
    pub(super) fn peek_layout<S: crate::text_style::ToTLine>(
        &self,
        chrome: &chrome::Chrome,
        lines: &[S],
    ) -> OverlayLayout {
        if !self.peek_is_docked() {
            let (origin, dims) = self.overlay_viewport();
            return layout_lines_overlay(origin, dims, chrome, lines, None, OverlayAnchor::Center);
        }
        let rows = self
            .term
            .0
            .saturating_sub(TAB_BAR_ROWS + self.status_rows()) as usize;
        let origin = (TAB_BAR_ROWS as usize, 0);
        // The last column is the sideline divider; the dock leaves it.
        // Half the column unless the operator dragged it, so the two seats
        // share no row by default.
        let (lo, hi) = dock_bounds(rows);
        let height =
            self.edge_sizes
                .resolve(super::edge_drag::Edge::ReplyDock, (rows / 2).max(1), lo, hi);
        let dims = (height, self.panel_w() as usize - 1);
        // Follow the tail: the newest message and the reply stay in view.
        let follow = lines.len().checked_sub(1);
        let mut layout = layout_lines_overlay_at(
            origin,
            dims,
            chrome,
            lines,
            follow,
            false,
            OverlayAnchor::Center,
        );
        // Sit on the column's floor; over the answered row, take the top.
        let floor = (origin.0 + rows).saturating_sub(layout.framed.lines.len());
        let cursor_row = self.peek.as_ref().map_or(0, |p| {
            p.cursor.saturating_sub(self.sideline_offset()) + origin.0
        });
        layout.origin = (if cursor_row >= floor { origin.0 } else { floor }, 0);
        layout
    }

    /// The dock paints over sideline rows, so it owns a press inside its box.
    pub(super) fn dock_holds(&self, l: &OverlayLayout, rep: crate::mouse::MouseReport) -> bool {
        let (row, col) = (rep.row as usize, rep.col as usize);
        self.peek_is_docked() && chrome::framed_contains(&l.framed, l.origin, row, col)
    }
}
