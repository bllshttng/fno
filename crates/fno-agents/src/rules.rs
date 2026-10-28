//! The rule schema's store-side answers: `check_answer` (the `rules-check`
//! mode) and `measure_answer` (the `rules-measure` mode) read the live law
//! rows, so they stay in fno-agents. The schema itself, the tier-0 match and
//! BM25 moved to the `fno` crate (crates/fno/src/rules.rs) so the fnh
//! harness can link them in process; this module re-exports that surface for
//! the callers that still reach it at `crate::rules`.

pub use fno::rules::{
    bm25_rank, matches, parse_file, parse_table, Authority, Disposition, Event, Rule, RuleFile,
    When,
};

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

fn law_id(row: &Value) -> Option<&str> {
    row.get("decision_id").and_then(Value::as_str)
}

fn count(map: &mut BTreeMap<String, usize>, key: &str) {
    *map.entry(key.to_owned()).or_insert(0) += 1;
}

/// The `rules-check` answer: the file's rules validated and counted, and
/// every live law either carried by a rule (`law = "<id>"`) or disposed of.
/// `ok` holds only when nothing is invalid and no live law is missing.
pub fn check_answer(text: &str, laws: &[Value]) -> Value {
    let file = match parse_file(text) {
        Ok(f) => f,
        Err(e) => return json!({"ok": false, "error": e}),
    };
    let live: BTreeSet<&str> = laws.iter().filter_map(law_id).collect();
    let mut invalid = Vec::new();
    let mut seen = BTreeSet::new();
    let mut covered = BTreeSet::new();
    let (mut by_tier, mut by_action, mut by_family, mut tier0_by_family, mut by_kind) = (
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
        BTreeMap::new(),
    );
    for rule in &file.rule {
        for p in rule.problems() {
            invalid.push(json!({"id": rule.id, "reason": p}));
        }
        if !seen.insert(rule.id.clone()) {
            invalid.push(json!({"id": rule.id, "reason": format!("{}: duplicate id", rule.id)}));
        }
        if let Some(law) = &rule.law {
            if live.contains(law.as_str()) {
                covered.insert(law.clone());
            } else {
                invalid.push(
                    json!({"id": rule.id, "reason": format!("{}: law {law} is not live", rule.id)}),
                );
            }
        }
        count(&mut by_tier, &format!("tier{}", rule.tier()));
        count(&mut by_action, &rule.action);
        let family = rule.family.as_deref().unwrap_or("other");
        count(&mut by_family, family);
        if rule.tier() == 0 {
            count(&mut tier0_by_family, family);
        }
    }
    let ruled: BTreeSet<&str> = file.rule.iter().filter_map(|r| r.law.as_deref()).collect();
    for d in &file.disposition {
        if !fno::rules::DISPOSITION_KINDS.contains(&d.kind.as_str()) {
            invalid.push(json!({"id": d.law, "reason": format!("{}: unknown disposition {:?}", d.law, d.kind)}));
        }
        if ruled.contains(d.law.as_str()) {
            invalid.push(json!({"id": d.law, "reason": format!("{}: a law carries a rule or a disposition, not both", d.law)}));
        } else if live.contains(d.law.as_str()) {
            covered.insert(d.law.clone());
        } else {
            invalid.push(
                json!({"id": d.law, "reason": format!("{}: disposed law is not live", d.law)}),
            );
        }
        count(&mut by_kind, &d.kind);
    }
    let missing: Vec<&str> = live
        .iter()
        .copied()
        .filter(|id| !covered.contains(*id))
        .collect();
    json!({
        "ok": invalid.is_empty() && missing.is_empty(),
        "rules": file.rule.len(),
        "by_tier": by_tier,
        "by_action": by_action,
        "by_family": by_family,
        "tier0_by_family": tier0_by_family,
        "dispositions": by_kind,
        "laws_live": live.len(),
        "laws_annotated": covered.len(),
        "laws_missing": missing,
        "invalid": invalid,
    })
}

#[derive(Deserialize)]
struct Case {
    name: String,
    stage: String,
    query: String,
    expected: Vec<String>,
}

#[derive(Deserialize)]
struct Cases {
    cases: Vec<Case>,
}

/// The `rules-measure` answer: for each labeled case, BM25 over the live
/// laws' `{subject} {decision}`, scored as recall and precision.
pub fn measure_answer(cases: &str, laws: &[Value]) -> Value {
    let cases: Cases = match serde_json::from_str(cases) {
        Ok(c) => c,
        Err(e) => return json!({"ok": false, "error": format!("cases: {e}")}),
    };
    let docs: Vec<(String, String)> = laws
        .iter()
        .filter_map(|row| {
            let field = |k: &str| row.get(k).and_then(Value::as_str).unwrap_or("");
            Some((
                law_id(row)?.to_owned(),
                format!("{} {}", field("subject"), field("decision")),
            ))
        })
        .collect();
    let ratio = |hit: usize, of: usize| if of == 0 { 0.0 } else { hit as f64 / of as f64 };
    let mut rows = Vec::new();
    let mut sums = [0.0f64; 3];
    for case in &cases.cases {
        let expected: BTreeSet<&str> = case.expected.iter().map(String::as_str).collect();
        let ranked = bm25_rank(&docs, &case.query, 10);
        let hits = |n: usize| {
            ranked
                .iter()
                .take(n)
                .filter(|(id, _)| expected.contains(id.as_str()))
                .count()
        };
        let row = [
            ratio(hits(5), expected.len()),
            ratio(hits(10), expected.len()),
            ratio(hits(10), 10),
        ];
        for (sum, v) in sums.iter_mut().zip(row) {
            *sum += v;
        }
        rows.push(json!({
            "name": case.name,
            "stage": case.stage,
            "expected": expected.len(),
            "bm25_top10": ranked.iter().map(|(id, _)| id).collect::<Vec<_>>(),
            "bm25_recall_at_5": row[0],
            "bm25_recall_at_10": row[1],
            "bm25_precision_at_10": row[2],
        }));
    }
    let n = cases.cases.len().max(1) as f64;
    json!({
        "ok": true,
        "cases": cases.cases.len(),
        "laws": docs.len(),
        "mean": {
            "bm25_recall_at_5": sums[0] / n,
            "bm25_recall_at_10": sums[1] / n,
            "bm25_precision_at_10": sums[2] / n,
        },
        "per_case": rows,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rules_check_names_every_missing_law_and_dispositions_cover() {
        let laws = vec![
            json!({"decision_id": "d-11111111", "subject": "new-code-language"}),
            json!({"decision_id": "d-22222222", "subject": "merge-grant:x"}),
            json!({"decision_id": "d-33333333", "subject": "mail-raw"}),
        ];
        let text = "[[rule]]\nid = \"rust-only\"\nlaw = \"d-11111111\"\naction = \"block\"\nfamily = \"naming\"\n[rule.when]\npaths = [\"cli/src/fno/*.py\"]\n\n[[disposition]]\nlaw = \"d-22222222\"\nkind = \"grant\"\nreason = \"a head-pinned grant\"\n";
        let answer = check_answer(text, &laws);
        assert_eq!(answer["ok"], false);
        assert_eq!(answer["laws_missing"], json!(["d-33333333"]));
        assert_eq!(answer["laws_annotated"], 2);
        assert_eq!(answer["tier0_by_family"]["naming"], 1);
        let both = format!(
            "{text}\n[[disposition]]\nlaw = \"d-11111111\"\nkind = \"grant\"\nreason = \"x\"\n"
        );
        assert!(check_answer(&both, &laws)["invalid"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not both"));
        let with_gone = format!(
            "{text}\n[[disposition]]\nlaw = \"d-99999999\"\nkind = \"retire\"\nreason = \"x\"\n"
        );
        let answer = check_answer(&with_gone, &laws);
        assert!(answer["invalid"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not live"));
    }

    #[test]
    fn rules_measure_scores_bm25() {
        let laws = vec![
            json!({"decision_id": "d-1", "subject": "merge-approval", "decision": "merge the pull request when review passes"}),
            json!({"decision_id": "d-2", "subject": "merge-freeze", "decision": "a freeze holds every release merge"}),
            json!({"decision_id": "d-3", "subject": "review-lane", "decision": "review inside the session"}),
        ];
        let cases = r#"{"cases": [{"name": "freeze", "stage": "review", "query": "may I merge during the freeze", "expected": ["d-2"]}]}"#;
        let answer = measure_answer(cases, &laws);
        assert_eq!(answer["cases"], 1);
        assert_eq!(answer["per_case"][0]["bm25_top10"][0], "d-2");
        assert_eq!(answer["mean"]["bm25_recall_at_5"], 1.0);
        assert_eq!(answer["mean"]["bm25_precision_at_10"], 0.1);
        assert_eq!(measure_answer("not json", &laws)["ok"], false);
    }
}
