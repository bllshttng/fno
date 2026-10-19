//! Auto-reflow a tab's leaves to a template on pane count change - moved out
//! of server.rs (file budget shrink). Parent helpers resolve through the glob.
use super::*;
use crate::proto::TemplateName;

impl Core {
    /// Auto-reflow one tab's EXISTING leaves (no spawn, no rebind) to
    /// `template`: realize its topology for the live leaf count and
    /// substitute real pane ids back in via [`substitute_leaves`], skipping
    /// `SlotBinding`/`resolve_local_pane` since every leaf is already a live
    /// `PaneId`. The caller picks `template` via
    /// [`crate::templates::pick_template`]. Pushes no layout itself: every
    /// `maybe_reflow` call site already issues its own `push_layout` right
    /// after the mutation it just made.
    pub(super) fn reflow_tab(
        &mut self,
        sid: u64,
        tid: TabId,
        template: TemplateName,
    ) -> Result<(), String> {
        let ti = self.tab_index_by_id(sid, tid);
        let leaf_ids = {
            let tab = &self.session.squad(sid).expect("squad live").tabs[ti];
            tree::leaves(&tab.root)
        };
        let shape = crate::templates::topology(template, leaf_ids.len())
            .map_err(|e| format!("reflow template arity: {e}"))?;
        let vp = self.tab_rect(tid);
        let overflow = tree::layout(&shape, vp)
            .into_iter()
            .any(|(_, r)| r.rows < tree::MIN_ROWS || r.cols < tree::MIN_COLS);
        if overflow {
            // Too small to reflow: leave the tab's current shape alone rather
            // than forcing an unfittable one (same posture as a `layout apply`
            // that does not fit, but silent - this is an automatic background
            // reshape, not an operator-requested one to report a refusal for).
            return Ok(());
        }
        let map: std::collections::HashMap<u64, u64> = leaf_ids
            .iter()
            .enumerate()
            .map(|(slot, pane)| (slot as u64, *pane))
            .collect();
        let new_root = substitute_leaves(&shape, &map);
        let si = self
            .session
            .squads
            .iter()
            .position(|s| s.id == sid)
            .expect("squad live");
        {
            let tab = &mut self.session.squads[si].tabs[ti];
            tab.root = new_root;
            if let Err(e) = tree::check_invariants(tab) {
                e2e_log(format_args!("reflow_tab produced an invalid tree: {e}"));
            }
        }
        Ok(())
    }

    /// Called by every tree-mutation site after it adds or removes a tab
    /// leaf: picks a template for the NEW leaf count and reflows to it. A
    /// count outside 2/3/4, or an unfittable viewport, is a silent no-op
    /// ([`crate::templates::pick_template`] returns `None`;
    /// [`Self::reflow_tab`] returns `Ok(())` on overflow).
    pub(super) fn maybe_reflow(&mut self, sid: u64, tid: TabId) {
        let leaf_count = {
            let ti = self.tab_index_by_id(sid, tid);
            let Some(sq) = self.session.squad(sid) else {
                return;
            };
            tree::leaves(&sq.tabs[ti].root).len()
        };
        let vp = self.tab_rect(tid);
        if let Some(template) = crate::templates::pick_template(leaf_count, vp) {
            if let Err(e) = self.reflow_tab(sid, tid, template) {
                e2e_log(format_args!("maybe_reflow({tid:?}) declined: {e}"));
            }
        }
    }
}
