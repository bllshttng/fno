//! A PR closed without a merge leaves its node in_review over work no reader
//! can act on. This leg returns the node to the queue and tells the user. The
//! closed PR stays on the node as a stamped additional PR, so a later reopen
//! and merge still closes the node.

use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

use super::drift_scan::MergeDriftRecord;
use crate::claims::ClaimState;
use crate::graph_store;

/// Move the closed primary PR into `additional_prs`. Answers false, and writes
/// nothing, unless the row still reads in_review over that PR: the scan read
/// the graph before this lock.
pub(crate) fn demote_closed_primary(node: &mut Value, number: i64) -> bool {
    if node.get("status").and_then(Value::as_str) != Some("in_review")
        || node.get("pr_number").and_then(Value::as_i64) != Some(number)
    {
        return false;
    }
    let Some(obj) = node.as_object_mut() else {
        return false;
    };
    let url = obj.get("pr_url").cloned().unwrap_or(Value::Null);
    let mut additional = vec![json!({"number": number, "url": url, "merge_status": "closed"})];
    // The scan read every ref closed. The stamp lets a replacement PR rebind.
    for row in obj
        .get("additional_prs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut row = row.clone();
        if let Some(fields) = row.as_object_mut() {
            fields.insert("merge_status".into(), json!("closed"));
        }
        additional.push(row);
    }
    obj.insert("additional_prs".into(), Value::Array(additional));
    obj.insert("pr_number".into(), Value::Null);
    obj.insert("pr_url".into(), Value::Null);
    obj.insert("merge_status".into(), Value::Null);
    true
}

/// A live worker may reopen its own PR or open the replacement, so its node
/// waits. A later sweep reads it again once the claim ends.
fn claim_holds(node_id: &str) -> bool {
    let (state, _) = crate::claims::status(&format!("node:{node_id}"), None);
    matches!(state, ClaimState::Live | ClaimState::Suspect)
}

fn recovery_line(record: &MergeDriftRecord) -> String {
    let pr = record.pr_number;
    let link = record.pr_url.as_deref().unwrap_or("no url recorded");
    format!(
        "PR #{pr} closed without a merge, so its work is not on main. The node left in_review. Recover the commits with: git fetch origin pull/{pr}/head. PR: {link}"
    )
}

/// Apply the moves in one locked mutation, then write a thread comment per
/// node and one notice for the sweep. A dry run reads and reports only.
pub(crate) fn closed_unmerged_leg(
    graph_path: &Path,
    records: &[MergeDriftRecord],
    dry_run: bool,
) -> Vec<Value> {
    let candidates: Vec<&MergeDriftRecord> = records
        .iter()
        .filter(|r| !claim_holds(&r.node_id))
        .collect();
    let receipt = |r: &MergeDriftRecord| json!({"node_id": r.node_id, "pr_number": r.pr_number, "pr_url": r.pr_url});
    if dry_run {
        return candidates
            .iter()
            .map(|r| {
                let mut row = receipt(r);
                row["would"] = Value::Bool(true);
                row
            })
            .collect();
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    let moved: std::cell::RefCell<Vec<&MergeDriftRecord>> = std::cell::RefCell::new(Vec::new());
    let published =
        graph_store::mutate_rows(graph_path, Duration::from_secs(30), None, None, |rows| {
            // A conflict retry reruns the closure: the last attempt's rows win.
            moved.borrow_mut().clear();
            for record in &candidates {
                let node = rows
                    .iter_mut()
                    .find(|e| e.get("id").and_then(Value::as_str) == Some(record.node_id.as_str()));
                if node.is_some_and(|n| demote_closed_primary(n, record.pr_number)) {
                    moved.borrow_mut().push(*record);
                }
            }
            Ok(!moved.borrow().is_empty())
        });
    if let Err(e) = published {
        eprintln!("warning: the closed-PR move failed: {e}; nothing moved this run");
        return Vec::new();
    }
    let moved = moved.into_inner();
    if moved.is_empty() {
        return Vec::new();
    }
    let store = super::api::Store::new(graph_path);
    for record in &moved {
        let input = super::api::CommentCreateInput {
            body: recovery_line(record),
            kind: Some("progress".to_string()),
            author: Some("agent".to_string()),
            ..Default::default()
        };
        if let Err(e) = super::api::comment_create(&store, &record.node_id, input) {
            eprintln!("warning: {} comment failed: {}", record.node_id, e.0);
        }
    }
    let lines: Vec<String> = moved
        .iter()
        .map(|r| format!("{}: PR #{}", r.node_id, r.pr_number))
        .collect();
    crate::operator_notice::notify_operator(
        "footnote: PR closed without a merge",
        &format!(
            "{} node(s) left in_review and returned to the queue. Dispatch or defer each one. {}",
            moved.len(),
            lines.join("; ")
        ),
        None,
    );
    moved.iter().map(|r| receipt(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_primary_moves_to_a_stamped_additional_pr() {
        let url = "https://github.com/o/r/pull/7";
        let mut node = json!({
            "id": "x-aaaa", "status": "in_review", "pr_number": 7, "pr_url": url,
            "additional_prs": [{"number": 5, "url": null}],
        });
        assert!(!demote_closed_primary(&mut node.clone(), 8), "another PR");
        assert!(demote_closed_primary(&mut node, 7));
        assert_eq!(node["pr_number"], Value::Null);
        assert_eq!(node["pr_url"], Value::Null);
        assert_eq!(node["additional_prs"][0]["number"], 7);
        assert_eq!(node["additional_prs"][0]["url"], url);
        assert_eq!(node["additional_prs"][0]["merge_status"], "closed");
        assert_eq!(node["additional_prs"][1]["number"], 5);
        assert_eq!(node["additional_prs"][1]["merge_status"], "closed");
        // The second sweep finds the move already made.
        assert!(!demote_closed_primary(&mut node, 7));
    }
}
