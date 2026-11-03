//! The needs-me overlay's projection and render, moved out of client.rs
//! (file-budget shrink): the projection is one question - "what needs
//! me, worst first" - so it answers from its own module. The gesture sites,
//! the run-loop arms and the tests all read these items through the re-exports
//! in the parent. The feed overlay (`feed_view.rs`) reuses
//! [`View::join_fold_row`]'s join keys, so a feed row and a sideline row reach
//! a session through one deep-link path.

use super::*;
use crate::text_style::{TLine, TextStyle};

impl View {
    /// The roster row a fold item joins to: a name / node / session-id match
    /// against a layout row's name or its cwd basename (`cwd_base`, now carried
    /// on every row since US3, not only orphans).
    pub(crate) fn join_fold_row(&self, item: &crate::needs_overlay::FoldItem) -> Option<&AgentRow> {
        let keys: Vec<&str> = [
            item.name.as_deref(),
            item.node.as_deref(),
            Some(item.session_id.as_str()),
        ]
        .into_iter()
        .flatten()
        .collect();
        self.layout.agents.iter().find(|a| {
            keys.iter().any(|k| a.name == *k)
                || a.cwd_base.as_deref().is_some_and(|c| keys.contains(&c))
        })
    }

    /// The two-lane overlay projection: MINE first (open before done, file
    /// order preserved within each group - the operator wrote that order),
    /// then the operator-filtered needs queue (already worst-first). Each
    /// lane is capped independently at ten so a noisy lane never crowds the
    /// other out; `*_total` carries the true count for the footer.
    pub(crate) fn needs_projection(&self) -> NeedsProjection {
        let mut mine: Vec<crate::needs_overlay::MineItem> =
            self.mine_fold.clone().unwrap_or_default();
        mine.sort_by_key(|m| m.done);
        let mine_total = mine.len();
        let mine_shown = mine.len().min(MINE_CAP);
        let mut rows: Vec<NeedsOverlayRow> = mine
            .into_iter()
            .take(MINE_CAP)
            .map(NeedsOverlayRow::Mine)
            .collect();

        // Questions lead the NEED section: a real operator question, with an
        // asker to answer back to, outranks a bare carveout/claims pile. The
        // projection's order stands (high priority first, then newest).
        let questions: Vec<crate::needs_overlay::QuestionItem> =
            self.questions_fold.clone().unwrap_or_default().items;
        let need = self.needs_operator_queue();
        let need_total = questions.len() + need.len();
        let need_shown = need_total.min(NEEDS_CAP);
        rows.extend(
            questions
                .into_iter()
                .map(NeedsOverlayRow::Question)
                .chain(need.into_iter().map(NeedsOverlayRow::Need))
                .take(NEEDS_CAP),
        );

        NeedsProjection {
            rows,
            mine_shown,
            mine_total,
            need_shown,
            need_total,
        }
    }
}

pub(crate) struct NeedsProjection {
    pub(crate) rows: Vec<NeedsOverlayRow>,
    pub(crate) mine_shown: usize,
    pub(crate) mine_total: usize,
    pub(crate) need_shown: usize,
    pub(crate) need_total: usize,
}

impl NeedsProjection {
    pub(crate) fn selected_line(&self, sel: usize) -> usize {
        if sel < self.mine_shown {
            // instruction + MINE heading
            2 + sel
        } else {
            // instruction + MINE heading/rows/footer + THEY NEED YOU heading
            4 + self.mine_shown + sel.saturating_sub(self.mine_shown)
        }
    }
}

/// Build the needs-me overlay lines (two-laned by): MINE (the
/// operator's own priorities) above THEY NEED YOU (the severity-ranked union
/// + the selected row's answer options), each with its own state footer, on
/// the shared inverse-video chrome. `sel` is pre-clamped by the caller and
/// indexes `projection.rows` (MINE rows first, then need rows) - the cursor fill marks
/// the selected row wherever it falls. An answerable row lists its numbered
/// options only when it is the selected NEED row; a focus-only row is tagged.
/// Always renders both headings - an empty NEED lane shows "nothing needs
/// you", so the overlay never opens blank. Layout is pinned 1:1 with
/// [`NeedsProjection::selected_line`]: MINE rows occupy exactly
/// `mine_shown` lines and need rows exactly `need_shown` (or one "nothing
/// needs you" line), with no extra divider row between them.
pub(crate) fn needs_overlay_lines(
    projection: &NeedsProjection,
    sel: usize,
    mine_footer: NeedsFooter,
    need_footer: NeedsFooter,
) -> Vec<TLine> {
    let mine_rows = &projection.rows[..projection.mine_shown];
    let need_rows = &projection.rows[projection.mine_shown..];

    let styled = |style: TextStyle, text: &str| TLine::line(style, pad_to(text, ANSWER_OVERLAY_W));
    let mut lines = vec![styled(
        TextStyle::Hint,
        " digit answers · n/N cycle · ⏎ goto",
    )];

    lines.push(styled(TextStyle::Section, " Mine"));
    for row in mine_rows {
        let NeedsOverlayRow::Mine(item) = row else {
            continue;
        };
        let check = if item.done { '✓' } else { ' ' };
        lines.push(styled(
            TextStyle::Item,
            &format!("   [{check}] {}", item.text),
        ));
    }
    let mine_footer_line = match mine_footer {
        NeedsFooter::Folding => "   folding...".to_string(),
        NeedsFooter::Degraded => "   MINE unavailable".to_string(),
        NeedsFooter::AsOf if projection.mine_total > projection.mine_shown => format!(
            "   {} of {} shown",
            projection.mine_shown, projection.mine_total
        ),
        NeedsFooter::AsOf => String::new(),
    };
    lines.push(styled(TextStyle::Hint, &mine_footer_line));

    lines.push(styled(TextStyle::Section, " They need you"));
    if need_rows.is_empty() {
        lines.push(styled(TextStyle::Hint, "   nothing needs you"));
    } else {
        for row in need_rows.iter() {
            match row {
                NeedsOverlayRow::Question(q) => {
                    // Render `title` as the headline (the projection's one-line
                    // form of the question); the prose body appears only when
                    // selected, below.
                    let stale = if q.asker.as_ref().and_then(|a| a.live) == Some(false) {
                        "  STALE"
                    } else {
                        ""
                    };
                    lines.push(tail_styled(
                        &format!("   {} {}", need_glyph(NeedKind::Question), q.title),
                        stale,
                        TextStyle::WARN,
                    ));
                }
                NeedsOverlayRow::Need(r) => {
                    let tag = match r.kind {
                        NeedKind::BlockedFocusOnly => "  ⚠ focus",
                        _ => "",
                    };
                    let mut line = TLine::parts(&[
                        (&format!("   {} ", need_glyph(r.kind)), TextStyle::Item),
                        (&r.name, TextStyle::Value),
                        (&format!("  {}", r.reason), TextStyle::Meta),
                        (tag, TextStyle::WARN),
                    ]);
                    line = line.pad_to(ANSWER_OVERLAY_W);
                    lines.push(line);
                }
                NeedsOverlayRow::Mine(_) => {}
            }
        }
        let selected_need = sel
            .checked_sub(projection.mine_shown)
            .and_then(|i| need_rows.get(i));
        match selected_need {
            Some(NeedsOverlayRow::Need(r)) => {
                if let Some(ans) = r.answerable.as_ref() {
                    if !ans.prompt.is_empty() {
                        lines.push(styled(
                            TextStyle::Item,
                            &format!("   {}", ans.prompt.replace('\n', " ")),
                        ));
                    }
                    for o in &ans.options {
                        lines.push(styled(
                            TextStyle::Meta,
                            &format!("     {}. {}", o.idx, o.label),
                        ));
                    }
                }
            }
            Some(NeedsOverlayRow::Question(q)) => {
                // The prose body beneath the headline, then each option's text
                // with its what-happens-next clause.
                if let Some(body) = q.body.as_deref() {
                    if !body.is_empty() && q.body.as_deref() != Some(q.title.as_str()) {
                        lines.push(styled(
                            TextStyle::Item,
                            &format!("   {}", body.replace('\n', " ")),
                        ));
                    }
                }
                for opt in q.options.iter() {
                    lines.push(styled(
                        TextStyle::Meta,
                        &format!(
                            "     {}. {} -> {}",
                            opt.n,
                            opt.text,
                            opt.next.as_deref().unwrap_or("?")
                        ),
                    ));
                }
                if q.asker.as_ref().and_then(|a| a.live) == Some(false) {
                    lines.push(styled(
                        TextStyle::WARN,
                        "   the answer is recorded but reaches no session",
                    ));
                }
            }
            _ => {}
        }
    }
    let need_footer_line = match need_footer {
        NeedsFooter::Folding => "   folding events...".to_string(),
        NeedsFooter::Degraded => "   events fold unavailable - live badges only".to_string(),
        NeedsFooter::AsOf if projection.need_total > projection.need_shown => format!(
            "   {} of {} shown",
            projection.need_shown, projection.need_total
        ),
        NeedsFooter::AsOf => "   as of now".to_string(),
    };
    lines.push(styled(TextStyle::Hint, &need_footer_line));
    lines
}

/// `head` at item level and `tail` at `style`, padded to the overlay width.
fn tail_styled(head: &str, tail: &str, style: TextStyle) -> TLine {
    TLine::parts(&[(head, TextStyle::Item), (tail, style)]).pad_to(ANSWER_OVERLAY_W)
}
