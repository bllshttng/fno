//! The blocked rung: how an agent row decides Blocked, and the one merge
//! where the screen-manifest rung outranks the hook rung.
//!
//! The hook rung owns working/done. A harness whose session-state wiring
//! declares `screen_backstop` or no blocked producer also admits a fresh
//! screen `blocked` verdict over its last working/done report. This keeps a
//! parked permission prompt visible when its notification was missed.
//! Unknown harnesses and expired screen evidence never gain that authority.

use crate::agents_view::{report_is_live, resolved_rows};
use crate::proto::{AgentBadge, AnswerablePrompt};

/// True when the screen manifest owns the blocked rung for the harness: its
/// session-state wiring names no blocked producer (`blocked = "none"`), or
/// sets `screen_backstop` because its blocked event covers only some waits.
/// Fail-closed: unknown harness, non-supported job, unwired or event-named
/// wiring without the flag all read false.
fn wiring_names_no_blocked_producer(harness: Option<&str>) -> bool {
    let Some(h) = harness else {
        return false;
    };
    let Some(row) = resolved_rows().get(h) else {
        return false;
    };
    let Some(job) = row.get("hooks").and_then(|h| h.get("session_state")) else {
        return false;
    };
    if job.get("state").and_then(|v| v.as_str()) != Some("supported") {
        return false;
    }
    let Some(wiring) = job.get("wiring") else {
        return false;
    };
    wiring.get("blocked").and_then(|v| v.as_str()) == Some("none")
        || wiring.get("screen_backstop").and_then(|v| v.as_bool()) == Some(true)
}

/// The screen-manifest rung's badge mapping, moved from agents_view's
/// leg-less arm: a fresh verdict maps blocked/working; `idle` maps to no
/// badge (AgentBadge has no Idle variant; adding one is a proto bump,
/// serialized behind the v7 wire work). Anything malformed fails
/// badge-closed, and the answer payload rides only a live blocked verdict
/// (a malformed payload degrades to focus-only, never blanks the badge).
pub(crate) fn screen_rung_badge(
    row: &serde_json::Value,
    now_secs: u64,
) -> Option<(Option<AgentBadge>, Option<String>, Option<AnswerablePrompt>)> {
    let ss = row.get("screen_state")?;
    if ss.is_null() {
        return None;
    }
    let live = report_is_live(
        ss.get("at").and_then(|v| v.as_str()).unwrap_or(""),
        ss.get("ttl_ms").and_then(|v| v.as_u64()),
        now_secs,
    );
    if !live {
        return None; // TTL lapsed -> liveness-only
    }
    // Manifest vocabulary is working|idle|blocked.
    let badge = match ss.get("state").and_then(|v| v.as_str()) {
        Some("working") => Some(AgentBadge::Working),
        Some("blocked") => Some(AgentBadge::Blocked),
        _ => None,
    };
    // The matched rule doubles as the human hint, the way an inside-leg
    // reason does.
    let reason = badge
        .is_some()
        .then(|| ss.get("rule").and_then(|v| v.as_str()).map(str::to_string))
        .flatten();
    // Answer payload only for a live `blocked` scrape verdict that carried
    // one. from_value tolerates the missing/extra field.
    let answerable = if badge == Some(AgentBadge::Blocked) {
        ss.get("answerable")
            .filter(|v| !v.is_null())
            .and_then(|v| serde_json::from_value::<AnswerablePrompt>(v.clone()).ok())
    } else {
        None
    };
    Some((badge, reason, answerable))
}

/// The inside-leg rung's badge mapping, moved from agents_view: a live leg's
/// state word maps directly; a TTL-lapsed leg degrades to liveness-only.
/// Hook-badged blocks carry no answer payload in v1 (the grammar is a
/// scrape-rung concern); focus-only.
pub(crate) fn leg_badge(
    leg: &serde_json::Value,
    now_secs: u64,
) -> (Option<AgentBadge>, Option<String>, Option<AnswerablePrompt>) {
    let live = report_is_live(
        leg.get("received_at")
            .and_then(|v| v.as_str())
            .unwrap_or(""),
        leg.get("ttl_ms").and_then(|v| v.as_u64()),
        now_secs,
    );
    if !live {
        return (None, None, None);
    }
    let badge = match leg.get("state").and_then(|v| v.as_str()) {
        Some("working") => Some(AgentBadge::Working),
        Some("blocked") => Some(AgentBadge::Blocked),
        Some("done") => Some(AgentBadge::Done),
        _ => None,
    };
    let reason = leg
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    (badge, reason, None)
}

/// A fresh blocked screen may override working/done on a declared backstop
/// harness. A hook-blocked leg keeps its own authority. Answering the prompt
/// resolves the screen override on the next sweep.
pub(crate) fn merged_badge(
    leg: &serde_json::Value,
    row: &serde_json::Value,
    now_secs: u64,
) -> (Option<AgentBadge>, Option<String>, Option<AnswerablePrompt>) {
    let (badge, reason, answerable) = leg_badge(leg, now_secs);
    if badge != Some(AgentBadge::Blocked) {
        if let Some(merged) = screen_blocked_override(row, now_secs) {
            return merged;
        }
    }
    (badge, reason, answerable)
}

/// The merge's guarded form: the wiring check plus a fresh `blocked` state,
/// then the plain screen mapping.
fn screen_blocked_override(
    row: &serde_json::Value,
    now_secs: u64,
) -> Option<(Option<AgentBadge>, Option<String>, Option<AnswerablePrompt>)> {
    if !wiring_names_no_blocked_producer(
        row.get("harness")
            .or_else(|| row.get("provider"))
            .and_then(|v| v.as_str()),
    ) {
        return None;
    }
    match screen_rung_badge(row, now_secs) {
        Some((Some(AgentBadge::Blocked), reason, answerable)) => {
            Some((Some(AgentBadge::Blocked), reason, answerable))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents_view::derive_rows;

    fn reg(rows: &str) -> String {
        format!(r#"{{"schema_version": 6, "agents": [{rows}]}}"#)
    }

    const NOW_STAMP: &str = "2027-01-15T08:00:00Z";

    // Fresh screens badge hookless rows and back up a lapsed Claude hook.
    // Invalid or expired screen evidence never supplies a badge.
    #[test]
    fn agent_rows_screen_state_rung_requires_fresh_evidence() {
        let raw = reg(&format!(
            r#"{{"name":"scraped-blocked","cwd":"/w","status":"live",
                 "screen_state":{{"state":"blocked","rule":"permission_prompt",
                                  "seq":2,"at":"{recent}","ttl_ms":120000}}}},
               {{"name":"scraped-idle","cwd":"/w","status":"live",
                 "screen_state":{{"state":"idle","rule":"idle_prompt",
                                  "seq":1,"at":"{recent}","ttl_ms":120000}}}},
               {{"name":"scraped-lapsed","cwd":"/w","status":"live",
                 "screen_state":{{"state":"working","rule":"busy","seq":1,
                                  "at":"2020-01-01T00:00:00Z","ttl_ms":120000}}}},
               {{"name":"scraped-corrupt","cwd":"/w","status":"live",
                 "screen_state":{{"state":"working","rule":"busy","seq":1,
                                  "at":"garbage","ttl_ms":120000}}}},
               {{"name":"hook-lapsed","cwd":"/w","status":"live","provider":"claude",
                 "inside_leg":{{"state":"working","seq":9,
                                "received_at":"2020-01-01T00:00:00Z","ttl_ms":60000}},
                 "screen_state":{{"state":"blocked","rule":"leftover","seq":1,
                                  "at":"{recent}","ttl_ms":120000}}}}"#,
            recent = "2027-01-15T07:59:30Z",
        ));
        let now = crate::agents_view::rfc3339_like_to_secs(NOW_STAMP).unwrap();
        let rows = derive_rows(&raw, now).unwrap();
        let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        let blocked = get("scraped-blocked");
        assert_eq!(blocked.badge, Some(AgentBadge::Blocked));
        assert_eq!(blocked.reason.as_deref(), Some("permission_prompt"));
        assert_eq!(get("scraped-idle").badge, None, "idle = plain live row");
        assert_eq!(get("scraped-lapsed").badge, None, "TTL lapse ages out");
        assert_eq!(get("scraped-corrupt").badge, None, "corrupt stamp closed");
        assert_eq!(
            get("hook-lapsed").badge,
            Some(AgentBadge::Blocked),
            "a parked prompt stays blocked after the hook report lapses"
        );
    }

    // Screen backstops survive working and lapsed hook reports on every
    // wired harness; the answer payload still reaches the client.
    #[test]
    fn parked_prompts_outrank_working_and_lapsed_hook_reports() {
        for harness in ["claude", "opencode", "codex", "agy", "pi"] {
            let raw = reg(&format!(
                r#"{{"name":"cx-live-leg","cwd":"/w","status":"live","provider":"codex",
                 "inside_leg":{{"state":"working","seq":9,
                                "received_at":"{recent}","ttl_ms":60000}},
                 "screen_state":{{"state":"blocked","rule":"question_prompt",
                                  "seq":1,"at":"{recent}","ttl_ms":120000,
                                  "answerable":{{"prompt":"Which colour should the demo use?",
                                                  "options":[{{"idx":"1","label":"Red","keystroke":[49]}}],
                                                  "fingerprint":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],
                                                  "region_lines":20}}}}}},
               {{"name":"cx-lapsed-leg","cwd":"/w","status":"live","provider":"codex",
                 "inside_leg":{{"state":"working","seq":7,
                                "received_at":"2020-01-01T00:00:00Z","ttl_ms":60000}},
                 "screen_state":{{"state":"blocked","rule":"approval_prompt",
                                  "seq":1,"at":"{recent}","ttl_ms":120000}}}}"#,
                recent = "2027-01-15T07:59:30Z",
            ));
            let now = crate::agents_view::rfc3339_like_to_secs(NOW_STAMP).unwrap();
            let raw = raw.replace("codex", harness);
            let rows = derive_rows(&raw, now).unwrap();
            let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
            let merged = get("cx-live-leg");
            assert_eq!(merged.badge, Some(AgentBadge::Blocked));
            assert_eq!(merged.reason.as_deref(), Some("question_prompt"));
            let ans = merged.answerable.as_ref().expect("payload parsed");
            assert_eq!(ans.options.len(), 1);
            let lapsed = get("cx-lapsed-leg");
            assert_eq!(lapsed.badge, Some(AgentBadge::Blocked));
            assert_eq!(lapsed.reason.as_deref(), Some("approval_prompt"));
        }
    }

    // The merge's fail-closed guard: a LAPSED screen verdict, a NON-blocked
    // verdict, and an UNNAMED harness all leave the leg's own verdict (or
    // liveness) standing - the screen speaks only for the rung it owns,
    // fresh, on a row whose wiring admits the merge.
    #[test]
    fn screen_blocked_merge_is_fail_closed() {
        let raw = reg(&format!(
            r#"{{"name":"cx-lapsed-ss","cwd":"/w","status":"live","provider":"codex",
                 "inside_leg":{{"state":"working","seq":9,
                                "received_at":"{recent}","ttl_ms":60000}},
                 "screen_state":{{"state":"blocked","rule":"old","seq":1,
                                  "at":"2020-01-01T00:00:00Z","ttl_ms":120000}}}},
               {{"name":"cx-working-ss","cwd":"/w","status":"live","provider":"codex",
                 "inside_leg":{{"state":"working","seq":9,
                                "received_at":"{recent}","ttl_ms":60000}},
                 "screen_state":{{"state":"idle","rule":"idle_prompt","seq":1,
                                  "at":"{recent}","ttl_ms":120000}}}},
               {{"name":"unnamed-harness","cwd":"/w","status":"live",
                 "inside_leg":{{"state":"working","seq":9,
                                "received_at":"{recent}","ttl_ms":60000}},
                 "screen_state":{{"state":"blocked","rule":"x","seq":1,
                                  "at":"{recent}","ttl_ms":120000}}}}"#,
            recent = "2027-01-15T07:59:30Z",
        ));
        let now = crate::agents_view::rfc3339_like_to_secs(NOW_STAMP).unwrap();
        let rows = derive_rows(&raw, now).unwrap();
        let get = |n: &str| rows.iter().find(|r| r.name == n).unwrap();
        let lapsed = get("cx-lapsed-ss");
        assert_eq!(
            lapsed.badge,
            Some(AgentBadge::Working),
            "a lapsed screen verdict ages out; the live leg's own word stands"
        );
        assert!(
            lapsed.reason.is_none(),
            "the lapsed screen's rule never leaks into the badge reason"
        );
        let working = get("cx-working-ss");
        assert_eq!(
            working.badge,
            Some(AgentBadge::Working),
            "a non-blocked verdict never merges; the leg's state stands"
        );
        assert_eq!(
            get("unnamed-harness").badge,
            Some(AgentBadge::Working),
            "unnamed harness is fail-closed; the leg's state stands"
        );
    }
}
