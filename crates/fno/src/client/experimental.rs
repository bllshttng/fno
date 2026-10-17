//! The sideline menu's experimental section: features still in progress,
//! each behind a toggle that is off until the operator turns it on. The
//! choice persists in the view store under the feature's name.

use super::*;

pub(crate) const ORG_BOARD: &str = "org_board";
pub(crate) const PANE_SHARE: &str = "pane_share";

/// One experimental feature the menu can toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flag {
    OrgBoard,
    PaneShare,
}

/// The toggle states the menu builder renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Flags {
    pub(crate) org_board: bool,
    pub(crate) pane_share: bool,
}

impl Flag {
    pub(crate) const ALL: [Flag; 2] = [Flag::OrgBoard, Flag::PaneShare];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Flag::OrgBoard => "fleet org board",
            Flag::PaneShare => "pane share",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Flag::OrgBoard => ORG_BOARD,
            Flag::PaneShare => PANE_SHARE,
        }
    }

    pub(crate) fn on(self, flags: Flags) -> bool {
        match self {
            Flag::OrgBoard => flags.org_board,
            Flag::PaneShare => flags.pane_share,
        }
    }
}

pub(crate) fn flags(view: &View) -> Flags {
    Flags {
        org_board: view.experimental_org_board,
        pane_share: view.experimental_pane_share,
    }
}

/// Flip one toggle, persist it, and rebuild the open menu so its rows track
/// it. Turning the org board off while it is showing drops back to Agents.
pub(crate) fn toggle(view: &mut View, flag: Flag) {
    let on = !flag.on(flags(view));
    match flag {
        Flag::OrgBoard => {
            view.experimental_org_board = on;
            if !on && view.sideline_view == crate::view_store::SidelineView::Org {
                view.org_board = None;
                backlog_board::set_sideline_view(view, crate::view_store::SidelineView::Agents);
            }
        }
        Flag::PaneShare => view.experimental_pane_share = on,
    }
    crate::view_store::save_experimental(flag.name(), on);
    let word = if on { "on" } else { "off" };
    view.set_notice(format!("experimental {}: {word}", flag.label()));
    view.refresh_open_sideline_menu();
}
