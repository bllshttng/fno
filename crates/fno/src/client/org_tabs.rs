//! The org view's explorer and its five tabs: TABLE, RECENT, OVERVIEW,
//! GRAPH and HEALTH. Each painter returns lines with their tap targets, so
//! a tap routes to what the frame drew.

use super::backlog_style::{BLine, BRole, BSeg};
use super::org_board::{host_key, lead_key, seat_key, Item};
use super::org_seats::{Fleet, Lead, Machine, Seat, STATUS_ORDER};
use std::ops::Range;

/// One painted line plus the column ranges a tap on it selects.
pub(crate) struct Line {
    pub(crate) line: BLine,
    pub(crate) taps: Vec<(Range<usize>, String)>,
}

impl Line {
    pub(crate) fn tap_at(&self, col: usize) -> Option<&str> {
        self.taps
            .iter()
            .find(|(r, _)| r.contains(&col))
            .map(|(_, k)| k.as_str())
    }
}

/// A line under construction: segments with their roles, and tap ranges
/// counted in display columns.
#[derive(Default)]
pub(crate) struct Lb {
    segs: Vec<BSeg>,
    taps: Vec<(Range<usize>, String)>,
    col: usize,
    band: bool,
}

impl Lb {
    pub(crate) fn s(mut self, text: impl Into<String>, role: BRole) -> Self {
        let text = text.into();
        self.col += text
            .chars()
            .map(super::backlog_style::char_w)
            .sum::<usize>();
        self.segs.push(BSeg { text, role });
        self
    }
    pub(crate) fn k(mut self, text: impl Into<String>, role: BRole, key: &str) -> Self {
        let start = self.col;
        self = self.s(text, role);
        self.taps.push((start..self.col, key.to_string()));
        self
    }
    pub(crate) fn band(mut self, on: bool) -> Self {
        self.band = on;
        self
    }
    /// A tap anywhere on the line selects `key`.
    pub(crate) fn whole(mut self, key: &str) -> Self {
        self.taps.push((0..usize::MAX, key.to_string()));
        self
    }
    pub(crate) fn done(self) -> Line {
        let mut line = BLine::of(&self.segs);
        line.band = self.band;
        Line {
            line,
            taps: self.taps,
        }
    }
}

pub(crate) fn blank() -> Line {
    Lb::default().done()
}
pub(crate) fn meta(text: impl Into<String>) -> Line {
    Lb::default().s(text, BRole::Meta).done()
}

/// Pad or cut to `n` columns; a cut ends in an ellipsis.
pub(crate) fn fit(s: &str, n: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let len = flat.chars().count();
    if len > n {
        let mut out: String = flat.chars().take(n.saturating_sub(1)).collect();
        if n > 0 {
            out.push('\u{2026}');
        }
        out
    } else {
        format!("{flat}{}", " ".repeat(n - len))
    }
}

pub(crate) fn dur(secs: i64) -> String {
    let s = secs.max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=172_799 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

pub(crate) fn epoch(stamp: Option<&str>) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(stamp?)
        .ok()
        .map(|t| t.timestamp())
}

/// `12m ago` from an RFC 3339 stamp; None when the stamp is absent.
pub(crate) fn ago(stamp: Option<&str>, now: u64) -> Option<String> {
    epoch(stamp).map(|t| format!("{} ago", dur(now as i64 - t)))
}

pub(crate) fn glyph(status: &str) -> &'static str {
    match status {
        "blocked" => "\u{2298}",
        "writing" => "\u{25cf}",
        "parked" => "\u{25d0}",
        "quiet" => "\u{25cb}",
        _ => "\u{25cc}",
    }
}
pub(crate) fn status_role(status: &str) -> BRole {
    match status {
        "blocked" => BRole::Head,
        "writing" => BRole::Head,
        "parked" => BRole::Body,
        _ => BRole::Meta,
    }
}
pub(crate) fn name_role(s: &Seat) -> BRole {
    if s.role.is_some() {
        BRole::Head
    } else {
        BRole::Body
    }
}
/// The seat's model, else the basis of the gap.
pub(crate) fn model(s: &Seat) -> String {
    s.model
        .as_deref()
        .map(|m| m.trim_start_matches("claude-").to_string())
        .unwrap_or_else(|| "not published".into())
}
/// `1 session`, `2 sessions`.
pub(crate) fn count(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

pub(crate) fn rule(title: &str, width: usize) -> Line {
    let head = format!(" \u{2500}\u{2500} {title} ");
    let rest = width.saturating_sub(head.chars().count());
    Lb::default()
        .s(head, BRole::Head)
        .s("\u{2500}".repeat(rest), BRole::Meta)
        .done()
}
pub(crate) fn kv(key: &str, value: impl Into<String>, role: BRole) -> Line {
    Lb::default()
        .s(format!("   {}", fit(&format!("{key}:"), 12)), BRole::Meta)
        .s(value, role)
        .done()
}
/// One seat in scope, with the machine and lead it sits under.
#[derive(Clone, Copy)]
pub(crate) struct Row<'a> {
    pub(crate) m: &'a Machine,
    pub(crate) p: &'a Lead,
    pub(crate) s: &'a Seat,
}

impl Row<'_> {
    pub(crate) fn key(&self) -> String {
        seat_key(self.m, self.p, self.s)
    }
}

/// The seats an explorer selection covers: the fleet, one machine, one lead
/// or the lead a seat sits under.
pub(crate) fn scope<'a>(fleet: &'a Fleet, item: &Item) -> Vec<Row<'a>> {
    let mut rows = Vec::new();
    for (mi, m) in fleet.machines.iter().enumerate() {
        for (pi, p) in m.leads.iter().enumerate() {
            let keep = match item {
                Item::Root | Item::Feed | Item::System => true,
                Item::Host(h) => *h == mi,
                Item::Lead(h, q) | Item::Seat(h, q, _) => *h == mi && *q == pi,
            };
            if keep {
                rows.extend(p.seats.iter().map(|s| Row { m, p, s }));
            }
        }
    }
    rows
}

fn summary(rows: &[Row]) -> String {
    let n = |w: &str| rows.iter().filter(|r| r.s.status == w).count();
    format!(
        "{} \u{b7} {} writing \u{b7} {} parked \u{b7} {} no status",
        count(rows.len(), "session"),
        n("writing"),
        n("parked"),
        n("unknown")
    )
}

/// Leads in scope, in fleet order, each with its rows.
fn leads<'a>(rows: &[Row<'a>]) -> Vec<Vec<Row<'a>>> {
    let mut out: Vec<Vec<Row>> = Vec::new();
    for r in rows {
        match out
            .iter_mut()
            .find(|g| g[0].m.id == r.m.id && g[0].p.name == r.p.name)
        {
            Some(g) => g.push(*r),
            None => out.push(vec![*r]),
        }
    }
    out
}

/// One explorer entry, as the board lists them.
pub(crate) struct Entry {
    pub(crate) key: String,
    pub(crate) item: Item,
    pub(crate) depth: usize,
}

/// The explorer: the fleet tree (fleet, machines, leads, the expanded
/// lead's seats), then feed and system.
pub(crate) fn explorer(
    fleet: Option<&Fleet>,
    entries: &[Entry],
    sel: &str,
    expanded: &str,
    feed_count: Option<usize>,
    width: usize,
) -> Vec<Line> {
    let mut out = vec![Lb::default().s(" Explorer", BRole::Head).done()];
    let tree: Vec<&Entry> = entries
        .iter()
        .filter(|e| !matches!(e.item, Item::Feed | Item::System))
        .collect();
    let last: Vec<bool> = (0..tree.len())
        .map(|i| {
            tree[i + 1..]
                .iter()
                .find(|e| e.depth <= tree[i].depth)
                .is_none_or(|e| e.depth < tree[i].depth)
        })
        .collect();
    let mut open = [false; 4];
    for (i, e) in tree.iter().enumerate() {
        open[e.depth.min(3)] = last[i];
        let mut pre = String::new();
        for d in 1..e.depth {
            pre.push_str(if open[d] { "  " } else { "\u{2502} " });
        }
        if e.depth > 0 {
            pre.push_str(if last[i] { "\u{2514} " } else { "\u{251c} " });
        }
        let on = e.key == sel;
        let (label, count, role, seat) = match (&e.item, fleet) {
            (Item::Root, f) => (
                "Fleet".to_string(),
                f.map(|f| f.seats().count()),
                BRole::Head,
                None,
            ),
            (Item::Host(m), Some(f)) => {
                let m = &f.machines[*m];
                (
                    format!("\u{25a0} {}", m.host),
                    Some(m.leads.iter().map(|p| p.seats.len()).sum::<usize>()),
                    BRole::Body,
                    None,
                )
            }
            (Item::Lead(m, p), Some(f)) => {
                let lead = &f.machines[*m].leads[*p];
                let ex = e.key == expanded;
                (
                    format!("{} {}", if ex { "\u{25be}" } else { "\u{203a}" }, lead.name),
                    Some(lead.seats.len()),
                    if ex { BRole::Body } else { BRole::Meta },
                    None,
                )
            }
            (Item::Seat(m, p, s), Some(f)) => {
                let seat = &f.machines[*m].leads[*p].seats[*s];
                (seat.name.clone(), None, name_role(seat), Some(seat))
            }
            _ => continue,
        };
        let used = 1 + pre.chars().count() + if seat.is_some() { 2 } else { 0 };
        let mut lb = Lb::default()
            .s(if on { "\u{25b6}" } else { " " }, BRole::Head)
            .s(pre, BRole::Meta);
        if let Some(seat) = seat {
            lb = lb.s(format!("{} ", glyph(seat.status)), status_role(seat.status));
        }
        lb = lb.s(fit(&label, width.saturating_sub(used + 4)), role).s(
            count.map(|n| format!("{n:>3} ")).unwrap_or_default(),
            BRole::Meta,
        );
        out.push(lb.whole(&e.key).band(on).done());
    }
    out.push(blank());
    for (key, label, count) in [
        ("feed", "Feed", feed_count),
        ("system", "System", fleet.map(|f| f.machines.len())),
    ] {
        let on = sel == key;
        out.push(
            Lb::default()
                .s(if on { "\u{25b6}" } else { " " }, BRole::Head)
                .s(fit(label, width.saturating_sub(5)), BRole::Head)
                .s(
                    count.map_or_else(|| "  ? ".to_string(), |n| format!("{n:>3} ")),
                    BRole::Meta,
                )
                .whole(key)
                .band(on)
                .done(),
        );
    }
    out
}

/// The tab strip plus the scope it reads. Each tab is a tap target
/// `tab/<n>`.
pub(crate) fn header(fleet: &Fleet, item: &Item, tab: crate::view_store::OrgTab) -> Line {
    let mut lb = Lb::default().s(" ", BRole::Meta);
    for (i, t) in crate::view_store::OrgTab::ALL.into_iter().enumerate() {
        let label = format!(" {} ", t.word());
        let role = if t == tab { BRole::Tab } else { BRole::Meta };
        lb = lb.k(label, role, &format!("tab/{i}")).s(" ", BRole::Meta);
    }
    let scope = match item {
        Item::Host(m) => format!("machine {}", fleet.machines[*m].host),
        Item::Lead(m, p) | Item::Seat(m, p, _) => format!(
            "lead {} on {}",
            fleet.machines[*m].leads[*p].name, fleet.machines[*m].host
        ),
        _ => "fleet \u{b7} every machine".into(),
    };
    lb.s(format!("  {scope}"), BRole::Body).done()
}

pub(crate) fn table(rows: &[Row], sel: &str, width: usize, now: u64, in_lead: bool) -> Vec<Line> {
    const W: [usize; 6] = [18, 14, 10, 14, 9, 7];
    let fixed: usize = W.iter().sum::<usize>() + 1;
    let now_w = width.saturating_sub(fixed).max(8);
    let mut out = vec![
        Lb::default()
            .s(
                format!(
                    " {}{}{}{}{}{}Now",
                    fit("Lead", W[0]),
                    fit("Seat", W[1]),
                    fit("Runtime", W[2]),
                    fit("Model", W[3]),
                    fit("State", W[4]),
                    fit("Age", W[5])
                ),
                BRole::Head,
            )
            .done(),
        meta(format!(" {}", "\u{2500}".repeat(width.saturating_sub(2)))),
    ];
    for (gi, group) in leads(rows).into_iter().enumerate() {
        if gi > 0 {
            out.push(meta(format!(
                " {}",
                "\u{2508}".repeat(width.saturating_sub(2))
            )));
        }
        for (i, r) in group.iter().enumerate() {
            let key = r.key();
            let lead = if i > 0 {
                String::new()
            } else if in_lead {
                r.p.name.clone()
            } else {
                format!("{}@{}", r.p.name, r.m.host)
            };
            let age = epoch(r.s.started_at.as_deref())
                .map(|t| dur(now as i64 - t))
                .unwrap_or_else(|| "not published".into());
            let last = match r.s.last_message.as_deref().filter(|t| !t.trim().is_empty()) {
                Some(text) => text.to_string(),
                None => match ago(r.s.last_message_at.as_deref(), now) {
                    Some(a) => format!("last turn {a}"),
                    None => "not published".into(),
                },
            };
            let runtime = r.s.harness.as_deref().unwrap_or("not published");
            out.push(
                Lb::default()
                    .s(" ", BRole::Meta)
                    .s(format!("{} ", fit(&lead, W[0] - 1)), BRole::Meta)
                    .s(format!("{} ", glyph(r.s.status)), status_role(r.s.status))
                    .s(fit(&r.s.name, W[1] - 2), name_role(r.s))
                    .s(format!("{} ", fit(runtime, W[2] - 1)), BRole::Meta)
                    .s(format!("{} ", fit(&model(r.s), W[3] - 1)), BRole::Meta)
                    .s(fit(r.s.status, W[4]), status_role(r.s.status))
                    .s(format!("{} ", fit(&age, W[5] - 1)), BRole::Body)
                    .s(fit(&last, now_w), BRole::Meta)
                    .whole(&key)
                    .band(key == sel)
                    .done(),
            );
        }
    }
    out.push(blank());
    out.push(meta(format!(
        " {} \u{b7} enter on a seat opens it",
        summary(rows)
    )));
    out
}

pub(crate) fn recent(rows: &[Row], width: usize, now: u64) -> Vec<Line> {
    let mut seen: Vec<(i64, Row)> = rows
        .iter()
        .filter_map(|r| epoch(r.s.last_message_at.as_deref()).map(|t| (t, *r)))
        .collect();
    seen.sort_by(|a, b| b.0.cmp(&a.0));
    let mut out = vec![
        meta(" last turn per session \u{b7} newest first \u{b7} tap to open"),
        blank(),
    ];
    for (i, (_, r)) in seen.iter().enumerate() {
        let key = r.key();
        let stamp: String =
            r.s.last_message_at
                .as_deref()
                .unwrap_or("")
                .chars()
                .take(16)
                .collect::<String>()
                .replace('T', " ");
        out.push(
            Lb::default()
                .s(" \u{25a0} ", BRole::Meta)
                .s(format!("{stamp}Z"), BRole::Body)
                .s(format!(" \u{b7} #{}", i + 1), BRole::Meta)
                .whole(&key)
                .done(),
        );
        out.push(
            Lb::default()
                .s("   ", BRole::Meta)
                .s(r.s.name.clone(), name_role(r.s))
                .s(" \u{b7} ", BRole::Meta)
                .s(r.s.status, status_role(r.s.status))
                .s(
                    format!(
                        " \u{b7} {} \u{b7} lead {} \u{b7} {}",
                        r.m.host,
                        r.p.name,
                        ago(r.s.last_message_at.as_deref(), now).unwrap_or_default()
                    ),
                    BRole::Meta,
                )
                .whole(&key)
                .done(),
        );
        out.push(meta(format!(
            "   {}",
            fit(
                "text not published: the fleet map carries no message text",
                width.saturating_sub(4)
            )
            .trim_end()
        )));
        out.push(meta(format!(
            "   session {} \u{b7} {}{}",
            r.s.short,
            model(r.s),
            r.s.work
                .as_ref()
                .map(|w| format!(" \u{b7} {}", w.id))
                .unwrap_or_default()
        )));
        out.push(blank());
    }
    if seen.is_empty() {
        out.push(meta("   no published last turn in this scope"));
    }
    out
}

fn state_counts(rows: &[Row]) -> Line {
    let mut lb = Lb::default().s(format!("   {}", fit("state:", 12)), BRole::Meta);
    let mut first = true;
    for w in STATUS_ORDER {
        let n = rows.iter().filter(|r| r.s.status == w).count();
        if n == 0 {
            continue;
        }
        lb = lb.s(
            format!("{}{n} {w}", if first { "" } else { " \u{b7} " }),
            status_role(w),
        );
        first = false;
    }
    lb.done()
}

pub(crate) fn overview(
    fleet: &Fleet,
    item: &Item,
    rows: &[Row],
    width: usize,
    now: u64,
) -> Vec<Line> {
    let title = match item {
        Item::Lead(..) | Item::Seat(..) => "Lead",
        Item::Host(_) => "Machine",
        _ => "Fleet",
    };
    let hosts: Vec<String> = fleet
        .machines
        .iter()
        .filter(|m| rows.iter().any(|r| r.m.id == m.id) || matches!(item, Item::Host(_)))
        .filter(|m| !matches!(item, Item::Host(h) if fleet.machines[*h].id != m.id))
        .map(|m| {
            format!(
                "{} ({}, beat {} ago)",
                m.host,
                if m.fresh { "online" } else { "stale" },
                dur(m.age_s)
            )
        })
        .collect();
    let groups = leads(rows);
    let mut out = vec![
        rule(title, width),
        kv(
            "hosts",
            if hosts.is_empty() {
                "none".into()
            } else {
                hosts.join(" \u{b7} ")
            },
            BRole::Body,
        ),
        kv(
            "shape",
            format!(
                "{} \u{b7} {} \u{b7} {} with a role",
                count(groups.len(), "lead"),
                count(rows.len(), "session"),
                rows.iter().filter(|r| r.s.role.is_some()).count()
            ),
            BRole::Body,
        ),
        state_counts(rows),
        blank(),
    ];
    if matches!(item, Item::Lead(..) | Item::Seat(..)) {
        out.push(rule("Seats", width));
        for r in rows {
            let title =
                r.s.work
                    .as_ref()
                    .and_then(|w| w.title.clone())
                    .unwrap_or_else(|| r.s.short.clone());
            out.push(
                Lb::default()
                    .s("   ", BRole::Meta)
                    .s(format!("{} ", glyph(r.s.status)), status_role(r.s.status))
                    .s(fit(&r.s.name, 14), name_role(r.s))
                    .s(fit(&title, width.saturating_sub(30)), BRole::Meta)
                    .s(
                        fit(
                            &ago(r.s.last_message_at.as_deref(), now)
                                .unwrap_or_else(|| "not published".into()),
                            12,
                        ),
                        BRole::Meta,
                    )
                    .whole(&r.key())
                    .done(),
            );
        }
    } else {
        out.push(rule("Leads", width));
        for g in groups {
            let w = g.iter().filter(|r| r.s.status == "writing").count();
            out.push(
                Lb::default()
                    .s("   \u{25a0} ", BRole::Meta)
                    .s(fit(&g[0].p.name, 16), BRole::Body)
                    .s(fit(&g[0].m.host, 12), BRole::Meta)
                    .s(fit(&count(g.len(), "session"), 14), BRole::Body)
                    .s(
                        if w > 0 {
                            format!("{w} writing")
                        } else {
                            "idle".into()
                        },
                        if w > 0 { BRole::Head } else { BRole::Meta },
                    )
                    .whole(&lead_key(g[0].m, g[0].p))
                    .done(),
            );
        }
    }
    out
}

/// One column per lead (a lead over 6 seats wraps into a `↳` column), boxed
/// seats with the seat's name and model, the columns banded to the width.
pub(crate) fn graph(
    fleet: &Fleet,
    item: &Item,
    rows: &[Row],
    sel: &str,
    width: usize,
) -> Vec<Line> {
    const BW: usize = 18;
    const COLW: usize = BW + 2;
    const CAP: usize = 6;
    let ncol = (width.saturating_sub(5) / COLW).max(1);
    let title = match item {
        Item::Host(m) => format!("Machine {}", fleet.machines[*m].host),
        Item::Lead(m, p) | Item::Seat(m, p, _) => {
            format!("Lead {}", fleet.machines[*m].leads[*p].name)
        }
        _ => "Fleet".into(),
    };
    let mut out = vec![Lb::default()
        .s(" ", BRole::Meta)
        .s(title, BRole::Head)
        .s(
            "  one column per lead \u{b7} leads on top \u{b7} big groups wrap",
            BRole::Meta,
        )
        .done()];
    // A column is a list of cells; a cell is (text, role, tap key).
    type Cell = Vec<(String, BRole, String)>;
    let mut cols: Vec<Vec<Cell>> = Vec::new();
    for g in leads(rows) {
        let pkey = lead_key(g[0].m, g[0].p);
        let label = format!("{}@{}", g[0].p.name, g[0].m.host);
        for (c, chunk) in g.chunks(CAP).enumerate() {
            let head = if c == 0 {
                format!("\u{2261} {label}")
            } else {
                format!("  \u{21b3} {label}")
            };
            let mut col: Vec<Cell> = vec![vec![(
                fit(&head, BW),
                if c == 0 { BRole::Body } else { BRole::Meta },
                pkey.clone(),
            )]];
            for r in chunk {
                let key = r.key();
                let edge = if key == sel { BRole::Head } else { BRole::Meta };
                let bar = "\u{2500}".repeat(BW - 2);
                col.push(vec![(format!("\u{250c}{bar}\u{2510}"), edge, key.clone())]);
                col.push(vec![
                    ("\u{2502}".into(), edge, key.clone()),
                    (
                        format!("{} ", glyph(r.s.status)),
                        status_role(r.s.status),
                        key.clone(),
                    ),
                    (fit(&r.s.name, BW - 4), name_role(r.s), key.clone()),
                    ("\u{2502}".into(), edge, key.clone()),
                ]);
                col.push(vec![
                    ("\u{2502}".into(), edge, key.clone()),
                    (
                        fit(&format!(">_ {}", model(r.s)), BW - 2),
                        BRole::Meta,
                        key.clone(),
                    ),
                    ("\u{2502}".into(), edge, key.clone()),
                ]);
                col.push(vec![(format!("\u{2514}{bar}\u{2518}"), edge, key)]);
            }
            cols.push(col);
        }
    }
    for band in cols.chunks(ncol) {
        out.push(meta(" \u{2502}"));
        let h = band.iter().map(Vec::len).max().unwrap_or(0);
        for y in 0..h {
            let mut lb = Lb::default().s(" \u{2502} ", BRole::Meta);
            for col in band {
                let cell = col.get(y).cloned().unwrap_or_default();
                let w: usize = cell.iter().map(|(t, _, _)| t.chars().count()).sum();
                for (t, role, key) in cell {
                    lb = lb.k(t, role, &key);
                }
                lb = lb.s(" ".repeat(COLW.saturating_sub(w)), BRole::Meta);
            }
            out.push(lb.done());
        }
    }
    if cols.is_empty() {
        out.push(meta("   no seats in this scope"));
    }
    out.push(meta(format!(
        " \u{2514}{}",
        "\u{2500}".repeat(width.saturating_sub(3))
    )));
    out.push(meta(" tap a box to open its seat"));
    out
}

/// A machine's verdict counts only while its beat is fresh.
pub(crate) fn sampled(m: &Machine) -> Option<&str> {
    m.verdict.as_deref().filter(|_| m.fresh)
}

pub(crate) fn not_sampled() -> Line {
    Lb::default()
        .s(" health ", BRole::Body)
        .s("not sampled", BRole::Head)
        .s(
            " \u{b7} no health check runs on this snapshot; not a healthy verdict",
            BRole::Meta,
        )
        .done()
}

pub(crate) fn health(
    fleet: &Fleet,
    item: &Item,
    rows: &[Row],
    width: usize,
    now: u64,
) -> Vec<Line> {
    let machines: Vec<&Machine> = fleet
        .machines
        .iter()
        .enumerate()
        .filter(|(i, m)| match item {
            Item::Host(h) | Item::Lead(h, _) | Item::Seat(h, _, _) => h == i,
            _ => rows.iter().any(|r| r.m.id == m.id) || m.leads.is_empty(),
        })
        .map(|(_, m)| m)
        .collect();
    let mut out = Vec::new();
    if machines.iter().all(|m| sampled(m).is_none()) {
        out.push(not_sampled());
    } else {
        for m in &machines {
            out.push(kv(
                &m.host,
                match sampled(m) {
                    Some(v) => format!("{v} \u{b7} beat {} ago", dur(m.age_s)),
                    None => "not sampled".into(),
                },
                BRole::Body,
            ));
        }
    }
    out.push(blank());
    out.push(rule("Signals in this snapshot \u{b7} not a verdict", width));
    let old =
        |r: &&Row| epoch(r.s.last_message_at.as_deref()).is_some_and(|t| now as i64 - t > 7200);
    let signals: [(&str, Vec<&Row>, &str); 3] = [
        (
            "unknown",
            rows.iter().filter(|r| r.s.status == "unknown").collect(),
            "report no status",
        ),
        (
            "parked",
            rows.iter()
                .filter(|r| r.s.status == "parked" && r.s.status_age_s.unwrap_or(0) > 7200)
                .collect(),
            "parked longer than 2h",
        ),
        (
            "quiet",
            rows.iter().filter(old).collect(),
            "silent longer than 2h",
        ),
    ];
    for (status, hit, text) in signals {
        let names = hit
            .iter()
            .map(|r| r.s.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(
            Lb::default()
                .s(format!("   {} ", glyph(status)), status_role(status))
                .s(
                    fit(&format!("{} {text}", hit.len()), 30),
                    if hit.is_empty() {
                        BRole::Meta
                    } else {
                        BRole::Body
                    },
                )
                .s(
                    fit(
                        if names.is_empty() {
                            "-"
                        } else {
                            names.as_str()
                        },
                        width.saturating_sub(36),
                    ),
                    BRole::Meta,
                )
                .done(),
        );
    }
    out.push(blank());
    out.push(rule("Machines", width));
    for m in machines {
        out.push(
            Lb::default()
                .s("   \u{25a0} ", BRole::Meta)
                .s(
                    fit(
                        &format!(
                            "{} \u{b7} spawn cap {}",
                            m.host,
                            m.cap
                                .map_or_else(|| "not published".into(), |c| c.to_string())
                        ),
                        34,
                    ),
                    BRole::Body,
                )
                .s(
                    format!(
                        "memory {} \u{b7} CPU {}",
                        m.mem_used
                            .map_or_else(|| "not sampled".into(), |u| format!("{:.0}%", u * 100.0)),
                        m.cpu
                            .map_or_else(|| "not sampled".into(), |c| format!("{c:.0}%"))
                    ),
                    BRole::Meta,
                )
                .whole(&host_key(m))
                .done(),
        );
    }
    out
}
