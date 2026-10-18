//! The intel scorecard: per-model and per-lead counts over the fold's
//! session rows. This module only aggregates what the fold already read -
//! transcripts, the witnessed operator turns' text, the agent registry's
//! session-to-name join, and the graph's node rows. The counts feed the
//! skill's judgment; no model judgment runs here.

use crate::intel::SessionRow;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// One witnessed operator turn's text, truncated at capture: the by-lead
/// lens reads these, the report never quotes them whole.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OpText {
    pub(crate) ts: String,
    pub(crate) text: String,
}

/// The per-session operator-turn texts the scorecard pass collected, keyed
/// by session id. Present only under `--scorecard`.
pub(crate) type OpTexts = HashMap<String, Vec<OpText>>;

const TEXT_CAP_CHARS: usize = 400;
pub(crate) const TEXT_CAP_TURNS: usize = 40;

pub(crate) fn capture_text(ts: Option<&str>, text: &str) -> OpText {
    OpText {
        ts: ts.unwrap_or_default().to_string(),
        text: text.chars().take(TEXT_CAP_CHARS).collect(),
    }
}

/// One capped operator-turn text into a session's list, so a session that
/// argued all day cannot bloat the fold.
pub(crate) fn push_text(map: &mut OpTexts, session: &str, item: OpText) {
    let v = map.entry(session.to_string()).or_default();
    if v.len() < TEXT_CAP_TURNS {
        v.push(item);
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ModelRow {
    pub(crate) model: String,
    pub(crate) sessions: usize,
    /// Sessions bucketed by kind, so readers compare like with like.
    pub(crate) by_kind: BTreeMap<String, usize>,
    pub(crate) tool_calls: u64,
    pub(crate) tool_errors: BTreeMap<String, u64>,
    /// tool errors over tool calls, finite in 0..=1.
    pub(crate) tool_error_rate: f64,
    /// Distinct PRs the model's sessions touched (ledger join).
    pub(crate) prs_linked: usize,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct ReturnedComplaint {
    pub(crate) node: String,
    pub(crate) first_mention: String,
    pub(crate) last_mention: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct LeadRow {
    pub(crate) lead: String,
    pub(crate) sessions: usize,
    pub(crate) operator_turns: usize,
    /// Normalized asks asked two or more times across the lead's sessions.
    pub(crate) restated_asks: Vec<String>,
    /// Nodes the operator named before their closure and named again after.
    pub(crate) returned_complaints: Vec<ReturnedComplaint>,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Coverage {
    pub(crate) sessions_attributed: usize,
    pub(crate) sessions_unknown: usize,
    /// Operator-turn texts are a best-effort subset: witness-bound turns on
    /// the rollup replay carry timestamps only, so lead rows can undercount
    /// restated asks, never inflate them.
    pub(crate) texts_best_effort: bool,
}

#[derive(Debug, Serialize, PartialEq)]
pub(crate) struct Scorecard {
    pub(crate) window_days: u64,
    pub(crate) coverage: Coverage,
    pub(crate) by_model: Vec<ModelRow>,
    pub(crate) by_lead: Vec<LeadRow>,
    /// Measures no local store can answer today; the report names them so
    /// a reader never reads their absence as zero.
    pub(crate) unmeasured: Vec<&'static str>,
}

/// Backlog node ids the text names and the graph knows: lowercase prefix,
/// a dash, four to eight hex characters.
fn mention_node_ids<'a>(text: &str, known: &'a HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    for token in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '-') {
        let t = token.trim_matches('-');
        if is_node_id(t) && known.contains(t.to_lowercase().as_str()) {
            out.push(t.to_lowercase());
        }
    }
    out
}

fn is_node_id(t: &str) -> bool {
    let Some((prefix, hex)) = t.split_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_lowercase())
        && (4..=8).contains(&hex.len())
        && hex.chars().all(|c| c.is_ascii_hexdigit())
}

/// A lead row per registry name. Sessions the registry does not name are
/// skipped from lead rows (they stay in by_model and coverage); a lead
/// whose texts were capped still counts its witnessed turns from the rows.
pub(crate) fn build(
    rows: &[SessionRow],
    op_texts: &OpTexts,
    session_lead: &HashMap<String, String>,
    graph: &[Value],
) -> Scorecard {
    let mut by_model: BTreeMap<String, ModelRow> = BTreeMap::new();
    let mut unknown_sessions = 0usize;
    let mut attributed = 0usize;
    for row in rows {
        let model = match &row.model {
            Some(m) if !m.is_empty() => m.clone(),
            _ => {
                unknown_sessions += 1;
                continue;
            }
        };
        attributed += 1;
        let entry = by_model.entry(model).or_insert_with(|| ModelRow {
            model: String::new(),
            sessions: 0,
            by_kind: BTreeMap::new(),
            tool_calls: 0,
            tool_errors: BTreeMap::new(),
            tool_error_rate: 0.0,
            prs_linked: 0,
        });
        entry.model = model.clone();
        entry.sessions += 1;
        *entry.by_kind.entry(row.kind.clone()).or_insert(0) += 1;
        entry.tool_calls += row.tool_use as u64;
        for (class, n) in row.tool_errors.iter() {
            *entry.tool_errors.entry(class.clone()).or_insert(0) += n;
        }
    }
    // Distinct PRs per model need a per-model seen set, not a scan.
    let mut prs_seen: BTreeMap<String, HashSet<u64>> = BTreeMap::new();
    for row in rows {
        let Some(model) = row.model.as_ref().filter(|m| !m.is_empty()) else {
            continue;
        };
        if let Some(pr) = row.pr_number {
            prs_seen.entry(model.clone()).or_default().insert(pr);
        }
    }
    for (model, entry) in by_model.iter_mut() {
        let errors: u64 = entry.tool_errors.values().sum();
        entry.tool_error_rate = if entry.tool_calls > 0 {
            (errors as f64) / (entry.tool_calls as f64)
        } else {
            0.0
        };
        entry.prs_linked = prs_seen.get(model).map(|s| s.len()).unwrap_or(0);
    }

    // By lead: group sessions under registry names, then read the texts.
    let mut lead_sessions: BTreeMap<String, Vec<&SessionRow>> = BTreeMap::new();
    for row in rows {
        if let Some(lead) = session_lead.get(&row.session) {
            lead_sessions.entry(lead.clone()).or_default().push(row);
        }
    }
    // The graph's done dates, keyed by lowercase node id.
    let mut done_at: HashMap<String, String> = HashMap::new();
    for node in graph {
        let Some(id) = node.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Some(d) = node.get("completed_at").and_then(|v| v.as_str()) {
            done_at.insert(id.to_lowercase(), d.to_string());
        }
    }
    let mut by_lead = Vec::new();
    let known: HashSet<String> = done_at.keys().cloned().collect();
    for (lead, sessions) in lead_sessions {
        let mut texts: Vec<&OpText> = Vec::new();
        for s in &sessions {
            if let Some(ts) = op_texts.get(&s.session) {
                texts.extend(ts.iter());
            }
        }
        texts.sort_by(|a, b| a.ts.cmp(&b.ts));
        let mut ask_counts: HashMap<String, u64> = HashMap::new();
        for t in &texts {
            for ask in crate::repeated_asks::asks_in(&t.text) {
                *ask_counts.entry(ask).or_insert(0) += 1;
            }
        }
        let mut restated: Vec<String> = ask_counts
            .into_iter()
            .filter(|(_, n)| *n >= 2)
            .map(|(a, _)| a)
            .collect();
        restated.sort();
        // Returned complaints: a node the operator named before its closure
        // and named again after. The mention list must span the done date.
        let mut node_mentions: BTreeMap<String, Vec<&OpText>> = BTreeMap::new();
        for t in &texts {
            for node in mention_node_ids(&t.text, &known) {
                node_mentions.entry(node).or_default().push(t);
            }
        }
        let mut returned = Vec::new();
        for (node, mentions) in node_mentions {
            let Some(done) = done_at.get(&node) else {
                continue;
            };
            // Epochs when both stamps parse; the graph writes +00:00
            // offsets the transcript's Z form never string-orders the same
            // way, so a raw compare would misjudge same-day boundaries.
            let done_epoch = crate::intel::ts_secs(done);
            let stamp_cmp =
                |a: &str, b: &str, want_less: bool| match (crate::intel::ts_secs(a), done_epoch) {
                    (Some(x), Some(y)) => {
                        if want_less {
                            x < y
                        } else {
                            x > y
                        }
                    }
                    _ => {
                        if want_less {
                            a < b
                        } else {
                            a > b
                        }
                    }
                };
            let before = mentions
                .iter()
                .any(|m| !m.ts.is_empty() && stamp_cmp(&m.ts, done, true));
            let after = mentions
                .iter()
                .any(|m| !m.ts.is_empty() && stamp_cmp(&m.ts, done, false));
            if before && after {
                returned.push(ReturnedComplaint {
                    node,
                    first_mention: mentions.first().map(|m| m.ts.clone()).unwrap_or_default(),
                    last_mention: mentions.last().map(|m| m.ts.clone()).unwrap_or_default(),
                });
            }
        }
        by_lead.push(LeadRow {
            lead,
            sessions: sessions.len(),
            operator_turns: sessions.iter().map(|r| r.operator_turns.len()).sum(),
            restated_asks: restated,
            returned_complaints: returned,
        });
    }
    by_lead.sort_by(|a, b| a.lead.cmp(&b.lead));
    Scorecard {
        window_days: 0,
        coverage: Coverage {
            sessions_attributed: attributed,
            sessions_unknown: unknown_sessions,
            texts_best_effort: true,
        },
        by_model: by_model.into_values().collect(),
        by_lead,
        unmeasured: vec![
            "ci_rounds_per_merged_pr",
            "review_findings_per_pr",
            "reverts",
            "friction_events_per_model",
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intel::SessionRow;

    fn row(model: Option<&str>) -> SessionRow {
        let mut r = SessionRow::test_row("s1", "claude");
        r.model = model.map(str::to_string);
        r
    }

    fn op(ts: &str, text: &str) -> OpText {
        OpText {
            ts: ts.to_string(),
            text: text.to_string(),
        }
    }

    #[test]
    fn by_model_groups_rows_and_unknown_reads_coverage() {
        // AC3-HP: two models make two rows; a modelless session lands in
        // coverage, never in a row.
        let mut a = row(Some("glm-5.3-flash"));
        a.tool_use = 10;
        a.tool_errors = Some(BTreeMap::from([("command_failed".to_string(), 1)]));
        let mut b = row(Some("claude-opus-5-5"));
        b.tool_use = 4;
        let c = row(None);
        let sc = build(&[a, b, c], &OpTexts::new(), &HashMap::new(), &[]);
        assert_eq!(sc.by_model.len(), 2);
        assert_eq!(sc.coverage.sessions_unknown, 1);
        assert_eq!(sc.coverage.sessions_attributed, 2);
        let glm = sc
            .by_model
            .iter()
            .find(|m| m.model == "glm-5.3-flash")
            .unwrap();
        assert_eq!(glm.tool_calls, 10);
        assert_eq!(glm.tool_error_rate, 0.1);
    }

    #[test]
    fn a_complaint_spanning_a_node_closure_returns_once() {
        // AC3-ERR: named before the done date and again after it flags one
        // returned complaint; a node only named before closure flags none.
        // Fixture ids are computed so the source carries no backlog id.
        let reaper = format!("{}-{:x}", "zed", 49959u32);
        let reaped = format!("{}-{:x}", "zed", 2000u32);
        let mut r = row(Some("glm-5.3-flash"));
        r.session = "lead-1".into();
        let texts = vec![
            op(
                "2026-09-01T10:00:00Z",
                &format!("the reaper {reaper} is still broken"),
            ),
            op("2026-09-20T10:00:00Z", &format!("{reaper} came back again")),
            op(
                "2026-09-02T10:00:00Z",
                &format!("and {reaped} still reaps stale rows"),
            ),
        ];
        let mut op_texts = OpTexts::new();
        op_texts.insert("lead-1".to_string(), texts);
        let graph = vec![
            serde_json::json!({"id": reaper, "completed_at": "2026-09-04T00:00:00Z"}),
            serde_json::json!({"id": reaped, "completed_at": "2026-09-07T00:00:00Z"}),
        ];
        let sc = build(&[r], &op_texts, &HashMap::new(), &graph);
        assert_eq!(sc.by_lead.len(), 1);
        let lead = &sc.by_lead[0];
        assert_eq!(lead.returned_complaints.len(), 1);
        assert_eq!(lead.returned_complaints[0].node, reaper);
    }

    #[test]
    fn the_same_ask_twice_restates_once() {
        let mut r = row(Some("glm-5.3-flash"));
        r.session = "lead-2".into();
        let texts = vec![
            op(
                "2026-09-01T10:00:00Z",
                "can you rebase the branch before review?",
            ),
            op(
                "2026-09-02T10:00:00Z",
                "can you rebase the branch before review?",
            ),
        ];
        let mut op_texts = OpTexts::new();
        op_texts.insert("lead-2".to_string(), texts);
        let sc = build(&[r], &op_texts, &HashMap::new(), &[]);
        assert_eq!(sc.by_lead[0].restated_asks.len(), 1);
    }
}
