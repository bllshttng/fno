//! The sideline footer's button row: new session, new workspace, menu.
//! Each button opens what its key opens, and one fn lays the row out so the
//! paint and the hit-test cannot diverge.

use std::ops::Range;

use super::View;

/// The three buttons, by keymap action and bare label, left to right. ASCII
/// only: the former `☰` is an ambiguous-width glyph some terminals render
/// wide, which displaced that row's whole line.
const SESSION: (&str, &str) = ("toggle-composer", "+ session");
/// The session button's widest label, Claude's agents-view prompt wording.
/// It shows only where the whole row fits; narrower rows step down.
const TASK_PROMPT: &str = "+ describe a task for a new session";
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
    /// The footer at this panel width, one row of three buttons joined by
    /// ` · `: `+ session (i) · + workspace (N) · menu (P)`, with the session
    /// button reading the task prompt when the panel is wide. The keys resolve
    /// through the live chord table (`key_for`), so a rebind relabels the
    /// row. A narrow panel shows the keys only (`i · N · P`), each still a
    /// button. Narrower still, or with a recruit-mark tally on the row, the
    /// legacy one-button row paints.
    pub(super) fn footer_buttons(&self, panel_w: usize) -> FooterButtons {
        // last column is the divider
        let tw = panel_w.saturating_sub(1);
        if self.marks.is_empty() {
            // Columns as the row painter counts them, so a rebound key
            // with a wide glyph cannot move a range off its label.
            let cols = |s: &str| s.chars().map(super::glyph_cols).sum::<usize>();
            for (prompt, with_label) in [(true, true), (false, true), (false, false)] {
                let label = |(action, bare): (&str, &str)| {
                    let bare = if prompt && action == SESSION.0 {
                        TASK_PROMPT
                    } else {
                        bare
                    };
                    crate::keys::key_for(action).map(|k| {
                        if with_label {
                            format!("{bare} ({k})")
                        } else {
                            k.to_string()
                        }
                    })
                };
                let (Some(session), Some(workspace), Some(menu)) =
                    (label(SESSION), label(WORKSPACE), label(MENU))
                else {
                    continue;
                };
                let text = format!("{session} \u{b7} {workspace} \u{b7} {menu}");
                if cols(&text) <= tw {
                    let menu_at = cols(&text) - cols(&menu);
                    return FooterButtons {
                        session: Some(0..cols(&session)),
                        menu: Some(menu_at..cols(&text)),
                        text,
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
}
