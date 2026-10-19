//! Exact-anchor placement (`--at`) - moved out of server.rs (file budget
//! shrink). Parent helpers resolve through the glob.
use super::*;

impl Core {
    pub(super) fn place_strict_at_anchor(
        &mut self,
        pid: u64,
        placement: &PanePlacement,
    ) -> Result<(u64, TabId, bool), (u32, String)> {
        let anchor = placement.at.expect("strict placement carries an anchor");
        let dir = placement.split.unwrap_or(Dir::Down);
        let (sid, ti) = match self.session.find_pane(anchor) {
            Some(loc) => loc,
            None => {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} no longer exists"),
                ));
            }
        };
        // An explicit workspace/tab selector must name the anchor's actual
        // location; anything else is refused rather than redirecting placement.
        if !matches!(placement.target, PaneTarget::CurrentRoute) {
            let ok = matches!(self.resolve_placement_target(&placement.target, None), Ok(s) if s == Some(sid));
            if !ok {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} is not in the requested workspace"),
                ));
            }
        }
        if let Some(sel) = placement.tab.as_ref() {
            if matches!(sel, TabSel::New) {
                // Strict placement pins the anchor's tab and never mints one;
                // `--tab new` asks for the opposite, so the combination is
                // refused rather than silently dropping the new-tab request.
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    "exact placement cannot combine --at with a new-tab selector".into(),
                ));
            }
            let ok = matches!(self.resolve_tab_index(sid, sel), Ok(rti) if rti == ti);
            if !ok {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!("anchor pane {anchor} is not in the requested tab"),
                ));
            }
        }
        if let Some(cap) = placement.max_panes {
            let pane_count = tree::leaves(
                &self
                    .session
                    .squad(sid)
                    .expect("find_pane returned a live squad")
                    .tabs[ti]
                    .root,
            )
            .len();
            if pane_count >= cap {
                self.reap_pane(pid);
                return Err((
                    err_code::BAD_REQUEST,
                    format!(
                        "exact placement refused: target tab has {pane_count} panes and the cap is {cap}. Use --workspace <name> to place this worker elsewhere"
                    ),
                ));
            }
        }
        let tid = self
            .session
            .squad(sid)
            .expect("find_pane returned a live squad")
            .tabs[ti]
            .id;
        let vp = self.tab_rect(tid);
        let res = {
            let tab = &mut self
                .session
                .squad_mut(sid)
                .expect("find_pane returned a live squad")
                .tabs[ti];
            tree::split_at(tab, vp, anchor, dir, pid)
        };
        match res {
            // No auto-reflow: strict placement commits the exact split it
            // was asked for, and a reflow would re-tile it and make room a
            // second strict spawn must be refused.
            Ok(()) => Ok((sid, tid, false)),
            Err(tree::SplitError::TooSmall { .. }) => {
                self.reap_pane(pid);
                Err((
                    err_code::BAD_REQUEST,
                    "exact placement cannot fit: a resulting pane would be below the minimum size"
                        .into(),
                ))
            }
            Err(e) => {
                self.reap_pane(pid);
                Err((err_code::BAD_REQUEST, e.to_string()))
            }
        }
    }
}
