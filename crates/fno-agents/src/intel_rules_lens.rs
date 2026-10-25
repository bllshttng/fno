//! `fno-agents intel --lens rules`: how often each event rule fires, and how
//! often it fires wrongly.
//!
//! Every rule in `event_rules.toml` is a guess about a failure. Each fire
//! already lands as a `decision_span` row carrying `rule`, so the counts are a
//! journal read. Whether a fire was right needs a judge: the lens samples
//! fires with the reply excerpt the span recorded, a person marks each one in
//! a labels file, and the lens reads the file back as a false-fire rate.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use serde_json::Value;

const DAY_MS: i64 = 86_400_000;
const DEFAULT_SAMPLE: usize = 5;
const SHOW_CHARS: usize = 240;

#[derive(Debug, Clone, Serialize)]
pub struct Fire {
    pub span_id: String,
    pub ts: String,
    pub session: String,
    /// The reply tail the span recorded; absent on fires written before the
    /// engine kept one.
    pub excerpt: Option<String>,
}

#[derive(Debug, Serialize)]
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

/// The labels file: one JSON object per line, `{"span_id": "s-...",
/// "verdict": "true"|"false"}`. A true verdict means the fire was right. A
/// later line for the same span wins.
pub fn parse_labels(text: &str) -> Result<HashMap<String, bool>, String> {
    let mut out = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).map_err(|e| format!("labels line {}: {e}", n + 1))?;
        let span = v.get("span_id").and_then(Value::as_str);
        let verdict = match v.get("verdict") {
            Some(Value::Bool(b)) => Some(*b),
            Some(Value::String(s)) if s == "true" => Some(true),
            Some(Value::String(s)) if s == "false" => Some(false),
            _ => None,
        };
        match (span, verdict) {
            (Some(span), Some(verdict)) => {
                out.insert(span.to_string(), verdict);
            }
            _ => {
                return Err(format!(
                    "labels line {}: needs span_id and a verdict of true or false",
                    n + 1
                ))
            }
        }
    }
    Ok(out)
}

/// Fold `decision_span` journal rows (ts_ms, line) into one row per rule and
/// span kind, most fires first. `sample` of `None` keeps every fire.
pub fn fold(
    rows: &[(i64, String)],
    labels: &HashMap<String, bool>,
    now_ms: i64,
    sample: Option<usize>,
) -> Vec<RuleRow> {
    struct Acc {
        fires: Vec<(i64, Fire)>,
        sessions: BTreeSet<String>,
        last_7d: usize,
    }
    let mut by_rule: BTreeMap<(String, String), Acc> = BTreeMap::new();
    for (ts_ms, line) in rows {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(data) = v.get("data") else {
            continue;
        };
        let Some(rule) = data.get("rule").and_then(Value::as_str) else {
            continue;
        };
        let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or_default().to_string();
        let kind = text(data.get("span_kind"));
        let trace = data.get("trace");
        let fire = Fire {
            span_id: text(trace.and_then(|t| t.get("span_id"))),
            ts: text(v.get("ts")),
            session: text(trace.and_then(|t| t.get("actor_session"))),
            excerpt: data
                .get("excerpt")
                .and_then(Value::as_str)
                .map(str::to_string),
        };
        let acc = by_rule.entry((rule.to_string(), kind)).or_insert(Acc {
            fires: Vec::new(),
            sessions: BTreeSet::new(),
            last_7d: 0,
        });
        if now_ms - ts_ms <= 7 * DAY_MS {
            acc.last_7d += 1;
        }
        acc.sessions.insert(fire.session.clone());
        acc.fires.push((*ts_ms, fire));
    }
    let mut out: Vec<RuleRow> = by_rule
        .into_iter()
        .map(|((rule, kind), mut acc)| {
            let labeled: Vec<bool> = acc
                .fires
                .iter()
                .filter_map(|(_, f)| labels.get(&f.span_id).copied())
                .collect();
            acc.fires
                .sort_by_key(|(ts, f)| (labels.contains_key(&f.span_id), -ts));
            let fires = acc.fires.len();
            RuleRow {
                rule,
                kind,
                fires,
                sessions: acc.sessions.len(),
                last_7d: acc.last_7d,
                labeled: labeled.len(),
                false_fires: labeled.iter().filter(|v| !**v).count(),
                sample: acc
                    .fires
                    .into_iter()
                    .take(sample.unwrap_or(fires))
                    .map(|(_, f)| f)
                    .collect(),
            }
        })
        .collect();
    out.sort_by(|a, b| b.fires.cmp(&a.fires).then_with(|| a.rule.cmp(&b.rule)));
    out
}

/// Run the rule lens: `fno-agents intel --lens rules [--days N] [--sample N|all]
/// [--labels <file>] [--json]`.
pub(crate) fn run_lens(
    days: u64,
    json: bool,
    sample: &crate::intel_insights::SampleRequest,
    labels_path: Option<&str>,
) -> i32 {
    let labels = match labels_path {
        None => HashMap::new(),
        Some(path) => match std::fs::read_to_string(path)
            .map_err(|e| format!("{path}: {e}"))
            .and_then(|t| parse_labels(&t))
        {
            Ok(l) => l,
            Err(e) => {
                eprintln!("fno-agents intel --lens rules: {e}");
                return 2;
            }
        },
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // Fire spans land in the project journal (emit_span's target).
    let journal = crate::law_match::project_events_journal();
    let query = crate::event_store::EventQuery {
        since_ms: (days > 0).then(|| now_ms - days as i64 * DAY_MS),
        ..crate::event_store::EventQuery::of_types(&["decision_span"])
    };
    let rows = match crate::event_store::query_events(&journal, &query) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno-agents intel --lens rules: {}: {e}", journal.display());
            return 1;
        }
    };
    let rows: Vec<(i64, String)> = rows.into_iter().map(|r| (r.ts_ms, r.line)).collect();
    let sample = match sample {
        crate::intel_insights::SampleRequest::None => Some(DEFAULT_SAMPLE),
        crate::intel_insights::SampleRequest::N(n) => Some(*n),
        crate::intel_insights::SampleRequest::All => None,
    };
    let report = fold(&rows, &labels, now_ms, sample);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"days": days, "rules": report}))
                .unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print_report(days, &report);
    }
    0
}

fn print_report(days: u64, report: &[RuleRow]) {
    let total: usize = report.iter().map(|r| r.fires).sum();
    println!(
        "rule lens (days {days}): {} rule(s), {total} fire(s)",
        report.len()
    );
    println!(
        "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6}",
        "rule", "kind", "fires", "sessions", "last7d", "labeled", "false"
    );
    for r in report {
        let rate = if r.labeled == 0 {
            String::new()
        } else {
            format!(" ({}%)", r.false_fires * 100 / r.labeled)
        };
        println!(
            "  {:<26} {:<7} {:>6} {:>8} {:>6} {:>7} {:>6}{rate}",
            r.rule, r.kind, r.fires, r.sessions, r.last_7d, r.labeled, r.false_fires
        );
    }
    for r in report.iter().filter(|r| !r.sample.is_empty()) {
        println!("sample {} ({}):", r.rule, r.kind);
        for f in &r.sample {
            let excerpt = match &f.excerpt {
                Some(e) => {
                    let tail: Vec<char> = e.chars().collect();
                    let start = tail.len().saturating_sub(SHOW_CHARS);
                    tail[start..]
                        .iter()
                        .collect::<String>()
                        .replace('\n', " / ")
                }
                None => "(no excerpt: fired before spans kept one)".to_string(),
            };
            println!("  {} {} {}: {excerpt}", f.span_id, f.ts, f.session);
        }
    }
    println!(
        "label: append {{\"span_id\":\"s-...\",\"verdict\":\"true\"|\"false\"}} lines to a file, \
         then pass --labels <file>"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(
        ts_ms: i64,
        rule: &str,
        span_id: &str,
        session: &str,
        excerpt: Option<&str>,
    ) -> (i64, String) {
        let mut data = serde_json::json!({
            "span_kind": "block",
            "trace": {"trace_id": "none", "span_id": span_id, "actor_session": session},
            "rule": rule,
            "matched_event": format!("stop:{session}:t{ts_ms}"),
        });
        if let Some(e) = excerpt {
            data["excerpt"] = serde_json::json!(e);
        }
        (
            ts_ms,
            serde_json::json!({"ts": "2026-10-25T00:00:00Z", "type": "decision_span", "data": data})
                .to_string(),
        )
    }

    #[test]
    fn rule_lens_counts_fires_and_reads_labels_as_a_false_rate() {
        let now = 30 * DAY_MS;
        let rows = vec![
            span(
                now - DAY_MS,
                "chat_ask_unfiled",
                "s-1",
                "aaaa0001",
                Some("Which one?"),
            ),
            span(
                now - 2 * DAY_MS,
                "chat_ask_unfiled",
                "s-2",
                "aaaa0001",
                None,
            ),
            span(
                now - 20 * DAY_MS,
                "chat_ask_unfiled",
                "s-3",
                "bbbb0002",
                None,
            ),
            span(now - DAY_MS, "decided_ask_fyi", "s-4", "bbbb0002", None),
            // A span with no rule is a route or ask span, not a fire.
            (
                now,
                r#"{"ts":"x","type":"decision_span","data":{"span_kind":"route"}}"#.to_string(),
            ),
        ];
        let labels = parse_labels(
            "{\"span_id\":\"s-1\",\"verdict\":\"true\"}\n\n{\"span_id\":\"s-3\",\"verdict\":false}\n",
        )
        .unwrap();
        let report = fold(&rows, &labels, now, Some(2));
        assert_eq!(report.len(), 2);
        let chat = &report[0];
        assert_eq!(chat.rule, "chat_ask_unfiled");
        assert_eq!((chat.fires, chat.sessions, chat.last_7d), (3, 2, 2));
        assert_eq!((chat.labeled, chat.false_fires), (2, 1));
        // The unlabeled fire leads the sample so the next pass labels it.
        let ids: Vec<&str> = chat.sample.iter().map(|f| f.span_id.as_str()).collect();
        assert_eq!(ids, vec!["s-2", "s-1"]);
        assert_eq!(chat.sample[1].excerpt.as_deref(), Some("Which one?"));
        assert_eq!(fold(&rows, &labels, now, None)[0].sample.len(), 3);
        assert!(parse_labels("{\"span_id\":\"s-1\",\"verdict\":\"maybe\"}").is_err());
    }
}
