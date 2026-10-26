//! The early transport arms `gh-cache` and `target-status`: Python and shell
//! callers exec HERE through `command_call` or the binary directly,
//! transport-only - the group registers no client action. One dispatch line
//! in `bin/client.rs` covers the group, the same shape
//! `harness_reader::transport_doors` gives the harness probes; client.rs sits
//! against its line budget, so each family's body lives in its own module and
//! the arm comments moved with the code. `worktree-reapable` and
//! `launch-workdir` stay their own arms in client.rs: the x-9ee6 port owns
//! those moves into the worktree and launch_workdir modules.

/// The group's dispatch: `Some(exit code)` when `args` names one of these
/// arms, `None` so the caller falls through to the next arm.
pub fn transport_early_arms(args: &[String]) -> Option<i32> {
    match args.first().map(String::as_str) {
        // Permanent GitHub-fact rows (merged PRs, repo metadata); reconcile
        // and base-lineage reach it through command_call; transport-only.
        Some("gh-cache") => Some(crate::gh_cache::run()),
        // `target-status`: the orientation report door; Python's `fno do
        // target status` reaches it through command_call, transport-only.
        Some("target-status") => Some(crate::target_orient::run_status(&args[1..])),
        _ => None,
    }
}
