//! The org view's pages: a seat, the FEED of questions to the user, and
//! SYSTEM's machine table. A field with no reading names its basis.

use super::org_seats::{Fleet, Lead, Machine, Seat};
use super::org_tabs::{
    ago, blank, dur, epoch, fit, glyph, kv, meta, not_sampled, rule, sampled, status_role, Lb, Line,
};
use crate::text_style::TextStyle;

const NOT_PUBLISHED: &str = "not published";

fn or_np(v: Option<&str>) -> String {
    v.map(str::to_string)
        .unwrap_or_else(|| NOT_PUBLISHED.into())
}

/// The command that attaches a seat from its own machine.
pub(crate) fn attach_command(s: &Seat) -> String {
    format!("fno agents session attach {}", s.short)
}

pub(crate) fn seat(m: &Machine, p: &Lead, s: &Seat, width: usize, now: u64) -> Vec<Line> {
    let inner = width.saturating_sub(17);
    let mut out = vec![
        Lb::default()
            .s(" seat ", TextStyle::Meta)
            .s(s.name.clone(), TextStyle::Item)
            .s(" \u{b7} ", TextStyle::Meta)
            .s(
                format!("{} {}", glyph(s.status), s.status),
                status_role(s.status),
            )
            .done(),
        blank(),
        rule("Context", width),
        kv(
            "context",
            s.context_pct
                .map_or_else(|| NOT_PUBLISHED.into(), |p| format!("{p}% used")),
            TextStyle::Item,
        ),
        kv("runtime", or_np(s.harness.as_deref()), TextStyle::Item),
    ];
    out.push(if s.local.is_some() {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), TextStyle::Meta)
            .s("enter attaches here \u{b7} ", TextStyle::Title)
            .s(attach_command(s), TextStyle::Item)
            .done()
    } else {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), TextStyle::Meta)
            .s(attach_command(s), TextStyle::Item)
            .s(format!(" on {}", m.host), TextStyle::Meta)
            .done()
    });
    out.push(blank());
    out.push(rule("Current activity", width));
    out.push(kv("activity", s.status, status_role(s.status)));
    out.push(kv(
        "for",
        s.status_age_s.map_or_else(|| "unknown".into(), dur),
        TextStyle::Item,
    ));
    out.push(kv(
        "last turn",
        ago(s.last_message_at.as_deref(), now).unwrap_or_else(|| NOT_PUBLISHED.into()),
        TextStyle::Item,
    ));
    out.push(blank());
    match &s.work {
        Some(w) => {
            out.push(rule(&format!("Current work \u{b7} {}", w.id), width));
            let facts = [
                Some(w.id.clone()),
                w.status.clone(),
                w.priority.clone(),
                w.difficulty.clone(),
            ];
            out.push(kv(
                "node",
                facts
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" \u{b7} "),
                TextStyle::Item,
            ));
            out.push(kv(
                "title",
                fit(w.title.as_deref().unwrap_or("not in this graph"), inner)
                    .trim_end()
                    .to_string(),
                TextStyle::Item,
            ));
            out.push(match &w.plan {
                Some(plan) => kv(
                    "plan",
                    fit(plan, inner).trim_end().to_string(),
                    TextStyle::Item,
                ),
                None => kv("plan", "no plan_path", TextStyle::Meta),
            });
            if let Some(pr) = w.pr {
                out.push(kv("pr", format!("#{pr}"), TextStyle::Item));
            }
            if let Some((id, title)) = &w.epic {
                let text = match title {
                    Some(t) => format!("{id} \u{b7} {t}"),
                    None => id.clone(),
                };
                out.push(kv(
                    "epic",
                    fit(&text, inner).trim_end().to_string(),
                    TextStyle::Item,
                ));
            }
        }
        None => {
            out.push(rule("Current work", width));
            out.push(kv("node", "none", TextStyle::Meta));
        }
    }
    out.push(blank());
    out.push(rule("Last message", width));
    match s.last_message.as_deref().filter(|t| !t.trim().is_empty()) {
        Some(text) => {
            for line in text.lines().take(6) {
                out.push(
                    Lb::default()
                        .s("   \u{2502} ", TextStyle::Meta)
                        .s(fit(line, inner).trim_end().to_string(), TextStyle::Item)
                        .done(),
                );
            }
            if let Some(a) = ago(s.last_message_at.as_deref(), now) {
                out.push(meta(format!("   \u{b7} last turn {a}")));
            }
        }
        None => {
            let stamp = match ago(s.last_message_at.as_deref(), now) {
                Some(a) => format!("last turn {a}"),
                None => NOT_PUBLISHED.into(),
            };
            out.push(
                Lb::default()
                    .s("   \u{2502} ", TextStyle::Meta)
                    .s(stamp, TextStyle::Item)
                    .done(),
            );
        }
    }
    out.push(blank());
    out.push(rule("Seat", width));
    out.push(kv(
        "machine",
        format!("{} ({})", m.host, m.id),
        TextStyle::Item,
    ));
    out.push(kv("lead", p.name.clone(), TextStyle::Item));
    out.push(kv("project", or_np(s.project.as_deref()), TextStyle::Item));
    out.push(kv(
        "session",
        or_np(s.session_id.as_deref()),
        TextStyle::Item,
    ));
    out.push(kv(
        "model",
        match (&s.model, &s.model_basis) {
            (Some(model), Some(basis)) => format!("{model} ({basis})"),
            (Some(model), None) => model.clone(),
            _ => NOT_PUBLISHED.into(),
        },
        TextStyle::Item,
    ));
    out.push(kv(
        "started",
        or_np(s.started_at.as_deref()),
        TextStyle::Item,
    ));
    out.push(kv(
        "alive",
        epoch(s.started_at.as_deref())
            .map_or_else(|| NOT_PUBLISHED.into(), |t| dur(now as i64 - t)),
        TextStyle::Item,
    ));
    out.push(kv(
        "cwd",
        fit(&or_np(s.cwd.as_deref()), inner).trim_end().to_string(),
        TextStyle::Item,
    ));
    out.push(blank());
    out.push(meta(match (&s.local, m.local) {
        (Some(_), _) => {
            "   enter attaches \u{b7} space peeks \u{b7} x stops \u{b7} P portals \u{b7} u goes up to the lead"
        }
        (None, true) => {
            "   no pane here carries this session: run the terminal command \u{b7} u goes up to the lead"
        }
        (None, false) => {
            "   space peeks \u{b7} P opens a viewer pane \u{b7} u goes up to the lead"
        }
    }));
    out
}

pub(crate) fn feed(fold: Option<&crate::needs_overlay::QuestionsFold>, width: usize) -> Vec<Line> {
    let mut out = vec![
        Lb::default().s(" fleet / the user", TextStyle::Meta).done(),
        blank(),
        Lb::default().s(" Human requests", TextStyle::Title).done(),
    ];
    match fold {
        None => out.push(meta("   questions not read yet")),
        Some(f) if f.items.is_empty() => out.push(meta("   no open question")),
        Some(f) => {
            for q in &f.items {
                let title = if q.title.is_empty() {
                    "(no title)"
                } else {
                    q.title.as_str()
                };
                let kind = if q.kind.is_empty() {
                    "question"
                } else {
                    q.kind.as_str()
                };
                out.push(
                    Lb::default()
                        .s("  \u{25a0} ", TextStyle::Meta)
                        .s(format!("[{kind}] "), TextStyle::Meta)
                        .s(
                            fit(title, width.saturating_sub(kind.chars().count() + 8))
                                .trim_end()
                                .to_string(),
                            TextStyle::Item,
                        )
                        .done(),
                );
                let mut to = "      To: user".to_string();
                if let Some(a) = &q.asker {
                    to.push_str(&format!(" \u{b7} From: {}", a.handle));
                }
                if let Some(node) = &q.node {
                    to.push_str(&format!(" \u{b7} node {node}"));
                }
                out.push(meta(to));
                out.push(blank());
            }
        }
    }
    out.push(meta(
        " Viewing is not approval. Rule on the question board.",
    ));
    out
}

pub(crate) fn system(fleet: &Fleet, width: usize) -> Vec<Line> {
    let mut out = vec![
        Lb::default().s(" machines", TextStyle::Meta).done(),
        blank(),
        Lb::default()
            .s(
                format!(
                    " {}{}{}{}{}{}CPU",
                    fit("Host", 12),
                    fit("State", 9),
                    fit("Beat", 8),
                    fit("Work", 6),
                    fit("Cap", 15),
                    fit("Memory", 13)
                ),
                TextStyle::Title,
            )
            .done(),
    ];
    for m in &fleet.machines {
        out.push(
            Lb::default()
                .s(format!(" {}", fit(&m.host, 12)), TextStyle::Item)
                .s(
                    fit(if m.fresh { "online" } else { "stale" }, 9),
                    if m.fresh {
                        TextStyle::Item
                    } else {
                        TextStyle::Title
                    },
                )
                .s(fit(&dur(m.age_s), 8), TextStyle::Meta)
                .s(
                    fit(if m.takes_work { "yes" } else { "no" }, 6),
                    TextStyle::Meta,
                )
                .s(
                    fit(
                        &m.cap
                            .map_or_else(|| NOT_PUBLISHED.into(), |c| c.to_string()),
                        15,
                    ),
                    TextStyle::Meta,
                )
                .s(
                    fit(
                        &m.mem_used
                            .map_or_else(|| "not sampled".into(), |u| format!("{:.0}%", u * 100.0)),
                        13,
                    ),
                    TextStyle::Meta,
                )
                .s(
                    m.cpu
                        .map_or_else(|| "not sampled".into(), |c| format!("{c:.0}%")),
                    TextStyle::Meta,
                )
                .whole(&super::org_board::host_key(m))
                .done(),
        );
    }
    out.push(blank());
    out.push(rule("Connections", width));
    out.push(meta(format!(
        "   roster: fleet_machine rows on the shared primary \u{b7} {} machine{}",
        fleet.machines.len(),
        if fleet.machines.len() == 1 { "" } else { "s" }
    )));
    out.push(meta("   read it: fno agents fleet top --machines"));
    if let Some(e) = &fleet.seat_error {
        out.push(meta(format!("   seat facts not read: {e}")));
    }
    out.push(blank());
    out.push(rule("Health", width));
    if fleet.machines.iter().all(|m| sampled(m).is_none()) {
        out.push(not_sampled());
    } else {
        for m in &fleet.machines {
            out.push(kv(
                &m.host,
                sampled(m).unwrap_or("not sampled").to_string(),
                TextStyle::Item,
            ));
        }
    }
    out
}

/// A peek's transcript lines, already stripped of control bytes.
pub(crate) fn peek_page(lines: &[String]) -> Vec<Line> {
    let mut out = vec![meta(" read-only \u{b7} esc closes"), blank()];
    out.extend(
        lines
            .iter()
            .map(|l| Lb::default().s(format!(" {l}"), TextStyle::Item).done()),
    );
    out
}

pub(crate) fn keys_help() -> Vec<Line> {
    [
        " Org keys",
        "",
        "   j/k or arrows    move in the explorer",
        "   h/l or arrows    change tab \u{b7} 1-5 pick a tab",
        "   u or backspace   go up: seat, lead, machine, fleet",
        "   PageUp/PageDown  scroll the right pane",
        "   enter            open a lead \u{b7} attach a seat on this machine",
        "   space x P        peek, stop or portal a seat on this machine",
        "   space P          peek or view a seat on another machine, over ssh",
        "   s                start a seat on the selected machine \u{b7} esc cancels the prompt",
        "   r                review the PR diff of a seat whose node has a PR",
        "   W opens the HQ web page \u{b7} F full screen \u{b7} V cycle views \u{b7} ? keys \u{b7} esc close",
    ]
    .into_iter()
    .map(meta)
    .collect()
}
