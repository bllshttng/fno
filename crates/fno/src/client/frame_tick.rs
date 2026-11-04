//! The client loop's frame timer: when to wake an otherwise idle terminal
//! (nothing else redraws there) so a timed animation advances. Three sources:
//! the Working-row spin, the metrics skeleton's breathe and the need-input pulse. Re-armed each loop pass; with none running
//! there is no deadline and no wakeup.

use super::*;

impl View {
    pub(super) fn frame_deadline(&self) -> Option<Instant> {
        let spin = crate::lattice::spin_epoch()
            .filter(|_| self.working_row_on_screen())
            .map(|t0| (t0, crate::lattice::SPIN_FRAME_MS));
        let breathe = crate::lattice::spin_epoch()
            .filter(|_| self.breathe_on_screen())
            .map(|t0| (t0, crate::lattice::SPIN_FRAME_MS));
        let pulse = crate::lattice::spin_epoch()
            .filter(|_| self.need_input_on_screen())
            .map(|t0| (t0, crate::lattice::SPIN_FRAME_MS));
        spin.into_iter()
            .chain(breathe)
            .chain(pulse)
            .map(|(t0, step)| next_boundary(t0, step))
            .min()
    }

    /// A Working row a repaint would move: in a pane header of this tab, or
    /// in the sideline's scroll window. A Working row off screen wakes nothing.
    fn working_row_on_screen(&self) -> bool {
        let working = |a: &AgentRow| agent_lattice_state(a) == LatticeState::Working;
        let in_pane = self.layout.agents.iter().any(|a| {
            working(a)
                && a.pane_id
                    .is_some_and(|p| self.layout.panes.iter().any(|(id, _)| *id == p))
        });
        let sideline_shown = (self.sideline_full || self.panel_w() > 0)
            && self.sideline_view != crate::view_store::SidelineView::Backlog;
        in_pane
            || sideline_shown
                && self
                    .painted_rows()
                    .iter()
                    .skip(self.sideline_offset())
                    .take(self.sideline_visible_rows())
                    .any(|r| matches!(r, DisplayRow::Agent(a) if working(a)))
    }

    /// The strip row paints the need-input count only in the Agents view,
    /// and only while something needs input.
    fn need_input_on_screen(&self) -> bool {
        (self.sideline_full || self.panel_w() > 0)
            && self.sideline_view == crate::view_store::SidelineView::Agents
            && self.need_input_count() > 0
    }

    /// A CardMetrics row with an unserved field in the scroll window: its
    /// skeleton needs frames to pulse, and an off-screen one wakes nothing.
    fn breathe_on_screen(&self) -> bool {
        let sideline_shown = (self.sideline_full || self.panel_w() > 0)
            && self.sideline_view != crate::view_store::SidelineView::Backlog;
        sideline_shown
            && self
                .painted_rows()
                .iter()
                .skip(self.sideline_offset())
                .take(self.sideline_visible_rows())
                .any(|r| {
                    matches!(r, DisplayRow::CardMetrics(a) if card_line::has_loading(a, self.card_graph, crate::digest_overlay::now_secs()))
                })
    }
}

/// The next multiple of `step` ms after `t0`, strictly in the future.
fn next_boundary(t0: Instant, step: u64) -> Instant {
    let elapsed = t0.elapsed().as_millis() as u64;
    t0 + Duration::from_millis((elapsed / step + 1) * step)
}
