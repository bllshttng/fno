//! The reply dock: space on an agent row opens the peek docked under the
//! sideline list with its reply input already focused, as Claude's agents
//! view does. The dock is the peek itself in another place, so every peek
//! key, the reply send path and the persisted draft stay one implementation.

use super::*;

/// The narrowest sideline the dock fits in. Below it the reply opens in
/// the centered peek, focused the same way.
const MIN_DOCK_W: u16 = 40;

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
    pub(super) fn peek_layout(&self, chrome: &chrome::Chrome, lines: &[String]) -> OverlayLayout {
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
        // At most half the column, so the two seats never share a row.
        let dims = ((rows / 2).max(1), self.panel_w() as usize - 1);
        // Follow the last line, so a long transcript windows to its tail:
        // the newest message and the reply input stay in view.
        let follow = lines.len().checked_sub(1);
        let mut layout =
            layout_lines_overlay(origin, dims, chrome, lines, follow, OverlayAnchor::Center);
        // Sit on the column's floor, flush left: docked, not floating. When
        // the row being answered sits under that seat, the dock takes the
        // top half instead, so it never hides its own row.
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
