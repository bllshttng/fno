//! The notifications bell panel: ready and settled questions beside fleet
//! announcements. Its projection reads stay off the UI loop.

use super::*;
use serde_json::Value;

/// The panel's width until the operator drags its border; persisted after.
const DEFAULT_W: u16 = 48;
/// The narrowest drag; the tab strip still reads at this width.
const MIN_W: u16 = 32;

/// The three widths the panel takes: the dragged width, the wide
/// three-fifths toggle, and the whole terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Size {
    Normal,
    Wide,
    Full,
}

pub(crate) struct Panel {
    pub(super) open: bool,
    projection: Option<Value>,
    error: Option<String>,
    inflight: bool,
    last_read: Option<Instant>,
    generation: u64,
    pub(super) selected: usize,
    esc: Vec<u8>,
    unread: bool,
    pub(super) tab: Tab,
    /// The operator's dragged width inside the Normal size: loaded from
    /// mux-view.json at startup, persisted after a drag.
    width: u16,
    /// Which of the three widths paints now; holds across open and close.
    pub(super) size: Size,
    /// The size `F` returns to.
    restore: Size,
    /// The width at grab while a border drag is live.
    drag_from: Option<u16>,
    /// The Questions tab's marked rows, sent by `c` and dropped on close.
    pub(super) marks: std::collections::BTreeSet<String>,
}

#[derive(Clone)]
enum Row {
    Header(String),
    Question(String),
    Clear(usize),
    Announcement(String),
    Info(String),
}

#[derive(Debug, Clone)]
pub(super) enum Hit {
    Toggle,
    Focus,
    Clear,
    Question(String),
    Tab(Tab),
}

/// The strip and its order (the 2026-10-05 tabs ruling): System holds
/// machine and fleet alerts so they leave Announcements; All merges the
/// three, newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Tab {
    All,
    Questions,
    Announcements,
    System,
}

const TABS: [Tab; 4] = [Tab::All, Tab::Questions, Tab::Announcements, Tab::System];

impl Tab {
    fn label(self) -> &'static str {
        match self {
            Tab::All => "All",
            Tab::Questions => "Questions",
            Tab::Announcements => "Announcements",
            Tab::System => "System",
        }
    }
}

/// Columns the panel covers on a `cols`-wide terminal. The clamp is
/// transient: `width` keeps the operator's choice across a shrink and grow.
fn panel_width(view: &View, cols: usize) -> usize {
    let max = cols.saturating_sub(1).max(1);
    match view.bell.size {
        Size::Full => cols,
        Size::Wide => (cols * 3 / 5).max(MIN_W as usize).min(max),
        Size::Normal => (view.bell.width as usize).max(MIN_W as usize).min(max),
    }
}

impl Panel {
    pub(crate) fn initial() -> Self {
        Self {
            open: false,
            projection: None,
            error: None,
            inflight: false,
            last_read: None,
            generation: 0,
            selected: 0,
            esc: Vec::new(),
            unread: false,
            tab: Tab::All,
            width: crate::view_store::load_bell_width().unwrap_or(DEFAULT_W),
            size: Size::Normal,
            restore: Size::Normal,
            drag_from: None,
            marks: std::collections::BTreeSet::new(),
        }
    }
}

pub(crate) type Tx = tokio::sync::mpsc::UnboundedSender<(u64, Result<Value, String>)>;

/// Open the panel and hand back the generation a later
/// [`apply`] must carry: the snapshot seam pairs this with one synchronous
/// gather where the live loop would kick one.
pub(crate) fn open(view: &mut View) -> u64 {
    let generation = view.bell.generation.wrapping_add(1);
    view.bell.open = true;
    view.bell.generation = generation;
    view.bell.last_read = None;
    view.questions_kick_at = None;
    if let Some(projection) = view.bell.projection.as_ref() {
        mark_seen(projection);
        view.bell.unread = false;
    }
    view.feed = None;
    view.region_owner = super::region_focus::RegionOwner::Pane;
    generation
}

pub(crate) fn close(view: &mut View) {
    if view.bell.open {
        view.bell.open = false;
        view.bell.generation = view.bell.generation.wrapping_add(1);
        view.bell.drag_from = None;
        view.bell.marks.clear();
    }
}

pub(crate) fn toggle(view: &mut View) {
    if view.bell.open {
        close(view);
    } else {
        open(view);
    }
}

/// The bell's glyph: a single-column Nerd Font bell (the operator's
/// 2026-10-04 ask). A TUI cannot detect font coverage, so a terminal without
/// the font shows tofu - the fallback (`\u{2407}`, the ASCII bell control
/// picture) is a one-line const swap, and the live screenshot step is what
/// catches it.
const BELL_GLYPH: char = '\u{f0f3}';

pub(crate) fn button_label(view: &View) -> String {
    let mut label = String::from(BELL_GLYPH);
    let count = ready_count(view);
    if count > 0 {
        let digits = count.to_string();
        for ch in digits.chars() {
            label.push(match ch {
                '0' => '⁰',
                '1' => '¹',
                '2' => '²',
                '3' => '³',
                '4' => '⁴',
                '5' => '⁵',
                '6' => '⁶',
                '7' => '⁷',
                '8' => '⁸',
                '9' => '⁹',
                _ => ch,
            });
        }
    }
    if has_unread(view) {
        label.push('•');
    }
    label
}

/// The bell's seat: the far right of the mux top bar (terminal row 0), one
/// column in from the edge, so its last cell sits beside the pane border's
/// top-right corner instead of on it. Shows whether or not the sideline is
/// open. Full-terminal columns; a transient notice paints under it, never
/// over.
pub(crate) fn button_range(view: &View) -> std::ops::Range<usize> {
    let width = unicode_width::UnicodeWidthStr::width(button_label(view).as_str());
    let cols = view.term.1 as usize;
    cols.saturating_sub(width + 1)..cols.saturating_sub(1)
}

pub(super) fn button_at(view: &View, row: u16, col: u16) -> bool {
    row == 0 && button_range(view).contains(&(col as usize))
}

pub(crate) fn paint_button(view: &View, cells: &mut [Cell], cols: usize) {
    let range = button_range(view);
    paint(
        cells,
        cols,
        0,
        range.start,
        range.len(),
        &button_label(view),
        view.theme.brand,
        true,
    );
}

pub(crate) fn maybe_kick(view: &mut View, tx: &Tx) {
    if view.panel_w() == 0 {
        return;
    }
    let panel = &mut view.bell;
    if panel.inflight
        || panel
            .last_read
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(30))
    {
        return;
    }
    panel.inflight = true;
    panel.last_read = Some(Instant::now());
    let generation = panel.generation;
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send((generation, crate::messages_model::gather().await));
    });
}

pub(crate) fn apply(view: &mut View, generation: u64, result: Result<Value, String>) {
    let panel = &mut view.bell;
    panel.inflight = false;
    if panel.generation != generation {
        return;
    }
    match result {
        Ok(value) => {
            panel.error = None;
            panel.unread = has_unread_in(&value);
            panel.projection = Some(value);
        }
        Err(error) => panel.error = Some(error),
    }
    clamp_selection(view);
}

fn mark_seen(projection: &Value) {
    if let Some(ts) = projection
        .get("announcements")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|row| row.get("ts").and_then(Value::as_str))
        .filter_map(|ts| timestamp_key(ts).map(|epoch| (epoch, ts)))
        .max_by_key(|(epoch, _)| *epoch)
        .map(|(_, ts)| ts)
    {
        crate::view_store::save_bell_seen_at(ts);
    }
}

fn rows(view: &View) -> Vec<Row> {
    match view.bell.tab {
        Tab::Questions => question_rows(view),
        Tab::Announcements => announcement_rows(view, false),
        Tab::System => announcement_rows(view, true),
        Tab::All => all_rows(view),
    }
}

/// The Questions tab: the cached fold, open questions, then answered with
/// the clear row. A failed read keeps the last good fold and says so; it
/// never blanks the board to 0.
fn question_rows(view: &View) -> Vec<Row> {
    let mut out = Vec::new();
    let merged = view.questions_merged();
    if view.questions_degraded && view.questions_index.is_none() {
        out.push(Row::Info(format!(
            "stale: {} \u{b7} showing the last read",
            view.questions_degraded_reason
                .as_deref()
                .unwrap_or("read failed")
        )));
    } else if view.questions_fold.is_none() {
        out.push(Row::Info("loading questions...".into()));
        out.push(Row::Info(
            "\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}"
                .into(),
        ));
    }
    if view.questions_inflight && view.questions_fold.is_some() {
        out.push(Row::Info("refreshing...".into()));
    }
    let mut open: Vec<_> = merged
        .as_ref()
        .into_iter()
        .flat_map(|fold| fold.items.iter())
        .filter(|q| q.state == "open" && !q.settled)
        .collect();
    open.sort_by_key(|q| !q.ready);
    for q in open.iter() {
        out.push(Row::Question(q.id.clone()));
    }
    if merged.is_some() && open.is_empty() {
        out.push(Row::Info("no open questions".into()));
    }
    let settled: Vec<_> = merged
        .as_ref()
        .into_iter()
        .flat_map(|fold| fold.items.iter())
        .filter(|q| q.settled)
        .collect();
    if !settled.is_empty() {
        out.push(Row::Header(format!("Answered ({})", settled.len())));
        for q in settled.iter() {
            out.push(Row::Question(q.id.clone()));
        }
        out.push(Row::Clear(settled.len()));
    }
    out
}

/// One row per subject, newest first, expired rows hidden. The subject is
/// the summary with its digits collapsed, so progress updates of one story
/// ("busy for 40 minutes", "busy for 71 minutes") hold one row.
fn announcement_rows(view: &View, system: bool) -> Vec<Row> {
    let mut out = Vec::new();
    let Some(items) = view
        .bell
        .projection
        .as_ref()
        .and_then(|p| p.get("announcements"))
        .and_then(Value::as_array)
    else {
        if let Some(error) = view.bell.error.as_ref() {
            out.push(Row::Info(format!("unavailable: {error}")));
        } else {
            out.push(Row::Info("loading announcements...".into()));
            out.push(Row::Info(
                "\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}\u{2582}"
                    .into(),
            ));
        }
        return out;
    };
    let rows = deduped(
        items
            .iter()
            .filter(|item| standing(item))
            .filter(|item| item.get("system").and_then(Value::as_bool) == Some(system))
            .collect::<Vec<_>>(),
    );
    if rows.is_empty() {
        out.push(Row::Info("nothing here".into()));
    }
    for item in rows {
        push_announcement(view, &mut out, item);
    }
    if let Some(error) = view.bell.error.as_ref() {
        out.push(Row::Info(format!("stale: {error}")));
    }
    out
}

/// The All tab: open questions and deduped announcements in one stream,
/// newest first (the tabs ruling).
fn all_rows(view: &View) -> Vec<Row> {
    let mut out = Vec::new();
    if view.questions_degraded && view.questions_index.is_none() {
        out.push(Row::Info(format!(
            "stale: {} \u{b7} showing the last read",
            view.questions_degraded_reason
                .as_deref()
                .unwrap_or("read failed")
        )));
    } else if view.questions_merged().is_none() {
        out.push(Row::Info("loading questions...".into()));
    }
    let mut stream: Vec<(i64, Vec<Row>)> = Vec::new();
    for q in view
        .questions_merged()
        .as_ref()
        .into_iter()
        .flat_map(|fold| fold.items.iter())
        .filter(|q| q.state == "open" && !q.settled)
    {
        let ts = timestamp_key(&q.created_at).unwrap_or(0);
        stream.push((ts, vec![Row::Question(q.id.clone())]));
    }
    if let Some(items) = view
        .bell
        .projection
        .as_ref()
        .and_then(|p| p.get("announcements"))
        .and_then(Value::as_array)
    {
        for item in deduped(
            items
                .iter()
                .filter(|item| standing(item))
                .collect::<Vec<_>>(),
        ) {
            let ts = timestamp_of(item).unwrap_or(0);
            let mut group = Vec::new();
            push_announcement(view, &mut group, item);
            stream.push((ts, group));
        }
    }
    stream.sort_by(|a, b| b.0.cmp(&a.0));
    if stream.is_empty()
        && !(view.questions_degraded && view.questions_index.is_none())
        && view.questions_merged().is_some()
    {
        out.push(Row::Info("nothing here".into()));
    }
    for (_, group) in stream {
        out.extend(group);
    }
    out
}

fn push_announcement(view: &View, out: &mut Vec<Row>, item: &Value) {
    let sender = item.get("from").and_then(Value::as_str).unwrap_or("system");
    let summary = item.get("summary").and_then(Value::as_str).unwrap_or("");
    let badge = if item.get("system").and_then(Value::as_bool) == Some(true) {
        "\u{2699} "
    } else {
        ""
    };
    out.push(Row::Announcement(format!("{badge}{sender}: {summary}")));
    if let Some(body) = item
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        let width = panel_width(view, view.term.1 as usize)
            .saturating_sub(4)
            .max(1);
        out.extend(
            wrap_body(body, width)
                .into_iter()
                .map(|line| Row::Info(format!("  {line}"))),
        );
    }
    if let Some(expires) = item.get("expires").and_then(Value::as_str) {
        out.push(Row::Info(format!("  expires {expires}")));
    }
}

/// A row stands until its `expires`; the projection's own check rides a
/// cached read, so the panel re-reads the clock and hides the dead rows.
fn standing(item: &Value) -> bool {
    match item
        .get("expires")
        .and_then(Value::as_str)
        .and_then(timestamp_key)
    {
        Some(expires) => expires > chrono::Utc::now().timestamp_millis(),
        None => true,
    }
}

fn subject_of(item: &Value) -> String {
    item.get("summary")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

fn deduped<'a>(items: Vec<&'a Value>) -> Vec<&'a Value> {
    let mut newest: Vec<(String, &'a Value)> = Vec::new();
    for item in items {
        let subject = subject_of(item);
        match newest.iter_mut().find(|(s, _)| *s == subject) {
            Some((_, row)) if timestamp_of(*row) >= timestamp_of(item) => {}
            Some(slot) => slot.1 = item,
            None => newest.push((subject, item)),
        }
    }
    newest.into_iter().map(|(_, row)| row).collect()
}

/// The strip's counts, in TABS order. An unread board reads as an ellipsis,
/// never 0.
fn tab_counts(view: &View) -> [String; 4] {
    let unread = "…".to_string();
    let open = view.questions_merged().map(|fold| {
        fold.items
            .iter()
            .filter(|q| q.state == "open" && !q.settled)
            .count()
    });
    let Some((ann, sys)) = split_announcements(view) else {
        return [unread.clone(), unread.clone(), unread.clone(), unread];
    };
    let q = open.map_or(unread.clone(), |n| n.to_string());
    let total = open.unwrap_or(0) + ann + sys;
    [total.to_string(), q, ann.to_string(), sys.to_string()]
}

fn split_announcements(view: &View) -> Option<(usize, usize)> {
    let items = view
        .bell
        .projection
        .as_ref()
        .and_then(|p| p.get("announcements"))
        .and_then(Value::as_array)?;
    let mut ann = 0;
    let mut sys = 0;
    for item in deduped(items.iter().filter(|item| standing(item)).collect()) {
        if item.get("system").and_then(Value::as_bool) == Some(true) {
            sys += 1;
        } else {
            ann += 1;
        }
    }
    Some((ann, sys))
}

/// Each tab's columns on row 0, for the click mapper. The spans derive from
/// the same labels the draw paints (pass the one `tab_counts` read), so the
/// two never drift.
fn tab_spans(view: &View, counts: &[String; 4]) -> Vec<(Tab, std::ops::Range<usize>)> {
    let width = panel_width(view, view.term.1 as usize);
    let x0 = view.term.1 as usize - width;
    let mut col = x0 + 4;
    TABS.iter()
        .enumerate()
        .map(|(i, tab)| {
            let label = format!("{} {}", tab.label(), counts[i]);
            let span = col..col + label.chars().count();
            col = span.end + 1;
            (*tab, span)
        })
        .collect()
}

pub(super) fn hit(view: &View, row: u16, col: u16) -> Option<ChromeHit> {
    let panel = &view.bell;
    if !panel.open {
        return None;
    }
    let width = panel_width(view, view.term.1 as usize);
    let x0 = view.term.1 as usize - width;
    if (col as usize) < x0 || row as usize >= view.term.0 as usize {
        return None;
    }
    if row == 0 {
        let col = col as usize;
        let counts = tab_counts(view);
        return Some(ChromeHit::Bell(
            match tab_spans(view, &counts)
                .into_iter()
                .find(|(_, span)| span.contains(&col))
            {
                Some((tab, _)) => Hit::Tab(tab),
                None => Hit::Focus,
            },
        ));
    }
    if row as usize == view.term.0.saturating_sub(1) as usize {
        return Some(ChromeHit::Bell(Hit::Focus));
    }
    if col as usize == x0 {
        return Some(ChromeHit::Bell(Hit::Focus));
    }
    let all = rows(view);
    let visible = (view.term.0 as usize).saturating_sub(2);
    let start = panel.selected.saturating_sub(visible.saturating_sub(1));
    match all.get(start + row as usize - 1)? {
        Row::Question(id) => Some(ChromeHit::Bell(Hit::Question(id.clone()))),
        Row::Clear(_) => Some(ChromeHit::Bell(Hit::Clear)),
        _ => Some(ChromeHit::Bell(Hit::Focus)),
    }
}

/// Begin a width drag when a left press lands on the border column. Full
/// screen has no border to grab.
pub(super) fn begin_drag(view: &mut View, col: u16) -> bool {
    if !view.bell.open || view.bell.size == Size::Full {
        return false;
    }
    let cols = view.term.1 as usize;
    if col as usize != cols - panel_width(view, cols) {
        return false;
    }
    view.bell.drag_from = Some(view.bell.width);
    true
}

/// Set the width from the dragged border column; true when the painted
/// width changed. A drag out of Wide drops to Normal at the dragged width.
fn drag_to(view: &mut View, col: u16) -> bool {
    let cols = view.term.1 as usize;
    let max = cols.saturating_sub(1);
    if (MIN_W as usize) > max {
        return false;
    }
    let before = panel_width(view, cols);
    let want = cols.saturating_sub(col as usize).clamp(MIN_W as usize, max);
    view.bell.size = Size::Normal;
    view.bell.width = want as u16;
    clamp_selection(view);
    panel_width(view, cols) != before
}

/// End the drag; persist the width when it moved.
fn end_drag(view: &mut View) {
    if let Some(from) = view.bell.drag_from.take() {
        if view.bell.width != from {
            crate::view_store::save_bell_width(view.bell.width);
        }
    }
}

/// The first question row's id, for the snapshot seam's marked-row shot.
pub(super) fn first_question_id(view: &View) -> Option<String> {
    question_rows(view).into_iter().find_map(|r| match r {
        Row::Question(id) => Some(id),
        _ => None,
    })
}

/// The live drag owns every mouse event: Drag(Left) moves the border,
/// anything else ends it. False when no drag is live, so a panel with no
/// drag falls through to the hit and close lines the block ends with.
pub(super) fn drag_mouse(view: &mut View, col: u16, kind: MouseKind) -> bool {
    if view.bell.drag_from.is_none() {
        return false;
    }
    match kind {
        MouseKind::Drag(MouseButton::Left) => {
            drag_to(view, col);
            true
        }
        _ => {
            end_drag(view);
            true
        }
    }
}

pub(crate) fn apply_hit(view: &mut View, hit: Hit) {
    if let Some(projection) = view.bell.projection.as_ref() {
        mark_seen(projection);
        view.bell.unread = false;
    }
    match hit {
        Hit::Toggle => toggle(view),
        Hit::Tab(tab) => {
            view.bell.tab = tab;
            view.bell.selected = 0;
        }
        Hit::Focus => {}
        Hit::Clear if !view.question_acting => {
            view.question_clear_settled = true;
            view.question_acting = true;
        }
        Hit::Clear => {}
        Hit::Question(id) => view.open_detail_on(&id),
    }
}

/// The merged-view item for `id`, honoring the same index/fold precedence
/// `questions_merged` renders with: the healthy projection wins a shared id,
/// the fresh index wins while the projection is degraded, and an id only one
/// leg holds comes from wherever it lives. Reads the two legs by reference -
/// `questions_merged` itself would clone every fold on every row.
fn question_item<'a>(view: &'a View, id: &str) -> Option<&'a crate::needs_overlay::QuestionItem> {
    let index = view.questions_index.as_ref();
    let fold = view.questions_fold.as_ref();
    let find = |f: Option<&'a crate::needs_overlay::QuestionsFold>| {
        f.and_then(|f| f.items.iter().find(|q| q.id == id))
    };
    let (primary, secondary) = if view.questions_degraded {
        (index, fold)
    } else {
        (fold, index)
    };
    find(primary).or_else(|| find(secondary))
}

fn question_title(view: &View, id: &str) -> String {
    let Some(q) = question_item(view, id) else {
        return id.to_string();
    };
    let title = q.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    // A question with no text names its asker and node instead of a bare
    // id; the id stays the last resort for a row that names neither.
    let asker = q.asker.as_ref().map(|a| a.handle.as_str()).unwrap_or("");
    let node = q.node.as_deref().unwrap_or("");
    match (!asker.is_empty(), !node.is_empty()) {
        (true, true) => format!("from {asker} on {node}"),
        (true, false) => format!("from {asker}"),
        (false, true) => format!("on {node}"),
        (false, false) => id.to_string(),
    }
}

/// Whether the merged fold still lists `id` as an open question: the
/// archive door refuses settled ids, so marks and sends gate on this.
fn question_open(view: &View, id: &str) -> bool {
    view.questions_merged().is_some_and(|fold| {
        fold.items
            .iter()
            .any(|q| q.id == id && q.state == "open" && !q.settled)
    })
}

fn wrap_body(body: &str, width: usize) -> Vec<String> {
    let sanitized: String = body
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut cols = 0usize;
    for word in sanitized.split_whitespace() {
        let word_cols = unicode_width::UnicodeWidthStr::width(word);
        if !line.is_empty() && cols + 1 + word_cols > width {
            lines.push(std::mem::take(&mut line));
            cols = 0;
        }
        if word_cols <= width {
            if !line.is_empty() {
                line.push(' ');
                cols += 1;
            }
            line.push_str(word);
            cols += word_cols;
            continue;
        }
        for ch in word.chars() {
            let ch_cols = unicode_width::UnicodeWidthChar::width(ch)
                .unwrap_or(0)
                .max(1);
            if cols + ch_cols > width {
                lines.push(std::mem::take(&mut line));
                cols = 0;
            }
            line.push(ch);
            cols += ch_cols;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

pub(crate) fn keys(view: &mut View, bytes: &[u8]) {
    if !bytes.is_empty() {
        if let Some(projection) = view.bell.projection.as_ref() {
            mark_seen(projection);
            view.bell.unread = false;
        }
    }
    let mut esc = std::mem::take(&mut view.bell.esc);
    let keys = fold_selector_keys(&mut esc, bytes);
    view.bell.esc = esc;
    for byte in keys {
        match byte {
            0x1b | b'q' | b'B' => {
                close(view);
                return;
            }
            b'\t' => {
                let i = TABS.iter().position(|t| *t == view.bell.tab).unwrap_or(0);
                view.bell.tab = TABS[(i + 1) % TABS.len()];
                view.bell.selected = 0;
            }
            b'1'..=b'4' => {
                view.bell.tab = TABS[(byte - b'1') as usize];
                view.bell.selected = 0;
            }
            b'j' => {
                let len = rows(view).len();
                view.bell.selected = (view.bell.selected + 1).min(len.saturating_sub(1));
            }
            b'k' => {
                view.bell.selected = view.bell.selected.saturating_sub(1);
            }
            b'w' => {
                view.bell.size = if view.bell.size == Size::Wide {
                    Size::Normal
                } else {
                    Size::Wide
                };
                // A wider panel rewraps announcement bodies into fewer
                // rows, so the cursor can fall off the end.
                clamp_selection(view);
            }
            b'F' => {
                if view.bell.size == Size::Full {
                    view.bell.size = view.bell.restore;
                } else {
                    view.bell.restore = view.bell.size;
                    view.bell.size = Size::Full;
                }
                clamp_selection(view);
            }
            b' ' => {
                let selected = view.bell.selected;
                if view.bell.tab == Tab::Questions {
                    if let Some(Row::Question(id)) = rows(view).get(selected).cloned() {
                        // Only an open question marks: the archive door
                        // refuses settled ids, so a marked Answered row
                        // would turn c into a refusal instead of a clear.
                        if question_open(view, &id) {
                            if !view.bell.marks.remove(&id) {
                                view.bell.marks.insert(id);
                            }
                            // A mark advances the cursor, so space space
                            // space marks three rows.
                            view.bell.selected =
                                (view.bell.selected + 1).min(rows(view).len().saturating_sub(1));
                        }
                    }
                }
            }
            b'c' => {
                let listed: Vec<String> = question_rows(view)
                    .into_iter()
                    .filter_map(|r| match r {
                        Row::Question(id) => Some(id),
                        _ => None,
                    })
                    .filter(|id| question_open(view, id))
                    .collect();
                view.bell.marks.retain(|id| listed.contains(id));
                if !view.bell.marks.is_empty() && !view.question_acting {
                    let ids: Vec<String> = listed
                        .iter()
                        .filter(|id| view.bell.marks.contains(*id))
                        .cloned()
                        .collect();
                    view.question_archive = Some(ids);
                    view.question_acting = true;
                    view.bell.marks.clear();
                } else if rows(view).iter().any(|r| matches!(r, Row::Clear(_)))
                    && !view.question_acting
                {
                    view.question_clear_settled = true;
                    view.question_acting = true;
                }
            }
            b'\r' | b'\n' => {
                let selected = view.bell.selected;
                match rows(view).get(selected).cloned() {
                    Some(Row::Question(id)) => view.open_detail_on(&id),
                    Some(Row::Clear(_)) if !view.question_acting => {
                        view.question_clear_settled = true;
                        view.question_acting = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

pub(super) fn clamp_selection(view: &mut View) {
    view.bell.selected = view.bell.selected.min(rows(view).len().saturating_sub(1));
}

pub(crate) fn draw(view: &View, cells: &mut [Cell], rows_n: usize, cols: usize) {
    let panel = &view.bell;
    if !panel.open {
        return;
    }
    if rows_n == 0 || cols == 0 {
        return;
    }
    let width = panel_width(view, cols);
    let x0 = cols - width;
    let mut lines = rows(view);
    if lines.is_empty() {
        lines.push(Row::Info("nothing here".into()));
    }
    let visible = rows_n.saturating_sub(2);
    let start = panel.selected.saturating_sub(visible.saturating_sub(1));
    for r in 0..rows_n {
        let y = r * cols;
        cells[y + x0] = Cell {
            c: '│',
            fg: view.theme.brand,
            bg: Color::Default,
            flags: 0,
        };
        for c in x0 + 1..cols {
            cells[y + c] = Cell {
                c: ' ',
                fg: Color::Default,
                bg: Color::Default,
                flags: 0,
            };
        }
        if r == 0 {
            paint(
                cells,
                cols,
                r,
                x0 + 2,
                2,
                &BELL_GLYPH.to_string(),
                view.theme.brand,
                true,
            );
            let counts = tab_counts(view);
            for (tab, span) in tab_spans(view, &counts) {
                let i = TABS.iter().position(|t| t == &tab).unwrap_or(0);
                let (fg, bold) = if view.bell.tab == tab {
                    (view.theme.brand, true)
                } else {
                    (crate::theme::dim_fg(&view.theme), false)
                };
                paint(
                    cells,
                    cols,
                    r,
                    span.start,
                    span.len(),
                    &format!("{} {}", tab.label(), counts[i]),
                    fg,
                    bold,
                );
            }
        } else if r == rows_n - 1 {
            let footer = if view.bell.tab == Tab::Questions && !view.bell.marks.is_empty() {
                format!(
                    "{} marked · c clear marked · space mark · Esc close",
                    view.bell.marks.len()
                )
            } else {
                "1-4/Tab tab · j/k move · Enter open · space mark · c clear · w wide · F full · q close".to_string()
            };
            paint(
                cells,
                cols,
                r,
                x0 + 2,
                width.saturating_sub(2),
                &footer,
                Color::Default,
                false,
            );
        } else if let Some(row) = lines.get(start + r - 1) {
            let (label, color, bold) = match row {
                Row::Header(s) => (s.clone(), view.theme.brand, true),
                Row::Question(id) => {
                    let (glyph, color) = if view.bell.marks.contains(id) {
                        ("● ", view.theme.brand)
                    } else {
                        ("? ", Color::Default)
                    };
                    (format!("{glyph}{}", question_title(view, id)), color, true)
                }
                Row::Clear(n) => (format!("Clear all {n} answered"), view.theme.brand, true),
                Row::Announcement(s) => (format!("• {s}"), Color::Default, false),
                Row::Info(s) => (s.clone(), crate::theme::dim_fg(&view.theme), false),
            };
            paint(
                cells,
                cols,
                r,
                x0 + 2,
                width.saturating_sub(2),
                &label,
                color,
                bold,
            );
            if start + r - 1 == panel.selected {
                let (fg, bg, flags) = crate::cursor::cursor_style(&view.theme);
                for c in (x0 + 1)..cols {
                    let cell = &mut cells[y + c];
                    cell.fg = fg;
                    cell.bg = bg;
                    cell.flags = flags;
                }
            }
        }
    }
}

fn paint(
    cells: &mut [Cell],
    cols: usize,
    row: usize,
    start: usize,
    width: usize,
    text: &str,
    fg: Color,
    bold: bool,
) {
    let mut col = start;
    for ch in text.chars() {
        let ch = if ch.is_control() { ' ' } else { ch };
        let w = unicode_width::UnicodeWidthChar::width(ch)
            .unwrap_or(0)
            .max(1);
        if col + w > start + width || col + w > cols {
            break;
        }
        cells[row * cols + col] = Cell {
            c: ch,
            fg,
            bg: Color::Default,
            flags: if bold { cell_flags::BOLD } else { 0 },
        };
        if w == 2 && col + 1 < cols {
            cells[row * cols + col + 1] = Cell {
                c: ' ',
                fg,
                bg: Color::Default,
                flags: cell_flags::WIDE_SPACER,
            };
        }
        col += w;
    }
}

pub(crate) fn ready_count(view: &View) -> usize {
    view.questions_merged()
        .as_ref()
        .map(|fold| {
            fold.items
                .iter()
                .filter(|q| q.state == "open" && q.ready && !q.settled)
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn has_unread(view: &View) -> bool {
    view.bell.unread
}

fn has_unread_in(projection: &Value) -> bool {
    let Some(items) = projection.get("announcements").and_then(Value::as_array) else {
        return false;
    };
    let seen = crate::view_store::load_bell_seen_at().unwrap_or_default();
    let seen_at = timestamp_key(&seen);
    items
        .iter()
        .any(|item| timestamp_of(item).is_some_and(|ts| seen_at.is_none_or(|seen| ts > seen)))
}

fn timestamp_of(row: &Value) -> Option<i64> {
    row.get("ts")
        .and_then(Value::as_str)
        .and_then(timestamp_key)
}

fn timestamp_key(ts: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|date| date.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::super::tests::view_with_agents;
    use super::*;

    fn view_with_announcements(items: Vec<Value>) -> View {
        let mut v = view_with_agents(vec![]);
        v.term = (24, 100);
        v.bell.projection = Some(serde_json::json!({ "announcements": items }));
        v
    }

    fn ann(ts: &str, summary: &str, system: bool, expires: &str) -> Value {
        serde_json::json!({
            "ts": ts,
            "from": "fno/fleet-incident",
            "summary": summary,
            "body": "",
            "expires": expires,
            "system": system,
        })
    }

    fn announcement_texts(view: &View) -> Vec<String> {
        rows(view)
            .into_iter()
            .filter_map(|r| match r {
                Row::Announcement(s) => Some(s),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn tabs_dedup_and_split_rows() {
        let far = "2027-01-01T00:00:00Z";
        let v = view_with_announcements(vec![
            ann("2026-10-05T11:00:00Z", "busy for 40 minutes", true, far),
            ann("2026-10-05T12:00:00Z", "busy for 71 minutes", true, far),
            ann(
                "2026-10-05T10:00:00Z",
                "a dead story",
                true,
                "2020-01-01T00:00:00Z",
            ),
            ann("2026-10-05T09:00:00Z", "an agent notice", false, far),
        ]);
        // All: every deduped announcement, newest first. The expired row
        // and the deduped twin never render.
        let texts = announcement_texts(&v);
        assert_eq!(texts.len(), 2, "dedupe + expiry: {texts:?}");
        assert!(texts[0].contains("busy for 71"), "newest first: {texts:?}");
        assert!(
            texts.iter().all(|t| !t.contains("for 40")),
            "one row per subject"
        );
        assert!(
            texts.iter().all(|t| !t.contains("dead")),
            "expired rows hide"
        );

        // System: machine and fleet alerts only, still deduped.
        let mut system = view_with_announcements(vec![
            ann("2026-10-05T11:00:00Z", "busy for 40 minutes", true, far),
            ann("2026-10-05T12:00:00Z", "busy for 71 minutes", true, far),
            ann("2026-10-05T09:00:00Z", "an agent notice", false, far),
        ]);
        system.bell.tab = Tab::System;
        let texts = announcement_texts(&system);
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("busy for 71"));

        // Announcements: the non-system rows.
        system.bell.tab = Tab::Announcements;
        let texts = announcement_texts(&system);
        assert_eq!(texts.len(), 1, "{texts:?}");
        assert!(texts[0].contains("an agent notice"));

        // Counts: the unread question board reads as an ellipsis, never 0.
        assert_eq!(
            tab_counts(&system),
            [
                "2".to_string(),
                "\u{2026}".to_string(),
                "1".to_string(),
                "1".to_string()
            ]
        );

        // The strip spans land where the draw paints the labels.
        let spans = tab_spans(&system, &tab_counts(&system));
        assert_eq!(spans.len(), 4);
        assert!(spans[0].1.start >= 4, "the glyph keeps its seat");
        assert_eq!(spans[0].0, Tab::All);
        assert_eq!(spans[3].0, Tab::System);

        // Keys switch panels: 1-4 jump, Tab cycles and wraps.
        let mut v = view_with_announcements(vec![]);
        keys(&mut v, b"3");
        assert_eq!(v.bell.tab, Tab::Announcements);
        keys(&mut v, b"\t");
        assert_eq!(v.bell.tab, Tab::System);
        keys(&mut v, b"\t");
        assert_eq!(v.bell.tab, Tab::All, "Tab wraps");
        keys(&mut v, b"2");
        assert_eq!(v.bell.tab, Tab::Questions);
        keys(&mut v, b"1");
        assert_eq!(v.bell.tab, Tab::All);
    }

    #[test]
    fn stale_banner_yields_to_the_index() {
        let mut v = view_with_agents(vec![]);
        v.term = (24, 100);
        v.questions_index = Some(crate::needs_overlay::QuestionsFold {
            items: vec![crate::needs_overlay::QuestionItem {
                id: "q-1".into(),
                title: "from the index".into(),
                state: "open".into(),
                ready: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        v.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![crate::needs_overlay::QuestionItem {
                id: "q-1".into(),
                title: "from the projection".into(),
                state: "open".into(),
                ready: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        v.questions_degraded = true;
        v.questions_degraded_reason = Some("timed out".into());
        v.bell.tab = Tab::Questions;
        let stale = |view: &View| {
            rows(view)
                .iter()
                .any(|r| matches!(r, Row::Info(s) if s.starts_with("stale:")))
        };
        assert!(!stale(&v), "the index backs the list; no stale banner");
        v.questions_index = None;
        assert!(stale(&v), "no index: the banner says so");
    }

    #[test]
    fn row_titles_read_the_merged_view_and_name_the_asker_before_the_id() {
        let item = |id: &str, title: &str| crate::needs_overlay::QuestionItem {
            id: id.into(),
            title: title.into(),
            state: "open".into(),
            ready: true,
            ..Default::default()
        };
        let mut v = view_with_agents(vec![]);
        v.term = (24, 100);
        v.bell.tab = Tab::Questions;
        v.questions_index = Some(crate::needs_overlay::QuestionsFold {
            items: vec![
                item("q-shared", "from the index"),
                item("q-index-only", "index only"),
            ],
            ..Default::default()
        });
        v.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![
                item("q-shared", "from the projection"),
                crate::needs_overlay::QuestionItem {
                    id: "q-silent".into(),
                    asker: Some(crate::needs_overlay::QuestionAsker {
                        handle: "daemon".into(),
                        ..Default::default()
                    }),
                    node: Some("job-7".into()),
                    state: "open".into(),
                    ..Default::default()
                },
                crate::needs_overlay::QuestionItem {
                    id: "q-node-only".into(),
                    node: Some("job-7".into()),
                    state: "open".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        // The healthy projection wins a shared id; an id only the index
        // holds still reads its title.
        assert_eq!(question_title(&v, "q-shared"), "from the projection");
        assert_eq!(question_title(&v, "q-index-only"), "index only");
        // A question with no text names its asker and node, never a bare id.
        assert_eq!(question_title(&v, "q-silent"), "from daemon on job-7");
        assert_eq!(question_title(&v, "q-node-only"), "on job-7");
        // Degraded: the fresh index outvotes the stale projection.
        v.questions_degraded = true;
        assert_eq!(question_title(&v, "q-shared"), "from the index");
        // An id no leg holds keeps the honest last resort.
        assert_eq!(question_title(&v, "q-gone"), "q-gone");
    }

    #[test]
    fn the_cursor_row_inverts_like_the_focused_settings_row() {
        let mut v = view_with_agents(vec![]);
        v.term = (24, 100);
        v.bell.open = true;
        v.bell.tab = Tab::Questions;
        v.bell.selected = 0;
        v.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![crate::needs_overlay::QuestionItem {
                id: "q-1".into(),
                title: "Hold the release until the shot lands?".into(),
                state: "open".into(),
                ready: true,
                ..Default::default()
            }],
            ..Default::default()
        });
        let cols = 100usize;
        let rows_n = 24usize;
        let blank = Cell {
            c: ' ',
            fg: Color::Default,
            bg: Color::Default,
            flags: 0,
        };
        let mut cells = vec![blank; rows_n * cols];
        draw(&v, &mut cells, rows_n, cols);
        // The selected row's first body cell carries the focus pair - the
        // invert the settings modal's focused row uses - not the band pair.
        // The panel paints at the right edge; row 1 is its first body row.
        let (efg, ebg, eflags) = crate::cursor::cursor_style(&v.theme);
        let x0 = cols - panel_width(&v, cols);
        let cell = &cells[cols + x0 + 2];
        assert_eq!(
            (cell.fg, cell.bg, cell.flags),
            (efg, ebg, eflags),
            "the cursor row carries the focus pair"
        );
        let (_, bbg, bflags) = crate::cursor::resting_band_style(&v.theme);
        assert_ne!(
            (cell.bg, cell.flags),
            (bbg, bflags),
            "not the faint band pair"
        );
    }

    #[test]
    fn panel_sizes_drag_and_rewrap() {
        // A body of one 44-, one 21- and one 20-column word: at the default
        // width the wrap packs 44 and 42 on two lines; after the drag one
        // line packs 44 + 1 + 21 = 66, so the rewrap is measurable.
        let body = format!("{} {} {}", "a".repeat(44), "b".repeat(21), "c".repeat(20));
        let mut v = view_with_announcements(vec![serde_json::json!({
            "ts": "2026-10-14T10:00:00Z",
            "from": "fno/fleet-incident",
            "summary": "rollout",
            "body": body,
            "expires": "2027-01-01T00:00:00Z",
            "system": true,
        })]);
        v.bell.open = true;
        let x0 = |v: &View| (v.term.1 as usize) - panel_width(v, v.term.1 as usize);
        let widest_info = |v: &View| {
            rows(v)
                .iter()
                .filter_map(|r| match r {
                    Row::Info(s) if s.starts_with("  ") => Some(s.chars().count()),
                    _ => None,
                })
                .max()
                .unwrap_or(0)
        };

        // AC1: the default paints 48 wide (x0 52 on 100 columns), and the
        // announcement body wraps at the painted width less 4.
        assert_eq!(x0(&v), 52, "the default panel paints 48 wide");
        assert_eq!(widest_info(&v), 46, "the body wraps at 44 columns");

        // AC2: w paints three fifths of the terminal and back.
        keys(&mut v, b"w");
        assert_eq!(x0(&v), 40, "wide paints 60 of 100 columns");
        keys(&mut v, b"w");
        assert_eq!(x0(&v), 52, "a second w returns to the dragged width");

        // AC4: a stored 90 clamps transiently on 60 columns and survives.
        v.bell.width = 90;
        v.term = (24, 60);
        assert_eq!(
            60 - panel_width(&v, 60),
            1,
            "a stored 90 paints 59 on 60 columns"
        );
        v.term = (24, 100);
        assert_eq!(v.bell.width, 90, "the stored width survives the clamp");

        // AC6: grab the border at column 52 and drag to 30: the panel paints
        // 70 columns, the body rewraps to 66, and release leaves 70 stored.
        v.bell.width = 48;
        v.bell.size = Size::Normal;
        assert!(begin_drag(&mut v, 52), "the border column starts a drag");
        assert!(
            drag_mouse(&mut v, 30, MouseKind::Drag(MouseButton::Left)),
            "the drag consumes its own report"
        );
        assert!(
            v.bell.open,
            "a drag report left of the old border keeps the panel open"
        );
        assert_eq!(x0(&v), 30, "the border rides the pointer to column 30");
        assert_eq!(widest_info(&v), 68, "the body rewraps to 66 columns");
        end_drag(&mut v);
        assert_eq!(v.bell.width, 70, "release leaves the dragged width");
        assert!(v.bell.drag_from.is_none(), "the drag is over");

        // AC7: a grab-release with no movement changes nothing.
        assert!(begin_drag(&mut v, 30));
        assert!(drag_mouse(
            &mut v,
            31,
            MouseKind::Release(MouseButton::Left)
        ));
        assert_eq!(v.bell.width, 70, "no movement, no change");
    }

    #[test]
    fn questions_marks_clear_through_archive() {
        let q = |id: &str| crate::needs_overlay::QuestionItem {
            id: id.into(),
            title: id.into(),
            state: "open".into(),
            ready: true,
            ..Default::default()
        };
        let marks = |ids: &[&str]| {
            ids.iter()
                .map(|id| id.to_string())
                .collect::<std::collections::BTreeSet<_>>()
        };

        // AC8: space marks and advances, so three spaces mark the three open
        // rows and the footer names the count.
        let mut v = view_with_agents(vec![]);
        v.term = (24, 100);
        v.bell.open = true;
        v.bell.tab = Tab::Questions;
        v.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![
                q("q-a1"),
                q("q-b2"),
                q("q-c3"),
                crate::needs_overlay::QuestionItem {
                    settled: true,
                    ..q("q-d4")
                },
            ],
            ..Default::default()
        });
        keys(&mut v, b"   ");
        assert_eq!(
            v.bell.marks,
            marks(&["q-a1", "q-b2", "q-c3"]),
            "space toggles each open row in turn"
        );
        assert_eq!(v.bell.selected, 3, "each mark advanced the cursor");
        let text = crate::vt::frame_text(&v.compose());
        assert!(
            text.contains("3 marked"),
            "the footer names the count: {text:?}"
        );
        assert!(
            text.contains("● q-a1"),
            "marked rows paint the dot: {text:?}"
        );
        assert!(
            !text.contains("? q-a1"),
            "the unmarked glyph is gone: {text:?}"
        );

        // AC9: c archives the marked ids in row order and clears the marks.
        keys(&mut v, b"c");
        assert_eq!(
            v.question_archive,
            Some(vec![
                "q-a1".to_string(),
                "q-b2".to_string(),
                "q-c3".to_string()
            ]),
            "c archives the marked ids in row order"
        );
        assert!(v.question_acting, "the archive rides the acting latch");
        assert!(v.bell.marks.is_empty(), "the marks clear");
        assert!(
            !v.question_clear_settled,
            "the clear-settled fallback did not fire beside it"
        );

        // AC10: no marks with an Answered section falls back to today's
        // clear-settled; a mark on a question that left the list is not sent.
        let mut v2 = view_with_agents(vec![]);
        v2.term = (24, 100);
        v2.bell.open = true;
        v2.bell.tab = Tab::Questions;
        v2.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![
                q("q-e5"),
                crate::needs_overlay::QuestionItem {
                    settled: true,
                    ..q("q-f6")
                },
            ],
            ..Default::default()
        });
        // A settled row never marks: the archive door would refuse it, and
        // space on it neither marks nor advances.
        v2.bell.selected = 2;
        keys(&mut v2, b" ");
        assert!(v2.bell.marks.is_empty(), "a settled row does not mark");
        assert_eq!(v2.bell.selected, 2, "no mark, no advance");
        keys(&mut v2, b"c");
        assert!(v2.question_clear_settled, "no marks: clear settled fires");
        assert_eq!(v2.question_archive, None);

        let mut v3 = view_with_agents(vec![]);
        v3.term = (24, 100);
        v3.bell.open = true;
        v3.bell.tab = Tab::Questions;
        v3.questions_fold = Some(crate::needs_overlay::QuestionsFold {
            items: vec![q("q-a1")],
            ..Default::default()
        });
        v3.bell.marks = marks(&["q-a1", "q-gone"]);
        keys(&mut v3, b"c");
        assert_eq!(
            v3.question_archive,
            Some(vec!["q-a1".to_string()]),
            "the vanished id is pruned, not sent"
        );
    }
}
