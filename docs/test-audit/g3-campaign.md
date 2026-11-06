# G3 cut ledger: orchestration - loopcheck, finalize, daemon, lead, PR, review, merge, mail, events, plan, target

Base aa9a1f7e4d (origin/main at this branch's start, the G1 merge). Scope: the 398 files the path rule in the [README](README.md#campaigns) gives G3, all Rust. Before: 2,508 declarations. After: 2,435 declarations (ratio 0.97, cut 73). The collected Rust count before and after comes from the CI logs. The Counts section records it.

The README's half target (1,255 or fewer) is not reached, and the plan's fallback applies: each audited file records its retained floor. The reason is measured, not asserted. G3 is the orchestration safety chain. The 33 files read in full hold about 1,050 declarations and carried only 73 redundant rows. 67 of those 73 came from the three files G1's map flagged as widest (loop_check, finalize_e2e, authorized_merge). Every other deep-read file held its floor. The tests are incident-named. A row cites the repro, the law or the PR that demanded it. Fail-closed guards keep exactly one refusal each, and the parity files pin one golden per command or trigger arm. A mechanical junk sweep over the whole remaining tree found zero assertion-free probes, zero source-grep tests and zero literal-truth asserts. Cutting deeper deletes named incident proofs, which the keep rule forbids.

## Method

Thirty files in the highest declaration tiers were read in full, one judgment per file. The cut rules name six junk patterns:

- a duplicate contract proof
- a unit pin under a real-command keeper
- an assertion-free probe
- a weaker duplicate refusal row
- a source-prose grep
- a contract past the three-row cap

Six files took cuts, and the rest recorded floors. A pattern sweep then ran over every unaudited G3 file and flagged nothing. The sweep looks for assertion-free bodies, source reads of a crates path, and literal-truth asserts. Six small files were spot-read to confirm the density finding. Under the fleet-wide freeze on local test runs, no cargo test ran anywhere. Validation is CI-gated, and the preservation spot-check is waived for the same reason, stated here as the README requires. One cargo check --all-targets ran at the end of the cut and found no dead helpers.

## Counts

| Measure | Before | After |
|---|---:|---:|
| Declarations, Rust (the whole G3 scope is Rust) | 2,508 | 2,435 |
| Collected cases, Rust, fno-agents crate (unit nextest 4,462 + integration shards 1,329, cli-ci run 42094799694 at the base) | 5,791 | pending: same read from this PR's first green run |

## Files

One row per in-scope file. `kept in full` means this branch changed nothing in the file and the sweep found no junk pattern in it. Its test names are the contract record.

| File | Before | After | Contracts kept (credible failure) |
|---|---:|---:|---|
| `crates/fno-agents/codegen/event_retention.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/examples/wrapper_outcome_eval/tests.rs` | 15 | 15 | all keepers: anti-gaming evidence integrity, each rejection arm distinct |
| `crates/fno-agents/src/agents_event.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/authorized_merge/head_probe.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/authorized_merge/step_timer.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/authorized_merge/tests.rs` | 58 | 55 | 3 cuts: arm-effect and per-PR rule rows covered by the parity table (commit 9882aa998d) |
| `crates/fno-agents/src/backlog/closed_unmerged.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/backlog/merge_evidence.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/backlog/merge_state.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/backlog/orphan_plans.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/backlog/pr_link.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/backlog/provenance_cli_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/backlog/provenance_read.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/backlog/target_binding.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/bin/daemon.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/ci_cadence.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/ci_front.rs` | 13 | 13 | kept in full |
| `crates/fno-agents/src/ci_tier.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/claim_cli/reap.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/codex_daemon_readiness.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/codex_daemon_upgrade.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon/claude_stop.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/daemon/codex_thread_lane.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/convert.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/lifecycle.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/list_rows.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/daemon/report.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/rm_codex_rollback.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/sandbox_host.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon/sandbox_ingest.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/shell_writers_sweep.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/daemon/stop_refusal_detail.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon/sweeps.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/terminal_stop_sweep.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/tests/adopt_pin_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/tests/blocking_bound_tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/daemon/tests/codex_thread_lane.rs` | 23 | 23 | all keepers: lane safety chains (recovery freshness, posture mint, steer, zombie stop, seq gate, worktree bind) each incident-named |
| `crates/fno-agents/src/daemon/tests/gc_mux_member.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon/tests/gc_receipts.rs` | 38 | 38 | all keepers: reaper fail-closed gates (surface-removal hold, receipt-first, empty short id, idle exit, pid stop, recycled pid, settle, reverse-join sweep, receipt gate, pane kill), each incident-named |
| `crates/fno-agents/src/daemon/tests/gc_receipts/additional_pr_settle.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/daemon/tests/gc_receipts/blueprint_retirement.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/daemon/tests/gc_receipts/lineage_kind.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/daemon/tests/gc_receipts/open_pr_guard.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/daemon/tests/gc_receipts/retire_removes_session.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/daemon/tests/incident_pause.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/daemon/tests/keeper_stop.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon/tests/keeper_sweep.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/daemon/tests/pid_zombie_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/tests/reap_holds.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/daemon/tests/reap_session.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/daemon/tests/report_codex.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/tests/rm_refusal.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/daemon/tests/rm_success.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/daemon/tests/stop_claims.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/daemon/tests/store_socket_sweep_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/transcript_push.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/daemon/worktree_sweep.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/daemon_predecessor.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/daemon_tests.rs` | 54 | 54 | all keepers: bind race, rm door, reconcile transition arms, liveness ladder rungs, stream front door, switchboard identity CAS, each incident-named |
| `crates/fno-agents/src/eval_attempt/tests.rs` | 16 | 16 | all keepers: classifier state machine, cohort door folds, rev pin fail-closed, CLI arg-door wiring |
| `crates/fno-agents/src/eval_attempt/tests_observe.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/eval_events.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/eval_part4.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/eval_reap.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/evals.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/evals/bank.rs` | 13 | 13 | kept in full |
| `crates/fno-agents/src/evals/failure_types.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/evals/grading.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/evals/graduate.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/evals/research_grade.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/evals/rubric.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/evals/runner.rs` | 18 | 18 | all keepers: sealed-tree integrity, read fence, lane argv families |
| `crates/fno-agents/src/evals/tests_grade.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/evals/tests_mint.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/evals_arm/tests.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/evals_candidates.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/evals_capability.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/evals_climb/tests.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/evals_compare.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/evals_drafts.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/evals_gate.rs` | 36 | 35 | 1 cut: checkout_of unit pin of a 2-line helper; the real-boundary proof a covered crate head builds the checkout it read the bank from asserts the slot path in build argv (commit 3b96dee1b4) |
| `crates/fno-agents/src/evals_macro.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/evals_power.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/evals_predict_tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/evals_qualification/tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/evals_trend/tests.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/evals_trend/tests_cohort.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/event_context.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/event_rules.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/event_store/tests.rs` | 26 | 26 | all keepers: store durability contracts (schema ensure, routed reads, rotation, corrupt/bad-scope rejects, migration, dead-letter, journal_text family, query folds) |
| `crates/fno-agents/src/event_store/tests/coverage.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/event_store/tests/observation.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/events.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/events_limits.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/finalize.rs` | 30 | 30 | all keepers: merge-posture forgery pins, commit_wip trio, repo-identity cross-language parity, handoffs resolution |
| `crates/fno-agents/src/finalize_pointer_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/finalize_run_summary.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/gc_claude_stop.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/gc_inventory.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/gc_native.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/gc_sweep.rs` | 18 | 18 | all keepers: reaper fail-closed family (tmp residue, retention, unreadable age, lock families, unlink race) and the stale-do predicate's distinct input shapes |
| `crates/fno-agents/src/gc_verify.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/harness_daemon.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/heal/tests.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/heal_sweep.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/honesty_sweep.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/hook/git_protection/merge_gate.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/hook/lead_guard.rs` | 11 | 11 | kept in full |
| `crates/fno-agents/src/hook/pr_attribution.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/inbox_peers.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/intel_plans.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/lane_heal.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/law_pr_shape.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_answers.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/lead_checkin.rs` | 14 | 14 | kept in full |
| `crates/fno-agents/src/lead_checkin_beat_budget.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/lead_checkin_blueprint.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/lead_checkin_evals.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/lead_checkin_layout_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_checkin_lineup.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_checkin_machine.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_checkin_peer_blocked.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_checkin_posture.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_checkin_prompt_parked.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_checkin_readings_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_checkin_watch_tests.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_escalation.rs` | 18 | 18 | all keepers: route ladder arms, byte-identical row text, reading kinds, four distinct render-refusal classes, scope resolution |
| `crates/fno-agents/src/lead_eval.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/lead_eval_grade/mod.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_goal.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/lead_history.rs` | 21 | 21 | all keepers: verdict state machine one proof per arm (exceeded, absent, unknown, stalled, converging, unmeasured), scanner kinds, corrupt-store naming, hygiene render, beat cadence |
| `crates/fno-agents/src/lead_hygiene.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/lead_lifecycle.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/lead_mail.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_ops.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/lead_pushback.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_reads.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/lead_rebind.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/lead_settle.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/lead_state.rs` | 20 | 20 | all keepers: org vacancy verdict family one proof per roster arm, split detection, shape/term refusals, manifest byte pin, arm/rearm family |
| `crates/fno-agents/src/lead_term.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/lead_termination.rs` | 17 | 17 | all keepers: capacity-gate outcome arms (BlockedWithNext, Saturated, Split, SaturatedBlind, open), each with a distinct board shape |
| `crates/fno-agents/src/lead_verdict_inputs.rs` | 17 | 17 | all keepers: resolver refusal discipline, grant-heal family, close-count guard, transcript-served liveness override |
| `crates/fno-agents/src/lead_wake.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/liveness_sweep.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/loop_dispatch.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/loop_lead.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/loop_readiness.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loop_runtime.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loop_target.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/async_wait.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/loopcheck/attestation_journal.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/authorship.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/awaiting_merge.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/loopcheck/ci_checks.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/loopcheck/coverage_gate.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/loopcheck/coverage_receipt.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/loopcheck/footnote_evidence.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/held_refusal.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/loopcheck/holds.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/loopcheck/lead_decide.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/loopcheck/plan_fidelity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/read_bounds.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/loopcheck/review_count.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/review_findings.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/loopcheck/review_state.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/args_tests.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/bot_nudge_tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/bot_verdict_tests.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/bounded_run_tests.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/ci_checks_tests.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/coverage_classify_tests.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/coverage_status_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/coverage_tests.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/decide_tests.rs` | 32 | 32 | all keepers: review-gate family, P1/P2 regressions, watcher-hint message contracts |
| `crates/fno-agents/src/loopcheck/tests/findings_tests.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/fire_history_tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/gh_read_tests.rs` | 16 | 16 | all keepers: the 447 regression pair, quota-park arms, fallback gate trio, field-set superset invariant, fork-blip retries with measured rationale |
| `crates/fno-agents/src/loopcheck/tests/intent_tests.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/local_attestation_tests.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/plan_fidelity_tests.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/posture_tests.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/pr_read_tests.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/review_coverage_command_tests.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/review_findings_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/self_review_floor_tests.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/loopcheck/tests/settings_tests.rs` | 21 | 21 | all keepers: config fail-closed family; nearly every test pins a named P1/P2 spelling or sentinel refusal |
| `crates/fno-agents/src/loopcheck/tests/space_chokepoint_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/loopcheck/watch_lease.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/machine_mail.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/mail/frame.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail/state.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/mail_backfill.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_bus.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_carrier.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_cli/ack.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_cli/reply.rs` | 13 | 13 | kept in full |
| `crates/fno-agents/src/mail_cli/send/engine.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_cli/send/lanes.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/mail_cli/send/mod.rs` | 11 | 11 | kept in full |
| `crates/fno-agents/src/mail_cli/send/resolve.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_cli/send_raw.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_cli/team.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_control_drain.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_envelope.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/mail_header.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_hold.rs` | 17 | 17 | all keepers: hold arm/gate/park/drain families, control-pass framing with the byte-9 multibyte edge, grace never-extends |
| `crates/fno-agents/src/mail_inject.rs` | 35 | 35 | trim, no declaration cut: the marker-rewrite test kept only the recipient-row mapping at the mail boundary; the dialect arms are provider.rs's own inline tests (commit 00954addee) |
| `crates/fno-agents/src/mail_notify_self.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/mail_receipt.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_remote.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/mail_teach.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/mail_threads.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/main_ci_proof.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/main_ci_transition.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/merge_close.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/merge_command/mod.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/merge_freeze.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/merge_gates.rs` | 15 | 15 | kept in full |
| `crates/fno-agents/src/merge_grant/tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/merge_hold.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/merge_posture.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/merge_provenance.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/merge_queue/tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/merge_reap.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/org_board/pr_closure.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/org_board/pr_closure_produce.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/orphan_reap.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/plan_carry.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/plan_doc/brief_tests.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/plan_doc/cli_tests.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/plan_doc/codec.rs` | 17 | 17 | all keepers: codec corruption-fix family, named parse errors, parse-back gates, fragment reads, atomic write mode |
| `crates/fno-agents/src/plan_doc/consolidation.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/plan_doc/criteria_tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/plan_doc/doc.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/plan_doc/doc_tests.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/plan_doc/exec_validate_tests.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/plan_doc/folder_audit_tests.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/plan_doc/keeper.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/plan_doc/lock.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/plan_doc/migrate.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/plan_doc/mutate.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/plan_doc/project.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/plan_doc/reconcile.rs` | 19 | 19 | all keepers: tier1/2/3 sweep arms, corrupt-line repair, holds, link precedence, each a distinct state-machine transition |
| `crates/fno-agents/src/plan_doc/rollup.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/plan_doc/stamp.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/plan_doc/status.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/plan_schema.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/planning_lane.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/plans_dirs.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/plans_path.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/post_merge/ritual.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/post_merge/route.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_admission.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_base_check.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_body_check.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_create.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/pr_draft_ready.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_front.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/pr_idle.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/pr_info.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/pr_list.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_nudge.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/src/pr_park/tests.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/pr_push.rs` | 15 | 15 | kept in full |
| `crates/fno-agents/src/pr_rebase.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_status/adapter.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/pr_status/cache.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_status/logs.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_status/memo.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/pr_status/proxy.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_status/quota.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/pr_status/reviews.rs` | 13 | 13 | kept in full |
| `crates/fno-agents/src/pr_status/tests.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/src/pr_status/wait.rs` | 19 | 19 | all keepers: pinned-wait family, staleness predicate arms, cancel journal, backoff growth/cap, conflict exit |
| `crates/fno-agents/src/pr_status_facts/tests.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_steward.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_verify/tests.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/pr_watch/discover.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/pr_watch/dispatch.rs` | 23 | 22 | 1 cut: repoll-cutoff unit pin of a 2-line helper; the sweep-boundary tests prove floor-on, floor-off and floor-passage, and the unit row's unique arm only covered a defensive parse of an internally generated stamp (commit 4ec92ead20) |
| `crates/fno-agents/src/pr_watch/install.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_watch/render.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/pr_watch/state.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_watch/tick.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/pr_worktree.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/provenance.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/publish_review.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/question_bus.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/question_sweep.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/reap_evidence.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/reap_render.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/resume_receipt/revalidate.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/retired_lead.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/review_capability.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/review_findings.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/review_freshness.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/review_hold.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/review_invocation_owner.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/review_level.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/review_policy.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/review_summary.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/src/roster_reap.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/route_recovery.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/scoreboard/mod.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/src/scoreboard/readers.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/scoreboard_escalation.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/scoreboard_provider.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/state/tests/x43ce_provenance_repair.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/src/target_blast.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/target_init.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/target_orient.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/src/target_start.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/src/team_reap.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/src/watchdog/keeper.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/src/watchdog/tests.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/src/watchdog/unfinished.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/src/worktree_reapable.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/authorized_merge_journey.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/awaiting_merge_ruling/mod.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/claim_reap_parity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/codex_daemon_upgrade.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/codex_shared_daemon_attach.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/daemon_e2e.rs` | 28 | 28 | all keepers: served-while-sweeping, restart storm, recycled-pid refusal, registry divergence refusal family, drift stderr pin, cwd pin |
| `crates/fno-agents/tests/delivery_evaluate_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/evals_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/event_validate_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/events_sql_authority.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/finalize_e2e.rs` | 61 | 49 | 12 cuts: delivery-gate rows fold to pass, refuse and fail-closed; auto-merge arming drops withhold variants of the same blocked-reason mechanism, the strategy fallback and a duplicated DoneAdvisory row (commit 9a9bfb1c3b) |
| `crates/fno-agents/tests/heal_fleet_task.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/inbox_peers_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/lead_drain_unreadable_cause.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/lead_injected_language.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/lead_lifecycle_parity.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/lead_loop.rs` | 35 | 35 | all keepers: the lead driver decision family; the durable-progress regression documents its sibling passes either way |
| `crates/fno-agents/tests/lead_ops_parity.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/lead_quiet_scope.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/lead_reads_parity.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/lead_wake_pass_parity.rs` | 23 | 23 | all keepers: golden parity per trigger arm (answer, debounce bypass, ceilings, successor respawn, board diff, backstop, budget stop, blocked-read degrade, argv pin) |
| `crates/fno-agents/tests/loop_cancel.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/loop_check.rs` | 168 | 116 | 52 cuts: value repeats, second refusal rows of one fail-closed guard, and parser variants folded to the strongest boundary keeper per contract (commit 06881ac816) |
| `crates/fno-agents/tests/loop_check_pause.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/tests/loop_check_replacement_pr.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/loop_dispatch_census.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/loop_readiness_pre_launch.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/loop_runtime.rs` | 26 | 26 | all keepers: walker state machine, journal store-lock contention arms, bg-guard park, store-committed termination readers |
| `crates/fno-agents/tests/loop_target.rs` | 31 | 27 | 4 cuts: assertion-free preflight probe (body ends let _ = out), weaker second row of the path-traversal refusal, preflight missing-binary row subsumed by the e2e keeper, and a source-prose grep (commit c9014ccd13) |
| `crates/fno-agents/tests/loopcheck_decisions.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/loopcheck_hook_payload.rs` | 7 | 7 | kept in full |
| `crates/fno-agents/tests/loopcheck_missing_manifest.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/loopcheck_session_binding.rs` | 14 | 14 | kept in full |
| `crates/fno-agents/tests/machine_mail_entry.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_acceptance_journey.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/tests/mail_ack_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_conformance.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/tests/mail_drain_self_parity.rs` | 8 | 8 | kept in full |
| `crates/fno-agents/tests/mail_hold_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_hop_smoke.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_pane_prepare_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_reply_parity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/mail_seed_provenance_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_send_parity.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/mail_send_raw_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_sent_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_team_parity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/mail_unread_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/mail_withdraw_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/merge_command_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/merged_stamp/mod.rs` | 5 | 5 | kept in full |
| `crates/fno-agents/tests/plans_path_parity.rs` | 9 | 9 | kept in full |
| `crates/fno-agents/tests/post_merge_parity.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/pr_body_check.rs` | 12 | 12 | kept in full |
| `crates/fno-agents/tests/pr_push.rs` | 10 | 10 | kept in full |
| `crates/fno-agents/tests/pr_rebase_note.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/pr_watch_heal_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/pr_watch_install_parity.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/pr_watch_refresh_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/pr_watch_status_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/pr_watch_tick_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/publish_review_argv_parity.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/recovery_sweep_parity.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/relay_transcript_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/retro_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/review_capability_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/review_coverage_command.rs` | 15 | 15 | kept in full |
| `crates/fno-agents/tests/review_coverage_paths.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/tests/review_findings_parity.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/review_hold_parity.rs` | 22 | 22 | all keepers: golden parity per claim review-hold command arm (check, acquire, release, round, cap refusal, metadata, stdin door) |
| `crates/fno-agents/tests/review_invocation_parity.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/review_level_parity.rs` | 3 | 3 | kept in full |
| `crates/fno-agents/tests/review_policy_parity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/scoreboard_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/table_ownership.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/target_denominator_native.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/target_family_parity.rs` | 2 | 2 | kept in full |
| `crates/fno-agents/tests/test_owned_keeper_reap.rs` | 6 | 6 | kept in full |
| `crates/fno-agents/tests/watchdog_keeper_parity.rs` | 1 | 1 | kept in full |
| `crates/fno-agents/tests/watchdog_outage_parity.rs` | 4 | 4 | kept in full |
| `crates/fno-agents/tests/watchdog_recoverable_parity.rs` | 3 | 3 | kept in full |

