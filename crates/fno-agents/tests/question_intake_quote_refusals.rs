//! Refusal quote-lines and the decide-door-first order on the ask gate.
use fno_agents::paths::AgentsHome;
use fno_agents::question_intake::{run_intake, IntakeRequest};
use std::path::{Path, PathBuf};

fn tmp_home(tag: &str) -> AgentsHome {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-quote-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let home = AgentsHome::at(&p.join(".fno").join("agents"));
    home.ensure_root().unwrap();
    home
}

fn tmp_root(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-quote-root-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn req(question: &str, root: &Path) -> IntakeRequest {
    IntakeRequest {
        question: question.to_string(),
        template: false,
        ask: None,
        options: vec![],
        blocks: vec![],
        node: None,
        subject: None,
        session_id: Some("s1".to_string()),
        cwd: Some("/repo/fno".to_string()),
        asker: Some("worker-1".to_string()),
        laws: vec![],
        rung: None,
        storage_root: root.to_path_buf(),
        index_path: None,
        journal_path: None,
        display_name: None,
        render_cap: None,
        op: None,
        event: None,
        images: Vec::new(),
        screens_dir: None,
    }
}

const QUESTION_FILE: &str = "\
---
recommend: 1
---

Is a net-zero Python repair legal with no grant?

## Options
1. Yes, net zero or less needs no grant.
    What happens next: unblocks four fixes today
2. Stay strict.
    What happens next: every Python fix waits on a grant

## Blocked because
the reconcile fix and the merge_close fix are both Python edits

## Why these options
the three readings leads have acted on

## Downside
a repair can hide a feature

## Recommendation
Option 1, the narrowest door: every later fix needs it.

## Not thought through
whether a net-zero move between files counts

## Reversible
costly

## Cost if wrong
the push allowance drops to 0

## Meanwhile
stops
";
#[test]
fn refusals_quote_the_line_they_read_and_the_shape_they_want() {
    let home = tmp_home("quote");
    let root = tmp_root("quote");
    let question = QUESTION_FILE
        .replace(
            "## Reversible\ncostly",
            "## Reversible\nyes, after a backup",
        )
        .replace(
            "    What happens next: every Python fix waits on a grant\n",
            "",
        );
    let mut r = req(&question, &root);
    r.node = Some("x-aaaa".to_string());
    let answer = run_intake(&r, &home);
    assert_eq!(answer.exit_code, 2);
    assert_eq!(answer.refusal.as_deref(), Some("context"));
    let joined = answer.lines.join("\n");
    assert!(joined.contains("reads 'yes, after a backup'"), "{joined}");
    assert!(joined.contains("exactly one word"), "{joined}");
    assert!(joined.contains("option 2"), "{joined}");
    assert!(joined.contains("'What happens next:'"), "{joined}");
    assert!(answer.qid.is_none(), "a refusal mints no id");
}

#[test]
fn the_class_refusal_quotes_the_line_it_read() {
    // The refusal quotes the class: line and names 'decision' not a class.
    let home = tmp_home("quote-class");
    let root = tmp_root("quote-class");
    let class_case = QUESTION_FILE.replace(
        "---\nrecommend: 1\n---",
        "---\nrecommend: 1\nclass: decision\n---",
    );
    let mut r = req(&class_case, &root);
    r.node = Some("x-aaaa".to_string());
    let answer = run_intake(&r, &home);
    assert_eq!(answer.exit_code, 2);
    assert_eq!(answer.refusal.as_deref(), Some("class"));
    let joined = answer.lines.join("\n");
    assert!(joined.contains("read 'class: decision'"), "{joined}");
    assert!(joined.contains("'decision' is not a class"), "{joined}");
    assert!(answer.qid.is_none(), "a refusal mints no id");
}
