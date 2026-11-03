//! Fills the golden drafts and opens one PR a day for a person to review.
//!
//! `evals_drafts` writes a draft per typed correction with every answer field
//! empty. This pass hands the oldest open drafts to a bounded one-shot model
//! call. The daemon, not the model, decides what ships: a reply is kept only
//! when its YAML loads as a golden bank task and its own grade passes each
//! good fixture case and fails each bad one. Kept drafts go into one PR on
//! `evals/golden-drafts-<date>`. A person still reviews and merges it.
//! `.drafted` in the drafts folder is the pass's memory: one line per handled
//! hash, so a `no_case` or unproved draft is never retried.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::evals::bank;

const DEFAULT_MAX_PER_DAY: usize = 5;
const SPAWN_TIMEOUT_S: u64 = 300;
const MODEL: &str = "sonnet";

type Spawn<'a> = &'a dyn Fn(&str, &str, &Path, u64, &str) -> Result<(i32, String, String), String>;
type Open<'a> = &'a dyn Fn(&str, &str, &str, &[(PathBuf, Vec<u8>)]) -> Result<String, String>;

#[derive(Deserialize)]
struct Reply {
    verdict: String,
    #[serde(default)]
    bank: String,
    #[serde(default)]
    fixture: String,
    #[serde(default)]
    reason: String,
}

fn prompt(draft: &str) -> String {
    format!(
        "You turn one user correction into a golden eval task for this repository. Read docs/evals.md section Goldens first.\n\
Rules: the rule must be findable in the repo; the grade must be mechanical (file-exists or exit commands on files the worker writes); the prompt asks for recall of the exact action, never a choice among listed options; no `gh`.\n\
Draft:\n{draft}\n\
Reply with one JSON object and nothing else.\n\
A usable case: {{\"verdict\":\"case\",\"bank\":\"<full bank task YAML: id golden-<slug>, tier capability, prompt, grade, timeout_minutes, tags [golden], golden block with source, quote, expected, rule, failure_type, cost, outcome>\",\"fixture\":\"<metrics fixture YAML: cases list with at least 3 label good and 3 label bad, each {{id, label, files}}>\"}}\n\
No mechanical case: {{\"verdict\":\"no_case\",\"reason\":\"<one sentence>\"}}\n\
Do not invent a rule you cannot cite from a repo file."
    )
}

/// The first JSON object in a model reply.
fn parse_reply(out: &str) -> Result<Reply, String> {
    let (a, b) = (out.find('{'), out.rfind('}'));
    let (Some(a), Some(b)) = (a, b) else {
        return Err("no JSON object in the reply".into());
    };
    serde_json::from_str(&out[a..=b]).map_err(|e| format!("reply is not the JSON asked for: {e}"))
}

/// Stage the reply beside a scratch bank and prove it. Returns the task id.
fn validate(repo: &Path, reply: &Reply) -> Result<String, String> {
    let stage = tempfile::tempdir().map_err(|e| e.to_string())?;
    let bank_dir = stage.path().join("evals").join("bank");
    let metrics = stage.path().join("evals").join("fixtures").join("metrics");
    for dir in [&bank_dir, &metrics] {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let probe = bank_dir.join("draft.yaml");
    std::fs::write(&probe, &reply.bank).map_err(|e| e.to_string())?;
    let task = bank::load_task(&probe).map_err(|e| format!("bank YAML refused: {}", e.0))?;
    let Some(golden) = task.golden.as_ref() else {
        return Err("no golden block".into());
    };
    let slug_ok = task
        .id
        .strip_prefix("golden-")
        .is_some_and(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    if !slug_ok || task.tier != "capability" {
        return Err(format!(
            "id {:?} or tier is not a capability golden",
            task.id
        ));
    }
    if !task.tags.iter().any(|t| t == "golden") || task.tags.iter().any(|t| t == "gate") {
        return Err("tags must hold golden and never gate".into());
    }
    if repo
        .join("evals")
        .join("bank")
        .join(format!("{}.yaml", task.id))
        .exists()
    {
        return Err(format!("{} already exists in the bank", task.id));
    }
    let catalog = crate::evals::failure_types::Catalog::load(&repo.join("evals").join("bank"))?;
    if !catalog.types.iter().any(|t| t.id == golden.failure_type) {
        return Err(format!(
            "failure_type {:?} is not in the catalog",
            golden.failure_type
        ));
    }
    std::fs::write(bank_dir.join(format!("{}.yaml", task.id)), &reply.bank)
        .map_err(|e| e.to_string())?;
    std::fs::remove_file(&probe).map_err(|e| e.to_string())?;
    std::fs::write(metrics.join(format!("{}.yaml", task.id)), &reply.fixture)
        .map_err(|e| e.to_string())?;
    let failures = crate::evals::failure_types::golden_failures(&task, &bank_dir);
    if failures.is_empty() {
        Ok(task.id)
    } else {
        Err(format!("grade unproved: {}", failures.join("; ")))
    }
}

fn open_drafts(dir: &Path) -> Vec<(String, PathBuf)> {
    let handled = std::fs::read_to_string(dir.join(".drafted")).unwrap_or_default();
    let done: Vec<&str> = handled
        .lines()
        .filter_map(|l| l.split('\t').next())
        .collect();
    let mut found: Vec<(std::time::SystemTime, String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "yaml"))
        .filter_map(|p| {
            let hash = p.file_stem()?.to_string_lossy().into_owned();
            let at = std::fs::metadata(&p).and_then(|m| m.modified()).ok()?;
            (!done.contains(&hash.as_str())).then_some((at, hash, p))
        })
        .collect();
    // Oldest first: the 30-day expiry takes the oldest drafts.
    found.sort();
    found.into_iter().map(|(_, h, p)| (h, p)).collect()
}

fn record(dir: &Path, hash: &str, note: &str, draft: &Path) {
    use std::io::Write as _;
    let line = format!("{hash}\t{}\n", note.replace(['\n', '\t'], " "));
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(".drafted"))
        .and_then(|mut f| f.write_all(line.as_bytes()));
    let _ = std::fs::remove_file(draft);
}

pub(crate) fn max_per_day(repo: &Path) -> usize {
    match crate::agents_config::config_value_deep(repo, &["evals", "drafter", "max_per_day"]) {
        Some(toml::Value::Integer(n)) if n >= 0 => n as usize,
        _ => DEFAULT_MAX_PER_DAY,
    }
}

/// One daily pass. Returns the counts for the log.
pub(crate) fn fill(
    dir: &Path,
    repo: &Path,
    day: DateTime<Utc>,
    max: usize,
    spawn: Spawn,
    open: Open,
) -> Result<Value, String> {
    let (mut kept, mut dropped) = (Vec::new(), 0u64);
    let mut edits: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    for (hash, path) in open_drafts(dir).into_iter().take(max) {
        let draft = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let reply = spawn(
            &format!("golden-drafter-{hash}"),
            &prompt(&draft),
            repo,
            SPAWN_TIMEOUT_S,
            MODEL,
        )
        .and_then(|(code, out, err)| {
            if code == 0 {
                parse_reply(&out)
            } else {
                Err(format!("spawn exit {code}: {}", err.trim()))
            }
        });
        let reply = match reply {
            Ok(r) => r,
            // A failed call is not a verdict on the case: leave it for tomorrow.
            Err(e) => {
                eprintln!("golden drafter: {hash}: {e}");
                continue;
            }
        };
        if reply.verdict != "case" {
            record(dir, &hash, &format!("no_case: {}", reply.reason), &path);
            dropped += 1;
            continue;
        }
        match validate(repo, &reply) {
            Ok(id) => {
                edits.push((
                    PathBuf::from(format!("evals/bank/{id}.yaml")),
                    reply.bank.into_bytes(),
                ));
                edits.push((
                    PathBuf::from(format!("evals/fixtures/metrics/{id}.yaml")),
                    reply.fixture.into_bytes(),
                ));
                kept.push((hash, path, id));
            }
            Err(why) => {
                let note = if why.starts_with("grade unproved") {
                    format!("grade_unproved: {why}")
                } else {
                    format!("refused: {why}")
                };
                record(dir, &hash, &note, &path);
                dropped += 1;
            }
        }
    }
    if kept.is_empty() {
        return Ok(json!({"kept": 0, "dropped": dropped}));
    }
    let branch = format!("evals/golden-drafts-{}", day.format("%Y-%m-%d"));
    let title = format!(
        "Add {} golden eval task(s) drafted from corrections",
        kept.len()
    );
    let list: String = kept
        .iter()
        .map(|(_, _, id)| format!("- `{id}`\n"))
        .collect();
    let body = format!("The daemon drafted these from typed user corrections. Each loaded as a golden, passed its 3 good fixture cases and failed its 3 bad ones. Read the prompt, the rule and the grade before you merge.\n\n{list}");
    let url = open(&branch, &title, &body, &edits)?;
    for (hash, path, _) in &kept {
        record(dir, hash, &format!("pr {url}"), path);
    }
    Ok(json!({"kept": kept.len(), "dropped": dropped, "pr": url}))
}

/// The bank checkout the PR is cut from, or None when no registered project
/// carries one.
fn bank_repo() -> Option<PathBuf> {
    let has_bank = |p: &PathBuf| p.join("evals").join("bank").is_dir();
    crate::provider::plugin_root()
        .filter(|r| r.join(".git").exists())
        .into_iter()
        .chain(crate::backlog::settings::project_roots())
        .find(has_bank)
}

/// One claude one-shot with read tools, rooted at the checkout.
fn spawn_reader(
    name: &str,
    prompt: &str,
    cwd: &Path,
    timeout: u64,
    model: &str,
) -> Result<(i32, String, String), String> {
    let argv: Vec<String> = [
        "agents",
        "spawn",
        "--name",
        name,
        prompt,
        "--harness",
        "claude",
        "--once",
        "--cwd",
        &cwd.display().to_string(),
        "--timeout",
        &timeout.to_string(),
        "--",
        "--model",
        model,
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--tools",
        "Read,Grep,Glob",
        "--allowedTools",
        "Read,Grep,Glob",
        "--disable-slash-commands",
        "--no-chrome",
        "--no-session-persistence",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    crate::blueprint_judge::headless_spawn(&argv, timeout)
}

/// The daemon's daily entry, called after the drafts pass.
pub(crate) fn daily(dir: &Path) {
    let Some(repo) = bank_repo() else { return };
    // `open` returns an open PR on the day's branch without pushing to it, so
    // a second batch that day would be recorded as shipped and lost.
    let branch = format!("evals/golden-drafts-{}", Utc::now().format("%Y-%m-%d"));
    if matches!(crate::bank_pr::existing_pr(&repo, &branch), Ok(Some(_))) {
        return;
    }
    let max = max_per_day(&repo);
    let open = |branch: &str, title: &str, body: &str, edits: &[(PathBuf, Vec<u8>)]| {
        crate::bank_pr::open(&repo, branch, title, body, edits)
    };
    match fill(dir, &repo, Utc::now(), max, &spawn_reader, &open) {
        Ok(v) => eprintln!("golden drafter: {v}"),
        Err(e) => eprintln!("golden drafter: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const BANK: &str = "id: golden-demo\ntier: capability\nprompt: p\ngrade:\n  - kind: file-exists\n    path: reply.md\n  - kind: exit\n    command: \"grep -q yes reply.md\"\ntimeout_minutes: 5\ntags: [golden]\ngolden:\n  source: {kind: correction, ref: \"c:1\"}\n  quote: q\n  expected: e\n  rule: r\n  failure_type: evasive-answer\n  cost: trust\n  outcome: failed\n";

    fn fixture(bad_passes: bool) -> String {
        let bad = if bad_passes { "yes" } else { "nope" };
        format!("cases:\n  - {{id: g1, label: good, files: {{\"reply.md\": \"yes\\n\"}}}}\n  - {{id: g2, label: good, files: {{\"reply.md\": \"yes 2\\n\"}}}}\n  - {{id: g3, label: good, files: {{\"reply.md\": \"yes 3\\n\"}}}}\n  - {{id: b1, label: bad, files: {{\"reply.md\": \"{bad}\\n\"}}}}\n  - {{id: b2, label: bad, files: {{\"reply.md\": \"no\\n\"}}}}\n  - {{id: b3, label: bad, files: {{}}}}\n")
    }

    fn repo_with_catalog() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let bank_dir = repo.path().join("evals").join("bank");
        std::fs::create_dir_all(&bank_dir).unwrap();
        std::fs::write(
            bank_dir.join("failure-types.yaml"),
            "cost_order: [trust]\ntypes:\n  - {id: evasive-answer, cost: trust, description: d}\n",
        )
        .unwrap();
        repo
    }

    #[test]
    fn no_case_and_unproved_replies_are_recorded_and_the_rest_ship_in_one_pr() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        for h in ["aaa", "bbb", "ccc"] {
            std::fs::write(dir.path().join(format!("{h}.yaml")), "id: x\n").unwrap();
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        let calls = RefCell::new(0);
        let spawn = |name: &str, _: &str, _: &Path, _: u64, _: &str| {
            *calls.borrow_mut() += 1;
            let reply = match name {
                "golden-drafter-aaa" => {
                    json!({"verdict": "case", "bank": BANK, "fixture": fixture(false)})
                }
                "golden-drafter-bbb" => json!({"verdict": "no_case", "reason": "no repo rule"}),
                _ => {
                    json!({"verdict": "case", "bank": BANK.replace("golden-demo", "golden-demo2"), "fixture": fixture(true)})
                }
            };
            Ok((0, format!("sure: {reply}"), String::new()))
        };
        let seen: RefCell<Vec<PathBuf>> = RefCell::new(Vec::new());
        let open = |_: &str, _: &str, _: &str, edits: &[(PathBuf, Vec<u8>)]| {
            seen.borrow_mut()
                .extend(edits.iter().map(|(p, _)| p.clone()));
            Ok("https://example/pr/1".to_string())
        };
        let out = fill(dir.path(), repo.path(), Utc::now(), 5, &spawn, &open).unwrap();
        assert_eq!(out["kept"], 1, "{out}");
        let files: Vec<String> = seen
            .borrow()
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        assert_eq!(
            files,
            [
                "evals/bank/golden-demo.yaml",
                "evals/fixtures/metrics/golden-demo.yaml"
            ]
        );
        let handled = std::fs::read_to_string(dir.path().join(".drafted")).unwrap();
        assert_eq!(handled.lines().count(), 3, "{handled}");
        assert!(handled.contains("ccc\tgrade_unproved"), "{handled}");
        assert!(handled.contains("bbb\tno_case"), "{handled}");
        assert!(open_drafts(dir.path()).is_empty());
    }

    #[test]
    fn a_failed_spawn_leaves_the_draft_for_tomorrow() {
        let repo = repo_with_catalog();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("aaa.yaml"), "id: x\n").unwrap();
        let spawn = |_: &str, _: &str, _: &Path, _: u64, _: &str| Err("lane down".to_string());
        let open =
            |_: &str, _: &str, _: &str, _: &[(PathBuf, Vec<u8>)]| -> Result<String, String> {
                panic!("no PR without a kept draft")
            };
        let out = fill(dir.path(), repo.path(), Utc::now(), 5, &spawn, &open).unwrap();
        assert_eq!(out["kept"], 0);
        assert_eq!(open_drafts(dir.path()).len(), 1);
    }
}
