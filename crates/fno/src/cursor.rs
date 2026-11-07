//! The one cursor. Every list, menu, overlay and board paints its driven row
//! through [`cursor_style`], so the cursor reads the same on every surface
//! under every theme. A surface never reads `Theme::sel` for a cursor row:
//! `theme::cell_style` sends the cursor roles here, and the sideline, board,
//! feed and bell bands call this function directly.

use crate::proto::{cell_flags, Cell, Color};
use crate::theme::Theme;

/// `(fg, bg, flags)` for the row the user drives: the theme's text color as
/// the ground, its base color as the text. With no base, the emulator's own
/// pair swapped.
pub fn cursor_style(t: &Theme) -> (Color, Color, u8) {
    if t.inherit || t.base == Color::Default {
        // No BOLD: a terminal brightens bold text, so dark letters go gray.
        (Color::Default, Color::Default, cell_flags::INVERSE)
    } else {
        (t.base, t.title, 0)
    }
}

/// `(fg, bg, flags)` for a row that is marked but not driven: the focused
/// agent and the row under the pointer. It is a quiet surface band, never the
/// cursor, so two rows never both read as the one the keys act on.
pub fn resting_band_style(t: &Theme) -> (Color, Color, u8) {
    if t.inherit {
        (Color::Indexed(3), Color::Indexed(0), 0)
    } else {
        (t.stamp, t.sel, 0)
    }
}

/// Paint `cells` as cursor cells in place.
pub fn paint_cursor(cells: &mut [Cell], t: &Theme) {
    let (fg, bg, flags) = cursor_style(t);
    for cell in cells {
        cell.fg = fg;
        cell.bg = bg;
        cell.flags = flags;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cursor_role_resolves_to_the_one_style() {
        use crate::theme::{cell_style, Role, THEME_NAMES};
        for n in THEME_NAMES {
            let t = Theme::from_name(n).0;
            let want = cursor_style(&t);
            for role in [Role::BodySel, Role::BodyCursor, Role::BodyFocus] {
                assert_eq!(cell_style(role, &t), want, "{role:?} under {n}");
            }
        }
    }
}
