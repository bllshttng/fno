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
        "review-level" => Some(crate::review_level::run_review_level(rest)),
        "review-policy" => Some(crate::review_policy::run_review_policy(rest)),
        "target-init" => Some(crate::target_init::run(rest)),
        _ => None,
    }
}
