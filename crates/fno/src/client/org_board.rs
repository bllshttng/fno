//! The org view: the fleet command center. The explorer on the left lists
//! the fleet, its machines, the leads on each machine and the open lead's seats;
//! the right pane shows a tab over the selection, or a seat, feed or system
//! page. Every seat comes from the fleet map, so the other machine's seats
//! show beside this one's.

use super::backlog_style::{BLine, BRole};
use super::org_seats::{Fleet, FleetInputs, Lead, Machine, Seat};
use super::org_tabs::{Entry, Lb, Line};
use super::*;
use crate::view_store::{OrgTab, SidelineView};

pub(crate) type OrgTx = tokio::sync::mpsc::UnboundedSender<(u64, OrgMsg)>;

pub(crate) enum OrgMsg {
    Gather(FleetInputs),
}

pub(crate) fn apply(view: &mut View, gen: u64, msg: OrgMsg) {
    match msg {
        OrgMsg::Gather(inputs) => apply_fold(view, gen, inputs),
    }
}

/// The explorer's width when the right pane shows beside it.
const LEFT: usize = 34;
/// Below this width the explorer paints alone.
const MIN_SPLIT: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Item {
    Root,
    Host(usize),
    Lead(usize, usize),
    Seat(usize, usize, usize),
    Feed,
    System,
}

pub(crate) fn host_key(m: &Machine) -> String {
    format!("host/{}", m.id)
}
pub(crate) fn lead_key(m: &Machine, p: &Lead) -> String {
    format!("lead/{}/{}", m.id, p.name)
}
pub(crate) fn seat_key(m: &Machine, p: &Lead, s: &Seat) -> String {
    format!("seat/{}/{}/{}", m.id, p.name, s.name)
}

pub(crate) struct OrgBoard {
    pub(crate) fleet: Option<Fleet>,
    /// The last failed read and when it failed; the last good fleet stays.
    pub(crate) error: Option<(String, u64)>,
    /// The selected explorer key: `root`, `host/..`, `lead/..`, `seat/..`,
    /// `feed` or `system`.
    pub(crate) key: String,
    /// The one lead whose seats the explorer lists.
    pub(crate) expanded: String,
    pub(crate) tab: OrgTab,
    /// The right pane's first body line.
    pub(crate) scroll: usize,
    pub(crate) gen: u64,
    inflight: bool,
    last_read: Option<Instant>,
    pub(crate) keys_help: bool,
    esc: Vec<u8>,
}

/// One frame's lines: the explorer windowed to the body height, and the
/// right pane's visible lines with the column it starts at.
pub(crate) struct Frame {
    pub(crate) explorer: Vec<Line>,
    pub(crate) right_x: Option<usize>,
    pub(crate) right: Vec<Line>,
}

fn window_start(len: usize, height: usize, follow: Option<usize>) -> usize {
    match follow {
        Some(f) if len > height && height > 0 => f.saturating_sub(height - 1).min(len - height),
        _ => 0,
    }
}

fn title(text: &str, width: usize) -> Line {
    let head = format!("\u{2500} {text} ");
    let rest = width.saturating_sub(head.chars().count());
    Lb::default()
        .s(head, BRole::Head)
        .s("\u{2500}".repeat(rest), BRole::Meta)
        .done()
}

impl OrgBoard {
    pub(crate) fn new(gen: u64) -> Self {
        Self {
            fleet: None,
            error: None,
            key: "root".into(),
            expanded: String::new(),
            tab: crate::view_store::load_org_tab(),
            scroll: 0,
            gen,
            inflight: false,
            last_read: None,
            keys_help: false,
            esc: Vec::new(),
        }
    }

    /// Where `key` sits in the fleet, whether or not its lead is open.
    fn locate(&self, key: &str) -> Option<Item> {
        match key {
            "root" => return Some(Item::Root),
            "feed" => return Some(Item::Feed),
            "system" => return Some(Item::System),
            _ => {}
        }
        let f = self.fleet.as_ref()?;
        for (mi, m) in f.machines.iter().enumerate() {
            if host_key(m) == key {
                return Some(Item::Host(mi));
            }
            for (pi, p) in m.leads.iter().enumerate() {
                if lead_key(m, p) == key {
                    return Some(Item::Lead(mi, pi));
                }
                for (si, s) in p.seats.iter().enumerate() {
                    if seat_key(m, p, s) == key {
                        return Some(Item::Seat(mi, pi, si));
                    }
                }
            }
        }
        None
    }

    /// The selection; a key the last read dropped falls back to the fleet.
    pub(crate) fn current(&self) -> Item {
        self.locate(&self.key).unwrap_or(Item::Root)
    }

    pub(crate) fn entries(&self) -> Vec<Entry> {
        let entry = |key: String, item, depth| Entry { key, item, depth };
        let mut out = vec![entry("root".into(), Item::Root, 0)];
        if let Some(f) = &self.fleet {
            for (mi, m) in f.machines.iter().enumerate() {
                out.push(entry(host_key(m), Item::Host(mi), 1));
                for (pi, p) in m.leads.iter().enumerate() {
                    let key = lead_key(m, p);
                    let open = key == self.expanded;
                    out.push(entry(key, Item::Lead(mi, pi), 2));
                    if open {
                        for (si, s) in p.seats.iter().enumerate() {
                            out.push(entry(seat_key(m, p, s), Item::Seat(mi, pi, si), 3));
                        }
                    }
                }
            }
        }
        out.push(entry("feed".into(), Item::Feed, 0));
        out.push(entry("system".into(), Item::System, 0));
        out
    }

    /// Select `key`. A lead opens; a seat opens its lead.
    pub(crate) fn select(&mut self, key: &str) {
        let Some(item) = self.locate(key) else {
            return;
        };
        if let (Some(f), Item::Lead(m, p) | Item::Seat(m, p, _)) = (&self.fleet, item) {
            self.expanded = lead_key(&f.machines[m], &f.machines[m].leads[p]);
        }
        self.key = key.to_string();
        self.scroll = 0;
    }

    fn step(&mut self, delta: isize) {
        let entries = self.entries();
        let at = entries.iter().position(|e| e.key == self.key).unwrap_or(0);
        let to = (at as isize + delta).clamp(0, entries.len() as isize - 1) as usize;
        let key = entries[to].key.clone();
        self.select(&key);
    }

    pub(crate) fn up(&mut self) {
        let key = match (self.current(), &self.fleet) {
            (Item::Seat(m, p, _), Some(f)) => lead_key(&f.machines[m], &f.machines[m].leads[p]),
            (Item::Lead(m, _), Some(f)) => host_key(&f.machines[m]),
            _ => "root".into(),
        };
        self.select(&key);
    }

    pub(crate) fn set_tab(&mut self, tab: OrgTab) {
        self.tab = tab;
        self.scroll = 0;
        crate::view_store::save_org_tab(tab);
    }

    pub(crate) fn seat(&self) -> Option<(&Machine, &Lead, &Seat)> {
        let (Item::Seat(m, p, s), Some(f)) = (self.current(), &self.fleet) else {
            return None;
        };
        let machine = &f.machines[m];
        let lead = &machine.leads[p];
        Some((machine, lead, &lead.seats[s]))
    }

    /// The selected seat's registry row on this mux, when it has one.
    pub(crate) fn joined(&self) -> Option<AgentRow> {
        self.seat().and_then(|(_, _, s)| s.local.clone())
    }

    /// The right pane: fixed lines (title and tab strip), then the body
    /// the scroll offset windows.
    fn right(
        &self,
        questions: Option<&crate::needs_overlay::QuestionsFold>,
        width: usize,
        now: u64,
    ) -> (Vec<Line>, Vec<Line>) {
        if self.keys_help {
            return (vec![title("Keys", width)], super::org_pages::keys_help());
        }
        let item = self.current();
        if item == Item::Feed {
            return (
                vec![title("Feed", width)],
                super::org_pages::feed(questions, width),
            );
        }
        let Some(f) = &self.fleet else {
            let state = match &self.error {
                Some((e, _)) => format!("   fleet not read: {e}"),
                None => "   reading the fleet map".into(),
            };
            return (
                vec![title("Fleet", width)],
                vec![super::org_tabs::meta(state)],
            );
        };
        match item {
            Item::System => (
                vec![title("System", width)],
                super::org_pages::system(f, width),
            ),
            Item::Seat(m, p, s) => {
                let machine = &f.machines[m];
                let lead = &machine.leads[p];
                (
                    vec![title(&lead.seats[s].name, width)],
                    super::org_pages::seat(machine, lead, &lead.seats[s], width, now),
                )
            }
            _ => {
                use super::org_tabs as t;
                let name = match item {
                    Item::Host(m) => f.machines[m].host.as_str(),
                    Item::Lead(m, p) => f.machines[m].leads[p].name.as_str(),
                    _ => "Fleet",
                };
                let rows = t::scope(f, &item);
                let body = match self.tab {
                    OrgTab::Table => {
                        t::table(&rows, &self.key, width, now, matches!(item, Item::Lead(..)))
                    }
                    OrgTab::Recent => t::recent(&rows, width, now),
                    OrgTab::Overview => t::overview(f, &item, &rows, width, now),
                    OrgTab::Graph => t::graph(f, &item, &rows, &self.key, width),
                    OrgTab::Health => t::health(f, &item, &rows, width, now),
                };
                (
                    vec![
                        title(name, width),
                        t::header(f, &item, self.tab),
                        t::blank(),
                    ],
                    body,
                )
            }
        }
    }

    pub(crate) fn frame(
        &self,
        questions: Option<&crate::needs_overlay::QuestionsFold>,
        width: usize,
        body_h: usize,
        now: u64,
    ) -> Frame {
        let split = width >= MIN_SPLIT;
        let explorer_w = if split { LEFT } else { width };
        let explorer = super::org_tabs::explorer(
            self.fleet.as_ref(),
            &self.entries(),
            &self.key,
            &self.expanded,
            questions.map(|q| q.items.len()),
            explorer_w,
        );
        let follow = explorer.iter().position(|l| l.line.band);
        let start = window_start(explorer.len(), body_h, follow);
        let explorer: Vec<Line> = explorer.into_iter().skip(start).take(body_h).collect();
        if !split {
            return Frame {
                explorer,
                right_x: None,
                right: Vec::new(),
            };
        }
        let (mut right, body) = self.right(questions, width - LEFT - 1, now);
        let avail = body_h.saturating_sub(right.len());
        let start = self.scroll.min(body.len().saturating_sub(avail));
        right.extend(body.into_iter().skip(start).take(avail));
        Frame {
            explorer,
            right_x: Some(LEFT + 1),
            right,
        }
    }

    /// `fleet → bb16 → rowan → scout`.
    fn crumb(&self) -> String {
        let item = self.current();
        let Some(f) = &self.fleet else {
            return "fleet".into();
        };
        match item {
            Item::Feed => "feed".into(),
            Item::System => "system \u{b7} machines".into(),
            Item::Root => "fleet".into(),
            Item::Host(m) => format!("fleet \u{2192} {}", f.machines[m].host),
            Item::Lead(m, p) => format!(
                "fleet \u{2192} {} \u{2192} {}",
                f.machines[m].host, f.machines[m].leads[p].name
            ),
            Item::Seat(m, p, s) => format!(
                "fleet \u{2192} {} \u{2192} {} \u{2192} {}",
                f.machines[m].host,
                f.machines[m].leads[p].name,
                f.machines[m].leads[p].seats[s].name
            ),
        }
    }

    /// The bottom bar as one string: [`Self::footer_parts`] joined.
    #[cfg(test)]
    pub(crate) fn footer(&self, focused: bool, width: usize, now: u64) -> String {
        let (keys, state) = self.footer_parts(focused, width, now);
        format!("{keys}{state}")
    }

    /// The key bar, then where the cursor is and the read state. The keys
    /// lead and paint in body text, as every other fno view's key bar does.
    pub(crate) fn footer_parts(&self, focused: bool, width: usize, now: u64) -> (String, String) {
        let item = self.current();
        let mut keys = vec!["j/k move"];
        if width >= MIN_SPLIT {
            keys.push("h/l tab");
        }
        match item {
            Item::Seat(..) if self.joined().is_some() => {
                keys.extend(["enter attach", "space peek", "x stop", "P portal"])
            }
            Item::Root | Item::Host(_) | Item::Lead(..) => keys.push("enter open"),
            _ => {}
        }
        if matches!(item, Item::Host(_) | Item::Lead(..) | Item::Seat(..)) {
            keys.push("u up");
        }
        keys.extend(["F full", "? keys", "esc close"]);
        let mut keys = keys.join(" \u{b7} ");
        if !focused {
            keys = format!("tap to focus \u{b7} {keys}");
        }
        let mut out = format!("  \u{b7}  {}", self.crumb());
        if width < MIN_SPLIT {
            out.push_str(" \u{b7} F full screen for tabs");
        }
        if let Some(notice) = self.fleet.as_ref().and_then(|f| f.notice.as_deref()) {
            out.push_str(&format!(" \u{b7} {notice}"));
        }
        match (&self.error, &self.fleet) {
            (Some((e, at)), _) => out.push_str(&format!(
                " \u{b7} fleet not read: {e} (failed {}s ago)",
                now.saturating_sub(*at)
            )),
            (None, Some(f)) => out.push_str(&format!(
                " \u{b7} read {}s ago",
                now.saturating_sub(f.measured_at)
            )),
            (None, None) if self.inflight => out.push_str(" \u{b7} reading fleet"),
            (None, None) => out.push_str(" \u{b7} fleet not read"),
        }
        (keys, out)
    }

    /// The key a tap at (row, col) of the board's rect selects.
    fn tap(
        &self,
        questions: Option<&crate::needs_overlay::QuestionsFold>,
        width: usize,
        height: usize,
        row: usize,
        col: usize,
    ) -> Option<String> {
        let body_h = height.saturating_sub(1);
        if row >= body_h {
            return None;
        }
        let frame = self.frame(questions, width, body_h, now());
        let line = match frame.right_x {
            Some(x) if col >= x => {
                return frame.right.get(row)?.tap_at(col - x).map(str::to_string)
            }
            Some(x) if col + 1 == x => return None,
            _ => frame.explorer.get(row)?,
        };
        line.tap_at(col).map(str::to_string)
    }
}

fn now() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}

pub(crate) fn open(view: &mut View) {
    if !view.experimental_org_board {
        view.set_notice("fleet org board is off: menu > settings > experimental".into());
        return;
    }
    view.org_generation = view.org_generation.wrapping_add(1);
    view.org_board = Some(OrgBoard::new(view.org_generation));
    view.backlog_board = None;
    view.region_owner = super::region_focus::RegionOwner::Board;
    super::backlog_board::set_sideline_view(view, SidelineView::Org);
}
pub(crate) fn restore(view: &mut View) {
    match view.sideline_view {
        SidelineView::Org if view.experimental_org_board => open(view),
        SidelineView::Org => super::backlog_board::set_sideline_view(view, SidelineView::Agents),
        SidelineView::Backlog => View::open(view),
        SidelineView::Agents => {}
        SidelineView::Messages => {} // restored by the caller's messages arm
    }
}
pub(crate) fn maybe_kick(view: &mut View, tx: &OrgTx) {
    let Some(b) = view.org_board.as_mut() else {
        return;
    };
    if b.inflight
        || b.last_read
            .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
    {
        return;
    }
    b.inflight = true;
    b.last_read = Some(Instant::now());
    let gen = b.gen;
    let agents = view.layout.agents.clone();
    let tx = tx.clone();
    tokio::spawn(async move {
        let inputs = super::org_seats::gather(&crate::backlog_view::graph_path(), agents).await;
        let _ = tx.send((gen, OrgMsg::Gather(inputs)));
    });
}
/// A failed read, or a primary error after a good read, keeps the last good
/// fleet and records the failure for the bottom bar. With no good read yet,
/// fleet top's own local fallback paints.
pub(crate) fn apply_fold(view: &mut View, gen: u64, inputs: FleetInputs) {
    let Some(b) = view.org_board.as_mut().filter(|b| b.gen == gen) else {
        return;
    };
    b.inflight = false;
    let at = inputs.measured_at;
    match inputs.fleet {
        Err(e) => b.error = Some((e, at)),
        Ok(map) => {
            let primary = map
                .get("error")
                .and_then(serde_json::Value::as_str)
                .map(|e| format!("primary unreadable: {e}"));
            match primary {
                Some(e) if b.fleet.is_some() => b.error = Some((e, at)),
                _ => {
                    let agents = inputs.backlog.agents.clone();
                    b.fleet = Some(super::org_seats::fold(&map, &inputs.backlog, &agents, at));
                    b.error = None;
                }
            }
        }
    }
}
pub(crate) fn paint(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    width: usize,
    height: usize,
) {
    let Some(b) = &view.org_board else {
        return;
    };
    let width = width.min(cols);
    let height = height.min(rows);
    if width == 0 || height == 0 {
        return;
    }
    for r in 0..height {
        for c in 0..width {
            cells[r * cols + c] = Cell::default();
        }
    }
    let body_h = height - 1;
    let now = now();
    let frame = b.frame(view.questions_fold.as_ref(), width, body_h, now);
    let explorer_w = frame.right_x.map_or(width, |x| x - 1);
    let lines: Vec<BLine> = frame.explorer.into_iter().map(|l| l.line).collect();
    super::backlog_style::paint_panel_at(
        cells,
        rows,
        cols,
        0,
        0,
        explorer_w,
        body_h,
        &lines,
        None,
        &view.theme,
    );
    if let Some(x) = frame.right_x {
        let divider = vec![BLine::meta("\u{2502}"); body_h];
        super::backlog_style::paint_panel_at(
            cells,
            rows,
            cols,
            x - 1,
            0,
            1,
            body_h,
            &divider,
            None,
            &view.theme,
        );
        let lines: Vec<BLine> = frame.right.into_iter().map(|l| l.line).collect();
        super::backlog_style::paint_panel_at(
            cells,
            rows,
            cols,
            x,
            0,
            width - x,
            body_h,
            &lines,
            None,
            &view.theme,
        );
    }
    let focused = matches!(view.input_owner(), super::region_focus::RegionOwner::Board);
    super::backlog_style::paint_panel(
        cells,
        rows,
        cols,
        body_h,
        width,
        1,
        &[{
            let (keys, state) = b.footer_parts(focused, width, now);
            Lb::default()
                .s(keys, BRole::Body)
                .s(state, BRole::Meta)
                .done()
                .line
        }],
        None,
        &view.theme,
    );
}

pub(crate) async fn route_keys(
    view: &mut View,
    scanner: &mut Scanner,
    bytes: &[u8],
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    scanner.disarm_repeat();
    for event in scanner.scan(bytes, Instant::now()) {
        match event {
            Event::Forward(chunk) => {
                keys(view, &chunk, sock).await?;
            }
            event => match dispatch_event(view, event, sock).await? {
                DispatchFlow::Continue => {}
                DispatchFlow::Break => break,
                DispatchFlow::Detach => return Ok(StdinFlow::Detach),
            },
        }
    }
    Ok(StdinFlow::Continue)
}
pub(crate) async fn keys(
    view: &mut View,
    bytes: &[u8],
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<StdinFlow, String> {
    let Some(b) = view.org_board.as_mut() else {
        return Ok(StdinFlow::Continue);
    };
    let tokens = if bytes == [27] && b.esc.is_empty() {
        vec![ModalKey::Esc]
    } else {
        fold_modal_keys(&mut b.esc, bytes)
    };
    for token in tokens {
        let Some(b) = view.org_board.as_mut() else {
            break;
        };
        match token {
            ModalKey::Esc | ModalKey::Byte(b'q') => {
                if b.keys_help {
                    b.keys_help = false;
                } else {
                    view.org_board = None;
                    super::backlog_board::set_sideline_view(view, SidelineView::Agents);
                }
            }
            ModalKey::Byte(b'V') => super::backlog_board::cycle_sideline_view(view),
            ModalKey::Byte(b'F') => {
                view.board_full = !view.board_full;
                crate::view_store::save_board_full(view.board_full);
            }
            ModalKey::Byte(b'?') => b.keys_help = !b.keys_help,
            ModalKey::Up | ModalKey::Byte(b'k') => b.step(-1),
            ModalKey::Down | ModalKey::Byte(b'j') => b.step(1),
            ModalKey::Left | ModalKey::Byte(b'h') => b.set_tab(b.tab.step(false)),
            ModalKey::Right | ModalKey::Byte(b'l') => b.set_tab(b.tab.step(true)),
            ModalKey::Byte(n @ b'1'..=b'5') => b.set_tab(OrgTab::ALL[(n - b'1') as usize]),
            ModalKey::Byte(b'u' | 127 | 8) => b.up(),
            ModalKey::PageUp => b.scroll = b.scroll.saturating_sub(12),
            ModalKey::PageDown => b.scroll = b.scroll.saturating_add(12),
            ModalKey::Enter => match b.current() {
                Item::Seat(..) => {
                    let remote = b
                        .seat()
                        .map(|(m, _, s)| {
                            format!(
                                "{} runs on {}: {}",
                                s.name,
                                m.host,
                                super::org_pages::attach_command(s)
                            )
                        })
                        .unwrap_or_default();
                    match b.joined() {
                        Some(agent) => dispatch(view, agent, b'\r', sock).await?,
                        None => view.set_notice(remote),
                    }
                }
                Item::Feed | Item::System => {}
                _ => b.step(1),
            },
            ModalKey::Byte(key @ (b' ' | b'x' | b'P')) => {
                if let Some(agent) = b.joined() {
                    dispatch(view, agent, key, sock).await?;
                }
            }
            _ => {}
        }
    }
    Ok(StdinFlow::Continue)
}
/// Enter attaches, space peeks, `x` stops and `P` opens the portal picker.
/// The callers pass only a seat joined to this mux's registry by its full
/// session id, so no key ever acts on a seat by name.
pub(crate) async fn dispatch(
    view: &mut View,
    agent: AgentRow,
    key: u8,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let sid = agent.harness_session_id.clone();
    let harness = agent.harness.clone();
    if key == b'\r' {
        return super::node_detail::press_session(
            view,
            sid.as_deref(),
            harness.as_deref(),
            Some(&agent),
            sock,
        )
        .await;
    }
    let Some(agent) =
        super::node_detail::resolve_session(view, sid.as_deref(), harness.as_deref(), Some(&agent))
    else {
        view.set_notice("no registry row".into());
        return Ok(());
    };
    let action = match key {
        b' ' => MenuAction::Peek,
        b'x' => MenuAction::Stop,
        b'P' => MenuAction::PortalPicker,
        _ => return Ok(()),
    };
    super::row_menu::execute_row_menu_action(
        view,
        action,
        MenuTarget::Agent(AgentIdent::of(&agent)),
        sock,
    )
    .await
}

#[cfg(test)]
#[path = "tests/org_board_fixture.rs"]
mod fixtures;

pub(crate) async fn mouse(
    view: &mut View,
    rep: crate::mouse::MouseReport,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let width = if view.board_full {
        view.term.1 as usize
    } else {
        view.panel_w().saturating_sub(1) as usize
    };
    let height = (view.term.0 as usize).saturating_sub(if view.board_full {
        1
    } else {
        1 + view.bottom_row_is_chrome() as usize
    });
    if !matches!(rep.kind, MouseKind::Press(MouseButton::Left)) {
        return Ok(());
    }
    view.region_owner = super::region_focus::RegionOwner::Board;
    let Some(key) = view.org_board.as_ref().and_then(|b| {
        b.tap(
            view.questions_fold.as_ref(),
            width,
            height,
            rep.row as usize,
            rep.col as usize,
        )
    }) else {
        return Ok(());
    };
    let Some(b) = view.org_board.as_mut() else {
        return Ok(());
    };
    if let Some(n) = key
        .strip_prefix("tab/")
        .and_then(|n| n.parse::<usize>().ok())
    {
        if let Some(tab) = OrgTab::ALL.get(n) {
            b.set_tab(*tab);
        }
        return Ok(());
    }
    // A tap selects; a tap on the seat already selected acts like Enter.
    if key != b.key {
        b.select(&key);
        return Ok(());
    }
    if let Some(agent) = b.joined() {
        dispatch(view, agent, b'\r', sock).await?;
    }
    Ok(())
}
