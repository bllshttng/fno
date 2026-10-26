use super::*;

#[test]
fn superseded_candidate_is_visible_to_the_consolidation_gate() {
    // The consolidation gate reads `superseded_by` off the node summary; a
    // superseded candidate must stay visibly superseded, never read as live.
    let entry = json!({"id": "x-948e", "status": "ready", "superseded_by": "x-579e"});
    assert_eq!(
        node_summary(&entry, false),
        json!({"id": "x-948e", "status": "ready", "superseded_by": "x-579e"})
    );
}
