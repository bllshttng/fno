//! `fno-agents needs` - the needs-me-queue events-fold leg.
//!
//! A pure read-time fold over events.jsonl producing the two event-derived
//! attention reasons the mux client cannot see from live badges alone:
//!   - `review_wedged`: a green OPEN PR whose loop keeps blocking on review that
//!     will not self-heal (the codex usage-limit lesson - surface it EARLY).
//!   - `budget_stop`: a loop that terminated on `Budget` / `NoProgress` and
//!     needs a human to re-arm.
//!
//! Unlike [`crate::digest`] (which folds ONE session's activity), this folds
//! ALL sessions and emits at most one [`NeedItem`] per session - the worst
//! reason its latest events imply. Each item is resolved to a node/name/title
//! via the ledger bridge so the client can join it to a sideline row.
//!
//! The two event envelopes (Python nested `{"type","data":{...}}` and the
//! retired Rust flat `{"kind",...}`) are both read, same as digest. Read-only:
//! the command writes nothing and is rerunnable at will.

use crate::paths::AgentsHome;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Default fold window when `--since-epoch` is absent: the last 24h.
pub const DEFAULT_WINDOW_SECS: u64 = 24 * 60 * 60;

/// `fires` floor for `review_wedged`: the loop must have re-checked at least
/// this many times before a green-PR block counts as wedged (a fresh block
/// during a normal review wait is not yet a wedge). Hardcoded heuristic, not a
/// config knob - tune the const if it misfires (ponytail: no config for a value
/// that never changes); a hidden `--fires-floor` overrides it for tests.
pub const DEFAULT_FIRES_FLOOR: u64 = 2;

/// Below this age, a pile of unharvested carve-outs or stale claims is not yet
/// a needs-me item - only a pile that has actually gone stale belongs in the
/// queue (measured 44 carve-out rows, oldest 29 days; x-bbbb measured
/// 573 claim files, oldest specimens 56-75 days). Both legs are "somebody
/// should look at this pile" signals, read from durable on-disk state rather
/// than a recent event, so neither is windowed by `since` at all.
const DECISION_STALE_FLOOR_SECS: u64 = 7 * 24 * 60 * 60;

/// One reason a session needs a human, resolved and ready to render. `kind` is
/// a stable string (`review_wedged` | `budget_stop`) the client maps to its own
/// severity enum; the fold does not rank (the client owns the full 6-kind
/// order, of which this leg populates two).
///
/// A kind the client does not map is dropped from the operator view by its
/// `_ => continue` arm, which is how `mail_delivery_miss` stays out of the
/// panel while still flowing into the journal for a wake consumer to read.
/// Emitting a new kind here is therefore a quiet change on the operator
/// surface, never a loud one: to SHOW a new kind, map it there too.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NeedItem {
    pub kind: String,
    pub session_id: String,
    /// The graph node id, when the ledger resolves one.
    pub node: Option<String>,
    /// A display name to join a sideline row on (worktree basename or node id).
    pub name: Option<String>,
    pub title: Option<String>,
    /// The deciding event's timestamp (the wedge's loop_check, or the stop).
    pub ts: String,
    /// A one-line human reason.
    pub evidence: String,
    /// Does this item's node hold a live (or suspect) claim? Stamped by the IO
    /// layer, not the pure fold. The client renders an item that joins no roster
    /// row only when it is `live`, so a dead session's stale stop never nags.
    pub live: bool,
}

/// Read `<field>` regardless of envelope: nested under `/data` (unified) or
/// top-level (retired flat). Mirrors [`crate::digest`] - kept local so this
/// module stays a self-contained leaf (: no function-local cross-imports).
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.get("data")
        .and_then(|d| d.get(key))
        .or_else(|| v.get(key))
}

fn event_kind(v: &Value) -> Option<&str> {
    v.get("type")
        .and_then(|t| t.as_str())
        .or_else(|| v.get("kind").and_then(|k| k.as_str()))
}

fn event_ts(v: &Value) -> &str {
    v.get("ts").and_then(|t| t.as_str()).unwrap_or("")
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    field(v, key).and_then(|f| f.as_str())
}

/// The basename of a `/`-separated path.
fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Parse an RFC3339-ish ts to epoch seconds, tolerating the ledger's non-strict
/// (`...isoformat()`, no Z, fractional) forms. Same lenient parse as digest.
fn to_epoch_lenient(ts: &str) -> Option<u64> {
    crate::state::rfc3339_like_to_secs(ts).or_else(|| {
        let secs = ts.get(..19)?;
        crate::state::rfc3339_like_to_secs(&format!("{secs}Z"))
    })
}

/// A row's ts is in-window when it parses to `>= since` (an unparseable ts is
/// included - never silently dropped by the bound).
fn in_window(ts: &str, since: u64) -> bool {
    to_epoch_lenient(ts).is_none_or(|secs| secs >= since)
}

/// The latest loop_check state observed for a session. "Latest" is by
/// `(epoch, seq)`, NOT file order: events from a project + global events.jsonl
/// are concatenated, so a later-in-file line can be older in time; comparing by
/// parsed epoch (with a monotonic fold `seq` tiebreak for same-second events)
/// keeps the truly newest state per kind.
#[derive(Default, Clone)]
struct LoopState {
    decision: String,
    intent: String,
    ci: String,
    pr_state: String,
    reviewed: bool,
    fires: u64,
    ts: String,
    epoch: u64,
    seq: usize,
}

/// The latest termination observed for a session (same `(epoch, seq)` ordering).
#[derive(Default, Clone)]
struct TermState {
    reason: String,
    ts: String,
    epoch: u64,
    seq: usize,
}

#[derive(Default)]
struct SessionAcc {
    latest_loop: Option<LoopState>,
    latest_term: Option<TermState>,
}

/// The pure fold. `events_raw` is the newline-joined concatenation of every
/// events.jsonl source; `ledger_raw` is ledger.json. Emits one [`NeedItem`] per
/// qualifying session, sorted `(ts, session_id)` for deterministic output.
pub fn fold(events_raw: &str, ledger_raw: &str, since: u64, fires_floor: u64) -> Vec<NeedItem> {
    let mut sessions: HashMap<String, SessionAcc> = HashMap::new();
    // Monotonic fold position, breaking same-second ties in true stream order.
    let mut seq = 0usize;
    // mail_escalation events carry no session_id (they are about mail between
    // agents, not a target session), so they cannot enter the session-keyed
    // accumulator and the session gate below would drop them. They get their own
    // per-recipient accumulator (latest (epoch, seq) wins) and render
    // squadless-live in the client.
    // Keyed on (recipient, kind), not recipient alone. A standing question and a
    // delivery miss to the same handle are different things, and the client
    // renders one while dropping the other, so collapsing them would let a later
    // miss erase an escalation a human still owes an answer to.
    let mut mail_escalations: HashMap<(String, String), (u64, usize, NeedItem)> = HashMap::new();
    // operator_question / operator_question_closed, keyed by question_id (not
    // per-recipient-latest-wins like mail_escalation above): several distinct
    // decisions can be open on the operator at once, so every open question_id
    // gets its own row. Mirrors `outstanding/core.py::read_open_questions`.
    let mut questions: HashMap<String, (u64, usize, NeedItem)> = HashMap::new();
    let mut closed_questions: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in events_raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue; // torn/malformed tail line: skip, never abort (digest precedent)
        };
        let ts = event_ts(&v);
        let kind = event_kind(&v);
        // operator_question/_closed are exempt from the `since` window: a
        // question does not expire (no auto-close), so time-windowing
        // it here would silently resurrect the exact failure that node fixed
        // for the SessionStart block - just inside the mux queue instead.
        let windowed = !matches!(
            kind,
            Some("operator_question") | Some("operator_question_closed")
        );
        if windowed && !in_window(ts, since) {
            continue;
        }
        if kind == Some("operator_question") {
            let Some(qid) = str_field(&v, "question_id") else {
                continue;
            };
            if !crate::question_ladder::envelope_on_board(&v) {
                questions.remove(qid);
                continue;
            }
            let question = str_field(&v, "question").unwrap_or("");
            let evidence = str_field(&v, "ask").unwrap_or(question);
            let node = str_field(&v, "node").map(str::to_string);
            let name = node
                .clone()
                .or_else(|| str_field(&v, "cwd").map(|c| basename(c).to_string()));
            // The item's identity is the question id, matching the fold key
            // and outstanding/core.py. An asking session_id in the row must
            // not displace it: `answer <id>` has to name something closable.
            let session_id = qid.to_string();
            let epoch = to_epoch_lenient(ts).unwrap_or(0);
            seq += 1;
            if questions
                .get(qid)
                .is_none_or(|(e, s, _)| (epoch, seq) >= (*e, *s))
            {
                questions.insert(
                    qid.to_string(),
                    (
                        epoch,
                        seq,
                        NeedItem {
                            kind: "operator_question".to_string(),
                            session_id,
                            node,
                            name,
                            title: None,
                            ts: ts.to_string(),
                            evidence: evidence.to_string(),
                            // Stamped always-live by stamp_liveness (no node claim).
                            live: false,
                        },
                    ),
                );
            }
            continue;
        }
        if kind == Some("operator_question_closed") {
            if let Some(qid) = str_field(&v, "question_id") {
                closed_questions.insert(qid.to_string());
            }
            continue;
        }
        // mail_escalation is folded before the session gate: it carries no
        // session_id (it is mail between agents), so the gate below would drop
        // it. One NeedItem per (recipient, kind), latest of that pair wins; the
        // kind comes from the row's reason.
        if kind == Some("mail_escalation") {
            let Some(recipient) = str_field(&v, "recipient") else {
                continue;
            };
            let reason = str_field(&v, "reason").unwrap_or("");
            let sender = str_field(&v, "sender").unwrap_or("");
            let summary = str_field(&v, "summary").unwrap_or("");
            // A reachable-miss is an agent-to-agent DELIVERY failure: the
            // recipient was reachable, the live inject missed, the durable copy
            // is queued. It needs a retry or a wake, not an operator decision,
            // so it gets its own kind and the client's `_ => continue` arm keeps
            // it out of the needs panel by construction rather than by a filter
            // someone can forget to apply. attended-miss stays a question:
            // there the operator IS the attended recipient.
            // Named need_kind, not kind: the enclosing `kind` is the EVENT
            // type, and shadowing it here reads as the same thing twice.
            let need_kind = if reason == "reachable-miss" {
                "mail_delivery_miss"
            } else {
                "mail_question"
            };
            let epoch = to_epoch_lenient(ts).unwrap_or(0);
            seq += 1;
            // Same (epoch, seq) ordering as the session accumulator: a cross-source
            // concat never lets an older line clobber a newer escalation.
            let acc_key = (recipient.to_string(), need_kind.to_string());
            if mail_escalations
                .get(&acc_key)
                .is_none_or(|(e, s, _)| (epoch, seq) >= (*e, *s))
            {
                mail_escalations.insert(
                    acc_key,
                    (
                        epoch,
                        seq,
                        NeedItem {
                            kind: need_kind.to_string(),
                            // No target session; the recipient handle is the row's
                            // stable identity (id_key) and the roster join key.
                            session_id: recipient.to_string(),
                            node: None,
                            name: Some(recipient.to_string()),
                            title: None,
                            ts: ts.to_string(),
                            evidence: format!("{reason}: {sender} -> {recipient}: {summary}"),
                            // Stamped always-live by stamp_liveness (no node claim).
                            live: false,
                        },
                    ),
                );
            }
            continue;
        }
        let Some(sid) = str_field(&v, "session_id") else {
            continue; // an event with no session can't be joined to a row
        };
        if !matches!(
            kind,
            Some("loop_check") | Some("termination") | Some("loop_terminated")
        ) {
            continue;
        }
        let epoch = to_epoch_lenient(ts).unwrap_or(0);
        seq += 1;
        let acc = sessions.entry(sid.to_string()).or_default();
        match kind {
            Some("loop_check") => {
                // Keep the newest by (epoch, seq): a later-in-file but older-in-
                // time line (cross-source concat) never clobbers newer state.
                if acc
                    .latest_loop
                    .as_ref()
                    .is_none_or(|c| (epoch, seq) >= (c.epoch, c.seq))
                {
                    acc.latest_loop = Some(LoopState {
                        decision: str_field(&v, "decision").unwrap_or("").to_string(),
                        intent: str_field(&v, "intent").unwrap_or("").to_string(),
                        ci: str_field(&v, "ci").unwrap_or("").to_string(),
                        pr_state: str_field(&v, "pr_state").unwrap_or("").to_string(),
                        reviewed: field(&v, "reviewed")
                            .and_then(|r| r.as_bool())
                            .unwrap_or(false),
                        fires: field(&v, "fires").and_then(|f| f.as_u64()).unwrap_or(0),
                        ts: ts.to_string(),
                        epoch,
                        seq,
                    });
                }
            }
            _ => {
                if acc
                    .latest_term
                    .as_ref()
                    .is_none_or(|c| (epoch, seq) >= (c.epoch, c.seq))
                {
                    acc.latest_term = Some(TermState {
                        reason: str_field(&v, "reason").unwrap_or("").to_string(),
                        ts: ts.to_string(),
                        epoch,
                        seq,
                    });
                }
            }
        }
    }

    let ledger = LedgerIndex::parse(ledger_raw);
    let mut items: Vec<NeedItem> = Vec::new();
    for (sid, acc) in &sessions {
        if let Some((kind, ts, evidence)) = classify(acc, fires_floor) {
            let (node, name, title) = ledger.resolve(sid);
            items.push(NeedItem {
                kind: kind.to_string(),
                session_id: sid.clone(),
                node,
                name,
                title,
                ts,
                evidence,
                live: false, // stamped by the IO layer; the fold stays pure
            });
        }
    }
    for (_, (_, _, item)) in mail_escalations {
        items.push(item);
    }
    // Operator questions rank newest-first, matching
    // outstanding/core.py::read_open_questions: an ask filed minutes ago is
    // the one a human can still act on. Pre-ordered here by (epoch, seq)
    // descending - the fold's own total order, exact across a same-second
    // stream where a ts string compare is not - so the comparator's
    // same-(ts, session) ties below resolve through the stable sort into this
    // push order instead of HashMap order.
    let mut open_questions: Vec<_> = questions
        .into_iter()
        .filter(|(qid, _)| !closed_questions.contains(qid))
        .collect();
    open_questions.sort_by(|a, b| {
        let (epoch_a, seq_a, _) = a.1;
        let (epoch_b, seq_b, _) = b.1;
        epoch_b.cmp(&epoch_a).then_with(|| seq_b.cmp(&seq_a))
    });
    items.extend(open_questions.into_iter().map(|(_, (_, _, item))| item));
    // `kind` is part of the sort key because it is part of the mail accumulator's
    // key: one recipient can now yield both a mail_question and a
    // mail_delivery_miss, and for mail rows session_id IS the recipient, so those
    // two tie on (ts, session_id). `sort_by` is stable, so a tie would fall back
    // to push order, which comes from HashMap iteration and is randomized per
    // process. That makes `needs --json` reorder run to run and anything
    // diffing it flaky.
    items.sort_by(|a, b| {
        // Operator questions rank newest-first among themselves; the
        // direction flip is total only when BOTH sides are questions (mixing
        // it into the cross-kind arm would break sort_by's total-order
        // requirement). Every other pair keeps stream order.
        let is_question = |i: &NeedItem| i.kind == "operator_question";
        match (is_question(a), is_question(b)) {
            (true, true) => {
                b.ts.cmp(&a.ts)
                    .then_with(|| b.session_id.cmp(&a.session_id))
            }
            _ => {
                a.ts.cmp(&b.ts)
                    .then_with(|| a.session_id.cmp(&b.session_id))
                    .then_with(|| a.kind.cmp(&b.kind))
            }
        }
    });
    items
}

/// The reason a session's latest events imply, or `None` when nothing needs a
/// human. Termination is terminal, so a session that ended on `Budget` /
/// `NoProgress` is a `budget_stop`; any other termination (DonePRGreen, NoWork,
/// Interrupted, ...) means nothing needs me. A still-live loop whose latest
/// check is a green OPEN unreviewed block past the fires floor is `review_wedged`
/// - a later `allow` or a termination clears it (the latest event wins).
fn classify(acc: &SessionAcc, fires_floor: u64) -> Option<(&'static str, String, String)> {
    // Order by (epoch, seq), the same key the accumulator kept: epoch first (so
    // a fractional Python-isoformat stop is not misordered against a Z loop_check
    // by a lexical string compare), then the monotonic fold seq so a same-second
    // loop_check that RE-ARMS after a termination wins over the stop (codex P2).
    let terminated = match (&acc.latest_term, &acc.latest_loop) {
        (Some(t), Some(l)) => (t.epoch, t.seq) >= (l.epoch, l.seq),
        (Some(_), None) => true,
        (None, _) => false,
    };
    if terminated {
        let t = acc.latest_term.as_ref()?;
        return match t.reason.as_str() {
            "Budget" | "NoProgress" => Some((
                "budget_stop",
                t.ts.clone(),
                format!("loop stopped: {}", t.reason),
            )),
            _ => None,
        };
    }
    let l = acc.latest_loop.as_ref()?;
    // `intent == "promise"` is load-bearing: loopcheck Step 5 also blocks a
    // still-WORKING session that has opened a green PR with `intent:"none"`
    // (no promise, no backstop) - that is not wedged on review, it just has not
    // promised yet. Only a promise-intent block on a green OPEN unreviewed PR
    // that keeps re-firing is a review wedge (codex P2).
    let wedged = l.decision == "block"
        && l.intent == "promise"
        && l.ci == "SUCCESS"
        && l.pr_state == "OPEN"
        && !l.reviewed
        && l.fires >= fires_floor;
    if wedged {
        return Some((
            "review_wedged",
            l.ts.clone(),
            format!("green PR wedged on review ({} checks)", l.fires),
        ));
    }
    None
}

/// A minimal ledger index: maps a session id to its node/name/title. Reuses the
/// digest bridge's match keys (scalar `session_id`, `sessions[]` membership,
/// `graph_node_id`, `worktree`/`root_path` basename) but inverted - given a
/// session, return its display identity.
struct LedgerIndex {
    entries: Vec<Value>,
}

impl LedgerIndex {
    fn parse(ledger_raw: &str) -> Self {
        let entries = serde_json::from_str::<Value>(ledger_raw)
            .ok()
            .and_then(|root| {
                root.get("entries")
                    .and_then(|e| e.as_array())
                    .or_else(|| root.as_array())
                    .cloned()
            })
            .unwrap_or_default();
        LedgerIndex { entries }
    }

    fn entry_has_session(entry: &Value, sid: &str) -> bool {
        if entry.get("session_id").and_then(|s| s.as_str()) == Some(sid) {
            return true;
        }
        entry
            .get("sessions")
            .and_then(|s| s.as_array())
            .is_some_and(|arr| arr.iter().any(|s| s.as_str() == Some(sid)))
    }

    /// `(node, name, title)` for a session. `name` prefers the worktree basename
    /// (what a sideline orphan row carries), else the node id. All `None` when
    /// unresolved - the client renders a session-id-only squadless row then.
    fn resolve(&self, sid: &str) -> (Option<String>, Option<String>, Option<String>) {
        let Some(entry) = self
            .entries
            .iter()
            .find(|e| Self::entry_has_session(e, sid))
        else {
            return (None, None, None);
        };
        let node = entry
            .get("graph_node_id")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let title = entry
            .get("title")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let name = ["worktree", "root_path"]
            .iter()
            .find_map(|k| entry.get(*k).and_then(|v| v.as_str()))
            .map(|p| basename(p).to_string())
            .or_else(|| node.clone());
        (node, name, title)
    }
}

struct NeedsArgs {
    since_epoch: Option<u64>,
    fires_floor: u64,
    json: bool,
    items: bool,
    clear_settled: bool,
    answer: Option<String>,
    option: Option<u32>,
    words: Option<String>,
    done: bool,
    delegate: bool,
    archive: Vec<String>,
    sink: Option<String>,
    email: Option<String>,
    events_override: Vec<PathBuf>,
    ledger_override: Option<PathBuf>,
}

fn parse_args(rest: &[String]) -> Result<NeedsArgs, String> {
    let mut since_epoch: Option<u64> = None;
    let mut fires_floor = DEFAULT_FIRES_FLOOR;
    let mut json = false;
    let mut items = false;
    let mut clear_settled = false;
    let mut answer: Option<String> = None;
    let mut option: Option<u32> = None;
    let mut words: Option<String> = None;
    let mut done = false;
    let mut delegate = false;
    let mut archive: Vec<String> = Vec::new();
    let mut sink: Option<String> = None;
    let mut email: Option<String> = None;
    let mut events_override: Vec<PathBuf> = Vec::new();
    let mut ledger_override: Option<PathBuf> = None;

    let mut it = expand_eq(rest).into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--since-epoch" => {
                since_epoch = Some(
                    it.next()
                        .and_then(|v| v.parse::<u64>().ok())
                        .ok_or("--since-epoch needs a non-negative integer")?,
                )
            }
            "--fires-floor" => {
                fires_floor = it
                    .next()
                    .and_then(|v| v.parse::<u64>().ok())
                    .ok_or("--fires-floor needs a non-negative integer")?
            }
            "--json" | "-J" => json = true,
            "--items" => items = true,
            "--clear-settled" => clear_settled = true,
            "--answer" => answer = Some(it.next().ok_or("--answer needs an item id")?),
            "--option" => {
                option = Some(
                    it.next()
                        .and_then(|v| v.parse::<u32>().ok())
                        .filter(|n| *n > 0)
                        .ok_or("--option needs a positive option number")?,
                )
            }
            "--words" => words = Some(it.next().ok_or("--words needs the answer text")?),
            "--done" => done = true,
            "--delegate" => delegate = true,
            "--archive" => archive.push(it.next().ok_or("--archive needs an item id")?),
            "--sink" => sink = Some(it.next().ok_or("--sink needs a sink name")?),
            "--email" => email = Some(it.next().ok_or("--email needs a login")?),
            "--events" => {
                events_override.push(PathBuf::from(it.next().ok_or("--events needs a path")?))
            }
            "--ledger" => {
                ledger_override = Some(PathBuf::from(it.next().ok_or("--ledger needs a path")?))
            }
            other => return Err(format!("unknown needs flag: {other}")),
        }
    }
    Ok(NeedsArgs {
        since_epoch,
        fires_floor,
        json,
        items,
        clear_settled,
        answer,
        option,
        words,
        done,
        delegate,
        archive,
        sink,
        email,
        events_override,
        ledger_override,
    })
}

/// Split `--key=value` into `["--key","value"]`.
fn expand_eq(rest: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(rest.len());
    for a in rest {
        if let Some(eq) = a.find('=') {
            if a.starts_with("--") && eq > 2 {
                out.push(a[..eq].to_string());
                out.push(a[eq + 1..].to_string());
                continue;
            }
        }
        out.push(a.clone());
    }
    out
}

/// The question journal family: the cwd's space journal + global
/// `~/.fno/events.jsonl` + `~/.fno/questions.jsonl`. Every reader of the
/// question family folds these same three paths as a UNION (a question can
/// land in any one of them), so the list lives here and nowhere else.
pub(crate) fn question_journals(fno_dir: &Path, cwd: &Path) -> Vec<PathBuf> {
    vec![
        crate::paths::space_dir(cwd).join("events.jsonl"),
        fno_dir.join("events.jsonl"),
        fno_dir.join("questions.jsonl"),
    ]
}

/// Default event/ledger sources: the repo's space journal + global
/// `~/.fno/events.jsonl` + `~/.fno/questions.jsonl` + `~/.fno/ledger.json`.
fn default_sources(home: &AgentsHome, cwd: &Path) -> (Vec<PathBuf>, PathBuf) {
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let events = question_journals(&fno_dir, cwd);
    let ledger = fno_dir.join("ledger.json");
    (events, ledger)
}

/// The two kinds that mark a question open or answered, wherever the family
/// of question readers folds them.
pub(crate) const QUESTION_TYPES: &[&str] = &["operator_question", "operator_question_closed"];

/// The kinds the attention projection folds: the question pair plus the
/// answer and delivery rows that flip an item to answered and feed the
/// panel's answered rows.
pub(crate) const PROJECTION_TYPES: &[&str] = &[
    "operator_question",
    "operator_question_closed",
    "attention_answer",
    "attention_delivery",
];

/// The types the `--items` read folds unwindowed: the question pair plus
/// answer rows. Deliveries ride the TTL window below; the events-leg kinds
/// (mail_escalation, loop_check, termination, loop_terminated) belong to
/// `fold`, never this read.
pub(crate) const ITEMS_TYPES: &[&str] = &[
    "operator_question",
    "operator_question_closed",
    "attention_answer",
];

/// One store's read outcome for the `--items` sources readout. A store that
/// exists and fails to read is `readable: false`; the output never shows an
/// empty `items` as a clean read.
#[derive(Serialize)]
struct SourceRead {
    store: String,
    readable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// The `--items` leg: the attention projection with named sources. Every
/// surface that delivers or answers an item reads this one projection, so a
/// store an operator cannot read is a named source, never a silent empty list.
fn run_items(home: &AgentsHome, cwd: &Path) -> i32 {
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let mut sources: Vec<SourceRead> = Vec::new();
    let mut journals_raw = String::new();
    // Deliveries older than the answered panel's TTL can never surface in
    // the fold, so the read bounds them by TTL plus slack instead of pulling
    // the fleet's full delivery history (112k rows / 36.8 MB on the live
    // store measured 2026-09-28).
    let delivery_since_ms =
        crate::claims::now_ms() - (crate::attention::ANSWERED_TTL_S as i64 + 900) * 1000;
    for path in question_journals(&fno_dir, cwd) {
        let store = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("journal")
            .to_string();
        let mut content = String::new();
        let mut error: Option<String> = None;
        let queries = [
            crate::event_store::EventQuery::of_types(ITEMS_TYPES),
            crate::event_store::EventQuery {
                since_ms: Some(delivery_since_ms),
                ..crate::event_store::EventQuery::of_types(&["attention_delivery"])
            },
        ];
        for q in queries {
            match crate::event_store::journal_text_checked(&path, &q) {
                Ok(part) => {
                    content.push_str(&part);
                    if !part.ends_with('\n') {
                        content.push('\n');
                    }
                }
                Err(e) => {
                    error.get_or_insert(e.to_string());
                }
            }
        }
        match error {
            None => {
                sources.push(SourceRead {
                    store,
                    readable: true,
                    error: None,
                });
                journals_raw.push_str(&content);
            }
            Some(e) => sources.push(SourceRead {
                store,
                readable: false,
                error: Some(e),
            }),
        }
    }
    // Escalation notes: the (slug, text) pairs the projection folds.
    let notes_dir = crate::escalation::dir(cwd);
    let mut notes: Vec<(String, String)> = Vec::new();
    match std::fs::read_dir(&notes_dir) {
        Ok(entries) => {
            let mut paths: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
                .collect();
            paths.sort();
            for path in paths {
                let slug = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("note")
                    .to_string();
                match std::fs::read_to_string(&path) {
                    Ok(text) => notes.push((slug, text)),
                    Err(e) => sources.push(SourceRead {
                        store: format!("escalations/{slug}"),
                        readable: false,
                        error: Some(e.to_string()),
                    }),
                }
            }
            sources.push(SourceRead {
                store: "escalations".to_string(),
                readable: true,
                error: None,
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            sources.push(SourceRead {
                store: "escalations".to_string(),
                readable: true,
                error: None,
            });
        }
        Err(e) => sources.push(SourceRead {
            store: "escalations".to_string(),
            readable: false,
            error: Some(e.to_string()),
        }),
    }
    // The user lane file.
    let lane_path = crate::org_board::scope::operator_lane_path(cwd);
    let lane_text = match std::fs::read_to_string(&lane_path) {
        Ok(text) => {
            sources.push(SourceRead {
                store: "lane".to_string(),
                readable: true,
                error: None,
            });
            text
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            sources.push(SourceRead {
                store: "lane".to_string(),
                readable: true,
                error: None,
            });
            String::new()
        }
        Err(e) => {
            sources.push(SourceRead {
                store: "lane".to_string(),
                readable: false,
                error: Some(e.to_string()),
            });
            String::new()
        }
    };
    let mut items = crate::attention::project(
        &journals_raw,
        &notes,
        &lane_text,
        crate::claims::now_ms() as u64 / 1000,
    );
    if let Ok(registry) = crate::state::load_registry(&home.registry_json()) {
        crate::attention::attach_reach(&mut items, &registry);
    }
    // The settled flag shares the sweep's question fold, rules and all. An
    // unreadable graph closes nothing.
    let facts = crate::question_sweep::read_closed_rung_facts(cwd, home);
    let settled: std::collections::HashSet<String> = settled_questions(&facts.raw, &facts)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    mark_settled(&mut items, &settled);
    for item in items.iter_mut() {
        if let Some(node) = item.node.clone() {
            item.node_title = facts.titles.get(&node).cloned();
            item.node_stage = facts.statuses.get(&node).cloned();
        }
    }
    let as_of = now_secs();
    let answered = crate::attention::answered(&journals_raw, as_of);
    let payload = json!({
        "as_of": as_of,
        "sources": sources,
        "items": items,
        "answered": answered,
    });
    println!(
        "{}",
        serde_json::to_string(&payload).expect("serializing an owned value never fails")
    );
    let any_unreadable = payload["sources"]
        .as_array()
        .map(|a| a.iter().any(|s| s.get("readable") == Some(&json!(false))))
        .unwrap_or(false);
    if any_unreadable {
        return 1;
    }
    0
}

/// Mark only open question/pin items the canonical node-closed fold returned.
fn mark_settled(
    items: &mut [crate::attention::AttentionItem],
    settled: &std::collections::HashSet<String>,
) {
    for item in items.iter_mut() {
        if item.state != "open" || !matches!(item.kind.as_str(), "question" | "pin") {
            continue;
        }
        item.settled = settled.contains(&item.id);
    }
}

/// The `--clear-settled` door: close every still-open question whose subject
/// resolved (a merged PR it names, a newer head in its family, or a terminal
/// node rung), right now, through the same close rows the daemon sweep
/// writes, so the attention arm's vault-page write picks them up on its next
/// beat. The bell's Clear all calls this; a graph it cannot read closes
/// nothing.
fn run_clear_settled(home: &AgentsHome, cwd: &Path) -> i32 {
    let facts = crate::question_sweep::read_facts_with_prs(cwd, home);
    let settled = settled_questions(&facts.raw, &facts);
    let now = crate::claims::now_ms() / 1000;
    let (cleared, errors) = crate::question_sweep::close_questions(
        &crate::provider_cap::questions_path(home),
        &settled,
        now,
        "clear-settled",
    );
    let emitter = crate::events::EventEmitter::new(home.events_jsonl(), "daemon");
    let _ = emitter.emit(
        "question_sweep",
        &json!({
            "closed": cleared.len(),
            "tasks_closed": 0,
            "legacy_moved": 0,
            "outcome": if errors.is_empty() { "ok" } else { "partial" },
            "closed_by": "clear-settled",
        }),
    );
    println!(
        "{}",
        json!({
            "cleared": cleared,
            "errors": errors,
            "as_of": crate::claims::now_ms() / 1000,
        })
    );
    i32::from(!errors.is_empty())
}

/// The `--answer` door: record one durable `attention_answer` row for an open
/// item, first answer wins, and print a one-line JSON receipt. The mux is the
/// caller this exists for, but any process gets the same receipt. The arm owns
/// delivery (the clear, the resume, the team escalation) on its next beat;
/// the door never delivers.
fn run_answer(home: &AgentsHome, cwd: &Path, args: &NeedsArgs, item_id: &str) -> i32 {
    let picks = [
        args.option.is_some(),
        args.words.is_some(),
        args.done,
        args.delegate,
    ];
    if picks.iter().filter(|p| **p).count() != 1 {
        eprintln!(
            "fno-agents: needs --answer needs exactly one of --option <n>, --words <text>, --done, --delegate"
        );
        return 2;
    }
    // The mux stamps no login. The command page stamps the invited login it
    // authenticated, and a login is legal only there.
    let (sink, email) = match (args.sink.as_deref(), args.email.as_deref()) {
        (Some("mux"), None) => ("mux", None),
        (Some("mux"), Some(_)) => {
            eprintln!("fno-agents: needs --email is legal only with --sink command_page");
            return 2;
        }
        (Some("command_page"), Some(login)) if !login.trim().is_empty() => {
            if args.delegate {
                eprintln!("fno-agents: --delegate stays in the mux; the page cannot delegate");
                return 2;
            }
            ("command_page", Some(login.trim()))
        }
        (Some("command_page"), _) => {
            eprintln!("fno-agents: needs --sink command_page requires --email <login>");
            return 2;
        }
        (other, _) => {
            eprintln!(
                "fno-agents: needs --answer accepts only --sink mux or command_page (got {})",
                other.unwrap_or("none")
            );
            return 2;
        }
    };
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    // The same projection `run_items` folds, so a closed or already-answered
    // item refuses for the reason the panel will show. An unreadable store
    // makes the projection incomplete; the refusal names it instead of
    // reading as a clean not-found.
    let (items, unreadable, _) = crate::attention_arm::read_items_at(&fno_dir, cwd);
    let Some(item) = items.iter().find(|i| i.id == item_id) else {
        if unreadable.is_empty() {
            eprintln!("fno-agents: not found: {item_id}");
        } else {
            eprintln!(
                "fno-agents: not found: {item_id} (some stores unreadable: {})",
                unreadable.join("; ")
            );
        }
        return 2;
    };
    if item.state != "open" {
        eprintln!("fno-agents: not open: {item_id} (state {})", item.state);
        return 2;
    }
    let answer = if let Some(n) = args.option {
        if n as usize > item.options.len() {
            eprintln!(
                "fno-agents: option {n} is outside 1..={} for {item_id}",
                item.options.len()
            );
            return 2;
        }
        crate::attention_file::FileAnswer::Option(n)
    } else if args.done {
        if item.kind != "pin" {
            eprintln!("fno-agents: only a pin takes --done: {item_id} is a question");
            return 2;
        }
        crate::attention_file::FileAnswer::Done
    } else if args.delegate {
        // A copy's team ladder lives on the asking machine, so delegating
        // here would record an act this machine can never deliver.
        if let Some(host) = &item.host {
            eprintln!("fno-agents: {item_id} was asked on {host}; delegate it there");
            return 2;
        }
        if let Err(reason) = delegable(item, cwd) {
            eprintln!("fno-agents: {reason}");
            return 2;
        }
        crate::attention_file::FileAnswer::Delegate
    } else {
        crate::attention_file::FileAnswer::Words(args.words.clone().unwrap_or_default())
    };
    let attested_by = email.map_or_else(|| "mux".to_string(), |e| format!("command_page:{e}"));
    match crate::attention_arm::append_answer_row_stamped(
        home,
        item_id,
        sink,
        &answer,
        "sink",
        "needs_door",
        &attested_by,
        email,
    ) {
        Ok((_, superseded)) => {
            println!(
                "{}",
                answer_receipt(item_id, item.host.as_deref(), superseded)
            );
            0
        }
        Err(e) => {
            eprintln!("fno-agents: needs: {e}");
            1
        }
    }
}

/// The door's stdout receipt. `carries_to` names the asking machine's host
/// when the item is a copy, so the operator sees the answer will travel.
fn answer_receipt(item_id: &str, host: Option<&str>, superseded: bool) -> Value {
    let mut receipt = json!({"recorded": true, "item_id": item_id, "superseded": superseded});
    if let Some(host) = host {
        receipt["carries_to"] = json!(host);
    }
    receipt
}

/// Whether the agents may decide `item` for the user: a question (a pin has
/// nothing to decide), recorded reversible as `yes` or `costly` (an
/// irreversible or unstated call stays the user's), and a live team over
/// its node to decide it (else the question would close with nobody on it).
fn delegable(item: &crate::attention::AttentionItem, cwd: &Path) -> Result<(), String> {
    let id = &item.id;
    if item.kind == "pin" {
        return Err(format!("a pin has no delegate: {id}"));
    }
    match item.reversible.as_deref().map(str::trim) {
        Some("yes" | "costly") => {}
        Some("no") => return Err(format!("irreversible: the user decides {id}")),
        _ => return Err(format!("reversibility not recorded: the user decides {id}")),
    }
    if crate::attention_reply::team_holder(item, cwd).is_none() {
        let node = item.node.as_deref().unwrap_or("its node");
        return Err(format!("no live team covers {node}; answer it yourself"));
    }
    Ok(())
}

/// The `--archive` door: withdraw open items with no answer through the
/// in-process clear core, the same close `fno inbox outstanding clear` writes.
/// An id that is not open is refused by name and closes nothing; the rest
/// close. Prints one JSON receipt. `caller` is `None` outside tests, so the
/// clear resolves who is at the door from the process itself.
fn run_archive(
    home: &AgentsHome,
    cwd: &Path,
    args: &NeedsArgs,
    caller: Option<crate::question_clear::ClearCaller>,
) -> i32 {
    if args.sink.as_deref() != Some("mux") {
        eprintln!("fno-agents: needs --archive accepts only --sink mux");
        return 2;
    }
    let fno_dir = home
        .root()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(".fno"));
    let (items, _, _) = crate::attention_arm::read_items_at(&fno_dir, cwd);
    let mut lines = Vec::new();
    let mut ids = Vec::new();
    for id in &args.archive {
        match items.iter().find(|i| &i.id == id) {
            None => lines.push(format!("not found: {id}")),
            Some(i) if i.state != "open" => {
                lines.push(format!("not open: {id} (state {})", i.state))
            }
            Some(_) => ids.push(id.clone()),
        }
    }
    let refused = lines.len();
    let (mut code, mut archived) = (0, 0);
    if !ids.is_empty() {
        let answer = crate::question_clear::run_clear(&crate::question_clear::ClearRequest {
            ids,
            answer: None,
            cap: crate::question_intake::QUESTION_CAP,
            provenance: json!({}),
            closed_by: Some("mux".to_string()),
            caller,
            journal_path: crate::paths::space_dir(cwd).join("events.jsonl"),
            index_path: fno_dir.join("questions.jsonl"),
            decisions_path: fno_dir.join("decisions.jsonl"),
            graph: crate::backlog::settings::graph_path(),
            repo_root: cwd.to_path_buf(),
        });
        code = answer.exit_code;
        archived = answer.closed.len();
        lines.extend(answer.lines);
    }
    println!("{}", json!({"archived": archived, "lines": lines}));
    if refused > 0 {
        for line in &lines[..refused] {
            eprintln!("fno-agents: {line}");
        }
        if code == 0 {
            code = 2;
        }
    }
    code
}

/// One held node: the node an open question blocks, that question, and when
/// it was asked.

#[derive(Debug, Clone, Serialize)]
pub(crate) struct HeldRow {
    pub(crate) node: String,
    pub(crate) question_id: String,
    /// The question's first line, capped at 80 chars for rendering.
    pub(crate) question: String,
    pub(crate) ts: String,
    pub(crate) epoch: u64,
}

/// The held map: node id -> the open question id that holds it. A node is
/// HELD when an OPEN `operator_question` row names it in `data.blocks`; the
/// oldest such question wins so every reader names the same one. A closed
/// question never holds, and a question with no `blocks` array holds nothing.
pub(crate) fn held_nodes(journals: &[PathBuf]) -> std::collections::BTreeMap<String, String> {
    held_rows(journals)
        .into_iter()
        .map(|r| (r.node, r.question_id))
        .collect()
}

/// The fail-open door over [`held_nodes`]: the fold resolves the state root,
/// which panics in a process with no declared hermetic root, and no caller of
/// a held read may die on it - the map it cannot read is an empty one.
pub(crate) fn held_map(fno_dir: &Path, cwd: &Path) -> std::collections::BTreeMap<String, String> {
    std::panic::catch_unwind(|| held_nodes(&question_journals(fno_dir, cwd))).unwrap_or_default()
}

/// The held rows: one per blocked node, oldest question first. Journals fold
/// in order without concatenation: a space journal plus the global pair
/// reaches 53MB here, and the raw carrier was the whole cost.
fn held_rows(journals: &[PathBuf]) -> Vec<HeldRow> {
    let mut rows = HeldFold::default();
    for path in journals {
        let content = crate::event_store::journal_text(path, crate::needs::QUESTION_TYPES);
        rows.absorb(&content);
    }
    rows.finish()
}

/// The asked/closed fold over question journals, in journal order. Latest
/// ask of a qid wins (journal order, mirroring
/// `scan_unrecorded_decisions`); the oldest OPEN ask per node then wins.
#[derive(Default)]
struct HeldFold {
    asked: HashMap<String, (u64, String, Value)>,
    closed: std::collections::HashSet<String>,
}

impl HeldFold {
    fn absorb(&mut self, raw: &str) {
        for line in raw.lines() {
            if line.trim().is_empty() || !line.contains("operator_question") {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue; // torn/malformed tail line: skip, never abort
            };
            let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
            let data = v
                .get("data")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            match kind {
                "operator_question" => {
                    let Some(qid) = data.get("question_id").and_then(Value::as_str) else {
                        continue;
                    };
                    let ts_env = v.get("ts").and_then(Value::as_str).unwrap_or("");
                    let epoch = to_epoch_lenient(ts_env).unwrap_or(0);
                    self.asked
                        .insert(qid.to_string(), (epoch, ts_env.to_string(), data));
                }
                "operator_question_closed" => {
                    if let Some(qid) = data.get("question_id").and_then(Value::as_str) {
                        self.closed.insert(qid.to_string());
                    }
                }
                _ => {}
            }
        }
    }

    fn finish(self) -> Vec<HeldRow> {
        struct Best {
            epoch: u64,
            qid: String,
            question: String,
            ts: String,
        }
        let mut best: std::collections::BTreeMap<String, Best> = std::collections::BTreeMap::new();
        let mut qids: Vec<(&String, &(u64, String, Value))> = self.asked.iter().collect();
        qids.sort_by(|a, b| a.0.cmp(b.0));
        for (qid, (epoch, ts, data)) in qids {
            if self.closed.contains(qid) {
                continue;
            }
            let Some(blocks) = data.get("blocks").and_then(Value::as_array) else {
                continue;
            };
            let question: String = data
                .get("question")
                .and_then(Value::as_str)
                .unwrap_or("")
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(80)
                .collect();
            let ts = ts.clone();
            for node in blocks {
                let Some(node) = node.as_str() else {
                    continue;
                };
                let take = match best.get(node) {
                    None => true,
                    Some(b) => (*epoch, qid.as_str()) < (b.epoch, b.qid.as_str()),
                };
                if take {
                    best.insert(
                        node.to_string(),
                        Best {
                            epoch: *epoch,
                            qid: qid.to_string(),
                            question: question.clone(),
                            ts: ts.clone(),
                        },
                    );
                }
            }
        }
        let mut rows: Vec<HeldRow> = best
            .into_iter()
            .map(|(node, b)| HeldRow {
                node,
                question_id: b.qid,
                question: b.question,
                ts: b.ts,
                epoch: b.epoch,
            })
            .collect();
        rows.sort_by_key(|r| (r.epoch, r.question_id.clone()));
        rows
    }
}

/// The open questions whose subject resolved, each with the close reason
/// its rule produced. A question's node set is the `node` field plus every
/// `blocks` entry, and the first rule that holds wins:
///
/// 1. `pr-merged` - a node in the set reads `merge_status: merged` for PR
///    P and the subject names P (`PR P` or `#P` in the text, so a
///    head-scoped subject about another PR never closes). Ignores
///    `why_user`.
/// 2. `superseded` - the subject is head-scoped and another open question
///    with the same `node` field and family re-asked at a different head
///    later (ties break on the larger id). Ignores `why_user`.
/// 3. `node-closed`/`moot` - a node in the set reads done or superseded,
///    and every PR the question names is merged or read closed; a user-only
///    `why_user` closes moot. A merge_status stamp alone is never a terminal
///    rung: a multi-ship node or a reopened node can carry one while work
///    remains.
///
/// Pure over journal contents; the caller supplies the graph facts and
/// does the writing.
pub(crate) fn settled_questions(
    raw: &str,
    facts: &crate::question_sweep::RungFacts,
) -> Vec<(String, String)> {
    let (asked, closed) = open_question_rows(raw);
    let mut open: Vec<(String, Value)> = asked
        .into_iter()
        .filter(|(qid, _)| !closed.contains(qid))
        .collect();
    open.sort_by(|a, b| a.0.cmp(&b.0));
    // The newest open head-scoped ask per (node field, family):
    // (ts, id, head), for the supersede rule.
    let mut newest: std::collections::HashMap<(String, String), (&str, &str, &str)> =
        std::collections::HashMap::new();
    for (qid, data) in &open {
        let (Some(node), Some(subject)) = (
            data.get("node").and_then(Value::as_str),
            data.get("subject").and_then(Value::as_str).map(str::trim),
        ) else {
            continue;
        };
        if let Some((family, head)) = crate::question_sweep::head_scoped(subject) {
            let ts = data.get("ts").and_then(Value::as_str).unwrap_or("");
            let key = (node.to_string(), family.to_string());
            let newer = match newest.get(&key) {
                Some((old_ts, old_id, _)) => (ts, qid.as_str()) > (*old_ts, *old_id),
                None => true,
            };
            if newer {
                newest.insert(key, (ts, qid.as_str(), head));
            }
        }
    }
    let mut out = Vec::new();
    for (qid, data) in &open {
        let subject = data
            .get("subject")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        let head = crate::question_sweep::head_scoped(subject);
        let nodes = question_nodes(data);
        let mut reason = None;
        // 1. pr-merged: the subject names the thing that resolved. The
        // head-scoped shape alone is not enough: a multi-ship node carries
        // merge_status merged from its last ship while the next head's
        // question is already live, and a head token names no PR. Every
        // merged PR feeds the rule, primary and additional_prs alike.
        for n in &nodes {
            let Some(prs) = facts.merged_prs.get(n) else {
                continue;
            };
            if let Some(pr) = prs.iter().find(|pr| subject_names_pr(subject, **pr)) {
                reason = Some(format!("pr-merged: PR {pr} merged (node {n})"));
                break;
            }
        }
        // 2. superseded: a newer open ask in this head family on this
        // node. The map holds the family's newest, so any other id it
        // holds is newer than this row.
        if reason.is_none() {
            if let (Some((family, this_head)), Some(node)) =
                (head, data.get("node").and_then(Value::as_str))
            {
                if let Some((_, new_id, new_head)) =
                    newest.get(&(node.to_string(), family.to_string()))
                {
                    if *new_id != qid.as_str()
                        && !crate::question_sweep::same_head(this_head, new_head)
                    {
                        reason = Some(format!("superseded: {new_id} re-asks at head {new_head}"));
                    }
                }
            }
        }
        // 3. node-closed / moot: the node read a terminal rung, and no PR
        // the question names can still need the answer.
        if reason.is_none() {
            if let Some((n, status)) = closed_node(&nodes, facts) {
                if question_prs(data)
                    .into_iter()
                    .all(|pr| pr_settled(pr, data, &nodes, facts))
                {
                    let word = if user_only(data) {
                        "moot"
                    } else {
                        "node-closed"
                    };
                    reason = Some(format!("{word}: {n} {status}"));
                }
            }
        }
        if let Some(reason) = reason {
            out.push((qid.clone(), reason));
        }
    }
    out
}

/// One question the node-closed rule holds because a PR it names reads open.
pub(crate) struct HeldQuestion {
    pub(crate) id: String,
    pub(crate) data: Value,
    pub(crate) node: String,
    pub(crate) status: String,
    pub(crate) pr: u64,
}

/// The hop action that records the move to the lead, so it happens once.
pub(crate) const OPEN_PR_HOP: &str = "open-pr";

/// Every `(asker cwd, PR)` the node-closed rule must read before it closes a
/// question. A row with no cwd has no repo to read, so it stays open.
pub(crate) fn prs_to_read(
    raw: &str,
    facts: &crate::question_sweep::RungFacts,
) -> std::collections::BTreeSet<(String, u64)> {
    let (asked, closed) = open_question_rows(raw);
    let mut reads = std::collections::BTreeSet::new();
    for (qid, data) in &asked {
        let nodes = question_nodes(data);
        if closed.contains(qid) || closed_node(&nodes, facts).is_none() {
            continue;
        }
        let cwd = row_cwd(data);
        if cwd.is_empty() {
            continue;
        }
        for pr in question_prs(data) {
            if !pr_settled(pr, data, &nodes, facts) {
                reads.insert((cwd.to_string(), pr));
            }
        }
    }
    reads
}

/// The open questions a closed node left with a live PR, for the lead. A
/// user-only ask stays on the user's board, and a row moved once stays put.
pub(crate) fn questions_for_the_lead(
    raw: &str,
    facts: &crate::question_sweep::RungFacts,
    settled: &[(String, String)],
) -> Vec<HeldQuestion> {
    let (asked, closed) = open_question_rows(raw);
    let mut out = Vec::new();
    for (qid, data) in asked {
        let nodes = question_nodes(&data);
        let moved = data["via"]
            .as_array()
            .is_some_and(|via| via.iter().any(|hop| hop["action"] == OPEN_PR_HOP));
        if closed.contains(&qid)
            || moved
            || user_only(&data)
            || settled.iter().any(|(id, _)| *id == qid)
        {
            continue;
        }
        let Some((node, status)) = closed_node(&nodes, facts) else {
            continue;
        };
        let (node, status) = (node.to_string(), status.to_string());
        let pr = question_prs(&data).into_iter().find(|pr| {
            facts.pr_open.get(&(row_cwd(&data).to_string(), *pr)) == Some(&true)
                && !pr_settled(*pr, &data, &nodes, facts)
        });
        if let Some(pr) = pr {
            out.push(HeldQuestion {
                id: qid,
                data,
                node,
                status,
                pr,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

/// A question's node set: the `node` field plus every `blocks` entry.
fn question_nodes(data: &Value) -> Vec<String> {
    let mut nodes: Vec<String> = data
        .get("node")
        .and_then(Value::as_str)
        .map(str::to_string)
        .into_iter()
        .collect();
    if let Some(blocks) = data.get("blocks").and_then(Value::as_array) {
        nodes.extend(blocks.iter().filter_map(Value::as_str).map(str::to_string));
    }
    nodes
}

/// The first node in the set that reads a terminal rung, with that rung.
fn closed_node<'a>(
    nodes: &'a [String],
    facts: &'a crate::question_sweep::RungFacts,
) -> Option<(&'a str, &'a str)> {
    nodes.iter().find_map(|n| {
        let status = facts.statuses.get(n)?;
        ["done", "superseded"]
            .contains(&status.as_str())
            .then_some((n.as_str(), status.as_str()))
    })
}

fn user_only(data: &Value) -> bool {
    crate::escalation::why_user_is_user_only(
        data.get("why_user").and_then(Value::as_str).unwrap_or(""),
        &crate::question_ladder::Rules::default(),
    )
}

fn row_cwd(data: &Value) -> &str {
    data.get("cwd").and_then(Value::as_str).unwrap_or("").trim()
}

/// The PRs a question names. A subject can be absent, so the text counts too,
/// but only as `PR P`: a bare `#2` in prose can mean an option or an issue.
fn question_prs(data: &Value) -> Vec<u64> {
    let field = |key: &str| data.get(key).and_then(Value::as_str).unwrap_or("");
    let mut prs = named_prs(field("subject"), true);
    prs.extend(named_prs(field("question"), false));
    prs
}

/// A named PR no longer needs an answer only on a positive read: merged on
/// one of the question's nodes, or read closed. An unread PR holds the row.
fn pr_settled(
    pr: u64,
    data: &Value,
    nodes: &[String],
    facts: &crate::question_sweep::RungFacts,
) -> bool {
    let merged = nodes
        .iter()
        .any(|n| facts.merged_prs.get(n).is_some_and(|prs| prs.contains(&pr)));
    merged || facts.pr_open.get(&(row_cwd(data).to_string(), pr)) == Some(&false)
}

/// Every PR number `text` names: `PR P` with any page separator, and `#P`
/// when `bare_hash` allows it.
fn named_prs(text: &str, bare_hash: bool) -> Vec<u64> {
    let bytes = text.as_bytes();
    let mut prs = Vec::new();
    for i in 0..bytes.len() {
        let word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        let mut digits = if bare_hash && bytes[i] == b'#' {
            i + 1
        } else if word_start && bytes[i..].len() >= 2 && bytes[i..i + 2].eq_ignore_ascii_case(b"pr")
        {
            i + 2
        } else {
            continue;
        };
        while bytes[i] != b'#'
            && matches!(bytes.get(digits), Some(b'#' | b'-' | b'/' | b':' | b' '))
        {
            digits += 1;
        }
        let end = bytes[digits..]
            .iter()
            .position(|b| !b.is_ascii_digit())
            .map_or(bytes.len(), |n| digits + n);
        if let Ok(pr) = text[digits..end].parse::<u64>() {
            if !prs.contains(&pr) {
                prs.push(pr);
            }
        }
    }
    prs
}

/// The subject names PR P.
fn subject_names_pr(subject: &str, pr: u64) -> bool {
    named_prs(subject, true).contains(&pr)
}

/// The open (asked, never closed) question rows: the journal fold the
/// closed-rung consumers share.
fn open_question_rows(raw: &str) -> (HashMap<String, Value>, std::collections::HashSet<String>) {
    let mut asked: HashMap<String, Value> = HashMap::new();
    let mut closed: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in raw.lines() {
        if line.trim().is_empty() || !line.contains("operator_question") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let data = v
            .get("data")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        match kind {
            "operator_question" => {
                if let Some(qid) = data.get("question_id").and_then(Value::as_str) {
                    let qid = qid.to_string();
                    let mut row = data;
                    // The envelope ts rides beside the data: the supersede
                    // rule orders a family by it.
                    if let (Some(obj), Some(ts)) = (row.as_object_mut(), v.get("ts")) {
                        obj.insert("ts".into(), ts.clone());
                    }
                    asked.insert(qid, row);
                }
            }
            "operator_question_closed" => {
                if let Some(qid) = data.get("question_id").and_then(Value::as_str) {
                    closed.insert(qid.to_string());
                }
            }
            _ => {}
        }
    }
    (asked, closed)
}

/// Stamp each item's `live` bit from its node claim (x-aaaa 1.4): an item whose
/// node holds a Live or Suspect claim (a suspect TTL-unexpired claim still
/// protects the slot) renders even without a roster row; an unclaimed or
/// node-less one stays `live=false` and the client drops it when unjoined. This
/// is the IO half of the fold, kept out of the pure [`fold`] so it stays testable.
fn stamp_liveness(mut items: Vec<NeedItem>) -> Vec<NeedItem> {
    for item in &mut items {
        // A mail escalation is always-live by design: it carries no node claim
        // (it is about mail between agents, not a target session), so the
        // node-keyed stamp below would mark it dead and the client's
        // squadless-render branch would drop it -- the silent eat this closes.
        // The live bit here is an honest "surface this with no roster row"
        // label, not a session-liveness claim.
        // Same reasoning for operator_question (no node claim behind a
        // question either) and for the aggregate carveout_stale/stale_claims
        // rows (no single node owns a pile of carve-outs or claims).
        // mail_delivery_miss rides the same rule: the client drops it from the
        // operator view, but it stays in the JSON for a wake consumer, and a
        // node-keyed stamp would label it dead when nothing was ever claimed.
        // `worker_refused` carries no node either (it is registry-derived,
        // not ledger-resolved), and unlike those rows it needs no honest
        // fiction: the item exists ONLY because `refused_worker_items` just
        // probed the row and it answered -- refused-but-reachable IS live.
        if matches!(
            item.kind.as_str(),
            "mail_question"
                | "mail_delivery_miss"
                | "operator_question"
                | "carveout_stale"
                | "stale_claims"
                | "worker_refused"
        ) {
            item.live = true;
            continue;
        }
        item.live = item.node.as_deref().is_some_and(|n| {
            let (state, _) = crate::claims::status(&format!("node:{n}"), None);
            matches!(
                state,
                crate::claims::ClaimState::Live | crate::claims::ClaimState::Suspect
            )
        });
    }
    items
}

/// One unharvested-carve-out summary row, when the pile has actually gone
/// stale (>= [`DECISION_STALE_FLOOR_SECS`] since the oldest entry). Reads
/// `.fno/carveouts.jsonl` directly (one JSON object per line, same append-only
/// convention as `events.jsonl`) rather than the events fold: a carve-out has
/// no natural expiry, so this is never windowed by `since` at all.
///
/// One aggregate row, not one per carve-out: measured 44 rows at once,
/// and a needs-me queue flooded with individual rows would just get truncated
/// by the client's worst-first cap anyway. "N carve-outs, oldest Xd" is what
/// actually answers "should I go clean this up."
pub fn carveout_age_item(carveouts_raw: &str, now: u64) -> Option<NeedItem> {
    let mut count = 0u64;
    let mut oldest: Option<(u64, String)> = None;
    for line in carveouts_raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(trimmed) else {
            continue; // malformed line: skip, never abort
        };
        let Some(ts) = v.get("ts").and_then(|t| t.as_str()) else {
            continue;
        };
        let Some(epoch) = to_epoch_lenient(ts) else {
            continue;
        };
        // `backfill` rows are the /pr-merged slot's, not the retro sweep's
        // (it skips them entirely), so counting them would mint a Decision
        // row whose advertised remedy cannot clear it.
        if v.get("kind").and_then(|k| k.as_str()) == Some("backfill") {
            continue;
        }
        count += 1;
        if oldest.as_ref().is_none_or(|(e, _)| epoch < *e) {
            oldest = Some((epoch, ts.to_string()));
        }
    }
    let (oldest_epoch, oldest_ts) = oldest?;
    let age = now.saturating_sub(oldest_epoch);
    if age < DECISION_STALE_FLOOR_SECS {
        return None;
    }
    Some(NeedItem {
        kind: "carveout_stale".to_string(),
        // No target session; this is a pile-level signal. A stable constant
        // key so the client's cursor re-anchor (keyed on session_id) does not
        // drift row identity across folds.
        session_id: "carveouts".to_string(),
        node: None,
        name: None,
        title: None,
        ts: oldest_ts,
        evidence: format!(
            "{count} unharvested carve-out(s), oldest {}d - fno backlog retro sweep-carveouts --apply",
            age / 86_400
        ),
        live: false, // stamped true by stamp_liveness
    })
}

/// A claim's identity + age, read by the IO layer from the claims directory
/// (mirrors what `client_commands::claim_sweep_payload` already scans) and handed
/// to [`stale_claim_item`], which stays pure and testable.
pub struct ClaimAge {
    pub key: String,
    pub holder: String,
    pub acquired_at_ms: i64,
    pub state: crate::claims::ClaimState,
}

/// One stale-claim summary row, when the oldest `Stale` claim has actually
/// gone stale for a while (>= [`DECISION_STALE_FLOOR_SECS`]). A `Stale` claim
/// (dead pid, TTL expired) that is minutes old is normal churn; one that is
/// weeks old is an orphaned lock nobody cleaned up (x-bbbb measured 573 claim
/// files, 570 dead pids, oldest specimens 56-75 days).
///
/// One aggregate row, not one per claim, for the same reason as
/// [`carveout_age_item`]: 570 individual rows would just be truncated by the
/// client's worst-first cap.
pub fn stale_claim_item(claims: &[ClaimAge], now_ms: i64) -> Option<NeedItem> {
    let oldest = claims
        .iter()
        .filter(|c| matches!(c.state, crate::claims::ClaimState::Stale))
        .min_by_key(|c| c.acquired_at_ms)?;
    let stale_count = claims
        .iter()
        .filter(|c| matches!(c.state, crate::claims::ClaimState::Stale))
        .count();
    let age_secs = ((now_ms - oldest.acquired_at_ms).max(0) / 1000) as u64;
    if age_secs < DECISION_STALE_FLOOR_SECS {
        return None;
    }
    Some(NeedItem {
        kind: "stale_claims".to_string(),
        session_id: "claims".to_string(),
        node: None,
        name: None,
        title: None,
        // The oldest claim's acquire time, so band sorting orders the row by
        // its true age instead of an empty string floating it above every
        // fresher operator question.
        ts: chrono::DateTime::from_timestamp_millis(oldest.acquired_at_ms)
            .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default(),
        evidence: format!(
            "{stale_count} stale claim(s), oldest {}d ({}, holder {})",
            age_secs / 86_400,
            oldest.key,
            oldest.holder
        ),
        live: false, // stamped true by stamp_liveness
    })
}

/// One item per registry row the progress axis classifies `refused`
/// (Task 3.4): alive, reachable, and unable to think, because its
/// endpoint was handed a model it cannot serve. Nothing else rotates this
/// state today -- `status` reads `live`, the pid is real, and every roster
/// reader that stops at reachability keeps it forever. This is the leg that
/// makes the progress axis actionable rather than decorative.
///
/// Reads the live registry directly (IO layer, like [`carveout_age_item`] /
/// [`stale_claim_item`]), never from `events.jsonl`: a refusal is a current
/// FACT about a row, not an event that happened once. ONE batched truth probe
/// for the whole registry -- the same probe `fno agents list` already pays for
/// `status` -- so this leg costs one subprocess no matter how large the roster.
/// Best-effort: an unreadable registry or a probe failure degrades to no
/// items, never a crash of `fno agents needs`.
fn refused_worker_items(home: &AgentsHome) -> Vec<NeedItem> {
    refused_worker_items_with(home, crate::truth_probe::family1_truth_probe_many)
}

/// [`refused_worker_items`] with the batch probe injected, so a test can count
/// the subprocesses the leg costs without shelling out to a real `fno`.
///
/// No stat gate on this leg, deliberately. The decision reads `observed_model`
/// to tell a refusing worker from a working one, and transcript growth says
/// nothing about WHICH model answered. A stat can only stand in for a probe
/// where the question is liveness.
fn refused_worker_items_with(
    home: &AgentsHome,
    truth_fn: impl Fn(&[String]) -> std::collections::HashMap<String, crate::truth_probe::TruthProbe>,
) -> Vec<NeedItem> {
    let registry = match crate::daemon::load_registry_asserted(&home.registry_json()) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let handles: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        registry
            .entries
            .iter()
            .map(crate::daemon::registry_truth_handle)
            .filter(|h| seen.insert(h.clone()))
            .collect()
    };
    let truths = truth_fn(&handles);
    let mut items = Vec::new();
    for e in &registry.entries {
        let probe = truths.get(&crate::daemon::registry_truth_handle(e));
        let (progress, _basis) = crate::daemon::progress_from_truth(
            probe,
            crate::truth_probe::BatchOutcome::Measured,
            e.harness_name(),
            e.route_settings_path.as_deref(),
        );
        if progress != "refused" {
            continue;
        }
        let model = probe
            .and_then(|p| p.observed_model.get("model"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown model");
        items.push(NeedItem {
            kind: "worker_refused".to_string(),
            session_id: e
                .harness_session_id
                .clone()
                .unwrap_or_else(|| e.name.clone()),
            // No ledger resolution here (this leg is registry-derived, not
            // event-derived, unlike every other item in this file) -- the
            // client joins on `name` instead, same as `carveout_age_item` /
            // `stale_claim_item`.
            node: None,
            name: Some(e.name.clone()),
            title: Some(format!("{} refused: answering as {model}", e.name)),
            ts: chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
            evidence: format!(
                "{} is alive and reachable but answering as {model}, not a claude model",
                e.name
            ),
            live: false, // stamped by stamp_liveness
        });
    }
    items
}

/// Current epoch seconds; `0` if the clock is somehow before the epoch.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Scan `dir` for `node:` / `dispatch:` claim files and their age, for
/// [`stale_claim_item`]. Same prefix pair `client_commands::claim_sweep_payload`
/// scans (an orphaned dispatch lock is as dead as an orphaned node lock), and
/// the same read primitives. Fail-open: a missing/unreadable dir or an
/// unparseable lockfile yields fewer entries, never an error - same posture
/// as `claim_sweep_payload`.
fn scan_claim_ages(dir: &Path) -> Vec<ClaimAge> {
    let records = match crate::claim_store::records_in(dir, None, true) {
        Ok(records) => records,
        Err(error) => {
            eprintln!("claim ages {}: {error}", dir.display());
            return Vec::new();
        }
    };
    let records: Vec<_> = records
        .into_iter()
        .filter(|r| r.key.starts_with("node:") || r.key.starts_with("dispatch:"))
        .collect();
    // One primed witness: a per-row verdict paid a cold truth run per session.
    let states: Vec<_> = {
        let (witness, _answer) = crate::claim_commands::session_witness_primed_for(&records);
        records
            .iter()
            .map(|rec| {
                crate::claims::classify_with_basis_and_exclusivity(
                    rec,
                    None,
                    &|pid| crate::claims::probe_pid(pid),
                    None,
                    Some(&witness),
                )
                .0
            })
            .collect()
    };
    records
        .into_iter()
        .zip(states)
        .map(|(rec, state)| ClaimAge {
            key: rec.key,
            holder: rec.holder,
            acquired_at_ms: rec.acquired_at,
            state,
        })
        .collect()
}

/// The whole needs fold over explicit sources: events read + fold + the three
/// non-event legs (carveout age, stale claims, refused workers) + liveness
/// stamp. Shared by the `needs` command and the lead board's in-process needs
/// read, so the two surfaces cannot drift (the lead board is why this is `pub`).
/// `cwd` anchors the carveout leg: the command passes the process cwd it inherits,
/// the board passes its own resolved cwd (the process cwd is not guaranteed to
/// be the project for an in-process caller).
pub fn collect_needs_items(
    home: &AgentsHome,
    event_paths: &[PathBuf],
    ledger_path: &Path,
    since: u64,
    fires_floor: u64,
    cwd: &Path,
) -> Vec<NeedItem> {
    let mut events_raw = String::new();
    for p in event_paths {
        // Every source journal — questions.jsonl included — answers from
        // committed rows in commit order now; the import inside `event_lines`
        // pulls any journal bytes a pre-cutover writer (or fixture) left.
        if let Ok(lines) = crate::loopcheck::event_lines(p) {
            for line in lines {
                events_raw.push_str(&line);
                events_raw.push('\n');
            }
        }
    }
    let ledger_raw = std::fs::read_to_string(ledger_path).unwrap_or_default();

    let mut items = fold(&events_raw, &ledger_raw, since, fires_floor);

    // Carve-out-age and stale-claim legs: durable on-disk state, not events,
    // so they are read directly here (IO layer) rather than folded from
    // `events_raw`, and never windowed by `since` (see the doc comments on
    // `carveout_age_item` / `stale_claim_item`). The ledger lives under the
    // canonical checkout's `.fno/` (`ledger_root` in `backlog/carveout.rs` on the write
    // side); a worktree only sees it through a skip-if-missing symlink, so
    // read the canonical path directly and fall back to cwd outside a repo.
    let carveouts_path = crate::paths::canonical_repo_root(cwd)
        .map(|r| r.join(".fno").join("carveouts.jsonl"))
        .unwrap_or_else(|| PathBuf::from(".fno").join("carveouts.jsonl"));
    let carveouts_raw = std::fs::read_to_string(&carveouts_path).unwrap_or_default();
    if let Some(item) = carveout_age_item(&carveouts_raw, now_secs()) {
        items.push(item);
    }
    if let Some(dir) = crate::claims::claims_dir_for(None) {
        let ages = scan_claim_ages(&dir);
        if let Some(item) = stale_claim_item(&ages, crate::claims::now_ms()) {
            items.push(item);
        }
    }
    items.extend(refused_worker_items(home));

    stamp_liveness(items)
}

/// The `fno-agents needs` command. Read-only; exits 0 on empty/corrupt input (only
/// a usage error exits 2), so the overlay caller never sees a failure it must
/// handle beyond a nonzero exit.
pub async fn run_needs(rest: &[String], home: &AgentsHome) -> i32 {
    let args = match parse_args(rest) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("fno-agents: {msg}");
            return 2;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // The --items leg reads the attention projection; everything else is the
    // session-needs fold (never touches the ledger).
    if !args.archive.is_empty() {
        if args.answer.is_some() {
            eprintln!("fno-agents: needs --archive and --answer are separate doors; pass one");
            return 2;
        }
        return run_archive(home, &cwd, &args, None);
    }
    if let Some(id) = args.answer.clone() {
        return run_answer(home, &cwd, &args, &id);
    }
    if args.clear_settled {
        if args.items || args.answer.is_some() || !args.archive.is_empty() {
            eprintln!("fno-agents: needs --clear-settled is its own door; pass it alone");
            return 2;
        }
        return run_clear_settled(home, &cwd);
    }
    if args.items {
        return run_items(home, &cwd);
    }
    let (default_events, default_ledger) = default_sources(home, &cwd);
    let explicit_events = !args.events_override.is_empty();
    let mut event_paths = if explicit_events {
        args.events_override
    } else {
        default_events
    };
    // One journal per space: the cwd journal and the canonical journal are the
    // same file now (the worktree fork is what the spaces move retired), so
    // this push collapses to a no-op and stays only as the shape's record.
    let canonical = crate::paths::canonical_repo_root(&cwd);
    if let Some(root) = &canonical {
        let canonical_events = crate::paths::space_dir(root).join("events.jsonl");
        let cwd_events = crate::paths::space_dir(&cwd).join("events.jsonl");
        if !explicit_events
            && canonical_events != cwd_events
            && !event_paths.contains(&canonical_events)
        {
            event_paths.push(canonical_events);
        }
    }
    let ledger_path = args.ledger_override.unwrap_or(default_ledger);

    let since = args
        .since_epoch
        .unwrap_or_else(|| now_secs().saturating_sub(DEFAULT_WINDOW_SECS));
    let items = collect_needs_items(
        home,
        &event_paths,
        &ledger_path,
        since,
        args.fires_floor,
        &cwd,
    );

    if args.json {
        println!(
            "{}",
            serde_json::to_string(&items).expect("serializing an owned value never fails")
        );
    } else {
        for item in &items {
            let name = item.name.as_deref().unwrap_or(&item.session_id);
            println!("{} {} - {}", item.kind, name, item.evidence);
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    // A promise-intent loop_check (the review-wedge case). Non-wedge tests care
    // about the other fields, so a promise intent is the convenient default.
    fn loop_check(
        ts: &str,
        session: &str,
        decision: &str,
        ci: &str,
        pr_state: &str,
        reviewed: bool,
        fires: u64,
    ) -> String {
        loop_check_i(
            ts, session, decision, "promise", ci, pr_state, reviewed, fires,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn loop_check_i(
        ts: &str,
        session: &str,
        decision: &str,
        intent: &str,
        ci: &str,
        pr_state: &str,
        reviewed: bool,
        fires: u64,
    ) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"loop_check","source":"hook","data":{{"session_id":"{session}","decision":"{decision}","intent":"{intent}","ci":"{ci}","pr_state":"{pr_state}","reviewed":{reviewed},"fires":{fires}}}}}"#
        )
    }

    fn termination(ts: &str, session: &str, reason: &str) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"termination","source":"hook","data":{{"session_id":"{session}","reason":"{reason}"}}}}"#
        )
    }

    // The whole default window: since=0 lets every fixture ts through.
    const ALL: u64 = 0;

    #[test]
    fn green_open_unreviewed_block_is_review_wedged() {
        let events = loop_check(
            "2026-07-03T02:00:00Z",
            "s",
            "block",
            "SUCCESS",
            "OPEN",
            false,
            5,
        );
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "review_wedged");
        assert_eq!(items[0].session_id, "s");
        assert!(items[0].evidence.contains("5 checks"));
    }

    /// A registry home with `rows` claude rows, each with a resolvable handle.
    fn refused_home(tag: &str, rows: usize) -> AgentsHome {
        use std::sync::atomic::{AtomicU32, Ordering};
        static C: AtomicU32 = AtomicU32::new(0);
        let n = C.fetch_add(1, Ordering::Relaxed);
        let root =
            std::path::PathBuf::from(format!("/tmp/fnoneeds{tag}{}_{n}", std::process::id()));
        let home = AgentsHome::at(&root);
        home.ensure_root().unwrap();
        // Written as JSON rather than a struct literal: `RegistryEntry` has 30
        // optional fields with serde defaults and none of them matter here.
        let entries: Vec<serde_json::Value> = (0..rows)
            .map(|i| {
                serde_json::json!({
                    "name": format!("worker-{i:02}"),
                    "short_id": format!("{i:02}aaaaaa"),
                    "harness": "claude",
                    "harness_session_id": format!("sid-{i:02}"),
                    "cwd": "/tmp",
                    "project_root": "/tmp",
                    "status": "live",
                    "created_at": "2026-08-19T00:00:00Z",
                })
            })
            .collect();
        std::fs::write(
            home.registry_json(),
            serde_json::to_string(&serde_json::json!({
                "schema_version": 6,
                "agents": entries,
            }))
            .unwrap(),
        )
        .unwrap();
        home
    }

    fn refused_probe(model: &str) -> crate::truth_probe::TruthProbe {
        crate::truth_probe::TruthProbe {
            state: "working".into(),
            reachability: Some("reachable".into()),
            basis: Some("transcript".into()),
            last_activity_age_s: Some(1.0),
            last_activity_basis: None,
            last_event_at: None,
            last_message: None,
            observed_model: serde_json::json!({"kind": "observed", "model": model}),
            provider_refusal: None,
            harness_title: None,
        }
    }

    #[test]
    fn refused_leg_pays_exactly_one_batch_call_regardless_of_roster_size() {
        // this leg used to spawn one Python interpreter per registry
        // row on every `fno agents needs`. One batch, one process, 24 rows.
        let home = refused_home("batchonce", 24);
        let calls = std::cell::RefCell::new(Vec::new());

        let items = refused_worker_items_with(&home, |handles: &[String]| {
            calls.borrow_mut().push(handles.to_vec());
            handles
                .iter()
                .map(|h| (h.clone(), refused_probe("gpt-5.6-luna")))
                .collect()
        });

        let calls = calls.into_inner();
        assert_eq!(calls.len(), 1, "one registry read, one probe process");
        assert_eq!(calls[0].len(), 24, "every row rides that one batch");
        assert_eq!(items.len(), 24, "and every row is still classified");
        assert!(items[0].title.as_ref().unwrap().contains("gpt-5.6-luna"));
        std::fs::remove_dir_all(home.root()).ok();
    }

    #[test]
    fn budget_termination_is_budget_stop() {
        let events = termination("2026-07-03T02:00:00Z", "s", "Budget");
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "budget_stop");
        assert!(items[0].evidence.contains("Budget"));
    }

    fn mail_escalation(
        ts: &str,
        reason: &str,
        sender: &str,
        recipient: &str,
        summary: &str,
    ) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"mail_escalation","source":"target","data":{{"reason":"{reason}","sender":"{sender}","recipient":"{recipient}","summary":"{summary}"}}}}"#
        )
    }

    #[test]
    fn mail_escalation_folds_to_mail_question_without_a_session() {
        // The trap this node closes: a mail_escalation carries no session_id, so
        // the session gate and the loop_check kind filter would both drop it. The
        // fold arm handles it before the gate and emits a mail_question row keyed
        // by recipient (the row identity, not a target session).
        let events = mail_escalation(
            "2026-07-03T02:00:00Z",
            "question",
            "etl",
            "web",
            "which schema?",
        );
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, "mail_question");
        assert_eq!(items[0].name.as_deref(), Some("web"));
        assert_eq!(items[0].session_id, "web");
        assert!(items[0].evidence.contains("question"));
        assert!(items[0].evidence.contains("etl -> web"));
    }

    #[test]
    fn a_later_delivery_miss_does_not_erase_a_standing_question() {
        // The trap the kind split opens if the map stays keyed on recipient
        // alone: latest-wins would replace the question row with a
        // mail_delivery_miss, which the client drops, so an escalation the code
        // itself calls a human's problem vanishes from the panel. A worker asks
        // `ops` a question, then anyone else's send to `ops` misses, and the
        // question is gone. The debounce is per (sender, recipient), so a
        // second sender never suppresses the miss.
        let events = format!(
            "{}\n{}\n",
            mail_escalation(
                "2026-07-03T02:00:00Z",
                "question",
                "etl",
                "ops",
                "which auth?"
            ),
            mail_escalation(
                "2026-07-03T03:00:00Z",
                "reachable-miss",
                "web",
                "ops",
                "ping"
            ),
        );
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);
        let question = items
            .iter()
            .find(|i| i.kind == "mail_question")
            .expect("the standing question survives a later delivery miss");
        assert!(question.evidence.contains("which auth?"));
        assert!(
            items.iter().any(|i| i.kind == "mail_delivery_miss"),
            "and the miss is still folded, under its own kind"
        );
    }

    #[test]
    fn termination_with_fractional_ts_still_wins_over_z_loop_check() {
        // Lexically ".5" < "Z", so a same-second fractional termination would
        // sort BEFORE the loop_check and misclassify a real stop; epoch compare
        // fixes it (gemini HIGH finding).
        let events = [
            loop_check(
                "2026-07-03T02:00:00Z",
                "s",
                "block",
                "SUCCESS",
                "OPEN",
                false,
                5,
            ),
            termination("2026-07-03T02:00:00.5", "s", "Budget"),
        ]
        .join("\n");
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);
        assert_eq!(items.len(), 1);
        assert_eq!(
            items[0].kind, "budget_stop",
            "the termination wins despite its fractional ts"
        );
    }

    #[test]
    fn ledger_resolves_node_name_title() {
        let events = loop_check(
            "2026-07-03T02:00:00Z",
            "sess-x",
            "block",
            "SUCCESS",
            "OPEN",
            false,
            5,
        );
        let ledger = r#"{"entries":[{"session_id":"sess-x","graph_node_id":"x-aaaa","title":"needs queue","worktree":"/w/footnote/x-aaaa"}]}"#;
        let items = fold(&events, ledger, ALL, DEFAULT_FIRES_FLOOR);
        assert_eq!(items[0].node.as_deref(), Some("x-aaaa"));
        assert_eq!(items[0].name.as_deref(), Some("x-aaaa"));
        assert_eq!(items[0].title.as_deref(), Some("needs queue"));
    }

    // --- operator_question (x-bbbb: NeedKind::Decision producer) --------------

    fn operator_question(ts: &str, qid: &str, question: &str, node: Option<&str>) -> String {
        let node_field = node
            .map(|n| format!(r#","node":"{n}""#))
            .unwrap_or_default();
        format!(
            r#"{{"ts":"{ts}","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"{question}"{node_field}}}}}"#
        )
    }

    fn operator_question_closed(ts: &str, qid: &str) -> String {
        format!(
            r#"{{"ts":"{ts}","type":"operator_question_closed","source":"target","data":{{"question_id":"{qid}"}}}}"#
        )
    }

    #[test]
    fn question_index_is_a_default_source_and_ask_is_folded_as_evidence() {
        let _root = crate::paths::DeclaredRoot::declare("question_index_is_a_default_");
        let tmp = tempfile::tempdir().unwrap();
        let state_dir = tmp.path().join(".fno");
        let home = AgentsHome::at(state_dir.join("agents"));
        std::fs::create_dir_all(&state_dir).unwrap();
        let index = state_dir.join("questions.jsonl");
        std::fs::write(
            &index,
            r#"{"ts":"2026-07-03T02:00:00Z","type":"operator_question","source":"target","data":{"question_id":"q-index-only","question":"long context","ask":"pick alpha or beta"}}
"#,
        )
        .unwrap();

        let (sources, _) = default_sources(&home, tmp.path());
        let events = sources
            .iter()
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect::<Vec<_>>()
            .join("\n");
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);

        assert!(
            sources.contains(&index),
            "the machine-wide question index is read"
        );
        assert_eq!(items.len(), 1, "the index-only row reaches the needs fold");
        assert_eq!(items[0].session_id, "q-index-only");
        assert_eq!(items[0].evidence, "pick alpha or beta");
    }

    #[test]
    fn closed_operator_question_is_dropped() {
        let events = [
            operator_question("2026-07-03T02:00:00Z", "q-abc", "auto-merge or hold?", None),
            operator_question_closed("2026-07-03T03:00:00Z", "q-abc"),
        ]
        .join("\n");
        assert!(fold(&events, "", ALL, DEFAULT_FIRES_FLOOR).is_empty());
    }

    #[test]
    fn operator_question_survives_a_56_day_old_since_window() {
        // The defect this leg closes: a 24h-windowed fold hides a question
        // that has been open for weeks. `since` here is "now" (2099), so a
        // windowed kind would be excluded - operator_question must not be.
        let events = operator_question("2026-06-01T00:00:00Z", "q-old", "still waiting", None);
        let since = crate::state::rfc3339_like_to_secs("2099-01-01T00:00:00Z").unwrap();
        let items = fold(&events, "", since, DEFAULT_FIRES_FLOOR);
        assert_eq!(
            items.len(),
            1,
            "operator_question bypasses the since window"
        );
        assert_eq!(items[0].kind, "operator_question");
    }

    // --- held fold --------------------------------------------------

    fn operator_question_blocked(ts: &str, qid: &str, question: &str, blocks: &[&str]) -> String {
        let blocks = blocks
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"ts":"{ts}","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"{question}","blocks":[{blocks}]}}}}"#
        )
    }

    fn fold_facts(
        statuses: &[(&str, &str)],
        merged: &[(&str, u64)],
    ) -> crate::question_sweep::RungFacts {
        let mut merged_prs: std::collections::BTreeMap<String, Vec<u64>> = Default::default();
        for (n, p) in merged {
            merged_prs.entry(n.to_string()).or_default().push(*p);
        }
        crate::question_sweep::RungFacts {
            statuses: statuses
                .iter()
                .map(|(n, s)| (n.to_string(), s.to_string()))
                .collect(),
            merged_prs,
            raw: String::new(),
            ..Default::default()
        }
    }

    #[test]
    fn a_done_node_holds_a_question_until_the_pr_it_names_reads_closed() {
        // The two real spellings: the PR only in the text, and only in the
        // subject. An unread PR holds the row and moves nothing.
        for (text, subject) in [
            ("Close PR 3212 (x-gone)?", ""),
            ("Land or close it?", r#","subject":"pr-3212-land-or-close"#),
        ] {
            let subject = if subject.is_empty() {
                String::new()
            } else {
                format!(r#"{subject}""#)
            };
            let raw = format!(
                r#"{{"ts":"2026-10-19T17:08:01Z","type":"operator_question","source":"target","data":{{"question_id":"q-pr","question":"{text}","cwd":"/repo","node":"x-gone"{subject}}}}}"#
            );
            let mut facts = fold_facts(&[("x-gone", "done")], &[]);
            let key = ("/repo".to_string(), 3212);
            assert!(settled_questions(&raw, &facts).is_empty(), "{raw}");
            assert!(questions_for_the_lead(&raw, &facts, &[]).is_empty());
            assert!(prs_to_read(&raw, &facts).contains(&key));

            facts.pr_open.insert(key.clone(), true);
            assert!(settled_questions(&raw, &facts).is_empty(), "{raw}");
            let held = questions_for_the_lead(&raw, &facts, &[]);
            assert_eq!(held.len(), 1);
            assert_eq!((held[0].node.as_str(), held[0].pr), ("x-gone", 3212));

            facts.pr_open.insert(key, false);
            assert_eq!(
                settled_questions(&raw, &facts),
                vec![("q-pr".to_string(), "node-closed: x-gone done".to_string())]
            );
        }
        // A bare `#99` in prose names no PR, so it reads nothing and holds nothing.
        let raw = r#"{"ts":"t","type":"operator_question","source":"target","data":{"question_id":"q-opt","question":"Take option #99?","cwd":"/repo","node":"x-gone"}}"#;
        let facts = fold_facts(&[("x-gone", "done")], &[]);
        assert!(prs_to_read(raw, &facts).is_empty());
        assert_eq!(settled_questions(raw, &facts).len(), 1);
    }

    #[test]
    fn a_user_only_or_already_moved_question_stays_where_it_sits() {
        let row = |qid: &str, extra: &str| {
            format!(
                r#"{{"ts":"t","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"Close PR 9?","cwd":"/repo","node":"x-a"{extra}}}}}"#
            )
        };
        let raw = [
            row("q-taste", r#","why_user":"a product or taste call""#),
            row("q-moved", r#","via":[{"action":"open-pr"}]"#),
        ]
        .join("\n");
        let mut facts = fold_facts(&[("x-a", "done")], &[]);
        facts.pr_open.insert(("/repo".to_string(), 9), true);
        assert!(settled_questions(&raw, &facts).is_empty());
        assert!(questions_for_the_lead(&raw, &facts, &[]).is_empty());
    }

    #[test]
    fn sweep_closes_only_terminal_rungs() {
        let raw = operator_question_blocked("2026-07-03T02:00:00Z", "q-hold", "pick", &["x-bbbb"]);
        let mk = |status: &str| settled_questions(&raw, &fold_facts(&[("x-bbbb", status)], &[]));
        assert_eq!(
            mk("done"),
            vec![("q-hold".to_string(), "node-closed: x-bbbb done".to_string())]
        );
        assert_eq!(
            mk("superseded"),
            vec![(
                "q-hold".to_string(),
                "node-closed: x-bbbb superseded".to_string()
            )]
        );
        assert!(mk("in_progress").is_empty());
        assert!(mk("blocked").is_empty());
    }

    fn operator_question_with_why_user(ts: &str, qid: &str, why: &str, blocks: &[&str]) -> String {
        let blocks = blocks
            .iter()
            .map(|b| format!("\"{b}\""))
            .collect::<Vec<_>>()
            .join(",");
        format!(
            r#"{{"ts":"{ts}","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"approve?","blocks":[{blocks}],"why_user":"{why}"}}}}"#
        )
    }

    #[test]
    fn sweep_closes_a_user_only_question_whose_subject_resolved() {
        // A resolved subject closes the question, user-only why_user
        // included: a merged PR or a done node left its question open
        // forever under the old spare. The reason records which rule
        // closed it and names the node.
        let facts = fold_facts(&[("x-bbbb", "done")], &[]);
        let raw = operator_question_with_why_user(
            "2026-07-03T02:00:00Z",
            "q-user",
            "a product or taste call",
            &["x-bbbb"],
        );
        assert_eq!(
            settled_questions(&raw, &facts),
            vec![("q-user".to_string(), "moot: x-bbbb done".to_string())]
        );
        let raw = operator_question_with_why_user(
            "2026-07-03T02:00:00Z",
            "q-mach",
            "reversible, any agent on the machine can run it",
            &["x-bbbb"],
        );
        assert_eq!(
            settled_questions(&raw, &facts),
            vec![("q-mach".to_string(), "node-closed: x-bbbb done".to_string())]
        );
        let raw = operator_question_blocked("2026-07-03T02:00:00Z", "q-plain", "pick", &["x-bbbb"]);
        assert_eq!(
            settled_questions(&raw, &facts),
            vec![(
                "q-plain".to_string(),
                "node-closed: x-bbbb done".to_string()
            )]
        );
        // Rule 1 ignores why_user, but only when the subject names the
        // thing that resolved: the merged PR or one of its heads. A taste
        // ask about rules on the same node names no PR and stays open -
        // the machine close reaches a user-only question only that far.
        let merged = fold_facts(&[("x-live", "in_progress")], &[("x-live", 7)]);
        let paint = r#"{"ts":"2026-07-03T02:00:00Z","type":"operator_question","source":"target","data":{"question_id":"q-paint","question":"approve?","node":"x-live","blocks":["x-live"],"subject":"visual approval PR 7 at 1111111a","why_user":"taste"}}"#;
        assert_eq!(
            settled_questions(paint, &merged),
            vec![(
                "q-paint".to_string(),
                "pr-merged: PR 7 merged (node x-live)".to_string()
            )]
        );
        let grant = r#"{"ts":"2026-07-03T02:00:00Z","type":"operator_question","source":"target","data":{"question_id":"q-grant","question":"approve?","node":"x-live","blocks":["x-live"],"subject":"merge-grant for PR #7 on footnote","why_user":"irreversible"}}"#;
        assert_eq!(
            settled_questions(grant, &merged),
            vec![(
                "q-grant".to_string(),
                "pr-merged: PR 7 merged (node x-live)".to_string()
            )]
        );
        let rules = r#"{"ts":"2026-07-03T02:00:02Z","type":"operator_question","source":"target","data":{"question_id":"q-rules","question":"approve?","node":"x-live","blocks":["x-live"],"subject":"approve the rules","why_user":"taste"}}"#;
        assert!(
            settled_questions(rules, &merged).is_empty(),
            "a taste ask that names no PR stays open"
        );
        // A multi-ship node carries merge_status merged from its last ship
        // while the next head's question is live: a subject naming another
        // PR never closes on that stamp.
        let next_head = r#"{"ts":"2026-07-03T02:00:03Z","type":"operator_question","source":"target","data":{"question_id":"q-next","question":"approve?","node":"x-live","blocks":["x-live"],"subject":"visual approval PR 9 at cccccccd","why_user":"taste"}}"#;
        assert!(
            settled_questions(next_head, &merged).is_empty(),
            "a head-scoped ask about another PR stays open on a stale merged stamp"
        );
    }

    #[test]
    fn a_newer_head_supersedes_the_older_open_question_in_its_family() {
        // One open question per head family: the older ask closes
        // superseded when a newer ask re-asks the family at a different
        // head. The same family on another node is its own family, a
        // prefix of the same sha is the same head (no supersede), and a
        // non-head subject never supersedes.
        let mk = |qid: &str, hh: &str, subject: &str, node: &str| {
            format!(
                r#"{{"ts":"2026-07-03T{hh}:00:00Z","type":"operator_question","source":"target","data":{{"question_id":"{qid}","question":"does the paint look right?","node":"{node}","blocks":["{node}"],"subject":"{subject}"}}}}"#
            )
        };
        let raw = [
            mk("q-old", "02", "visual approval PR 7 at 1111111a", "x-aaaa"),
            mk("q-new", "05", "visual approval PR 7 at 2222222b", "x-aaaa"),
        ]
        .join("\n");
        let settled = settled_questions(&raw, &fold_facts(&[("x-aaaa", "in_progress")], &[]));
        assert_eq!(
            settled,
            vec![(
                "q-old".to_string(),
                "superseded: q-new re-asks at head 2222222b".to_string()
            )]
        );
        // The same family on two nodes: neither closes.
        let raw = [
            mk("q-a", "02", "visual approval PR 7 at 1111111a", "x-aaaa"),
            mk("q-b", "05", "visual approval PR 7 at 2222222b", "x-bbbb"),
        ]
        .join("\n");
        assert!(settled_questions(
            &raw,
            &fold_facts(&[("x-aaaa", "in_progress"), ("x-bbbb", "in_progress")], &[])
        )
        .is_empty());
        // A prefix of the same sha is the same head: no supersede.
        let raw = [
            mk("q-old", "02", "visual approval PR 7 at 1111111a", "x-aaaa"),
            mk("q-new", "05", "visual approval PR 7 at 1111111", "x-aaaa"),
        ]
        .join("\n");
        assert!(settled_questions(&raw, &fold_facts(&[("x-aaaa", "in_progress")], &[])).is_empty());
        // A subject whose last token is not a sha never supersedes.
        let raw = [
            mk("q-old", "02", "visual approval PR 7 at 1111111a", "x-aaaa"),
            mk("q-new", "05", "approve the rules", "x-aaaa"),
        ]
        .join("\n");
        assert!(settled_questions(&raw, &fold_facts(&[("x-aaaa", "in_progress")], &[])).is_empty());
        // head_scoped's shape: the split spellings, the hex gate, and the
        // no-family guard.
        assert_eq!(
            crate::question_sweep::head_scoped("visual approval PR 7 at 05ea5bcc"),
            Some(("visual approval PR 7 at", "05ea5bcc"))
        );
        assert_eq!(
            crate::question_sweep::head_scoped("merge-grant:footnote#3354@d2ad514dabc"),
            Some(("merge-grant:footnote#3354", "d2ad514dabc"))
        );
        assert_eq!(crate::question_sweep::head_scoped("lead-hold-cap"), None);
        assert_eq!(
            crate::question_sweep::head_scoped("visual approval PR 12 at defaced"),
            None
        );
        assert_eq!(crate::question_sweep::head_scoped("@abcdef1"), None);
    }

    #[test]
    fn operator_questions_rank_newest_first_and_other_kinds_stay_ascending() {
        // AC11: the fold mirrors outstanding/core.py's rank - question
        // rows newest-first among themselves, every other kind keeps ascending
        // (ts, session_id, kind) stream order. Interleaved streams, so push
        // order alone can satisfy neither half.
        let events = [
            operator_question("2026-07-03T01:00:00Z", "q-old", "older ask", None),
            termination("2026-07-03T02:00:00Z", "sa", "Budget"),
            operator_question("2026-07-03T03:00:00Z", "q-new", "newer ask", None),
            termination("2026-07-03T02:30:00Z", "sb", "Budget"),
        ]
        .join("\n");
        let items = fold(&events, "", ALL, DEFAULT_FIRES_FLOOR);

        let question_ids: Vec<&str> = items
            .iter()
            .filter(|i| i.kind == "operator_question")
            .map(|i| i.session_id.as_str())
            .collect();
        let stop_sessions: Vec<&str> = items
            .iter()
            .filter(|i| i.kind == "budget_stop")
            .map(|i| i.session_id.as_str())
            .collect();

        assert_eq!(question_ids, ["q-new", "q-old"]);
        assert_eq!(stop_sessions, ["sa", "sb"]);
    }

    // --- carveout_age_item ------------------------------------------------

    fn carveout(ts: &str, kind: &str) -> String {
        format!(r#"{{"ts":"{ts}","kind":"{kind}","id":"c-1"}}"#)
    }

    #[test]
    fn stale_carveout_pile_produces_one_aggregate_item() {
        let raw = [
            carveout("2026-06-01T00:00:00Z", "oos-bug"),
            carveout("2026-06-15T00:00:00Z", "deferred"),
            // The /pr-merged slot's rows are not sweep-harvestable; counting
            // one would mint a row the advertised remedy cannot clear.
            carveout("2026-06-20T00:00:00Z", "backfill"),
        ]
        .join("\n");
        let now = crate::state::rfc3339_like_to_secs("2026-07-15T00:00:00Z").unwrap();
        let item = carveout_age_item(&raw, now).expect("29-day-old pile is stale");
        assert_eq!(item.kind, "carveout_stale");
        assert!(item.evidence.contains("2 unharvested"));
        assert!(item.evidence.contains("44d"));
    }

    // --- stale_claim_item ---------------------------------------------------

    fn claim_age(
        key: &str,
        holder: &str,
        acquired_at_ms: i64,
        state: crate::claims::ClaimState,
    ) -> ClaimAge {
        ClaimAge {
            key: key.to_string(),
            holder: holder.to_string(),
            acquired_at_ms,
            state,
        }
    }

    #[test]
    fn old_stale_claim_produces_one_aggregate_item() {
        let now_ms = 1_800_000_000_000_i64; // an arbitrary "now"
        let fifty_six_days_ms = 56 * 24 * 60 * 60 * 1000;
        let claims = vec![claim_age(
            "node:x-orphan",
            "dead-holder",
            now_ms - fifty_six_days_ms,
            crate::claims::ClaimState::Stale,
        )];
        let item = stale_claim_item(&claims, now_ms).expect("56-day-old stale claim");
        assert_eq!(item.kind, "stale_claims");
        assert!(item.evidence.contains("node:x-orphan"));
        assert!(item.evidence.contains("dead-holder"));
        assert!(item.evidence.contains("56d"));
        // ts carries the oldest claim's acquire time (band sort orders by age),
        // never an empty string that floats the row above fresher questions.
        // 56 days before the test's now (1.8e12 ms) lands in 2026-11.
        assert!(item.ts.starts_with("2026-11-20T"), "ts={}", item.ts);
    }

    #[test]
    fn held_rows_read_a_store_committed_question() {
        // AC11-HELD: an open store-only question holds the node; a store-only
        // close releases it.
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("events.jsonl");
        let ask = serde_json::json!({
            "ts": "2026-09-17T12:00:00Z", "type": "operator_question", "source": "test",
            "data": {"question_id": "q-1", "blocks": ["x-1"], "question": "proceed?"}
        });
        crate::event_store::append_envelope(&journal, &ask.to_string(), None).unwrap();
        let held = held_rows(&[journal.clone()]);
        assert_eq!(held.len(), 1, "{held:?}");
        assert_eq!(held[0].node, "x-1");
        let close = serde_json::json!({
            "ts": "2026-09-17T13:00:00Z", "type": "operator_question_closed", "source": "test",
            "data": {"question_id": "q-1"}
        });
        crate::event_store::append_envelope(&journal, &close.to_string(), None).unwrap();
        assert!(
            held_rows(&[journal]).is_empty(),
            "the close releases the node"
        );
    }

    /// An open question row in the space journal, and the args for a mux
    /// option answer against it. The [`DeclaredRoot`] guard rides the return
    /// so the fixture outlives setup: dropping it restores the env and
    /// deletes the root.
    fn door_setup(tag: &str) -> (tempfile::TempDir, crate::paths::DeclaredRoot, NeedsArgs) {
        let root = crate::paths::DeclaredRoot::declare(tag);
        let dir = tempfile::tempdir().unwrap();
        let cwd = dir.path().join("repo");
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        std::fs::create_dir_all(crate::paths::space_dir(&cwd)).unwrap();
        let ask = serde_json::json!({
            "ts": "2026-09-26T05:00:00Z", "type": "operator_question", "source": "test",
            "data": {
                "question_id": "q-door", "question": "pick one", "ask": "pick one",
                "session_id": "s1", "cwd": "/repo/fno", "asker": "w1", "node": "x-1",
                "options": ["a", "b"]
            }
        });
        crate::event_store::append_envelope(&space, &ask.to_string(), None).unwrap();
        let args = NeedsArgs {
            since_epoch: None,
            fires_floor: 0,
            json: false,
            items: false,
            clear_settled: false,
            answer: Some("q-door".to_string()),
            option: Some(2),
            words: None,
            done: false,
            delegate: false,
            archive: vec![],
            sink: Some("mux".to_string()),
            email: None,
            events_override: vec![],
            ledger_override: None,
        };
        (dir, root, args)
    }

    #[test]
    fn ac1_hp_the_mux_door_records_a_durable_answer_row() {
        let (dir, _root, args) = door_setup("needs_door_hp_open_");
        let cwd = dir.path().join("repo");
        let home = crate::paths::AgentsHome::from_env();
        let code = run_answer(&home, &cwd, &args, "q-door");
        assert_eq!(code, 0);
        let home2 = crate::paths::AgentsHome::from_env();
        let raw =
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home2), &[]);
        assert!(
            raw.contains("attention_answer") && raw.contains("\"sink\":\"mux\""),
            "the mux row landed: {raw}"
        );
        // The projection shows it answered.
        let fno_dir = home2.root().parent().map(Path::to_path_buf).unwrap();
        let (items, _, _) = crate::attention_arm::read_items_at(&fno_dir, &cwd);
        let item = items.iter().find(|i| i.id == "q-door").unwrap();
        assert_eq!(item.state, "answered");
    }

    #[test]
    fn ac1_err_the_door_refuses_and_names_the_reason() {
        let (dir, _root, mut args) = door_setup("needs_door_err_refus");
        let cwd = dir.path().join("repo");
        let home = crate::paths::AgentsHome::from_env();
        // Absent id.
        assert_eq!(run_answer(&home, &cwd, &args, "q-none"), 2);
        // Sink other than mux (and a missing sink).
        args.sink = Some("file".to_string());
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.sink = None;
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.sink = Some("mux".to_string());
        // Two picks at once.
        args.words = Some("nope".to_string());
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.words = None;
        // Option out of range.
        args.option = Some(3);
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.option = None;
        // --done on a question.
        args.done = true;
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        // --delegate on an unstated, an irreversible, and a reversible call
        // with no live team over its node: each stays the user's.
        args.done = false;
        args.delegate = true;
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        for (id, rev) in [("q-rev-no", "no"), ("q-rev-yes", "yes")] {
            let ask = serde_json::json!({
                "ts": "2026-09-26T05:00:00Z", "type": "operator_question", "source": "test",
                "data": {"question_id": id, "question": "pick", "asker": "w1", "node": "x-1",
                         "options": ["a", "b"], "context": {"reversible": rev}}
            });
            crate::event_store::append_envelope(&space, &ask.to_string(), None).unwrap();
            assert_eq!(run_answer(&home, &cwd, &args, id), 2, "{id}");
        }
        // Nothing landed: the door writes nothing on a refusal.
        let home2 = crate::paths::AgentsHome::from_env();
        let raw =
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home2), &[]);
        assert!(
            !raw.contains("attention_answer"),
            "refusals write nothing: {raw}"
        );
    }

    #[test]
    fn needs_door_command_page_stamps_the_login_and_refuses_the_rest() {
        let (dir, _root, mut args) = door_setup("needs_door_cmd_page_");
        let cwd = dir.path().join("repo");
        let home = crate::paths::AgentsHome::from_env();
        let row_count = || {
            let home = crate::paths::AgentsHome::from_env();
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home), &[])
                .matches("attention_answer")
                .count()
        };
        // A page answer with no login, a login beside the mux sink, and a
        // delegate from the page each refuse and write nothing.
        args.sink = Some("command_page".to_string());
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.email = Some("  ".to_string());
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.sink = Some("mux".to_string());
        args.email = Some("ann@example.com".to_string());
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        args.sink = Some("command_page".to_string());
        args.option = None;
        args.delegate = true;
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        assert_eq!(row_count(), 0);
        // The real answer lands one row naming the login and the machine.
        args.delegate = false;
        args.option = Some(2);
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 0);
        let home2 = crate::paths::AgentsHome::from_env();
        let raw =
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home2), &[]);
        let row = raw
            .lines()
            .find(|l| l.contains("attention_answer"))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(row).unwrap();
        let data = &v["data"];
        assert_eq!(data["sink"], "command_page", "{row}");
        assert_eq!(data["email"], "ann@example.com", "{row}");
        assert_eq!(data["person"], "ann@example.com", "{row}");
        assert_eq!(data["authority"], "sink", "{row}");
        assert_eq!(data["attested_by"], "command_page:ann@example.com", "{row}");
        assert!(data.get("machine_id").is_some(), "{row}");
        // A second click is refused as not open: the first answer closed it.
        assert_eq!(run_answer(&home, &cwd, &args, "q-door"), 2);
        assert_eq!(row_count(), 1);
    }

    #[test]
    fn ac2_hp_the_door_records_an_answer_for_a_peer_copy() {
        let (dir, _root, mut args) = door_setup("needs_door_peer_hp_");
        let cwd = dir.path().join("repo");
        let home = crate::paths::AgentsHome::from_env();
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        // A copy of a question asked on hosta, riding this machine's board.
        let copy = serde_json::json!({
            "ts": "2026-09-26T05:00:00Z", "type": "operator_question", "source": "test",
            "data": {
                "question_id": "q-copy@hosta", "question": "pick one", "ask": "copy ask",
                "session_id": "s9", "cwd": "/repo/fno", "asker": "w9", "node": "x-9",
                "options": ["a", "b"], "host": "hosta", "origin_machine": "m-other"
            }
        });
        crate::event_store::append_envelope(&space, &copy.to_string(), None).unwrap();
        // A delegate on the copy refuses and names the asking machine; a
        // refusal writes nothing, so a second id keeps the test honest.
        args.answer = Some("q-del@hostb".to_string());
        args.delegate = true;
        args.option = None;
        assert_eq!(
            run_answer(&home, &cwd, &args, "q-del@hostb"),
            2,
            "delegate on a copy refuses"
        );
        // The option answer records through the shared seam, keyed to the
        // copy id, and the projection reads the copy answered. The refusal
        // above set delegate and left option unset; clear both so this call
        // answers as an option.
        args.answer = Some("q-copy@hosta".to_string());
        args.delegate = false;
        args.option = Some(1);
        assert_eq!(
            run_answer(&home, &cwd, &args, "q-copy@hosta"),
            0,
            "the copy answer records"
        );
        let home2 = crate::paths::AgentsHome::from_env();
        let raw =
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home2), &[]);
        assert!(
            raw.contains("\"item_id\":\"q-copy@hosta\""),
            "the copy answer row landed: {raw}"
        );
        let fno_dir = home2.root().parent().map(Path::to_path_buf).unwrap();
        let (items, _, _) = crate::attention_arm::read_items_at(&fno_dir, &cwd);
        let item = items.iter().find(|i| i.id == "q-copy@hosta").unwrap();
        assert_eq!(item.state, "answered");
        // The receipt names the carrier.
        let receipt = answer_receipt("q-copy@hosta", Some("hosta"), false);
        assert_eq!(receipt["carries_to"], json!("hosta"));
        assert_eq!(answer_receipt("q-x", None, false).get("carries_to"), None);
    }

    #[test]
    fn the_archive_door_withdraws_open_items_and_refuses_the_rest() {
        let (dir, _root, mut args) = door_setup("needs_door_archive_");
        let cwd = dir.path().join("repo");
        let home = crate::paths::AgentsHome::from_env();
        let space = crate::paths::space_dir(&cwd).join("events.jsonl");
        let index = crate::provider_cap::questions_path(&home);
        let ask = |id: &str| {
            serde_json::json!({
                "ts": "2026-09-26T05:00:00Z", "type": "operator_question", "source": "test",
                "data": {"question_id": id, "question": "pick one", "asker": "w1",
                         "session_id": "s1", "cwd": "/repo/fno", "options": ["a", "b"]}
            })
        };
        crate::event_store::append_envelope(&space, &ask("q-two").to_string(), None).unwrap();
        for id in ["q-door", "q-two"] {
            crate::provider_cap::append_questions_row(&index, &ask(id)).unwrap();
        }
        args.answer = None;
        args.archive = vec!["q-door".into(), "q-two".into(), "q-none".into()];
        // An unknown id is refused by name; the open ones still close.
        assert_eq!(run_archive(&home, &cwd, &args, Some(Default::default())), 2);
        let closes = |path: &Path| {
            crate::event_store::journal_text(path, &[])
                .matches("operator_question_closed")
                .count()
        };
        assert_eq!(closes(&space), 2, "one project close each");
        assert_eq!(closes(&index), 2, "one index close each");
        let fno_dir = home.root().parent().map(Path::to_path_buf).unwrap();
        let (items, _, _) = crate::attention_arm::read_items_at(&fno_dir, &cwd);
        assert!(items.iter().all(|i| i.state != "open"), "{items:?}");
        // A closed id is refused and writes nothing more.
        args.archive = vec!["q-door".into()];
        assert_eq!(run_archive(&home, &cwd, &args, Some(Default::default())), 2);
        assert_eq!(closes(&index), 2);
        // --archive beside --answer is a usage error.
        let rest: Vec<String> = ["--archive", "q-door", "--answer", "q-door", "--sink", "mux"]
            .map(String::from)
            .to_vec();
        assert_eq!(
            tokio::runtime::Runtime::new()
                .unwrap()
                .block_on(run_needs(&rest, &home)),
            2
        );
    }

    #[test]
    fn ac1_edge_append_marks_a_second_row_superseded() {
        // First answer wins across writers: the second row records
        // superseded and changes nothing.
        let _root = crate::paths::DeclaredRoot::declare("needs_door_edge_sup");
        let home = crate::paths::AgentsHome::from_env();
        let first = crate::attention_arm::append_answer_row(
            &home,
            "q-sup",
            "questions",
            &crate::attention_file::FileAnswer::Words("narrow".to_string()),
            "file_edit",
            "attention_arm",
            "file:q-sup",
        )
        .unwrap();
        assert_eq!(first.1, false, "the first row wins");
        let second = crate::attention_arm::append_answer_row(
            &home,
            "q-sup",
            "mux",
            &crate::attention_file::FileAnswer::Words("wide".to_string()),
            "sink",
            "needs_door",
            "mux",
        )
        .unwrap();
        assert_eq!(second.1, true, "the second row records superseded");
        // A delegate row carries the flag and the delegate sentence as words.
        crate::attention_arm::append_answer_row(
            &home,
            "q-dlg",
            "mux",
            &crate::attention_file::FileAnswer::Delegate,
            "sink",
            "needs_door",
            "mux",
        )
        .unwrap();
        let raw =
            crate::event_store::journal_text(&crate::provider_cap::questions_path(&home), &[]);
        let row = raw.lines().find(|l| l.contains("q-dlg")).unwrap();
        assert!(row.contains("\"delegate\":true"), "{row}");
        assert!(row.contains(crate::attention_file::DELEGATE_TEXT), "{row}");
    }
}
