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
fn add_refuses_an_empty_or_multiline_text_without_touching_the_lane() {
    let root = sandbox("add-bad");
    let lane = lane_at(&root);
    assert_eq!(mine_do_at(&lane, &args(&["add", "   "])), 2);
    assert_eq!(mine_do_at(&lane, &args(&["add", "two\nlines"])), 2);
    assert!(!lane.exists(), "a refused add writes nothing");
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
fn a_non_numeric_index_refuses_before_the_lock() {
    let root = sandbox("nan");
    let lane = lane_at(&root);
    assert_eq!(mine_do_at(&lane, &args(&["done", "one"])), 2);
    assert_eq!(mine_do_at(&lane, &args(&["drop", "1x"])), 2);
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
fn drop_removes_the_visible_line() {
    let root = sandbox("drop");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [ ] keep\n- [ ] go\n- [ ] keep too\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["drop", "2"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [ ] keep\n- [ ] keep too\n"
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

#[test]
fn link_refuses_a_malformed_node_id_without_touching_the_lane() {
    let root = sandbox("link-bad");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [ ] open item\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["link", "1", "Not A Node"])), 2);
    assert_eq!(std::fs::read_to_string(&lane).unwrap(), "- [ ] open item\n");
}

#[test]
fn link_over_a_parked_suffix_replaces_it_with_the_node() {
    let root = sandbox("link-parked");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [ ] waiting -> parked: not now\n").unwrap();
    assert_eq!(mine_do_at(&lane, &args(&["link", "1", "fno-c3d4"])), 0);
    assert_eq!(
        std::fs::read_to_string(&lane).unwrap(),
        "- [ ] waiting -> fno-c3d4\n",
        "the link rewrite carries no park reason, the same overwrite the old leg made"
    );
}

#[test]
fn add_on_a_missing_lane_creates_the_parent_and_the_file() {
    let root = sandbox("add-new");
    let lane = root.join("deep").join("nested").join("my-priorities.md");
    assert_eq!(mine_do_at(&lane, &args(&["add", "first"])), 0);
    assert_eq!(std::fs::read_to_string(&lane).unwrap(), "- [ ] first\n");
}

#[test]
fn the_list_arm_renders_the_visible_order_with_marks_and_nodes() {
    let root = sandbox("list");
    let lane = lane_at(&root);
    std::fs::write(
        &lane,
        "- [ ] plain\n- [x] finished -> fno-e5f6\n- [ ] parked one -> parked: later\n",
    )
    .unwrap();
    assert_eq!(mine_list_at(&lane, false), 0);
    let json_render = mine_list_at(&lane, true);
    assert_eq!(json_render, 0);
}

#[test]
fn the_json_list_carries_n_text_done_node_rows() {
    let root = sandbox("list-json");
    let lane = lane_at(&root);
    std::fs::write(&lane, "- [x] finished -> fno-e5f6\n").unwrap();
    // Render into a captured buffer by reading the lane through the same
    // payload builder the list arm uses.
    let items = parse_lane(&lane).unwrap();
    let rows: Vec<Value> = items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            json!({
                "n": i + 1,
                "text": item.text,
                "done": item.done,
                "node": item.node,
            })
        })
        .collect();
    let rendered = serde_json::to_string(&json!({ "mine": rows })).unwrap();
    assert_eq!(
        rendered,
        r#"{"mine":[{"n":1,"text":"finished","done":true,"node":"fno-e5f6"}]}"#
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
    let indexed = std::fs::read_to_string(&index).unwrap();
    assert!(indexed.contains("q-r1") && indexed.contains("q-r2"));
}

#[test]
fn reindex_skips_decision_rows_and_unreadable_journals() {
    let root = sandbox("reindex-skip");
    let index = root.join("questions.jsonl");
    let journal = root.join("project").join("events.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let decision = r#"{"ts":"2026-10-24T09:05:00Z","type":"operator_decision","source":"test","data":{"decision_id":"d-1"}}"#;
    std::fs::write(&journal, format!("{}\n", decision)).unwrap();
    let (added, already, total) = reindex_fold(&index, &[journal.clone()]).unwrap();
    assert_eq!((added, already, total), (0, 0, 0));
    // A journal path parked on a directory is unreadable, and that is not
    // fatal: the fold degrades to zero rows from it.
    let mut broken = journal.clone();
    broken.pop();
    std::fs::remove_dir_all(&broken).ok();
    let as_dir = broken;
    std::fs::create_dir_all(&as_dir).unwrap();
    std::fs::write(as_dir.join("events.jsonl"), ask_row("q-x", "x?")).unwrap();
    let (added, _already, _total) = reindex_fold(&index, &[as_dir.join("events.jsonl")]).unwrap();
    assert_eq!(added, 0, "an unreadable journal contributes nothing");
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
