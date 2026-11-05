//! The GitHub Issues backend over `gh`, the port of the deleted Python
//! `github_backend.py`.
//!
//! id shape `owner/repo#N`; read and close carry their repo in the id, so
//! they need no configuration, while `list_open` needs the
//! `FNO_TRACKER_GITHUB_REPO` scope. Parent and blockers stay degraded
//! (`None` / `[]`): sub-issues and dependencies cost two REST calls per open
//! issue per refresh, and guessing them costs more than the truth. All gh
//! I/O touches the [`GhRun`] seam so tests feed recorded responses.
//! `gh` on PATH is the fno gh proxy shim, which admits `issue` subcommands
//! through the quota broker; `gh api graphql` is refused by the proxy.

use super::{Candidate, State, Tracker, TrackerError, TrackerNode};
use serde_json::Value;
use std::sync::OnceLock;

/// The one I/O seam. The real impl bounds `gh` at 30 s through
/// `bounded_cmd::output_with_timeout_result`, SIGKILL at the bound.
pub trait GhRun: Send + Sync {
    fn run(&self, args: &[String]) -> Result<(i32, String, String), String>;
}

/// `as_app` puts the GitHub App installation token on the gh child, for the
/// writes that must show the App as the actor.
struct RealGh {
    as_app: bool,
}

impl GhRun for RealGh {
    fn run(&self, args: &[String]) -> Result<(i32, String, String), String> {
        let mut cmd = std::process::Command::new("gh");
        cmd.args(args);
        if self.as_app {
            crate::github_app::inject(&mut cmd, std::ffi::OsStr::new("gh"));
        }
        match crate::bounded_cmd::output_with_timeout_result(cmd, 30) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(
                "gh binary not found on PATH; install GitHub CLI to use the github tracker backend"
                    .into(),
            ),
            Err(e) => Err(format!("gh failed: {e}")),
            Ok(out) => {
                let code = out.status.code().unwrap_or(-1);
                if code == 137 || out.status.code().is_none() {
                    // bounded_cmd SIGKILLs the child's process group at the
                    // bound; a signal death inside the window is the timeout.
                    return Err(format!("gh timed out: gh {}", args.join(" ")));
                }
                Ok((
                    code,
                    String::from_utf8_lossy(&out.stdout).into_owned(),
                    String::from_utf8_lossy(&out.stderr).into_owned(),
                ))
            }
        }
    }
}

/// One approved board request read for the owner-run import. The issue's
/// labels are read only for the both-labels check inside `list_labeled` and
/// never reach a row.
pub struct RequestIssue {
    pub number: i64,
    pub title: String,
    pub body: String,
    pub login: String,
}

/// One comment read off an issue. Visitor text: the caller treats `body` as
/// data and checks `login` itself.
pub struct IssueComment {
    pub id: i64,
    pub login: String,
    pub body: String,
    pub minimized: bool,
}

/// `owner/repo#N`.
pub fn parse_github_id(id: &str) -> Result<(String, String, i64), TrackerError> {
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(r"^([A-Za-z0-9._-]+)/([A-Za-z0-9._-]+)#([0-9]+)$").unwrap()
    });
    let caps = re.captures(id).ok_or_else(|| {
        TrackerError::Backend(format!(
            "not a GitHub issue id: {id:?} (expected owner/repo#N)"
        ))
    })?;
    Ok((
        caps.get(1).unwrap().as_str().to_string(),
        caps.get(2).unwrap().as_str().to_string(),
        caps.get(3).unwrap().as_str().parse::<i64>().unwrap_or(0),
    ))
}

/// `owner/repo`, before it reaches a gh argv.
fn require_repo(repo: &str) -> Result<(), TrackerError> {
    static REPO_RE: OnceLock<regex::Regex> = OnceLock::new();
    let repo_re =
        REPO_RE.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$").unwrap());
    if repo_re.is_match(repo) {
        Ok(())
    } else {
        Err(TrackerError::Backend(format!(
            "not a GitHub repo: {repo:?} (expected owner/repo)"
        )))
    }
}

/// stderr fragments gh emits when an issue does not exist (vs. a network or
/// auth failure, which surfaces as `Backend` so the caller can degrade).
fn not_found_stderr(stderr: &str) -> bool {
    let low = stderr.to_lowercase();
    ["could not resolve", "not found", "no issue"]
        .iter()
        .any(|frag| low.contains(frag))
}

/// gh can return rc=0 with empty or non-JSON stdout (a deprecation notice, a
/// truncated pipe); that is a backend fault, never a parse panic escaping the
/// contract.
fn loads(raw: &str, label: &str) -> Result<Value, TrackerError> {
    if raw.trim().is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_str(raw)
        .map_err(|e| TrackerError::Backend(format!("gh returned non-JSON for {label}: {e}")))
}

fn state_of(raw: Option<&str>) -> State {
    if raw.unwrap_or("").eq_ignore_ascii_case("OPEN") {
        State::Open
    } else {
        State::Closed
    }
}

/// The GitHub Issues tracker. `default_repo` scopes `list_open` /
/// `list_closed_since`; without it they warn and return empty, never a silent
/// fallback to another backend.
pub struct GitHubTracker {
    default_repo: Option<String>,
    gh: Box<dyn GhRun>,
}

impl GitHubTracker {
    pub fn from_env() -> Self {
        Self::new(
            std::env::var("FNO_TRACKER_GITHUB_REPO")
                .ok()
                .filter(|v| !v.trim().is_empty()),
            Box::new(RealGh { as_app: false }),
        )
    }

    /// A tracker scoped to `repo` whose gh calls ride the App token. With no
    /// App configured `inject` leaves the child on the user's token.
    pub fn as_app(repo: &str) -> Self {
        Self::new(Some(repo.to_string()), Box::new(RealGh { as_app: true }))
    }

    pub fn new(default_repo: Option<String>, gh: Box<dyn GhRun>) -> Self {
        Self { default_repo, gh }
    }

    fn run_gh(
        &self,
        args: Vec<String>,
        label: &str,
    ) -> Result<(i32, String, String), TrackerError> {
        self.gh
            .run(&args)
            .map_err(|e| TrackerError::Backend(format!("gh failed for {label}: {e}")))
    }

    /// Open issues carrying both the request label and the approval label.
    /// Not a `Tracker` method: Linear and the graph have no such read. The
    /// `--label` filters only narrow what `gh` returns; the Rust-side
    /// both-labels check below is the authority, because GitHub label names
    /// ignore case and how `gh` combines two filters is not relied on here.
    pub fn list_labeled(
        &self,
        repo: &str,
        request_label: &str,
        approved_label: &str,
        limit: u32,
    ) -> Result<Vec<RequestIssue>, TrackerError> {
        require_repo(repo)?;
        static LABEL_RE: OnceLock<regex::Regex> = OnceLock::new();
        let label_re =
            LABEL_RE.get_or_init(|| regex::Regex::new(r"^[A-Za-z0-9._ -]{1,50}$").unwrap());
        for label in [request_label, approved_label] {
            if !label_re.is_match(label) {
                return Err(TrackerError::Backend(format!(
                    "not a usable label: {label:?} (expected 1-50 letters, digits, dots, underscores, spaces or dashes)"
                )));
            }
        }
        if request_label.eq_ignore_ascii_case(approved_label) {
            return Err(TrackerError::Backend(format!(
                "the approval label equals the request label ({request_label}); a visitor could approve their own issue"
            )));
        }
        let (rc, out, err) = self.run_gh(
            vec![
                "issue".into(),
                "list".into(),
                "-R".into(),
                repo.to_string(),
                "--state".into(),
                "open".into(),
                "--label".into(),
                request_label.to_string(),
                "--label".into(),
                approved_label.to_string(),
                "--json".into(),
                "number,title,body,author,labels".into(),
                "--limit".into(),
                limit.to_string(),
            ],
            repo,
        )?;
        if rc != 0 {
            return Err(TrackerError::Backend(format!(
                "gh issue list failed for {repo}: {}",
                err.trim()
            )));
        }
        let data = loads(&out, repo)?;
        let items = match data {
            Value::Array(items) => items,
            _ => Vec::new(),
        };
        Ok(items
            .iter()
            .filter_map(|it| {
                let number = it.get("number").and_then(Value::as_i64)?;
                if number < 1 {
                    return None;
                }
                let names: Vec<String> = it
                    .get("labels")
                    .and_then(Value::as_array)
                    .map(|ls| {
                        ls.iter()
                            .filter_map(|l| l.get("name").and_then(Value::as_str))
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let carries = |label: &str| names.iter().any(|n| n.eq_ignore_ascii_case(label));
                if !carries(request_label) || !carries(approved_label) {
                    return None;
                }
                Some(RequestIssue {
                    number,
                    title: it
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    body: it
                        .get("body")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    login: it
                        .get("author")
                        .and_then(|a| a.get("login"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
            })
            .collect())
    }

    /// Open one issue and return its number. `gh issue create` prints the new
    /// issue's URL on its last stdout line.
    pub fn create_issue(&self, repo: &str, title: &str, body: &str) -> Result<i64, TrackerError> {
        require_repo(repo)?;
        let (rc, out, err) = self.run_gh(
            vec![
                "issue".into(),
                "create".into(),
                "-R".into(),
                repo.to_string(),
                "--title".into(),
                title.to_string(),
                "--body".into(),
                body.to_string(),
            ],
            repo,
        )?;
        if rc != 0 {
            return Err(TrackerError::Backend(format!(
                "gh issue create failed for {repo}: {}",
                err.trim()
            )));
        }
        let url = out
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("");
        url.trim()
            .rsplit_once("/issues/")
            .and_then(|(_, n)| n.parse::<i64>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| {
                TrackerError::Backend(format!(
                    "gh issue create for {repo} printed no issue URL: {:?}",
                    out.trim()
                ))
            })
    }

    /// Replace one issue's body.
    pub fn edit_issue_body(&self, repo: &str, number: i64, body: &str) -> Result<(), TrackerError> {
        require_repo(repo)?;
        let id = format!("{repo}#{number}");
        let (rc, _out, err) = self.run_gh(
            vec![
                "issue".into(),
                "edit".into(),
                number.to_string(),
                "-R".into(),
                repo.to_string(),
                "--body".into(),
                body.to_string(),
            ],
            &id,
        )?;
        if rc != 0 {
            if not_found_stderr(&err) {
                return Err(TrackerError::NotFound(id));
            }
            return Err(TrackerError::Backend(format!(
                "gh issue edit failed for {id}: {}",
                err.trim()
            )));
        }
        Ok(())
    }

    /// The issue author's login and every comment, oldest first. `id` is the
    /// number ending the comment URL: it only grows, so a cursor can sit on it,
    /// which GraphQL's opaque node id cannot do.
    pub fn list_issue_comments(
        &self,
        repo: &str,
        number: i64,
    ) -> Result<(String, Vec<IssueComment>), TrackerError> {
        require_repo(repo)?;
        let id = format!("{repo}#{number}");
        let (rc, out, err) = self.run_gh(
            vec![
                "issue".into(),
                "view".into(),
                number.to_string(),
                "-R".into(),
                repo.to_string(),
                "--json".into(),
                "author,comments".into(),
            ],
            &id,
        )?;
        if rc != 0 {
            if not_found_stderr(&err) {
                return Err(TrackerError::NotFound(id));
            }
            return Err(TrackerError::Backend(format!(
                "gh issue view failed for {id}: {}",
                err.trim()
            )));
        }
        let data = loads(&out, &id)?;
        let mut comments: Vec<IssueComment> = data
            .get("comments")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|c| {
                        let url = c.get("url").and_then(Value::as_str)?;
                        let id = url
                            .rsplit_once("#issuecomment-")?
                            .1
                            .parse::<i64>()
                            .ok()
                            .filter(|n| *n > 0)?;
                        Some(IssueComment {
                            id,
                            login: c
                                .pointer("/author/login")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            body: c
                                .get("body")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string(),
                            minimized: c
                                .get("isMinimized")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        comments.sort_by_key(|c| c.id);
        let author = data
            .pointer("/author/login")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok((author, comments))
    }

    /// Post one comment on an issue.
    pub fn comment_issue(&self, repo: &str, number: i64, body: &str) -> Result<(), TrackerError> {
        require_repo(repo)?;
        let id = format!("{repo}#{number}");
        let (rc, _out, err) = self.run_gh(
            vec![
                "issue".into(),
                "comment".into(),
                number.to_string(),
                "-R".into(),
                repo.to_string(),
                "--body".into(),
                body.to_string(),
            ],
            &id,
        )?;
        if rc != 0 {
            if not_found_stderr(&err) {
                return Err(TrackerError::NotFound(id));
            }
            return Err(TrackerError::Backend(format!(
                "gh issue comment failed for {id}: {}",
                err.trim()
            )));
        }
        Ok(())
    }
}

impl Tracker for GitHubTracker {
    fn name(&self) -> &str {
        "github"
    }

    fn read(&self, id: &str) -> Result<TrackerNode, TrackerError> {
        let (owner, repo, number) = parse_github_id(id)?;
        let (rc, out, err) = self.run_gh(
            vec![
                "issue".into(),
                "view".into(),
                number.to_string(),
                "-R".into(),
                format!("{owner}/{repo}"),
                "--json".into(),
                "title,state,body,url".into(),
            ],
            id,
        )?;
        if rc != 0 {
            if not_found_stderr(&err) {
                return Err(TrackerError::NotFound(id.to_string()));
            }
            return Err(TrackerError::Backend(format!(
                "gh issue view failed for {id}: {}",
                err.trim()
            )));
        }
        let data = loads(&out, id)?;
        Ok(TrackerNode {
            id: id.to_string(),
            title: data
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            state: state_of(data.get("state").and_then(Value::as_str)),
            parent: None,
            blocked_by: vec![],
            details: data.get("body").and_then(Value::as_str).map(str::to_string),
            url: data.get("url").and_then(Value::as_str).map(str::to_string),
            size: None,
        })
    }

    fn list_open(&self) -> Result<Vec<Candidate>, TrackerError> {
        let Some(repo) = &self.default_repo else {
            eprintln!(
                "fno tracker: github backend has no FNO_TRACKER_GITHUB_REPO scope; \
                 list_open returns nothing. Set it to enumerate issues."
            );
            return Ok(Vec::new());
        };
        let (rc, out, err) = self.run_gh(
            vec![
                "issue".into(),
                "list".into(),
                "-R".into(),
                repo.clone(),
                "--state".into(),
                "open".into(),
                "--json".into(),
                "number,title,state,createdAt,body,url".into(),
                "--limit".into(),
                "1000".into(),
            ],
            repo,
        )?;
        if rc != 0 {
            return Err(TrackerError::Backend(format!(
                "gh issue list failed for {repo}: {}",
                err.trim()
            )));
        }
        let data = loads(&out, repo)?;
        let items = match data {
            Value::Array(items) => items,
            _ => Vec::new(),
        };
        Ok(items
            .iter()
            .map(|it| Candidate {
                node: TrackerNode {
                    id: format!(
                        "{repo}#{}",
                        it.get("number").and_then(Value::as_i64).unwrap_or(0)
                    ),
                    title: it.get("title").and_then(Value::as_str).map(str::to_string),
                    state: state_of(it.get("state").and_then(Value::as_str)),
                    parent: None,
                    blocked_by: vec![],
                    details: it.get("body").and_then(Value::as_str).map(str::to_string),
                    url: it.get("url").and_then(Value::as_str).map(str::to_string),
                    size: None,
                },
                priority: "p2".to_string(),
                rank: None,
                created_at: it
                    .get("createdAt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                closed_at: None,
            })
            .collect())
    }

    fn list_closed_since(&self, days: u32) -> Option<Result<Vec<Candidate>, TrackerError>> {
        let inner = || -> Result<Vec<Candidate>, TrackerError> {
            let Some(repo) = self.default_repo.as_deref() else {
                return Ok(Vec::new());
            };
            let since =
                (chrono::Utc::now() - chrono::Duration::days(days as i64)).format("%Y-%m-%d");
            let (rc, out, err) = self.run_gh(
                vec![
                    "issue".into(),
                    "list".into(),
                    "-R".into(),
                    repo.to_string(),
                    "--state".into(),
                    "closed".into(),
                    "--search".into(),
                    format!("closed:>={since}"),
                    "--json".into(),
                    "number,title,state,createdAt,closedAt,body,url".into(),
                    "--limit".into(),
                    "200".into(),
                ],
                repo,
            )?;
            if rc != 0 {
                return Err(TrackerError::Backend(format!(
                    "gh issue list failed for {repo}: {}",
                    err.trim()
                )));
            }
            let data = loads(&out, repo)?;
            let items = match data {
                Value::Array(items) => items,
                _ => Vec::new(),
            };
            Ok(items
                .iter()
                .map(|it| Candidate {
                    node: TrackerNode {
                        id: format!(
                            "{repo}#{}",
                            it.get("number").and_then(Value::as_i64).unwrap_or(0)
                        ),
                        title: it.get("title").and_then(Value::as_str).map(str::to_string),
                        state: State::Closed,
                        parent: None,
                        blocked_by: vec![],
                        details: it.get("body").and_then(Value::as_str).map(str::to_string),
                        url: it.get("url").and_then(Value::as_str).map(str::to_string),
                        size: None,
                    },
                    priority: "p2".to_string(),
                    rank: None,
                    created_at: it
                        .get("createdAt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    closed_at: it
                        .get("closedAt")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .collect())
        };
        Some(inner())
    }

    fn close(&self, id: &str) -> Result<(), TrackerError> {
        let (owner, repo, number) = parse_github_id(id)?;
        let (rc, _out, err) = self.run_gh(
            vec![
                "issue".into(),
                "close".into(),
                number.to_string(),
                "-R".into(),
                format!("{owner}/{repo}"),
            ],
            id,
        )?;
        if rc != 0 {
            if not_found_stderr(&err) {
                return Err(TrackerError::NotFound(id.to_string()));
            }
            return Err(TrackerError::Backend(format!(
                "gh issue close failed for {id}: {}",
                err.trim()
            )));
        }
        Ok(())
    }
}
