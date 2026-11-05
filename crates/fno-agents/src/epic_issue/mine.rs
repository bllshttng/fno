//! A teammate's side of a shared epic: file the nodes it claimed.
//!
//! Each owner line on the epic issue ends with a hidden id marker. A teammate
//! who claimed lines with `/claim` runs `fno backlog add requests --epic <n>`.
//! The import reads the issue body, keeps the open lines that name the
//! teammate's own login, and files each as a local node under the owner's id.
//! The PR's `Fixes <id>` line then names the owner's node too, and no id map
//! exists. Dry run by default. A local row that already holds an id is never
//! overwritten.

use super::{is_login, CLOSE_MARKER, ID_MARKER_CLOSE, ID_MARKER_OPEN, OPEN_MARKER};
use crate::backlog::requests::safe_title;
use crate::tracker::github::GitHubTracker;
use crate::tracker::Tracker;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const OPEN_BOX: &str = "- [ ] ";
const DONE_BOX: &str = "- [x] ";

pub(crate) struct Args<'a> {
    pub(crate) project: &'a str,
    pub(crate) repo: &'a str,
    pub(crate) number: i64,
    pub(crate) cwd: &'a str,
    pub(crate) apply: bool,
    pub(crate) json: bool,
}

struct Ctx<'a> {
    repo: &'a str,
    number: i64,
    project: &'a str,
    cwd: &'a str,
    login: &'a str,
}

impl Ctx<'_> {
    fn evidence(&self) -> String {
        format!("github:{}#{}", self.repo, self.number)
    }
}

/// One owner line of the checklist, read back.
#[derive(Debug, PartialEq)]
struct Line {
    id: String,
    title: String,
    handle: Option<String>,
    done: bool,
    stale: bool,
}

/// The shape of a minted node id: a short lowercase prefix, a dash, and 4 to 8
/// hex digits. The id comes from an issue body and becomes a row id, which
/// reaches file names, so nothing looser than a minted id gets through.
fn is_node_id(id: &str) -> bool {
    let Some((prefix, hex)) = id.split_once('-') else {
        return false;
    };
    prefix.starts_with(|c: char| c.is_ascii_lowercase())
        && prefix.len() <= 8
        && prefix
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && (4..=8).contains(&hex.len())
        && hex
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A contributor line has no marker, so it reads back as `None`. The visible
/// tail is `title · @handle · #PR · stale`, each part optional but in that
/// order, so the parts come off the right end one at a time. The title may
/// hold ` · ` itself.
fn parse_line(raw: &str) -> Option<Line> {
    let raw = raw.trim();
    let (done, rest) = match (raw.strip_prefix(OPEN_BOX), raw.strip_prefix(DONE_BOX)) {
        (Some(rest), _) => (false, rest),
        (_, Some(rest)) => (true, rest),
        _ => return None,
    };
    let (visible, tail) = rest.rsplit_once(ID_MARKER_OPEN)?;
    let id = tail.strip_suffix(ID_MARKER_CLOSE)?;
    if !is_node_id(id) {
        return None;
    }
    let mut visible = visible.trim_end();
    let mut stale = false;
    if let Some(v) = visible.strip_suffix(" · stale") {
        stale = true;
        visible = v;
    }
    if let Some((v, pr)) = visible.rsplit_once(" · #") {
        if !pr.is_empty() && pr.bytes().all(|b| b.is_ascii_digit()) {
            visible = v;
        }
    }
    let mut handle = None;
    if let Some((v, h)) = visible.rsplit_once(" · @") {
        if is_login(h) {
            handle = Some(h.to_string());
            visible = v;
        }
    }
    // The body is a public page anyone with write access can edit, and a node
    // title reaches agent prompts: only the safe characters get through.
    let title = safe_title(&visible.replace("&lt;", "<"));
    (!title.is_empty()).then(|| Line {
        id: id.to_string(),
        title,
        handle,
        done,
        stale,
    })
}

/// The text between the checklist markers.
fn checklist_of(body: &str) -> Option<&str> {
    let open = body.find(OPEN_MARKER)? + OPEN_MARKER.len();
    let close = open + body[open..].find(CLOSE_MARKER)?;
    Some(&body[open..close])
}

#[derive(Debug, Default)]
struct Outcome {
    filed: usize,
    skipped: usize,
    receipt: Vec<String>,
}

impl Outcome {
    fn skip(&mut self, id: &str, why: &str) {
        self.skipped += 1;
        self.receipt.push(format!("skipped {id}: {why}"));
    }
}

fn mirror_row(id: &str, title: &str, ctx: &Ctx) -> Value {
    json!({
        "id": id,
        "title": title,
        "details": format!(
            "Claimed on https://github.com/{repo}/issues/{n} by {login}. The id is the owner's, so a `Fixes {id}` line in the PR names the owner's node too. Filed by `fno backlog add requests --epic {n}`.",
            repo = ctx.repo, n = ctx.number, login = ctx.login,
        ),
        "type": "feature",
        // A row with no plan reads as an idea: every graph write that
        // supplies plan data re-derives it, so a stored `ready` would not last.
        "status": "idea",
        "priority": "p2",
        "domain": "code",
        "project": ctx.project,
        "cwd": ctx.cwd,
        "parent": Value::Null,
        "epic_issue": ctx.number,
        "source_kind": "from_observation",
        "request_origin": "unknown",
        "origin_evidence": ctx.evidence(),
        "public": false,
        "blocks_everything": false,
        "created_at": crate::graph_store::now_isoformat(),
    })
}

/// File each of the login's live lines that has no local row. Pure over
/// `rows`, so the dry run answers from the body the apply writes.
fn file_lines(rows: &mut Vec<Value>, checklist: &str, ctx: &Ctx) -> Outcome {
    let mut out = Outcome::default();
    let evidence = ctx.evidence();
    for line in checklist.lines().filter_map(parse_line) {
        let mine = line
            .handle
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(ctx.login));
        if !mine {
            continue;
        }
        let id = line.id.as_str();
        if line.done {
            out.skip(id, "the line is done");
            continue;
        }
        if line.stale {
            out.skip(
                id,
                "the claim went stale; comment /claim on the issue again",
            );
            continue;
        }
        if let Some(row) = rows
            .iter()
            .find(|r| crate::graph_store::entry_id(r) == Some(id))
        {
            let held = row
                .get("origin_evidence")
                .and_then(Value::as_str)
                .unwrap_or("none");
            if held == evidence {
                out.skip(id, "already filed");
            } else {
                out.skip(
                    id,
                    &format!(
                        "a local node already holds this id, with origin_evidence {held} and not {evidence}; rename or remove that node first"
                    ),
                );
            }
            continue;
        }
        let mut title = line.title.clone();
        if crate::backlog::title_gate::refusal_message(id, &title).is_some() {
            title = format!("Claimed {id} on {}#{}", ctx.repo, ctx.number);
        }
        rows.push(mirror_row(id, &title, ctx));
        out.filed += 1;
        out.receipt.push(format!("filed {id}: {title}"));
    }
    out
}

fn import(gh: &GitHubTracker, graph: &Path, ctx: &Ctx, apply: bool) -> Result<Outcome, String> {
    let issue = gh
        .read(&format!("{}#{}", ctx.repo, ctx.number))
        .map_err(|e| e.to_string())?;
    let body = issue.details.unwrap_or_default();
    let Some(checklist) = checklist_of(&body) else {
        return Err(format!(
            "{}#{} carries no fno checklist; the owner has not shared that epic",
            ctx.repo, ctx.number
        ));
    };
    if !apply {
        let mut rows = crate::graph_store::read_rows_strict(graph)
            .map_err(|e| format!("graph read failed: {e}"))?;
        return Ok(file_lines(&mut rows, checklist, ctx));
    }
    let mut outcome = Outcome::default();
    crate::graph_store::mutate_rows(graph, WRITE_TIMEOUT, None, None, |rows| {
        outcome = file_lines(rows, checklist, ctx);
        Ok(outcome.filed > 0)
    })
    .map_err(|e| format!("graph write failed: {e}"))?;
    Ok(outcome)
}

fn print(out: &Outcome, apply: bool, json: bool) {
    if json {
        let results: Vec<Value> = out.receipt.iter().map(|l| json!({"line": l})).collect();
        let doc = json!({
            "dry_run": !apply, "filed": out.filed, "skipped": out.skipped, "results": results,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }
    for line in &out.receipt {
        println!("{line}");
    }
    let verb = if apply { "imported" } else { "dry run" };
    println!("{verb}: {} filed, {} skipped", out.filed, out.skipped);
    if !apply {
        println!("dry run: nothing was written; pass --apply to import");
    }
}

pub(crate) fn run(args: &Args) -> i32 {
    let Some(login) = crate::gh_login::current() else {
        eprintln!(
            "requests: --epic could not read your GitHub login from gh. A GH_TOKEN or GITHUB_TOKEN variable hides it: unset the variable for this command, or run `gh auth login`"
        );
        return 1;
    };
    let ctx = Ctx {
        repo: args.repo,
        number: args.number,
        project: args.project,
        cwd: args.cwd,
        login: &login,
    };
    let graph = crate::backlog::settings::graph_path();
    match import(&GitHubTracker::from_env(), &graph, &ctx, args.apply) {
        Ok(out) => {
            print(&out, args.apply, args.json);
            0
        }
        Err(e) => {
            eprintln!("requests: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{node_line, ClaimRecord};
    use super::*;
    use crate::tracker::github::GhRun;

    /// A GitHub whose one issue holds `body`.
    struct Issue(String);

    impl GhRun for Issue {
        fn run(&self, args: &[String]) -> Result<(i32, String, String), String> {
            assert_eq!(&args[..2], ["issue", "view"], "the import only reads");
            let view = json!({"title": "t", "state": "OPEN", "url": "u", "body": self.0});
            Ok((0, view.to_string(), String::new()))
        }
    }

    fn held(node: &str, holder: &str) -> ClaimRecord {
        serde_json::from_value(json!({
            "key": format!("node:{node}"), "holder": holder, "acquired_at": 0,
            "pid": null, "host": "h",
        }))
        .unwrap()
    }

    #[test]
    fn a_rendered_owner_line_reads_back_and_a_contributor_line_does_not() {
        let row =
            json!({"id": "own-1a2b", "title": "Ship Vec<T> and ask @octocat", "pr_number": 12});
        let rendered = node_line(&row, &[held("own-1a2b", "github:Sam")], None, &[]);
        let read = parse_line(&rendered).expect("an owner line carries its id");
        assert_eq!(
            (
                read.id.as_str(),
                read.handle.as_deref(),
                read.done,
                read.stale
            ),
            ("own-1a2b", Some("Sam"), false, false)
        );
        // Markup and mentions in the owner's title do not survive the read.
        assert_eq!(read.title, "Ship Vec T and ask @ octocat");
        // The title cannot forge a marker: the render escapes `<`.
        let forged = json!({"id": "own-2222", "title": "x <!-- fno:evil -->"});
        assert_eq!(
            parse_line(&node_line(&forged, &[], None, &[])).unwrap().id,
            "own-2222"
        );
        let stale = parse_line("- [ ] Gone · @Sam · stale <!-- fno:own-3333 -->").unwrap();
        assert!(stale.stale && stale.handle.as_deref() == Some("Sam"));
        assert!(
            parse_line("- [x] Done · @Sam · #9 <!-- fno:own-4444 -->")
                .unwrap()
                .done
        );
        // A contributor line, a marker whose id is no minted id (a space, a
        // path, a short hex tail), and prose.
        for not_a_line in [
            "- [ ] Idea · @Sam",
            "- [ ] Odd · @Sam <!-- fno:a b -->",
            "- [ ] Odd · @Sam <!-- fno:.. -->",
            "- [ ] Odd · @Sam <!-- fno:own-12 -->",
            "Notes <!-- fno:own-5555 -->",
        ] {
            assert_eq!(parse_line(not_a_line), None, "{not_a_line}");
        }
    }

    #[test]
    fn an_import_files_the_logins_open_lines_once_and_never_overwrites_a_row() {
        let body = format!(
            "intro\n{OPEN_MARKER}\n{}\n{CLOSE_MARKER}\n",
            [
                "- [ ] Fix the picker · @Sam <!-- fno:own-1a2b -->",
                "- [ ] Second · @other <!-- fno:own-2222 -->",
                "- [ ] Idea · @Sam",
                "- [x] Old · @Sam · #9 <!-- fno:own-3333 -->",
                "- [ ] Gone · @Sam · stale <!-- fno:own-4444 -->",
            ]
            .join("\n")
        );
        let dir = tempfile::tempdir().unwrap();
        let graph = dir.path().join("graph.json");
        crate::graph_store::seed_rows(&graph, &[json!({"id": "me-0001", "title": "Mine"})])
            .unwrap();
        let gh = GitHubTracker::new(Some("o/r".into()), Box::new(Issue(body)));
        let ctx = Ctx {
            repo: "o/r",
            number: 7,
            project: "p",
            cwd: "/tmp/p",
            login: "sam",
        };
        let ids = || -> Vec<String> {
            let rows = crate::graph_store::read_rows(&graph).unwrap();
            rows.iter()
                .filter_map(|r| r["id"].as_str().map(str::to_string))
                .collect()
        };

        let dry = import(&gh, &graph, &ctx, false).unwrap();
        assert_eq!((dry.filed, dry.skipped), (1, 2), "{:?}", dry.receipt);
        assert_eq!(ids(), ["me-0001"], "a dry run writes nothing");

        let first = import(&gh, &graph, &ctx, true).unwrap();
        assert_eq!(first.filed, 1);
        let second = import(&gh, &graph, &ctx, true).unwrap();
        assert_eq!(second.filed, 0);
        assert!(second.receipt.iter().any(|l| l.contains("already filed")));
        assert_eq!(
            ids(),
            ["me-0001", "own-1a2b"],
            "one new node, the owner's id"
        );
        let rows = crate::graph_store::read_rows(&graph).unwrap();
        let node = rows.iter().find(|r| r["id"] == "own-1a2b").unwrap();
        assert_eq!(node["epic_issue"], 7);
        assert_eq!(node["origin_evidence"], "github:o/r#7");
        assert_eq!(node["status"], "idea");

        // A row that holds the id for another reason is named, not replaced.
        let mut local =
            vec![json!({"id": "own-1a2b", "title": "Mine", "origin_evidence": "github:o/r#99"})];
        let clash = file_lines(
            &mut local,
            checklist_of(&format!(
                "{OPEN_MARKER}\n- [ ] Fix the picker · @Sam <!-- fno:own-1a2b -->\n{CLOSE_MARKER}"
            ))
            .unwrap(),
            &ctx,
        );
        assert_eq!(clash.filed, 0);
        let said = clash.receipt.join("\n");
        assert!(
            said.contains("github:o/r#99") && said.contains("github:o/r#7"),
            "{said}"
        );
        assert_eq!(local[0]["title"], "Mine");
    }
}
