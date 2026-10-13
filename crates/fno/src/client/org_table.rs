//! The org board's Table mode: one row per worker session, with columns
//! that earn a place and a basis named for every missing reading, so a
//! shown cell is never blank.

use super::org_board::{Row, Selected};
use super::*;

/// The Table's columns: name and width. The front of the list is the left
/// edge of the row; as the column narrows, whole columns drop from the
/// right end. MAIL is the one conditional column, so it is not listed
/// here; the row builder appends it when some row has unread mail.
pub(crate) const COLUMNS: [(&str, usize); 9] = [
    ("WORKER", 14),
    ("STATE", 8),
    ("NODE", 8),
    ("LEAD", 10),
    ("ACTIVE", 6),
    ("CTX", 5),
    ("RUNTIME", 14),
    ("PR", 6),
    ("LAST", 36),
];

/// The columns one row shows, left to right: the fixed set plus MAIL in
/// front of LAST when `mail` is set. Values line up with this list.
pub(crate) fn columns(mail: bool) -> Vec<(&'static str, usize)> {
    let mut shown: Vec<(&'static str, usize)> = COLUMNS.iter().take(8).copied().collect();
    if mail {
        shown.push(("MAIL", 4));
    }
    shown.push(("LAST", 36));
    shown
}

/// One rendered row: fixed columns padded whole (a column shows only when
/// the whole cell fits, so a narrow sideline drops columns from the right
/// end and never clips a cell mid-word), then LAST taking the rest.
pub(crate) fn cells(values: &[String], width: usize, mail: bool) -> String {
    let cols = columns(mail);
    let mut text = String::new();
    for (k, (name, w)) in cols.iter().enumerate() {
        let value = &values[k];
        if name == &"LAST" {
            if !value.is_empty() {
                let room = width.saturating_sub(text.chars().count());
                let rest: String = value.chars().take(room).collect();
                text.push_str(&rest);
            }
            break;
        }
        if text.chars().count() + w + 1 > width {
            break;
        }
        let trimmed: String = value.chars().take(*w).collect();
        text.push_str(&trimmed);
        let pad = w.saturating_sub(trimmed.chars().count()) + 1;
        text.push_str(&" ".repeat(pad));
    }
    text
}

/// The header row, from the same column list the rows use.
pub(crate) fn header(width: usize, mail: bool) -> String {
    let names: Vec<String> = columns(mail).iter().map(|(n, _)| n.to_string()).collect();
    cells(&names, width, mail)
}

/// One worker row's cells, in `columns(mail)` order. A missing reading
/// shows its basis, never a blank cell: `ended` on a former session's CTX
/// and ACTIVE, or `?` on a live row with no reading.
pub(crate) fn row(row: &Row, now: u64, width: usize, mail: bool) -> String {
    let cols = columns(mail);
    let mut values = vec![String::new(); cols.len()];
    let Selected::Session(s) = &row.selected else {
        return cells(&values, width, mail);
    };
    let a = s.agent.as_ref();
    values[0] = s
        .agent
        .as_ref()
        .map(|a| a.name.clone())
        .or_else(|| s.view.session_id.clone())
        .unwrap_or_default();
    let last = values.len() - 1;
    values[last] = a
        .and_then(|a| a.tail.clone())
        .or_else(|| s.view.command.clone())
        .unwrap_or_default();
    let Some(agent) = a else {
        return cells(&values, width, mail);
    };
    values[1] = if agent.exited {
        "former".to_string()
    } else {
        match agent.badge {
            Some(AgentBadge::Working) => "working".to_string(),
            Some(AgentBadge::Blocked) => "blocked".to_string(),
            Some(AgentBadge::Done) => "done".to_string(),
            None => "?".to_string(),
        }
    };
    values[2] = agent.node.clone().unwrap_or_default();
    values[3] = row.lead.clone();
    let (ctx, active) = if agent.exited {
        ("ended".to_string(), "ended".to_string())
    } else {
        let ctx = match agent.context_used_pct {
            Some(_) => super::row_meter::ctx_cell(agent.context_used_pct),
            None => "?".to_string(),
        };
        let active = match agent.last_activity_age_s {
            Some(age) => super::row_meter::up_cell(Some(now.saturating_sub(age)), now),
            None => "?".to_string(),
        };
        (ctx, active)
    };
    values[4] = active;
    values[5] = ctx;
    values[6] = super::card_line::harness_model(agent).unwrap_or_else(|| "?".to_string());
    values[7] = agent.pr.map(|n| n.to_string()).unwrap_or_default();
    if mail {
        let mail_idx = cols.iter().position(|(n, _)| n == &"MAIL").unwrap_or(0);
        values[mail_idx] = agent
            .mail_unread
            .filter(|n| *n > 0)
            .map(|n| n.to_string())
            .unwrap_or_default();
    }
    cells(&values, width, mail)
}
