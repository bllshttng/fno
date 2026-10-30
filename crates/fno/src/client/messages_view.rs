//! The Messages tab: three columns over the full terminal - the flat chat
//! list of agents and channels, the selected agent's chats, and the
//! selected conversation as a two-sided bubble thread. The model is the
//! `mail-threads` projection; painting runs through the sideline's BLine
//! path; keys and mouse mirror org_board. The strip row
//! (`Agents  Messages`) is the sideline's, not this module's.

use super::backlog_style::{BLine, BRole, BSeg};
use super::*;
use crate::messages_model::MessagesSnapshot;
use crate::org_model::OrgTree;
use crate::view_store::SidelineView;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_MESSAGES_GEN: AtomicU64 = AtomicU64::new(0);

fn seg(text: impl Into<String>, role: BRole) -> BSeg {
    BSeg {
        text: text.into(),
        role,
    }
}

/// Push one spacer line unless the paint already ends in one (item 12:
/// blanks never stack).
/// One tab cell of a strip: the label padded one space each side, so the
/// active cell paints as a full inverted block (the active mux tab's look).
/// Inactive cells keep the same width, so the click spans never move.
fn tab_seg(label: &str, active: bool, focused: bool) -> BSeg {
    let role = if active {
        BRole::Tab
    } else if focused {
        BRole::Body
    } else {
        BRole::Meta
    };
    seg(format!(" {label} "), role)
}

/// The cell span each strip tab owns, from the labels in paint order and
/// the separator width between them. The click map reads the same spans.
fn tab_spans(start: usize, labels: &[&str], sep: usize) -> Vec<std::ops::Range<usize>> {
    let mut at = start;
    labels
        .iter()
        .map(|l| {
            let span = at..at + l.chars().count() + 2;
            at = span.end + sep;
            span
        })
        .collect()
}

/// The band pair for a column's cursor row: the terminal's own pair,
/// swapped, when the column holds focus; a dim swap (gray ground) when
/// focus is elsewhere, so the selection stays visible but reads secondary.
fn column_band(focused: bool, theme: &crate::theme::Theme) -> (Color, Color, u8) {
    use crate::proto::cell_flags::INVERSE;
    if focused {
        (Color::Default, Color::Default, INVERSE)
    } else {
        (crate::theme::dim_fg(theme), Color::Default, INVERSE)
    }
}

/// A message body as left-aligned lines inside one box: each stored line
/// wraps on its own, indented under the sender label. A raw `\n` in a
/// painted cell moved the terminal cursor and overwrote the next row, so
/// no output line ever carries one. The harness's `<pasted_content>` wrapper
/// lines are transport, not words the user typed, so they never paint.
fn body_lines(text: &str, w: usize, role: BRole) -> Vec<BLine> {
    let inner = w.saturating_sub(BODY_INDENT.len()).max(1);
    let mut out = Vec::new();
    for para in text
        .replace("\r\n", "\n")
        .replace('\t', "    ")
        .split(['\n', '\r'])
    {
        let tag = para.trim();
        if (tag.starts_with("<pasted_content") || tag.starts_with("</pasted_content"))
            && tag.ends_with('>')
        {
            continue;
        }
        for mut line in BLine::of(&[seg(para, role)]).wrap(inner) {
            line.text.insert_str(0, BODY_INDENT);
            line.roles.splice(0..0, [role; BODY_INDENT.len()]);
            out.push(line);
        }
    }
    out
}

const BODY_INDENT: &str = "  ";

/// The width a right-side bubble wraps to, so it reads as its own column.
fn bubble_w(w: usize) -> usize {
    ((w * 7) / 10).max(12).min(w).max(1)
}

/// The time under a burst's last message, on that message's side.
fn time_line(ts_time: String, right: bool, w: usize) -> BLine {
    let mut line = BLine::meta(ts_time);
    if right {
        flush_right(std::slice::from_mut(&mut line), w);
    }
    line
}

/// The reader's own side: each line, trimmed, ends at the pane's right
/// edge.
fn flush_right(block: &mut [BLine], w: usize) {
    for line in block {
        let start = line.text.len() - line.text.trim_start().len();
        // A blank body line trims to nothing; its end never precedes start.
        let end = line.text.trim_end().len().max(start);
        let (lead, kept) = (
            line.text[..start].chars().count(),
            line.text[start..end].chars().count(),
        );
        line.text = line.text[start..end].to_string();
        line.roles = line.roles.iter().skip(lead).take(kept).copied().collect();
        let used: usize = line.text.chars().map(super::backlog_style::char_w).sum();
        let pad = w.saturating_sub(used);
        line.text.insert_str(0, &" ".repeat(pad));
        line.roles
            .splice(0..0, std::iter::repeat_n(BRole::Body, pad));
    }
}

/// True for a participant name that is only a session id: a UUID or its
/// 8-hex handle, never a registry name.
fn is_raw_id(name: &str) -> bool {
    let head: String = name.chars().take(8).collect();
    head.len() == 8
        && head.chars().all(|c| c.is_ascii_hexdigit())
        && name.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// The column-1 label for a participant: the registry name, else the
/// harness plus the 8-char handle for an unnamed session. `None` hides the
/// row: the `unknown` placeholder and the operator's own `user` voice,
/// which the You row already holds.
fn agent_label(p: &Value) -> Option<String> {
    let name = text_of(p, "name");
    let name = if name.is_empty() {
        text_of(p, "key")
    } else {
        name
    };
    if name.is_empty() || matches!(name, "unknown" | "user") {
        return None;
    }
    if !is_raw_id(name) {
        return Some(name.to_string());
    }
    let handle: String = name.chars().take(8).collect();
    match text_of(p, "harness") {
        "" => Some(handle),
        harness => Some(format!("{harness} {handle}")),
    }
}

fn push_blank(lines: &mut Vec<BLine>, owners: &mut Vec<Option<usize>>) {
    if lines.last().is_some_and(|l| l.text.is_empty()) {
        return;
    }
    lines.push(BLine::meta(""));
    owners.push(None);
}

/// Close a burst: its last message's HH:MM, on that message's side, at
/// the body's right edge (`w` less the two-column margin).
fn push_time(
    lines: &mut Vec<BLine>,
    owners: &mut Vec<Option<usize>>,
    ts: &str,
    right: bool,
    w: usize,
) {
    let time = MessagesBoard::ts_time(ts);
    if !time.is_empty() {
        lines.push(time_line(time, right, w.saturating_sub(2).max(1)));
        owners.push(None);
    }
}

/// A JSON bool the reader trusts; the projection carries real booleans.
fn bool_of(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The channel row's display word: the retired `leads` scope reads as
/// `leads` (item 5: no lead anymore), everything else as itself, never
/// with a `#`.
fn channel_label(scope: &str) -> String {
    if scope == "leads" {
        "leads".to_string()
    } else {
        scope.to_string()
    }
}

/// The display label for a sender or system arm: the retired `lead-settle`
/// stamp reads `lead-settle` (item 5). A display rename only - the mail
/// identity, and every stored row, still say `fno/lead-settle`.
fn display_label(name: &str) -> String {
    name.replace("lead-settle", "lead-settle")
}

pub(crate) type MessagesTx =
    tokio::sync::mpsc::UnboundedSender<(u64, Result<Value, String>, Result<OrgTree, String>)>;

/// Which column owns the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Col {
    Tree,
    Chats,
    Thread,
}

impl Col {
    /// Tab's column step.
    fn next(self) -> Self {
        match self {
            Col::Tree => Col::Chats,
            Col::Chats => Col::Thread,
            Col::Thread => Col::Tree,
        }
    }
    /// Esc's step back (item 8): Thread -> Chats -> Tree -> close.
    fn prev(self) -> Self {
        match self {
            Col::Tree => Col::Thread,
            Col::Chats => Col::Tree,
            Col::Thread => Col::Chats,
        }
    }
}

/// Column 2's tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChatTab {
    Chats,
    System,
    Archive,
}

/// Column 1's tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListTab {
    Agents,
    Archive,
}

/// Column 1's filter row (item 5): the agent list, or the broadcast
/// groups. The groups read as filters only from here, never as unlabeled
/// rows among the agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ListFilter {
    All,
    Broadcasts,
}

/// Column 1's sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SortMode {
    Last,
    Alpha,
}

pub(crate) struct MessagesBoard {
    pub(crate) snapshot: MessagesSnapshot,
    pub(crate) col: Col,
    pub(crate) cursors: [usize; 3],
    pub(crate) sel_agent: Option<String>,
    pub(crate) sel_thread: Option<String>,
    pub(crate) col2: ChatTab,
    pub(crate) list_tab: ListTab,
    pub(crate) filter: ListFilter,
    pub(crate) sort_mode: SortMode,
    pub(crate) pending_message_id: Option<String>,
    /// A clicked `@handle` awaiting the gather: resolves to the participant
    /// whose name (or key) matches, then selects it in column 1.
    pub(crate) pending_handle: Option<String>,
    selected_message_id: Option<String>,
    pub(crate) detail: Option<super::messages_detail::SessionDetail>,
    pub(super) reply: Option<super::messages_reply::ReplyState>,
    pub(crate) gen: u64,
    pub(crate) inflight: bool,
    /// The store fingerprint at the last kick: a changed fingerprint is the
    /// only reason to re-read (item 1).
    pub(crate) last_fingerprint: Option<(usize, u64, u64)>,
    /// The stat poll's own gate: the fingerprint walk runs at most every 5s.
    pub(crate) last_poll: Option<Instant>,
    esc: Vec<u8>,
}

impl MessagesBoard {
    pub(crate) fn new(gen: u64) -> Self {
        Self {
            // The cached last read paints at once; the one baseline read
            // refreshes behind it (stale-while-revalidate, item 1).
            snapshot: crate::messages_model::MessagesSnapshot::with_cache(),
            col: Col::Tree,
            cursors: [0; 3],
            sel_agent: None,
            sel_thread: None,
            col2: ChatTab::Chats,
            list_tab: ListTab::Agents,
            filter: ListFilter::All,
            sort_mode: SortMode::Last,
            pending_message_id: None,
            pending_handle: None,
            selected_message_id: None,
            detail: None,
            reply: None,
            gen,
            inflight: false,
            last_fingerprint: None,
            last_poll: None,
            esc: Vec::new(),
        }
    }

    /// The active column's cursor, as an index into that column's rows.
    fn cur(&self) -> usize {
        self.cursors[self.col as usize]
    }
    fn set_cur(&mut self, v: usize) {
        self.cursors[self.col as usize] = v;
    }
    fn step(&mut self, down: bool, len: usize) {
        if len == 0 {
            self.set_cur(0);
            return;
        }
        self.set_cur(if down {
            (self.cur() + 1).min(len - 1)
        } else {
            self.cur().saturating_sub(1)
        });
    }
}

/// Column 1's rows, derived fresh per paint from the projection: a flat
/// chat list, no tree.
#[derive(Debug)]
pub(crate) enum TreeRow {
    /// A broadcast group row (Broadcasts filter), by its scope.
    Channel(String),
    /// One mail participant, live or ended, by name (never a raw session
    /// id: the projection resolves the name from the id).
    Agent {
        key: String,
        name: String,
        live: bool,
    },
}

/// Column 2's rows for the selected agent, per its active tab.
#[derive(Debug)]
pub(crate) enum ChatRow {
    /// A system-sender entry: the arm's canonical name, opening the
    /// aggregate System view.
    SystemEntry { arm: String },
    /// A chat conversation: who, the last summary, unread.
    Thread {
        chat_id: String,
        partner: String,
        partner_key: String,
        last: String,
        unread: bool,
        /// A message this agent sent is parked or failed: it waits on the sender.
        stuck: bool,
        ts: String,
    },
    /// The Me row: the user's own thread with the selected agent, read from
    /// the session transcript's typed turns. It heads the Chats tab.
    YouThread {
        session_id: String,
        name: String,
        last_ts: String,
        unread: bool,
    },
}

fn text_of<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

impl MessagesBoard {
    fn projection(&self) -> Option<&Value> {
        self.snapshot.projection.as_ref()
    }

    /// True when an ended participant was reaped into a scope's Archive
    /// (the R3 landing): an `archive_scope` on the projection row.
    fn reaped(&self, p: &Value) -> bool {
        !bool_of(p, "live") && !text_of(p, "archive_scope").is_empty()
    }

    /// Column 1's agent rows: every non-system participant, the active tab
    /// filtering, the active sort ordering. Live agents first in either
    /// mode; reaped agents sort below live in either mode. The Agents tab
    /// lists everyone (reaped last, dimmed); the Archive tab lists reaped
    /// only.
    fn agent_rows(&self) -> Vec<&Value> {
        let mut agents: Vec<&Value> = self
            .projection()
            .and_then(|p| p.get("participants"))
            .and_then(Value::as_array)
            .map(|ps| {
                ps.iter()
                    .filter(|p| !bool_of(p, "system"))
                    .filter(|p| match self.list_tab {
                        ListTab::Agents => true,
                        ListTab::Archive => self.reaped(p),
                    })
                    .collect()
            })
            .unwrap_or_default();
        let reaped_key = |p: &Value| self.reaped(p);
        match self.sort_mode {
            SortMode::Last => agents.sort_by(|a, b| {
                reaped_key(a)
                    .cmp(&reaped_key(b))
                    .then(text_of(b, "last_ts").cmp(text_of(a, "last_ts")))
                    .then(text_of(a, "name").cmp(text_of(b, "name")))
            }),
            SortMode::Alpha => agents.sort_by(|a, b| {
                reaped_key(a)
                    .cmp(&reaped_key(b))
                    .then(text_of(a, "name").cmp(text_of(b, "name")))
                    .then(text_of(a, "key").cmp(text_of(b, "key")))
            }),
        }
        agents
    }

    /// Column 1's rows, per the active filter (item 5): the Broadcasts
    /// filter lists the broadcast groups alone; the All filter lists the
    /// agents alone, per the active tab and sort. The groups never sit
    /// unlabeled among the agents again.
    pub(crate) fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows: Vec<TreeRow> = Vec::new();
        if self.filter == ListFilter::Broadcasts {
            if let Some(chans) = self
                .projection()
                .and_then(|p| p.get("channels"))
                .and_then(Value::as_array)
            {
                let mut scopes: Vec<&str> = chans
                    .iter()
                    .filter_map(|c| c.get("scope").and_then(Value::as_str))
                    .collect();
                scopes.sort_unstable();
                scopes.dedup();
                for scope in scopes {
                    rows.push(TreeRow::Channel(scope.to_string()));
                }
            }
            return rows;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for p in self.agent_rows() {
            if !seen.insert(text_of(p, "key")) {
                continue;
            }
            let Some(name) = agent_label(p) else {
                continue;
            };
            rows.push(TreeRow::Agent {
                key: text_of(p, "key").to_string(),
                name,
                live: bool_of(p, "live"),
            });
        }
        // A session the user typed into but no mail ever named still gets a
        // row, so its Me thread stays one step down from an agent.
        if self.list_tab == ListTab::Agents {
            for t in self.you_threads() {
                let sid = text_of(t, "session_id");
                let held = rows.iter().any(|r| {
                    matches!(r, TreeRow::Agent { key, .. }
                        if self.you_rows_for(key).iter().any(|y| text_of(y, "session_id") == sid))
                });
                if !held && !sid.is_empty() {
                    let name = match text_of(t, "name") {
                        "" => sid.chars().take(8).collect(),
                        name => name.to_string(),
                    };
                    rows.push(TreeRow::Agent {
                        key: sid.to_string(),
                        name,
                        live: true,
                    });
                }
            }
        }
        rows
    }

    fn you_threads(&self) -> Vec<&Value> {
        self.projection()
            .and_then(|p| p.get("you"))
            .and_then(Value::as_array)
            .map(|rows| rows.iter().collect())
            .unwrap_or_default()
    }

    /// The user's own threads with one agent: the You rows whose session is
    /// the agent's (its key, session id or fno id), or whose name is its name.
    fn you_rows_for(&self, agent: &str) -> Vec<&Value> {
        let p = self
            .projection()
            .and_then(|p| p.get("participants"))
            .and_then(Value::as_array)
            .and_then(|ps| ps.iter().find(|p| text_of(p, "key") == agent));
        let ids: Vec<&str> = [
            Some(agent),
            p.map(|p| text_of(p, "session_id")),
            p.map(|p| text_of(p, "fno_id")),
        ]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect();
        let name = p.map(|p| text_of(p, "name")).unwrap_or("");
        self.you_threads()
            .into_iter()
            .filter(|t| {
                ids.contains(&text_of(t, "session_id"))
                    || (!name.is_empty() && text_of(t, "name") == name)
            })
            .collect()
    }

    /// The selected agent's rows for the active tab.
    pub(crate) fn active_chat_rows(&self) -> Vec<ChatRow> {
        self.sel_agent
            .as_deref()
            .map(|a| self.chat_rows(a))
            .unwrap_or_default()
    }

    /// Column 2's rows for `agent`, per the active tab: the Chats tab lists
    /// the user's own thread with the agent first (Me), then the pair
    /// threads whose other party is not reaped; the System tab one entry per
    /// system arm; the Archive tab the threads whose other party is reaped.
    pub(crate) fn chat_rows(&self, agent: &str) -> Vec<ChatRow> {
        let proj = self.projection();
        match self.col2 {
            ChatTab::System => {
                let mut arms: Vec<String> = proj
                    .and_then(|p| p.get("system"))
                    .and_then(Value::as_object)
                    .and_then(|m| m.get(agent))
                    .and_then(Value::as_array)
                    .map(|rows| {
                        rows.iter()
                            .filter_map(|r| r.get("from").and_then(Value::as_str))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                arms.sort();
                arms.dedup();
                return arms
                    .into_iter()
                    .map(|arm| ChatRow::SystemEntry { arm })
                    .collect();
            }
            _ => {}
        }
        let mut rows: Vec<ChatRow> = Vec::new();
        let marks = crate::view_store::load_messages_read_marks();
        if self.col2 == ChatTab::Chats {
            let mut mine = self.you_rows_for(agent);
            mine.sort_by(|a, b| text_of(b, "last_ts").cmp(text_of(a, "last_ts")));
            for t in mine {
                let sid = text_of(t, "session_id");
                let last_ts = text_of(t, "last_ts");
                rows.push(ChatRow::YouThread {
                    session_id: sid.to_string(),
                    name: "Me".to_string(),
                    last_ts: last_ts.to_string(),
                    unread: marks.get(&format!("you:{sid}")).map(String::as_str) < Some(last_ts),
                });
            }
        }
        if let Some(threads) = proj
            .and_then(|p| p.get("threads"))
            .and_then(Value::as_array)
        {
            for t in threads {
                let parts = t.get("participants").and_then(Value::as_array);
                let has_me =
                    parts.is_some_and(|ps| ps.iter().filter_map(Value::as_str).any(|s| s == agent));
                if !has_me {
                    continue;
                }
                let other = parts
                    .map(|ps| {
                        ps.iter()
                            .filter_map(Value::as_str)
                            .find(|s| *s != agent)
                            .unwrap_or(agent)
                    })
                    .unwrap_or(agent);
                let other_row = proj
                    .and_then(|p| p.get("participants"))
                    .and_then(Value::as_array)
                    .and_then(|ps| ps.iter().find(|p| text_of(p, "key") == other));
                if other_row.is_some_and(|p| bool_of(p, "system")) {
                    continue;
                }
                let other_reaped = other_row.is_some_and(|p| {
                    !bool_of(p, "live") && !text_of(p, "archive_scope").is_empty()
                });
                if other_reaped != (self.col2 == ChatTab::Archive) {
                    continue;
                }
                let other_name = self.participant_name(other);
                let chat_id = text_of(t, "chat_id");
                let rows_of = t.get("rows").and_then(Value::as_array);
                let last_row = rows_of.and_then(|rs| rs.last());
                let last = last_row.map(|r| text_of(r, "summary")).unwrap_or("");
                let ts = last_row.map(|r| text_of(r, "ts")).unwrap_or("");
                let unread =
                    !chat_id.is_empty() && marks.get(chat_id).map(String::as_str) < Some(ts);
                // Any message this agent sent that is still parked or failed
                // waits on the sender, wherever it sits in the chat.
                let stuck = rows_of.is_some_and(|rs| {
                    rs.iter().any(|r| {
                        text_of(r, "from_key") == agent
                            && matches!(text_of(r, "state"), "parked" | "failed")
                    })
                });
                rows.push(ChatRow::Thread {
                    chat_id: chat_id.to_string(),
                    partner: other_name,
                    partner_key: other.to_string(),
                    last: last.to_string(),
                    unread,
                    stuck,
                    ts: ts.to_string(),
                });
            }
        }
        rows
    }

    /// The display name for a participant key, falling back to the raw key.
    pub(super) fn participant_name(&self, key: &str) -> String {
        let proj = self.projection();
        proj.and_then(|p| p.get("participants"))
            .and_then(Value::as_array)
            .and_then(|ps| ps.iter().find(|p| text_of(p, "key") == key))
            .and_then(agent_label)
            .unwrap_or_else(|| key.to_string())
    }

    /// The thread content the third column shows: rows behind the current
    /// selection - a stored pair thread, a `# channel`, or the System
    /// exchange.
    pub(super) fn conversation_rows(&self) -> Vec<&Value> {
        let Some(sel) = self.sel_thread.as_deref() else {
            return Vec::new();
        };
        let Some(proj) = self.projection() else {
            return Vec::new();
        };
        if let Some(scope) = sel.strip_prefix("channel:") {
            return proj
                .get("channels")
                .and_then(Value::as_array)
                .and_then(|cs| cs.iter().find(|c| text_of(c, "scope") == scope))
                .and_then(|c| c.get("rows"))
                .and_then(Value::as_array)
                .map(|rs| rs.iter().collect())
                .unwrap_or_default();
        }
        if let Some(arm) = sel.strip_prefix("system:") {
            // Per-arm filter (item 6): only the selected arm's own rows, to
            // the selected agent when one is. Every arm listing showed one
            // aggregate thread before.
            let mut rows: Vec<&Value> = Vec::new();
            if let Some(m) = proj.get("system").and_then(Value::as_object) {
                match self.sel_agent.as_deref() {
                    Some(agent) => {
                        if let Some(rs) = m.get(agent).and_then(Value::as_array) {
                            rows.extend(rs.iter().filter(|r| text_of(r, "from") == arm));
                        }
                    }
                    None => {
                        for rs in m.values().filter_map(Value::as_array) {
                            rows.extend(rs.iter().filter(|r| text_of(r, "from") == arm));
                        }
                    }
                }
            }
            return rows;
        }
        proj.get("threads")
            .and_then(Value::as_array)
            .and_then(|ts| ts.iter().find(|t| text_of(t, "chat_id") == sel))
            .and_then(|t| t.get("rows"))
            .and_then(Value::as_array)
            .map(|rs| rs.iter().collect())
            .unwrap_or_default()
    }

    /// The three columns' lines.
    pub(crate) fn columns(&self, w: usize) -> (Vec<BLine>, Vec<BLine>, Vec<BLine>) {
        let (tree_w, part_w) = split(w);
        let tree = self.tree_column(tree_w);
        let chats = self.chats_column(part_w);
        let content = self.thread_lines(w.saturating_sub(tree_w + part_w)).0;
        (tree, chats, content)
    }

    /// Column 1: the filter row (item 5), the All filter's tab strip, then
    /// the active list. Band rides the cursor row; `a`/`b` switch the
    /// filter, `s` toggles the sort shown in the strip.
    fn tree_column(&self, w: usize) -> Vec<BLine> {
        let wrap_w = w.saturating_sub(2).max(1);
        let mut lines: Vec<BLine> = Vec::new();
        let focused = self.col == Col::Tree;
        let filter_row = BLine::of(&[
            seg("a", BRole::Meta),
            tab_seg("All", self.filter == ListFilter::All, focused),
            seg(" \u{b7} b", BRole::Meta),
            tab_seg("Broadcasts", self.filter == ListFilter::Broadcasts, focused),
        ]);
        lines.push(filter_row);
        if self.filter == ListFilter::All {
            let sort_word = match self.sort_mode {
                SortMode::Last => "last",
                SortMode::Alpha => "a-z",
            };
            let agents_active = self.list_tab == ListTab::Agents;
            let strip = BLine::of(&[
                tab_seg(
                    &format!("Agents \u{b7} {sort_word}"),
                    agents_active,
                    focused,
                ),
                seg("|", BRole::Meta),
                tab_seg("Archive", !agents_active, focused),
            ]);
            lines.push(strip);
        }
        let error = self.snapshot.error_line(board_now());
        if !error.is_empty() {
            lines.extend(BLine::meta(error).wrap(wrap_w));
        }
        if self.snapshot.projection.is_none() {
            if self.inflight {
                // Skeleton rows (item 1): the list's shape before the read
                // lands.
                for _ in 0..3 {
                    lines.push(BLine::meta("\u{b7} \u{b7} \u{b7} \u{b7} \u{b7}"));
                }
            } else {
                lines.extend(BLine::meta("(not read yet - opening gathers once)").wrap(wrap_w));
            }
        }
        let rows = self.tree_rows();
        if self.filter == ListFilter::All
            && self.list_tab == ListTab::Archive
            && !rows.iter().any(|r| matches!(r, TreeRow::Agent { .. }))
        {
            lines.extend(BLine::meta("(no archived agents)").wrap(wrap_w));
        }
        if self.filter == ListFilter::Broadcasts && rows.is_empty() {
            lines.extend(BLine::meta("(no broadcast groups)").wrap(wrap_w));
        }
        lines.push(BLine::meta(""));
        for (i, row) in rows.iter().enumerate() {
            let mut line = match row {
                TreeRow::Channel(scope) => {
                    BLine::of(&[seg(format!("fleet:{}", channel_label(scope)), BRole::Label)])
                }
                TreeRow::Agent { name, live, .. } => BLine::of(&[
                    seg("  ", BRole::Meta),
                    seg(name.clone(), if *live { BRole::Body } else { BRole::Meta }),
                ]),
            };
            line.band = i == self.cursors[0];
            lines.push(line);
        }
        lines
    }

    /// Column 2: the tab strip, then the active tab's rows. Placeholders
    /// wrap to the column's width (item 4); a system arm shows its display
    /// label (item 5).
    fn chats_column(&self, w: usize) -> Vec<BLine> {
        let wrap_w = w.saturating_sub(2).max(1);
        let focused = self.col == Col::Chats;
        let strip = BLine::of(&[
            tab_seg("Chats", self.col2 == ChatTab::Chats, focused),
            tab_seg("System", self.col2 == ChatTab::System, focused),
            tab_seg("Archive", self.col2 == ChatTab::Archive, focused),
        ]);
        let mut lines = vec![strip, BLine::meta("")];
        if self.sel_agent.is_none() {
            lines.extend(BLine::meta("(select an agent in column 1)").wrap(wrap_w));
            return lines;
        }
        let rows = self.active_chat_rows();
        if rows.is_empty() {
            let empty = match self.col2 {
                ChatTab::Chats => "(no chats)",
                ChatTab::System => "(no system mail)",
                ChatTab::Archive => "(no archived chats)",
            };
            lines.extend(BLine::meta(empty).wrap(wrap_w));
            return lines;
        }
        for (i, row) in rows.iter().enumerate() {
            let mut line = match row {
                ChatRow::SystemEntry { arm } => {
                    BLine::of(&[seg("  ", BRole::Meta), seg(display_label(arm), BRole::Body)])
                }
                ChatRow::Thread {
                    partner,
                    last,
                    unread,
                    stuck,
                    ..
                } => BLine::of(&[
                    // Two slots: `!` (a sent message waits on the sender)
                    // never hides `*` (unread for the reader).
                    seg(if *stuck { "!" } else { " " }, BRole::Warn),
                    seg(if *unread { "*" } else { " " }, BRole::Meta),
                    seg(partner.clone(), BRole::Body),
                    seg(format!(" \u{b7} {last}"), BRole::Meta),
                ]),
                ChatRow::YouThread {
                    name,
                    last_ts,
                    unread,
                    ..
                } => BLine::of(&[
                    // The same two slots as a thread row, so names line up.
                    seg(" ", BRole::Warn),
                    seg(if *unread { "*" } else { " " }, BRole::Meta),
                    seg(name.clone(), BRole::Body),
                    seg(format!(" \u{b7} {}", Self::ts_time(last_ts)), BRole::Meta),
                ]),
            };
            line.band = i == self.cursors[1];
            lines.push(line);
        }
        lines
    }
}

impl MessagesBoard {
    /// The local HH:MM a stored ts shows under.
    fn ts_time(ts: &str) -> String {
        chrono::DateTime::parse_from_rfc3339(ts)
            .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default()
    }

    /// Minutes from one stored ts to the next; `None` when either is
    /// unreadable.
    fn ts_gap_minutes(from: &str, to: &str) -> Option<i64> {
        Some(
            (chrono::DateTime::parse_from_rfc3339(to).ok()?
                - chrono::DateTime::parse_from_rfc3339(from).ok()?)
            .num_minutes(),
        )
    }

    /// Column 3's title: the other party's display name plus the info and
    /// more affordances (item 10).
    fn thread_title(&self) -> BLine {
        let Some(sel) = self.sel_thread.as_deref() else {
            return BLine::meta("Thread");
        };
        if let Some(sid) = sel.strip_prefix("you:") {
            let name = self
                .you_row(sid)
                .map(|t| text_of(t, "name").to_string())
                .unwrap_or_else(|| sid.get(..8).unwrap_or(sid).to_string());
            let name_role = if self.col == Col::Thread {
                BRole::Head
            } else {
                BRole::Body
            };
            return BLine::of(&[
                seg(name, name_role),
                seg("  [i]  ...".to_string(), BRole::Meta),
            ]);
        }
        let name = if let Some(scope) = sel.strip_prefix("channel:") {
            channel_label(scope)
        } else if sel.starts_with("system:") {
            display_label(&sel["system:".len()..])
        } else {
            let other = self
                .conversation_rows()
                .first()
                .and_then(|r| {
                    let me = self.sel_agent.as_deref().unwrap_or("");
                    let from = text_of(r, "from_key");
                    let to = text_of(r, "to_key");
                    Some(if from == me {
                        to.to_string()
                    } else {
                        from.to_string()
                    })
                })
                .or_else(|| self.sel_agent.clone())
                .unwrap_or_default();
            self.participant_name(&other)
        };
        // The focused column's title is bold; elsewhere it reads plain.
        let name_role = if self.col == Col::Thread {
            BRole::Head
        } else {
            BRole::Body
        };
        let title = BLine::of(&[
            seg(name, name_role),
            seg("  [i]  ...".to_string(), BRole::Meta),
        ]);
        title
    }

    /// Column 3's lines, with the conversation row each line belongs to
    /// (`None` for separators) so the click map mirrors the paint. A
    /// thread: the selected agent's own mail flush right, the other party's
    /// on the left, each burst closed by its last message's HH:MM on that
    /// message's side (a gap of five minutes or more ends a burst), one name
    /// label per run of consecutive same-sender rows, and the body only -
    /// no envelopes, no delivered-header lines (item 9 runs, item 12
    /// padding).
    pub(crate) fn thread_lines(&self, w: usize) -> (Vec<BLine>, Vec<Option<usize>>) {
        let title = self.thread_title();
        if let Some(sid) = self
            .sel_thread
            .as_deref()
            .and_then(|s| s.strip_prefix("you:"))
        {
            return self.you_thread_lines(w, sid);
        }
        let mut lines = vec![title];
        let mut owners: Vec<Option<usize>> = vec![None];
        let wrap_w = w.saturating_sub(2).min(((w * 7) / 10).max(12)).max(1);
        let Some(_sel) = self.sel_thread.as_deref() else {
            let wrapped = BLine::meta("(select a chat in column 2)").wrap(wrap_w);
            owners.extend(std::iter::repeat(None).take(wrapped.len()));
            lines.extend(wrapped);
            return (lines, owners);
        };
        let rows = self.conversation_rows();
        if rows.is_empty() {
            let wrapped = BLine::meta("(no messages)").wrap(wrap_w);
            owners.extend(std::iter::repeat(None).take(wrapped.len()));
            lines.extend(wrapped);
            return (lines, owners);
        }
        let mine = self.sel_agent.as_deref().unwrap_or("");
        let wrap_w = w.saturating_sub(2).max(1);
        // The previous row's ts and side: a gap of five minutes or more
        // closes its burst with the time on the side that sent last.
        let mut last: Option<(String, bool)> = None;
        let mut run_key: Option<String> = None;
        for (index, r) in rows.iter().enumerate() {
            let selected = self.selected_message_id.as_deref() == Some(text_of(r, "id"))
                || index == self.cursors[2];
            let ts = text_of(r, "ts");
            let key = text_of(r, "from_key");
            let sys = bool_of(r, "system");
            let right = key == mine && !sys;
            if let Some((prev, prev_right)) = &last {
                if Self::ts_gap_minutes(prev, ts).is_none_or(|g| g >= 5) {
                    push_time(&mut lines, &mut owners, prev, *prev_right, w);
                    run_key = None;
                }
            }
            last = Some((ts.to_string(), right));
            let sender = if sys {
                display_label(&text_of(r, "from"))
            } else {
                self.participant_name(key)
            };
            let width = if right { bubble_w(wrap_w) } else { wrap_w };
            let mut block = Vec::new();
            if run_key.as_deref() != Some(key) {
                push_blank(&mut lines, &mut owners);
                // The name line is the envelope's formal structure, so it
                // paints in the label color, distinct from the dim meta and
                // the bold subject under it.
                block.push(BLine::of(&[seg(sender.clone(), BRole::Label)]));
            }
            run_key = Some(key.to_string());
            // The sender's --subject rides the row; a headline line under
            // the sender, only when one was given.
            let subject = text_of(r, "subject");
            if !subject.is_empty() {
                block.extend(body_lines(subject, width, BRole::Head));
            }
            block.extend(body_lines(text_of(r, "body"), width, BRole::Body));
            // A sent row says what the recipient's harness did with it, and
            // a stuck one names the key that moves it.
            if let Some((text, role)) = right.then(|| state_line(r)).flatten() {
                block.extend(body_lines(&text, width, role));
            }
            if right {
                flush_right(&mut block, wrap_w);
            }
            for mut line in block {
                line.band = selected;
                lines.push(line);
                owners.push(Some(index));
            }
        }
        if let Some((prev, prev_right)) = &last {
            push_time(&mut lines, &mut owners, prev, *prev_right, w);
        }
        (lines, owners)
    }

    /// Column 3 for a selected You thread: the operator's typed turn flush
    /// right, then the assistant's reply on the left, one item at a time,
    /// with the same burst clock and wrap idioms `thread_lines` uses. Owners index
    /// ITEMS (two bubbles per item), so the cursor indexes `item = cur/2`.
    fn you_thread_lines(&self, w: usize, sid: &str) -> (Vec<BLine>, Vec<Option<usize>>) {
        let title = self.thread_title();
        let mut lines = vec![title];
        let mut owners: Vec<Option<usize>> = vec![None];
        let wrap_w = w.saturating_sub(2).max(1);
        let items = self.you_items(sid);
        if items.is_empty() {
            let wrapped = BLine::meta("(no You items)").wrap(wrap_w);
            owners.extend(std::iter::repeat(None).take(wrapped.len()));
            lines.extend(wrapped);
            return (lines, owners);
        }
        let agent_name = self
            .you_row(sid)
            .map(|t| text_of(t, "name").to_string())
            .unwrap_or_else(|| sid.get(..8).unwrap_or(sid).to_string());
        let mut last: Option<(String, bool)> = None;
        for (item_index, item) in items.iter().enumerate() {
            let selected = item_index == self.cursors[2];
            let ts = item.get("ts").and_then(Value::as_str).unwrap_or("");
            let typed = item.get("typed_text").and_then(Value::as_str).unwrap_or("");
            let reply = item.get("reply_text").and_then(Value::as_str).unwrap_or("");
            if let Some((prev, prev_right)) = &last {
                if Self::ts_gap_minutes(prev, ts).is_none_or(|g| g >= 5) {
                    push_time(&mut lines, &mut owners, prev, *prev_right, w);
                }
            }
            // The reply is the item's last send; with none, the user's turn.
            last = Some((ts.to_string(), reply.is_empty()));
            // The operator's typed turn, flush right under its own Me label.
            if !typed.is_empty() {
                push_blank(&mut lines, &mut owners);
                let mut block = vec![BLine::of(&[seg("Me", BRole::Label)])];
                block.extend(body_lines(typed, bubble_w(wrap_w), BRole::Body));
                flush_right(&mut block, wrap_w);
                for mut line in block {
                    line.band = selected;
                    lines.push(line);
                    owners.push(Some(item_index));
                }
            }
            // The assistant's reply, under the session's name.
            if !reply.is_empty() {
                push_blank(&mut lines, &mut owners);
                let mut label = BLine::of(&[seg(agent_name.clone(), BRole::Label)]);
                label.band = selected;
                lines.push(label);
                owners.push(Some(item_index));
                for mut line in body_lines(reply, wrap_w, BRole::Body) {
                    line.band = selected;
                    lines.push(line);
                    owners.push(Some(item_index));
                }
            }
        }
        if let Some((prev, prev_right)) = &last {
            push_time(&mut lines, &mut owners, prev, *prev_right, w);
        }
        (lines, owners)
    }

    /// The selected You thread's items, oldest first.
    pub(crate) fn you_items(&self, sid: &str) -> Vec<Value> {
        self.projection()
            .and_then(|p| p.get("you"))
            .and_then(Value::as_array)
            .and_then(|rows| {
                rows.iter()
                    .find(|t| text_of(t, "session_id") == sid)
                    .and_then(|t| t.get("items").and_then(Value::as_array).cloned())
            })
            .unwrap_or_default()
    }

    /// The selected You thread's projection row.
    fn you_row(&self, sid: &str) -> Option<&Value> {
        self.projection()
            .and_then(|p| p.get("you"))
            .and_then(Value::as_array)
            .and_then(|rows| rows.iter().find(|t| text_of(t, "session_id") == sid))
    }
}

/// The full-surface paint: three columns from row 1 (the strip owns
/// row 0), footer hints on the last row.
pub(crate) fn paint(
    view: &View,
    cells: &mut [Cell],
    rows: usize,
    cols: usize,
    width: usize,
    height: usize,
) {
    let Some(b) = &view.messages_board else {
        return;
    };
    let width = width.min(cols);
    let height = height.min(rows);
    if width == 0 || height < 2 {
        return;
    }
    for r in 1..height {
        for c in 0..width {
            cells[r * cols + c] = Cell::default();
        }
    }
    let (tree, partners, thread) = b.columns(width);
    let tree_w = (width / 4).max(14).min(width / 2);
    let part_w = (width / 4).max(14).min(width.saturating_sub(tree_w) / 2);
    let thread_w = width.saturating_sub(tree_w + part_w);
    let body_h = height.saturating_sub(2);
    // The cursor's painted line follows the window, so a long tree keeps
    // the selection visible and the click map's window matches the paint.
    // Each column's rows start after its own prefix lines (strip, status,
    // spacer), so the follow line is that prefix plus the cursor.
    let tree_prefix = tree.len().saturating_sub(b.tree_rows().len());
    super::backlog_style::paint_panel_banded(
        cells,
        rows,
        cols,
        (0, 1, tree_w, body_h),
        &tree,
        Some(tree_prefix + b.cursors[0]),
        column_band(b.col == Col::Tree, &view.theme),
        &view.theme,
    );
    super::backlog_style::paint_panel_banded(
        cells,
        rows,
        cols,
        (tree_w, 1, part_w, body_h),
        &partners,
        Some(partners.len().saturating_sub(b.active_chat_rows().len()) + b.cursors[1]),
        column_band(b.col == Col::Chats, &view.theme),
        &view.theme,
    );
    super::backlog_style::paint_panel_banded(
        cells,
        rows,
        cols,
        (tree_w + part_w, 1, thread_w, body_h),
        &thread,
        Some(
            thread
                .iter()
                .rposition(|line| line.band)
                .unwrap_or_else(|| thread.len().saturating_sub(1)),
        ),
        column_band(b.col == Col::Thread, &view.theme),
        &view.theme,
    );
    // The context help line (item 8), on the row the three panels leave
    // free. Truncated to the width: at a degenerate terminal the keys still
    // work, only the hint shortens.
    let help = "h/l or \u{2190}/\u{2192} columns \u{b7} j/k move \u{b7} enter open \u{b7} esc back \u{b7} a/b filter \u{b7} s sort \u{b7} d details \u{b7} r retry \u{b7} m id \u{b7} y copy \u{b7} q close";
    let help: String = help.chars().take(width.saturating_sub(1)).collect();
    for (i, ch) in help.chars().enumerate() {
        if let Some(cell) = cells.get_mut((height - 1) * cols + i) {
            *cell = Cell {
                c: ch,
                fg: crate::theme::dim_fg(&view.theme),
                bg: Color::Default,
                flags: 0,
            };
        }
    }
}

/// The full-surface paint the client's paint chain calls: Messages is a
/// full-surface view, so the strip row is the sideline's and the board
/// paints from row 1.
pub(crate) fn paint_full(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    view.paint_top_row(cells, cols, cols);
    paint(view, cells, rows, cols, cols, rows);
    super::messages_reply::paint(view, cells, rows, cols);
}

/// The chord door (prefix+M, item 8): open Messages from anywhere. The tab
/// paints full surface, but only while the sideline shows: a hidden
/// sideline would swallow the open, so the chord reveals it first.
pub(crate) fn open_from_chord(view: &mut View) {
    view.panel_on = true;
    open(view);
}

/// Lifecycle.
pub(crate) fn open(view: &mut View) {
    let gen = NEXT_MESSAGES_GEN.fetch_add(1, Ordering::Relaxed);
    view.messages_board = Some(MessagesBoard::new(gen));
    view.backlog_board = None;
    view.org_board = None;
    view.region_owner = super::region_focus::RegionOwner::Board;
    super::backlog_board::set_sideline_view(view, SidelineView::Messages);
}

/// Open the Messages tab with a pending message id. The projection gather
/// resolves it to a chat before the thread is selected.
pub(crate) fn open_message(view: &mut View, id: String) {
    open(view);
    if let Some(board) = view.messages_board.as_mut() {
        board.pending_message_id = Some(id);
    }
}

/// Open the Messages tab filtered to a clicked `@handle`. The projection
/// gather resolves the name to its participant, then column 1 selects it;
/// a handle that never messaged this fleet notices instead of selecting.
pub(crate) fn open_handle(view: &mut View, name: String) {
    open(view);
    if let Some(board) = view.messages_board.as_mut() {
        board.pending_handle = Some(name);
    }
}

/// Restore after launch: a persisted Messages sideline reopens it.
pub(crate) fn restore(view: &mut View) {
    if view.sideline_view == SidelineView::Messages {
        open(view);
    }
}

/// A landed gather: apply under the gen guard. The org tree result rides
/// the same channel and is not used by the flat chat list.
pub(crate) fn apply_gather(
    view: &mut View,
    gen: u64,
    mail: Result<Value, String>,
    _tree: Result<OrgTree, String>,
) {
    let Some(b) = view.messages_board.as_mut().filter(|b| b.gen == gen) else {
        return;
    };
    b.inflight = false;
    let mut notice = None;
    match mail {
        Ok(v) => {
            crate::messages_model::remember(&v);
            b.snapshot.apply(v);
            if let Some(id) = b.pending_message_id.take() {
                let target = b.projection().and_then(|projection| {
                    let pair = projection
                        .get("threads")?
                        .as_array()?
                        .iter()
                        .find_map(|thread| {
                            let rows = thread.get("rows")?.as_array()?;
                            let row = rows.iter().find(|row| text_of(row, "id") == id)?;
                            Some((
                                text_of(thread, "chat_id").to_string(),
                                Some(text_of(row, "to_key").to_string()),
                            ))
                        });
                    if pair.is_some() {
                        return pair;
                    }
                    let channel =
                        projection
                            .get("channels")?
                            .as_array()?
                            .iter()
                            .find_map(|channel| {
                                let rows = channel.get("rows")?.as_array()?;
                                rows.iter().any(|row| text_of(row, "id") == id).then(|| {
                                    (format!("channel:{}", text_of(channel, "scope")), None)
                                })
                            });
                    if channel.is_some() {
                        return channel;
                    }
                    projection
                        .get("system")?
                        .as_object()?
                        .iter()
                        .find_map(|(agent, rows)| {
                            rows.as_array()?.iter().find_map(|row| {
                                (text_of(row, "id") == id).then(|| {
                                    // The per-arm thread (item 6), not the
                                    // agent's aggregate.
                                    (
                                        format!("system:{}", text_of(row, "from")),
                                        Some(agent.clone()),
                                    )
                                })
                            })
                        })
                });
                if let Some((thread, agent)) = target.filter(|(thread, _)| !thread.is_empty()) {
                    b.sel_thread = Some(thread.clone());
                    if let Some(agent) = agent {
                        b.sel_agent = Some(agent.clone());
                        let rows = b.chat_rows(&agent);
                        if let Some(i) = rows
                            .iter()
                            .position(|row| {
                                matches!(row, ChatRow::Thread { chat_id, .. } if chat_id == &thread)
                            })
                            .or_else(|| {
                                rows.iter().position(|row| {
                                    matches!(row, ChatRow::SystemEntry { arm }
                                        if format!("system:{arm}") == thread)
                                })
                            })
                        {
                            b.cursors[1] = i;
                        }
                    }
                    b.cursors[2] = b
                        .conversation_rows()
                        .iter()
                        .position(|row| text_of(row, "id") == id)
                        .unwrap_or(0);
                    b.selected_message_id = Some(id);
                    b.col = Col::Thread;
                } else {
                    notice = Some(format!("message {id}: no conversation found"));
                }
            }
            if let Some(handle) = b.pending_handle.take() {
                let want = handle.trim_start_matches('@').to_string();
                let rows = b.tree_rows();
                let hit = rows.iter().position(|r| {
                    matches!(r, TreeRow::Agent { key, name, .. }
                        if name.eq_ignore_ascii_case(&want) || key.eq_ignore_ascii_case(&want))
                });
                match hit {
                    Some(i) => {
                        if let TreeRow::Agent { key, .. } = &rows[i] {
                            b.sel_agent = Some(key.clone());
                            b.sel_thread = None;
                            b.cursors[0] = i;
                            b.col = Col::Chats;
                        }
                    }
                    None => notice = Some(format!("@{want}: no messages found")),
                }
            }
        }
        Err(reason) => b.snapshot.fail(reason, board_now()),
    }
    if let Some(notice) = notice {
        view.set_notice(notice);
    }
}

/// Kick a gather only when the store changed (item 1): the stat walk runs
/// every 5s; the expensive read fires on a fingerprint change, or when the
/// board has never read.
pub(crate) fn maybe_kick(view: &mut View, tx: &MessagesTx) {
    if let Some(b) = view.messages_board.as_mut() {
        if b.inflight {
            return;
        }
        let never_read = b.snapshot.projection.is_none();
        if !never_read
            && b.last_poll
                .is_some_and(|t| t.elapsed() < Duration::from_secs(5))
        {
            return;
        }
        b.last_poll = Some(Instant::now());
        let fp = crate::messages_model::store_fingerprint();
        let changed = match (&fp, b.last_fingerprint) {
            (Some(now), Some(last)) => now != &last,
            // Never kicked: read once to learn the baseline even when the
            // cache is painting.
            (Some(_), None) => true,
            (None, _) => false,
        };
        if !never_read && !changed {
            return;
        }
        b.inflight = true;
        b.last_fingerprint = fp;
        let gen = b.gen;
        let agents = view.layout.agents.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let graph = crate::backlog_view::graph_path();
            let inputs = crate::org_model::gather(&graph, agents).await;
            let tree = crate::org_model::derive(&inputs, board_now());
            let mail = crate::messages_model::gather().await;
            let _ = tx.send((gen, mail, tree));
        });
    }
}

/// The column widths the paint and the click map share.
fn split(width: usize) -> (usize, usize) {
    let tree_w = (width / 4).max(14).min(width / 2);
    let part_w = (width / 4).max(14).min(width.saturating_sub(tree_w) / 2);
    (tree_w, part_w)
}

/// The screen rows a column's lines painted at, given its scroll start.
fn column_rect(lines: usize, body_h: usize, follow_line: usize) -> (usize, usize) {
    let len = lines.max(1);
    let start = if len <= body_h {
        0
    } else {
        follow_line
            .saturating_sub(body_h - 1)
            .min(len.saturating_sub(body_h))
    };
    (start, body_h.min(len))
}

/// Keys while the board owns the keyboard: the org board's shape, with
/// Tab as the column step.
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
    if super::messages_reply::active(view) {
        return super::messages_reply::keys(view, bytes, sock).await;
    }
    let Some(b) = view.messages_board.as_mut() else {
        return Ok(StdinFlow::Continue);
    };
    if let Some(detail) = b.detail.as_mut() {
        if super::messages_detail::detail_keys(detail, bytes) {
            b.detail = None;
        }
        return Ok(StdinFlow::Continue);
    }
    let tokens = if bytes == [27] && b.esc.is_empty() {
        vec![ModalKey::Esc]
    } else {
        fold_modal_keys(&mut b.esc, bytes)
    };
    for token in tokens {
        let Some(b) = view.messages_board.as_mut() else {
            break;
        };
        match token {
            ModalKey::Byte(b'q') => {
                view.messages_board = None;
                super::backlog_board::set_sideline_view(view, SidelineView::Agents);
            }
            // Esc backs one column (item 8); from column 1 it closes. It
            // never wraps: only h from Tree wraps, mirroring l from Thread.
            ModalKey::Esc => {
                if b.col == Col::Tree {
                    view.messages_board = None;
                    super::backlog_board::set_sideline_view(view, SidelineView::Agents);
                } else {
                    b.col = b.col.prev();
                }
            }
            ModalKey::Byte(b'V') => {
                super::backlog_board::cycle_sideline_view(view);
            }
            // h/l and the arrows step the columns (item 8).
            ModalKey::Left | ModalKey::Byte(b'h') => {
                b.col = b.col.prev();
            }
            ModalKey::Right | ModalKey::Byte(b'l') => {
                b.col = b.col.next();
            }
            ModalKey::Up | ModalKey::Byte(b'k') => {
                let len = column_len(b);
                b.step(false, len);
            }
            ModalKey::Down | ModalKey::Byte(b'j') => {
                let len = column_len(b);
                b.step(true, len);
            }
            // The filter row's keys (item 5).
            ModalKey::Byte(b'a') => {
                b.filter = ListFilter::All;
                b.cursors[0] = 0;
            }
            ModalKey::Byte(b'b') => {
                b.filter = ListFilter::Broadcasts;
                b.cursors[0] = 0;
            }
            ModalKey::Enter => act(view, sock).await?,
            ModalKey::Byte(b'd') => open_detail(view),
            ModalKey::Byte(b's') if b.col == Col::Tree => {
                b.sort_mode = if b.sort_mode == SortMode::Last {
                    SortMode::Alpha
                } else {
                    SortMode::Last
                };
            }
            ModalKey::Byte(b'\t') => {
                // In the chats column Tab cycles the tab; elsewhere it
                // steps the column.
                if b.col == Col::Chats {
                    b.col2 = match b.col2 {
                        ChatTab::Chats => ChatTab::System,
                        ChatTab::System => ChatTab::Archive,
                        ChatTab::Archive => ChatTab::Chats,
                    };
                    b.cursors[1] = 0;
                } else {
                    b.col = b.col.next();
                }
            }
            ModalKey::Byte(b'r') if b.col == Col::Thread => retry_selected(view),
            ModalKey::Byte(b'm') => open_chat_popup(view),
            ModalKey::Byte(b'y') => copy_bubble(view, false),
            ModalKey::Byte(b'Y') => copy_bubble(view, true),
            _ => {}
        }
    }
    Ok(StdinFlow::Continue)
}

/// The active column's row count.
fn column_len(b: &MessagesBoard) -> usize {
    match b.col {
        Col::Tree => b.tree_rows().len(),
        Col::Chats => b.active_chat_rows().len(),
        Col::Thread => thread_len(b),
    }
}

/// The Thread column's navigable length: a You thread's items, else the
/// conversation rows.
fn thread_len(b: &MessagesBoard) -> usize {
    match b.sel_thread.as_deref().and_then(|s| s.strip_prefix("you:")) {
        Some(sid) => b.you_items(sid).len(),
        None => b.conversation_rows().len(),
    }
}

/// Enter on the active column's row.
async fn act(
    view: &mut View,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    let Some(b) = view.messages_board.as_mut() else {
        return Ok(());
    };
    match b.col {
        Col::Tree => {
            let rows = b.tree_rows();
            let Some(row) = rows.get(b.cursors[0]) else {
                return Ok(());
            };
            match row {
                TreeRow::Channel(scope) => {
                    b.sel_thread = Some(format!("channel:{scope}"));
                    b.cursors[2] = b.conversation_rows().len().saturating_sub(1);
                    b.col = Col::Thread;
                }
                TreeRow::Agent { key, .. } => {
                    b.sel_agent = Some(key.clone());
                    b.sel_thread = None;
                    b.col = Col::Chats;
                }
            }
        }
        Col::Chats => {
            // Enter resolves through active_chat_rows: the You tab's rows
            // and the agent tabs' share one path.
            let rows = b.active_chat_rows();
            let Some(row) = rows.get(b.cursors[1]) else {
                return Ok(());
            };
            match row {
                ChatRow::SystemEntry { arm } => {
                    b.sel_thread = Some(format!("system:{arm}"));
                    b.cursors[2] = b.conversation_rows().len().saturating_sub(1);
                    b.col = Col::Thread;
                }
                ChatRow::YouThread {
                    session_id,
                    last_ts,
                    ..
                } => {
                    crate::view_store::save_messages_read_mark(
                        &format!("you:{session_id}"),
                        last_ts,
                    );
                    b.sel_thread = Some(format!("you:{session_id}"));
                    b.cursors[2] = b.you_items(session_id).len().saturating_sub(1);
                    b.col = Col::Thread;
                }
                ChatRow::Thread { chat_id, ts, .. } => {
                    crate::view_store::save_messages_read_mark(chat_id, ts);
                    b.sel_thread = Some(chat_id.clone());
                    b.cursors[2] = b.conversation_rows().len().saturating_sub(1);
                    b.col = Col::Thread;
                }
            }
        }
        Col::Thread => {
            if let Some(sent) = selected_sent_row(b).filter(|r| text_of(r, "state") == "parked") {
                return submit_parked(view, sock, &sent).await;
            }
            let row = b
                .conversation_rows()
                .get(b.cursors[2])
                .map(|row| (**row).clone());
            if let Some(row) = row {
                super::messages_reply::open(view, row);
            }
        }
    }
    Ok(())
}

/// d on a chat-list or chat row: the session details modal. In the thread
/// column it opens on the other party.
fn open_detail(view: &mut View) {
    let key = {
        let Some(b) = view.messages_board.as_ref() else {
            return;
        };
        match b.col {
            Col::Tree => b.tree_rows().get(b.cursors[0]).and_then(tree_row_agent_key),
            Col::Chats => b.sel_agent.clone().and_then(|a| {
                b.chat_rows(&a)
                    .into_iter()
                    .nth(b.cursors[1])
                    .and_then(|row| match row {
                        ChatRow::Thread { partner_key, .. } => Some(partner_key),
                        ChatRow::SystemEntry { .. } | ChatRow::YouThread { .. } => None,
                    })
            }),
            Col::Thread => b
                .conversation_rows()
                .get(b.cursors[2])
                .and_then(|row| {
                    let me = b.sel_agent.as_deref().unwrap_or("");
                    let from = text_of(row, "from_key");
                    let to = text_of(row, "to_key");
                    Some(if from == me {
                        to.to_string()
                    } else {
                        from.to_string()
                    })
                })
                .filter(|k| !k.is_empty()),
        }
    };
    if let Some(key) = key {
        super::messages_detail::open(view, key);
    }
}

/// `m` (or the title's `...`): a one-row popup naming the thread's chat id.
fn open_chat_popup(view: &mut View) {
    let Some(b) = view.messages_board.as_ref() else {
        return;
    };
    let Some(sel) = b.sel_thread.as_deref() else {
        return;
    };
    let value = sel.to_string();
    let label = match sel.strip_prefix("channel:") {
        Some(scope) => format!("channel {scope}"),
        None => "chat".to_string(),
    };
    let rows = vec![PopupRow::Info { label, value }];
    let popup = Popup::new(rows, Anchor::Center)
        .title("conversation")
        .footer("esc close")
        .plain_body();
    if let Some(b) = view.messages_board.as_mut() {
        b.detail = Some(super::messages_detail::SessionDetail {
            popup,
            // A conversation popup carries no session key and never
            // receives a token answer: the empty key matches nothing.
            key: String::new(),
            tokens_pending: false,
        });
    }
}

/// `y` copies the selected bubble's fmail id; `Y` copies its whole body. On
/// a You thread it copies the typed message, or the typed-and-reply pair.
fn copy_bubble(view: &mut View, whole_body: bool) {
    let Some(b) = view.messages_board.as_ref() else {
        return;
    };
    if let Some(sid) = b.sel_thread.as_deref().and_then(|s| s.strip_prefix("you:")) {
        let items = b.you_items(sid);
        let Some(item) = items.get(b.cursors[2]) else {
            return;
        };
        let typed = text_of(item, "typed_text").to_string();
        let value = if whole_body {
            let reply = text_of(item, "reply_text");
            if reply.is_empty() {
                typed
            } else {
                format!("{typed}\n\n{reply}")
            }
        } else {
            typed
        };
        if !value.is_empty() {
            super::feed_detail::copy_value(view, value);
        }
        return;
    }
    // The last rung for a stuck message: its text, to paste by hand into
    // the recipient's session.
    if let Some(sent) =
        selected_sent_row(b).filter(|r| matches!(text_of(r, "state"), "parked" | "failed"))
    {
        let body = text_of(&sent, "body").to_string();
        if !body.is_empty() {
            super::feed_detail::copy_value(view, body);
        }
        return;
    }
    let row = b
        .conversation_rows()
        .get(b.cursors[2])
        .map(|row| (**row).clone());
    let Some(row) = row else {
        return;
    };
    let id = text_of(&row, "id").to_string();
    let body = text_of(&row, "body").to_string();
    let value = if whole_body { body } else { id };
    if value.is_empty() {
        return;
    }
    super::feed_detail::copy_value(view, value);
}

/// The line under a sent message: its delivery state, and for a stuck one
/// the cause and the keys that act on it. Submit or retry comes first; copy
/// and paste by hand is the last rung, offered once a retry failed too.
fn state_line(r: &Value) -> Option<(String, BRole)> {
    let to = match text_of(r, "to") {
        "" => "the recipient",
        to => to,
    };
    let reason = reason_words(text_of(r, "state_reason"), to);
    let failures = r.get("state_failures").and_then(Value::as_u64).unwrap_or(0);
    let delivered = if failures > 0 {
        "delivered on retry"
    } else {
        "delivered"
    };
    match text_of(r, "state") {
        "" => None,
        "accepted-new-turn" => Some((
            format!("{delivered}: {to} took it as a new turn"),
            BRole::Meta,
        )),
        "accepted-steer" => Some((format!("{delivered}: {to} read it mid-turn"), BRole::Meta)),
        "parked" => Some(("parked \u{b7} enter submit".to_string(), BRole::Meta)),
        "failed" => {
            let cause = if reason.is_empty() {
                String::new()
            } else {
                format!(": {reason}")
            };
            // No-break spaces hold the keys on one line when the cause wraps.
            let (head, keys) = if failures > 1 {
                ("retry failed", " \u{b7} r retry \u{b7} y copy")
            } else {
                ("failed", " \u{b7} r retry")
            };
            let keys = keys.trim_start().replace(' ', "\u{a0}");
            Some((format!("{head}{cause} {keys}"), BRole::Warn))
        }
        word => Some((word.to_string(), BRole::Meta)),
    }
}

/// A lane's failure reason in words. The lanes join their reason tokens
/// with `;`, so the first token with a phrase wins; an unknown token reads
/// with its hyphens as spaces.
fn reason_words(reason: &str, to: &str) -> String {
    let phrase = |token: &str| -> Option<String> {
        Some(match token {
            "no-pane-ref" => format!("{to} has no pane to type into"),
            "not-confirmed" => format!("{to} did not show it in a turn"),
            "no-recipient" => format!("no agent named {to} is registered"),
            "bus-only" => format!("{to} takes mail only, never typed text"),
            "attach-failed" => format!("could not attach to {to}'s pane"),
            "no-confirm-source" => format!("{to}'s harness cannot confirm a turn"),
            "io-error" => "the pane connection broke".to_string(),
            "unsafe-text" => "the text holds characters a pane cannot take".to_string(),
            "no-binary" => "the harness program is missing".to_string(),
            t if t.starts_with("mux-send-failed") => "the pane refused the text".to_string(),
            _ => return None,
        })
    };
    let tokens = || reason.split(';').map(str::trim).filter(|t| !t.is_empty());
    tokens()
        .find_map(phrase)
        .or_else(|| tokens().next().map(|t| t.replace('-', " ")))
        .unwrap_or_default()
}

/// The selected thread row when it is a message this agent sent, with its
/// delivery state: the target of r, enter-on-parked and the copy rung.
fn selected_sent_row(b: &MessagesBoard) -> Option<Value> {
    if b.sel_thread
        .as_deref()
        .is_some_and(|s| s.starts_with("you:"))
    {
        return None;
    }
    let mine = b.sel_agent.as_deref()?;
    let row = b
        .conversation_rows()
        .get(b.cursors[2])
        .map(|r| (**r).clone())?;
    (text_of(&row, "from_key") == mine).then_some(row)
}

/// Enter on a parked sent message: one Enter into the recipient's pane, so
/// the text already waiting there submits. A resend would type it twice.
async fn submit_parked(
    view: &mut View,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
    row: &Value,
) -> Result<(), String> {
    let session = text_of(row, "to_key").to_string();
    let name = view
        .messages_board
        .as_ref()
        .map(|b| b.participant_name(&session))
        .unwrap_or_else(|| session.clone());
    let Some((pane, expected_identity)) =
        super::messages_reply::endpoint_pane(view, &name, &session)
    else {
        view.set_notice(format!(
            "submit: {name} has no pane on screen; open a portal first, or press y to copy the message"
        ));
        return Ok(());
    };
    let request_id = view.next_reply_request_id;
    view.next_reply_request_id = view.next_reply_request_id.wrapping_add(1).max(1);
    write_msg(
        sock,
        &ClientMsg::PaneInput(crate::proto::pane_input::PaneInputRequest {
            request_id,
            pane,
            expected_identity,
            bytes: b"\r".to_vec(),
        }),
    )
    .await
    .map_err(|e| format!("submit input send failed: {e}"))?;
    view.set_notice(format!(
        "submit sent to {name}; the state moves once its turn record holds the message"
    ));
    Ok(())
}

/// r on a failed sent message: run the live ladder again for the same
/// message id, off the input path, and report the result as a notice.
fn retry_selected(view: &mut View) {
    let Some(row) = view.messages_board.as_ref().and_then(selected_sent_row) else {
        return;
    };
    let state = text_of(&row, "state");
    if state != "failed" {
        view.set_notice(format!("r retries a failed message; this one is {state}"));
        return;
    }
    let id = text_of(&row, "id").to_string();
    let to = match text_of(&row, "to") {
        "" => "the recipient".to_string(),
        to => to.to_string(),
    };
    let tx = view.reply_notice_tx.clone();
    view.set_notice(format!("retrying {id}"));
    tokio::spawn(async move {
        let out = tokio::process::Command::new(crate::digest_overlay::fno_agents_bin())
            .args(["mail-threads", "retry", &id])
            .output()
            .await;
        let notice = match out {
            Ok(out) => retry_notice(&String::from_utf8_lossy(&out.stdout), &to),
            Err(e) => format!("retry could not run: {e}; press y to copy the message"),
        };
        if let Some(tx) = tx {
            let _ = tx.send(notice);
        }
    });
}

/// The notice for one retry answer: the state it reached, or the cause and
/// the next step when it failed again.
fn retry_notice(stdout: &str, to: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(stdout.trim()) else {
        return "retry gave no answer; press y to copy the message".to_string();
    };
    let reason = reason_words(text_of(&v, "reason"), to);
    match text_of(&v, "state") {
        "failed" => format!(
            "retry failed: {reason}. Next: r to try again, or y to copy it and paste it into the recipient's session"
        ),
        "refused" => format!("retry did not run: {reason}"),
        word => format!("retry: {word}"),
    }
}

/// The agent key a chat-list row resolves to, for the details modal.
fn tree_row_agent_key(row: &TreeRow) -> Option<String> {
    match row {
        TreeRow::Agent { key, .. } => Some(key.clone()),
        TreeRow::Channel(_) => None,
    }
}

/// Mouse: a tap selects; a tap on the row already selected acts like
/// Enter. The click map mirrors the paint's geometry (strip at row 0,
/// each column's header line at row 1).
pub(crate) async fn mouse(
    view: &mut View,
    rep: crate::mouse::MouseReport,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if !matches!(
        rep.kind,
        crate::proto::MouseKind::Press(crate::proto::MouseButton::Left)
    ) {
        return Ok(());
    }
    view.region_owner = super::region_focus::RegionOwner::Board;
    if rep.row == 0 {
        // The bell keeps the tab bar's right seat, painted under this strip:
        // its click routes before the words act (R15).
        if bell::button_at(view, rep.row, rep.col) {
            if matches!(rep.kind, MouseKind::Press(MouseButton::Left)) {
                apply_hit(view, ChromeHit::Bell(bell::Hit::Toggle), sock).await?;
            }
            return Ok(());
        }
        // The strip row: only its words act (R15).
        for (start, w, view_switch) in view.top_row_spans() {
            if (rep.col as usize) >= start && (rep.col as usize) < start + w {
                apply_hit(view, ChromeHit::TopRow(view_switch), sock).await?;
            }
        }
        return Ok(());
    }
    if view
        .messages_board
        .as_ref()
        .is_some_and(|b| b.detail.is_some() || b.reply.is_some())
    {
        // Modals own the pointer; clicking outside dismisses a detail card.
        if let Some(d) = view.messages_board.as_ref().and_then(|b| b.detail.as_ref()) {
            if !d.popup.render(view.term).contains(rep.row, rep.col) {
                if let Some(b) = view.messages_board.as_mut() {
                    b.detail = None;
                }
            }
        }
        return Ok(());
    }
    let (tree_w, part_w) = split(view.term.1 as usize);
    let col = if (rep.col as usize) < tree_w {
        Col::Tree
    } else if (rep.col as usize) < tree_w + part_w {
        Col::Chats
    } else {
        Col::Thread
    };
    if let Some(b) = view.messages_board.as_mut() {
        b.col = col;
    }
    let body_h = (view.term.0 as usize).saturating_sub(3);
    if rep.row == 1 {
        // A strip's own labels switch tabs (items 5, 7 and 8); the thread
        // column's title affordances were handled above.
        let hit = |span: std::ops::Range<usize>, rel: usize| span.contains(&rel);
        if view.messages_board.is_none() {
            return Ok(());
        }
        match col {
            Col::Tree => {
                // The filter row: `a All · b Broadcasts` (item 5).
                let rel = rep.col as usize;
                let spans = tab_spans(1, &["All", "Broadcasts"], 4);
                if hit(spans[0].clone(), rel) {
                    if let Some(b) = view.messages_board.as_mut() {
                        b.filter = ListFilter::All;
                        b.cursors[0] = 0;
                    }
                } else if hit(spans[1].clone(), rel) {
                    if let Some(b) = view.messages_board.as_mut() {
                        b.filter = ListFilter::Broadcasts;
                        b.cursors[0] = 0;
                    }
                }
                return Ok(());
            }
            Col::Chats => {
                let rel = (rep.col as usize).saturating_sub(tree_w);
                let spans = tab_spans(0, &["Chats", "System", "Archive"], 0);
                let tab = if hit(spans[0].clone(), rel) {
                    Some(ChatTab::Chats)
                } else if hit(spans[1].clone(), rel) {
                    Some(ChatTab::System)
                } else if hit(spans[2].clone(), rel) {
                    Some(ChatTab::Archive)
                } else {
                    None
                };
                if let Some(tab) = tab {
                    if let Some(b) = view.messages_board.as_mut() {
                        b.col2 = tab;
                        b.cursors[1] = 0;
                    }
                }
                return Ok(());
            }
            Col::Thread => {}
        }
    }
    if rep.row == 2 && col == Col::Tree {
        // The All filter's tab strip: `Agents · <sort> | Archive` (item 5).
        let hit = |span: std::ops::Range<usize>, rel: usize| span.contains(&rel);
        let Some(board) = view.messages_board.as_ref() else {
            return Ok(());
        };
        if board.filter == ListFilter::All {
            let label = match board.sort_mode {
                SortMode::Last => "Agents · last",
                SortMode::Alpha => "Agents · a-z",
            };
            let spans = tab_spans(0, &[label, "Archive"], 1);
            let rel = rep.col as usize;
            if hit(spans[0].clone(), rel) {
                if let Some(b) = view.messages_board.as_mut() {
                    b.list_tab = ListTab::Agents;
                }
            } else if hit(spans[1].clone(), rel) {
                if let Some(b) = view.messages_board.as_mut() {
                    b.list_tab = ListTab::Archive;
                }
            }
            return Ok(());
        }
    }
    let index = match col {
        Col::Tree => {
            let Some(board) = view.messages_board.as_ref() else {
                return Ok(());
            };
            // Rows start after the column's own prefix lines (strips,
            // status, skeleton or not-read note, spacer), so the row index
            // reads off the painted line against that prefix.
            let rows_len = board.tree_rows().len();
            let lines = board.tree_column(tree_w);
            let start_line = lines.len().saturating_sub(rows_len);
            let (scroll, _) = column_rect(lines.len(), body_h, start_line + board.cursors[0]);
            (rep.row as usize)
                .checked_sub(1)
                .map(|i| i + scroll)
                .and_then(|line| line.checked_sub(start_line))
        }
        Col::Chats => {
            let Some(board) = view.messages_board.as_ref() else {
                return Ok(());
            };
            let rows_len = board.active_chat_rows().len();
            let lines = board.chats_column(part_w);
            let start_line = lines.len().saturating_sub(rows_len);
            let (scroll, _) = column_rect(lines.len(), body_h, start_line + board.cursors[1]);
            (rep.row as usize)
                .checked_sub(1)
                .map(|i| i + scroll)
                .and_then(|line| line.checked_sub(start_line))
        }
        Col::Thread => {
            let (tree_w, part_w) = split(view.term.1 as usize);
            let thread_w = (view.term.1 as usize).saturating_sub(tree_w + part_w);
            let Some(board) = view.messages_board.as_ref() else {
                return Ok(());
            };
            if rep.row == 1 {
                // The title line: the name and [i] open the detail modal,
                // ... opens the chat-id popup (item 10).
                let title = board.thread_title();
                let rel = (rep.col as usize).saturating_sub(tree_w + part_w);
                let name_len = title.text.find("  [i]").unwrap_or(title.text.len());
                let at = |span: std::ops::Range<usize>| span.contains(&rel);
                if at(0..name_len) || at(name_len + 2..name_len + 5) {
                    open_detail(view);
                    return Ok(());
                }
                if at(name_len + 7..name_len + 10) {
                    open_chat_popup(view);
                }
                return Ok(());
            }
            let (lines, owners) = board.thread_lines(thread_w);
            let follow = lines
                .iter()
                .rposition(|line| line.band)
                .unwrap_or_else(|| lines.len().saturating_sub(1));
            let painted_body_h = (view.term.0 as usize).saturating_sub(2);
            let (start, _) = column_rect(lines.len(), painted_body_h, follow);
            (rep.row as usize)
                .checked_sub(1)
                .map(|i| i + start)
                .and_then(|line| owners.get(line).copied().flatten())
        }
    };
    let Some(b) = view.messages_board.as_mut() else {
        return Ok(());
    };
    let in_range = match col {
        Col::Tree => index.is_some_and(|i| i < b.tree_rows().len()),
        Col::Chats => index.is_some_and(|i| i < b.active_chat_rows().len()),
        Col::Thread => index.is_some_and(|i| i < thread_len(b)),
    };
    if !in_range {
        return Ok(());
    }
    let i = index.unwrap_or(0);
    let slot = match col {
        Col::Tree => 0,
        Col::Chats => 1,
        Col::Thread => 2,
    };
    if col == Col::Thread {
        let row = b.conversation_rows().get(i).map(|row| (**row).clone());
        b.cursors[2] = i;
        if let Some(row) = row {
            // A bubble tap selects and copies its fmail id (AC18-HP); the
            // reply composer opens from Enter, never a tap.
            let id = text_of(&row, "id").to_string();
            if !id.is_empty() {
                super::feed_detail::copy_value(view, id);
            }
        }
        return Ok(());
    }
    if b.cursors[slot] != i {
        b.cursors[slot] = i;
        return Ok(());
    }
    act(view, sock).await
}

#[cfg(test)]
#[path = "tests/messages_fixture.rs"]
mod fixtures;

fn board_now() -> u64 {
    chrono::Utc::now().timestamp().max(0) as u64
}
