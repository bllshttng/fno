//! A shared epic as one GitHub issue.
//!
//! An epic marked `share=github` gets one issue in its project's origin repo.
//! fno keeps a markdown checklist between two markers, one line per child
//! the owner may publish, and never touches a byte outside them. Nodes stay in
//! each person's own store; the issue is the one surface other people can
//! read. The heal pass calls [`run_pass`] beside the PR steward, and
//! `pr create` calls [`ref_line`] so each child PR points back at the issue.
//! A `/claim` comment on the issue adds a line, and a claim with no PR goes
//! stale after [`STALE_AFTER_DAYS`]; the `claim` module decides both. Each
//! owner line ends with a hidden id marker, and the `mine` module reads it back
//! so a teammate's own fno can file the node it claimed.

mod claim;
mod mine;

pub(crate) use mine::{run as import_claimed, Args as ImportArgs};

use crate::backlog::projections::{is_publishable, project_key};
use crate::claims::ClaimRecord;
use crate::graph_store::is_terminal_entry;
use crate::tracker::github::GitHubTracker;
use crate::tracker::Tracker;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub(crate) const OPEN_MARKER: &str = "<!-- fno-epic-checklist -->";
pub(crate) const CLOSE_MARKER: &str = "<!-- /fno-epic-checklist -->";
/// A finished epic keeps syncing this long, so its last checkbox lands.
const FINISHED_GRACE_DAYS: i64 = 2;
/// A `/claim` this old with no `Refs #<issue>` PR reads `stale` and is released.
const STALE_AFTER_DAYS: i64 = 7;
const RECORD_TIMEOUT: Duration = Duration::from_secs(30);

fn text<'a>(row: &'a Value, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("").trim()
}

fn pos_int(v: Option<&Value>) -> Option<i64> {
    let n = match v? {
        Value::Number(n) => n.as_i64()?,
        Value::String(s) => s.trim().parse().ok()?,
        _ => return None,
    };
    (n > 0).then_some(n)
}

fn is_shared(epic: &Value) -> bool {
    text(epic, "share").eq_ignore_ascii_case("github")
}

/// The issue number on an epic: a JSON number when the heal pass wrote it,
/// numeric text when a person adopted an issue with `--set epic_issue=<n>`.
fn recorded_issue(epic: &Value) -> Option<i64> {
    pos_int(epic.get("epic_issue"))
}

fn is_done(row: &Value) -> bool {
    text(row, "status") == "done"
}

/// The children the checklist shows. A node contained in another ships with
/// its owner, so it has no line. A child closed for good without being done
/// (superseded, wont-do) has no line either.
fn checklist_rows<'a>(rows: &'a [Value], epic_id: &str) -> Vec<&'a Value> {
    rows.iter()
        .filter(|r| text(r, "parent") == epic_id)
        .filter(|r| text(r, "contained_in").is_empty())
        .filter(|r| is_publishable(r))
        .filter(|r| is_done(r) || !is_terminal_entry(r))
        .collect()
}

fn one_line(title: &str) -> String {
    title.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A title in issue-body markdown. It is private text going to a public
/// page: `<` could open a marker or a tag, and `@name` would notify a
/// stranger. An issue title is plain text, so it takes [`one_line`] only.
fn clean_title(title: &str) -> String {
    one_line(title)
        .replace('<', "&lt;")
        .replace('@', "@\u{200b}")
}

fn handle_of(claims: &[ClaimRecord], node: &str, owner_login: Option<&str>) -> Option<String> {
    let key = format!("node:{node}");
    let claim = claims.iter().find(|c| c.key == key)?;
    let login = match claim.holder.strip_prefix("github:") {
        Some(login) => login,
        None => owner_login?,
    };
    is_login(login).then(|| login.to_string())
}

/// A GitHub login: letters, digits and dashes, at most 39 characters. It is
/// the only text of a person's that reaches a line unescaped.
fn is_login(login: &str) -> bool {
    (1..=39).contains(&login.len()) && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Does the body carry `Refs #<n>` or `Closes #<n>` on a line of its own?
fn names_issue(body: &str, number: i64) -> bool {
    let (refs, closes) = (format!("refs #{number}"), format!("closes #{number}"));
    body.lines().any(|l| {
        let l = l.trim().to_lowercase();
        l == refs || l == closes
    })
}

fn render_line(row: &Value, handle: Option<&str>) -> String {
    let mut parts = vec![clean_title(text(row, "title"))];
    if let Some(handle) = handle {
        parts.push(format!("@{handle}"));
    }
    if let Some(pr) = pos_int(row.get("pr_number")) {
        parts.push(format!("#{pr}"));
    }
    let mark = if is_done(row) { 'x' } else { ' ' };
    format!("- [{mark}] {}", parts.join(" · "))
}

/// The hidden tail of an owner line. It is an HTML comment, so a reader sees
/// nothing, and it carries the node id alone. An id that could close the
/// comment never reaches the issue.
const ID_MARKER_OPEN: &str = "<!-- fno:";
const ID_MARKER_CLOSE: &str = " -->";

fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// An owner child's line. A stale `/claim` took its handle from the ledger.
fn node_line(
    row: &Value,
    claims: &[ClaimRecord],
    owner_login: Option<&str>,
    ledger: &[claim::Entry],
) -> String {
    let id = text(row, "id");
    let line = match ledger
        .iter()
        .find(|e| e.stale && e.node.as_deref() == Some(id))
    {
        Some(e) => format!("{} · stale", render_line(row, Some(&e.login))),
        None => render_line(row, handle_of(claims, id, owner_login).as_deref()),
    };
    if is_plain_id(id) {
        format!("{line} {ID_MARKER_OPEN}{id}{ID_MARKER_CLOSE}")
    } else {
        line
    }
}

/// A `/claim` line with no node behind it.
fn contributor_line(e: &claim::Entry) -> String {
    let mut parts = vec![clean_title(&e.title), format!("@{}", e.login)];
    if let Some(pr) = e.pr {
        parts.push(format!("#{pr}"));
    }
    if e.stale {
        parts.push("stale".to_string());
    }
    format!("- [ ] {}", parts.join(" · "))
}

fn identity_marker(epic_id: &str) -> String {
    format!("<!-- fno-epic:{epic_id} -->")
}

fn initial_body(epic: &Value, checklist: &str) -> String {
    format!(
        "{}\nTracking issue for the epic \"{}\". fno keeps the checklist between the markers. Write anywhere else on this issue.\n\n{OPEN_MARKER}\n{checklist}\n{CLOSE_MARKER}\n",
        identity_marker(text(epic, "id")),
        clean_title(text(epic, "title")),
    )
}

/// Replace the text between the markers and nothing else. A body with no
/// markers gets the block appended. A body with one marker, or the two out of
/// order, is refused: guessing where the block ends could eat a person's text.
fn splice(body: &str, checklist: &str) -> Result<String, String> {
    let block = format!("{OPEN_MARKER}\n{checklist}\n{CLOSE_MARKER}");
    match (body.find(OPEN_MARKER), body.find(CLOSE_MARKER)) {
        (Some(open), Some(close)) if close > open => Ok(format!(
            "{}{block}{}",
            &body[..open],
            &body[close + CLOSE_MARKER.len()..]
        )),
        (None, None) if body.trim().is_empty() => Ok(format!("{block}\n")),
        (None, None) => Ok(format!("{}\n\n{block}\n", body.trim_end())),
        _ => Err(
            "the checklist markers are damaged (one is missing or they are out of order); fix the issue body by hand"
                .to_string(),
        ),
    }
}

#[derive(Debug, PartialEq)]
pub(crate) enum Action {
    Created,
    Edited,
    Unchanged,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Synced {
    pub number: i64,
    pub action: Action,
    /// The epic row does not carry `number` yet.
    pub record: bool,
}

/// An issue that already holds this epic's identity marker. It covers the
/// window where an issue opened but its number never reached the epic row.
fn find_issue(gh: &GitHubTracker, epic_id: &str) -> Result<Option<i64>, String> {
    let marker = identity_marker(epic_id);
    let open = gh.list_open().map_err(|e| e.to_string())?;
    Ok(open
        .iter()
        .find(|c| {
            c.node
                .details
                .as_deref()
                .is_some_and(|b| b.contains(&marker))
        })
        .and_then(|c| c.node.id.rsplit_once('#'))
        .and_then(|(_, n)| n.parse().ok()))
}

/// One pass for one epic. `Ok(None)` means it has no child to publish.
pub(crate) fn sync_epic(
    gh: &GitHubTracker,
    repo: &str,
    rows: &[Value],
    epic: &Value,
    claims: &[ClaimRecord],
    owner_login: Option<&str>,
) -> Result<Option<Synced>, String> {
    let id = text(epic, "id");
    let kids = checklist_rows(rows, id);
    if kids.is_empty() {
        return Ok(None);
    }
    let ledger = claim::ledger_of(epic);
    let checklist = kids
        .iter()
        .map(|k| node_line(k, claims, owner_login, &ledger))
        .chain(
            ledger
                .iter()
                .filter(|e| e.node.is_none())
                .map(contributor_line),
        )
        .collect::<Vec<_>>()
        .join("\n");
    let (number, record) = match recorded_issue(epic) {
        Some(n) => (n, false),
        None => match find_issue(gh, id)? {
            Some(n) => (n, true),
            None => {
                let title = one_line(text(epic, "title"));
                let number = gh
                    .create_issue(repo, &title, &initial_body(epic, &checklist))
                    .map_err(|e| e.to_string())?;
                return Ok(Some(Synced {
                    number,
                    action: Action::Created,
                    record: true,
                }));
            }
        },
    };
    let current = gh
        .read(&format!("{repo}#{number}"))
        .map_err(|e| e.to_string())?
        .details
        .unwrap_or_default();
    let next = splice(&current, &checklist)?;
    let action = if next == current {
        Action::Unchanged
    } else {
        gh.edit_issue_body(repo, number, &next)
            .map_err(|e| e.to_string())?;
        Action::Edited
    };
    Ok(Some(Synced {
        number,
        action,
        record,
    }))
}

fn finished_long_ago(epic: &Value, now: chrono::DateTime<chrono::Utc>) -> bool {
    is_terminal_entry(epic)
        && chrono::DateTime::parse_from_rfc3339(text(epic, "completed_at")).is_ok_and(|t| {
            now.signed_duration_since(t) > chrono::Duration::days(FINISHED_GRACE_DAYS)
        })
}

fn record_issue(epic_id: &str, number: i64) -> Result<(), String> {
    record_fields(epic_id, &[("epic_issue", json!(number))])
}

/// Write fields onto the epic row under the graph lock.
fn record_fields(epic_id: &str, fields: &[(&str, Value)]) -> Result<(), String> {
    crate::graph_store::mutate_rows(
        &crate::backlog::settings::graph_path(),
        RECORD_TIMEOUT,
        None,
        None,
        |rows| {
            let Some(obj) = rows
                .iter_mut()
                .find(|r| text(r, "id") == epic_id)
                .and_then(Value::as_object_mut)
            else {
                return Ok(false);
            };
            for (key, value) in fields {
                obj.insert((*key).into(), value.clone());
            }
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The heal pass's arm: sync every shared epic of the project at `cwd`. The
/// returned token joins the tick row's counts, like the steward's. `pulls` is
/// the open PR list the heal pass already holds: it says which `/claim` has a
/// PR that writes `Refs #<issue>`, at no extra gh call.
pub(crate) fn run_pass(cwd: &Path, pulls: &[Value], dry_run: bool) -> &'static str {
    let Ok(project) =
        crate::law_match::resolve_project(Some(cwd), &crate::law_match::settings_sources())
    else {
        return "epic_issue_off_no_project";
    };
    let rows = match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path()) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("epic-issue: the graph read failed: {e}");
            return "epic_issue_error";
        }
    };
    let now = chrono::Utc::now();
    let epics: Vec<&Value> = rows
        .iter()
        .filter(|r| is_shared(r) && project_key(r) == project && !finished_long_ago(r, now))
        .collect();
    if epics.is_empty() {
        return "epic_issue_none";
    }
    if dry_run {
        for epic in &epics {
            eprintln!("epic-issue: would sync {}", text(epic, "id"));
        }
        return "epic_issue_dry_run";
    }
    let repo = match crate::pr_list::origin_slug(cwd, None) {
        Ok(repo) => repo,
        Err(e) => {
            eprintln!("epic-issue: could not resolve owner/repo: {e}");
            return "epic_issue_error";
        }
    };
    let claims_read = crate::claims::list(Some("node:"), None, false);
    let mut claims = claims_read.clone().unwrap_or_default();
    let login = crate::gh_login::current();
    let gh = GitHubTracker::as_app(&repo);
    let (mut changed, mut failed) = (false, false);
    for epic in epics {
        let id = text(epic, "id");
        let mut epic_row = epic.clone();
        // Comments can exist only once the issue does, and an unreadable claim
        // store must not read as "nobody holds anything".
        if let (Some(number), Ok(held)) = (recorded_issue(epic), &claims_read) {
            match claim::run(&gh, cwd, &repo, number, &rows, epic, held, pulls) {
                Ok((next, moved)) => {
                    changed |= moved;
                    epic_row = next;
                    claims = crate::claims::list(Some("node:"), None, false).unwrap_or(claims);
                }
                Err(e) => {
                    eprintln!("epic-issue: {id}: the /claim read failed: {e}");
                    failed = true;
                }
            }
        }
        match sync_epic(&gh, &repo, &rows, &epic_row, &claims, login.as_deref()) {
            Ok(Some(done)) => {
                changed |= done.action != Action::Unchanged;
                if done.record {
                    if let Err(e) = record_issue(id, done.number) {
                        eprintln!(
                            "epic-issue: {id}: issue {} is open but the epic row did not take it ({e}); run: fno backlog update {id} --set epic_issue={}",
                            done.number, done.number
                        );
                        failed = true;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("epic-issue: {id}: {e}");
                failed = true;
            }
        }
    }
    match (failed, changed) {
        (true, _) => "epic_issue_error",
        (false, true) => "epic_issue_synced",
        (false, false) => "epic_issue_unchanged",
    }
}

/// The line a child PR's body carries: `Refs #<n>`, or `Closes #<n>` when no
/// other published child of the epic is still open. A node that carries its
/// own `epic_issue` is a claim filed from a teammate's checklist: it has no
/// siblings here, so only the owner can tell it is the last, and it always
/// refs. None when neither the node nor its parent has an issue, or the body
/// already names it.
pub(crate) fn ref_line(rows: &[Value], node: &str, body: &str) -> Option<String> {
    let row = rows.iter().find(|r| text(r, "id") == node)?;
    let (number, last) = match recorded_issue(row) {
        Some(number) => (number, false),
        None => {
            let parent = text(row, "parent");
            let epic = rows.iter().find(|r| text(r, "id") == parent)?;
            let last = !checklist_rows(rows, parent)
                .iter()
                .any(|r| text(r, "id") != node && !is_done(r));
            (recorded_issue(epic)?, last)
        }
    };
    if names_issue(body, number) {
        return None;
    }
    Some(if last {
        format!("Closes #{number}")
    } else {
        format!("Refs #{number}")
    })
}

/// `ref_line` for the node bound to the session at `cwd`, read from the live
/// graph. Any read that fails adds no line: the body is still a valid PR body.
pub(crate) fn body_ref_line(cwd: &Path, body: &str) -> Option<String> {
    let manifest = crate::target_orient::read_manifest(cwd)?;
    let node = manifest.get("graph_node_id").filter(|n| !n.is_empty())?;
    match crate::graph_store::read_rows_strict(&crate::backlog::settings::graph_path()) {
        Ok(rows) => ref_line(&rows, node, body),
        Err(e) => {
            eprintln!("pr-create: warning: no epic issue line, the graph read failed: {e}");
            None
        }
    }
}

/// One `ref_line` per distinct issue among a batch PR's members, in member
/// order. A batch merge closes every member at once, so an epic counts as
/// finished when its only open published children ride this batch; two
/// members of the same epic still share one line. A filed claim only refs
/// its own issue.
pub(crate) fn batch_ref_lines(rows: &[Value], members: &[&str], body: &str) -> Vec<String> {
    let ids: std::collections::HashSet<&str> = members.iter().copied().collect();
    let mut lines = Vec::new();
    let mut emitted = std::collections::HashSet::new();
    for node in members {
        let Some(row) = rows.iter().find(|r| text(r, "id") == *node) else {
            continue;
        };
        let (number, last) = match recorded_issue(row) {
            Some(number) => (number, false),
            None => {
                let parent = text(row, "parent");
                let Some(epic) = rows.iter().find(|r| text(r, "id") == parent) else {
                    continue;
                };
                let Some(number) = recorded_issue(epic) else {
                    continue;
                };
                let last = !checklist_rows(rows, parent)
                    .iter()
                    .any(|r| !ids.contains(text(r, "id")) && !is_done(r));
                (number, last)
            }
        };
        if names_issue(body, number) || !emitted.insert(number) {
            continue;
        }
        lines.push(if last {
            format!("Closes #{number}")
        } else {
            format!("Refs #{number}")
        });
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::github::GhRun;
    use std::sync::{Arc, Mutex};

    /// One in-memory GitHub: issues by number, and every write it saw.
    #[derive(Default)]
    struct Hub {
        bodies: Vec<String>,
        titles: Vec<String>,
        creates: usize,
        /// Comments on issue 1 as `(login, body)`; an id is position plus one.
        comments: Vec<(String, String)>,
        replies: Vec<String>,
    }

    struct FakeGh(Arc<Mutex<Hub>>);

    impl GhRun for FakeGh {
        fn run(&self, args: &[String]) -> Result<(i32, String, String), String> {
            let mut hub = self.0.lock().unwrap();
            let arg = |flag: &str| {
                args.iter()
                    .position(|a| a == flag)
                    .map(|i| args[i + 1].clone())
                    .unwrap()
            };
            Ok(match (args[0].as_str(), args[1].as_str()) {
                ("issue", "create") => {
                    hub.creates += 1;
                    hub.bodies.push(arg("--body"));
                    hub.titles.push(arg("--title"));
                    let url = format!("https://github.com/o/r/issues/{}\n", hub.bodies.len());
                    (0, url, String::new())
                }
                ("issue", "edit") => {
                    let n: usize = args[2].parse().unwrap();
                    hub.bodies[n - 1] = arg("--body");
                    (0, String::new(), String::new())
                }
                ("issue", "view") if arg("--json") == "author,comments" => {
                    let items: Vec<Value> = hub
                        .comments
                        .iter()
                        .enumerate()
                        .map(|(i, (login, body))| {
                            json!({
                                "author": {"login": login}, "body": body, "isMinimized": false,
                                "url": format!("https://github.com/o/r/issues/1#issuecomment-{}", i + 1),
                            })
                        })
                        .collect();
                    let view = json!({"author": {"login": "fnodotdev"}, "comments": items});
                    (0, view.to_string(), String::new())
                }
                ("issue", "comment") => {
                    let body = arg("--body");
                    hub.replies.push(body.clone());
                    hub.comments.push(("fnodotdev".into(), body));
                    (0, String::new(), String::new())
                }
                ("issue", "view") => {
                    let n: usize = args[2].parse().unwrap();
                    let view = json!({"title": "t", "state": "OPEN", "url": "u", "body": hub.bodies[n - 1]});
                    (0, view.to_string(), String::new())
                }
                ("issue", "list") => {
                    let items: Vec<Value> = hub
                        .bodies
                        .iter()
                        .enumerate()
                        .map(|(i, b)| json!({"number": i + 1, "title": "t", "state": "OPEN", "body": b}))
                        .collect();
                    (0, Value::Array(items).to_string(), String::new())
                }
                _ => (1, String::new(), "unexpected gh call".into()),
            })
        }
    }

    fn tracker(hub: &Arc<Mutex<Hub>>) -> GitHubTracker {
        GitHubTracker::new(Some("o/r".into()), Box::new(FakeGh(hub.clone())))
    }

    fn child(id: &str, title: &str, extra: Value) -> Value {
        let mut row = json!({"id": id, "title": title, "parent": "epic", "status": "ready"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn epic(extra: Value) -> Value {
        let mut row = json!({"id": "epic", "title": "Shared work", "share": "github"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn epic2(extra: Value) -> Value {
        let mut row = json!({"id": "epic2", "title": "Second epic", "share": "github"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn child_of(id: &str, title: &str, parent: &str, extra: Value) -> Value {
        let mut row = child(id, title, extra);
        row["parent"] = json!(parent);
        row
    }

    fn claim(node: &str, holder: &str) -> ClaimRecord {
        serde_json::from_value(json!({
            "key": format!("node:{node}"), "holder": holder, "acquired_at": 0,
            "pid": null, "host": "h",
        }))
        .unwrap()
    }

    #[test]
    fn one_pass_opens_one_issue_with_a_line_per_public_child() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let epic = epic(json!({
            "title": "Ship Vec<T>  work",
            "epic_claims": [{"login": "visitor-2", "title": "Contributor idea", "at": "2026-11-01T00:00:00Z"}],
        }));
        let rows = vec![
            epic.clone(),
            child("kid-1", "First", json!({"pr_number": 12})),
            child("kid-2", "Second", json!({"status": "done", "pr_number": 9})),
            child("kid-3", "Third", json!({})),
            child("kid-4", "Local only", json!({"public": false})),
        ];
        let claims = vec![
            claim("kid-1", "github:visitor-1"),
            claim("kid-3", "target-session:abc"),
        ];
        let out = sync_epic(&tracker(&hub), "o/r", &rows, &epic, &claims, Some("owner")).unwrap();
        assert_eq!(
            out,
            Some(Synced {
                number: 1,
                action: Action::Created,
                record: true
            })
        );
        let body = hub.lock().unwrap().bodies[0].clone();
        let inside = body
            .split_once(OPEN_MARKER)
            .and_then(|(_, rest)| rest.split_once(CLOSE_MARKER))
            .unwrap()
            .0
            .trim()
            .to_string();
        // Each owner line ends with its id in a hidden comment. The visible
        // text is what it was, and a contributor line has no node to name.
        assert_eq!(inside.matches("<!-- fno:").count(), 3, "{inside}");
        assert!(inside.contains("#9 <!-- fno:kid-2 -->\n"), "{inside}");
        let visible: Vec<&str> = inside
            .lines()
            .map(|l| l.split(" <!-- fno:").next().unwrap())
            .collect();
        assert_eq!(
            visible,
            [
                "- [ ] First · @visitor-1 · #12",
                "- [x] Second · #9",
                "- [ ] Third · @owner",
                "- [ ] Contributor idea · @visitor-2",
            ]
        );
        assert!(!body.contains("Local only"), "{body}");
        // The issue title is plain text; only the body copy is escaped.
        assert_eq!(hub.lock().unwrap().titles[0], "Ship Vec<T> work");
        assert!(body.contains("\"Ship Vec&lt;T> work\""), "{body}");
        // An epic whose children are all local opens nothing.
        let alone = sync_epic(&tracker(&hub), "o/r", &rows[..1], &epic, &[], None);
        assert_eq!(alone, Ok(None));
        assert_eq!(hub.lock().unwrap().creates, 1);
    }

    #[test]
    fn a_second_pass_edits_inside_the_markers_and_keeps_every_other_byte() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let mut rows = vec![epic(json!({})), child("kid-1", "First", json!({}))];
        let first = sync_epic(&tracker(&hub), "o/r", &rows, &rows[0].clone(), &[], None).unwrap();
        assert_eq!(first.unwrap().action, Action::Created);
        // A person writes above and below the block.
        let typed = {
            let mut hub = hub.lock().unwrap();
            hub.bodies[0] = format!(
                "Notes from Sam\n\n{}\n\nSee the design doc.",
                hub.bodies[0].trim_end()
            );
            hub.bodies[0].clone()
        };
        rows[0]["epic_issue"] = json!(1);
        rows.push(child("kid-2", "Second", json!({})));
        let out = sync_epic(&tracker(&hub), "o/r", &rows, &rows[0].clone(), &[], None).unwrap();
        assert_eq!(
            out,
            Some(Synced {
                number: 1,
                action: Action::Edited,
                record: false
            })
        );
        let hub = hub.lock().unwrap();
        assert_eq!(hub.creates, 1, "no second issue");
        let (head, tail) = (
            typed.split(OPEN_MARKER).next().unwrap(),
            typed.rsplit(CLOSE_MARKER).next().unwrap(),
        );
        assert!(
            hub.bodies[0].starts_with(head) && hub.bodies[0].ends_with(tail),
            "{}",
            hub.bodies[0]
        );
        assert!(hub.bodies[0].contains("- [ ] Second"), "{}", hub.bodies[0]);
        // Half a block is damage; the pass refuses rather than guess where
        // a person's text starts.
        assert!(splice("a\n<!-- fno-epic-checklist -->\nb", "c").is_err());
    }

    #[test]
    fn an_issue_missing_from_the_row_is_found_by_its_marker_not_opened_again() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let rows = vec![epic(json!({})), child("kid-1", "First", json!({}))];
        sync_epic(&tracker(&hub), "o/r", &rows, &rows[0].clone(), &[], None).unwrap();
        let out = sync_epic(&tracker(&hub), "o/r", &rows, &rows[0].clone(), &[], None).unwrap();
        assert_eq!(
            out,
            Some(Synced {
                number: 1,
                action: Action::Unchanged,
                record: true
            })
        );
        assert_eq!(hub.lock().unwrap().creates, 1);
    }

    #[test]
    fn a_child_pr_refs_the_issue_and_the_last_open_child_closes_it() {
        let rows = vec![
            epic(json!({"epic_issue": 7})),
            child("kid-1", "One", json!({})),
            child("kid-2", "Two", json!({})),
            child("kid-3", "Three", json!({"status": "done"})),
            child("kid-4", "Local only", json!({"public": false})),
        ];
        assert_eq!(ref_line(&rows, "kid-1", "body"), Some("Refs #7".into()));
        let mut rows = rows;
        rows[2]["status"] = json!("done");
        assert_eq!(ref_line(&rows, "kid-1", "body"), Some("Closes #7".into()));
        assert_eq!(ref_line(&rows, "kid-1", "Intro\nrefs #7\n"), None);
        rows[0].as_object_mut().unwrap().remove("epic_issue");
        assert_eq!(ref_line(&rows, "kid-1", "body"), None);
        // A claim filed from a teammate's checklist has no parent here. It
        // carries its own issue and only refs: the owner knows the last child.
        let filed = [json!({"id": "own-1a2b", "title": "Claimed", "epic_issue": 7})];
        assert_eq!(ref_line(&filed, "own-1a2b", "body"), Some("Refs #7".into()));
        assert_eq!(
            ref_line(&filed, "own-1a2b", "Fixes own-1a2b\nrefs #7\n"),
            None
        );
    }

    #[test]
    fn a_batch_body_carries_one_line_per_epic_and_closes_only_a_finished_checklist() {
        let rows = vec![
            epic(json!({"epic_issue": 7})),
            epic2(json!({"epic_issue": 9})),
            child("kid-1", "One", json!({})),
            child("kid-2", "Two", json!({})),
            child("kid-3", "Three", json!({"status": "done"})),
            child_of("kid-4", "Other epic", "epic2", json!({})),
            child("kid-5", "Blocked by CI", json!({"status": "blocked"})),
        ];
        // The shared epic keeps Refs while kid-2 and kid-5 stay open; epic2's
        // only child rides this batch, so its merge completes that checklist.
        assert_eq!(
            batch_ref_lines(&rows, &["kid-1", "kid-4"], "batch body"),
            vec!["Refs #7".to_string(), "Closes #9".to_string()]
        );
        // Every open child of the shared epic rides this batch, so its merge
        // finishes the checklist and the line closes the issue.
        let mut rows = rows;
        rows[5]["status"] = json!("done");
        rows[6]["status"] = json!("done");
        assert_eq!(
            batch_ref_lines(&rows, &["kid-1", "kid-2"], "batch body"),
            vec!["Closes #7".to_string()]
        );
        // One line per issue even when two members share the epic; an issue
        // the body already names adds no line.
        assert_eq!(
            batch_ref_lines(&rows, &["kid-1", "kid-2", "kid-1"], "Intro\nrefs #7\n"),
            Vec::<String>::new()
        );
        // A filed claim refs its own issue even as the only member; an epic
        // without an issue adds none.
        let filed = [
            json!({"id": "own-1a2b", "title": "Claimed", "epic_issue": 8}),
            child("kid-6", "Unpublished epic", json!({})),
        ];
        let mut rows = rows;
        rows[0].as_object_mut().unwrap().remove("epic_issue");
        rows.push(filed[0].clone());
        rows.push(filed[1].clone());
        assert_eq!(
            batch_ref_lines(&rows, &["own-1a2b", "kid-6"], "batch body"),
            vec!["Refs #8".to_string()]
        );
    }

    #[test]
    fn a_private_title_cannot_ping_a_stranger_or_forge_a_marker() {
        let line = render_line(
            &child(
                "kid-1",
                "ask @octocat <!-- /fno-epic-checklist -->",
                json!({}),
            ),
            None,
        );
        assert!(
            !line.contains("@octocat") && !line.contains("<!--"),
            "{line}"
        );
    }

    use super::claim::{claim_pass, command_of, parse_match, ClaimIn, ClaimPass, Command, Judge};
    use crate::backlog::requests::ClaimOp;
    use chrono::Utc;

    fn comment(hub: &Arc<Mutex<Hub>>, login: &str, body: &str) {
        hub.lock()
            .unwrap()
            .comments
            .push((login.into(), body.into()));
    }

    fn pass(
        hub: &Arc<Mutex<Hub>>,
        rows: &[Value],
        epic: &Value,
        claims: &[ClaimRecord],
        pulls: &[Value],
        judge: Judge,
    ) -> ClaimPass {
        claim_pass(&ClaimIn {
            gh: &tracker(hub),
            repo: "o/r",
            number: 1,
            rows,
            epic,
            claims,
            pulls,
            now: Utc::now(),
            judge,
        })
        .unwrap()
    }

    /// What the heal pass stores on the epic row after a claim pass.
    fn store(epic: &mut Value, pass: &ClaimPass) {
        epic["epic_claims"] = serde_json::to_value(&pass.ledger).unwrap();
        if let Some(cursor) = pass.cursor {
            epic["epic_claim_cursor"] = json!(cursor);
        }
    }

    /// Open the issue so a later pass can edit its body.
    fn open_issue(hub: &Arc<Mutex<Hub>>, rows: &[Value]) {
        let mut seed = rows[0].clone();
        seed.as_object_mut().unwrap().remove("epic_issue");
        sync_epic(&tracker(hub), "o/r", rows, &seed, &[], None).unwrap();
    }

    const NO_MATCH: Judge = &|_, _| Ok(None);

    #[test]
    fn a_claim_comment_adds_one_line_and_each_comment_is_answered_once() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let rows = vec![
            epic(json!({"epic_issue": 1})),
            child("kid-1", "First", json!({})),
        ];
        open_issue(&hub, &rows);
        for body in [
            "/claim New chooser",
            "/claim Second idea",
            "/claim Third idea",
            "/claim Fourth idea",
            "just chatting",
            "Hi\n/claim Not on the first line",
        ] {
            comment(&hub, "visitor", body);
        }
        // A visitor's forged marker for comment 4 does not pre-answer it.
        comment(&hub, "troll", "<!-- fno-claim-reply:4 -->");
        let first = pass(&hub, &rows, &rows[0], &[], &[], NO_MATCH);
        // Three claims fit the cap; the fourth gets one reply and no line.
        assert_eq!(first.ledger.len(), 3);
        assert_eq!(hub.lock().unwrap().replies.len(), 1);
        let mut epic_row = rows[0].clone();
        store(&mut epic_row, &first);
        sync_epic(&tracker(&hub), "o/r", &rows, &epic_row, &[], None).unwrap();
        let body = hub.lock().unwrap().bodies[0].clone();
        assert!(body.contains("- [ ] New chooser · @visitor"), "{body}");
        // A second pass skips the bot's own reply and changes nothing.
        let again = pass(&hub, &rows, &epic_row, &[], &[], NO_MATCH);
        assert_eq!(again.ledger, first.ledger);
        assert_eq!(hub.lock().unwrap().replies.len(), 1);
    }

    #[test]
    fn a_claim_on_an_owner_line_takes_that_node_and_release_frees_it() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let rows = vec![
            epic(json!({"epic_issue": 1, "project": "p"})),
            child("kid-1", "Fix the picker", json!({"project": "p"})),
        ];
        open_issue(&hub, &rows);
        comment(&hub, "visitor", "/claim fix the PICKER");
        comment(&hub, "other", "/claim Fix the picker");
        let first = pass(&hub, &rows, &rows[0], &[], &[], NO_MATCH);
        let grants: Vec<_> = first
            .ops
            .iter()
            .filter_map(|op| match op {
                ClaimOp::Grant { node, holder, .. } => Some((node.as_str(), holder.as_str())),
                ClaimOp::Release { .. } => None,
            })
            .collect();
        assert_eq!(grants, [("kid-1", "github:visitor")]);
        assert_eq!(first.ledger[0].node.as_deref(), Some("kid-1"));
        let replies = hub.lock().unwrap().replies.clone();
        assert!(
            replies.len() == 1 && replies[0].contains("held by visitor"),
            "{replies:?}"
        );
        // Once the claim is in the store, the line names the holder.
        let mut epic_row = rows[0].clone();
        store(&mut epic_row, &first);
        let held = [claim("kid-1", "github:visitor")];
        sync_epic(&tracker(&hub), "o/r", &rows, &epic_row, &held, None).unwrap();
        let body = hub.lock().unwrap().bodies[0].clone();
        assert!(body.contains("- [ ] Fix the picker · @visitor"), "{body}");
        // A held claim renews in place and writes nothing. A child that turns
        // done frees its claim instead of renewing it forever.
        let quiet = pass(&hub, &rows, &epic_row, &held, &[], NO_MATCH);
        assert!(quiet.ops.is_empty() && quiet.renewals.len() == 1);
        let mut finished = rows.clone();
        finished[1]["status"] = json!("done");
        let closed = pass(&hub, &finished, &epic_row, &held, &[], NO_MATCH);
        assert!(closed.ledger.is_empty() && closed.renewals.is_empty());
        assert!(matches!(&closed.ops[..], [ClaimOp::Release { .. }]));
        comment(&hub, "visitor", "/release");
        let freed = pass(&hub, &rows, &epic_row, &held, &[], NO_MATCH);
        assert!(freed.ledger.is_empty());
        assert!(
            matches!(&freed.ops[..], [ClaimOp::Release { node, holder }]
                if node == "kid-1" && holder == "github:visitor"),
            "a release and no renewing grant"
        );
    }

    #[test]
    fn only_the_first_line_of_a_comment_is_a_command_and_the_model_answer_is_strict() {
        assert_eq!(
            command_of("/claim  Fix the chooser "),
            Some(Command::Claim("Fix the chooser".into()))
        );
        assert_eq!(
            command_of("/release New chooser"),
            Some(Command::Release(Some("New chooser".into())))
        );
        assert_eq!(command_of("/release"), Some(Command::Release(None)));
        for not_a_command in [
            "/claim",
            "/claimed x",
            "Hi\n/claim x",
            "> /claim x",
            "/claim see https://x.example/a",
            "/claim see www.x.example",
            "",
        ] {
            assert_eq!(command_of(not_a_command), None, "{not_a_command:?}");
        }
        let Some(Command::Claim(title)) = command_of("/claim <!-- /fno-epic-checklist -->") else {
            panic!("a markup title still claims");
        };
        assert!(!title.contains('<'), "{title}");
        assert_eq!(parse_match("same work\nMATCH 2", 2), Ok(Some(1)));
        assert_eq!(parse_match("none of them\nNONE\n", 2), Ok(None));
        assert!(parse_match("MATCH 3", 2).is_err() && parse_match("maybe", 2).is_err());
    }
}
