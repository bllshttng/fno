//! The navigator's catalog (prefix+f): the flat global find rows, local plus
//! remote. Moved out of client.rs under the file-budget ratchet; the module is
//! named by the question it answers: which rows does prefix+f list?

use super::*;

impl View {
    /// The navigator's flat GLOBAL catalog: one [`NavRow`] per squad, per
    /// tab (ignoring expand state - a collapsed squad's tabs still appear,
    /// the key difference from [`display_rows`]), per plain pane (v22: those
    /// NOT already shown as an agent row), per agent across the WHOLE
    /// session, and one row per remote seat while the machine layer is
    /// layered. Shares the agent -> [`ChromeHit`] mapping with [`row_action`]
    /// (via [`agent_hit`]) so a keyboard goto and a mouse click never
    /// diverge. Squad/tab rows carry their own SelectSquad/SelectTab in
    /// `hit`; an agent row carries a `goto_squad` prefix (its pane lives in
    /// another squad). The `+ new workspace` footer is omitted - the
    /// navigator is a goto-existing picker (Discretion 4). Fully owned (no
    /// layout borrow) so goto can mutate the view after building it.
    pub(super) fn nav_rows(&self) -> Vec<NavRow> {
        let mut out = Vec::new();
        // One session, one row: a seat twin (the portal pane a reach names
        // `REMOTE_GLYPH name@host`) folds into its remote row and never
        // paints here, so `mercedes` lists the session once.
        let twin_names: std::collections::HashSet<String> = self
            .machine_tree
            .rows()
            .map(|r| super::machine_tree::twin_label(&r.name))
            .collect();
        let cross = |sq: u64| (sq != self.layout.active_squad).then_some(sq);
        for s in &self.layout.squads {
            out.push(
                NavRow::new(
                    s.name.clone(),
                    PaneState::Idle,
                    None,
                    None,
                    ChromeHit::Cmds(vec![Command::SelectSquad(s.id)]),
                    &[],
                )
                .with_menu(row_menu::NavMenu::Squad(s.id)),
            );
            for (t, tab) in s.tabs.iter().enumerate() {
                let tab_text = tab_label_text(&tab.name, t, tab.named);
                out.push(
                    NavRow::new(
                        format!("{} › {}", s.name, tab_text),
                        PaneState::Idle,
                        None,
                        None,
                        ChromeHit::Cmds(vec![Command::SelectTab(tab.id)]),
                        std::slice::from_ref(&s.name),
                    )
                    .with_menu(row_menu::NavMenu::Tab(tab.id)),
                );
                // Plain panes of the tab (v22): a pane already shown as an agent
                // row is skipped (the agent row is the richer view of the same
                // pane); the rest become goto-able so a bare shell pane in any
                // tab/squad is reachable, not just the active view (codex review).
                for p in &tab.panes {
                    if self.layout.agents.iter().any(|a| a.pane_id == Some(p.id)) {
                        continue;
                    }
                    out.push(NavRow::new(
                        format!("{} › {} › {}", s.name, tab_text, p.label),
                        PaneState::Idle,
                        cross(s.id),
                        Some(tab.id),
                        ChromeHit::Cmds(vec![Command::FocusPane(p.id)]),
                        &[p.id.to_string(), s.name.clone()],
                    ));
                }
            }
            // One session, one row: the seat twin folds into the
            // remote row appended below, so an open portal's pane never
            // paints twice in the catalog.
            for a in self
                .layout
                .agents
                .iter()
                .filter(|a| a.squad == Some(s.id) && !twin_names.contains(&a.name))
            {
                let label = match self.agent_tab_context(a.squad, a.tab) {
                    Some(TabContext::Named(name)) => format!("{} › {}", a.name, name),
                    Some(TabContext::Ordinal(n)) => format!("{} › {} ·{n}", s.name, a.name),
                    None => format!("{} › {}", s.name, a.name),
                };
                out.push(
                    NavRow::new(
                        label,
                        nav_agent_state(a),
                        cross(s.id),
                        None,
                        agent_hit(a, self.layout.active_squad),
                        &nav_agent_tokens(a, Some(&s.name)),
                    )
                    .with_menu(row_menu::NavMenu::Agent(AgentIdent::of(a))),
                );
            }
        }
        // Orphan agents (no live squad), mirroring display_rows' orphan section.
        for a in self.layout.agents.iter().filter(|a| {
            let orphan =
                !matches!(a.squad, Some(id) if self.layout.squads.iter().any(|s| s.id == id));
            orphan && !twin_names.contains(&a.name)
        }) {
            out.push(orphan_row(a, self.layout.active_squad));
        }
        // One row per remote seat, hit through the same agent_hit
        // a local row takes; Enter opens its portal (OpenRemote).
        for row in self.machine_tree.rows() {
            let remote = match &row.remote {
                Some(r) => r,
                None => continue,
            };
            let mut tokens = nav_agent_tokens(row, None);
            tokens.push(remote.host.clone());
            out.push(
                NavRow::new(
                    row.name.clone(),
                    nav_agent_state(row),
                    None,
                    None,
                    agent_hit(row, self.layout.active_squad),
                    &tokens,
                )
                .with_menu(row_menu::NavMenu::Agent(AgentIdent::of(row))),
            );
        }
        out
    }
}

fn orphan_row(a: &AgentRow, active_squad: u64) -> NavRow {
    NavRow::new(
        a.name.clone(),
        nav_agent_state(a),
        None,
        None,
        agent_hit(a, active_squad),
        &nav_agent_tokens(a, None),
    )
    .with_menu(row_menu::NavMenu::Agent(AgentIdent::of(a)))
}
