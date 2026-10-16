//! Friction: one typed row when an agent session hits a real problem, and a
//! guarded match that may offer the session a backlog vote.
//!
//! Six producer doors call [`record`]: the Stop path's `<help>` and
//! `RESULT: BLOCKED` (distress.rs), the spawn, join and done gate refusals,
//! and loop-check's read of the session's own PR (a review round spent
//! without a pass, a red CI check). Nothing else does: an ordinary tool call
//! never produces friction, so it never pays for a match.
//!
//! The guard order is the load contract, and each guard that refuses writes
//! no file: one row per session per fingerprint (an events read), then
//! `friction.match.enabled` (off by default), then three offers per session,
//! then the ledger debounce, then ONE backlog read connection interrupted
//! past [`MATCH_BUDGET`]. The row lands whatever the guards say; `match`
//! names what the match did. Every failure here is swallowed: friction never
//! changes a stop verdict, a gate verdict or an exit code.

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Past this the match is interrupted and skipped, never waited on.
pub(crate) const MATCH_BUDGET: Duration = Duration::from_millis(200);
const MAX_OFFERS: usize = 3;
const MAX_CANDIDATES: usize = 3;
const EVIDENCE_CAP: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Origin {
    Help,
    ResultBlocked,
    GateRefusal,
    ReviewRound,
    CiFailure,
}

impl Origin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Origin::Help => "help",
            Origin::ResultBlocked => "result_blocked",
            Origin::GateRefusal => "gate_refusal",
            Origin::ReviewRound => "review_round",
            Origin::CiFailure => "ci_failure",
        }
    }
}

/// The event logs a producer already writes: the friction row goes beside
/// its existing row.
pub(crate) struct Sinks<'a> {
    pub project: &'a Path,
    pub global: &'a Path,
}

/// One friction signal. `key` is what makes two signals of one origin the
/// same signal (the capped reason, the gate plus reason, the PR plus round
/// count, the PR plus head); the fingerprint hashes origin and key.
pub(crate) struct FrictionInput<'a> {
    pub harness_session: Option<&'a str>,
    pub node: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub origin: Origin,
    pub key: String,
    pub gate: Option<&'a str>,
    pub pr: Option<i64>,
    pub head_sha: Option<&'a str>,
    pub stage: Option<&'a str>,
    pub reason: &'a str,
    pub evidence: &'a str,
}

/// Where this session's ledger and pending offer live.
fn friction_dir() -> PathBuf {
    crate::paths::AgentsHome::from_env().root().join("friction")
}

/// `friction.match.enabled` in the first config that sets it; absent is off.
fn match_enabled() -> bool {
    crate::backlog::settings::config_candidates()
        .iter()
        .find_map(|doc| doc.get("friction")?.get("match")?.get("enabled")?.as_bool())
        .unwrap_or(false)
}

/// The production entry: resolves the graph, the friction dir and the config
/// switch, then runs [`record_with`]. Returns the row's `match` word when a
/// row was written.
pub(crate) fn record(sinks: &Sinks<'_>, input: &FrictionInput<'_>) -> Option<&'static str> {
    record_with(
        sinks,
        input,
        &crate::backlog::settings::graph_path(),
        &friction_dir(),
        match_enabled(),
    )
}

/// The gate-refusal door: `gate` (spawn, join or done) refused this
/// process's own session. The row goes beside the refusal's own row in
/// `events`; the session is the one this process can prove, else nothing.
/// `key` is the refusal's stable identity; a reason that carries live
/// figures passes a key without them, so one refusal re-fired is one row.
pub(crate) fn record_gate_refusal(
    events: &Path,
    gate: &str,
    node: Option<&str>,
    key: &str,
    reason: &str,
) {
    let get = |name: &str| std::env::var(name).ok();
    let ident = crate::spawn_context::resolve_self_identity(
        &get,
        None,
        None,
        &crate::paths::AgentsHome::from_env(),
    );
    let _ = record(
        &Sinks {
            project: events,
            global: events,
        },
        &FrictionInput {
            harness_session: ident.session_id.as_deref(),
            node,
            harness: ident.harness.as_deref(),
            origin: Origin::GateRefusal,
            key: format!("{gate}\n{}", cap(key)),
            gate: Some(gate),
            pr: None,
            head_sha: None,
            stage: None,
            reason,
            evidence: reason,
        },
    );
}

/// What loop-check's done read saw on this session's own PR. The read takes
/// no PR selector, so the PR is the one on the session's worktree branch,
/// and `harness_session` is the manifest's: that pair is the ownership.
pub(crate) struct PrRead<'a> {
    pub harness_session: Option<&'a str>,
    pub node: Option<&'a str>,
    pub harness: Option<&'a str>,
    pub number: i64,
    pub head_sha: &'a str,
    pub ci_failure: bool,
    pub failing_checks: &'a [String],
    pub rounds_used: i64,
    pub rounds_max: i64,
    pub unaddressed_blocking: usize,
    /// A hard local finding is still open at this head.
    pub hard_blocker: bool,
}

/// The PR doors: a review round spent without a pass (one row per round
/// count) and a red CI check (one row per head). `rounds_used` counts every
/// reviewed head, pass or fail, so a round counts as friction only while a
/// blocking finding from it is still open.
pub(crate) fn on_pr_read(sinks: &Sinks<'_>, pr: &PrRead<'_>) {
    on_pr_read_with(sinks, pr, record);
}

fn on_pr_read_with(
    sinks: &Sinks<'_>,
    pr: &PrRead<'_>,
    record: impl Fn(&Sinks<'_>, &FrictionInput<'_>) -> Option<&'static str>,
) {
    let head8: String = pr.head_sha.chars().take(8).collect();
    if pr.rounds_used >= 1 && (pr.hard_blocker || pr.unaddressed_blocking > 0) {
        let evidence = format!(
            "PR #{} spent review round {}/{} without a pass at {head8}; {} inline blocking finding(s) unaddressed, hard local finding open: {}",
            pr.number, pr.rounds_used, pr.rounds_max, pr.unaddressed_blocking, pr.hard_blocker
        );
        let _ = record(
            sinks,
            &FrictionInput {
                harness_session: pr.harness_session,
                node: pr.node,
                harness: pr.harness,
                origin: Origin::ReviewRound,
                key: format!("{}\n{}", pr.number, pr.rounds_used),
                gate: None,
                pr: Some(pr.number),
                head_sha: Some(pr.head_sha),
                stage: Some("review"),
                reason: &evidence,
                evidence: &evidence,
            },
        );
    }
    if pr.ci_failure {
        let checks: Vec<&str> = pr
            .failing_checks
            .iter()
            .take(5)
            .map(String::as_str)
            .collect();
        let evidence = format!(
            "PR #{} CI red at {head8}: {}",
            pr.number,
            if checks.is_empty() {
                "a failing check".to_string()
            } else {
                checks.join(", ")
            }
        );
        let _ = record(
            sinks,
            &FrictionInput {
                harness_session: pr.harness_session,
                node: pr.node,
                harness: pr.harness,
                origin: Origin::CiFailure,
                key: format!("{}\n{}", pr.number, pr.head_sha),
                gate: None,
                pr: Some(pr.number),
                head_sha: Some(pr.head_sha),
                stage: Some("ci"),
                reason: &evidence,
                evidence: &evidence,
            },
        );
    }
}

pub(crate) fn fingerprint(origin: Origin, key: &str) -> String {
    let digest = Sha256::digest(format!("{}\n{key}", origin.as_str()).as_bytes());
    digest.iter().take(6).map(|b| format!("{b:02x}")).collect()
}

fn cap(s: &str) -> String {
    s.chars().take(EVIDENCE_CAP).collect()
}

fn record_with(
    sinks: &Sinks<'_>,
    input: &FrictionInput<'_>,
    graph: &Path,
    dir: &Path,
    enabled: bool,
) -> Option<&'static str> {
    // Who is required: a signal no session owns is not this session's
    // friction, and nothing could ever read its offer.
    let session = input
        .harness_session
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let fp = fingerprint(input.origin, &input.key);
    if already_recorded(sinks.project, session, &fp) {
        return None;
    }
    let started = Instant::now();
    let mut stage = input.stage.map(str::to_string);
    let mut candidates: Vec<(String, String)> = Vec::new();
    let outcome = if !enabled {
        "off"
    } else {
        let mut ledger = Ledger::read(dir, session);
        if ledger.offers.len() >= MAX_OFFERS {
            "cap"
        } else if ledger.knows(&fp) {
            "debounce"
        } else {
            let text = format!("{} {}", input.reason, input.evidence);
            let tokens = match_tokens(&text);
            let found = if tokens.len() < 2 {
                Ok(Vec::new())
            } else {
                let node = input.node;
                match_open_nodes(graph, &tokens, node, session, stage.is_none()).map(
                    |(hits, phase)| {
                        if stage.is_none() {
                            stage = phase;
                        }
                        hits
                    },
                )
            };
            let word = match found {
                Err(Miss::Budget) => "budget",
                Err(Miss::Error) => "error",
                Ok(hits) if hits.is_empty() => "empty",
                Ok(hits) => {
                    candidates = hits;
                    "offered"
                }
            };
            if word == "offered" {
                let offer = json!({
                    "fingerprint": fp,
                    "origin": input.origin.as_str(),
                    "node": input.node,
                    "stage": stage,
                    "candidates": candidates.iter().map(|(id, _)| id).collect::<Vec<_>>(),
                    "model": crate::backlog::encounter_cli::vote_provenance(input.harness).get("model"),
                    "ts": crate::graph_store::now_isoformat(),
                });
                ledger.offers.push(offer);
                let text = offer_text(&fp, input, &candidates);
                let _ = write_atomic(&dir.join(format!("{session}.offer")), &text);
            } else {
                ledger.seen.push(Value::String(fp.clone()));
            }
            let _ = ledger.write(dir, session);
            word
        }
    };
    let mut data = Map::new();
    data.insert("session_id".into(), json!(session));
    if let Some(node) = input.node {
        data.insert("node_id".into(), json!(node));
    }
    if let Some(stage) = &stage {
        data.insert("stage".into(), json!(stage));
    }
    if let Some(h) = input.harness {
        data.insert("harness".into(), json!(h));
    }
    let provenance = crate::backlog::encounter_cli::vote_provenance(input.harness);
    if let Some(model) = provenance.get("model") {
        data.insert("model".into(), model.clone());
    }
    data.insert("evidence".into(), json!(cap(input.evidence)));
    data.insert("reason".into(), json!(cap(input.reason)));
    data.insert("origin".into(), json!(input.origin.as_str()));
    if let Some(g) = input.gate {
        data.insert("gate".into(), json!(g));
    }
    if let Some(pr) = input.pr {
        data.insert("pr".into(), json!(pr));
    }
    if let Some(head) = input.head_sha {
        data.insert("head_sha".into(), json!(head));
    }
    data.insert("fingerprint".into(), json!(fp));
    data.insert("match".into(), json!(outcome));
    if !candidates.is_empty() {
        let ids: Vec<&String> = candidates.iter().map(|(id, _)| id).collect();
        data.insert("candidates".into(), json!(ids));
    }
    if enabled {
        data.insert(
            "elapsed_ms".into(),
            json!(started.elapsed().as_millis() as u64),
        );
    }
    let env = json!({
        "ts": crate::loopcheck::now_rfc3339_utc(),
        "v": 1,
        "type": "friction",
        "source": "target",
        "data": Value::Object(data),
    });
    let timeout = Duration::from_secs(2);
    if let Err(e) = crate::claims::append_event_line(sinks.project, &env, timeout) {
        eprintln!(
            "friction: write to {} failed (non-fatal): {e}",
            sinks.project.display()
        );
        return None;
    }
    if sinks.global != sinks.project {
        let _ = crate::claims::append_event_line(sinks.global, &env, timeout);
    }
    Some(outcome)
}

/// True when the project log already holds this session's row for this
/// fingerprint. The stop path re-fires on every turn end and loop-check on
/// every fire, so this read is what keeps one red head one row.
fn already_recorded(project_events: &Path, session: &str, fp: &str) -> bool {
    let _ = crate::event_store::import_all(project_events);
    let Ok(rows) = crate::event_store::query_events(
        project_events,
        &crate::event_store::EventQuery {
            types: vec!["friction".to_string()],
            session_id: Some(session.to_string()),
            ..Default::default()
        },
    ) else {
        return false;
    };
    rows.iter()
        .filter_map(|r| serde_json::from_str::<Value>(&r.line).ok())
        .any(|v| v.pointer("/data/fingerprint").and_then(Value::as_str) == Some(fp))
}

const STOPWORDS: &[&str] = &[
    "about", "after", "again", "also", "because", "been", "before", "being", "cannot", "could",
    "does", "done", "each", "from", "have", "here", "into", "just", "more", "must", "need",
    "needs", "only", "other", "over", "should", "some", "still", "such", "than", "that", "their",
    "them", "then", "there", "they", "this", "under", "very", "were", "what", "when", "which",
    "while", "will", "with", "would", "your",
];

/// The match tokens: alphanumeric runs of four or more, lowercased, minus
/// the stopwords, the first eight distinct.
fn match_tokens(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() < 4 || STOPWORDS.contains(&word.as_str()) || out.contains(&word) {
            continue;
        }
        out.push(word);
        if out.len() == 8 {
            break;
        }
    }
    out
}

enum Miss {
    Budget,
    Error,
}

/// One read connection under the budget: the open nodes the tokens match
/// (never the working node), and the working node's open session phase for
/// this session when the producer named no stage.
fn match_open_nodes(
    graph: &Path,
    tokens: &[String],
    node: Option<&str>,
    session: &str,
    want_stage: bool,
) -> Result<(Vec<(String, String)>, Option<String>), Miss> {
    let started = Instant::now();
    let connection = crate::backlog::read_connection(graph).map_err(|_| Miss::Error)?;
    let Some(left) = MATCH_BUDGET.checked_sub(started.elapsed()) else {
        return Err(Miss::Budget);
    };
    // The timer interrupts the query in flight; dropping the sender first
    // (the query finished) wakes it with Disconnected, which interrupts
    // nothing.
    let fired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let handle = connection.get_interrupt_handle();
    let flag = fired.clone();
    let timer = std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(left) {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            handle.interrupt();
        }
    });
    let result = (|| -> Result<_, String> {
        let hits = crate::backlog::search::search_any(&connection, tokens, 30)?;
        let mut kept = Vec::new();
        for hit in hits {
            if Some(hit.id.as_str()) == node {
                continue;
            }
            let body = format!("{} {} {}", hit.title, hit.slug, hit.description).to_lowercase();
            if tokens.iter().filter(|t| body.contains(t.as_str())).count() < 2 {
                continue;
            }
            kept.push((hit.id, hit.title));
            if kept.len() == MAX_CANDIDATES {
                break;
            }
        }
        let mut phase = None;
        if let (true, Some(node)) = (want_stage, node) {
            phase = crate::backlog::sessions::load(&connection, node)?
                .into_iter()
                .rev()
                .find(|s| s.session_id == session && s.ended_at.is_none())
                .map(|s| s.phase);
        }
        Ok((kept, phase))
    })();
    drop(tx);
    let _ = timer.join();
    match result {
        Ok(found) => Ok(found),
        Err(_) if fired.load(std::sync::atomic::Ordering::SeqCst) => Err(Miss::Budget),
        Err(_) => Err(Miss::Error),
    }
}

fn offer_text(fp: &str, input: &FrictionInput<'_>, candidates: &[(String, String)]) -> String {
    let mut text = format!(
        "fno friction ({}, fingerprint {fp}): {}\nThese open nodes may be what you hit:\n",
        input.origin.as_str(),
        input.evidence.chars().take(200).collect::<String>(),
    );
    for (id, title) in candidates {
        text.push_str(&format!("  - {id}: {title}\n"));
    }
    text.push_str(&format!(
        "Only if one of them is what you hit, vote for it once:\n  \
         fno backlog encounter <id> --evidence \"<one sentence>\" --friction {fp}\n\
         If none is, ignore this.\n"
    ));
    text
}

/// This session's offers and the fingerprints whose match found nothing.
#[derive(Default)]
struct Ledger {
    offers: Vec<Value>,
    seen: Vec<Value>,
}

impl Ledger {
    fn path(dir: &Path, session: &str) -> PathBuf {
        dir.join(format!("{session}.json"))
    }

    fn read(dir: &Path, session: &str) -> Ledger {
        let doc: Value = std::fs::read_to_string(Self::path(dir, session))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or(Value::Null);
        let list = |k: &str| {
            doc.get(k)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        };
        Ledger {
            offers: list("offers"),
            seen: list("seen"),
        }
    }

    fn knows(&self, fp: &str) -> bool {
        self.seen.iter().any(|v| v.as_str() == Some(fp))
            || self
                .offers
                .iter()
                .any(|o| o.get("fingerprint").and_then(Value::as_str) == Some(fp))
    }

    fn write(&self, dir: &Path, session: &str) -> std::io::Result<()> {
        let doc = json!({"offers": self.offers, "seen": self.seen});
        write_atomic(&Self::path(dir, session), &doc.to_string())
    }
}

/// Temp file plus rename: two stops of one session never leave a torn file.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// The offer this session's ledger holds for `fp`, for the vote's
/// provenance, or `Err(ledger path)` when it holds none.
pub(crate) fn offer_for(session: &str, fp: &str) -> Result<Value, PathBuf> {
    let dir = friction_dir();
    Ledger::read(&dir, session)
        .offers
        .into_iter()
        .find(|o| o.get("fingerprint").and_then(Value::as_str) == Some(fp))
        .ok_or_else(|| Ledger::path(&dir, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SID: &str = "cccccccc-3333-4333-8333-333333333333";

    struct World {
        _dir: tempfile::TempDir,
        events: PathBuf,
        graph: PathBuf,
        friction: PathBuf,
    }

    fn world(nodes: &[Value]) -> World {
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        crate::graph_store::seed_rows_with_slugs(&graph, nodes).unwrap();
        World {
            events: dir.path().join("events.jsonl"),
            friction: dir.path().join("friction"),
            graph,
            _dir: dir,
        }
    }

    /// A row the store can represent: a row missing a required field is
    /// parked in nodes_raw, which the match never reads.
    fn node(id: &str, title: &str, status: &str) -> Value {
        json!({
            "id": id, "slug": id, "title": title, "type": "feature",
            "status": status, "priority": "p2", "domain": "code",
            "created_at": "2026-09-11T00:00:00+00:00"
        })
    }

    fn help<'a>(reason: &'a str) -> FrictionInput<'a> {
        FrictionInput {
            harness_session: Some(SID),
            node: Some("zz-0001"),
            harness: Some("codex"),
            origin: Origin::Help,
            key: reason.to_string(),
            gate: None,
            pr: None,
            head_sha: None,
            stage: None,
            reason,
            evidence: reason,
        }
    }

    fn rows(w: &World) -> Vec<Value> {
        let _ = crate::event_store::import_all(&w.events);
        crate::event_store::query_events(
            &w.events,
            &crate::event_store::EventQuery {
                types: vec!["friction".into()],
                ..Default::default()
            },
        )
        .unwrap()
        .iter()
        .map(|r| serde_json::from_str(&r.line).unwrap())
        .collect()
    }

    fn go(w: &World, input: &FrictionInput<'_>, enabled: bool) -> Option<&'static str> {
        let sinks = Sinks {
            project: &w.events,
            global: &w.events,
        };
        record_with(&sinks, input, &w.graph, &w.friction, enabled)
    }

    #[test]
    fn the_match_off_writes_one_row_and_no_file() {
        let w = world(&[]);
        let input = help("cargo keeper lock hangs the test run");
        assert_eq!(go(&w, &input, false), Some("off"));
        // A re-fire of the same signal is the same row: nothing new lands.
        assert_eq!(go(&w, &input, false), None);
        let rows = rows(&w);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["data"]["origin"], "help");
        assert_eq!(rows[0]["data"]["session_id"], SID);
        assert!(!w.friction.exists());
        // No session id: no row at all.
        let anon = FrictionInput {
            harness_session: None,
            ..help("another signal entirely here")
        };
        assert_eq!(go(&w, &anon, false), None);
    }

    #[test]
    fn a_match_offers_open_nodes_once_and_never_the_working_node() {
        let w = world(&[
            node("zz-0001", "keeper lock hangs cargo test", "ready"),
            node("zz-0002", "keeper lock hangs cargo test runs", "ready"),
            node("zz-0003", "keeper lock hangs cargo", "done"),
            node("zz-0004", "unrelated lock", "ready"),
        ]);
        let input = help("cargo keeper lock hangs the test run");
        assert_eq!(go(&w, &input, true), Some("offered"));
        let row = &rows(&w)[0];
        assert_eq!(row["data"]["candidates"], json!(["zz-0002"]));
        let offer = std::fs::read_to_string(w.friction.join(format!("{SID}.offer"))).unwrap();
        assert!(
            offer.contains("zz-0002") && offer.contains("--friction"),
            "{offer}"
        );
        // The same fingerprint from a new row source still debounces in the ledger.
        let ledger = Ledger::read(&w.friction, SID);
        assert!(ledger.knows(&fingerprint(Origin::Help, &input.key)));
    }

    #[test]
    fn the_cap_and_the_debounce_refuse_before_any_read() {
        let w = world(&[]);
        let mut ledger = Ledger::default();
        ledger
            .seen
            .push(json!(fingerprint(Origin::Help, "seen before signal")));
        ledger.write(&w.friction, SID).unwrap();
        assert_eq!(go(&w, &help("seen before signal"), true), Some("debounce"));
        for i in 0..MAX_OFFERS {
            ledger.offers.push(json!({"fingerprint": format!("f{i}")}));
        }
        ledger.write(&w.friction, SID).unwrap();
        assert_eq!(go(&w, &help("a fresh signal arrives"), true), Some("cap"));
        assert!(!w.friction.join(format!("{SID}.offer")).exists());
    }

    #[test]
    fn a_clean_review_round_is_not_friction() {
        let w = world(&[]);
        let sinks = Sinks {
            project: &w.events,
            global: &w.events,
        };
        let mut pr = PrRead {
            harness_session: Some(SID),
            node: Some("zz-0001"),
            harness: Some("claude"),
            number: 7,
            head_sha: "abcdef0123456789",
            ci_failure: false,
            failing_checks: &[],
            rounds_used: 1,
            rounds_max: 2,
            unaddressed_blocking: 0,
            hard_blocker: false,
        };
        let off =
            |s: &Sinks<'_>, i: &FrictionInput<'_>| record_with(s, i, &w.graph, &w.friction, false);
        on_pr_read_with(&sinks, &pr, off);
        assert!(rows(&w).is_empty());
        pr.hard_blocker = true;
        on_pr_read_with(&sinks, &pr, off);
        let rows = rows(&w);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["data"]["origin"], "review_round");
        assert_eq!(rows[0]["data"]["stage"], "review");
    }

    #[test]
    fn too_few_tokens_is_empty_and_marks_the_fingerprint_seen() {
        let w = world(&[]);
        let input = help("oops");
        assert_eq!(go(&w, &input, true), Some("empty"));
        assert!(Ledger::read(&w.friction, SID).knows(&fingerprint(Origin::Help, "oops")));
    }
}
