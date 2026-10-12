//! (x-3cb3) The deliberate break: a `fno` binary that cannot answer.
//!
//! This test sets `FNO_BIN`, which is PROCESS-GLOBAL. libtest runs the tests
//! in one integration binary on parallel threads, so an env-mutating test
//! sharing a binary with any other test is a race: the sibling inherits
//! `/bin/false` and fails, or wins the scheduling and makes this one read a
//! real payload. Hence its own file, which is its own binary, which is the
//! only structure that makes the mutation safe without a global lock or a
//! `--test-threads=1` invocation nobody remembers to type.

use fno::org_overlay::{fold_now, gate_now};

#[tokio::test]
async fn dark_binaries_fail_every_half_inside_their_budgets() {
    // SAFETY: this file holds exactly one test, so no other thread can be
    // reading the environment while it is written. See the module note.
    unsafe { std::env::set_var("FNO_BIN", "/bin/false") };
    unsafe { std::env::set_var("FNO_AGENTS_BIN", "/bin/false") };
    let start = std::time::Instant::now();

    let org = fold_now().await;
    let gate = gate_now().await;

    assert!(org.is_none(), "the lanes half fails bounded");
    assert!(gate.is_err(), "the gate half fails bounded");
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "both halves answer inside their budgets, not their timeouts"
    );
}
