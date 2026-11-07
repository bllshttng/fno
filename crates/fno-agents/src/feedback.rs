//! `fno do feedback`: draft a redacted upstream issue from a node or the
//! user's own words. The command files nothing. It prints the draft and a
//! prefilled `issues/new` link; the user's Submit click on github.com is the
//! only publish step, because an agent in a pane can answer any prompt.
//!
//! The redaction composes the shapes fno already carries: secret shapes
//! (`intel_html`), emails (`correction_detect`), home paths and session ids
//! (`backlog::title_gate`), vault references and work-map project names
//! (`skill_diff`). After masking, every class is scanned again; a surviving
//! match refuses the draft and the refusal names classes, never text.

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

/// The measured `issues/new` bound: a 6,000 character body answered 302,
/// 7,000 answered 500, 9,000 answered 414.
const LINK_MAX_CHARS: usize = 6000;
const LABEL: &str = "from-fno";
const DEFAULT_REPO: &str = "fnodotdev/fno";

const HELP: &str = "Draft a redacted upstream issue (nothing is filed)

Usage: fno do feedback (--node <id> | --text \"<words>\") [--kind bug|feature] [--json] [--offline]

  --node <id>     Draft from a backlog node's title and details.
  --text <words>  Draft from the user's own words.
  --kind          bug (default) or feature. A node's type picks it when absent.
  --json, -J      Print one JSON object instead of the readable draft.
  --offline       Skip the duplicate search on GitHub.

Redaction classes, masked in this order and rescanned before output: secret,
path (home paths), email, session (ids), private_path (vault and state
paths), project (work-map names except fno), user_host (login and host name).

Exit codes: 0 draft printed, 2 misuse, 3 refused by the rescan (no draft),
4 node unreadable. The command prints a link; open it and press Submit to
file the issue. It never files anything itself.";

/// Hits per class. Every class is present, so the JSON shape is fixed.
pub(crate) type Counts = BTreeMap<&'static str, usize>;

const CLASSES: [&str; 7] = [
    "secret",
    "path",
    "email",
    "session",
    "private_path",
    "project",
    "user_host",
];

/// What the redaction needs from the machine: work-map project names and the
/// login and host names. Tests build one by hand.
#[derive(Debug, Default, Clone)]
pub(crate) struct Ctx {
    projects: Vec<String>,
    user: Option<String>,
    host: Option<String>,
    home: Option<String>,
}

impl Ctx {
    fn from_machine(cwd: &Path) -> Self {
        let user = ["USER", "LOGNAME"]
            .iter()
            .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()));
        let host = std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ctx {
            projects: crate::skill_diff::project_names(cwd),
            user,
            host,
            home: std::env::var("HOME").ok().filter(|home| !home.is_empty()),
        }
    }

    /// Names worth masking: three or more characters, so a login of `a`
    /// never rewrites every article.
    fn names(&self) -> Vec<&str> {
        self.user
            .iter()
            .chain(self.host.iter())
            .map(String::as_str)
            .filter(|n| n.chars().count() >= 3)
            .collect()
    }
}

fn rx(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).expect("static regex"))
}

fn private_path_rx() -> &'static Regex {
    static RX: OnceLock<Regex> = OnceLock::new();
    rx(&RX, r"\binternal/\S*|\S*/\.fno/\S*")
}

fn leak_rx(class: &str) -> &'static Regex {
    crate::backlog::title_gate::leak_patterns()
        .iter()
        .find(|(name, _)| *name == class)
        .map(|(_, rx)| rx)
        .expect("title_gate carries the class")
}

/// Replace whole-word, case-insensitive occurrences of `token`. A word edge
/// is any neighbour that is not an ASCII letter or digit.
fn replace_token(text: &str, token: &str, with: &str) -> (String, usize) {
    if token.is_empty() {
        return (text.to_string(), 0);
    }
    let (hay, needle) = (text.to_ascii_lowercase(), token.to_ascii_lowercase());
    let (mut out, mut count, mut from) = (String::new(), 0usize, 0usize);
    while let Some(pos) = hay[from..].find(&needle) {
        let at = from + pos;
        let end = at + needle.len();
        let edge = |c: Option<char>| c.is_none_or(|c| !c.is_ascii_alphanumeric());
        let ok = edge(hay[..at].chars().next_back()) && edge(hay[end..].chars().next());
        out.push_str(&text[from..at]);
        if ok {
            out.push_str(with);
            count += 1;
        } else {
            out.push_str(&text[at..end]);
        }
        from = end;
    }
    out.push_str(&text[from..]);
    (out, count)
}

/// Mask every private class. Order matters: secrets first (a token can sit
/// inside a path), then paths, then the rest.
pub(crate) fn redact(text: &str, ctx: &Ctx) -> (String, Counts) {
    let mut counts: Counts = CLASSES.iter().map(|c| (*c, 0)).collect();
    let secret = crate::intel_html::secret_rx();
    counts.insert("secret", secret.find_iter(text).count());
    let mut out = secret
        .replace_all(text, |caps: &regex::Captures| {
            match (caps.get(2), caps.get(4)) {
                (Some(key), _) => format!("{}[redacted]", key.as_str()),
                (None, Some(url)) => format!("{}[redacted]@", url.as_str()),
                (None, None) => "[redacted]".to_string(),
            }
        })
        .into_owned();
    if let Some(home) = ctx.home.as_ref().filter(|home| !home.is_empty()) {
        let home = Regex::new(&format!(r"{}(?:/[^\s]+)?", regex::escape(home)))
            .expect("escaped home path");
        *counts.entry("path").or_default() += home.find_iter(&out).count();
        out = home.replace_all(&out, "<local path>").into_owned();
    }
    for (class, rx, with) in [
        ("path", leak_rx("home-path"), "<local path>"),
        ("email", crate::correction_detect::email_rx(), "[email]"),
        ("session", leak_rx("session-id"), "[session]"),
        ("private_path", private_path_rx(), "<private path>"),
    ] {
        *counts.entry(class).or_default() += rx.find_iter(&out).count();
        out = rx.replace_all(&out, with).into_owned();
    }
    let named = ctx
        .projects
        .iter()
        .map(|n| (n.trim(), "[project]", "project"))
        .chain(ctx.user.iter().map(|n| (n.as_str(), "[user]", "user_host")))
        .chain(ctx.host.iter().map(|n| (n.as_str(), "[host]", "user_host")));
    for (name, with, class) in named {
        if name.is_empty() || name == "fno" || (class == "user_host" && name.chars().count() < 3) {
            continue;
        }
        let (next, n) = replace_token(&out, name, with);
        out = next;
        *counts.entry(class).or_default() += n;
    }
    (out, counts)
}

/// A secret shape whose value is not the `[redacted]` marker. The masked
/// `token=[redacted]` still matches the key=value shape, so the marker itself
/// must not count as a survivor.
fn secret_survives(text: &str) -> bool {
    crate::intel_html::secret_rx()
        .captures_iter(text)
        .any(|caps| {
            let value = caps.get(3).or_else(|| caps.get(5)).or_else(|| caps.get(0));
            value.is_some_and(|m| !m.as_str().starts_with("[redacted]"))
        })
}

/// Every class still present in `text`, in the vocabulary order. Names the
/// class, never the matched text.
fn residual_classes(text: &str, ctx: &Ctx) -> Vec<&'static str> {
    let mut found = Vec::new();
    if secret_survives(text) {
        found.push("secret");
    }
    if leak_rx("home-path").is_match(text)
        || ctx
            .home
            .as_ref()
            .is_some_and(|home| !home.is_empty() && text.contains(home))
    {
        found.push("path");
    }
    if crate::correction_detect::email_rx().is_match(text) {
        found.push("email");
    }
    if leak_rx("session-id").is_match(text) {
        found.push("session");
    }
    let hits = crate::skill_diff::redaction_violations(text, &ctx.projects);
    if hits.iter().any(|h| !h.starts_with("project:")) {
        found.push("private_path");
    }
    if hits.iter().any(|h| h.starts_with("project:")) {
        found.push("project");
    }
    let lowered = text.to_ascii_lowercase();
    let named = ctx.names().into_iter().any(|n| {
        let probe = replace_token(&lowered, n, "\u{0}");
        probe.1 > 0
    });
    if named {
        found.push("user_host");
    }
    found
}

/// The rescan: a surviving match refuses the draft.
pub(crate) fn check_clean(title: &str, body: &str, ctx: &Ctx) -> Result<(), String> {
    let mut classes = residual_classes(title, ctx);
    classes.extend(residual_classes(body, ctx));
    classes.sort_unstable();
    classes.dedup();
    if classes.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "feedback: refused, the redacted draft still carries: {}. Nothing was printed.",
            classes.join(", ")
        ))
    }
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `(url, url_has_body)`. When the full link passes the measured bound the
/// link carries the title only and the user pastes the printed body.
fn issue_link(repo: &str, kind: &str, title: &str, body: &str) -> (String, bool) {
    let template = if kind == "feature" {
        "feature_request.md"
    } else {
        "bug_report.md"
    };
    let labels = format!(
        "{LABEL},{}",
        if kind == "feature" {
            "enhancement"
        } else {
            "bug"
        }
    );
    let head = format!(
        "https://github.com/{repo}/issues/new?template={}&labels={}&title={}",
        percent_encode(template),
        percent_encode(&labels),
        percent_encode(title),
    );
    let full = format!("{head}&body={}", percent_encode(body));
    if full.len() <= LINK_MAX_CHARS {
        (full, true)
    } else {
        (head, false)
    }
}

fn body_for(kind: &str, words: &str, reference: &str, harness: &str) -> String {
    let env = format!(
        "- footnote version: {}\n- OS: {}\n- CLI runtime: {harness}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
    );
    let (first, rest): (&[(&str, &str)], &[(&str, &str)]) = if kind == "feature" {
        (
            &[("What problem are you trying to solve?", words)],
            &[
                ("Proposed solution", "_not stated_"),
                ("Alternatives considered", "_not stated_"),
                (
                    "Additional context",
                    "_drafted by fno from a local finding_",
                ),
            ],
        )
    } else {
        (
            &[("What happened?", words)],
            &[
                ("What did you expect to happen?", "_not stated_"),
                ("Steps to reproduce", "_not stated_"),
                ("Environment", env.as_str()),
                ("Logs or output", "_none attached_"),
            ],
        )
    };
    let mut out = String::new();
    for (heading, text) in first.iter().chain(rest.iter()) {
        out.push_str(&format!("## {heading}\n\n{text}\n\n"));
    }
    out.push_str(&format!("fno-feedback-ref: {reference}\n"));
    out
}

fn sha12(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The repo the issue targets: the plugin manifest's `supportUrl`, else the
/// published default.
fn repo_slug() -> String {
    crate::provider::plugin_root()
        .and_then(|root| std::fs::read_to_string(root.join(".claude-plugin/plugin.json")).ok())
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|v| v.get("supportUrl")?.as_str().map(str::to_string))
        .and_then(|url| {
            let rest = url.strip_prefix("https://github.com/")?;
            let mut parts = rest.split('/');
            Some(format!("{}/{}", parts.next()?, parts.next()?))
        })
        .unwrap_or_else(|| DEFAULT_REPO.to_string())
}

type GhFn<'a> = dyn Fn(&[String]) -> Result<String, String> + 'a;

/// One bounded `gh` call through the same 30 second transport as the tracker.
fn real_gh(args: &[String]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("gh");
    cmd.args(args);
    let out = crate::bounded_cmd::output_with_timeout_result(cmd, 30)
        .map_err(|e| format!("gh failed: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("gh exited {}", out.status.code().unwrap_or(-1)))
    }
}

/// Up to five open or closed issues: the ref query first, then the title
/// terms. A failed read never blocks the draft; the first reason is kept.
fn dedup(gh: &GhFn, repo: &str, reference: &str, title: &str) -> (Vec<Value>, Option<String>) {
    let terms: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 4)
        .take(6)
        .map(str::to_string)
        .collect();
    let mut queries = vec![format!(
        "repo:{repo} is:issue \"fno-feedback-ref: {reference}\""
    )];
    if !terms.is_empty() {
        queries.push(format!("repo:{repo} is:issue {}", terms.join(" ")));
    }
    let (mut hits, mut unread): (Vec<Value>, Option<String>) = (Vec::new(), None);
    for q in queries {
        let args: Vec<String> = ["api", "-X", "GET", "search/issues", "-f"]
            .iter()
            .map(|s| s.to_string())
            .chain([format!("q={q}")])
            .collect();
        match gh(&args).and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.to_string())) {
            Ok(v) => {
                for item in v
                    .get("items")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let number = item.get("number").cloned().unwrap_or(Value::Null);
                    if hits.iter().all(|h| h.get("number") != Some(&number)) && hits.len() < 5 {
                        hits.push(json!({
                            "number": number,
                            "title": item.get("title").cloned().unwrap_or(Value::Null),
                            "url": item.get("html_url").cloned().unwrap_or(Value::Null),
                            "state": item.get("state").cloned().unwrap_or(Value::Null),
                        }));
                    }
                }
            }
            Err(e) => {
                unread.get_or_insert(e);
            }
        }
    }
    (hits, unread)
}

#[derive(Debug)]
pub(crate) struct Draft {
    repo: String,
    kind: String,
    title: String,
    body: String,
    url: String,
    url_has_body: bool,
    dedup: Vec<Value>,
    dedup_unread: Option<String>,
    counts: Counts,
    reference: String,
}

pub(crate) enum Source<'a> {
    Node {
        id: &'a str,
        title: String,
        details: String,
        kind: String,
    },
    Text(&'a str),
}

/// Build the draft. `Err` is the rescan refusal.
pub(crate) fn build(
    source: &Source,
    kind_flag: Option<&str>,
    ctx: &Ctx,
    repo: &str,
    gh: Option<&GhFn>,
) -> Result<Draft, String> {
    let (raw_title, raw_words, node_kind, id) = match source {
        Source::Node {
            id,
            title,
            details,
            kind,
        } => (
            title.clone(),
            details.clone(),
            Some(kind.as_str()),
            Some(*id),
        ),
        Source::Text(text) => {
            let line = text.lines().next().unwrap_or("");
            (line.to_string(), text.to_string(), None, None)
        }
    };
    let kind = match kind_flag.or(node_kind) {
        Some("feature") => "feature",
        _ => "bug",
    };
    let (title, mut counts) = redact(&raw_title, ctx);
    let (words, word_counts) = redact(&raw_words, ctx);
    // The text title is the body's first line, so its hits count once.
    let title: String = if id.is_some() {
        title
    } else {
        counts = CLASSES.iter().map(|c| (*c, 0)).collect();
        title.chars().take(80).collect()
    };
    for (class, n) in word_counts {
        *counts.entry(class).or_default() += n;
    }
    let reference = id.map(str::to_string).unwrap_or_else(|| sha12(&title));
    let harness = std::env::var("FNO_HARNESS_NAME").unwrap_or_else(|_| "unknown".into());
    let title = format!(
        "[{}] {}",
        if kind == "feature" { "feature" } else { "bug" },
        title
    );
    let body = body_for(kind, &words, &reference, &harness);
    check_clean(&title, &body, ctx)?;
    let (url, url_has_body) = issue_link(repo, kind, &title, &body);
    let (dedup, dedup_unread) = match gh {
        Some(gh) => dedup(gh, repo, &reference, &title),
        None => (Vec::new(), None),
    };
    Ok(Draft {
        repo: repo.to_string(),
        kind: kind.to_string(),
        title,
        body,
        url,
        url_has_body,
        dedup,
        dedup_unread,
        counts,
        reference,
    })
}

fn render_json(d: &Draft) -> Value {
    json!({
        "repo": d.repo, "kind": d.kind, "title": d.title, "body": d.body,
        "url": d.url, "url_has_body": d.url_has_body,
        "dedup": { "matches": d.dedup, "unread": d.dedup_unread },
        "redactions": json!(d.counts), "ref": d.reference,
    })
}

fn render_text(d: &Draft) -> String {
    let mut out = format!("Title: {}\n\n{}\n", d.title, d.body);
    out.push_str(&format!(
        "Redactions: {}\n",
        d.counts.values().sum::<usize>()
    ));
    for m in &d.dedup {
        out.push_str(&format!(
            "dedup: #{} {} {} ({})\n",
            m["number"],
            m["title"].as_str().unwrap_or(""),
            m["url"].as_str().unwrap_or(""),
            m["state"].as_str().unwrap_or(""),
        ));
    }
    if let Some(why) = &d.dedup_unread {
        out.push_str(&format!("dedup: unread ({why})\n"));
    }
    if !d.url_has_body {
        out.push_str("The body is too long for the link. Paste the body above into the issue.\n");
    }
    out.push_str(
        "Read the whole draft first: no pattern knows a private name that is not a project.\n",
    );
    out.push_str(&format!("{}\n", d.url));
    out.push_str("Nothing was filed. Open the link and press Submit to file it.\n");
    out
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

/// Entry for `fno do feedback <tail>`; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{HELP}");
        return 0;
    }
    let node = flag_value(args, "--node");
    let text = flag_value(args, "--text");
    let kind_flag = flag_value(args, "--kind");
    if node.is_some() == text.is_some() {
        eprintln!("feedback: pass exactly one of --node <id> or --text \"<words>\". See --help.");
        return 2;
    }
    if kind_flag
        .as_deref()
        .is_some_and(|k| k != "bug" && k != "feature")
    {
        eprintln!("feedback: --kind wants bug or feature.");
        return 2;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let source = match (&node, &text) {
        (Some(id), _) => {
            let store = crate::backlog::api::Store::new(&crate::graph_get::graph_path_for(&cwd));
            match crate::backlog::api::node(&store, id) {
                Ok(Some(n)) => Source::Node {
                    id,
                    title: n.title,
                    details: n.description.unwrap_or_default(),
                    kind: n.kind,
                },
                _ => {
                    eprintln!("feedback: node {id} could not be read. No draft was made.");
                    return 4;
                }
            }
        }
        (None, Some(words)) => Source::Text(words),
        (None, None) => return 2,
    };
    let offline = args.iter().any(|a| a == "--offline");
    let gh: Option<&GhFn> = if offline {
        None
    } else {
        Some(&real_gh as &GhFn)
    };
    let ctx = Ctx::from_machine(&cwd);
    match build(&source, kind_flag.as_deref(), &ctx, &repo_slug(), gh) {
        Ok(draft) => {
            if crate::json_output::requested(args) {
                println!("{}", render_json(&draft));
            } else {
                print!("{}", render_text(&draft));
            }
            0
        }
        Err(refusal) => {
            eprintln!("{refusal}");
            3
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx {
            projects: vec!["acmecorp".into(), "fno".into()],
            user: Some("jdoe42".into()),
            host: Some("privatehost".into()),
            home: Some("/srv/report-user-home".into()),
        }
    }

    const FIXTURE: &str = "token=ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 mail jane@example.com \
        at /Users/jane/x/y session 0f8e2f4a-1b2c-4d5e-8f90-123456789abc \
        in acmecorp, vault internal/notes/a.md, login jdoe42, host privatehost session ses_abc123Private /srv/report-user-home/private.txt";

    #[test]
    fn draft_masks_every_private_class() {
        let draft = build(&Source::Text(FIXTURE), None, &ctx(), "o/r", None).unwrap();
        for leaked in [
            "ghp_",
            "example.com",
            "/Users/",
            "0f8e2f4a",
            "acmecorp",
            "internal/",
            "jdoe42",
            "privatehost",
            "ses_abc123Private",
            "/srv/report-user-home",
        ] {
            assert!(
                !draft.title.contains(leaked) && !draft.body.contains(leaked),
                "{leaked}"
            );
        }
        assert_eq!(draft.counts.values().sum::<usize>(), 10);
        assert!(draft.url_has_body && draft.url.len() <= LINK_MAX_CHARS);
        let padded = format!("{FIXTURE} {}", "word ".repeat(2000));
        let long = build(&Source::Text(&padded), None, &ctx(), "o/r", None).unwrap();
        assert!(!long.url_has_body && !long.url.contains("&body="));
    }

    #[test]
    fn a_surviving_secret_refuses_the_draft() {
        let planted = "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8";
        let refusal = check_clean("[bug] x", planted, &ctx()).unwrap_err();
        assert!(refusal.contains("secret") && !refusal.contains("ghp_"));
        assert!(check_clean("[bug] x", "token=[redacted]", &ctx()).is_ok());
    }

    #[test]
    fn dedup_reads_the_ref_and_never_blocks() {
        let calls = std::cell::Cell::new(0);
        let gh = |_: &[String]| -> Result<String, String> {
            calls.set(calls.get() + 1);
            if calls.get() == 1 {
                Ok(r#"{"items":[{"number":7,"title":"t","html_url":"u","state":"open"}]}"#.into())
            } else {
                Err("gh timed out".into())
            }
        };
        let draft = build(
            &Source::Text("a gate refused wrongly"),
            None,
            &ctx(),
            "o/r",
            Some(&gh as &GhFn),
        )
        .unwrap();
        assert_eq!(draft.dedup.len(), 1);
        assert_eq!(draft.dedup_unread.as_deref(), Some("gh timed out"));
    }
}
