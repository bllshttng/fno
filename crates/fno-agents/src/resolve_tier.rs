//! The tier a session token resolves a registry row at. Owns the token-shape
//! gate and the per-row tier walk so `client_commands` stays under its file
//! budget; the resolver (`find_agent_entry`) and every caller read them
//! through the `client_commands` re-export.

use serde_json::Value;

use crate::identity::session_handle_tier;

/// The tier a token resolves registry row `entry` at: 0 full id, 1 canonical
/// handle, 2 legacy suffix. The row's own id (`fno_id`) addresses it at the
/// full tier; a legacy short or name-valued fno_id keeps resolving through
/// the tiers below. A predecessor id addresses the row at the FULL tier only:
/// succession retired it, so delivery naming A follows the row that now
/// answers as B, while A's retired short/handle forms stay retired.
pub(crate) fn entry_session_tier(entry: &Value, token: &str) -> Option<u8> {
    if is_session_shaped(token) && entry.get("fno_id").and_then(Value::as_str) == Some(token) {
        return Some(0);
    }
    // The row's fno handle (the head of its fno-minted fno_id) addresses it at
    // the same short tier as the harness head, inside the union below.
    let own: Vec<&str> = ["harness_session_id", "related_session_id"]
        .iter()
        .filter_map(|key| entry.get(*key).and_then(Value::as_str))
        .collect();
    if crate::identity::fno_handle(entry.get("fno_id").and_then(Value::as_str), &own)
        .is_some_and(|handle| handle.eq_ignore_ascii_case(token.trim()))
    {
        return Some(1);
    }
    let session_id = entry.get("harness_session_id").and_then(Value::as_str)?;
    if let Some(tier) = session_handle_tier(token, session_id) {
        return Some(tier);
    }
    // The one optional related id addresses the row at the same tiers as the
    // primary (: both ids stay valid forever).
    if let Some(related) = entry.get("related_session_id").and_then(Value::as_str) {
        if let Some(tier) = session_handle_tier(token, related) {
            return Some(tier);
        }
    }
    entry
        .get("predecessor_session_ids")
        .and_then(Value::as_array)
        .and_then(|ids| {
            ids.iter()
                .filter_map(Value::as_str)
                .any(|id| session_handle_tier(token, id) == Some(0))
                .then_some(0)
        })
}

/// The row's stored session id a token named at the FULL tier: primary
/// first, then the one optional related id, then predecessors. `None` for
/// every shorter address (name, handle, short) -- by contract those select
/// the row's current session, so the exact-id resume contract only fires
/// when this returns one.
pub(crate) fn entry_matched_full_id(entry: &Value, token: &str) -> Option<String> {
    let id_at_full_tier = |key: &str| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .filter(|id| session_handle_tier(token, id) == Some(0))
            .map(str::to_string)
    };
    id_at_full_tier("harness_session_id")
        .or_else(|| id_at_full_tier("related_session_id"))
        .or_else(|| {
            entry
                .get("predecessor_session_ids")
                .and_then(Value::as_array)?
                .iter()
                .filter_map(Value::as_str)
                .find(|id| session_handle_tier(token, id) == Some(0))
                .map(str::to_string)
        })
}

/// True for a token worth probing a harness store with -- the Rust mirror of
/// `store_fallback.is_session_shaped`. A plain unknown NAME never probes, so a
/// typo keeps today's refusal instead of paying for three store reads.
pub(crate) fn is_session_shaped(token: &str) -> bool {
    let token = token.trim();
    if let Some(rest) = token.strip_prefix("ses_") {
        return !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric());
    }
    (token.len() == 8 && token.bytes().all(|b| b.is_ascii_alphanumeric()))
        || crate::resume_wake::is_uuid_shaped(&token.to_ascii_lowercase())
}

#[cfg(test)]
mod exact_id_tests {
    use super::*;
    use serde_json::json;

    /// The exact-id detector names a stored id only at the FULL tier.
    /// The primary returns itself; a related or predecessor full id returns
    /// the stored id the caller spelled (case-insensitively, the uuid
    /// family's contract); a name, a short, or a predecessor's retired
    /// short form returns None -- those keep the row's current session.
    #[test]
    fn detector_matches_full_ids_only() {
        let entry = json!({
            "name": "w1",
            "harness_session_id": "77770001-2222-3333-4444-555555555555",
            "related_session_id": "77770002-2222-3333-4444-555555555555",
            "predecessor_session_ids": ["77770003-2222-3333-4444-555555555555"]
        });
        let matched = |token: &str| entry_matched_full_id(&entry, token);
        assert_eq!(
            matched("77770001-2222-3333-4444-555555555555").as_deref(),
            Some("77770001-2222-3333-4444-555555555555")
        );
        assert_eq!(
            matched("77770003-2222-3333-4444-555555555555").as_deref(),
            Some("77770003-2222-3333-4444-555555555555")
        );
        assert_eq!(
            matched("77770002-2222-3333-4444-555555555555").as_deref(),
            Some("77770002-2222-3333-4444-555555555555")
        );
        assert_eq!(
            matched(&"77770003-2222-3333-4444-555555555555".to_uppercase()).as_deref(),
            Some("77770003-2222-3333-4444-555555555555")
        );
        assert!(matched("77770001").is_none(), "primary short keeps current");
        assert!(
            matched("77770003").is_none(),
            "predecessor short stays retired"
        );
        assert!(matched("w1").is_none(), "a name never names a session");
    }
}
