//! Escalation notes: one markdown file per superuser-tier call, written by a
//! lead into the project's escalations directory and read here by the lead
//! check-in (overdue action) and `state path` (shell hooks). The note stands
//! alone: it names what is being decided, why now, every option with its
//! consequence, the recommendation, and the default taken on silence. No ids,
//! so a phone push reads complete.

use std::path::{Path, PathBuf};

use crate::finalize::{
    read_project_id, resolve_obsidian_vault, resolve_project_name, resolve_vault_root,
};

/// The four classes that reach the superuser. Everything else, the lead
/// decides and logs.
pub const CLASSES: [&str; 4] = [
    "public-surface",
    "irreversible",
    "money-security",
    "law-change",
];

const SECTION_DECIDING: &str = "What is being decided";
const SECTION_WHY_NOW: &str = "Why it matters now";
const SECTION_OPTIONS: &str = "Options";
const SECTION_RECOMMENDATION: &str = "Recommendation";
const SECTION_ON_DEADLINE: &str = "If no answer by the deadline";
const NEXT_SPLIT: &str = "What happens next:";
// Question-file sections. The ask intake reads them for its own shape, and
// the attention projection maps every one an escalation note carries
// (attention.rs item_from_note). Names match docs/architecture/attention-items.md.
const SECTION_BLOCKED_BECAUSE: &str = "Blocked because";
const SECTION_WHY_THESE: &str = "Why these options";
const SECTION_DOWNSIDE: &str = "Downside";
const SECTION_UNKNOWNS: &str = "Not thought through";
const SECTION_REVERSIBLE: &str = "Reversible";
const SECTION_COST_IF_WRONG: &str = "Cost if wrong";
const SECTION_MEANWHILE: &str = "Meanwhile";
const SECTION_WHY_USER: &str = "Why user";

#[derive(Default, Clone, PartialEq, Debug)]
pub struct EscalationOption {
    pub text: String,
    pub next: String,
}

#[derive(Default, Clone, PartialEq, Debug)]
pub struct Escalation {
    /// Set by [`scan`]; a bare [`parse`] carries no path.
    pub path: PathBuf,
    pub class: String,
    pub status: String,
    pub node: Option<String>,
    pub raised_by: String,
    pub raised_at: String,
    pub deadline: String,
    pub recommend: Option<usize>,
    pub on_silence: String,
    pub title: String,
    pub deciding: String,
    pub why_now: String,
    pub options: Vec<EscalationOption>,
    pub recommendation: String,
    pub on_deadline: String,
    // Question-file context fields; empty for an escalation note.
    pub blocked_because: String,
    pub options_rationale: String,
    pub downside: String,
    pub unknowns: String,
    pub reversible: String,
    pub cost_if_wrong: String,
    pub meanwhile: String,
    /// The user-only reason a reversible, recommended question still
    /// reaches the user (irreversible, money or credential, outside the
    /// machine, law-change, product or taste). Empty reads as absent.
    pub why_user: String,
    /// The ask span this question routes (`answers_ask:` frontmatter), the
    /// pointer that links the escalate route span to the ask it answers.
    pub answers_ask: Option<String>,
    /// `## ` headings no field reads. The page renders known sections only,
    /// so their text never reaches the user.
    pub unknown_sections: Vec<String>,
}

/// Every section heading a field reads.
pub const KNOWN_SECTIONS: [&str; 14] = [
    SECTION_DECIDING,
    SECTION_WHY_NOW,
    SECTION_OPTIONS,
    SECTION_RECOMMENDATION,
    SECTION_ON_DEADLINE,
    SECTION_BLOCKED_BECAUSE,
    SECTION_WHY_THESE,
    SECTION_DOWNSIDE,
    SECTION_UNKNOWNS,
    SECTION_REVERSIBLE,
    SECTION_COST_IF_WRONG,
    SECTION_MEANWHILE,
    SECTION_WHY_USER,
    "Answer",
];

/// The project's escalations directory: `<vault>/internal/<project>/escalations`
/// when obsidian is enabled with a vault, else the space's `escalations/`.
/// Mirrors `resolve_handoffs_dir`'s vault branch (finalize.rs); a config knob
/// would be an unrequested extra.
pub fn dir(cwd: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    vault_dir_with_home(cwd, home.as_deref(), "escalations")
}

/// The project's question pages directory (attention arm), same contract as
/// [`dir`]: `<vault>/internal/<project>/questions`, else `<space>/questions`.
pub fn questions_dir(cwd: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    vault_dir_with_home(cwd, home.as_deref(), "questions")
}

/// Where a cwd sits against the vault root.
enum VaultCwd {
    Outside,
    /// Inside `internal/<project>/`, or `internal/agents/<project>/`, so the
    /// folder names the project that owns it.
    Owned(String),
    /// Inside `internal/` but under no project folder: `internal/` itself,
    /// `internal/agents`, or an agent session dir.
    Unowned,
}

/// Classify `cwd` against the vault. Both sides canonicalize, so the
/// `internal/footnote -> fno` symlink reads as `fno`. `internal/agents` is the
/// cross-project inbox, never a project: only a child that is itself a vault
/// project folder names an owner.
fn classify_vault_cwd(vroot: &Path, cwd: &Path) -> VaultCwd {
    let vroot = vroot.canonicalize().unwrap_or_else(|_| vroot.to_path_buf());
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    let Ok(rel) = cwd.strip_prefix(&vroot) else {
        return VaultCwd::Outside;
    };
    let mut parts = rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    // Only the `internal/` tree is project-folder space. A repo cloned
    // elsewhere in the vault keeps resolving through its own id or remote.
    if parts.next().as_deref() != Some("internal") {
        return VaultCwd::Outside;
    }
    let owner = match parts.next() {
        Some(seg) if seg == "agents" => parts
            .next()
            .filter(|child| vroot.join("internal").join(child).is_dir()),
        other => other,
    };
    match owner {
        Some(o) if crate::finalize::valid_project_id(&o) => VaultCwd::Owned(o),
        _ => VaultCwd::Unowned,
    }
}

pub(crate) fn vault_dir_with_home(cwd: &Path, home: Option<&Path>, leaf: &str) -> PathBuf {
    let settings = cwd.join(".fno/config.toml");
    let mut candidates: Vec<PathBuf> = vec![settings.clone()];
    if let Some(h) = home {
        candidates.push(h.join(".fno/config.toml"));
    }
    if let Some(vault) = resolve_obsidian_vault(&candidates) {
        if let Some(vroot) = resolve_vault_root(&vault, home) {
            // A temp-dir cwd whose project name is a basename fallback is a
            // test or probe process (a leaked test daemon once wrote 203
            // `internal/fnoe<pid>_<n>/questions` dirs into the real vault).
            // Contain such writes in the space dir.
            if crate::finalize::vault_write_is_temp_stray(home, cwd) {
                return crate::paths::space_dir(cwd).join(leaf);
            }
            let declared = read_project_id(&cwd.join(".fno/config.toml")).is_some()
                || home.is_some_and(|h| read_project_id(&h.join(".fno/config.toml")).is_some());
            let project = match classify_vault_cwd(&vroot, cwd) {
                VaultCwd::Owned(owner) if !declared => owner,
                // The vault is no project, so a cwd inside it names none:
                // a vault-level `project.id` says whose pages these are.
                VaultCwd::Unowned if !declared => {
                    match read_project_id(&vroot.join(".fno/config.toml")) {
                        Some(id) => id,
                        None => return crate::paths::space_dir(cwd).join(leaf),
                    }
                }
                _ => resolve_project_name(None, home, cwd),
            };
            return vroot.join("internal").join(project).join(leaf);
        }
    }
    crate::paths::space_dir(cwd).join(leaf)
}

/// Tolerant parse: anything missing is empty/None and [`problems`] names it.
pub fn parse(text: &str) -> Escalation {
    let mut esc = Escalation::default();
    let mut lines = text.lines().peekable();
    if lines.peek().map(|l| l.trim() == "---").unwrap_or(false) {
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == "---" {
                break;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "class" => esc.class = value.to_string(),
                "status" => esc.status = value.to_string(),
                "node" => esc.node = (!value.is_empty()).then(|| value.to_string()),
                "raised_by" => esc.raised_by = value.to_string(),
                "raised_at" => esc.raised_at = value.to_string(),
                "deadline" => esc.deadline = value.to_string(),
                "recommend" => esc.recommend = value.parse::<usize>().ok(),
                "on_silence" => esc.on_silence = value.to_string(),
                "why_user" | "why-user" => esc.why_user = value.to_string(),
                "answers_ask" => esc.answers_ask = (!value.is_empty()).then(|| value.to_string()),
                _ => {}
            }
        }
    }

    let mut sections: Vec<(String, String)> = Vec::new();
    for line in lines {
        if let Some(name) = line.strip_prefix("## ") {
            sections.push((name.trim().to_string(), String::new()));
        } else if esc.title.is_empty() && line.starts_with("# ") {
            esc.title = line[2..].trim().to_string();
        } else if let Some(last) = sections.last_mut() {
            last.1.push_str(line);
            last.1.push('\n');
        }
    }
    let section = |name: &str| -> String {
        sections
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, body)| body.trim().to_string())
            .unwrap_or_default()
    };
    esc.deciding = section(SECTION_DECIDING);
    esc.why_now = section(SECTION_WHY_NOW);
    esc.options = parse_options(&section(SECTION_OPTIONS));
    esc.recommendation = section(SECTION_RECOMMENDATION);
    esc.on_deadline = section(SECTION_ON_DEADLINE);
    esc.blocked_because = section(SECTION_BLOCKED_BECAUSE);
    esc.options_rationale = section(SECTION_WHY_THESE);
    esc.downside = section(SECTION_DOWNSIDE);
    esc.unknowns = section(SECTION_UNKNOWNS);
    esc.reversible = section(SECTION_REVERSIBLE);
    esc.cost_if_wrong = section(SECTION_COST_IF_WRONG);
    esc.meanwhile = section(SECTION_MEANWHILE);
    let why_user = section(SECTION_WHY_USER);
    if !why_user.is_empty() {
        esc.why_user = why_user;
    }
    esc.unknown_sections = sections
        .iter()
        .map(|(n, _)| n.clone())
        .filter(|n| !KNOWN_SECTIONS.contains(&n.as_str()))
        .collect();
    esc
}

fn is_numbered(line: &str) -> bool {
    let mut chars = line.chars();
    chars.next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        && chars.next() == Some('.')
        && chars.next() == Some(' ')
}

/// The reasons only a user can answer, shared by the ask gate and the
/// question sweep: a question carrying one of these in why_user survives
/// its node closing, whatever the ask gate decided.
pub fn why_user_is_user_only(why: &str) -> bool {
    let why = why.to_ascii_lowercase();
    [
        "irreversible",
        "money",
        "credential",
        "outside",
        "product",
        "taste",
        "law-change",
    ]
    .iter()
    .any(|k| why.contains(k))
}

fn parse_options(body: &str) -> Vec<EscalationOption> {
    let mut out: Vec<EscalationOption> = Vec::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let new_option = is_numbered(line);
        if new_option {
            out.push(EscalationOption::default());
        }
        let Some(cur) = out.last_mut() else {
            continue;
        };
        let numbered_text = |s: &str| -> String {
            match s.find(". ") {
                Some(i) => s[i + 2..].trim().to_string(),
                None => s.to_string(),
            }
        };
        if let Some(pos) = line.find(NEXT_SPLIT) {
            let before = line[..pos].trim();
            let before = before.strip_suffix('.').unwrap_or(before);
            cur.next = line[pos + NEXT_SPLIT.len()..].trim().to_string();
            if new_option {
                cur.text = numbered_text(before);
            } else if !before.is_empty() {
                cur.text.push(' ');
                cur.text.push_str(before);
            }
        } else if new_option {
            cur.text = numbered_text(line);
        } else {
            cur.text.push(' ');
            cur.text.push_str(line);
        }
    }
    out
}

/// Every defect, so a lead fixes the note instead of debugging a silent push.
pub fn problems(e: &Escalation) -> Vec<String> {
    let mut out = Vec::new();
    if e.class.is_empty() {
        out.push("missing class".to_string());
    } else if !CLASSES.contains(&e.class.as_str()) {
        out.push(format!("unknown class: {}", e.class));
    }
    for (name, body) in [
        (SECTION_DECIDING, &e.deciding),
        (SECTION_WHY_NOW, &e.why_now),
        (SECTION_RECOMMENDATION, &e.recommendation),
        (SECTION_ON_DEADLINE, &e.on_deadline),
    ] {
        if body.trim().is_empty() {
            out.push(format!("missing section: {name}"));
        }
    }
    if e.options.is_empty() {
        out.push(format!("missing section: {SECTION_OPTIONS}"));
    } else if e.options.len() < 2 {
        out.push("fewer than two options".to_string());
    }
    for (i, option) in e.options.iter().enumerate() {
        if option.next.is_empty() {
            out.push(format!("option {} has no {}", i + 1, NEXT_SPLIT));
        }
    }
    if chrono::DateTime::parse_from_rfc3339(&e.deadline).is_err() {
        out.push(format!("deadline does not parse: {}", e.deadline));
    }
    match e.recommend {
        None => out.push("recommend outside the options".to_string()),
        Some(n) if n == 0 || n > e.options.len() => {
            out.push("recommend outside the options".to_string())
        }
        Some(_) => {}
    }
    if e.class == "irreversible" && e.on_silence != "wait" {
        out.push("an irreversible call waits (on_silence: wait)".to_string());
    }
    out
}

/// The phone push: a title and a body that read complete with no id in them.
pub fn phone_text(e: &Escalation) -> (String, String) {
    let title = format!("fno escalation ({}): {}", e.class, e.title);
    let mut body = String::new();
    let problems = problems(e);
    if !problems.is_empty() {
        body.push_str(&format!(
            "incomplete escalation: missing {}\n\n",
            problems.join(", ")
        ));
    }
    if !e.deciding.is_empty() {
        body.push_str(&e.deciding);
        body.push_str("\n\n");
    }
    if !e.options.is_empty() {
        body.push_str("Options:\n");
        for (i, option) in e.options.iter().enumerate() {
            body.push_str(&format!(
                "{}. {} What happens next: {}\n",
                i + 1,
                option.text,
                if option.next.is_empty() {
                    "(missing)".to_string()
                } else {
                    option.next.clone()
                }
            ));
        }
        body.push('\n');
    }
    if let Some(n) = e.recommend {
        body.push_str(&format!("Recommended: option {n}\n"));
    }
    let silence = match (e.on_silence.as_str(), e.recommend) {
        ("take-recommended", Some(n)) => format!("take option {n}"),
        _ => "the lead waits".to_string(),
    };
    body.push_str(&format!("No answer by {}: {}", e.deadline, silence));
    (title, body)
}

/// Open and past the deadline.
pub fn overdue(e: &Escalation, now: chrono::DateTime<chrono::Utc>) -> bool {
    if e.status != "open" {
        return false;
    }
    chrono::DateTime::parse_from_rfc3339(&e.deadline)
        .map(|d| d < now)
        .unwrap_or(false)
}

/// The inverse of [`parse`], for a Rust writer.
pub fn to_note(e: &Escalation) -> String {
    let mut fm = String::new();
    fm.push_str(&format!("class: {}\n", e.class));
    fm.push_str(&format!("status: {}\n", e.status));
    if let Some(node) = &e.node {
        fm.push_str(&format!("node: {node}\n"));
    }
    fm.push_str(&format!("raised_by: {}\n", e.raised_by));
    fm.push_str(&format!("raised_at: {}\n", e.raised_at));
    fm.push_str(&format!("deadline: {}\n", e.deadline));
    if let Some(n) = e.recommend {
        fm.push_str(&format!("recommend: {n}\n"));
    }
    fm.push_str(&format!("on_silence: {}\n", e.on_silence));
    let options = e
        .options
        .iter()
        .enumerate()
        .map(|(i, o)| format!("{}. {} What happens next: {}", i + 1, o.text, o.next))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "---\n{fm}---\n# {}\n\n## {SECTION_DECIDING}\n{}\n\n## {SECTION_WHY_NOW}\n{}\n\n## {SECTION_OPTIONS}\n{options}\n\n## {SECTION_RECOMMENDATION}\n{}\n\n## {SECTION_ON_DEADLINE}\n{}\n",
        e.title, e.deciding, e.why_now, e.recommendation, e.on_deadline
    )
}

/// Every note in the directory, path-stamped and sorted. An unreadable
/// directory is an error, never an empty list; one unreadable note logs and
/// skips.
pub fn scan(dir: &Path) -> std::io::Result<Vec<Escalation>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|e| e.to_str()) == Some("md"))
        .collect();
    entries.sort();
    let mut out = Vec::new();
    for path in entries {
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut e = parse(&text);
                e.path = path.clone();
                out.push(e);
            }
            Err(err) => eprintln!("escalation: unreadable note {}: {err}", path.display()),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claims::test_env_lock;

    const EXAMPLE: &str = r#"---
class: irreversible
status: open
node: x-aaaa
raised_by: lead-a792
raised_at: 2026-09-15T09:00:00Z
deadline: 2026-09-16T09:00:00Z
recommend: 2
on_silence: wait
---
# Rewrite release history to remove a leaked token

## What is being decided
Whether to force-push the release branch to remove a token that a commit leaked.

## Why it matters now
The token is live. Anyone with read access to the repo can use it until it is rotated or removed.

## Options
1. Force-push the branch without the commit. What happens next: every open PR on the branch must rebase, and forks keep the old commit.
2. Rotate the token and leave history alone. What happens next: the old token stops working in minutes, and nothing is rewritten.

## Recommendation
Option 2. It closes the risk and cannot break anyone's checkout.

## If no answer by the deadline
The lead waits. A force push cannot be undone.
"#;

    #[test]
    fn example_note_parses_clean_and_phones_complete() {
        let e = parse(EXAMPLE);
        assert_eq!(e.class, "irreversible");
        assert_eq!(e.title, "Rewrite release history to remove a leaked token");
        assert_eq!(e.node.as_deref(), Some("x-aaaa"));
        assert_eq!(e.recommend, Some(2));
        assert!(problems(&e).is_empty(), "{:?}", problems(&e));

        let (title, body) = phone_text(&e);
        assert_eq!(
            title,
            "fno escalation (irreversible): Rewrite release history to remove a leaked token"
        );
        assert!(body.contains("Whether to force-push"), "{body}");
        assert!(
            body.contains(
                "1. Force-push the branch without the commit What happens next: every open PR"
            ),
            "{body}"
        );
        assert!(body.contains("2. Rotate the token"), "{body}");
        assert!(body.contains("Recommended: option 2"), "{body}");
        assert!(
            body.contains("No answer by 2026-09-16T09:00:00Z: the lead waits"),
            "{body}"
        );
        assert!(!body.contains("x-aaaa"), "no id in the body: {body}");
    }

    #[test]
    fn problems_names_each_defect() {
        let bad = parse(
            "---\nclass: fleet-config\nstatus: open\nraised_by: k\nraised_at: 2026-09-15T09:00:00Z\ndeadline: 2026-09-16T09:00:00Z\nrecommend: 1\non_silence: wait\n---\n# t\n\n## What is being decided\nd\n\n## Why it matters now\nw\n\n## Options\n1. Only one. What happens next: n.\n\n## Recommendation\nr\n\n## If no answer by the deadline\nx\n",
        );
        let found = problems(&bad);
        assert!(
            found
                .iter()
                .any(|p| p.contains("unknown class: fleet-config")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|p| p.contains("fewer than two options")),
            "{found:?}"
        );

        let hasty = parse(
            "---\nclass: irreversible\nstatus: open\nraised_by: k\nraised_at: 2026-09-15T09:00:00Z\ndeadline: 2026-09-16T09:00:00Z\nrecommend: 1\non_silence: take-recommended\n---\n# t\n\n## What is being decided\nd\n\n## Why it matters now\nw\n\n## Options\n1. A. What happens next: n.\n2. B. What happens next: m.\n\n## Recommendation\nr\n\n## If no answer by the deadline\nx\n",
        );
        let found = problems(&hasty);
        assert!(
            found.iter().any(|p| p.contains("irreversible call waits")),
            "{found:?}"
        );
    }

    #[test]
    fn a_section_no_field_reads_is_named() {
        let esc = parse("Pick one?\n\n## Options\n1. A.\n## The table\n| a |\n");
        assert_eq!(esc.unknown_sections, ["The table"]);
    }

    #[test]
    fn a_law_change_reaches_the_user_like_the_other_user_classes() {
        assert!(why_user_is_user_only("law-change: it retires two laws"));
        assert!(why_user_is_user_only("a taste call"));
        assert!(!why_user_is_user_only("the user asked to see it"));
    }

    #[test]
    fn overdue_needs_open_and_a_past_deadline() {
        let mut e = parse(EXAMPLE);
        let now = chrono::DateTime::parse_from_rfc3339("2026-09-17T00:00:00Z").unwrap();
        assert!(overdue(&e, now.into()));
        e.status = "answered".to_string();
        assert!(!overdue(&e, now.into()));
        e.status = "open".to_string();
        let before = chrono::DateTime::parse_from_rfc3339("2026-09-15T00:00:00Z").unwrap();
        assert!(!overdue(&e, before.into()));
        e.deadline = "not a time".to_string();
        assert!(!overdue(&e, now.into()));
    }

    #[test]
    fn vault_branch_refuses_temp_cwd_basename_fallback() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("fno-escalation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        // A vault only the HOME-side config knows: the throwaway cwd has none.
        std::fs::create_dir_all(base.join(".fno")).unwrap();
        std::fs::write(
            base.join(".fno/config.toml"),
            "[obsidian]\nenabled = true\nvault = \"c3po\"\n",
        )
        .unwrap();
        // A throwaway cwd under a temp root, no project id, no git remote:
        // without the refusal the vault branch names it by its basename into
        // the REAL vault (the stray-dir leak this test pins).
        let cwd = std::env::temp_dir().join(format!("fnoe{}_", std::process::id()));
        std::fs::create_dir_all(&cwd).unwrap();
        let refused = vault_dir_with_home(&cwd, Some(&base), "questions");
        assert_eq!(
            refused,
            crate::paths::space_dir(&cwd).join("questions"),
            "a temp cwd with a fallback name writes the space dir, never the vault"
        );
        // A declared project id still takes the vault branch, unchanged.
        std::fs::create_dir_all(cwd.join(".fno")).unwrap();
        std::fs::write(cwd.join(".fno/config.toml"), "[project]\nid = \"fno\"\n").unwrap();
        let with_id = vault_dir_with_home(&cwd, Some(&base), "questions");
        assert_eq!(
            with_id,
            base.join("c3po/internal/fno/questions"),
            "a declared project id keeps the vault branch"
        );
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&cwd);
    }

    #[test]
    fn vault_cwd_resolves_to_owning_project_not_its_own_name() {
        let _lock = test_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let base = std::env::temp_dir().join(format!("fno-escalation-vcwd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let vault = base.join("c3po");
        for d in [
            "internal/fno",
            "internal/agents/0a1b2c3d",
            "internal/agents/fno",
        ] {
            std::fs::create_dir_all(vault.join(d)).unwrap();
        }
        std::os::unix::fs::symlink(vault.join("internal/fno"), vault.join("internal/footnote"))
            .unwrap();
        std::fs::create_dir_all(base.join(".fno")).unwrap();
        std::fs::write(
            base.join(".fno/config.toml"),
            "[obsidian]\nenabled = true\nvault = \"c3po\"\n",
        )
        .unwrap();
        // The vault is a git repo with a remote, as the real one is: without
        // the vault branch the remote slug names it `c3po`.
        for args in [
            &["init", "-q"][..],
            &["remote", "add", "origin", "git@github.com:x/c3po.git"][..],
        ] {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&vault)
                .args(args)
                .status()
                .unwrap();
            assert!(st.success());
        }
        let questions = |cwd: &Path| vault_dir_with_home(cwd, Some(&base), "questions");
        let fno_dir = vault.join("internal/fno/questions");
        assert_eq!(questions(&vault.join("internal/fno")), fno_dir);
        assert_eq!(questions(&vault.join("internal/footnote")), fno_dir);
        assert_eq!(questions(&vault.join("internal/agents/fno")), fno_dir);
        // Agent session dirs and `internal/agents` itself own no project:
        // with no vault-level id the write leaves the vault.
        for cwd in [
            vault.join("internal/agents"),
            vault.join("internal/agents/0a1b2c3d"),
        ] {
            let got = questions(&cwd);
            assert!(
                !got.starts_with(vault.join("internal")),
                "{} must not write the vault: {}",
                cwd.display(),
                got.display()
            );
        }
        // A vault-level project id names the owner of those cwds.
        std::fs::create_dir_all(vault.join(".fno")).unwrap();
        std::fs::write(vault.join(".fno/config.toml"), "[project]\nid = \"fno\"\n").unwrap();
        assert_eq!(questions(&vault.join("internal/agents")), fno_dir);
        assert_eq!(questions(&vault.join("internal/agents/0a1b2c3d")), fno_dir);
        let _ = std::fs::remove_dir_all(&base);
    }
}
