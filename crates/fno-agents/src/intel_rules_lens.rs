//! `fno-agents intel --lens rules`: how often each event rule fires, and how
//! often it fires wrongly. Each rule is a guess about a failure, and each
//! fire is a `decision_span` row carrying `rule`, so the counts are a journal
//! read. A judge marks sampled fires in a labels file, and the lens reads the
//! file back as a false-fire rate.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;

const DAY_MS: i64 = 86_400_000;

pub struct Fire {
    pub span_id: String,
    pub ts: String,
    pub session: String,
    /// The reply tail the span kept; absent on fires from before it kept one.
    pub excerpt: Option<String>,
}

pub struct RuleRow {
    pub rule: String,
    pub kind: String,
    pub fires: usize,
    pub sessions: usize,
    pub last_7d: usize,
    pub labeled: usize,
    pub false_fires: usize,
    /// Unlabeled fires first, newest first, so each labeling pass moves on.
    pub sample: Vec<Fire>,
}

/// One `{"span_id": "s-...", "verdict": "true"|"false"}` per line. A true
/// verdict means the fire was right. A later line for the same span wins.
pub fn parse_labels(text: &str) -> Result<HashMap<String, bool>, String> {
    let mut out = HashMap::new();
    for (n, line) in text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        let v: Value = serde_json::from_str(line).unwrap_or(Value::Null);
        let verdict = match v.get("verdict").and_then(Value::as_str) {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        match (v.get("span_id").and_then(Value::as_str), verdict) {
            (Some(span), Some(verdict)) => out.insert(span.to_string(), verdict),
            _ => {
                return Err(format!(
                    "labels line {}: needs span_id and a verdict of \"true\" or \"false\"",
                    n + 1
                ))
            }
        };
    }
    Ok(out)
}

/// Fold `decision_span` rows (ts_ms, line) into one row per rule and span
/// kind, most fires first. A `sample` of `None` keeps every fire.
pub fn fold(
    rows: &[(i64, String)],
    labels: &HashMap<String, bool>,
    now_ms: i64,
    sample: Option<usize>,
) -> Vec<RuleRow> {
    let mut by_rule: BTreeMap<(String, String), Vec<(i64, Fire)>> = BTreeMap::new();
    for (ts_ms, line) in rows {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = &v["data"];
        let Some(rule) = data["rule"].as_str() else {
            continue;
        };
        let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
        let fire = Fire {
            span_id: text(&data["trace"]["span_id"]),
            ts: text(&v["ts"]),
            session: text(&data["trace"]["actor_session"]),
            excerpt: data["excerpt"].as_str().map(str::to_string),
        };
        let key = (rule.to_string(), text(&data["span_kind"]));
        by_rule.entry(key).or_default().push((*ts_ms, fire));
    }
    let mut out: Vec<RuleRow> = by_rule
        .into_iter()
        .map(|((rule, kind), mut fires)| {
            let verdicts: Vec<bool> = fires
                .iter()
                .filter_map(|(_, f)| labels.get(&f.span_id).copied())
                .collect();
            let sessions: BTreeSet<&str> = fires.iter().map(|(_, f)| f.session.as_str()).collect();
            let sessions = sessions.len();
            let last_7d = fires
                .iter()
                .filter(|(ts, _)| now_ms - ts <= 7 * DAY_MS)
                .count();
            fires.sort_by_key(|(ts, f)| (labels.contains_key(&f.span_id), -ts));
            let total = fires.len();
            RuleRow {
                rule,
                kind,
                fires: total,
                sessions,
                last_7d,
                labeled: verdicts.len(),
                false_fires: verdicts.iter().filter(|v| !**v).count(),
                sample: fires
                    .into_iter()
                    .take(sample.unwrap_or(total))
                    .map(|(_, f)| f)
                    .collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.fires.cmp(&a.fires).then_with(|| a.rule.cmp(&b.rule)));
    out
}

/// `fno-agents intel --lens rules [--days N] [--sample N|all] [--labels <file>]`.
pub(crate) fn run_lens(
    days: u64,
    sample: &crate::intel_insights::SampleRequest,
    labels_path: Option<&str>,
) -> i32 {
    let labels = match labels_path.map(|p| {
        std::fs::read_to_string(p)
            .map_err(|e| format!("{p}: {e}"))
            .and_then(|t| parse_labels(&t))
    }) {
        None => HashMap::new(),
        Some(Ok(l)) => l,
        Some(Err(e)) => {
            eprintln!("fno-agents intel --lens rules: {e}");
            return 2;
        }
    };
    let now_ms = crate::claims::now_ms();
    // Fire spans land in the project journal (emit_span's target).
    let journal = crate::law_match::project_events_journal();
    let query = crate::event_store::EventQuery {
        since_ms: (days > 0).then(|| now_ms - days as i64 * DAY_MS),
        ..crate::event_store::EventQuery::of_types(&["decision_span"])
    };
    let rows = match crate::event_store::query_events(&journal, &query) {
        Ok(rows) => rows
            .into_iter()
            .map(|r| (r.ts_ms, r.line))
            .collect::<Vec<_>>(),
        Err(e) => {
            eprintln!("fno-agents intel --lens rules: {}: {e}", journal.display());
            return 1;
        }
    };
    let sample = match sample {
        crate::intel_insights::SampleRequest::None => Some(5),
        crate::intel_insights::SampleRequest::N(n) => Some(*n),
        crate::intel_insights::SampleRequest::All => None,
    };
    let report = fold(&rows, &labels, now_ms, sample);
    let total: usize = report.iter().map(|r| r.fires).sum();
    println!(
        "rule lens (days {days}): {} rule(s), {total} fire(s)",
        report.len()
    );
    println!(
        "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6}",
        "rule", "kind", "fires", "sessions", "last7d", "labeled", "false"
    );
    for r in &report {
        let rate = match r.labeled {
            0 => String::new(),
            n => format!(" ({}%)", r.false_fires * 100 / n),
        };
        println!(
            "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6}{rate}",
            r.rule, r.kind, r.fires, r.sessions, r.last_7d, r.labeled, r.false_fires
        );
    }
    for r in report.iter().filter(|r| !r.sample.is_empty()) {
        println!("sample {} ({}):", r.rule, r.kind);
        for f in &r.sample {
            let excerpt = f.excerpt.as_deref().map(|e| e.replace('\n', " / "));
            let excerpt = excerpt
                .as_deref()
                .unwrap_or("(no excerpt: fired before spans kept one)");
            println!("  {} {} {}: {excerpt}", f.span_id, f.ts, f.session);
        }
    }
    println!("label: append {{\"span_id\":\"s-...\",\"verdict\":\"true\"|\"false\"}} lines to a file, then pass --labels <file>");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(
        days_ago: i64,
        rule: &str,
        id: &str,
        session: &str,
        excerpt: Option<&str>,
    ) -> (i64, String) {
        let data = serde_json::json!({"span_kind": "block", "rule": rule, "excerpt": excerpt,
            "trace": {"span_id": id, "actor_session": session}});
        (
            30 * DAY_MS - days_ago * DAY_MS,
            serde_json::json!({"type": "decision_span", "data": data}).to_string(),
        )
    }

    #[test]
    fn rule_lens_counts_fires_and_reads_labels_as_a_false_rate() {
        let rows = vec![
            span(1, "chat_ask_unfiled", "s-1", "aaaa0001", Some("Which one?")),
            span(2, "chat_ask_unfiled", "s-2", "aaaa0001", None),
            span(20, "chat_ask_unfiled", "s-3", "bbbb0002", None),
            span(1, "decided_ask_fyi", "s-4", "bbbb0002", None),
            // A span with no rule is a route or ask span, not a fire.
            (
                0,
                r#"{"type":"decision_span","data":{"span_kind":"route"}}"#.to_string(),
            ),
        ];
        let labels = parse_labels(
            "{\"span_id\":\"s-1\",\"verdict\":\"true\"}\n\n{\"span_id\":\"s-3\",\"verdict\":\"false\"}\n",
        )
        .unwrap();
        let report = fold(&rows, &labels, 30 * DAY_MS, Some(2));
        assert_eq!(report.len(), 2);
        let chat = &report[0];
        assert_eq!(chat.rule, "chat_ask_unfiled");
        assert_eq!((chat.fires, chat.sessions, chat.last_7d), (3, 2, 2));
        assert_eq!((chat.labeled, chat.false_fires), (2, 1));
        // The unlabeled fire leads the sample so the next pass labels it.
        let ids: Vec<&str> = chat.sample.iter().map(|f| f.span_id.as_str()).collect();
        assert_eq!(ids, vec!["s-2", "s-1"]);
        assert_eq!(chat.sample[1].excerpt.as_deref(), Some("Which one?"));
        assert_eq!(fold(&rows, &labels, 30 * DAY_MS, None)[0].sample.len(), 3);
        assert!(parse_labels("{\"span_id\":\"s-1\",\"verdict\":\"maybe\"}").is_err());
    }
}
