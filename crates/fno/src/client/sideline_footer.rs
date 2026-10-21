//! The sideline footer's button row: new session, new workspace, menu.
//! Each button opens what its key opens, and one fn lays the row out so the
//! paint and the hit-test cannot diverge.

use std::ops::Range;

use super::View;

/// The three buttons, by keymap action and bare label, left to right. ASCII
/// only: the former `☰` is an ambiguous-width glyph some terminals render
/// wide, which displaced that row's whole line.
const SESSION: (&str, &str) = ("toggle-composer", "+ session");
const WORKSPACE: (&str, &str) = ("new-workspace", "+ workspace");
const MENU: (&str, &str) = ("sideline-menu", "menu");

/// The footer row as painted, with each button's column range. A `None`
/// range means that button has no seat: the row then opens new workspace.
#[derive(Debug, Default)]
pub(super) struct FooterButtons {
    pub(super) text: String,
    pub(super) session: Option<Range<usize>>,
    pub(super) menu: Option<Range<usize>>,
}

impl View {
    /// The footer at this panel width. The hinted labels (`+ session (i)`)
    /// resolve through the live chord table (`key_for`), so a rebind
    /// relabels the row. When the hints do not fit, the bare labels keep
    /// every button's seat (the density-button rule: the hint drops, the
    /// affordance stays). Narrower still, or with a recruit-mark tally on
    /// the row, the legacy one-button row paints.
    pub(super) fn footer_buttons(&self, panel_w: usize) -> FooterButtons {
        // last column is the divider
        let tw = panel_w.saturating_sub(1);
        if self.marks.is_empty() {
            for hint in [true, false] {
                let label = |(action, bare): (&str, &str)| match crate::keys::key_for(action) {
                    Some(k) if hint => format!("{bare} ({k})"),
                    _ => bare.to_string(),
                };
                let (session, workspace, menu) = (label(SESSION), label(WORKSPACE), label(MENU));
                let left = format!("{session}  {workspace}");
                if left.len() + 1 + menu.len() <= tw {
                    let menu_at = tw - menu.len();
                    return FooterButtons {
                        text: format!("{left:<menu_at$}{menu}"),
                        session: Some(0..session.len()),
                        menu: Some(menu_at..tw),
                    };
                }
            }
            // Too narrow for three: the row is one new-workspace button.
            return FooterButtons {
                text: WORKSPACE.1.to_string(),
                ..Default::default()
            };
        }
        FooterButtons {
            text: format!("{}   {} marked \u{b7}R", WORKSPACE.1, self.marks.len()),
            ..Default::default()
        }
    }

    /// The column range of the footer's `menu` button, shared by the
    /// renderer and the hit-test so a click lands where it draws.
    pub(super) fn footer_menu_range(&self, panel_w: usize) -> Option<Range<usize>> {
        self.footer_buttons(panel_w).menu
    }
}
