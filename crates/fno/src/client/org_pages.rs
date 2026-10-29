//! The org view's pages: a seat, the FEED of questions to the user, and
//! SYSTEM's machine table. A field with no reading names its basis.

use super::backlog_style::BRole;
use super::org_seats::{Fleet, Lead, Machine, Seat};
use super::org_tabs::{
    ago, blank, dur, epoch, fit, glyph, kv, meta, not_sampled, rule, sampled, status_role, Lb, Line,
};

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
            .s(" seat ", BRole::Meta)
            .s(s.name.clone(), BRole::Body)
            .s(" \u{b7} ", BRole::Meta)
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
            BRole::Body,
        ),
        kv("runtime", or_np(s.harness.as_deref()), BRole::Body),
    ];
    out.push(if s.local.is_some() {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), BRole::Meta)
            .s("enter attaches here \u{b7} ", BRole::Head)
            .s(attach_command(s), BRole::Body)
            .done()
    } else {
        Lb::default()
            .s(format!("   {}", fit("terminal:", 12)), BRole::Meta)
            .s(attach_command(s), BRole::Body)
            .s(format!(" on {}", m.host), BRole::Meta)
            .done()
    });
    out.push(blank());
    out.push(rule("Current activity", width));
    out.push(kv("activity", s.status, status_role(s.status)));
    out.push(kv(
        "for",
        s.status_age_s.map_or_else(|| "unknown".into(), dur),
        BRole::Body,
    ));
    out.push(kv(
        "last turn",
        ago(s.last_message_at.as_deref(), now).unwrap_or_else(|| NOT_PUBLISHED.into()),
        BRole::Body,
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
                BRole::Body,
            ));
            out.push(kv(
                "title",
                fit(w.title.as_deref().unwrap_or("not in this graph"), inner)
                    .trim_end()
                    .to_string(),
                BRole::Body,
            ));
            out.push(match &w.plan {
                Some(plan) => kv("plan", fit(plan, inner).trim_end().to_string(), BRole::Body),
                None => kv("plan", "no plan_path", BRole::Meta),
            });
            if let Some(pr) = w.pr {
                out.push(kv("pr", format!("#{pr}"), BRole::Body));
            }
            if let Some((id, title)) = &w.epic {
                let text = match title {
                    Some(t) => format!("{id} \u{b7} {t}"),
                    None => id.clone(),
                };
                out.push(kv(
                    "epic",
                    fit(&text, inner).trim_end().to_string(),
                    BRole::Body,
                ));
            }
        }
        None => {
            out.push(rule("Current work", width));
            out.push(kv("node", "none", BRole::Meta));
        }
    }
    out.push(blank());
    out.push(rule("Last message", width));
    let stamp = match (ago(s.last_message_at.as_deref(), now), &s.last_message_at) {
        (Some(a), Some(at)) => format!("{NOT_PUBLISHED} \u{b7} last turn {a} ({at})"),
        _ => NOT_PUBLISHED.into(),
    };
    out.push(
        Lb::default()
            .s("   \u{2502} ", BRole::Meta)
            .s(stamp, BRole::Body)
            .done(),
    );
    out.push(blank());
    out.push(rule("Seat", width));
    out.push(kv("machine", format!("{} ({})", m.host, m.id), BRole::Body));
    out.push(kv("lead", p.name.clone(), BRole::Body));
    out.push(kv("project", or_np(s.project.as_deref()), BRole::Body));
    out.push(kv("session", or_np(s.session_id.as_deref()), BRole::Body));
    out.push(kv(
        "model",
        match (&s.model, &s.model_basis) {
            (Some(model), Some(basis)) => format!("{model} ({basis})"),
            (Some(model), None) => model.clone(),
            _ => NOT_PUBLISHED.into(),
        },
        BRole::Body,
    ));
    out.push(kv("started", or_np(s.started_at.as_deref()), BRole::Body));
    out.push(kv(
        "alive",
        epoch(s.started_at.as_deref())
            .map_or_else(|| NOT_PUBLISHED.into(), |t| dur(now as i64 - t)),
        BRole::Body,
    ));
    out.push(kv(
        "cwd",
        fit(&or_np(s.cwd.as_deref()), inner).trim_end().to_string(),
        BRole::Body,
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
            "   this seat runs on another machine: attach there \u{b7} u goes up to the lead"
        }
    }));
    out
}

pub(crate) fn feed(fold: Option<&crate::needs_overlay::QuestionsFold>, width: usize) -> Vec<Line> {
    let mut out = vec![
        Lb::default().s(" fleet / the user", BRole::Meta).done(),
        blank(),
        Lb::default().s(" Human requests", BRole::Head).done(),
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
                        .s("  \u{25a0} ", BRole::Meta)
                        .s(format!("[{kind}] "), BRole::Meta)
                        .s(
                            fit(title, width.saturating_sub(kind.chars().count() + 8))
                                .trim_end()
                                .to_string(),
                            BRole::Body,
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
        Lb::default().s(" machines", BRole::Meta).done(),
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
                BRole::Head,
            )
            .done(),
    ];
    for m in &fleet.machines {
        out.push(
            Lb::default()
                .s(format!(" {}", fit(&m.host, 12)), BRole::Body)
                .s(
                    fit(if m.fresh { "online" } else { "stale" }, 9),
                    if m.fresh { BRole::Body } else { BRole::Head },
                )
                .s(fit(&dur(m.age_s), 8), BRole::Meta)
                .s(fit(if m.takes_work { "yes" } else { "no" }, 6), BRole::Meta)
                .s(
                    fit(
                        &m.cap
                            .map_or_else(|| NOT_PUBLISHED.into(), |c| c.to_string()),
                        15,
                    ),
                    BRole::Meta,
                )
                .s(
                    fit(
                        &m.mem_used
                            .map_or_else(|| "not sampled".into(), |u| format!("{:.0}%", u * 100.0)),
                        13,
                    ),
                    BRole::Meta,
                )
                .s(
                    m.cpu
                        .map_or_else(|| "not sampled".into(), |c| format!("{c:.0}%")),
                    BRole::Meta,
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
                BRole::Body,
            ));
        }
    }
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
        "   W opens the web command page \u{b7} F full screen \u{b7} V cycle views \u{b7} ? keys \u{b7} esc close",
    ]
    .into_iter()
    .map(meta)
    .collect()
}
