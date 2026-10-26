//! The early transport arms `gh-cache`, `target-status`, `launch-workdir` and
//! `worktree-reapable`: Python and shell callers exec HERE through
//! `command_call` or the binary directly, transport-only - the group
//! registers no client action. One dispatch line in `bin/client.rs` covers
//! the group, the same shape `harness_reader::transport_doors` gives the
//! harness probes; client.rs sits against its line budget, so each family's
//! body lives in its own module and the arm comments moved with the code.

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
        // `launch-workdir`: the spawn door's launch-cwd resolution (see
        // launch_workdir.rs doc). Registers no client action; Python's
        // ensure_launch_workdir reaches it through command_call, and a `hold`
        // answer is a valid exit-0 answer the caller renders.
        Some("launch-workdir") => Some(crate::launch_workdir::run_launch_workdir(&args[1..])),
        // `worktree-reapable`: the worktree-removal gate; callers:
        // worktree_gate.py, worktree-reapable.sh.
        Some("worktree-reapable") => Some(crate::worktree_reapable::run_client(&args[1..])),
        _ => None,
    }
}
