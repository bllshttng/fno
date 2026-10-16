//! `fno backlog publish`: render the public backlog board, then put it
//! where the world can read it.
//!
//! One command, three targets: `--out <dir>` writes `index.html` for any
//! static host, `--exec <command>` runs an uploader with `{file}` as the
//! staged page (Cloudflare KV, S3, anything), and the default target
//! publishes a `backlog/` subtree to the project repo's `gh-pages` branch
//! through git plumbing, so no branch, index or working tree of the user's
//! checkout moves.
//!
//! The page itself is `fno board-render`'s public projection: selection
//! (open, public, project-scoped rows) and the title leak gate run inside
//! the renderer, and its `deny_unknown_fields` request guard refuses a
//! binary that predates the projection, so an old install can never render
//! the private graph onto a public path.

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const GH_PAGES_REF: &str = "refs/fno/publish/gh-pages";

/// The staged render and its card count, the one input every target takes.
struct Staged {
    dir: PathBuf,
    cards: u64,
}

pub fn run(args: &[String]) -> i32 {
    let mut project: Option<String> = None;
    let mut out: Option<String> = None;
    let mut exec: Option<String> = None;
    let mut url: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize, name: &str| -> Option<String> {
            *i += 1;
            args.get(*i).cloned().or_else(|| {
                eprintln!("publish: {name} needs a value");
                None
            })
        };
        match args[i].as_str() {
            "-h" | "--help" => {
                println!("{}", help_text());
                return 0;
            }
            "--project" => match take(&mut i, "--project") {
                Some(v) => project = Some(v),
                None => return 2,
            },
            "--out" => match take(&mut i, "--out") {
                Some(v) => out = Some(v),
                None => return 2,
            },
            "--exec" => match take(&mut i, "--exec") {
                Some(v) => exec = Some(v),
                None => return 2,
            },
            "--url" => match take(&mut i, "--url") {
                Some(v) => url = Some(v),
                None => return 2,
            },
            other => {
                eprintln!("publish: unknown argument {other}; see --help");
                return 2;
            }
        }
        i += 1;
    }
    if out.is_some() && exec.is_some() {
        eprintln!("publish: --out and --exec refuse together; pick one target");
        return 2;
    }
    // The public projection reads the local graph; under an external tracker
    // backend it would publish a stale or unrelated board. The same refusal
    // the roadmap door runs.
    if super::workflows::active_backend_name() != "graph" {
        eprintln!(
            "publish: the public projection reads the local graph, which is \
             unavailable under an external tracker backend"
        );
        return 2;
    }
    let project = match project {
        Some(p) => p,
        None => {
            match super::create_cli::detect_project(&std::env::current_dir().unwrap_or_default()) {
                Some(p) => p,
                None => {
                    eprintln!("publish: no project maps to the cwd; pass --project <slug>");
                    return 2;
                }
            }
        }
    };
    match publish(&project, out.as_deref(), exec.as_deref(), url.as_deref()) {
        Ok(line) => {
            println!("{line}");
            0
        }
        Err(message) => {
            eprintln!("publish: {message}");
            1
        }
    }
}

fn help_text() -> String {
    "Render the public backlog board and publish it.\n\
     \n\
     USAGE:\n\
     \x20   fno backlog publish [--project <slug>] [--out <dir> | --exec <command>] [--url <url>]\n\
     \n\
     One command, three targets. --out writes <dir>/index.html for any static\n\
     host. --exec runs an uploader with {file} as the staged page (Cloudflare\n\
     KV, S3, anything). With neither, the board publishes a backlog/ subtree to\n\
     the project repo's gh-pages branch through git plumbing; the user's\n\
     checkout never moves.\n\
     \n\
     The page is the public projection: open, public rows only, titles gated\n\
     for home paths and session ids, no details, plans or sessions."
        .to_string()
}

fn publish(
    project: &str,
    out: Option<&str>,
    exec: Option<&str>,
    url: Option<&str>,
) -> Result<String, String> {
    let staged = render_public(project)?;
    // The stage is scratch: every target consumes the staged page before it
    // returns (the gh-pages blob lands in git's object db, the uploader gets
    // its own path), so the dir goes away on the success and error paths
    // alike. The scheduler shape from the docs runs this every 15 minutes.
    let result = (|| {
        if let Some(dir) = out {
            let target = publish_out(&staged.dir, Path::new(dir))?;
            return Ok(format!(
                "published {} cards for {project} to {dir}: {}",
                staged.cards,
                url.unwrap_or(&target)
            ));
        }
        if let Some(text) = exec {
            publish_exec(&staged.dir, text)?;
            let tail = url
                .map(|u| u.to_string())
                .unwrap_or_else(|| "published (pass --url to print the address)".to_string());
            return Ok(format!(
                "published {} cards for {project} to exec: {tail}",
                staged.cards
            ));
        }
        publish_ghpages(project, &staged.dir, staged.cards)
    })();
    let _ = std::fs::remove_dir_all(&staged.dir);
    result
}

/// Render the public projection into a fresh stage dir through the front
/// binary. Stderr rides inherited, so board-render's own withheld-title
/// warning reaches the operator.
fn render_public(project: &str) -> Result<Staged, String> {
    let stage = std::env::temp_dir().join(format!("fno-publish-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).map_err(|e| format!("stage dir: {e}"))?;
    let page = stage.join("index.html");
    let request = json!({
        "targets": [{"path": page.display().to_string(), "scope": project}],
        "public": {"project": project},
    });
    let mut child = Command::new(crate::scrape::fno_bin())
        .arg("board-render")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("cannot run the fno binary: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "board-render took no stdin".to_string())?
        .write_all(request.to_string().as_bytes())
        .map_err(|e| format!("the board-render request failed to write: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("board-render did not finish: {e}"))?;
    let exit = output.status.code().unwrap_or(1);
    let cards = check_receipt(&String::from_utf8_lossy(&output.stdout), exit)?;
    Ok(Staged { dir: stage, cards })
}

/// Fail closed on anything but a written row for the one target: an
/// unreadable receipt or a nonzero exit most likely means the installed
/// binary predates the public projection and refused the request, and a
/// failed row means nothing was staged.
fn check_receipt(text: &str, exit: i32) -> Result<u64, String> {
    let receipt: Value = serde_json::from_str(text).map_err(|_| {
        if exit != 0 {
            format!(
                "board-render exited {exit} with an unreadable receipt; the installed \
                 fno may predate the public board render; run `fno doctor update --rust`"
            )
        } else {
            format!(
                "board-render's receipt was unreadable: {}",
                first_line(text)
            )
        }
    })?;
    if let Some(row) = receipt
        .get("failed")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
    {
        return Err(format!(
            "the render failed: {}",
            row.get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
        ));
    }
    let row = receipt
        .get("written")
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| "board-render wrote nothing and reported no failure".to_string())?;
    let cards = row
        .get("cards")
        .and_then(Value::as_u64)
        .ok_or_else(|| "the written row carried no card count".to_string())?;
    if cards == 0 {
        return Err("the public projection rendered zero cards; nothing published".to_string());
    }
    Ok(cards)
}

fn first_line(text: &str) -> String {
    text.lines()
        .next()
        .unwrap_or("")
        .chars()
        .take(120)
        .collect()
}

/// The static-directory target: stage then rename, so a reader never sees
/// a half page. Returns the file URL.
fn publish_out(stage: &Path, dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let body = std::fs::read(stage.join("index.html")).map_err(|e| format!("read: {e}"))?;
    let tmp = dir.join(".index.html.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("write: {e}"))?;
    std::fs::rename(&tmp, dir.join("index.html")).map_err(|e| format!("rename: {e}"))?;
    let abs = std::fs::canonicalize(dir)
        .map(|p| p.join("index.html"))
        .unwrap_or_else(|_| dir.join("index.html"));
    Ok(format!("file://{}", abs.display()))
}

/// The uploader target: one `sh -c` with the staged page as `$1`, stdio
/// inherited so upload progress and errors are the user's own. The path is
/// an argument, never spliced into the script text, so no character in the
/// temp dir's name can break the uploader's command.
fn publish_exec(stage: &Path, text: &str) -> Result<(), String> {
    if !text.contains("{file}") {
        return Err(
            "--exec needs {file} where the staged page goes; nothing was published".to_string(),
        );
    }
    // A `{file}` the user single-quoted loses its quotes, so the wrapped
    // spelling keeps working; inside single quotes `"$1"` would not expand.
    let script = text
        .replace("'{file}'", "\"$1\"")
        .replace("{file}", "\"$1\"");
    let status = Command::new("sh")
        .arg("-c")
        .arg(&script)
        .arg("sh")
        .arg(stage.join("index.html"))
        .status()
        .map_err(|e| format!("cannot run sh: {e}"))?;
    if !status.success() {
        let code = status.code().unwrap_or(1);
        return Err(format!(
            "exec target exited {code}; nothing else was published"
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The gh-pages target: git plumbing only. Nothing here moves the user's
// HEAD, index or working tree; the commit is built from loose objects and
// pushed by refspec.
// ---------------------------------------------------------------------------

/// One publish over a repo dir: fetch the gh-pages parent, build the new
/// root tree (the `backlog` subtree plus `.nojekyll`, every other file of
/// the parent's tree kept), and push it. `Ok(false)` is the unchanged
/// short-circuit: the built tree equals the parent's, so no commit and no
/// push. This is the function the test drives over a temp repo with a
/// local bare origin.
fn push_ghpages_tree(root: &Path, page: &Path) -> Result<bool, String> {
    let parent = match git(
        root,
        &[
            "fetch",
            "--quiet",
            "origin",
            &format!("+refs/heads/gh-pages:{GH_PAGES_REF}"),
        ],
    ) {
        Ok(_) => Some(GH_PAGES_REF.to_string()),
        Err(e) if e.contains("couldn't find remote ref") => None,
        Err(e) => return Err(format!("fetch of the gh-pages parent failed: {e}")),
    };
    let page = page.to_string_lossy().into_owned();
    let blob = git(root, &["hash-object", "-w", &page])?;
    let subtree = mktree(root, &[format!("100644 blob {blob}\tindex.html")])?;
    let mut entries: Vec<String> = match &parent {
        Some(p) => git(root, &["ls-tree", p])?
            .lines()
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    };
    entries.retain(|l| !l.ends_with("\tbacklog"));
    if !entries.iter().any(|l| l.ends_with("\t.nojekyll")) {
        let empty = git_in(root, &["hash-object", "-w", "--stdin"], None)?;
        entries.push(format!("100644 blob {empty}\t.nojekyll"));
    }
    entries.push(format!("040000 tree {subtree}\tbacklog"));
    // git's canonical tree order compares a tree's name as `name/`, so a
    // plain name sort can emit a tree fsck calls malformed when a tree's
    // name prefixes a sibling file's name (`lib-thing` sorts before `lib/`
    // in git, after `lib` in plain byte order). mktree writes what it reads.
    entries.sort_by_key(|l| tree_sort_key(l.as_str()));
    let tree = mktree(root, &entries)?;
    if let Some(p) = &parent {
        let parent_tree = git(root, &["rev-parse", &format!("{p}^{{tree}}")])?;
        if parent_tree == tree {
            return Ok(false);
        }
    }
    let mut commit_args = vec![
        "commit-tree",
        tree.as_str(),
        "-m",
        "Publish the backlog board",
    ];
    if let Some(p) = &parent {
        commit_args.extend(["-p", p.as_str()]);
    }
    let commit = git(root, &commit_args)?;
    git(
        root,
        &[
            "push",
            "--quiet",
            "origin",
            &format!("{commit}:refs/heads/gh-pages"),
        ],
    )?;
    Ok(true)
}

/// The tree line's git sort key: the name, with `/` appended when the
/// entry is a tree (the mode's second field), which is git's own
/// base-name comparison for tree objects.
fn tree_sort_key(line: &str) -> String {
    let (meta, name) = line.split_once('\t').unwrap_or((line, line));
    if meta.split_whitespace().nth(1) == Some("tree") {
        format!("{name}/")
    } else {
        name.to_string()
    }
}

fn mktree(root: &Path, lines: &[String]) -> Result<String, String> {
    git_in(root, &["mktree"], Some(lines.join("\n").as_str()))
}

/// One git invocation over the repo dir, stdout captured. Stdin carries the
/// command's text input when given (mktree, hash-object --stdin), else it
/// is closed.
fn git_in(root: &Path, args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.args(args).current_dir(root);
    cmd.stdout(Stdio::piped());
    match stdin {
        Some(text) => {
            cmd.stdin(Stdio::piped());
            let mut child = cmd
                .spawn()
                .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
            child
                .stdin
                .take()
                .ok_or_else(|| "git took no stdin".to_string())?
                .write_all(text.as_bytes())
                .map_err(|e| format!("git stdin: {e}"))?;
            let out = child
                .wait_with_output()
                .map_err(|e| format!("git did not finish: {e}"))?;
            finish_git(args, out)
        }
        None => {
            cmd.stdin(Stdio::null());
            let out = cmd
                .output()
                .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))?;
            finish_git(args, out)
        }
    }
}

fn finish_git(_args: &[&str], out: std::process::Output) -> Result<String, String> {
    if out.status.success() {
        return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    git_in(root, args, None)
}

/// `owner` and `repo` out of a GitHub remote URL; anything else refuses,
/// because the gh-pages target only knows GitHub's URL shape.
pub(crate) fn parse_origin(url: &str) -> Option<(String, String)> {
    let url = url.trim().trim_end_matches(".git");
    let path = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
    {
        rest
    } else {
        return None;
    };
    let mut parts = path.splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(owner), Some(repo))
            if !owner.is_empty() && !repo.is_empty() && !repo.contains('/') =>
        {
            Some((owner.to_string(), repo.to_string()))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The Pages API leg: read the repo's Pages state, enable it on the first
// publish, and never switch an existing source.
// ---------------------------------------------------------------------------

enum Pages {
    Enabled {
        html_url: String,
    },
    /// Pages is on but serves something else; the board is not live there.
    Mismatch(String),
    NotFound,
    /// gh missing, or GitHub answered something else. Carries the text.
    Unavailable(String),
}

fn pages_state(owner: &str, repo: &str) -> Pages {
    let mut cmd = Command::new("gh");
    cmd.args(["api", &format!("repos/{owner}/{repo}/pages")]);
    // Behind the fleet budget: output() pipes both streams, so the refused
    // shape reads like any gh failure (exit 75, the ledger line on stderr).
    let out = match crate::gh_budget::output(cmd) {
        Ok(o) => o,
        Err(e) => return Pages::Unavailable(format!("gh api did not finish: {e}")),
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("404") {
            return Pages::NotFound;
        }
        return Pages::Unavailable(err.trim().chars().take(200).collect());
    }
    let state: Value = match serde_json::from_slice(&out.stdout) {
        Ok(v) => v,
        Err(_) => return Pages::Unavailable("gh api returned unreadable JSON".to_string()),
    };
    let build_type = state
        .get("build_type")
        .and_then(Value::as_str)
        .unwrap_or("legacy");
    let source = state.get("source");
    let branch = source
        .and_then(|s| s.get("branch"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let path = source
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if build_type == "workflow" || branch != "gh-pages" || path != "/" {
        return Pages::Mismatch(format!(
            "branch {branch:?} path {path:?} (build_type {build_type})"
        ));
    }
    match state.get("html_url").and_then(Value::as_str) {
        Some(u) => Pages::Enabled {
            html_url: u.to_string(),
        },
        None => Pages::Unavailable("gh api carried no html_url".to_string()),
    }
}

/// The first-publish enable: `source[branch]=gh-pages source[path]=/` and
/// nothing else. A failure leaves the branch pushed; the caller exits 1
/// quoting GitHub's message.
fn enable_pages(owner: &str, repo: &str) -> Result<(), String> {
    let mut cmd = Command::new("gh");
    cmd.args([
        "api",
        "-X",
        "POST",
        &format!("repos/{owner}/{repo}/pages"),
        "-f",
        "source[branch]=gh-pages",
        "-f",
        "source[path]=/",
    ]);
    let out = crate::gh_budget::output(cmd).map_err(|e| format!("cannot run gh: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr)
        .trim()
        .chars()
        .take(200)
        .collect())
}

fn guessed_pages_url(owner: &str, repo: &str) -> String {
    format!("https://{owner}.github.io/{repo}/backlog/")
}

fn publish_ghpages(project: &str, stage: &Path, cards: u64) -> Result<String, String> {
    let root = match super::settings::project_root(project) {
        Some(r) => r,
        None => {
            return Err(format!(
                "project {project:?} has no path in work.workspaces; pass --out or --exec"
            ))
        }
    };
    let root = PathBuf::from(root);
    let remote = git(&root, &["remote", "get-url", "origin"])?;
    let (owner, repo) = parse_origin(&remote).ok_or_else(|| {
        format!(
            "origin {remote:?} is not a GitHub repo; the gh-pages target only knows GitHub. \
             Publish with --out or --exec instead"
        )
    })?;
    let pushed = push_ghpages_tree(&root, &stage.join("index.html"))?;
    let state = pages_state(&owner, &repo);
    // A URL is only claimed when Pages is verified to serve gh-pages /, or
    // when this run just enabled it. Every other state refuses: an unreadable
    // or mismatched Pages source means the printed address would not be live.
    match &state {
        Pages::Enabled { .. } => {}
        Pages::NotFound => {
            if !pushed {
                return Err(format!(
                    "the board is unchanged, but Pages was never enabled for \
                     {owner}/{repo}; nothing is live there. Enable Pages (gh-pages, /) \
                     or publish with --out or --exec"
                ));
            }
            enable_pages(&owner, &repo).map_err(|e| {
                format!(
                    "pushed gh-pages, but enabling Pages failed: {e}. The branch is pushed; \
                          enable Pages for the gh-pages / source and the board goes live"
                )
            })?;
        }
        Pages::Mismatch(desc) => {
            let prefix = if pushed {
                "pushed gh-pages, but"
            } else {
                "the board is unchanged, but"
            };
            return Err(format!(
                "{prefix} Pages serves {desc}; the board is not live there. \
                 Switch Pages to gh-pages /, or publish with --out into the folder Pages serves"
            ));
        }
        Pages::Unavailable(why) => {
            let prefix = if pushed {
                "pushed gh-pages, but"
            } else {
                "the board is unchanged, but"
            };
            return Err(format!(
                "{prefix} the Pages state is unreadable ({why}); the board \
                 is not confirmed live. Check Pages serves gh-pages / for {owner}/{repo}, \
                 or publish with --out or --exec"
            ));
        }
    }
    let url_line = match &state {
        Pages::Enabled { html_url } => {
            format!("{}/backlog/", html_url.trim_end_matches('/'))
        }
        // Pages just came on for gh-pages /: the canonical address.
        _ => guessed_pages_url(&owner, &repo),
    };
    Ok(format!(
        "published {cards} cards for {project} to gh-pages: {url_line}",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh temp dir, removed on drop so parallel tests never share one.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("fno-publish-test-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn stage_page(tag: &str, body: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(tag);
        let page = dir.0.join("index.html");
        std::fs::write(&page, body).unwrap();
        (dir, page)
    }

    #[test]
    fn the_out_target_writes_the_page_and_names_the_url() {
        let (_stage, page) = stage_page("out", "<html>board</html>");
        let out = TempDir::new("out-dir");
        let url = publish_out(&page.parent().unwrap(), &out.0).unwrap();
        assert!(url.starts_with("file://"), "{url}");
        let written = std::fs::read_to_string(out.0.join("index.html")).unwrap();
        assert_eq!(written, "<html>board</html>");
    }

    #[test]
    fn exec_targets_refuse_without_a_slot_or_on_a_nonzero_exit() {
        let (_stage, page) = stage_page("exec", "data");
        let stage = page.parent().unwrap();
        let err = publish_exec(stage, "cp nowhere").unwrap_err();
        assert!(err.contains("{file}"), "{err}");
        publish_exec(stage, "test -f {file}").unwrap();
        let err = publish_exec(stage, "test -f {file} && exit 7").unwrap_err();
        assert!(
            err.contains("exited 7") && err.contains("nothing else was published"),
            "{err}"
        );
        // The staged path rides as `$1`, so a temp dir whose name carries a
        // single quote reaches the uploader intact; the old splice turned
        // that name into a shell syntax error. The wrapped `'{file}'`
        // spelling keeps working beside the bare one.
        let dir =
            std::env::temp_dir().join(format!("fno-publish-quote'dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "data").unwrap();
        publish_exec(&dir, "test -f {file}").unwrap();
        publish_exec(&dir, "test -f '{file}'").unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_receipt_check_fails_closed() {
        assert_eq!(
            check_receipt(r#"{"written":[{"path":"/x","cards":3}]}"#, 0).unwrap(),
            3
        );
        let err = check_receipt(r#"{"failed":[{"path":"/x","error":"boom"}]}"#, 0).unwrap_err();
        assert!(err.contains("boom"), "{err}");
        let err = check_receipt("not json", 2).unwrap_err();
        assert!(err.contains("fno doctor update --rust"), "{err}");
        let err = check_receipt(r#"{"written":[]}"#, 0).unwrap_err();
        assert!(err.contains("wrote nothing"), "{err}");
    }

    /// The plumbing over a temp work repo whose origin is a local bare
    /// repo: the pushed gh-pages tree keeps a parent's other files, adds
    /// backlog/index.html and .nojekyll, the work repo's HEAD never moves,
    /// an identical page short-circuits, and a changed page pushes again.
    #[test]
    fn ghpages_plumbing_keeps_other_files_and_reports_unchanged() {
        let work = TempDir::new("work");
        let origin = TempDir::new("origin");
        git(&work.0, &["init", "--quiet"]).unwrap();
        git(&work.0, &["config", "user.email", "t@t.invalid"]).unwrap();
        git(&work.0, &["config", "user.name", "t"]).unwrap();
        git(&origin.0, &["init", "--bare", "--quiet", "origin.git"]).unwrap();
        let bare = origin.0.join("origin.git");
        let bare_str = bare.to_string_lossy().into_owned();
        git(&work.0, &["remote", "add", "origin", &bare_str]).unwrap();
        // Seed a parent commit with a CNAME, pushed straight to gh-pages.
        let cname = git_in(
            &work.0,
            &["hash-object", "-w", "--stdin"],
            Some("footnote.sh"),
        )
        .unwrap();
        let seed_tree = mktree(&work.0, &[format!("100644 blob {cname}\tCNAME")]).unwrap();
        let seed = git(&work.0, &["commit-tree", &seed_tree, "-m", "seed"]).unwrap();
        git(
            &work.0,
            &[
                "push",
                "--quiet",
                "origin",
                &format!("{seed}:refs/heads/gh-pages"),
            ],
        )
        .unwrap();

        // git orders a tree's name as `name/`, so `lib-thing` sorts before
        // the `lib` tree. mktree writes what it reads; the key must match.
        let mut order = vec![
            "040000 tree b\tlib".to_string(),
            "100644 blob a\tlib-thing".to_string(),
        ];
        order.sort_by_key(|l| tree_sort_key(l.as_str()));
        assert_eq!(order[0], "100644 blob a\tlib-thing");
        assert_eq!(order[1], "040000 tree b\tlib");

        let (_stage, page) = stage_page("gh", "<html>public board</html>");
        assert!(push_ghpages_tree(&work.0, &page).unwrap());
        let listing = git(&bare, &["ls-tree", "refs/heads/gh-pages"]).unwrap();
        assert!(listing.contains("CNAME"), "{listing}");
        assert!(listing.contains(".nojekyll"), "{listing}");
        assert!(listing.contains("backlog"), "{listing}");
        let subtree = git(&bare, &["ls-tree", "refs/heads/gh-pages:backlog"]).unwrap();
        assert!(subtree.contains("index.html"), "{subtree}");
        // The work repo's HEAD is still unborn: no commit, no branch move.
        assert!(git(&work.0, &["rev-parse", "--verify", "-q", "HEAD"]).is_err());

        assert!(
            !push_ghpages_tree(&work.0, &page).unwrap(),
            "an identical page is unchanged"
        );
        let (_stage2, page2) = stage_page("gh2", "<html>public board v2</html>");
        assert!(
            push_ghpages_tree(&work.0, &page2).unwrap(),
            "a changed page pushes again"
        );
    }
}
