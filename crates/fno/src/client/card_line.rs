//! Shared sideline card fields and hit ranges.

use std::collections::HashMap;
use std::ops::Range;

use unicode_width::UnicodeWidthStr;

use crate::proto::{AgentRow, BacklogCard};

/// Keep node and PR adjacent at the panel's right edge, separated by ` · `.
pub(super) fn identity_spans(a: &AgentRow, text_w: usize) -> IdentitySpans {
    let node = a.node.as_deref().filter(|s| !s.is_empty());
    let node_w = node.map_or(0, |s| s.width());
    // A bare PR number keeps main's six-column age-slot cap; the binding
    // suffix rides on top of an admitted number, never rescues an oversized
    // one.
    let bare_w = a.pr.map(|n| format!("#{n}").width()).filter(|w| *w <= 6);
    let suffix = binding_suffix(a);
    let pr_w = bare_w.map(|w| w + suffix.width());
    let pr = pr_w.filter(|w| *w <= text_w).map(|w| text_w - w..text_w);
    let gap = usize::from(node_w > 0 && pr.is_some()) * 3;
    let node_end = pr
        .as_ref()
        .map_or(text_w, |span| span.start.saturating_sub(gap));
    let node_span = (node_w > 0 && node_end >= node_w).then(|| node_end - node_w..node_end);
    let separator = node_span
        .as_ref()
        .zip(pr.as_ref())
        .map(|(node, pr)| node.end..pr.start);
    IdentitySpans {
        node: node_span,
        separator,
        pr,
    }
}

/// The other-bindings count that follows the PR text: ` +N PRs` when any
/// other binding carries a pr, else ` +N open` when another binding stays
/// open. Empty when the row drives nothing else. `N` counts the other
/// bindings; wire order is the fold's strongest-first order, the wire
/// carrying no ts.
pub(super) fn binding_suffix(a: &AgentRow) -> String {
    let others = a
        .bindings
        .iter()
        .filter(|b| Some(b.node.as_str()) != a.node.as_deref())
        .count();
    if others == 0 {
        return String::new();
    }
    let others_with_pr = a
        .bindings
        .iter()
        .filter(|b| Some(b.node.as_str()) != a.node.as_deref() && b.pr.is_some())
        .count();
    if others_with_pr > 0 {
        format!(" +{others_with_pr} PRs")
    } else {
        let open = a
            .bindings
            .iter()
            .filter(|b| Some(b.node.as_str()) != a.node.as_deref() && b.state == "open")
            .count();
        if open > 0 {
            format!(" +{others} open")
        } else {
            String::new()
        }
    }
}

/// Fit the node beside a name that must stay whole: `keep` is the first
/// column the node may take. The node keeps its tail and cuts its front
/// (`…0e630a0a`), and drops when fewer than three of its characters fit.
pub(super) fn fit_node(
    node: &str,
    span: Range<usize>,
    keep: usize,
) -> Option<(Range<usize>, String)> {
    if span.start >= keep {
        return Some((span, node.to_string()));
    }
    let room = span.end.saturating_sub(keep);
    if room < 4 {
        return None;
    }
    let chars: Vec<char> = node.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(room - 1)..]
        .iter()
        .collect();
    Some((span.end - room..span.end, format!("\u{2026}{tail}")))
}

pub(super) fn node_span(a: &AgentRow, text_w: usize) -> Option<Range<usize>> {
    identity_spans(a, text_w).node
}

pub(super) fn pr_span(a: &AgentRow, text_w: usize) -> Option<Range<usize>> {
    identity_spans(a, text_w).pr
}

pub(super) struct IdentitySpans {
    pub node: Option<Range<usize>>,
    pub separator: Option<Range<usize>>,
    pub pr: Option<Range<usize>>,
}

/// One metrics-line field's render state: a served value, a loading skeleton
/// (the fold has not landed yet), or hidden - this harness or role cannot
/// report it, so it never prints a `?`.
pub(super) enum MetricCell {
    Value(String),
    Loading,
    Hidden,
}

/// What the card's graph slot plots, from `config.mux.card_graph`:
/// the tool-activity ramp (default, blank until two intervals exist) or
/// the context percent's fill bar (steady for the whole session).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum CardGraph {
    #[default]
    Activity,
    Context,
}

/// The four metrics-line fields in paint order: activity, context,
/// compactions, tokens. Cost moved to line 2 (the operator's 2026-10-04
/// mockup; ruled d-027912c6), where card_detail_text paints it served-only.
/// Hidden outranks served: codex transcripts carry no context window or
/// compaction boundaries, so those two fields hide there, while its tool
/// calls land like any other transcript. Only claude and codex transcripts
/// resolve at all (`SessionTranscripts::find`), so any other harness shows
/// a dash for the activity cell - never a fabricated flat line - and a
/// bare pane or an exited row's unserved fields hide instead of pulsing.
pub(super) fn metric_cells(a: &AgentRow, mode: CardGraph) -> [MetricCell; 4] {
    let reportable = !a.exited && matches!(a.harness.as_deref(), Some("claude" | "codex"));
    let codex = a.harness.as_deref() == Some("codex");
    let field = |value: Option<String>, hidden: bool| {
        if hidden {
            MetricCell::Hidden
        } else {
            match value {
                Some(v) => MetricCell::Value(v),
                None if !reportable => MetricCell::Hidden,
                None => MetricCell::Loading,
            }
        }
    };
    let activity = match a.harness.as_deref() {
        Some("claude" | "codex") => match activity_cell(a, mode) {
            Some(cell) => MetricCell::Value(cell.text),
            None => MetricCell::Hidden,
        },
        // A transcriptless harness (opencode, pi): a dash, never a fake zero.
        Some(_) => MetricCell::Value("-".into()),
        // A bare pane has nothing that could ever land: hidden.
        None => MetricCell::Hidden,
    };
    [
        activity,
        field(a.context_used_pct.map(|p| format!("{p}%")), codex),
        field(a.compaction_count.map(|n| format!("{n}c")), codex),
        field(a.session_tokens.map(super::row_meter::token_cell), false),
    ]
}

/// The line-3 activity cell, per `config.mux.card_graph`. `Activity`
/// draws the served intervals from the server's ring as up to 8 ramp cells
/// scaled to the card's own max, blank until two intervals exist; the
/// painter colors each cell by its failed share. `Context` draws the
/// context percent's fill bar, steady for the whole session.
const ACTIVITY_CELLS: usize = 8;
const FILL_BAR_CELLS: usize = 5;
const ACTIVITY_RAMP: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

pub(super) struct ActivityCell {
    /// The glyphs, no percent suffix: context is a plain number beside it.
    pub text: String,
    /// One color kind per glyph (0 ok, 1 warn, 2 error), same length.
    pub kinds: Vec<u8>,
}

/// The failed-share color stops: zero failures paints ok, any share under
/// half paints warn, half or more paints error.
fn color_kind(calls: u8, failed: u8) -> u8 {
    if calls == 0 || failed == 0 {
        0
    } else if failed * 2 < calls {
        1
    } else {
        2
    }
}

pub(super) fn activity_cell(a: &AgentRow, mode: CardGraph) -> Option<ActivityCell> {
    if mode == CardGraph::Context {
        // The steady fill bar of the context percent for the whole
        // session, bare of the percent (the number sits beside it);
        // uncolored, so every kind reads ok.
        let p = a.context_used_pct?;
        let eighths = (usize::from(p.min(100)) * FILL_BAR_CELLS * 8).div_ceil(100);
        let bar = super::row_meter::bar_of(eighths, FILL_BAR_CELLS);
        let kinds = vec![0; bar.chars().count()];
        return Some(ActivityCell { text: bar, kinds });
    }
    let pairs = a.activity.as_ref()?;
    // The ramp waits for two intervals; before that the slot stays blank.
    if pairs.len() < 2 {
        return None;
    }
    let max = pairs.iter().map(|p| p.0).max().unwrap_or(0);
    let pad = ACTIVITY_CELLS.saturating_sub(pairs.len());
    let mut text: String = std::iter::repeat('▁').take(pad).collect();
    let mut kinds: Vec<u8> = std::iter::repeat(0).take(pad).collect();
    for (calls, failed) in pairs {
        text.push(ramp_height(*calls, max));
        kinds.push(color_kind(*calls, *failed));
    }
    Some(ActivityCell { text, kinds })
}

/// The height map: zero reads the flat baseline; everything else scales to
/// the card's own max over the 8 cells.
fn ramp_height(calls: u8, max: u8) -> char {
    if calls == 0 || max == 0 {
        ACTIVITY_RAMP[0]
    } else {
        ACTIVITY_RAMP[(usize::from(calls) * ACTIVITY_CELLS / usize::from(max)).clamp(1, 7)]
    }
}

/// Past this age a Loading field gives up the pulse and holds a static dash.
const LOADING_DASH_AFTER_S: u64 = 10;

/// Whether any field still waits on the fold: the breathe timer's arm signal.
/// A row whose Loading fields are all past [`LOADING_DASH_AFTER_S`] holds a
/// static dash and arms no more frames.
pub(super) fn has_loading(a: &AgentRow, mode: CardGraph, now: u64) -> bool {
    metric_cells(a, mode)
        .iter()
        .any(|c| matches!(c, MetricCell::Loading))
        && !loading_gave_up(a, now)
}

/// Past 10s a row's Loading fields hold a static dash: the pulse gave up. A
/// row with no `started_at` reads as fully aged - reportable rows set it at
/// spawn, so this covers a wire gap rather than a real case.
pub(super) fn loading_gave_up(a: &AgentRow, now: u64) -> bool {
    now.saturating_sub(a.started_at.unwrap_or(0)) > LOADING_DASH_AFTER_S
}

/// Skeleton widths mirror each field's served width (ramp, percent, count,
/// tokens) so a landing fold does not reflow the line.
const LOADING_W: [usize; 4] = [8, 4, 3, 8];

pub(super) fn metrics(
    a: &AgentRow,
    mode: CardGraph,
    now: u64,
    message: Option<&str>,
    width: usize,
) -> String {
    let phase = crate::lattice::spin_epoch().map(|t0| t0.elapsed().as_millis() as u64);
    let gave_up = loading_gave_up(a, now);
    let fields: Vec<String> = metric_cells(a, mode)
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            MetricCell::Value(v) => Some(v.clone()),
            MetricCell::Loading if !gave_up => {
                Some(super::row_meter::skeleton_cell(LOADING_W[i], phase))
            }
            // The fold never landed: a static dash at the field's own width
            // keeps the line from reflowing while it stops the pulse.
            MetricCell::Loading => Some(format!("{:<w$}", "-", w = LOADING_W[i])),
            MetricCell::Hidden => None,
        })
        .collect();
    let prefix = fields.join(" · ");
    let Some(message) = message.filter(|s| !s.is_empty()) else {
        return crate::chrome::clip(&prefix, width);
    };
    if prefix.is_empty() {
        // Every field hid: the message stands alone, no leading separator.
        return crate::chrome::clip(message, width);
    }
    let separator = " · ";
    let room = width.saturating_sub(crate::chrome::str_cols(&prefix));
    let separator_w = crate::chrome::str_cols(separator);
    if room <= separator_w {
        return crate::chrome::clip(&prefix, width);
    }
    format!(
        "{prefix}{separator}{}",
        crate::chrome::fit_ellipsis(message, room - separator_w)
    )
}

/// Card line 1's label in `w` columns: `slug · role`. The slug is the word
/// that names the card, so it stays whole and only the role is cut, from
/// its end. Too little room for a readable role leaves the slug alone; a
/// slug wider than `w` keeps its tail, the name cell's rule.
pub(super) fn card_label(slug: &str, role: Option<&str>, w: usize) -> String {
    const SEP: &str = " \u{b7} ";
    const MIN_ROLE_W: usize = 4;
    let slug_w = slug.width();
    if slug_w >= w {
        return crate::chrome::clip_tail(slug, w);
    }
    let Some(role) = role.filter(|r| !r.is_empty()) else {
        return slug.to_string();
    };
    let room = w - slug_w;
    if room < SEP.width() + MIN_ROLE_W {
        return slug.to_string();
    }
    let role = crate::chrome::fit_ellipsis(role, room - SEP.width());
    format!("{slug}{SEP}{role}")
}

/// A `claude-` id split into family and version: `claude-opus-4-8` is
/// `("opus", [4, 8])`. A trailing date stamp is no part of the version.
fn claude_family(id: &str) -> Option<(&str, Vec<u32>)> {
    let rest = id.strip_prefix("claude-")?;
    let mut family = None;
    let mut version = Vec::new();
    for part in rest.split('-') {
        if part.len() == 8 && part.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        match part.parse::<u32>() {
            Ok(n) => version.push(n),
            Err(_) => family = family.or(Some(part)),
        }
    }
    Some((family?, version))
}

/// Each Claude family's newest version in the models.dev catalog.
pub(super) type NewestFamilies = HashMap<String, Vec<u32>>;

pub(super) fn newest_families<'a>(ids: impl IntoIterator<Item = &'a str>) -> NewestFamilies {
    let mut newest = NewestFamilies::new();
    for (family, version) in ids.into_iter().filter_map(claude_family) {
        let slot = newest.entry(family.to_string()).or_default();
        if version > *slot {
            *slot = version;
        }
    }
    newest
}

/// The catalog read once per client: paint runs every frame, and the cache
/// refreshes daily. Tests read no catalog, so they never see the machine's.
fn catalog_families() -> &'static NewestFamilies {
    static NEWEST: std::sync::OnceLock<NewestFamilies> = std::sync::OnceLock::new();
    NEWEST.get_or_init(|| {
        if cfg!(test) {
            return NewestFamilies::new();
        }
        let catalog = crate::model_catalog::load(&crate::model_catalog::state_dir());
        let ids = catalog
            .ok()
            .and_then(|c| c.get("anthropic").map(|p| p.model_ids()))
            .unwrap_or_default();
        newest_families(ids.iter().map(String::as_str))
    })
}

/// The card's short model, with no `[1m]` window tag. A `claude-` id names
/// its family alone when it is that family's newest (`opus`), else the
/// family and version (`opus-4.8`). With no catalog it names the family.
pub(super) fn short_model(model: &str, newest: &NewestFamilies) -> String {
    let model = model.split('[').next().unwrap_or(model);
    let Some((family, version)) = claude_family(model) else {
        return model.to_string();
    };
    match newest.get(family) {
        Some(top) if *top != version && !version.is_empty() => {
            let dotted: Vec<String> = version.iter().map(u32::to_string).collect();
            format!("{family}-{}", dotted.join("."))
        }
        _ => family.to_string(),
    }
}

/// Line 2's model, shortened to the name users recognize.
pub(super) fn model_label(a: &AgentRow) -> Option<String> {
    a.model
        .as_deref()
        .map(|m| short_model(m, catalog_families()))
        .filter(|m| !m.is_empty())
}

/// The board's model badge includes the harness; card line 2 keeps just the
/// model label so the lead name or role remains visible.
pub(super) fn harness_model(a: &AgentRow) -> Option<String> {
    match (a.harness.as_deref(), model_label(a)) {
        (Some(h), Some(m)) => Some(format!("{h}/{m}")),
        (Some(h), None) => Some(h.to_string()),
        (None, model) => model,
    }
}

/// The card's name: the worker name without the node and model the card
/// now shows in their own columns. `t-<node>-opus` keeps only `t`, so it
/// reads `t-<node slug>` when the board knows the node; with no slug, it
/// keeps the node rather than show a bare prefix.
pub(super) fn slug(a: &AgentRow, backlog: &[BacklogCard]) -> String {
    let model_tokens: Vec<&str> = a
        .model
        .as_deref()
        .map(|m| m.split('[').next().unwrap_or(m).split('-').collect())
        .unwrap_or_default();
    let strip_model = |name: &str| -> String {
        match name.rsplit_once('-') {
            Some((head, last))
                if !head.is_empty()
                    && model_tokens.iter().any(|t| t.eq_ignore_ascii_case(last)) =>
            {
                head.to_string()
            }
            _ => name.to_string(),
        }
    };
    let Some(node) = a.node.as_deref().filter(|n| !n.is_empty()) else {
        return strip_model(&a.name);
    };
    let bare = node.replace('-', "");
    let parts: Vec<&str> = a.name.split('-').collect();
    let node_parts: Vec<&str> = node.split('-').collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        if parts[i..].starts_with(&node_parts) {
            i += node_parts.len();
        } else if parts[i] == bare {
            i += 1;
        } else {
            kept.push(parts[i]);
            i += 1;
        }
    }
    if kept.len() == parts.len() {
        return strip_model(&a.name);
    }
    let rest = strip_model(&kept.join("-"));
    if rest.contains('-') {
        return rest;
    }
    // Only a prefix (or nothing) is left: the node was the name's substance.
    match backlog
        .iter()
        .find(|c| c.id == node)
        .map(|c| c.slug.as_str())
    {
        Some(s) if !s.is_empty() && rest.is_empty() => s.to_string(),
        Some(s) if !s.is_empty() => format!("{rest}-{s}"),
        _ => strip_model(&a.name),
    }
}

#[cfg(test)]
mod binding_display_tests {
    use super::*;
    use crate::proto::SessionBinding;

    fn row(bindings: Vec<SessionBinding>) -> AgentRow {
        AgentRow {
            name: "w".into(),
            node: Some("x-cur".into()),
            pr: Some(3159),
            bindings,
            ..Default::default()
        }
    }

    fn binding(node: &str, pr: Option<u64>, state: &str, inferred: bool) -> SessionBinding {
        SessionBinding {
            node: node.into(),
            pr,
            source: "claim".into(),
            state: state.into(),
            inferred,
        }
    }

    #[test]
    fn suffix_is_empty_then_counts_others() {
        // Nothing else drives the row: no suffix, however the one binding
        // sits or a merged other closes out.
        assert_eq!(binding_suffix(&row(vec![])), "");
        let only_current = row(vec![binding("x-cur", Some(3159), "open", false)]);
        assert_eq!(binding_suffix(&only_current), "");
        // A merged other without a pr is closed news: still no suffix.
        let closed = row(vec![
            binding("x-cur", Some(3159), "open", false),
            binding("x-oth1", None, "merged", false),
        ]);
        assert_eq!(binding_suffix(&closed), "");
        // Others carrying prs count as PRs; others without count as open
        // slots instead.
        let a = row(vec![
            binding("x-cur", Some(3159), "open", false),
            binding("x-oth1", Some(100), "open", false),
            binding("x-oth2", Some(200), "merged", false),
        ]);
        assert_eq!(binding_suffix(&a), " +2 PRs");
        let b = row(vec![
            binding("x-cur", Some(3159), "open", false),
            binding("x-oth1", None, "open", false),
            binding("x-oth2", None, "merged", false),
        ]);
        assert_eq!(binding_suffix(&b), " +2 open");
    }

    #[test]
    fn pr_span_width_covers_node_pr_and_count() {
        // `x-cur · #3159 +1 PRs` at the right edge of 40 columns.
        let a = row(vec![
            binding("x-cur", Some(3159), "open", false),
            binding("x-oth", Some(100), "open", false),
        ]);
        let spans = identity_spans(&a, 40);
        let pr = spans.pr.expect("pr span renders");
        assert_eq!(pr, 28..40);
        assert_eq!(spans.node, Some(20..25));
        assert_eq!(spans.separator, Some(25..28));
    }

    #[test]
    fn fit_node_cuts_the_node_front_before_the_name() {
        // `scout` ends at column 7 with one space after it, so the node may
        // start at 8. The specimen cut the name to `scou` instead.
        let node = "zz-00aa11bb";
        assert_eq!(fit_node(node, 10..21, 8), Some((10..21, node.into())));
        assert_eq!(
            fit_node(node, 6..17, 8),
            Some((8..17, "\u{2026}00aa11bb".into()))
        );
        assert_eq!(fit_node(node, 6..17, 14), None, "under 3 node chars drops");
    }
}
