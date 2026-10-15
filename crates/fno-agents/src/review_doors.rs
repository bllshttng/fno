//! Hidden binary-direct review doors. Dispatched from the client's
//! hidden-verb run BEFORE the routable-verb table, so the routable-verb
//! parity guard and CLIENT_VERB_USAGE never see these verbs: they read
//! state and print, they are not `fno agents` verbs.

/// Route a hidden review door by verb, `None` when the verb is not one.
/// Returning `Some(exit)` is terminal for the dispatch (the caller returns
/// the code).
pub fn dispatch(verb: &str, rest: &[String]) -> Option<i32> {
    match verb {
        "review-summary" => Some(crate::review_summary::run_review_summary(rest)),
        "review-level" => Some(run_review_level_door(rest)),
        "review-policy" => Some(crate::review_policy::run_review_policy(rest)),
        _ => None,
    }
}

/// The `review-level` door: the capability group rides UNDER it, so
/// fno-agents gains no root verb and menu-caps stays at root 12/12. The
/// interception runs before the level parser, and only when the second
/// token is one of the four action words - `capability` is not a level
/// word, so no valid level call changes, and a bare `review-level
/// capability` falls through to the level door (its usage refusal).
fn run_review_level_door(rest: &[String]) -> i32 {
    if rest.first().map(String::as_str) == Some("capability")
        && rest.get(1).map(String::as_str).is_some_and(|action| {
            matches!(
                action,
                "reviewers" | "github-apps" | "invocation" | "skill-presence"
            )
        })
    {
        crate::review_capability::run(&rest[1..])
    } else {
        crate::review_level::run_review_level(rest)
    }
}
