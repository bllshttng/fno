//! The two `gh` text gates: the GraphQL reserve (direct PR reads route
//! through `fno do pr`) and the `gh pr create` closure line.

use super::segments::{self, base_lower, effective_argv, is_digits, Segment};
use super::Ctx;
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// A wrong `--body-file` path must never make the hook read something large.
const BODY_FILE_CAP: u64 = 1 << 20;

/// Drop the repo and host options gh takes before its command words.
pub(super) fn strip_gh_global_opts(argv: &[String]) -> Vec<String> {
    fn strip(mut tokens: &[String]) -> &[String] {
        while let Some(token) = tokens.first() {
            if matches!(token.as_str(), "-R" | "--repo" | "--hostname") {
                if tokens.len() < 2 {
                    return &[];
                }
                tokens = &tokens[2..];
            } else if (token.starts_with("-R") && token != "-R")
                || token.starts_with("--repo=")
                || token.starts_with("--hostname=")
            {
                tokens = &tokens[1..];
            } else {
                break;
            }
        }
        tokens
    }
    let mut out: Vec<String> = argv.iter().take(1).cloned().collect();
    let command = strip(&argv[argv.len().min(1)..]);
    if command.first().map(String::as_str) == Some("pr") {
        out.push("pr".to_string());
        out.extend(strip(&command[1..]).iter().cloned());
    } else {
        out.extend(command.iter().cloned());
    }
    out
}

/// Every direct GraphQL PR read at command position, as `(kind, pr)`.
pub(super) fn graphql_reads(segs: &[Segment]) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    for seg in segs {
        let argv = effective_argv(seg);
        let Some(first) = argv.first() else { continue };
        if !matches!(
            base_lower(first).as_str(),
            "gh" | "$fno_real_gh" | "${fno_real_gh}"
        ) {
            continue;
        }
        let argv = strip_gh_global_opts(&argv);
        let low: Vec<String> = argv.iter().map(|t| t.to_lowercase()).collect();
        let pr = || {
            argv.iter()
                .skip(3)
                .find(|v| is_digits(v))
                .cloned()
                .unwrap_or_else(|| "<n>".to_string())
        };
        if argv.len() < 3 {
            continue;
        }
        match (low[1].as_str(), low[2].as_str()) {
            ("pr", "list") => out.push(("list", "<n>".to_string())),
            ("pr", "checks" | "status") => out.push(("status", pr())),
            ("pr", "view") => out.push(("info", pr())),
            ("api", _) if low[2..].iter().any(|t| t == "graphql") => {
                out.push(("graphql", "<n>".to_string()))
            }
            _ => {}
        }
    }
    out
}

/// The deny-leaning read for a command shlex cannot split.
pub(super) fn graphql_reads_fallback(command: &str) -> Vec<(&'static str, String)> {
    static R: OnceLock<[Regex; 4]> = OnceLock::new();
    let res = R.get_or_init(|| {
        let prefix = r"(?i)\bgh\s+(?:(?:-R|--repo|--hostname)(?:=|\s+)?\S+\s+)*pr\s+";
        [
            Regex::new(&format!(r"{prefix}(?:checks|status)\b")).expect("static regex"),
            Regex::new(&format!(r"{prefix}list\b")).expect("static regex"),
            Regex::new(&format!(r"{prefix}view\b")).expect("static regex"),
            Regex::new(r"(?i)\bgh\s+api\s+graphql\b").expect("static regex"),
        ]
    });
    ["status", "list", "info", "graphql"]
        .into_iter()
        .zip(res.iter())
        .filter(|(_, re)| re.is_match(command))
        .map(|(kind, _)| (kind, "<n>".to_string()))
        .collect()
}

pub(super) fn graphql_reason(kind: &str, pr: &str) -> String {
    match kind {
        "info" => format!(
            "[fno GraphQL reserve] use `fno do pr info {pr}` for state/head/mergeability. \
             This refusal is unconditional: the read is ROUTED, never rationed."
        ),
        "list" => "[fno GraphQL reserve] use `fno do pr list` for a REST-backed listing. \
                   This refusal is unconditional: the read is ROUTED, never rationed."
            .to_string(),
        "status" => format!(
            "[fno GraphQL reserve] use `fno do pr status {pr}` for CI. Its CI read is REST; \
             optional review-thread and coverage reads inside it remain GraphQL."
        ),
        _ => "[fno GraphQL reserve] direct `gh api graphql` is discretionary; route it \
              through `fno do pr graphql-exec --purpose discretionary -- api graphql \
              -f query=...`. Everything after -- is gh's argv, command words first, \
              so repeat the refused command's flags after the endpoint tokens. This \
              refusal is unconditional, so waiting for a quota reset changes nothing."
            .to_string(),
    }
}

fn closure_marker() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"(?i)CLOSURE_TRAILER|Backlog-Closure|Fixes\s+[a-z][a-z0-9]{0,7}-?[0-9a-f]{4,8}")
            .expect("static regex")
    })
}

fn find_pr_create_segments(segs: &[Segment]) -> Vec<&Segment> {
    segs.iter()
        .filter(|seg| {
            let argv = effective_argv(seg);
            if argv.first().map(|t| base_lower(t)) != Some("gh".to_string()) {
                return false;
            }
            let argv = strip_gh_global_opts(&argv);
            argv.len() >= 3
                && argv[1].eq_ignore_ascii_case("pr")
                && argv[2].eq_ignore_ascii_case("create")
        })
        .collect()
}

/// `(hatch, head, body_files)` for one create segment, each by argv
/// position: a `--body` quoting `--head x` or `FNO_PR_CLOSURE_OK=1` in
/// prose is a word, never the flag.
fn pr_create_signals(seg: &Segment) -> (bool, Option<String>, Vec<String>) {
    let (argv, assignments) = segments::effective_argv_and_assignments(seg);
    let hatch = assignments.iter().any(|a| a == "FNO_PR_CLOSURE_OK=1");
    let argv = strip_gh_global_opts(&argv);
    let mut head = None;
    let mut body_files = Vec::new();
    let mut i = 0;
    while i < argv.len() {
        let tok = argv[i].as_str();
        if matches!(tok, "--head" | "-H") && i + 1 < argv.len() {
            head = Some(argv[i + 1].clone());
            i += 2;
            continue;
        }
        if let Some(v) = tok.strip_prefix("--head=") {
            head = Some(v.to_string());
        } else if matches!(tok, "--body-file" | "-F") && i + 1 < argv.len() {
            body_files.push(argv[i + 1].clone());
            i += 2;
            continue;
        } else if let Some(v) = tok.strip_prefix("--body-file=") {
            body_files.push(v.to_string());
        }
        i += 1;
    }
    (hatch, head, body_files)
}

/// `os.path.normpath`, lexical: collapse `//`, `.` and `..`.
fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|p| *p != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// True (allow) unless the file exists, reads under the cap and carries no
/// closure marker. A path another segment of the same command names allows:
/// the hook runs before that write-then-create flow writes it.
fn body_file_allows(path: &str, cwd: &std::path::Path, touched: &HashSet<String>) -> bool {
    use std::io::Read;
    let full = cwd.join(path);
    let mut bytes = Vec::new();
    match std::fs::File::open(&full) {
        Ok(file) => {
            if file.take(BODY_FILE_CAP).read_to_end(&mut bytes).is_err() {
                return true;
            }
        }
        Err(_) => return true,
    }
    let meta = std::fs::metadata(&full);
    if touched.contains(&normpath(path).to_lowercase())
        || !meta.as_ref().is_ok_and(|m| m.is_file())
        || meta.is_ok_and(|m| m.len() > BODY_FILE_CAP)
    {
        return true;
    }
    let text = String::from_utf8_lossy(&bytes).replace('\0', "");
    closure_marker().is_match(&text)
}

/// The refusal for a `gh pr create` whose branch names a node and whose
/// command shows no composed closure line. It asserts a positive marker
/// that the composition step ran, never an absence: a false allow is CI's
/// to catch, a false deny is the defect.
pub(super) fn closure_refusal(
    command: &str,
    segs: Option<&[Segment]>,
    ctx: &Ctx,
) -> Option<String> {
    static FALLBACK: OnceLock<Regex> = OnceLock::new();
    let creates: Vec<(bool, Option<String>, Vec<String>)> = match segs {
        Some(segs) => find_pr_create_segments(segs)
            .into_iter()
            .map(pr_create_signals)
            .collect(),
        None => {
            let re =
                FALLBACK.get_or_init(|| Regex::new(r"(?i)gh\s+pr\s+create").expect("static regex"));
            re.find_iter(command)
                .map(|_| (false, None, Vec::new()))
                .collect()
        }
    };
    if creates.is_empty() {
        return None;
    }
    let create_segs: Vec<&Segment> = segs.map(find_pr_create_segments).unwrap_or_default();
    let touched: HashSet<String> = segs
        .unwrap_or_default()
        .iter()
        .filter(|seg| !create_segs.contains(seg))
        .flatten()
        .map(|t| normpath(t).to_lowercase())
        .collect();
    for (hatch, head, body_files) in creates {
        if let Some(reason) = one_closure_refusal(command, hatch, head, &body_files, &touched, ctx)
        {
            return Some(reason);
        }
    }
    None
}

fn one_closure_refusal(
    command: &str,
    hatch: bool,
    head: Option<String>,
    body_files: &[String],
    touched: &HashSet<String>,
    ctx: &Ctx,
) -> Option<String> {
    // The hook is a separate process: an inline `FNO_PR_CLOSURE_OK=1 gh pr
    // create` never reaches this env, so the assignment prefix is read too.
    if hatch || std::env::var("FNO_PR_CLOSURE_OK").as_deref() == Ok("1") {
        return None;
    }
    let head_ref = head
        .or_else(|| ctx.probes.current_branch())
        .unwrap_or_default();
    let ids = ctx.probes.branch_node_ids(&head_ref);
    if ids.is_empty() || closure_marker().is_match(command) {
        return None;
    }
    let judged: Vec<&String> = body_files
        .iter()
        .filter(|p| !body_file_allows(p, &ctx.cwd, touched))
        .collect();
    if !body_files.is_empty() && judged.is_empty() {
        return None;
    }
    let detail = judged
        .first()
        .map(|p| {
            format!(
                "the body file {p} exists and carries no Fixes line; or open the whole-path door /fno:ship pr create.\n"
            )
        })
        .unwrap_or_default();
    // Names candidates, never a trailer to paste: a branch segment can fit
    // the id grammar without being a node, and the generator reads the graph.
    Some(format!(
        "[fno closure trailer] branch segments that fit the node-id grammar: {}. \
         check-pr-node-closure reds this PR unless the body claims at least one REAL node.\n\
         {detail}Generate the ONE line (graph-checked, with contained_in descendants) via \
         `fno do pr closure-trailer <node-id> --extra <id> [...]` and paste its output; the \
         gate reads only the last closure line, so replace every line in the body with this \
         one.\nDo NOT paste a candidate from this message: a segment can match the grammar \
         without being a node, and one unknown id voids the whole binding at merge.\n\
         Ad-hoc PR that closes nothing: re-run with FNO_PR_CLOSURE_OK=1.",
        ids.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::super::fake::{bash, Fake};
    use super::super::{judge_with, Ctx};
    use super::*;

    fn ctx<'a>(dir: &std::path::Path, fake: &'a Fake) -> Ctx<'a> {
        Ctx {
            home: dir.join("home"),
            cwd: dir.to_path_buf(),
            probes: fake,
        }
    }

    #[test]
    fn graphql_reads_are_routed_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake::default();
        let c = ctx(dir.path(), &fake);
        let deny = |cmd: &str| judge_with(&bash(cmd), &c).unwrap_or_default();
        assert!(
            deny("gh pr checks 12").starts_with("[fno GraphQL reserve] use `fno do pr status 12`")
        );
        assert!(deny("gh -R o/r pr view 7 --json state").contains("`fno do pr info 7`"));
        assert!(deny("cd x && gh pr list").contains("`fno do pr list`"));
        assert!(deny("gh api graphql -f query=x").contains("graphql-exec"));
        assert!(deny("echo 'unbalanced && gh pr status").contains("`fno do pr status <n>`"));
        assert_eq!(
            judge_with(&bash("fno backlog idea \"gh pr view 3\""), &c),
            None
        );
        assert_eq!(judge_with(&bash("gh api repos/o/r/pulls/3"), &c), None);
    }

    #[test]
    fn pr_create_needs_a_closure_marker_on_a_node_branch() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake {
            branch: Some("feature/x-c166-port".into()),
            node_ids: vec!["x-c166".into()],
            ..Fake::default()
        };
        let c = ctx(dir.path(), &fake);
        let verdict = |cmd: &str| judge_with(&bash(cmd), &c);
        let refusal = verdict("gh pr create --title t --body \"$BODY\"").expect("refused");
        assert!(
            refusal.contains("x-c166") && refusal.contains("FNO_PR_CLOSURE_OK=1"),
            "{refusal}"
        );
        assert_eq!(verdict("FNO_PR_CLOSURE_OK=1 gh pr create -t t"), None);
        assert_eq!(verdict("gh pr create -t t --body \"Fixes x-c166\""), None);
        assert!(verdict("gh pr create -t t --body \"FNO_PR_CLOSURE_OK=1\"").is_some());
        // A body file is judged on its own trailer; a missing one allows.
        std::fs::write(dir.path().join("body.md"), "no trailer").unwrap();
        let denied = verdict("gh pr create -t t --body-file body.md").expect("refused");
        assert!(denied.contains("the body file body.md exists"), "{denied}");
        std::fs::write(dir.path().join("ok.md"), "Fixes x-c166\n").unwrap();
        assert_eq!(verdict("gh pr create -t t --body-file ok.md"), None);
        assert_eq!(verdict("gh pr create -t t -F missing.md"), None);
        // A file another segment writes first is not judged stale.
        assert_eq!(
            verdict("printf x > body.md && gh pr create -t t -F body.md"),
            None
        );
    }

    #[test]
    fn pr_create_on_a_branch_naming_no_node_is_not_gated() {
        let dir = tempfile::tempdir().unwrap();
        let fake = Fake {
            branch: Some("chore/docs".into()),
            ..Fake::default()
        };
        assert_eq!(
            judge_with(&bash("gh pr create -t t"), &ctx(dir.path(), &fake)),
            None
        );
        assert_eq!(normpath("./a//b/../c"), "a/c");
    }
}
