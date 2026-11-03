use super::*;
use serde_json::json;
use tempfile::TempDir;

fn make_folder_plan(root: &Path, name: &str) {
    let folder = root.join(name);
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(
        folder.join("00-INDEX.md"),
        "---\nstatus: ready\n---\n# plan\n",
    )
    .unwrap();
}

fn owner_ids(owners: &[NonTerminalOwner]) -> Vec<&str> {
    owners.iter().map(|o| o.node_id.as_str()).collect()
}

#[test]
fn only_a_non_terminal_node_owning_a_folder_plan_by_basename_counts() {
    let tmp = TempDir::new().unwrap();
    make_folder_plan(tmp.path(), "live-folder");
    make_folder_plan(tmp.path(), "done-folder");
    make_folder_plan(tmp.path(), "orphan-folder");
    make_folder_plan(tmp.path(), "renamed-root-folder");
    let entries = vec![
        json!({"id": "ab-1", "status": "ready", "plan_path": "internal/fno/plans/live-folder"}),
        // The doc frontmatter may still say ready, but the node is done.
        json!({"id": "ab-2", "status": "done", "plan_path": "internal/fno/plans/done-folder"}),
        json!({"id": "ab-3", "status": "ready", "plan_path": "internal/fno/plans/some-other-doc.md"}),
        // A different absolute root than the scanned dir: the basename join still matches.
        json!({"id": "ab-4", "status": "ready", "plan_path": "/Users/other/fno/plans/renamed-root-folder"}),
    ];

    let owners = scan(tmp.path(), &entries).unwrap();

    assert_eq!(owner_ids(&owners), ["ab-1", "ab-4"]);
    assert_eq!(owners[1].folder, "renamed-root-folder");
}

#[test]
fn an_unscannable_plans_dir_reads_as_none_not_as_zero_owners() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("does-not-exist");
    let entries = vec![json!({"id": "ab-5", "status": "ready", "plan_path": "x"})];

    assert!(scan(&missing, &entries).is_none());
    assert!(scan(tmp.path(), &entries).unwrap().is_empty());
}
