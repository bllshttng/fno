//! A shared epic as one GitHub issue.
//!
//! An epic marked `share=github` gets one issue in its project's origin repo.
//! fno keeps a markdown checklist between two markers, one line per child
//! the owner may publish, and never touches a byte outside them. Nodes stay in
//! each person's own store; the issue is the one surface other people can
//! read. The heal pass calls [`run_pass`] beside the PR steward, and
//! `pr create` calls [`ref_line`] so each child PR points back at the issue.

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

fn clean_title(title: &str) -> String {
    // A title is private text going to a public page: `<` could open a
    // marker or a tag, and `@name` would notify a stranger.
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
    (!login.is_empty() && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
        .then(|| login.to_string())
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
    let checklist = kids
        .iter()
        .map(|k| render_line(k, handle_of(claims, text(k, "id"), owner_login).as_deref()))
        .collect::<Vec<_>>()
        .join("\n");
    let (number, record) = match recorded_issue(epic) {
        Some(n) => (n, false),
        None => match find_issue(gh, id)? {
            Some(n) => (n, true),
            None => {
                let title = clean_title(text(epic, "title"));
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
            obj.insert("epic_issue".into(), json!(number));
            Ok(true)
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// The heal pass's arm: sync every shared epic of the project at `cwd`. The
/// returned token joins the tick row's counts, like the steward's.
pub(crate) fn run_pass(cwd: &Path, dry_run: bool) -> &'static str {
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
    let claims = crate::claims::list(Some("node:"), None, false).unwrap_or_default();
    let login = crate::gh_login::current();
    let gh = GitHubTracker::as_app(&repo);
    let (mut changed, mut failed) = (false, false);
    for epic in epics {
        let id = text(epic, "id");
        match sync_epic(&gh, &repo, &rows, epic, &claims, login.as_deref()) {
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
/// other published child of the epic is still open. None when the node's
/// parent has no issue, or the body already names it.
pub(crate) fn ref_line(rows: &[Value], node: &str, body: &str) -> Option<String> {
    let parent = text(rows.iter().find(|r| text(r, "id") == node)?, "parent");
    let epic = rows.iter().find(|r| text(r, "id") == parent)?;
    let number = recorded_issue(epic)?;
    let last = !checklist_rows(rows, parent)
        .iter()
        .any(|r| text(r, "id") != node && !is_done(r));
    let (refs, closes) = (format!("refs #{number}"), format!("closes #{number}"));
    if body.lines().any(|l| {
        let l = l.trim().to_lowercase();
        l == refs || l == closes
    }) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracker::github::GhRun;
    use std::sync::{Arc, Mutex};

    /// One in-memory GitHub: issues by number, and every write it saw.
    #[derive(Default)]
    struct Hub {
        bodies: Vec<String>,
        creates: usize,
        edits: usize,
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
                    let url = format!("https://github.com/o/r/issues/{}\n", hub.bodies.len());
                    (0, url, String::new())
                }
                ("issue", "edit") => {
                    hub.edits += 1;
                    let n: usize = args[2].parse().unwrap();
                    hub.bodies[n - 1] = arg("--body");
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
        let mut row = json!({"id": id, "title": title, "parent": "x-e000", "status": "ready"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        row
    }

    fn epic(extra: Value) -> Value {
        let mut row = json!({"id": "x-e000", "title": "Shared work", "share": "github"});
        row.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
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
        let epic = epic(json!({}));
        let rows = vec![
            epic.clone(),
            child("x-c001", "First", json!({"pr_number": 12})),
            child(
                "x-c002",
                "Second",
                json!({"status": "done", "pr_number": 9}),
            ),
            child("x-c003", "Third", json!({})),
            child("x-c004", "Local only", json!({"public": false})),
        ];
        let claims = vec![
            claim("x-c001", "github:visitor-1"),
            claim("x-c003", "target-session:abc"),
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
        assert_eq!(
            inside,
            "- [ ] First · @visitor-1 · #12\n- [x] Second · #9\n- [ ] Third · @owner"
        );
        assert!(!body.contains("Local only"), "{body}");
        // An epic whose children are all local opens nothing.
        let alone = sync_epic(&tracker(&hub), "o/r", &rows[..1], &epic, &[], None);
        assert_eq!(alone, Ok(None));
        assert_eq!(hub.lock().unwrap().creates, 1);
    }

    #[test]
    fn a_second_pass_edits_inside_the_markers_and_keeps_every_other_byte() {
        let hub = Arc::new(Mutex::new(Hub::default()));
        let mut rows = vec![epic(json!({})), child("x-c001", "First", json!({}))];
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
        rows.push(child("x-c002", "Second", json!({})));
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
        let rows = vec![epic(json!({})), child("x-c001", "First", json!({}))];
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
            child("x-c001", "One", json!({})),
            child("x-c002", "Two", json!({})),
            child("x-c003", "Three", json!({"status": "done"})),
            child("x-c004", "Local only", json!({"public": false})),
        ];
        assert_eq!(ref_line(&rows, "x-c001", "body"), Some("Refs #7".into()));
        let mut rows = rows;
        rows[2]["status"] = json!("done");
        assert_eq!(ref_line(&rows, "x-c001", "body"), Some("Closes #7".into()));
        assert_eq!(ref_line(&rows, "x-c001", "Intro\nrefs #7\n"), None);
        rows[0].as_object_mut().unwrap().remove("epic_issue");
        assert_eq!(ref_line(&rows, "x-c001", "body"), None);
    }

    #[test]
    fn a_private_title_cannot_ping_a_stranger_or_forge_a_marker() {
        let line = render_line(
            &child(
                "x-c001",
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
}
