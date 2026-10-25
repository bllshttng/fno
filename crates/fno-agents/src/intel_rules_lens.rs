//! The intel rules lens: every event-rule fire in the window, per rule. A
//! fire counts as `filed` when the same session filed an
//! `operator_question` within ten minutes of it. That is a proxy for a true
//! ask, never a label. Where fnod scored the fire (`fnod_is_ask` on the
//! span), the lens splits the fires by fnod's answer against the same proxy,
//! so the keyword verdict and fnod's can be compared on records alone.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

const FILED_WINDOW_MS: i64 = 10 * 60 * 1000;
const FNOD_YES: f64 = 0.5;

#[derive(Debug, Default, PartialEq, Serialize)]
pub(crate) struct RuleTally {
    pub rule: String,
    pub fires: usize,
    pub filed: usize,
    /// Fires with no fnod score: the server was off, absent or slow.
    pub unscored: usize,
    pub fnod_yes: usize,
    pub fnod_yes_filed: usize,
    pub fnod_no: usize,
    pub fnod_no_filed: usize,
}

fn actor(v: &Value) -> Option<String> {
    let data = v.get("data")?;
    data.get("asker")
        .or_else(|| data.pointer("/trace/actor_session"))
        .and_then(Value::as_str)
        .map(crate::identity::canonical_handle)
}

/// Fold `(ts_ms, type, row)` journal rows into one tally per rule.
pub(crate) fn tally(rows: &[(i64, &str, Value)]) -> Vec<RuleTally> {
    let asks: Vec<(i64, String)> = rows
        .iter()
        .filter(|(_, t, _)| *t == "operator_question")
        .filter_map(|(ts, _, v)| Some((*ts, actor(v)?)))
        .collect();
    let mut out: BTreeMap<String, RuleTally> = BTreeMap::new();
    for (ts, kind, v) in rows {
        let Some(data) = v.get("data") else { continue };
        let rule = data.get("rule").and_then(Value::as_str);
        if *kind != "decision_span" || rule.is_none() {
            continue;
        }
        let rule = rule.unwrap_or_default();
        let who = actor(v);
        let filed = asks
            .iter()
            .any(|(at, a)| Some(a) == who.as_ref() && (*ts..=ts + FILED_WINDOW_MS).contains(at));
        let t = out.entry(rule.to_string()).or_insert_with(|| RuleTally {
            rule: rule.to_string(),
            ..RuleTally::default()
        });
        t.fires += 1;
        t.filed += usize::from(filed);
        match data.get("fnod_is_ask").and_then(Value::as_f64) {
            None => t.unscored += 1,
            Some(p) if p >= FNOD_YES => {
                t.fnod_yes += 1;
                t.fnod_yes_filed += usize::from(filed);
            }
            Some(_) => {
                t.fnod_no += 1;
                t.fnod_no_filed += usize::from(filed);
            }
        }
    }
    out.into_values().collect()
}

/// `fno-agents intel --lens rules [--days N] [--json]`.
pub fn run(days: u64, json: bool) -> i32 {
    let journal = crate::law_match::project_events_journal();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let query = crate::event_store::EventQuery {
        since_ms: (days > 0).then(|| now_ms - days as i64 * 86_400_000),
        ..crate::event_store::EventQuery::of_types(&["decision_span", "operator_question"])
    };
    let rows = match crate::event_store::query_events(&journal, &query) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("fno-agents intel --lens rules: {e}");
            return 1;
        }
    };
    let parsed: Vec<(i64, &str, Value)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                r.ts_ms,
                r.r#type.as_str(),
                serde_json::from_str(&r.line).ok()?,
            ))
        })
        .collect();
    let tallies = tally(&parsed);
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&tallies).unwrap_or_else(|_| "[]".to_string())
        );
        return 0;
    }
    println!("rules lens (days {days}): {} rule(s) fired", tallies.len());
    for t in &tallies {
        println!(
            "  {}: {} fires, {} filed; fnod yes {} ({} filed), no {} ({} filed), unscored {}",
            t.rule,
            t.fires,
            t.filed,
            t.fnod_yes,
            t.fnod_yes_filed,
            t.fnod_no,
            t.fnod_no_filed,
            t.unscored
        );
    }
    println!(
        "limit: filed = the session filed a question within 10 minutes of the fire; a proxy for a true ask, not a label. fnod yes = is_ask >= {FNOD_YES}."
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rules_lens_splits_fnod_answers_against_the_filed_proxy() {
        let span = |p: Option<f64>| {
            let mut data = json!({"span_kind": "block", "rule": "chat_ask_unfiled",
                "trace": {"actor_session": "aaaa1111"}});
            if let Some(p) = p {
                data["fnod_is_ask"] = json!(p);
            }
            json!({"type": "decision_span", "data": data})
        };
        let ask = json!({"type": "operator_question", "data": {"asker": "aaaa1111"}});
        let rows = vec![
            (0, "decision_span", span(Some(0.9))),
            (60_000, "operator_question", ask),
            (20 * 60_000, "decision_span", span(Some(0.1))),
            (40 * 60_000, "decision_span", span(None)),
            (
                40 * 60_000,
                "decision_span",
                json!({"data": {"span_kind": "ask"}}),
            ),
        ];
        assert_eq!(
            tally(&rows),
            vec![RuleTally {
                rule: "chat_ask_unfiled".to_string(),
                fires: 3,
                filed: 1,
                unscored: 1,
                fnod_yes: 1,
                fnod_yes_filed: 1,
                fnod_no: 1,
                fnod_no_filed: 0,
            }]
        );
    }
}
