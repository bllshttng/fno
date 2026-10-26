//! Classify harvested items into node or inbox candidates.
//!
//! Deterministic and verbatim: the body carries the source's reasoning
//! unchanged plus a cite, and the title comes from the finding's own first
//! line. An item that cannot be tied to a source is marked `uncited` and
//! never lands.

use std::sync::OnceLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::dedup::cv_cite_needle;
use super::{char_len, take_chars, Candidate, RawItem, KIND_CARVEOUT, TIER_INBOX, TIER_NODE};

pub const DEFAULT_PRIORITY: &str = "p3";
pub const TITLE_CAP: usize = 100;
pub const BODY_CAP: usize = 6000;

pub const DISPOSITION_NODE: &str = "node";
pub const DISPOSITION_INBOX: &str = "inbox";
pub const DISPOSITION_ARCHIVE: &str = "archive";

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("classify regex"))
}

/// critical lands p1, not p0: p0 needs a human ack the harvest cannot give,
/// so a p0 mapping failed every critical finding on every harvest.
pub fn severity_to_priority(severity: Option<&str>) -> String {
    match severity.unwrap_or("").to_lowercase().as_str() {
        "critical" | "high" => "p1",
        "medium" => "p2",
        "low" => "p3",
        _ => DEFAULT_PRIORITY,
    }
    .to_string()
}

pub fn severity_to_difficulty(severity: Option<&str>) -> String {
    match severity.unwrap_or("").to_lowercase().as_str() {
        "critical" | "high" => "high",
        "low" => "low",
        _ => "medium",
    }
    .to_string()
}

/// node for critical/high/medium and for severity-less deliberate work;
/// inbox for low.
pub fn severity_to_tier(severity: Option<&str>) -> String {
    match severity {
        None => TIER_NODE,
        Some(s) if matches!(s.to_lowercase().as_str(), "critical" | "high" | "medium") => TIER_NODE,
        Some(_) => TIER_INBOX,
    }
    .to_string()
}

/// The first non-empty line, with badges, HTML tags and markdown markers
/// stripped. The codex bot wraps its badge in `<sub>` tags, which would
/// otherwise survive the badge strip.
pub(crate) fn clean_line(text: &str) -> String {
    static BADGE: OnceLock<Regex> = OnceLock::new();
    static TAG: OnceLock<Regex> = OnceLock::new();
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static TRAIL: OnceLock<Regex> = OnceLock::new();
    for raw in py_splitlines(text) {
        let s = re(&BADGE, r"!\[[^\]]*\]\([^)]*\)|!\[[^\]]*\]").replace_all(raw, "");
        let s = re(&TAG, r"</?[a-zA-Z][^>]*>").replace_all(&s, "");
        let s = re(&LEAD, r"^[#>\-\*\s`]+").replace(&s, "");
        let s = re(&TRAIL, r"[\*`\s]+$").replace(&s, "");
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    String::new()
}

/// Python's `str.splitlines()`: splits on every line boundary it knows and
/// drops the separators.
pub(crate) fn py_splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        let is_break = matches!(
            ch,
            '\n' | '\r'
                | '\u{0b}'
                | '\u{0c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        );
        if is_break {
            out.push(&text[start..i]);
            let mut end = i + ch.len_utf8();
            if ch == '\r' {
                if let Some(&(_, '\n')) = chars.peek() {
                    chars.next();
                    end += 1;
                }
            }
            start = end;
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn cap_title(summary: String) -> String {
    if char_len(&summary) > TITLE_CAP {
        format!("{}…", take_chars(&summary, TITLE_CAP - 1).trim_end())
    } else {
        summary
    }
}

/// One-line summary from the finding's own text, never a generic stub.
/// A carve-out titles from its description first: `need` names a blocker,
/// not a summary, and it is optional.
pub fn derive_title(item: &RawItem) -> String {
    let mut summary = clean_line(&item.text);
    if item.kind == KIND_CARVEOUT {
        if summary.is_empty() {
            summary = item.title_hint.as_deref().unwrap_or("").trim().to_string();
        }
        if item.subkind.as_deref() == Some("oos-bug") && !summary.to_lowercase().starts_with("bug")
        {
            summary = format!("bug: {summary}");
        }
    }
    if summary.is_empty() {
        summary = "(untitled left-out item)".to_string();
    }
    cap_title(summary)
}

fn resolve_priority(item: &RawItem) -> String {
    if let Some(p) = item.priority.as_deref().filter(|p| !p.is_empty()) {
        return p.to_string();
    }
    if let Some(s) = item.severity.as_deref().filter(|s| !s.is_empty()) {
        return severity_to_priority(Some(s));
    }
    DEFAULT_PRIORITY.to_string()
}

/// Verbatim reasoning plus a source cite, truncated past the cap. The cite,
/// the truncation marker and a carve-out's `Blocked on:` line are reserved
/// out of the budget first, so the whole body fits.
pub fn build_body(item: &RawItem, cap: usize) -> String {
    let mut reasoning = item.text.clone();
    let mut cite_bits: Vec<String> = Vec::new();
    if let Some(pr) = item.source_pr {
        cite_bits.push(format!("PR #{pr}"));
    }
    if let Some(r) = item.reviewer.as_deref().filter(|r| !r.is_empty()) {
        cite_bits.push(format!("reviewer `{r}`"));
    }
    match item.url.as_deref().filter(|u| !u.is_empty()) {
        Some(u) => cite_bits.push(u.to_string()),
        None if !item.source_id.is_empty() => cite_bits.push(cv_cite_needle(&item.source_id)),
        None => {}
    }
    let cite = if cite_bits.is_empty() {
        String::new()
    } else {
        format!("Source: {}", cite_bits.join(", "))
    };

    let link = match (
        item.url.as_deref().filter(|u| !u.is_empty()),
        item.source_pr,
    ) {
        (Some(u), _) => u.to_string(),
        (None, Some(pr)) if pr != 0 => format!("PR #{pr}"),
        _ => "source".to_string(),
    };
    let marker = format!("\n\n[... truncated; full text at {link} ...]");
    let need = item.title_hint.as_deref().unwrap_or("").trim().to_string();
    let blocked = if item.kind == KIND_CARVEOUT && !need.is_empty() {
        format!("Blocked on: {need}")
    } else {
        String::new()
    };
    let overhead = char_len(&marker)
        + if cite.is_empty() {
            0
        } else {
            char_len(&cite) + 2
        }
        + if blocked.is_empty() {
            0
        } else {
            char_len(&blocked) + 2
        };
    let budget = cap.saturating_sub(overhead).max(200);
    if char_len(&reasoning) > budget {
        reasoning = format!("{}{marker}", take_chars(&reasoning, budget).trim_end());
    }
    let parts = [reasoning.trim().to_string(), blocked, cite];
    parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn opt(v: &Option<String>) -> Value {
    v.as_ref().map_or(Value::Null, |s| Value::String(s.clone()))
}

/// One item to one candidate, marked `uncited` when it has no cite. A
/// review finding's cite is its PR; a carve-out's cite is its own ledger
/// id, because the PR-less sweep has no PR to offer.
pub fn classify_item(item: &RawItem, body_cap: usize) -> Candidate {
    let cited =
        !item.source_id.is_empty() && (item.source_pr.is_some() || item.kind == KIND_CARVEOUT);
    if !cited {
        return Candidate {
            title: derive_title(item),
            body: item.text.clone(),
            tier: TIER_NODE.to_string(),
            priority: DEFAULT_PRIORITY.to_string(),
            source_pr: item.source_pr,
            source_id: item.source_id.clone(),
            uncited: true,
            ..Candidate::default()
        };
    }
    let mut extra = Map::new();
    extra.insert("kind".into(), Value::String(item.kind.clone()));
    extra.insert("subkind".into(), opt(&item.subkind));
    extra.insert("severity".into(), opt(&item.severity));
    extra.insert("reviewer".into(), opt(&item.reviewer));
    Candidate {
        title: derive_title(item),
        body: build_body(item, body_cap),
        tier: severity_to_tier(item.severity.as_deref()),
        priority: resolve_priority(item),
        source_pr: item.source_pr,
        source_id: item.source_id.clone(),
        finding_text: item.text.clone(),
        extra,
        ..Candidate::default()
    }
}

/// `(cited, uncited)`.
pub fn classify(items: &[RawItem]) -> (Vec<Candidate>, Vec<Candidate>) {
    let mut cited = Vec::new();
    let mut uncited = Vec::new();
    for it in items {
        let c = classify_item(it, BODY_CAP);
        if c.uncited {
            uncited.push(c);
        } else {
            cited.push(c);
        }
    }
    (cited, uncited)
}

const PM_STUCK_REASONS: [&str; 4] = ["noprogress", "budget", "interrupted", "aborted"];

/// Rule-first disposition for one postmortem: node, inbox, or archive.
/// Ambiguous goes to inbox. The candidate is `None` for archive.
pub fn classify_postmortem(item: &RawItem, body_cap: usize) -> (String, Option<Candidate>) {
    static ONEOFF: OnceLock<Regex> = OnceLock::new();
    static WEDGE: OnceLock<Regex> = OnceLock::new();
    static BLOCKLIST: OnceLock<Regex> = OnceLock::new();
    let reason = item.subkind.clone().unwrap_or_default();
    let reason_lower = reason.to_lowercase();
    // Archive only on an explicit one-off reason kind. The finalize stuck
    // reasons contain the same tokens, so they are exempt by exact match.
    if !PM_STUCK_REASONS.contains(&reason_lower.as_str())
        && re(&ONEOFF, r"(?i)cancel|interrupt|abort").is_match(&reason)
    {
        return (DISPOSITION_ARCHIVE.to_string(), None);
    }
    if reason_lower.starts_with("done") {
        return (DISPOSITION_ARCHIVE.to_string(), None);
    }
    let wedge = re(
        &WEDGE,
        r"(?i)split.?brain|wedge|respawn\s+loop|orphan|deadlock|stale\W+(\w+\W+){0,3}claim|claim\W+(\w+\W+){0,3}stale|budget\s+blowout",
    )
    .is_match(&item.text);
    // A real wedge always surfaces; only a non-wedge is archived by the
    // blocklist of negative tool claims and transient errors.
    if !wedge
        && re(
            &BLOCKLIST,
            r"(?i)\bis\s+(?:absent|missing|not\s+installed)\b|\b(?:doesn'?t|does\s+not)\s+(?:exist|work)\b|\bis\s+broken\b|\b(?:rate[\s-]?limit(?:ed)?|connection\s+reset|network\s+(?:error|timeout)|transient\s+(?:error|failure))\b",
        )
        .is_match(&item.text)
    {
        return (DISPOSITION_ARCHIVE.to_string(), None);
    }
    let tier = if wedge { TIER_NODE } else { TIER_INBOX };
    let mut title = item.title_hint.as_deref().unwrap_or("").trim().to_string();
    if title.is_empty() {
        title = clean_line(&item.text);
    }
    if title.is_empty() {
        title = "(untitled postmortem)".to_string();
    }
    let title = cap_title(title);
    let mut body = item.text.trim().to_string();
    let marker = "\n\n[... truncated; full text in the postmortem file ...]";
    let cite = format!("Source: {}", item.source_id);
    let budget = body_cap
        .saturating_sub(char_len(marker) + char_len(&cite) + 2)
        .max(200);
    if char_len(&body) > budget {
        body = format!("{}{marker}", take_chars(&body, budget).trim_end());
    }
    let mut extra = Map::new();
    extra.insert("kind".into(), Value::String(item.kind.clone()));
    extra.insert("subkind".into(), opt(&item.subkind));
    let candidate = Candidate {
        title,
        body: if body.is_empty() {
            cite.clone()
        } else {
            format!("{body}\n\n{cite}")
        },
        tier: tier.to_string(),
        priority: if wedge {
            "p2".to_string()
        } else {
            DEFAULT_PRIORITY.to_string()
        },
        source_pr: None,
        source_id: item.source_id.clone(),
        finding_text: item.text.clone(),
        extra,
        ..Candidate::default()
    };
    let disposition = if wedge {
        DISPOSITION_NODE
    } else {
        DISPOSITION_INBOX
    };
    (disposition.to_string(), Some(candidate))
}
