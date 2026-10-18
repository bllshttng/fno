//! The intel decision lens: for every ruling in the window, trace who issued
//! it, where its authority came from, whether anything enforced it, and
//! whether the fleet obeyed. A read-side join over the machine decision
//! store ([`crate::decision_index`]), the witnessed-operator-turn index
//! ([`crate::operator_witness::SubmitIndex`]), the mail bus
//! ([`crate::provenance::BusIndex`]) and the decision-span journal.
//!
//! Events first, transcripts second: every label cites a record, never
//! model judgment. A ruling that reads as the user's word with no witnessed
//! submit behind it reads `unsourced` (the E033 specimen), and a ruling
//! whose authority arrived through coordination lanes reads `relayed` (the
//! relayed-chain specimen from the 2026-10-13 lead retro). Compliance is
//! measured only where records exist:
//! `uncontested` means no recorded overturn or gate span, never proven
//! obedience, and the report prints that limit.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use crate::decision_index as di;
use crate::operator_witness::SubmitIndex;

/// Where a ruling's authority came from, ranked by the record it cites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Authority {
    /// An operator/chat_attested ruling whose issuing session has a
    /// witnessed `operator_submit` behind it.
    WitnessedOperator,
    /// The ruling cites a law-lane decision id in its text or rationale.
    LawCitation,
    /// The issuing session appears in a `reign_checkin` succession event.
    ReignEvent,
    /// The authority arrived through an agent/team/role/beastmode lane:
    /// coordination, never law.
    Relayed,
    /// Reads as the user's word with no witnessed turn behind it, or names
    /// no authority at all.
    Unsourced,
}

impl Authority {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Authority::WitnessedOperator => "witnessed_operator",
            Authority::LawCitation => "law_citation",
            Authority::ReignEvent => "reign_event",
            Authority::Relayed => "relayed",
            Authority::Unsourced => "unsourced",
        }
    }
}

/// Classify one ruling's authority from its records. `authority` is the
/// row's `authority_source`; `witnessed` whether the issuing session has a
/// witnessed submit; `reign_hit` whether the session appears in a
/// `reign_checkin` event; `cited_law` whether the text cites a law-lane id.
pub(crate) fn classify_authority(
    authority: &str,
    witnessed: bool,
    reign_hit: bool,
    cited_law: bool,
) -> Authority {
    if authority == "operator" || authority == "chat_attested" {
        // The witness decides: the lane claims the user's word, and only a
        // witnessed submit proves it (the E033 shape).
        return if witnessed {
            Authority::WitnessedOperator
        } else {
            Authority::Unsourced
        };
    }
    if reign_hit {
        return Authority::ReignEvent;
    }
    if cited_law {
        return Authority::LawCitation;
    }
    if authority.is_empty() {
        return Authority::Unsourced;
    }
    Authority::Relayed
}

/// Whether the ruling text or rationale cites a decision id that is
/// law-lane in this store.
fn cites_law(row: &Value, law_ids: &HashSet<String>) -> bool {
    let text = row.get("decision").and_then(Value::as_str).unwrap_or("");
    let rationale = row.get("rationale").and_then(Value::as_str).unwrap_or("");
    extract_decision_ids(text)
        .iter()
        .chain(extract_decision_ids(rationale).iter())
        .any(|id| law_ids.contains(&id.to_lowercase()))
}

/// Every `d-<8 hex>` id in `text`, case-preserved.
pub(crate) fn extract_decision_ids(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        if bytes[i] == b'd'
            && bytes[i + 1] == b'-'
            && bytes[i + 2..i + 10].iter().all(u8::is_ascii_hexdigit)
        {
            out.push(text[i..i + 10].to_string());
            i += 10;
        } else {
            i += 1;
        }
    }
    out
}

/// Sessions named by `reign_checkin` journal rows: the row's `session`
/// data field, or an envelope source that is itself session-shaped. An
/// issuer absent from this set reads no crown authority; the class never
/// infers one.
fn reign_sessions(journal: &Path) -> HashSet<String> {
    let text = crate::event_store::journal_text(journal, &["reign_checkin"]);
    let mut out = HashSet::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) != Some("reign_checkin") {
            continue;
        }
        if let Some(session) = v.pointer("/data/session").and_then(Value::as_str) {
            out.insert(session.to_string());
        }
        if let Some(source) = v.get("source").and_then(Value::as_str) {
            if source.len() == 8 {
                out.insert(source.to_string());
            }
        }
    }
    out
}

/// One report row per ruling in the window.
#[derive(Debug, Serialize)]
struct LensRow {
    id: String,
    ts: String,
    issuer: String,
    authority: &'static str,
    enforced: String,
    obeyed: String,
    subject: String,
}

/// One fleet mail-graph hop: a from > to pair with its count, whether the
/// last row went unanswered, and the ruling ids its bodies relay.
#[derive(Debug, Serialize)]
struct MailHop {
    pair: String,
    count: usize,
    unanswered: bool,
    relays: Vec<String>,
}

#[derive(Debug, Serialize)]
struct LensReport {
    days: u64,
    rulings: Vec<LensRow>,
    unsourced: usize,
    prose_only: usize,
    mail_hops: Vec<MailHop>,
    unanswered: usize,
    limit: &'static str,
}

const COMPLIANCE_LIMIT: &str = "obedience reads recorded overturns, retracts and gate spans only; uncontested means nothing against it is on record, never proven obedience";

/// Whether any `decision_span` journal row names this ruling: the record of
/// a guard block or gate override on a move the ruling governs. Returns the
/// span kind when the row carries one.
fn enforcement_span(row_id: &str, span_lines: &[&str]) -> Option<String> {
    let wanted = row_id.to_lowercase();
    for line in span_lines {
        if !line.to_lowercase().contains(&wanted) {
            continue;
        }
        let kind = serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|v| {
                v.pointer("/data/span_kind")
                    .or_else(|| v.pointer("/data/class"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_else(|| "decision_span".to_string());
        return Some(kind);
    }
    None
}

/// The obedience verdict from the store's retirement map plus the spans.
fn obedience_for(
    row_id: &str,
    retired: &BTreeMap<String, ((String, String), &'static str)>,
    span_hit: bool,
) -> String {
    match retired.get(&row_id.to_lowercase()) {
        Some((_, kind)) => format!("overturned:{kind}"),
        None if span_hit => "blocked:gate_span".to_string(),
        None => "uncontested".to_string(),
    }
}

/// The fleet mail graph over bus rows: pair counts over 8-char prefixes,
/// unanswered tails, and the ruling ids each hop's bodies relay.
fn mail_hops(rows: &[crate::provenance::BusRow]) -> (Vec<MailHop>, usize) {
    let short = |id: &Option<String>| {
        id.as_deref()
            .map(|s| s.chars().take(8).collect::<String>())
            .unwrap_or_default()
    };
    let mut pairs: BTreeMap<(String, String), Vec<&crate::provenance::BusRow>> = BTreeMap::new();
    for row in rows {
        let from = short(&row.from_session);
        let to = short(&row.to_session);
        if from.is_empty() || to.is_empty() {
            continue;
        }
        pairs.entry((from, to)).or_default().push(row);
    }
    let mut hops = Vec::new();
    let mut unanswered_total = 0usize;
    for ((from, to), rows) in pairs {
        let last = rows.last().expect("non-empty by construction");
        let last_ts = crate::intel::ts_secs(&last.ts);
        let answered = rows.iter().any(|r| {
            short(&r.from_session) == to
                && short(&r.to_session) == from
                && crate::intel::ts_secs(&r.ts).is_some_and(|t2| last_ts.is_none_or(|t1| t2 > t1))
        });
        if !answered {
            unanswered_total += 1;
        }
        let mut relays = Vec::new();
        for row in rows {
            for id in extract_decision_ids(&row.body) {
                let lower = id.to_lowercase();
                if !relays.contains(&lower) {
                    relays.push(lower);
                }
            }
        }
        hops.push(MailHop {
            pair: format!("{from} > {to}"),
            count: rows.len(),
            unanswered: !answered,
            relays,
        });
    }
    hops.sort_by(|a, b| b.count.cmp(&a.count).then(a.pair.cmp(&b.pair)));
    (hops, unanswered_total)
}

/// Build the report rows from raw store rows plus the journal and bus
/// reads. Pure over its inputs; `run_lens` does the IO around it.
fn lens_rows(
    rows: &[Value],
    witness: &SubmitIndex,
    reign: &HashSet<String>,
    span_lines: &[&str],
    cutoff: Option<u64>,
) -> (Vec<LensRow>, usize, usize) {
    // The law-lane id set, for citation checks.
    let law_ids: HashSet<String> = rows
        .iter()
        .filter(|r| di::is_law(r))
        .filter_map(|r| r.get("decision_id").and_then(Value::as_str))
        .map(|s| s.to_lowercase())
        .collect();
    let retired = di::retirement_map(rows);
    let mut out = Vec::new();
    for row in rows.iter() {
        if row.get("_event_type").and_then(Value::as_str) != Some("operator_decision") {
            continue;
        }
        let Some(id) = row
            .get("decision_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let ts = row.get("ts").and_then(Value::as_str).unwrap_or("");
        if let Some(cutoff) = cutoff {
            match crate::state::rfc3339_like_to_secs(ts) {
                Some(secs) if secs >= cutoff => {}
                _ => continue,
            }
        }
        let issuer = row
            .get("decided_by")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let authority_src = row
            .get("authority_source")
            .and_then(Value::as_str)
            .unwrap_or("");
        let witnessed = witness.has_session_or_prefix(&issuer);
        let reign_hit = reign.contains(&issuer);
        let authority = classify_authority(
            authority_src,
            witnessed,
            reign_hit,
            cites_law(row, &law_ids),
        );
        let span = enforcement_span(id, span_lines);
        let enforced = match &span {
            Some(kind) => format!("gate_span:{kind}"),
            None => "prose_only".to_string(),
        };
        let obeyed = obedience_for(id, &retired, span.is_some());
        out.push(LensRow {
            id: id.to_string(),
            ts: ts.to_string(),
            issuer,
            authority: authority.as_str(),
            enforced,
            obeyed,
            subject: row
                .get("subject")
                .and_then(Value::as_str)
                .unwrap_or("")
                .chars()
                .take(60)
                .collect(),
        });
    }
    out.sort_by(|a, b| b.ts.cmp(&a.ts));
    let unsourced = out
        .iter()
        .filter(|r| r.authority == Authority::Unsourced.as_str())
        .count();
    let prose_only = out.iter().filter(|r| r.enforced == "prose_only").count();
    (out, unsourced, prose_only)
}

/// Run the decision lens: `fno-agents intel --lens decisions [--days N] [--json]`.
pub fn run_lens(days: u64, json: bool) -> i32 {
    let home = crate::paths::AgentsHome::from_env();
    let journal = home.events_jsonl();
    let graph = crate::graph_get::default_graph_path();
    let jsonl = di::default_state_path("decisions.jsonl");
    let (raw, damaged) = match di::read_store_rows(&graph, &jsonl) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fno-agents intel --lens decisions: {e}");
            return 1;
        }
    };
    let witness = SubmitIndex::load(&journal);
    let reign = reign_sessions(&journal);
    let span_text = crate::event_store::journal_text(&journal, &["decision_span"]);
    let span_lines: Vec<&str> = span_text.lines().collect();
    let cutoff = if days == 0 {
        None
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Some(now.saturating_sub(days * 86_400))
    };
    let (rulings, unsourced, prose_only) = lens_rows(&raw, &witness, &reign, &span_lines, cutoff);
    let bus = crate::provenance::BusIndex::load(&crate::intel::bus_log_path(
        &home
            .root()
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| home.root().to_path_buf()),
    ));
    let bus_rows: Vec<crate::provenance::BusRow> = bus
        .rows()
        .iter()
        .filter(|r| match (cutoff, crate::intel::ts_secs(&r.ts)) {
            (Some(cutoff), Some(secs)) => secs >= cutoff,
            (None, _) => true,
            (Some(_), None) => false,
        })
        .cloned()
        .collect();
    let (hops, unanswered) = mail_hops(&bus_rows);
    let report = LensReport {
        days,
        unsourced,
        prose_only,
        rulings,
        mail_hops: hops,
        unanswered,
        limit: COMPLIANCE_LIMIT,
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
        );
    } else {
        print_report(&report, damaged);
    }
    0
}

fn print_report(report: &LensReport, damaged: usize) {
    let total = report.rulings.len();
    println!("decision lens (days {}): {total} ruling(s)", report.days);
    if damaged > 0 {
        println!("  damaged store lines: {damaged}");
    }
    println!(
        "  {:<11} {:<21} {:<9} {:<19} {:<24} {}",
        "ruling", "ts", "issuer", "authority", "enforced", "obeyed"
    );
    for r in &report.rulings {
        println!(
            "  {:<11} {:<21} {:<9} {:<19} {:<24} {}",
            r.id, r.ts, r.issuer, r.authority, r.enforced, r.obeyed
        );
    }
    if total > 0 {
        let up = report.unsourced * 100 / total;
        let pp = report.prose_only * 100 / total;
        println!(
            "shares: unsourced {}/{} ({up}%), prose-only enforced {}/{} ({pp}%)",
            report.unsourced, total, report.prose_only, total
        );
    } else {
        println!("shares: unsourced 0/0, prose-only enforced 0/0");
    }
    println!(
        "mail graph: {} hop(s), {} unanswered",
        report.mail_hops.len(),
        report.unanswered
    );
    for hop in report.mail_hops.iter().take(20) {
        let relay = if hop.relays.is_empty() {
            String::new()
        } else {
            format!(" relays {}", hop.relays.join(","))
        };
        let state = if hop.unanswered { " unanswered" } else { "" };
        println!("  {} x{}{relay}{state}", hop.pair, hop.count);
    }
    println!("limit: {}", report.limit);
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::operator_witness::SubmitIndex;

    fn row(id: &str, authority: &str, decided_by: &str, ts: &str, body: &str) -> Value {
        serde_json::json!({
            "_event_type": "operator_decision",
            "decision_id": id,
            "authority_source": authority,
            "decided_by": decided_by,
            "ts": ts,
            "subject": "test ruling",
            "decision": body,
            "rationale": "",
        })
    }

    #[test]
    fn intel_lens_authority_classification_rules() {
        let a = classify_authority("operator", true, false, false);
        assert_eq!(a, Authority::WitnessedOperator);
        // The E033 shape: recorded as the user's ruling, no witnessed turn.
        let a = classify_authority("operator", false, false, false);
        assert_eq!(a, Authority::Unsourced);
        let a = classify_authority("chat_attested", false, false, false);
        assert_eq!(a, Authority::Unsourced);
        let a = classify_authority("agent", false, false, false);
        assert_eq!(a, Authority::Relayed);
        let a = classify_authority("beastmode", false, false, false);
        assert_eq!(a, Authority::Relayed);
        let a = classify_authority("agent", false, true, false);
        assert_eq!(a, Authority::ReignEvent);
        let a = classify_authority("agent", false, false, true);
        assert_eq!(a, Authority::LawCitation);
        let a = classify_authority("", false, false, false);
        assert_eq!(a, Authority::Unsourced);
    }

    #[test]
    fn intel_lens_extract_decision_ids_scans_hex() {
        let ids = extract_decision_ids("per d-20122c30 and mail:d-0e9ed907, also D-DEADBEEF no");
        assert_eq!(ids, vec!["d-20122c30", "d-0e9ed907"]);
        assert!(extract_decision_ids("no ids here").is_empty());
    }

    #[test]
    fn intel_lens_enforcement_reads_the_span_journal() {
        let span = r#"{"type":"decision_span","data":{"span_kind":"guard-block","decision_id":"d-20122c30"}}"#;
        let lines = vec![span];
        assert_eq!(
            enforcement_span("d-20122c30", &lines),
            Some("guard-block".to_string())
        );
        assert_eq!(enforcement_span("d-00000000", &lines), None);
    }

    #[test]
    fn intel_lens_obedience_reads_the_retirement_map() {
        let retired: BTreeMap<String, ((String, String), &'static str)> = BTreeMap::new();
        assert_eq!(obedience_for("d-11111111", &retired, false), "uncontested");
        assert_eq!(
            obedience_for("d-11111111", &retired, true),
            "blocked:gate_span"
        );
        let mut retired = retired;
        retired.insert(
            "d-22222222".to_string(),
            (
                ("2026-10-16T00:00:00Z".to_string(), "d-33333333".to_string()),
                "superseded",
            ),
        );
        assert_eq!(
            obedience_for("d-22222222", &retired, false),
            "overturned:superseded"
        );
    }

    #[test]
    fn intel_lens_rows_label_authority_classes() {
        let rows = vec![
            row(
                "d-aaaa1111",
                "operator",
                "8724ab44",
                "2026-10-14T20:49:49Z",
                "user said",
            ),
            row(
                "d-bbbb2222",
                "agent",
                "abcd1234",
                "2026-10-15T10:00:00Z",
                "echo",
            ),
        ];
        // An empty witness index: no session has a submit.
        let witness = SubmitIndex::empty();
        let reign = HashSet::new();
        let (out, unsourced, prose_only) = lens_rows(&rows, &witness, &reign, &[], None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].authority, "unsourced");
        assert_eq!(out[1].authority, "relayed");
        assert_eq!(unsourced, 1);
        assert_eq!(prose_only, 2);
        let rows = vec![
            row(
                "d-law00001",
                "operator",
                "8724ab44",
                "2026-08-25T00:00:00Z",
                "the law",
            ),
            row(
                "d-cccc3333",
                "agent",
                "abcd1234",
                "2026-10-15T10:00:00Z",
                "per d-law00001 we hold",
            ),
        ];
        let witness = SubmitIndex::empty();
        let reign = HashSet::new();
        let (out, _u, _p) = lens_rows(&rows, &witness, &reign, &[], None);
        // d-law00001: operator authority, no witness -> unsourced even in the law lane.
        assert_eq!(out[1].authority, "unsourced");
        // The citing row: agent lane, cites a law-lane id -> law_citation.
        assert_eq!(out[0].authority, "law_citation");
    }

    #[test]
    fn intel_lens_mail_graph_counts_unanswered_and_relays() {
        let mk = |id: &str, from: &str, to: &str, ts: &str, body: &str| crate::provenance::BusRow {
            id: id.to_string(),
            ts: ts.to_string(),
            from_session: Some(from.to_string()),
            to_session: Some(to.to_string()),
            body: body.to_string(),
            words: body.split_whitespace().count(),
        };
        let rows = vec![
            mk(
                "m1",
                "aaaaaaaa",
                "bbbbbbbb",
                "2026-10-15T10:00:00Z",
                "run d-20122c30",
            ),
            mk("m2", "bbbbbbbb", "aaaaaaaa", "2026-10-15T10:01:00Z", "done"),
            mk(
                "m3",
                "aaaaaaaa",
                "cccccccc",
                "2026-10-15T11:00:00Z",
                "per d-20122c30",
            ),
        ];
        let (hops, unanswered) = mail_hops(&rows);
        assert_eq!(hops.len(), 2);
        assert_eq!(unanswered, 1);
        let relay_hop = hops
            .iter()
            .find(|h| h.pair == "aaaaaaaa > cccccccc")
            .unwrap();
        assert!(relay_hop.unanswered);
        assert_eq!(relay_hop.relays, vec!["d-20122c30"]);
        let answered_hop = hops
            .iter()
            .find(|h| h.pair == "aaaaaaaa > bbbbbbbb")
            .unwrap();
        assert!(!answered_hop.unanswered);
        assert_eq!(answered_hop.relays.len(), 0);
    }
}
