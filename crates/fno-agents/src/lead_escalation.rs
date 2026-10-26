//! The lead-escalation question renderer : one pure function set
//! behind a hidden JSON command, reached as `fno-agents lead-escalation-text`.
//! The lane's reconcile fold lives in Rust (`fleet_task::reconcile`, reached
//! from Python through the `fleet-task` transport); this side only renders
//! text from the reading the producer passed.
//!
//! A question states only a reading its writer handed over. An empty set is
//! not a reading, so the renderer refuses it as data (`ok: false`) instead of
//! minting "a board the lead could not read" for a board that was read fine.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

/// How many stalled rows the question names before it says "and N more".
const MAX_LISTED_IDS: usize = 20;

const MARKER: &str = "lead-escalation";

/// The `questions.route` decision: who else learns of an escalated
/// question beyond the board row. `direct`, the unset default, keeps the
/// question on the board alone - the relay hop the user retired is the
/// mail copy. `triage` is a team's opt-in to a triage lead. Anything
/// else degrades to direct and says so.
/// Returns `(mail_relay, nudge)`.
fn relay_mail(route: Option<&str>) -> (bool, Option<String>) {
    match route {
        Some("triage") => (true, None),
        Some(other) => (
            false,
            Some(format!(
                "questions.route reads '{other}' (direct or triage), so direct \
                 applies: the question stays on the board and no relay mail went \
                 out. For a decision only the user can make, file it with \
                 fno inbox outstanding ask. Set questions.route=triage in \
                 .fno/config.toml to route questions through a triage lead."
            )),
        ),
        None => (
            false,
            Some(
                "questions.route=direct (default): the question stays on the board \
                 and no relay mail went out. For a decision only the user can make, \
                 file it with fno inbox outstanding ask. Set questions.route=triage \
                 in .fno/config.toml to route questions through a triage lead."
                    .to_string(),
            ),
        ),
    }
}

/// The config candidates the route knob reads: the caller's cwd first, then
/// home, per key (the same cwd-then-home fallback `read_project_id` uses).
fn route_candidates(cwd: Option<&Path>, agents_home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(c) = cwd {
        candidates.push(c.join(".fno/config.toml"));
    }
    if let Some(h) = agents_home {
        // AgentsHome roots at ~/.fno/agents; config.toml lives at ~/.fno.
        if let Some(base) = h.parent() {
            candidates.push(base.join("config.toml"));
        }
    }
    candidates
}

/// Tolerant scan: the first candidate that sets `questions.route` wins.
fn route_from_configs(candidates: &[PathBuf]) -> Option<String> {
    for candidate in candidates {
        let Some(table) = crate::finalize::load_config_toml(candidate) else {
            continue;
        };
        if let Some(route) = crate::finalize::toml_string_at(&table, &["questions", "route"]) {
            return Some(route);
        }
    }
    None
}

// Reading ids are minted here so no call site writes a string literal that
// can drift from the parser below. A scope's commas become `+` so a
// comma-splitting consumer never cuts the id in half.
pub fn reading_board_unreadable() -> String {
    "reading:board-unreadable".to_owned()
}

pub fn reading_questions_unreadable() -> String {
    "reading:questions-unreadable".to_owned()
}

pub fn reading_sources_unreadable() -> String {
    "reading:sources-unreadable".to_owned()
}

pub fn reading_undelivered(scope: &str) -> String {
    format!("reading:undelivered:{}", scope.replace(',', "+"))
}

pub fn reading_delivery_unreadable(scope: &str) -> String {
    format!("reading:delivery-unreadable:{}", scope.replace(',', "+"))
}

pub fn reading_term_reached() -> String {
    "reading:term-reached".to_owned()
}

pub fn reading_term_unreadable() -> String {
    "reading:term-unreadable".to_owned()
}

pub fn reading_stale_team_doc() -> String {
    "reading:stale-team-doc".to_owned()
}

/// A verdict escalation states the verdict itself: a degraded or unknown
/// lead often has no stalled board row to name.
pub fn reading_verdict(scope: &str) -> String {
    format!("reading:verdict:{}", scope.replace(',', "+"))
}

#[derive(Deserialize)]
pub struct EscalationRequest {
    pub stalled: Vec<String>,
    pub key: String,
    pub reason: String,
    #[serde(default)]
    pub live: Option<bool>,
    #[serde(default)]
    pub unknown_reason: Option<String>,
    /// The lead verdict summary whose FIRST word is the verdict name
    /// (`stalled ...`, `degraded ...`). Anything else is a failed read and
    /// is recorded as `verdict unreadable` - never silent.
    #[serde(default)]
    pub verdict: Option<String>,
    /// The scope the verdict was read for; names the handoff offer's team.
    #[serde(default)]
    pub scope: Option<String>,
    /// The escalating session. Naming it scopes the rendered needle and the
    /// returned marker to that session's team scope, so the operator
    /// question channel keys per lead; absent or unreadable keeps the
    /// legacy shared channel.
    #[serde(default)]
    pub session_id: Option<String>,
    /// The `questions.route` knob, read by the transport from config and
    /// passed in so `render` stays deterministic. `None` reads as direct.
    #[serde(default)]
    pub route: Option<String>,
}

#[derive(Serialize)]
pub struct EscalationAnswer {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub question: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The fold marker the caller records the question under; present on
    /// every ok answer so the caller never re-derives the channel.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub marker: Option<String>,
    /// False under `questions.route=direct`: the caller skips the
    /// presiding lead's mail copy. The board row still records.
    pub mail_relay: bool,
    /// The teaching line the caller prints when the relay mail is skipped.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nudge: Option<String>,
}

fn refused(message: &str) -> EscalationAnswer {
    EscalationAnswer {
        ok: false,
        question: None,
        mail: None,
        message: Some(format!("lead escalation refused: {message}")),
        marker: None,
        mail_relay: false,
        nudge: None,
    }
}

fn subject_for_rows(ids: &[String]) -> String {
    let shown = ids[..ids.len().min(MAX_LISTED_IDS)].join(", ");
    let shown = if ids.len() > MAX_LISTED_IDS {
        format!("{shown}, and {} more", ids.len() - MAX_LISTED_IDS)
    } else {
        shown
    };
    format!(
        "{count} board row(s) nothing is clearing: {shown}",
        count = ids.len()
    )
}

/// One known `reading:` id -> the subject sentence it asserts. Returns None
/// for an unknown kind; the caller refuses.
fn subject_for_reading(id: &str) -> Option<String> {
    let scope_of = |prefix: &str| id.strip_prefix(prefix).map(|s| s.replace('+', ", "));
    if let Some(scope) = scope_of("reading:undelivered:") {
        return Some(format!(
            "a quiet board: no row is actionable, but scope {scope} still has undelivered nodes"
        ));
    }
    if let Some(scope) = scope_of("reading:delivery-unreadable:") {
        return Some(format!(
            "a quiet board whose delivery count for scope {scope} it could not read"
        ));
    }
    if let Some(scope) = scope_of("reading:verdict:") {
        return Some(if scope.is_empty() {
            "its own verdict, with no board row stalled".to_owned()
        } else {
            format!("its own verdict for scope {scope}, with no board row stalled")
        });
    }
    match id {
        "reading:board-unreadable" => Some("a board it could not read".to_owned()),
        "reading:questions-unreadable" => {
            Some("a clean board whose operator-question queue it could not read".to_owned())
        }
        "reading:sources-unreadable" => {
            Some("a quiet board with queues it could not read".to_owned())
        }
        _ => None,
    }
}

fn closing_for(live: Option<bool>, unknown_reason: Option<&str>, rows: bool) -> String {
    let mut closing = if live == Some(true) {
        if rows {
            "It is still leading and holding these rows, so decide whether to \
             unblock them, defer them, or tell it to stand down."
                .to_owned()
        } else {
            "It is still leading, so decide whether to act on this reading or \
             tell it to stand down."
                .to_owned()
        }
    } else {
        if rows {
            "It has exited, so nothing restarts it on its own - decide whether \
             to unblock these rows, defer them, or team a new lead."
                .to_owned()
        } else {
            "It has exited, so nothing restarts it on its own - decide whether \
             to act on this reading or team a new lead."
                .to_owned()
        }
    };
    if live.is_none() {
        if let Some(reason) = unknown_reason {
            closing.push_str(&format!(" (liveness unreadable: {reason})"));
        }
    }
    closing
}

/// the lead verdict rides the question. `stalled`/`degraded`/`unknown` append
/// the spawn `--succeed` handoff offer; `unreadable` is a failed verdict
/// read, and it is named - escalation never goes silent on it. Any other
/// verdict (a converging lead stops on an actionable-board escalation)
/// rides truthfully as `Verdict ...`. The handoff scope is the explicit one,
/// else the quiet-board reading's scope (`+` minted back to `,`), else the
/// `<scope>` placeholder.
fn append_verdict(closing: &mut String, verdict: &str, scope: Option<&str>, readings: &[String]) {
    let name = verdict.split(' ').next().unwrap_or("");
    if name == "stalled" || name == "degraded" || name == "unknown" {
        let handoff = scope
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                readings.iter().find_map(|r| {
                    r.strip_prefix("reading:undelivered:")
                        .or_else(|| r.strip_prefix("reading:verdict:"))
                        .filter(|s| !s.is_empty())
                        .map(|s| s.replace('+', ","))
                })
            })
            .unwrap_or_else(|| "<scope>".to_owned());
        closing.push_str(&format!(
            " Verdict {verdict}. To hand this team to a fresh-context \
             successor, run fno agents spawn --promote {handoff} --hand-off."
        ));
    } else if name == "unreadable" {
        let detail = verdict
            .split_once(' ')
            .map(|(_, rest)| rest)
            .unwrap_or("read failed");
        closing.push_str(&format!(" verdict unreadable: {detail}."));
    } else {
        closing.push_str(&format!(" Verdict {verdict}."));
    }
}

/// The one entry point both branches fold through: the marker leads because
/// the recorded text is capped and `already_asked` must keep matching.
pub fn render(req: &EscalationRequest) -> EscalationAnswer {
    let mut rows: Vec<String> = vec![];
    let mut readings: Vec<String> = vec![];
    for id in &req.stalled {
        if id.starts_with("reading:") {
            readings.push(id.clone());
        } else {
            rows.push(id.clone());
        }
    }
    if rows.is_empty() && readings.is_empty() {
        return refused(
            "the stalled set is empty; an escalation states the reading its \
             producer passed, and an empty set states none",
        );
    }
    if !rows.is_empty() && !readings.is_empty() {
        return refused(&format!(
            "reading id(s) [{}] mixed with board row(s) [{}]: one question \
             states one reading of the board",
            readings.join(", "),
            rows.join(", ")
        ));
    }
    if readings.len() > 1 {
        return refused(&format!(
            "multiple readings [{}]: one question states one reading",
            readings.join(", ")
        ));
    }

    let (subject, rows_branch) = if readings.len() == 1 {
        let reading = &readings[0];
        if reading.starts_with("reading:verdict:") {
            let name = req.verdict.as_deref().and_then(|v| v.split(' ').next());
            match name {
                None => return refused("a verdict reading carries no verdict"),
                Some("converging") => {
                    return refused("the verdict reads converging; it states nothing to escalate")
                }
                Some(_) => {}
            }
        }
        match subject_for_reading(reading) {
            Some(s) => (s, false),
            None => return refused(&format!("unknown reading id [{reading}]")),
        }
    } else {
        rows.sort();
        rows.dedup();
        (subject_for_rows(&rows), true)
    };

    let mut closing = closing_for(req.live, req.unknown_reason.as_deref(), rows_branch);
    if let Some(verdict) = req.verdict.as_deref() {
        append_verdict(&mut closing, verdict, req.scope.as_deref(), &readings);
    }
    // The per-lead channel: naming the session scopes both the rendered
    // needle and the returned marker to that session's team scope; an
    // unreadable registry keeps the legacy shared channel.
    let marker = match req.session_id.as_deref() {
        Some(sid) => crate::paths::AgentsHome::from_env_opt()
            .and_then(|home| crate::state::load_registry(&home.registry_json()).ok())
            .map(|registry| scoped_marker(&registry.entries, sid))
            .unwrap_or_else(|| MARKER.to_owned()),
        None => MARKER.to_owned(),
    };
    let question = format!(
        "[{marker}:{key}] The lead stopped on {subject}. Reason given: {reason}. {closing}",
        key = req.key,
        reason = req.reason,
    );
    let mail = format!(
        "A team under yours stopped on {subject}. Reason given: {reason}. \
         It presides over territory yours contains - check on it before this \
         reaches the operator.",
        subject = subject,
        reason = req.reason,
    );
    let (mail_relay, nudge) = relay_mail(req.route.as_deref());
    EscalationAnswer {
        ok: true,
        question: Some(question),
        mail: Some(mail),
        message: None,
        marker: Some(marker),
        mail_relay,
        nudge,
    }
}

/// `fno-agents lead-escalation-text`: the hidden binary-direct transport. One
/// JSON request on stdin, one JSON answer on stdout, exit 0 whenever an
/// answer was computed (a refusal is data); exit 2 on malformed args or an
/// unreadable request.
pub fn run_lead_escalation_text(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("usage: fno-agents lead-escalation-text (one JSON request on stdin)");
        return 0;
    }
    if !args.is_empty() {
        eprintln!("fno-agents lead-escalation-text: unexpected arguments; the request rides stdin");
        return 2;
    }
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        eprintln!("fno-agents lead-escalation-text: could not read stdin");
        return 2;
    }
    let mut req: EscalationRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fno-agents lead-escalation-text: bad request: {e}");
            return 2;
        }
    };
    // The knob read lives here, not in `render`: the renderer stays
    // deterministic on its request, and the tests pin the route through the
    // field the transport fills.
    if req.route.is_none() {
        req.route = route_from_configs(&route_candidates(
            std::env::current_dir().ok().as_deref(),
            crate::paths::AgentsHome::from_env_opt()
                .map(|h| h.root().to_path_buf())
                .as_deref(),
        ));
    }
    let answer = serde_json::to_string(&render(&req)).expect("serializes");
    println!("{answer}");
    0
}

/// The escalation channel's per-lead scope read: resolve the escalating
/// session's registry row and return its `role_scope`. Session ids compare
/// under the crate's one identity contract (`claims::same_session_id`), and
/// a blank scope reads as None, keeping a malformed row on the legacy
/// shared channel.
fn escalation_scope(entries: &[crate::state::RegistryEntry], session_id: &str) -> Option<String> {
    let row = entries.iter().find(|e| {
        e.harness_session_id
            .as_deref()
            .is_some_and(|sid| crate::claims::same_session_id(sid, session_id))
    })?;
    row.role_scope
        .clone()
        .filter(|scope| !scope.trim().is_empty())
}

/// The ask's fold marker, scoped by the lead's team scope. A scopeless
/// session keeps the legacy shared channel. The needle key stays the
/// caller's dedupe key: the text reads `[{marker}:{key}]`.
fn scoped_marker(entries: &[crate::state::RegistryEntry], session_id: &str) -> String {
    match escalation_scope(entries, session_id) {
        Some(scope) => format!("{MARKER}:{scope}"),
        None => MARKER.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDS: [&str; 2] = ["x-1111", "x-2222"];
    const KEY: &str = "0a1b2c3d4e5f";
    const REASON: &str = "NoProgress";

    fn req(stalled: Vec<&str>, live: Option<bool>) -> EscalationRequest {
        EscalationRequest {
            stalled: stalled.into_iter().map(str::to_owned).collect(),
            key: KEY.to_owned(),
            reason: REASON.to_owned(),
            live,
            unknown_reason: None,
            verdict: None,
            scope: None,
            session_id: None,
            route: None,
        }
    }

    // --- the questions.route decision ---

    #[test]
    fn direct_route_skips_the_relay_mail_and_teaches_the_board() {
        let (mail_relay, nudge) = relay_mail(None);
        assert!(!mail_relay);
        let nudge = nudge.expect("direct carries the teaching line");
        assert!(nudge.contains("fno inbox outstanding ask"));
        assert!(nudge.contains("questions.route=triage"));

        let ans = render(&req(IDS.to_vec(), Some(true)));
        assert!(ans.ok);
        assert!(!ans.mail_relay);
        assert!(ans.nudge.is_some());
    }

    #[test]
    fn triage_keeps_the_mail_and_stays_quiet() {
        let (mail_relay, nudge) = relay_mail(Some("triage"));
        assert!(mail_relay);
        assert!(nudge.is_none());

        let mut r = req(IDS.to_vec(), Some(true));
        r.route = Some("triage".to_owned());
        let ans = render(&r);
        assert!(ans.mail_relay);
        assert!(ans.nudge.is_none());
    }

    #[test]
    fn an_unknown_route_degrades_to_direct_and_names_the_value() {
        let (mail_relay, nudge) = relay_mail(Some("ladder"));
        assert!(!mail_relay);
        let nudge = nudge.expect("unknown carries the teaching line");
        assert!(nudge.contains("reads 'ladder'"));
        assert!(nudge.contains("fno inbox outstanding ask"));
    }

    #[test]
    fn the_route_candidate_list_reads_cwd_then_agents_home() {
        let cwd = Path::new("/repo");
        let home = Path::new("/u/.fno/agents");
        let candidates = route_candidates(Some(cwd), Some(home));
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/repo/.fno/config.toml"),
                PathBuf::from("/u/.fno/config.toml"),
            ]
        );
        assert!(route_candidates(None, None).is_empty());
    }

    #[test]
    fn route_scan_reads_the_first_candidate_that_sets_the_key() {
        let dir = std::env::temp_dir().join(format!("lead-esc-route-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bare = dir.join("bare.toml");
        std::fs::write(&bare, "[project]\nid = \"fno\"\n").unwrap();
        let set = dir.join("set.toml");
        std::fs::write(
            &set,
            "[project]\nid = \"fno\"\n[questions]\nroute = \"triage\"\n",
        )
        .unwrap();
        // A file without the key keeps the caller looking; a bad table reads
        // as absent.
        assert_eq!(
            route_from_configs(&[bare.clone(), set.clone()]).as_deref(),
            Some("triage")
        );
        assert_eq!(route_from_configs(&[bare.clone()]), None);
        let garbage = dir.join("garbage.toml");
        std::fs::write(&garbage, "not toml at all [[[").unwrap();
        assert_eq!(
            route_from_configs(&[garbage, set]).as_deref(),
            Some("triage")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn question(req: &EscalationRequest) -> String {
        render(req).question.expect("ok case carries a question")
    }

    // --- AC2-HP: the row branch is byte-identical to today's Python text ---

    #[test]
    fn row_text_is_byte_identical_to_the_python_it_ports() {
        for live in [Some(true), Some(false), None] {
            let text = question(&req(IDS.to_vec(), live));
            let closing = match live {
                Some(true) => "It is still leading and holding these rows, so \
                               decide whether to unblock them, defer them, or \
                               tell it to stand down."
                    .to_owned(),
                Some(false) => "It has exited, so nothing restarts it on its \
                                own - decide whether to unblock these rows, \
                                defer them, or team a new lead."
                    .to_owned(),
                None => "It has exited, so nothing restarts it on its own - \
                         decide whether to unblock these rows, defer them, or \
                         team a new lead."
                    .to_owned(),
            };
            let want = format!(
                "[lead-escalation:{KEY}] The lead stopped on 2 board row(s) \
                 nothing is clearing: x-1111, x-2222. Reason given: NoProgress. \
                 {closing}"
            );
            assert_eq!(text, want, "live={live:?}");
        }
    }

    // --- the reading branch (AC1-HP, AC6-HP) ---

    #[test]
    fn every_reading_kind_carries_its_subject() {
        let cases = [
            (reading_board_unreadable(), "a board it could not read"),
            (
                reading_questions_unreadable(),
                "a clean board whose operator-question queue it could not read",
            ),
            (
                reading_sources_unreadable(),
                "a quiet board with queues it could not read",
            ),
            (
                reading_undelivered("x-aaaa"),
                "a quiet board: no row is actionable, but scope x-aaaa still has undelivered nodes",
            ),
            (
                reading_delivery_unreadable("x-aaaa"),
                "a quiet board whose delivery count for scope x-aaaa it could not read",
            ),
        ];
        for (id, subject) in cases {
            let text = question(&req(vec![id.as_str()], Some(true)));
            assert!(text.contains(subject), "{id}: {text}");
        }
    }

    // --- AC4: the verdict rides the question, never silent ---

    fn with_verdict(
        mut r: EscalationRequest,
        verdict: &str,
        scope: Option<&str>,
    ) -> EscalationRequest {
        r.verdict = Some(verdict.to_owned());
        r.scope = scope.map(str::to_owned);
        r
    }

    #[test]
    fn a_stalled_verdict_carries_the_handoff_offer() {
        let text = question(&with_verdict(
            req(IDS.to_vec(), Some(true)),
            "stalled 16, 13, 10: undelivered is not falling",
            Some("x-bbbb"),
        ));
        assert!(text.starts_with(&format!("[{MARKER}:{KEY}]")));
        assert!(text.contains("Verdict stalled 16, 13, 10: undelivered is not falling."));
        assert!(text.contains("run fno agents spawn --promote x-bbbb --hand-off"));
    }

    #[test]
    fn the_quiet_reading_names_the_handoff_scope_when_none_is_explicit() {
        let text = question(&with_verdict(
            req(vec!["reading:undelivered:x-bbbb+x-1111"], Some(true)),
            "stalled undelivered 9",
            None,
        ));
        assert!(text.contains("--promote x-bbbb,x-1111 --hand-off"));
    }

    #[test]
    fn a_verdict_reading_escalates_with_no_stalled_row() {
        let reading = reading_verdict("fnh,fno");
        let text = question(&with_verdict(
            req(vec![reading.as_str()], Some(true)),
            "degraded compactions 12 of 3",
            None,
        ));
        assert!(text.contains("stopped on its own verdict for scope fnh, fno"));
        assert!(text.contains("Verdict degraded compactions 12 of 3."));
        assert!(text.contains("--promote fnh,fno --hand-off"));

        let ans = render(&with_verdict(
            req(vec![reading.as_str()], Some(true)),
            "converging",
            None,
        ));
        assert!(!ans.ok);
        assert!(ans
            .message
            .expect("refusal names itself")
            .contains("converging"));
    }

    #[test]
    fn a_failed_verdict_read_is_named_not_silent() {
        let text = question(&with_verdict(
            req(IDS.to_vec(), Some(true)),
            "unreadable registry unreadable: disk",
            Some("x-bbbb"),
        ));
        assert!(text.contains("verdict unreadable: registry unreadable: disk."));
        assert!(!text.contains("--succeed"));
    }

    // --- AC5-ERR: refusals are data ---

    #[test]
    fn empty_set_is_refused() {
        let ans = render(&req(vec![], None));
        assert!(!ans.ok);
        let msg = ans.message.expect("refusal names itself");
        assert!(msg.contains("lead escalation refused"));
        assert!(msg.contains("empty"));
    }

    #[test]
    fn reading_mixed_with_rows_is_refused() {
        let ans = render(&req(vec!["reading:board-unreadable", "x-1111"], Some(true)));
        assert!(!ans.ok);
        let msg = ans.message.expect("refusal names itself");
        assert!(msg.contains("reading:board-unreadable"));
        assert!(msg.contains("x-1111"));
    }

    #[test]
    fn two_readings_are_refused() {
        let ans = render(&req(
            vec!["reading:board-unreadable", "reading:undelivered:x-1"],
            None,
        ));
        assert!(!ans.ok);
        let msg = ans.message.expect("refusal names itself");
        assert!(msg.contains("multiple readings"));
        assert!(msg.contains("reading:board-unreadable"));
    }

    #[test]
    fn unknown_reading_kind_is_refused() {
        let ans = render(&req(vec!["reading:bogus"], None));
        assert!(!ans.ok);
        let msg = ans.message.expect("refusal names itself");
        assert!(msg.contains("unknown reading id [reading:bogus]"));
    }

    // --- the mail twin ---

    #[test]
    fn mail_names_the_subject_and_reason() {
        let ans = render(&req(vec!["reading:undelivered:x-aaaa"], Some(true)));
        let mail = ans.mail.expect("ok case carries mail");
        assert!(mail.contains("A team under yours stopped on a quiet board:"));
        assert!(mail.contains("Reason given: NoProgress"));
    }

    // --- the per-lead scope read ---

    fn registry_row(sid: &str, scope: Option<&str>) -> crate::state::RegistryEntry {
        crate::state::RegistryEntry {
            name: "lead".to_owned(),
            harness_session_id: Some(sid.to_owned()),
            role_scope: scope.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn scope_resolution_reads_blank_scope_and_unknown_session_as_none() {
        let rows = vec![
            registry_row("plain-session", Some("  ")),
            registry_row("teamed-session", Some("fno")),
            registry_row("ses_kept1", Some("reaper")),
        ];
        assert!(escalation_scope(&rows, "plain-session").is_none());
        assert!(escalation_scope(&rows, "no-such-session").is_none());
        assert_eq!(
            escalation_scope(&rows, "ses_kept1").as_deref(),
            Some("reaper")
        );
    }

    #[test]
    fn scoped_marker_names_the_teamed_session_only() {
        let rows = vec![registry_row("lead-a-session", Some("fno"))];
        assert_eq!(
            scoped_marker(&rows, "lead-a-session"),
            "lead-escalation:fno"
        );
        assert_eq!(scoped_marker(&rows, "no-such-session"), MARKER);
    }
}
