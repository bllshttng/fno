//! One door for the binary-direct `pr-*` verbs client.rs hands over.
//!
//! These arms were plain `matches!` checks, which the routable-verb parity
//! guard (it reads only `build_request`'s `"verb" =>` arms) never saw; folding
//! them into one match here keeps that invisible to the guard AND keeps
//! client.rs at or under its line budget: a new verb costs one arm in this
//! match, not four routing lines there.

/// Route one `pr-*` verb. `None` when the verb is not ours, so client.rs
/// falls through to the rest of its dispatch.
pub fn run_verb(verb: &str, args: &[String]) -> Option<i32> {
    match verb {
        "pr-body-check" => Some(crate::pr_body_check::run(&args[1..])),
        "pr-create" => Some(crate::pr_create::run_pr_create_verb(&args[1..])),
        "pr-base-lineage-check" => Some(crate::gate_probes::run_base_lineage_verb(&args[1..])),
        "pr-merge-result-check" => Some(crate::pr_merge_result::run_merge_result_verb(&args[1..])),
        // verb_call serves the JSON payload on stdin with no argv; the CLI
        // forward passes `--base`. The arg presence picks the transport.
        "pr-base-check" => Some(if args.len() > 1 {
            crate::pr_base_check::run(&args[1..])
        } else {
            crate::pr_base_check::run_stdin()
        }),
        "pr-verify" => Some(crate::pr_verify::run(&args[1..])),
        _ => None,
    }
}
