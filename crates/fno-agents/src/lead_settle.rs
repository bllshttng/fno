//! The lead_settle arm: one pass over the org payload that mails the owning
//! lead when a covered PR settles green and when a covered node merges and
//! closes. It rides the `arm_watch` tick's org read, so the lead arms no
//! watch and relaunches nothing: the daemon is the push half, the check-in
//! beat stays the backstop.

use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The open-PR listing's row cap. A repo with more open PRs than this
/// answers the covered PRs the listing carries and names the cap in the
/// note; the rest read as absent until the count drops.
const LIST_PR_LIMIT: usize = 200;

/// The listing read's wall-budget floor; load scales it up to the 150s
/// cap, the same shape the org read applies.
const STATUS_READ_BUDGET_S: u64 = 30;

/// One pass's receipt: what the tick row journals.
pub struct SettleOutcome {
    pub mailed: u64,
    pub reads: u64,
    pub note: String,
}

/// The seen key: proof this pass covered the node, so a later beat can see
/// the node leave the fold.
fn seen_key(scope: &str, node: &str) -> String {
    format!("lead_settle_seen:{scope}:{node}")
}

/// The settle key: the send-once token per covered node.
fn settle_key(scope: &str, node: &str) -> String {
    format!("lead_settle:{scope}:{node}")
}

fn head12(head: &str) -> String {
    head.chars().take(12).collect()
}

/// One listed PR's settle fields: what the green pass mails from. Derived
/// with `fno do pr status`'s own pure helpers (`verdict_for`, the settled
/// rule), so the cheap listing cannot drift from the heavy payload's
/// verdict.
struct SettleView {
    settled: bool,
    green: bool,
    head: String,
    ready: bool,
    blockers: String,
}

fn settle_view(row: &Value) -> Option<SettleView> {
    let rollup = row.get("statusCheckRollup")?.as_array()?;
    let (verdict, _, counts) =
        crate::pr_status::verdict_for(&crate::pr_status::without_coverage_statuses(rollup));
    let unsettled = counts.get("unsettled").and_then(Value::as_i64).unwrap_or(0);
    let total = counts.get("total").and_then(Value::as_i64).unwrap_or(0);
    let head = row
        .get("headRefOid")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string(); // The listing rides GraphQL, so the merge state arrives UPPERCASE; the
                      // lowercased word is the form the REST payloads and readers expect.
    let state = row
        .get("mergeStateStatus")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_lowercase();
    let ready =
        row.get("mergeable").and_then(Value::as_str) == Some("MERGEABLE") && state == "clean";
    let blockers = if ready {
        String::new()
    } else if state.is_empty() {
        "merge state unread".to_string()
    } else {
        format!("merge state {state}")
    };
    Some(SettleView {
        settled: verdict != "unknown" && total > 0 && unsettled == 0,
        green: verdict == "green",
        head,
        ready,
        blockers,
    })
}

/// The pure pass. `graph` reads one PR's node rows, `list` reads one repo's
/// open-PR listing from a covered node's cwd, and `mail` sends one team
/// mail; all handed in so a test drives the whole pass.
pub(crate) fn evaluate(
    payload: &Value,
    store: &Path,
    graph: &dyn Fn(i64) -> Result<Vec<Value>, String>,
    list: &mut dyn FnMut(&Path) -> Result<Vec<Value>, String>,
    mail: &mut dyn FnMut(&str, &str) -> bool,
    now_unix: u64,
) -> SettleOutcome {
    let mut notes: Vec<String> = Vec::new();
    let mut mailed: u64 = 0;
    let mut reads: u64 = 0;
    let Some(teams) = payload.get("teams").and_then(Value::as_array) else {
        return SettleOutcome {
            mailed: 0,
            reads: 0,
            note: "the org payload carries no teams list".to_string(),
        };
    };
    let mut scopes_seen: Vec<(String, bool)> = Vec::new();
    // (scope, node, pr, cwd) - the covered set this pass answers for.
    let mut covered: Vec<(String, String, i64, PathBuf)> = Vec::new();
    for team in teams {
        let Some(scope) = team.get("scope").and_then(Value::as_str) else {
            continue;
        };
        let Some(fold) = team.get("scope_nodes") else {
            notes.push(format!("team {scope} carries no fold"));
            continue;
        };
        if fold.get("status").and_then(Value::as_str) != Some("ok") {
            let reason = fold
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("the fold did not run");
            notes.push(format!("team {scope} fold unread: {reason}"));
            continue;
        }
        if !scopes_seen.iter().any(|(s, _)| s == scope) {
            // The freed scan runs only when the fold's owners resolved: a
            // transient owner failure nulls every owned field, and reading
            // that as "the node left the fold" would forget keys a good
            // beat needed.
            let owners_ok = fold.get("owned_total").is_some_and(Value::is_number);
            scopes_seen.push((scope.to_string(), owners_ok));
        }
        let Some(nodes) = fold.get("nodes").and_then(Value::as_array) else {
            continue;
        };
        for node in nodes {
            let owned = node.get("owned").and_then(Value::as_bool) == Some(true);
            let pr = node
                .get("pr_number")
                .and_then(Value::as_i64)
                .filter(|pr| *pr > 0);
            let (Some(node_id), Some(pr), Some(cwd)) = (
                node.get("id").and_then(Value::as_str),
                pr,
                node.get("cwd").and_then(Value::as_str),
            ) else {
                continue;
            };
            if !owned {
                continue;
            }
            covered.push((
                scope.to_string(),
                node_id.to_string(),
                pr,
                PathBuf::from(cwd),
            ));
        }
    }
    // The seen stamp covers every covered node, read or not: the freed pass
    // needs the node's PR number the beat it leaves the fold.
    for (scope, node, pr, _) in &covered {
        crate::operator_notice::mark_once(store, &seen_key(scope, node), &pr.to_string());
    }
    // The green pass: one mail per covered PR that settled green at a head
    // this arm has not mailed. One open-PR listing answers every covered
    // PR in the repo, so a covered PR absent from it has left the open set
    // (a merged PR also leaves it; the freed pass answers for that), and a
    // failed listing reads one note, not one per PR.
    let mut listings: HashMap<PathBuf, Option<Vec<Value>>> = HashMap::new();
    for (scope, node, pr, cwd) in &covered {
        let rows = listings.entry(cwd.clone()).or_insert_with(|| {
            reads += 1;
            match list(cwd) {
                Ok(rows) => {
                    if rows.len() >= LIST_PR_LIMIT {
                        notes.push(format!(
                            "the open-PR listing in {} hit its {LIST_PR_LIMIT}-row cap; \
                             covered PRs beyond it read as absent until the count drops",
                            cwd.display()
                        ));
                    }
                    Some(rows)
                }
                Err(reason) => {
                    notes.push(format!(
                        "the open-PR listing in {} unread: {reason}",
                        cwd.display()
                    ));
                    None
                }
            }
        });
        let Some(rows) = rows else { continue };
        let Some(row) = rows
            .iter()
            .find(|row| row.get("number").and_then(Value::as_i64) == Some(*pr))
        else {
            continue;
        };
        let Some(view) = settle_view(row) else {
            continue;
        };
        if !view.settled || !view.green || view.head.is_empty() {
            continue;
        }
        let text = if view.ready {
            format!(
                "PR #{pr} on {node} settled green at {}. GitHub reads it clean.",
                head12(&view.head)
            )
        } else {
            format!(
                "PR #{pr} on {node} settled green at {}. Blockers: {}.",
                head12(&view.head),
                view.blockers
            )
        };
        let token = format!(
            "green:{pr}@{}:{}",
            view.head,
            if view.ready { "ready" } else { "blocked" }
        );
        let key = settle_key(scope, node);
        let scope_for_mail = scope.clone();
        match crate::operator_notice::notify_signal_via(
            store,
            now_unix,
            0,
            &key,
            &token,
            "lead settle",
            &text,
            None,
            || mail(&scope_for_mail, &text),
        ) {
            crate::operator_notice::Verdict::Sent => mailed += 1,
            crate::operator_notice::Verdict::SendFailed => notes.push(format!(
                "PR #{pr} on {node} mail failed; the next beat retries"
            )),
            _ => {}
        }
    }
    // The freed pass: a node this arm saw covered whose scope still folds but
    // which left the covered set. Its graph row decides whether it merged.
    for (scope, owners_ok) in &scopes_seen {
        if !owners_ok {
            notes.push(format!(
                "team {scope} owners unread; the freed scan keeps its keys"
            ));
            continue;
        }
        let still: Vec<&str> = covered
            .iter()
            .filter(|(s, _, _, _)| s == scope)
            .map(|(_, n, _, _)| n.as_str())
            .collect();
        let prefix = format!("lead_settle_seen:{scope}:");
        for key in crate::operator_notice::keys_with_prefix(store, &prefix) {
            let Some(node) = key.rsplit_once(':').map(|(_, node)| node.to_string()) else {
                continue;
            };
            if still.contains(&node.as_str()) {
                continue;
            }
            let Some(pr) = crate::operator_notice::stored_token(store, &key)
                .and_then(|token| token.parse::<i64>().ok())
            else {
                // A key with no readable PR can never resolve; drop it.
                crate::operator_notice::forget_at(store, &key);
                continue;
            };
            match graph(pr) {
                Err(reason) => {
                    notes.push(format!("PR #{pr} on {node} graph unread: {reason}"));
                }
                Ok(rows) => {
                    let closed = rows
                        .iter()
                        .find(|row| row.get("id").and_then(Value::as_str) == Some(node.as_str()))
                        .map(|row| {
                            row.get("status").and_then(Value::as_str) == Some("done")
                                || row.get("merge_status").and_then(Value::as_str) == Some("merged")
                        })
                        .unwrap_or(false);
                    if !closed {
                        // The row reads any other status: the node is no
                        // longer this arm's business.
                        crate::operator_notice::forget_at(store, &key);
                        crate::operator_notice::forget_at(store, &settle_key(scope, &node));
                        continue;
                    }
                    let text = format!(
                        "PR #{pr} on {node} merged and the node closed. Plan the next dispatch."
                    );
                    let key_settle = settle_key(scope, &node);
                    let scope_for_mail = scope.clone();
                    match crate::operator_notice::notify_signal_via(
                        store,
                        now_unix,
                        0,
                        &key_settle,
                        &format!("freed:{pr}"),
                        "lead settle",
                        &text,
                        None,
                        || mail(&scope_for_mail, &text),
                    ) {
                        crate::operator_notice::Verdict::Sent => {
                            mailed += 1;
                            crate::operator_notice::forget_at(store, &key);
                            crate::operator_notice::forget_at(store, &key_settle);
                        }
                        crate::operator_notice::Verdict::Deduped => {
                            crate::operator_notice::forget_at(store, &key);
                            crate::operator_notice::forget_at(store, &key_settle);
                        }
                        crate::operator_notice::Verdict::SendFailed
                        | crate::operator_notice::Verdict::RateHeld => {
                            // Keep the seen key: the next beat retries.
                            notes.push(format!(
                                "PR #{pr} on {node} mail failed; the next beat retries"
                            ));
                        }
                    }
                }
            }
        }
    }
    // A scope that left the payload takes its keys with it, the same cleanup
    // the team alarm's clocks get.
    for key in crate::operator_notice::keys_with_prefix(store, "lead_settle_seen:") {
        let departed = key
            .strip_prefix("lead_settle_seen:")
            .and_then(|rest| rest.split(':').next())
            .is_some_and(|scope| !scopes_seen.iter().any(|(s, _)| s == scope));
        if departed {
            crate::operator_notice::forget_at(store, &key);
        }
    }
    for key in crate::operator_notice::keys_with_prefix(store, "lead_settle:") {
        let departed = key
            .strip_prefix("lead_settle:")
            .and_then(|rest| rest.split(':').next())
            .is_some_and(|scope| !scopes_seen.iter().any(|(s, _)| s == scope));
        if departed {
            crate::operator_notice::forget_at(store, &key);
        }
    }
    SettleOutcome {
        mailed,
        reads,
        note: notes.join("; "),
    }
}

/// The daemon-facing pass over the org payload the arm_watch tick already
/// read. Every child is bounded; a failed read or send names itself in the
/// note and never mails a half answer. One open-PR listing per covered
/// repo answers every settle question in one bounded gh spawn, and the
/// mail leg is the in-process `lead_mail::send` - the daemon appends the
/// durable envelope itself, so no mail child runs at all.
pub fn run(payload: &Value, config_cwd: &Path, now_unix: u64) -> SettleOutcome {
    let store = crate::operator_notice::notify_signals_path();
    let graph_path = crate::org_board::scope::graph_json_path(config_cwd);
    let graph = |pr: i64| -> Result<Vec<Value>, String> {
        crate::graph_store::read_pr_rows(&graph_path, Some(pr)).map_err(|e| e.to_string())
    };
    // The same load-scaled shape the org read uses: a starved machine's
    // read dies at a fixed bound every beat, and the pass mails nothing
    // while the arm reads error.
    let status_budget_s = crate::bounded_cmd::load_scaled_budget_s(STATUS_READ_BUDGET_S, 150);
    let limit = LIST_PR_LIMIT.to_string();
    let fields = "number,headRefOid,statusCheckRollup,mergeable,mergeStateStatus";
    let mut list = |cwd: &Path| -> Result<Vec<Value>, String> {
        let args = [
            "pr",
            "list",
            "--state",
            "open",
            "--limit",
            limit.as_str(),
            "--json",
            fields,
        ];
        let out = crate::loopcheck::bounded_read(
            std::ffi::OsStr::new("gh"),
            &args,
            cwd,
            "lead-settle listing",
            std::time::Duration::from_secs(status_budget_s),
        )
        .map_err(|error| crate::loopcheck::bounded_read_diagnostic("lead-settle", &error))?;
        if !out.status.success() {
            return Err(format!(
                "gh pr list failed: {}",
                String::from_utf8_lossy(&out.stderr_tail).trim()
            ));
        }
        serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("the open-PR listing did not parse: {e}"))
    };
    let mut mail = |scope: &str, text: &str| -> bool {
        crate::lead_mail::send(crate::lead_mail::SETTLE_SENDER, scope, text).is_ok()
    };
    evaluate(payload, &store, &graph, &mut list, &mut mail, now_unix)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    const NOW: u64 = 1_788_523_200; // 2026-09-04T12:00 to the second

    fn temp_store(name: &str) -> PathBuf {
        // pid + counter: same-process tests must not share a store.
        static SEQ: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "fno-lead-settle-{}-{}-{name}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ))
    }

    type Log = Rc<RefCell<Vec<String>>>;

    fn mail_log(log: Log) -> impl FnMut(&str, &str) -> bool {
        move |scope: &str, text: &str| {
            log.borrow_mut().push(format!("{scope}: {text}"));
            true
        }
    }

    /// A listing that must never be read: a pass with no covered nodes.
    fn panic_listing() -> impl FnMut(&Path) -> Result<Vec<Value>, String> {
        |_cwd: &Path| panic!("a pass with no covered nodes reads no listing")
    }

    /// One open-PR listing row: the GraphQL field names `gh pr list --json`
    /// prints.
    fn listed_row(number: i64, head: &str, mergeable: &str, state: &str, rollup: Value) -> Value {
        json!({
            "number": number,
            "headRefOid": head,
            "mergeable": mergeable,
            "mergeStateStatus": state,
            "statusCheckRollup": rollup,
        })
    }

    fn green_rollup() -> Value {
        json!([{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}])
    }

    fn red_rollup() -> Value {
        json!([{"name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}])
    }

    fn team(scope: &str, nodes: Value) -> Value {
        json!({
            "holder": "sess-1", "level": 1, "scope": scope,
            "status": "manifest-only",
            "scope_nodes": {"status": "ok", "owned_total": 1, "nodes": nodes},
        })
    }

    fn covered_node(id: &str, pr: i64, owned: Value) -> Value {
        json!({"id": id, "owned": owned, "pr_number": pr, "cwd": "/r"})
    }

    /// AC1: one green settle mails once; the second pass dedupes.
    #[test]
    fn a_green_settle_mails_once_and_the_second_pass_is_quiet() {
        let store = temp_store("ac1");
        let payload = json!({"teams": [team("s1", json!([covered_node("x-1", 7, json!(true))]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let rows = vec![listed_row(
            7,
            "abcdef1234567890",
            "MERGEABLE",
            "CLEAN",
            green_rollup(),
        )];
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> { Ok(rows.clone()) };
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 1);
        assert_eq!(out.reads, 1, "one listing answers every covered PR");
        assert_eq!(log.borrow().len(), 1);
        assert!(log.borrow()[0].contains("s1: PR #7 on x-1 settled green at abcdef123456"));
        assert!(log.borrow()[0].contains("GitHub reads it clean."));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW + 300);
        assert_eq!(out.mailed, 0);
        assert_eq!(log.borrow().len(), 1);
    }

    /// AC4: a covered node that left the fold with a done row mails the
    /// freed notice and both keys leave the store.
    #[test]
    fn a_node_that_merged_and_closed_mails_freed_and_forgets() {
        let store = temp_store("ac4");
        crate::operator_notice::mark_once(&store, &seen_key("s1", "x-1"), "7");
        let payload = json!({"teams": [team("s1", json!([]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> {
            Ok(vec![json!({"id": "x-1", "status": "done"})])
        };
        let mut list = panic_listing();
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 1);
        assert!(log.borrow()[0].contains("PR #7 on x-1 merged and the node closed"));
        assert!(crate::operator_notice::stored_token(&store, &seen_key("s1", "x-1")).is_none());
        assert!(crate::operator_notice::stored_token(&store, &settle_key("s1", "x-1")).is_none());
    }

    /// AC5: unowned nodes and an unresolved fold read nothing and mail
    /// nothing; the note names the unresolved team.
    #[test]
    fn unowned_and_unresolved_are_skipped_and_named() {
        let store = temp_store("ac5");
        let payload = json!({"teams": [
            team("s1", json!([covered_node("x-1", 7, json!(false)), covered_node("x-2", 8, Value::Null)])),
            json!({
                "holder": "sess-2", "level": 1, "scope": "s2",
                "status": "manifest-only",
                "scope_nodes": {"status": "unresolved", "reason": "scope does not compile"},
            }),
        ]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let mut list = panic_listing();
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        assert_eq!(out.reads, 0);
        assert!(log.borrow().is_empty());
        assert!(out.note.contains("s2"));
    }

    /// AC6: a failed send stores no token, so the next pass sends again.
    #[test]
    fn a_failed_send_retries_the_next_pass() {
        let store = temp_store("ac6");
        let payload = json!({"teams": [team("s1", json!([covered_node("x-1", 7, json!(true))]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let rows = vec![listed_row(
            7,
            "abcdef1234567890",
            "MERGEABLE",
            "CLEAN",
            green_rollup(),
        )];
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> { Ok(rows.clone()) };
        let log = Log::new(RefCell::new(Vec::new()));
        let sink = Rc::clone(&log);
        let mut mail = move |scope: &str, text: &str| -> bool {
            sink.borrow_mut().push(format!("{scope}: {text}"));
            false
        };
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW + 300);
        assert_eq!(out.mailed, 0);
        assert_eq!(
            log.borrow().len(),
            2,
            "the send retried instead of deduping"
        );
    }

    /// A transient owner-read failure keeps every key: the fold reads ok
    /// with `owned_total` null, and that is not "the node left the fold".
    #[test]
    fn an_owners_unread_beat_keeps_the_keys() {
        let store = temp_store("ac-owners");
        crate::operator_notice::mark_once(&store, &seen_key("s1", "x-1"), "7");
        let payload = json!({"teams": [json!({
            "holder": "sess-1", "level": 1, "scope": "s1",
            "status": "manifest-only",
            "scope_nodes": {"status": "ok", "owned_total": Value::Null,
                            "owned_reason": "a live team's scope does not compile",
                            "nodes": [covered_node("x-1", 7, Value::Null)]},
        })]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> {
            Ok(vec![json!({"id": "x-1", "status": "in_progress"})])
        };
        let mut list = panic_listing();
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        assert!(out.note.contains("owners unread"));
        assert_eq!(
            crate::operator_notice::stored_token(&store, &seen_key("s1", "x-1")),
            Some("7".to_string())
        );
    }

    /// A red row mails nothing and stores no settle token.
    #[test]
    fn a_red_row_mails_nothing() {
        let store = temp_store("red");
        let payload = json!({"teams": [team("s1", json!([covered_node("x-1", 7, json!(true))]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let rows = vec![listed_row(
            7,
            "abcdef1234567890",
            "MERGEABLE",
            "CLEAN",
            red_rollup(),
        )];
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> { Ok(rows.clone()) };
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        assert!(log.borrow().is_empty());
        assert!(crate::operator_notice::stored_token(&store, &settle_key("s1", "x-1")).is_none());
    }

    /// A covered PR absent from the open listing has left the open set (a
    /// merge between beats): it skips quietly, and the seen key stays for
    /// the freed pass to answer.
    #[test]
    fn a_pr_absent_from_the_listing_skips_quietly() {
        let store = temp_store("absent");
        let payload = json!({"teams": [team("s1", json!([covered_node("x-1", 7, json!(true))]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let rows = vec![listed_row(
            8,
            "1234abcdef567890",
            "MERGEABLE",
            "CLEAN",
            green_rollup(),
        )];
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> { Ok(rows.clone()) };
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        assert!(out.note.is_empty(), "the skip names nothing: {}", out.note);
        assert!(log.borrow().is_empty());
        assert_eq!(
            crate::operator_notice::stored_token(&store, &seen_key("s1", "x-1")),
            Some("7".to_string()),
            "the seen stamp covers every covered node, read or not"
        );
    }

    /// A green row GitHub cannot merge names its merge state in the mail.
    #[test]
    fn a_green_blocked_row_names_the_merge_state() {
        let store = temp_store("blocked");
        let payload = json!({"teams": [team("s1", json!([covered_node("x-1", 7, json!(true))]))]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let rows = vec![listed_row(
            7,
            "abcdef1234567890",
            "CONFLICTING",
            "DIRTY",
            green_rollup(),
        )];
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> { Ok(rows.clone()) };
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 1);
        assert!(log.borrow()[0].contains("Blockers: merge state dirty."));
    }

    /// A failed listing reads one note per repo, not one per PR, mails
    /// nothing, and the next beat retries the read.
    #[test]
    fn a_failed_listing_notes_once_and_the_next_beat_retries() {
        let store = temp_store("unread");
        let payload = json!({"teams": [team(
            "s1",
            json!([
                covered_node("x-1", 7, json!(true)),
                covered_node("x-2", 8, json!(true)),
            ]),
        )]});
        let graph = |_pr: i64| -> Result<Vec<Value>, String> { Ok(vec![]) };
        let mut list = move |_cwd: &Path| -> Result<Vec<Value>, String> {
            Err("loop-check: the read timed out".to_string())
        };
        let log = Log::new(RefCell::new(Vec::new()));
        let mut mail = mail_log(Rc::clone(&log));
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW);
        assert_eq!(out.mailed, 0);
        assert_eq!(out.reads, 1);
        assert_eq!(
            out.note.matches("unread").count(),
            1,
            "one note per repo, not per PR: {}",
            out.note
        );
        assert!(log.borrow().is_empty());
        let out = evaluate(&payload, &store, &graph, &mut list, &mut mail, NOW + 300);
        assert_eq!(out.reads, 1, "the next beat retries the read");
    }
}
