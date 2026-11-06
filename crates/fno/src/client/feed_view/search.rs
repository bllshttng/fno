//! The feed's search surface: the one shared grammar (`crate::search_query`)
//! parsed once per gesture; the pushable half travels as projection flags,
//! the rest is matched client-side over [`event_fields`]. The keys overlay
//! and the search bar's completion read the same shared table, so the feed
//! never carries its own copy of the grammar.

use crate::feed_overlay::FeedFilter;

/// The flat flags one parsed query pushes into the projection, `None` when
/// any term falls outside the pushable keys. The projection then receives
/// nothing at all, and the client matches every landed page itself - the one
/// rule that keeps a half-pushed query from lying about its scope.
pub(crate) fn prefilter(q: &crate::search_query::Parsed) -> Option<FeedFilter> {
    let pd = q.pushdown()?;
    let mut f = FeedFilter::default();
    for (key, vals) in pd.vals {
        let joined = vals.join(",");
        match key {
            "id" => f.node = Some(joined),
            "kind" => f.kind = Some(joined),
            "area" => f.area = Some(joined),
            "session" => f.session = Some(joined),
            "agent" => f.agent = Some(joined),
            "harness" => f.harness = Some(joined),
            "lead" => f.lead = Some(joined),
            // A key with no projection flag is not pushable; pushdown
            // already refuses it, so this is the belt under the belt.
            _ => return None,
        }
    }
    if let Some((lo, hi)) = pd.ts {
        f.since = lo.map(|v| v.max(0) as u64);
        f.until = hi.map(|v| v.max(0) as u64);
    }
    Some(f)
}

/// Parse one query for the feed surface; `Err` carries the shared refusal
/// (an unknown key, a node-only key) verbatim for the footer.
pub(crate) fn parse_query(text: &str) -> Result<crate::search_query::Parsed, String> {
    crate::search_query::parse(
        text,
        crate::search_query::Surface::Event,
        crate::search_query::now_secs(),
    )
}

/// The `?` overlay: the panel's keys, then every key the shared table marks
/// answerable on feed rows - read from `search_query::KEYS`, never a local
/// copy, so the overlay and the grammar cannot drift.
pub(crate) fn feed_keys_popup() -> crate::popup::Popup {
    use crate::popup::{Popup, PopupRow};
    let pick = |label: &str| PopupRow::Entry {
        glyph: " ".into(),
        label: label.into(),
        hint: String::new(),
        enabled: true,
    };
    let kind_word = |k: crate::search_query::Kind| match k {
        crate::search_query::Kind::Value => "value prefix",
        crate::search_query::Kind::Text => "text",
        crate::search_query::Kind::Date => "date",
        crate::search_query::Kind::Number => "number",
        crate::search_query::Kind::Age => "age",
        crate::search_query::Kind::Flag => "exact word",
        crate::search_query::Kind::Sort => "sort",
    };
    let mut rows: Vec<PopupRow> = vec![
        PopupRow::Header("keys".into()),
        PopupRow::Rule,
        pick("up/down row - enter details - o order"),
        pick("g home (newest) - G oldest - arrows pan"),
        pick("/ search - ? keys - esc close"),
        PopupRow::Header("query keys".into()),
        PopupRow::Rule,
    ];
    for def in crate::search_query::KEYS.iter().filter(|d| d.event) {
        let names: Vec<String> = def.names.iter().map(|n| format!("{n}:")).collect();
        rows.push(pick(&format!(
            "{} - {}",
            names.join(" "),
            kind_word(def.kind)
        )));
    }
    Popup::new(rows, crate::popup::Anchor::Center)
        .title("feed keys")
        .footer("esc close")
}

/// One Tab press on the bar text: complete the token before the cursor.
/// A key prefix completes to the full key names the event surface takes; a
/// value after a value key completes from the distinct values in the loaded
/// window. Repeated Tab cycles. `None` leaves the text.
pub(crate) fn complete(text: &str, items: &[crate::feed_overlay::FeedItem]) -> Option<String> {
    let (prefix, token) = match text.rsplit_once(' ') {
        Some((p, t)) => (format!("{p} "), t),
        None => (String::new(), text),
    };
    if !token.contains(':') {
        let mut cands: Vec<&str> = Vec::new();
        for def in crate::search_query::KEYS.iter().filter(|d| d.event) {
            for name in def.names {
                if name.starts_with(token) && !cands.contains(name) {
                    cands.push(name);
                }
            }
        }
        cands.sort();
        if cands.is_empty() {
            return None;
        }
        let next = cands
            .iter()
            .position(|c| c.to_string() == token)
            .map(|i| i + 1)
            .unwrap_or(0);
        return Some(format!("{prefix}{}", cands[next % cands.len()]));
    }
    let (key, value) = token.split_once(':')?;
    let field = match key {
        "id" | "n" => "id",
        "sid" | "session" => "session",
        "a" | "agent" => "agent",
        "h" | "harness" => "harness",
        "k" | "kind" => "kind",
        "l" | "lead" => "lead",
        _ => return None,
    };
    let ctx = crate::feed_overlay::EventCtx::from_rows(&[], &[]);
    let mut vals: Vec<String> = Vec::new();
    for it in items {
        for v in crate::feed_overlay::event_fields(it, &ctx)
            .get(field)
            .into_iter()
            .flatten()
        {
            if v.starts_with(value) && !vals.contains(v) {
                vals.push(v.clone());
            }
        }
    }
    vals.sort();
    if vals.is_empty() {
        return None;
    }
    let next = vals
        .iter()
        .position(|v| v == value)
        .map(|i| i + 1)
        .unwrap_or(0);
    Some(format!("{prefix}{key}:{}", vals[next % vals.len()]))
}

/// The bar's text settled: parse the query, recompute the pushable flags,
/// and re-arm the head page under the new filter. A refusal shows in the
/// footer verbatim and fetches nothing.
pub(crate) fn apply_query(o: &mut super::FeedOverlay) {
    o.scan_pages = 0;
    o.scan_note = None;
    if o.query_text.is_empty() {
        o.parsed = None;
        o.pushed = false;
        o.filter = None;
        o.want_page = Some(crate::feed_overlay::PageReq::Head);
        o.want = true;
        return;
    }
    match parse_query(&o.query_text) {
        Ok(parsed) => {
            let pre = prefilter(&parsed);
            o.pushed = pre.is_some();
            o.filter = pre;
            o.parsed = Some(Ok(parsed));
            o.win = super::page::FeedWindow::default();
            o.want_page = Some(crate::feed_overlay::PageReq::Head);
            o.want = true;
        }
        Err(e) => {
            o.parsed = Some(Err(e));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn _refusal_case() {
        // AC8-EDGE: the shared refusal names `s:` as node-only, and the
        // caller arms no fetch from a refused parse.
        let err = crate::search_query::parse(
            "s:ready",
            crate::search_query::Surface::Event,
            1_791_854_400,
        )
        .expect_err("s: refuses on events");
        assert!(err.contains("s:"), "the refusal names the key: {err}");
    }

    fn _prefilter_case() {
        let pf = |q: &str| prefilter(&parse_query(q).unwrap());
        // h:codex k:question -> --harness codex --kind question
        let f = pf("h:codex k:question").expect("one positive group pushes");
        assert_eq!(f.harness.as_deref(), Some("codex"));
        assert_eq!(f.kind.as_deref(), Some("question"));
        // Two groups push nothing.
        assert!(pf("id:x-1 | k:pane").is_none(), "two groups push none");
        // A negation pushes nothing.
        assert!(pf("-k:question").is_none(), "negation pushes none");
        // Free text pushes nothing.
        assert!(pf("stall").is_none(), "bare words push none");
        // A date pushes the epoch bounds.
        let f = pf("ts:>=2026-10-01").expect("a date pushes");
        assert!(f.since.is_some());
        assert_eq!(f.until, None);
        // A bare node id pushes --node.
        let f = pf("n-1234").expect("an id-shaped bare word pushes");
        assert_eq!(f.node.as_deref(), Some("n-1234"));
    }
}
