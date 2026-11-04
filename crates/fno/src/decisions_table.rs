//! `fno inbox decisions --table`: the Decisions table, one interactive
//! screen. It runs in a mux pane, so its stdin is a real terminal. A retract
//! or an overrule leaves the screen and runs the law door with that terminal
//! inherited, which is the operator proof the door reads (`law_cli`).

use crate::decisions_overlay::{
    action_argv, load, load_reviewed, mark_reviewed, visible, Action, Author, DecisionRow, Filter,
};
use std::collections::HashSet;
use std::io::{Read, Write};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Lane,
    State,
    Author,
    Audit,
    Retract,
    Overrule,
    Review,
    Quit,
    Other,
}

pub fn parse_key(b: &[u8]) -> Key {
    match b {
        b"\x1b[A" | b"k" => Key::Up,
        b"\x1b[B" | b"j" => Key::Down,
        b"l" => Key::Lane,
        b"s" => Key::State,
        b"a" => Key::Author,
        b"u" => Key::Audit,
        b"r" => Key::Retract,
        b"o" => Key::Overrule,
        b"v" => Key::Review,
        b"q" | b"\x03" | b"\x04" => Key::Quit,
        _ => Key::Other,
    }
}

pub struct Table {
    pub rows: Vec<DecisionRow>,
    pub reviewed: HashSet<String>,
    pub filter: Filter,
    pub sel: usize,
}

impl Table {
    fn shown(&self) -> Vec<&DecisionRow> {
        visible(&self.rows, &self.filter, &self.reviewed)
    }

    fn selected(&self) -> Option<&DecisionRow> {
        self.shown().get(self.sel).copied()
    }

    /// Apply a navigation or filter key; an action key returns to the loop.
    pub fn apply(&mut self, key: Key) {
        match key {
            Key::Up => self.sel = self.sel.saturating_sub(1),
            Key::Down => self.sel = (self.sel + 1).min(self.shown().len().saturating_sub(1)),
            Key::Lane => self.filter.cycle_lane(),
            Key::State => self.filter.cycle_state(),
            Key::Author => self.filter.cycle_author(),
            Key::Audit => self.filter.toggle_audit(),
            _ => {}
        }
        self.sel = self.sel.min(self.shown().len().saturating_sub(1));
    }
}

fn fit(s: &str, n: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = flat.chars().take(n).collect();
    while out.chars().count() < n {
        out.push(' ');
    }
    out
}

fn who(r: &DecisionRow) -> &'static str {
    match r.author() {
        Author::User => "user",
        Author::Agent => "agent",
        Author::Unknown => "?",
    }
}

/// The screen as lines, `h` rows of at most `w` columns. The selected row is
/// reverse video and every other style is bold or dim, so the table reads the
/// same on a light and a dark ground.
pub fn render(t: &Table, w: usize, h: usize) -> Vec<String> {
    let shown = t.shown();
    let f = &t.filter;
    let head = format!(
        "\x1b[1mDecisions\x1b[0m  {} of {}  lane:{} state:{} by:{}{}",
        shown.len(),
        t.rows.len(),
        f.lane.unwrap_or("all"),
        f.state.unwrap_or("all"),
        match f.author {
            None => "all",
            Some(Author::User) => "user",
            Some(Author::Agent) => "agent",
            Some(Author::Unknown) => "?",
        },
        if f.audit_only { "  [audit queue]" } else { "" }
    );
    let mut out = vec![head];
    let body_rows = h.saturating_sub(3);
    let top = t.sel.saturating_sub(body_rows.saturating_sub(1));
    // 59 columns of fixed fields precede the text; one more keeps the last
    // column clear of the wrap edge.
    let text_w = w.saturating_sub(60);
    for (i, r) in shown.iter().enumerate().skip(top).take(body_rows) {
        let line = format!(
            "{} {} {} {} {} {} {}",
            fit(&r.lifecycle, 10),
            fit(&r.lane, 12),
            fit(r.ts.get(..10).unwrap_or(&r.ts), 10),
            fit(who(r), 5),
            if r.has_quote() { "Q" } else { " " },
            fit(&r.subject, 14),
            fit(&r.decision, text_w),
        );
        out.push(if i == t.sel {
            format!("\x1b[7m{line}\x1b[0m")
        } else {
            line
        });
    }
    while out.len() < h.saturating_sub(1) {
        out.push(String::new());
    }
    out.push("\x1b[2mj/k move  l lane  s state  a author  u audit queue  r retract  o overrule  v reviewed  q quit\x1b[0m".to_string());
    out
}

fn prompt(label: &str) -> Option<String> {
    print!("{label}: ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    let line = line.trim().to_string();
    // A leading dash would read as a door flag.
    (!line.is_empty() && !line.starts_with('-')).then_some(line)
}

/// Run the law door on `row` with this terminal inherited.
fn run_door(row: &DecisionRow, action: &Action) {
    let status = std::process::Command::new(crate::server::fno_bin())
        .args(action_argv(row, action))
        .status();
    match status {
        Ok(s) if s.success() => println!("done"),
        Ok(s) => println!("the door refused (exit {})", s.code().unwrap_or(-1)),
        Err(e) => println!("cannot run fno: {e}"),
    }
    let _ = prompt("press enter");
}

/// The reviewed-marks sidecar, beside the mux's other sidecars.
fn store_path() -> std::path::PathBuf {
    #[cfg(not(test))]
    return crate::proto::mux_sidecar_path("decisions-reviewed.json");
    #[cfg(test)]
    std::env::temp_dir().join("decisions-reviewed.json")
}

pub fn run() -> i32 {
    let rows = match load() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno inbox decisions --table: {e}");
            return 1;
        }
    };
    let store = store_path();
    let mut t = Table {
        rows,
        reviewed: load_reviewed(&store),
        filter: Filter::default(),
        sel: 0,
    };
    let mut _screen = match crate::observe::Screen::enter() {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("fno inbox decisions --table: {e}");
            return 1;
        }
    };
    let mut buf = [0u8; 16];
    loop {
        let (w, h) = crossterm::terminal::size().unwrap_or((100, 30));
        let mut out = std::io::stdout();
        let _ = out.write_all(b"\x1b[H\x1b[2J");
        let _ = out.write_all(render(&t, w as usize, h as usize).join("\r\n").as_bytes());
        let _ = out.flush();
        let n = std::io::stdin().read(&mut buf).unwrap_or(0);
        if n == 0 {
            return 0;
        }
        let key = parse_key(&buf[..n]);
        match key {
            Key::Quit => return 0,
            Key::Review => {
                if let Some(id) = t.selected().map(|r| r.decision_id.clone()) {
                    let _ = mark_reviewed(&store, &id);
                    t.reviewed.insert(id);
                    t.apply(Key::Other);
                }
            }
            Key::Retract | Key::Overrule => {
                let Some(row) = t.selected().cloned() else {
                    continue;
                };
                _screen = None;
                println!("{} {}: {}", row.decision_id, row.subject, row.decision);
                let action = if key == Key::Retract {
                    prompt("reason").map(|reason| Action::Retract { reason })
                } else {
                    prompt("new text").map(|new_text| Action::Overrule { new_text })
                };
                if let Some(a) = action {
                    run_door(&row, &a);
                    if let Ok(rows) = load() {
                        t.rows = rows;
                    }
                }
                _screen = crate::observe::Screen::enter().ok();
            }
            k => t.apply(k),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(id: &str, life: &str, ts: &str) -> DecisionRow {
        DecisionRow {
            decision_id: id.into(),
            lifecycle: life.into(),
            lane: "law".into(),
            ts: ts.into(),
            decision: "ship it".into(),
            authority_source: Some("operator".into()),
            ..Default::default()
        }
    }

    fn table() -> Table {
        Table {
            rows: vec![
                r("a", "live", "2026-01-01"),
                r("b", "retracted", "2026-02-01"),
            ],
            reviewed: HashSet::new(),
            filter: Filter::default(),
            sel: 0,
        }
    }

    #[test]
    fn selection_clamps_when_a_filter_narrows_the_list() {
        assert_eq!(parse_key(b"r"), Key::Retract);
        assert_eq!(parse_key(b"z"), Key::Other);
        let mut t = table();
        t.apply(parse_key(b"\x1b[B"));
        assert_eq!(t.sel, 1);
        t.apply(Key::State);
        assert_eq!(t.sel, 0);
        assert_eq!(t.shown().len(), 1);
    }

    #[test]
    fn render_marks_the_selected_row_and_fills_the_height() {
        let lines = render(&table(), 100, 10);
        assert_eq!(lines.len(), 10);
        assert!(lines[0].contains("2 of 2"));
        assert!(lines[1].starts_with("\x1b[7m"));
        assert!(lines[9].contains("q quit"));
    }
}
