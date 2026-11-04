//! The Decisions tab: the decisions table over the full terminal, a sibling
//! of the Messages tab. The strip row is the sideline's; the rows below are
//! the table's own render. Retract and overrule need the operator's
//! terminal, which this raw-mode client holds, so those keys open the table
//! in a pane where the law door can read a real tty.

use super::*;
use crate::decisions_overlay::{load, load_reviewed, mark_reviewed, Filter};
use crate::decisions_table::{parse_key, render, store_path, Key, Table};
use crate::view_store::SidelineView;

pub(crate) struct DecisionsBoard {
    table: Table,
}

pub(crate) fn open(view: &mut View) {
    match load() {
        Ok(rows) => open_rows(view, rows),
        Err(e) => view.set_notice(format!("decisions: {e}")),
    }
}

/// Open the tab on `rows`; the snapshot shoots it on a fixed set.
pub(crate) fn open_rows(view: &mut View, rows: Vec<crate::decisions_overlay::DecisionRow>) {
    view.panel_on = true;
    view.messages_board = None;
    view.backlog_board = None;
    view.org_board = None;
    view.decisions_board = Some(DecisionsBoard {
        table: Table {
            rows,
            reviewed: load_reviewed(&store_path()),
            filter: Filter::default(),
            pick: 0,
        },
    });
    view.region_owner = super::region_focus::RegionOwner::Board;
    super::backlog_board::set_sideline_view(view, SidelineView::Decisions);
}

/// The alerts row's tap: open the tab on the audit queue.
pub(crate) fn open_audit(view: &mut View) {
    open(view);
    if let Some(b) = view.decisions_board.as_mut() {
        b.table.apply(Key::Audit);
    }
}

/// Paint the style codes `render` emits (reset, bold, dim, reverse) into one
/// row of cells.
fn paint_ansi(row: &mut [Cell], line: &str) {
    let (mut flags, mut col) = (0u8, 0usize);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' && chars.peek() == Some(&'[') {
            chars.next();
            let mut code = String::new();
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
                code.push(d);
            }
            flags = match code.as_str() {
                "1" => flags | cell_flags::BOLD,
                "2" => flags | cell_flags::DIM,
                "7" => flags | cell_flags::INVERSE,
                _ => 0,
            };
        } else if let Some(cell) = row.get_mut(col) {
            *cell = Cell {
                c,
                flags,
                ..Cell::default()
            };
            col += 1;
        }
    }
}

pub(crate) fn paint_full(view: &View, cells: &mut [Cell], rows: usize, cols: usize) {
    cells[..rows * cols].fill(Cell::default());
    view.paint_top_row(cells, cols, cols);
    let Some(b) = &view.decisions_board else {
        return;
    };
    for (i, line) in render(&b.table, cols, rows.saturating_sub(1))
        .iter()
        .enumerate()
    {
        if let Some(row) = cells.get_mut((i + 1) * cols..(i + 2) * cols) {
            paint_ansi(row, line);
        }
    }
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
            Event::Forward(chunk) => keys(view, &chunk),
            event => match dispatch_event(view, event, sock).await? {
                DispatchFlow::Continue => {}
                DispatchFlow::Break => break,
                DispatchFlow::Detach => return Ok(StdinFlow::Detach),
            },
        }
    }
    Ok(StdinFlow::Continue)
}

pub(crate) fn keys(view: &mut View, bytes: &[u8]) {
    let key = if bytes == b"\x1b" {
        Key::Quit
    } else {
        parse_key(bytes)
    };
    let Some(b) = view.decisions_board.as_mut() else {
        return;
    };
    match key {
        Key::Quit => {
            view.decisions_board = None;
            super::backlog_board::set_sideline_view(view, SidelineView::Agents);
        }
        Key::Review => {
            if let Some(id) = b.table.selected().map(|r| r.decision_id.clone()) {
                let _ = mark_reviewed(&store_path(), &id);
                b.table.reviewed.insert(id);
                b.table.apply(Key::Other);
            }
        }
        Key::Goto => {
            if let Some(q) = b.table.selected().and_then(|r| r.question_id.clone()) {
                view.open_detail_on(&q);
            }
        }
        Key::Retract | Key::Overrule => {
            crate::decisions_table::open_pane(&view.session);
            view.set_notice(
                "retract and overrule run in the new pane: they need your terminal".into(),
            );
        }
        k => b.table.apply(k),
    }
}

pub(crate) async fn mouse(
    view: &mut View,
    rep: crate::mouse::MouseReport,
    sock: &mut (impl tokio::io::AsyncWrite + Unpin),
) -> Result<(), String> {
    if rep.row != 0
        || !matches!(
            rep.kind,
            crate::proto::MouseKind::Press(crate::proto::MouseButton::Left)
        )
    {
        return Ok(());
    }
    if bell::button_at(view, rep.row, rep.col) {
        return apply_hit(view, ChromeHit::Bell(bell::Hit::Toggle), sock).await;
    }
    for (start, w, v) in view.top_row_spans() {
        if (rep.col as usize) >= start && (rep.col as usize) < start + w {
            apply_hit(view, ChromeHit::TopRow(v), sock).await?;
        }
    }
    Ok(())
}
