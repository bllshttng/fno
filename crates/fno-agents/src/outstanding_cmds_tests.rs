//! Characterization tests for the native `fno inbox outstanding` subcommands.
//!
//! Goldens captured from the deleted Python leg (`cli/src/fno/outstanding/`)
//! before its deletion: the refusal lines, exit codes and lane bytes here
//! are what scripts and operators were mid-read on.

use super::*;
use std::path::PathBuf;

fn sandbox(tag: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "fno-agents-outstanding-cmds-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn lane_at(root: &std::path::Path) -> PathBuf {
    root.join("my-priorities.md")
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn add_appends_one_open_item_and_reports_the_new_count() {
    let root = sandbox("add");
    let lane = lane_at(&root);
    assert_eq!(mine_do_at(&lane, &args(&["add", "ship the port"])), 0);
    let text = std::fs::read_to_string(&lane).unwrap();
    assert_eq!(text, "- [ ] ship the port\n");
    assert_eq!(mine_do_at(&lane, &args(&["add", "second thing"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [ ] ship the port\n- [ ] second thing\n"
    );
}

#[test]
fn unknown_action_and_argument_counts_refuse_before_the_lock() {
    let root = sandbox("action-bad");
    let lane = lane_at(&root);
    assert_eq!(mine_do_at(&lane, &args(&["bogus", "x"])), 2);
    assert_eq!(mine_do_at(&lane, &args(&["done"])), 2);
    assert_eq!(mine_do_at(&lane, &args(&["link", "1"])), 2);
    assert_eq!(mine_do_at(&lane, &args(&["add"])), 2);
    assert!(!lane.exists());
}

#[test]
fn done_flips_the_mark_and_back_again() {
    let root = sandbox("done");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [ ] first\n- [x] second\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["done", "1"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [x] first\n- [x] second\n"
    );
    assert_eq!(mine_do_at(&lane, &args(&["done", "1"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [ ] first\n- [x] second\n"
    );
}

#[test]
fn out_of_range_index_refuses_under_the_lock_and_keeps_the_lane() {
    let root = sandbox("oob");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [ ] only\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["done", "2"])), 1);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [ ] only\n",
        "a refused mutation leaves the lane byte-identical"
    );
}

#[test]
fn link_writes_the_node_pointer_and_keeps_the_mark_and_ending() {
    let root = sandbox("link");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [x] done item\r\n- [ ] open item\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["link", "1", "fno-a1b2"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [x] done item -> fno-a1b2\r\n- [ ] open item\n"
    );
}

fn ask_row(qid: &str, question: &str) -> String {
    format!(
        r#"{{"ts":"2026-10-24T09:00:00Z","type":"operator_question","source":"test","data":{{"question_id":"{qid}","question":"{question}","blocks":[]}}}}"#
    )
}

#[test]
fn reindex_folds_journal_rows_into_the_index_once() {
    let root = sandbox("reindex");
    let index = root.join("questions.jsonl");
    let journal = root.join("project").join("events.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(
        &journal,
        format!(
            "{}\n{}\n",
            ask_row("q-r1", "first?"),
            ask_row("q-r2", "second?")
        ),
    )
    .unwrap();
    let (added, already, total) = reindex_fold(&index, &[journal.clone()]).unwrap();
    assert_eq!((added, already, total), (2, 0, 2));
    // Idempotent: a second fold reports already-indexed, adds nothing.
    let (added, already, total) = reindex_fold(&index, &[journal]).unwrap();
    assert_eq!((added, already, total), (0, 2, 2));
    // The fold commits through the store, so the committed view is the
    // reader: the raw journal bytes may never land.
    let indexed = crate::events::committed_journal_text(&index);
    assert!(indexed.contains("q-r1") && indexed.contains("q-r2"));
}

#[test]
fn an_unreadable_machine_index_refuses_the_fold() {
    let root = sandbox("reindex-refuse");
    let index = root.join("blocked");
    std::fs::create_dir_all(&index).unwrap();
    let journal = root.join("project").join("events.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    std::fs::write(&journal, ask_row("q-r9", "y?")).unwrap();
    let err = reindex_fold(&index, &[journal]).unwrap_err();
    assert!(!err.is_empty(), "the index read failure names itself");
}

fn refusal(kind: &str, agent: &str, origin: Option<&str>) -> crate::decide_door::DecideRefusal {
    match kind {
        "origin-authority" | "authority" => crate::decide_door::DecideRefusal::RefusedAuthority {
            agent: agent.to_string(),
            origin: origin.map(str::to_string),
        },
        _ => crate::decide_door::DecideRefusal::UnknownOrigin(origin.unwrap_or("nope").to_string()),
    }
}

#[test]
fn provenance_refusals_map_to_their_posture_kinds_and_texts() {
    let (kind, text, origin) = provenance_refusal(refusal("unknown-origin", "a1", Some("nope")));
    assert_eq!(kind, "unknown-origin");
    assert!(text.contains("mail origin 'nope' is unknown"), "{text}");
    assert_eq!(origin, None);
    let (kind, text, origin) = provenance_refusal(refusal("authority", "a1", None));
    assert_eq!(kind, "authority");
    assert!(
        text.contains("cannot record under superuser authority"),
        "{text}"
    );
    assert_eq!(origin, None);
    let (kind, text, origin) =
        provenance_refusal(refusal("origin-authority", "a1", Some("operator")));
    assert_eq!(kind, "origin-authority");
    assert!(text.contains("from operator origin"), "{text}");
    assert_eq!(origin.as_deref(), Some("operator"));
    let (kind, text, _) =
        provenance_refusal(crate::decide_door::DecideRefusal::UnattributedAuthority);
    assert_eq!(kind, "unattributed");
    assert_eq!(text, "the provenance resolver refused");
}

#[test]
fn a_delivery_without_an_asker_names_the_lead_escalation_lane() {
    let line = deliver_answer("q-d1", "ship?", "", None, "d-abc1", "yes");
    assert_eq!(
        line,
        "outstanding: q-d1 answered; no asker on record (lead escalation), nobody to wake. Decision d-abc1; recover it via: fno backlog decisions"
    );
}
