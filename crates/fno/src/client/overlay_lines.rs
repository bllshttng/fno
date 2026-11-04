//! The sideline overlay line builders the client shares: the navigator's
//! catalog, the peek transcript overlay, and the age cell. Moved out of
//! client.rs for the file-budget ratchet; re-exported so every call site
//! stays put.

use super::*;
use crate::text_style::{TLine, TextStyle};

const PEEK_OVERLAY_W: usize = 72;

const NAV_OVERLAY_W: usize = 54;

/// Build the navigator overlay lines: a top `find › <query>  [chip]`
/// line, then one line per FILTERED row with a leading state glyph and the
/// cursor row left to the caller's fill. A row that matched an identity token invisible in
/// its label appends that token as a `·<token>` suffix, so a hit
/// always shows WHY it hit - a row that appears arbitrary reads as a bug. An
/// empty result renders a single `no matches` line (the key handler BELs).
/// `rows` is pre-filtered; `cursor` is pre-clamped.
pub(crate) fn nav_overlay_lines(rows: &[NavRow], nav: &NavView) -> Vec<TLine> {
    let chip = match nav.state_filter {
        None => "all",
        Some(PaneState::Blocked) => "blocked",
        Some(PaneState::Working) => "working",
        Some(PaneState::DoneUnseen) => "done",
        Some(PaneState::Unmeasured) => "unmeasured",
        Some(PaneState::Idle) => "idle",
        Some(PaneState::Empty) => "empty",
    };
    let mut lines = vec![TLine::parts(&[
        (" find › ", TextStyle::Section),
        (&nav.query, TextStyle::Value),
        (&format!("   [{chip}]"), TextStyle::Badge),
    ])
    .pad_to(NAV_OVERLAY_W)];
    if rows.is_empty() {
        lines.push(TLine::line(
            TextStyle::Hint,
            pad_to("   no matches", NAV_OVERLAY_W),
        ));
        return lines;
    }
    let q = nav.query.to_lowercase();
    for r in rows.iter() {
        // The reason token: a match-key token that contains the query while
        // the label does not. A query that hits the visible label appends
        // nothing (AC4-UI: the row renders exactly as before).
        let label_lower = r.label.to_lowercase();
        let label = if q.is_empty() || label_lower.contains(&q) {
            r.label.clone()
        } else {
            match r
                .match_key
                .split(' ')
                .find(|t| t.contains(&q) && !label_lower.contains(t))
            {
                Some(token) => format!("{} ·{token}", r.label),
                None => r.label.clone(),
            }
        };
        lines.push(
            TLine::parts(&[
                (&format!("   {} ", nav_glyph(r.state)), TextStyle::Meta),
                (&label, TextStyle::Item),
            ])
            .pad_to(NAV_OVERLAY_W),
        );
    }
    lines
}

/// An agent nav row's identity tokens: the mux pane id, the squad (a
/// squad-hosted row), the portal as `portal:<n>` (kept apart from pane ids
/// and node hex), the bound node, the PR as `#N` (a substring hit for `N`
/// too), the harness, the full model id and the card's short model name.
/// The short name earns its own token because `opus-4.8` is no substring of
/// `claude-opus-4-8`.
pub(crate) fn nav_agent_tokens(a: &AgentRow, squad: Option<&str>) -> Vec<String> {
    let mut t = vec![
        a.pane_id.map(|p| p.to_string()).unwrap_or_default(),
        squad.unwrap_or_default().to_string(),
        a.portal.map(|p| format!("portal:{p}")).unwrap_or_default(),
        a.node.clone().unwrap_or_default(),
        a.pr.map(|n| format!("#{n}")).unwrap_or_default(),
        a.harness.clone().unwrap_or_default(),
        a.model.clone().unwrap_or_default(),
    ];
    t.extend(super::card_line::model_label(a).filter(|m| Some(m) != a.model.as_ref()));
    t
}

/// Humanize an age in seconds to `Ns`/`Nm`/`Nh`/`Nd` for the peek
/// header's `changed Ns ago` line (Discretion 3). A future stamp (clock skew)
/// is clamped by the caller to 0 before this, so `0s` is the floor.
pub(crate) fn humanize_ago(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// The table's last-activity cell: the same buckets as [`humanize_ago`],
/// right-justified to a fixed width of 4 so the column never reflows when a
/// value rolls from `59m` to `1h`. An absent reading renders EMPTY, like the
/// tail cell - the table never fabricates a placeholder value, and `0s` would
/// be a fabricated one. (`fno agents list` prints `?` for the same absence;
/// that lane's rows are one line each, where a blank reads as a bug.)
pub(crate) fn humanize_age(secs: Option<u64>) -> String {
    let body = match secs {
        None => String::new(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3600 => format!("{}m", s / 60),
        Some(s) if s < 86_400 => format!("{}h", s / 3600),
        // Capped at 999d (review finding: an uncapped day count breaks the
        // fixed-width-4 invariant once a row is silent for 1000+ days). A row
        // that old is a display curiosity, not a case worth a 5th column.
        Some(s) => format!("{}d", (s / 86_400).min(999)),
    };
    format!("{body:>4}")
}

/// Sanitize every external-sourced peek line (transcript body, scraped
/// reason) before it becomes cells: `fno agents session peek` reads raw
/// on-disk transcript text that can carry ANSI escapes / C0 controls, and the
/// peek path does NOT VT-parse (unlike pane output), so an unstripped ESC/CR
/// would reach the user's terminal. Tabs become spaces; every other
/// control char is dropped (a residual bracket-code is harmless text).
pub(crate) fn sanitize_peek_line(s: &str) -> String {
    s.chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

/// Build the read-only peek overlay lines: a header (badge glyph + name
/// + full wrapped status sentence), the answerable block when the row is
/// blocked (prompt + numbered options, reused verbatim), a divider, then the
/// transcript body (" loading…" until it arrives, "no activity yet" for an empty
/// one, error/timeout text rendered verbatim as body lines) and a footer hint.
/// `agent` is the LIVE row re-read per frame; `None` means it vanished between
/// key and frame (a transient single frame - the key handler re-anchors/closes).
pub(crate) fn peek_overlay_lines(
    agent: Option<&AgentRow>,
    peek: &PeekView,
    reply: Option<&str>,
    now_secs: u64,
) -> Vec<TLine> {
    let Some(a) = agent else {
        return vec![TLine::line(
            TextStyle::Hint,
            pad_to(" peek · row gone", PEEK_OVERLAY_W),
        )];
    };
    // the peek header reuses the sideline row's lattice state.
    // `agent_lattice_state` is both exit- and seen-aware (it routes the non-exit
    // case through `pane_state`), so the peek, the row, and the rollups agree
    // and no call site re-derives the precedence.
    let glyph = lattice_glyph(agent_lattice_state(a)).0;
    // The account glyph rides the peek header next to the name, same
    // vocabulary as the selector row.
    let mut header = match a.account.as_deref() {
        Some(acct) => format!(" {glyph} {}  @{acct}", a.name),
        None => format!(" {glyph} {}", a.name),
    };
    // Additive header labels, each present only when its data exists (no
    // placeholder dashes): `changed Ns ago` (a future stamp / clock skew clamps
    // to `0s` via saturating_sub) and `PR #N`.
    if let Some(updated) = a.updated_at {
        header.push_str(&format!(
            " · changed {} ago",
            humanize_ago(now_secs.saturating_sub(updated))
        ));
    }
    if let Some(pr) = a.pr {
        header.push_str(&format!(" · PR #{pr}"));
    }
    if a.started_at.is_some() {
        header.push_str(&format!(
            " · up {}",
            row_meter::up_cell(a.started_at, now_secs)
        ));
    }
    let mut lines = vec![TLine::line(
        TextStyle::Title,
        pad_to(&header, PEEK_OVERLAY_W),
    )];
    let styled = |style: TextStyle, text: &str| TLine::line(style, pad_to(text, PEEK_OVERLAY_W));
    if let Some(reason) = a.reason.as_deref().filter(|s| !s.is_empty()) {
        let mut wrapped = Vec::new();
        wrap_line(
            &sanitize_peek_line(reason),
            PEEK_OVERLAY_W - 3,
            &mut wrapped,
        );
        for wl in wrapped {
            lines.push(styled(TextStyle::WARN, &format!("   {wl}")));
        }
    }
    // answerable block: prompt + numbered options, mirroring the needs-me
    // overlay's body so a blocked peek reads identically. Digit answers (US3)
    // act on exactly these options.
    if let Some(ans) = &a.answerable {
        lines.push(styled(TextStyle::Item, ""));
        if !ans.prompt.is_empty() {
            lines.push(styled(
                TextStyle::Value,
                &format!("   {}", ans.prompt.replace('\n', " ")),
            ));
        }
        for o in &ans.options {
            lines.push(styled(
                TextStyle::Item,
                &format!("     {}. {}", o.idx, o.label),
            ));
        }
    }
    lines.push(styled(TextStyle::Item, "")); // divider before the transcript
    match &peek.body {
        None => lines.push(styled(TextStyle::Hint, "   loading…")),
        Some(body) if body.is_empty() => lines.push(styled(TextStyle::Hint, "   no activity yet")),
        Some(body) => {
            for l in body {
                lines.push(styled(
                    TextStyle::Item,
                    &format!(" {}", sanitize_peek_line(l)),
                ));
            }
        }
    }
    // The reply input (`m`) replaces the footer while open; else the
    // footer swaps by row state (attach is a dead end on an exited row - the bug
    // US6 closes - so it becomes `r respawn`; `m reply` shows in both).
    match reply {
        Some(buf) => lines.push(
            TLine::parts(&[
                (" reply: ", TextStyle::Meta),
                (buf, TextStyle::Value),
                ("_ (⏎ send · esc cancel)", TextStyle::Hint),
            ])
            .pad_to(PEEK_OVERLAY_W),
        ),
        None => lines.push(styled(
            TextStyle::Hint,
            if a.exited {
                " j/k peek · m reply · r respawn · esc back"
            } else {
                " j/k peek · digit answers · m reply · ⏎ attach · esc back"
            },
        )),
    }
    lines
}

impl View {
    /// Paint the connections modal. The modal reports the line its fill covers.
    pub(super) fn draw_connections(
        &self,
        conn: &crate::connections_view::ConnectionsView,
        cells: &mut [Cell],
        rows: usize,
        cols: usize,
    ) {
        let (origin, dims) = self.overlay_viewport();
        let chrome = chrome::Chrome::new("connections", Anchor::Center).footer("esc close");
        // Body lines, not strings: the view paints its type levels as spans.
        let (body, cursor) = conn.render_body(&self.layout.agents, overlay_text_cols(dims.1));
        let layout = super::overlay_paint::layout_body_overlay(
            origin,
            dims,
            &chrome,
            &body,
            cursor,
            OverlayAnchor::Center,
        );
        draw_overlay_layout(cells, rows, cols, &layout, &self.theme);
    }

    /// The navigator's framed block, from the live layout. One layout feeds
    /// the painter and the row menu's anchor, so the two cannot disagree.
    pub(super) fn nav_layout(
        &self,
        overlay_origin: (usize, usize),
        overlay_dims: (usize, usize),
    ) -> Option<OverlayLayout> {
        let nav = self.nav.as_ref()?;
        let lines = nav_overlay_lines(&self.nav_filtered(nav), nav);
        let chrome = chrome::Chrome::new("navigator", Anchor::Center)
            .footer("type to filter · ctrl-o actions · esc close");
        Some(layout_lines_overlay(
            overlay_origin,
            overlay_dims,
            &chrome,
            &lines,
            // +1 for the query line.
            Some(nav.cursor + 1),
            OverlayAnchor::Center,
        ))
    }

    /// Paint the navigator, if open.
    pub(super) fn draw_nav(
        &self,
        cells: &mut [Cell],
        rows: usize,
        cols: usize,
        overlay_origin: (usize, usize),
        overlay_dims: (usize, usize),
    ) {
        if let Some(layout) = self.nav_layout(overlay_origin, overlay_dims) {
            draw_overlay_layout(cells, rows, cols, &layout, &self.theme);
        }
    }
}

/// The workspace peek body: everything the layout already holds for
/// one squad - its origin, its tabs (the active one marked), and its member
/// rows with their states. Pure and local; no wire round trip.
pub(super) fn squad_peek_lines(layout: &LayoutView, sid: u64) -> Vec<String> {
    let Some(s) = layout.squads.iter().find(|s| s.id == sid) else {
        return vec!["workspace is no longer here".into()];
    };
    let mut out = vec![
        format!("origin  {}", s.canonical_cwd),
        format!("tabs    {} · panes {}", s.tabs.len(), s.panes),
    ];
    for (i, t) in s.tabs.iter().enumerate() {
        let marker = if i == s.active_tab { "*" } else { " " };
        let label = if t.name.is_empty() {
            format!("tab {}", i + 1)
        } else {
            t.name.clone()
        };
        out.push(format!("{marker} {label}"));
    }
    let members: Vec<&AgentRow> = layout
        .agents
        .iter()
        .filter(|a| a.squad == Some(sid))
        .collect();
    if members.is_empty() {
        out.push("members none".into());
        return out;
    }
    out.push("members".into());
    for a in members {
        let state = if a.exited {
            "exited"
        } else {
            // The one state vocabulary (pane_state + the nav filter words): a
            // finished-but-unseen member is `done`, not `idle`.
            match pane_state(a.badge, a.seen, a.pane_activity) {
                PaneState::Blocked => "blocked",
                PaneState::Working => "working",
                PaneState::DoneUnseen => "done",
                // NOT "unread": that word names output nobody has
                // looked at, which is `DoneUnseen` - a different row, filed
                // under "done". A filter typed as `unread` would then return
                // rows with no liveness reading and EXCLUDE every row that
                // actually has unseen output, which is this branch's own
                // defect: a word standing in for a fact it does not name.
                PaneState::Unmeasured => "unmeasured",
                PaneState::Idle => "idle",
                PaneState::Empty => "empty",
            }
        };
        let pane = a
            .pane_id
            .map(|p| format!(" · pane {p}"))
            .unwrap_or_default();
        out.push(format!("  {} · {state}{pane}", a.name));
    }
    out
}
