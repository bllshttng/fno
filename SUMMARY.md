# Summary: the claim reap leaf forwards to a Rust engine

## What landed

- `crates/fno-agents/src/claim_cli/reap.rs`: the reap engine ported from the Python `reap` leaf, with hermetic goldens captured from the original engine before the cutover, so capture-before-forward holds.
- `crates/fno-agents/tests/claim_reap_parity.rs`: a parity test asserting the Rust verdicts agree with the captured behavior case by case.
- `cli/src/fno/claims/core.py`: the Python reap engine (`reap_dead_claims` and the shared verdict ladder) is gone; the `reap` verb forwards to the Rust binary, the opt-out restore split into `_optout_restore.py`, and the now-unused imports left with the code.
- `cli/tests/unit/test_claim_reap.py`: the CLI-over-the-table and blind-roster tests still drive the verb and pin what survives the forward (fail-closed banner, dry-run default, apply archives); the probe and roster wiring tests now call a `_probe_sweep` helper that makes the exact `sweep_verdict` call the pre-forward CLI loop made, so the abandonment-probe contract stays pinned at the API the spawn guard shares.

## Deviation from the plan

The plan expected the probe and roster wiring tests to keep asserting on CLI output; once the verb forwards, the Rust roster arms answer fail-closed and the Python probe wiring is unreachable through the CLI. The tests were repointed, not deleted: kill-criterion coverage is unchanged and the net test count is flat.

## Verification

- `fno doctor test` on the changed-file set: 144 passed, exit 0.
- ruff + mypy: 591 files clean. `check-no-internal-refs.sh`: clean.
- The Rust side carries the parity test and goldens; the goldens are the original engine's own captured outputs.
