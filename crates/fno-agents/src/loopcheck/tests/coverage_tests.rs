use super::*;

#[test]
fn a_required_bot_that_went_stale_lands_in_stale_bots() {
    // The other reachable path: `stale_bots` carries the same gate weight
    // `missing_bots` always had. A required bot whose only verdict sits on
    // a commit it read two commits ago has not reviewed THIS code; the
    // entry keeps the sha it read so the block message can ask for a
    // re-read instead of a first read.
    let json = serde_json::json!({"reviews": pr826_reviews(), "comments": []});
    let required = vec!["chatgpt-codex-connector".to_string()];
    let stale = compute_review_info(&json, &required, &|_| Freshness::Stale);
    assert!(stale.missing_bots.is_empty());
    assert_eq!(stale.stale_bots.len(), 1);
    assert_eq!(stale.stale_bots[0].0, "chatgpt-codex-connector");
    assert!(!stale.all_required_passed());
    let carried = compute_review_info(&json, &required, &|_| Freshness::CarriedDocsOnly);
    assert!(carried.missing_bots.is_empty());
    assert!(carried.stale_bots.is_empty());
    // Activity timestamp is not a freshness question: a stale review is
    // still activity, and the no-progress probe must keep seeing it.
    assert_eq!(stale.latest_ts, "2026-08-12T17:51:48Z");
    assert_eq!(review_activity_ts(&json), "2026-08-12T17:51:48Z");
}
