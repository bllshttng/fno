# cli/tests cut ledger (everything except unit and agents)

Base sha: 2a898fbb6e. Scope: every `cli/tests` file outside `cli/tests/unit` and `cli/tests/agents`. Sibling cuts own those two trees.

| Measure | Before | After | Cut |
|---|---:|---:|---:|
| Declarations (README census pattern) | 2,792 | 1,180 | 58% |
| Collected cases (`pytest --collect-only`) | 3,235 | 1,348 | 58% |
| Files with tests | 221 | 212 | 9 deleted |

## Method

The keep rule in [README.md](README.md) decided every cut. A kept test names one contract it alone guards. Every sole guard for a refusal, lock or gate kept one row. Cases were deleted, not folded into parametrize rows, so the collected count fell with the declarations. A few files fold several fail-closed scenarios into one sweep that shares its setup, for example the keep reasons in `test_worktree_cleanup_merged.py`. One sweep now asserts every keep row, where nine sweeps ran before.

## Whole files deleted

- `relay/test_roundtrip.py`: no `cli/src` caller reaches `fno.relay.roundtrip`.
- `integration/test_claims_cross_impl.py`: `fno.claims.core` runs every claim operation through `fno-agents claim`, so the matrix compared Rust with Rust. Its runner `tests/test-claims-compat-matrix.sh` went with it.
- Six more went whole. Each tested a Python copy with no caller, a retired surface, or a kept twin. They are `integration/test_verify_child_promise.py`, `test_reconcile_session_satisfied.py`, `test_migrate_source_field.py`, `test_graph_hygiene.py`, `test_target_claim_integration.py` and `test_v2_paths.py`.
- `integration/test_selection_score.py` keeps two of seven. `undispatched.py` still sorts through the Python key, so its vote term keeps a guard.
- `test_no_stale_deleted_capture_refs.py`: a retired-name guard.

## Kept contracts by area

- Watchdog and recovery: wake ceilings, no wake without evidence, adoption refusals, the refused zero-row sweep, reap guards and redispatch aborts.
- Hooks: one row per parser branch of each guard, every deny envelope, the fail-open paths and the grant scrubs.
- pr_watch, done, retro: the chunk refusal, admission refusals, merge freeze, grant parks, close refusals and gh outage exit 4.
- Graph, fanout, collision, blueprint: config refusals, cursor order, lease loss, supersede refusals and the note evidence gate.
- Integration, worktree, doctor: dry-run veto, merge-trigger refusal, keep reasons, salvage, config refusals and the graph edit block.
- Lint, events, bus, mcp, goldens: every two-sided band and fail-closed lint path, emit refusals, lock budgets, golden refusals.
- Scoreboard, observer, skill-diff, top level: corrupt-input exits, traversal and replay guards, spawn reservation refusals, lazy-import budgets.

## Owed

- Preservation spot-check (five mutations, keeper goes red): not run. A fleet overload ban stopped local test runs during this cut. CI runs the remaining suite.
- `fno.relay.roundtrip` and the uncalled `fno.relay.registry` helpers are dead production code. Filed as its own node.

## Files changed

Twenty files with tests are unchanged and not listed.

| File | Before | After |
|---|---:|---:|
| `bus/test_alias_and_migration.py` | 6 | 3 |
| `bus/test_bus_addressed.py` | 8 | 4 |
| `bus/test_bus_convergence.py` | 10 | 3 |
| `bus/test_bus_cursor.py` | 8 | 4 |
| `bus/test_bus_cursor_addressed.py` | 5 | 2 |
| `bus/test_bus_log.py` | 12 | 5 |
| `bus/test_store_canon_flip.py` | 5 | 3 |
| `bus/test_to_lead.py` | 14 | 6 |
| `bus/test_to_project.py` | 18 | 8 |
| `events/test_emit_attestation_actor.py` | 11 | 4 |
| `events/test_emit_schema.py` | 11 | 3 |
| `events/test_ephemeral_set_parity.py` | 5 | 2 |
| `events/test_event_emit_cli.py` | 28 | 12 |
| `events/test_event_find.py` | 7 | 3 |
| `events/test_event_gc.py` | 9 | 5 |
| `events/test_migration.py` | 9 | 4 |
| `events/test_producer_equivalence.py` | 11 | 5 |
| `events/test_protocol_conformance.py` | 7 | 3 |
| `events/test_protocol_emit_cli.py` | 5 | 2 |
| `events/test_protocol_family.py` | 14 | 2 |
| `events/test_protocol_push.py` | 8 | 3 |
| `events/test_python_validator.py` | 27 | 7 |
| `events/test_retention_routing.py` | 5 | 3 |
| `events/test_rust_events_documented.py` | 8 | 2 |
| `events/test_schema_files.py` | 12 | 3 |
| `events/test_schema_manifest.py` | 8 | 3 |
| `goldens/test_advance_golden.py` | 3 | 2 |
| `goldens/test_lifecycle_golden.py` | 18 | 8 |
| `goldens/test_reconcile_golden.py` | 3 | 1 |
| `goldens/test_session_golden.py` | 9 | 4 |
| `goldens/test_triage_golden.py` | 5 | 2 |
| `graph/test_batched_member_selection.py` | 9 | 4 |
| `graph/test_discovery_pass.py` | 3 | 2 |
| `graph/test_entry_status_computed.py` | 17 | 5 |
| `graph/test_node_id.py` | 23 | 11 |
| `graph/test_open_pr_selection.py` | 13 | 6 |
| `hooks/test_bg_process_guard.py` | 14 | 13 |
| `hooks/test_init_target_state_skip_flags.py` | 12 | 10 |
| `hooks/test_operator_capture_nudge.py` | 5 | 4 |
| `hooks/test_recursive_grep_guard.py` | 12 | 10 |
| `hooks/test_register_session_start.py` | 6 | 4 |
| `integration/test_authorized_merge.py` | 7 | 4 |
| `integration/test_backlog_alias.py` | 25 | 9 |
| `integration/test_blueprint_firstfill.py` | 3 | 2 |
| `integration/test_blueprint_mutation.py` | 56 | 25 |
| `integration/test_canonical_account_binding.py` | 9 | 4 |
| `integration/test_carveout.py` | 29 | 13 |
| `integration/test_claims_concurrency.py` | 7 | 3 |
| `integration/test_claims_cross_impl.py` | 10 | deleted |
| `integration/test_codex_plugin_cli_boundary.py` | 8 | 5 |
| `integration/test_codex_plugin_real_boundary.py` | 2 | 1 |
| `integration/test_collision.py` | 45 | 25 |
| `integration/test_consolidation_audit.py` | 10 | 4 |
| `integration/test_doctor.py` | 14 | 7 |
| `integration/test_doctor_reads_back.py` | 23 | 11 |
| `integration/test_done.py` | 15 | 6 |
| `integration/test_e2e_hook_block.py` | 8 | 1 |
| `integration/test_effect_idempotency.py` | 4 | 3 |
| `integration/test_first_run_documentation.py` | 5 | 3 |
| `integration/test_fno_plan_brief.py` | 24 | 8 |
| `integration/test_graph_claim_filter.py` | 10 | 4 |
| `integration/test_graph_cli.py` | 76 | 26 |
| `integration/test_graph_hygiene.py` | 5 | deleted |
| `integration/test_health_monitor.py` | 37 | 9 |
| `integration/test_lane_slots_concurrency.py` | 2 | 1 |
| `integration/test_law_skill_contract.py` | 2 | 1 |
| `integration/test_mail_hold_release.py` | 6 | 2 |
| `integration/test_marketplace_lint.py` | 9 | 4 |
| `integration/test_migrate_paths.py` | 8 | 4 |
| `integration/test_migrate_source_field.py` | 7 | deleted |
| `integration/test_native_lane_bundles.py` | 4 | 2 |
| `integration/test_native_update_repaint.py` | 7 | 3 |
| `integration/test_operator_single_doc.py` | 7 | 3 |
| `integration/test_product_delivery_journey.py` | 6 | 3 |
| `integration/test_provenance_cmd.py` | 18 | 8 |
| `integration/test_rank_operator_only.py` | 6 | 2 |
| `integration/test_reconcile_session_satisfied.py` | 9 | deleted |
| `integration/test_retro_land.py` | 11 | 4 |
| `integration/test_retro_pr_scope.py` | 25 | 9 |
| `integration/test_retro_sentinel_consumer.py` | 19 | 6 |
| `integration/test_routing_inventory_policy.py` | 5 | 3 |
| `integration/test_selection_score.py` | 7 | 2 |
| `integration/test_skill_discovery.py` | 13 | 5 |
| `integration/test_state_cli.py` | 10 | 4 |
| `integration/test_target_claim_integration.py` | 2 | deleted |
| `integration/test_target_node_claim.py` | 8 | 4 |
| `integration/test_test_run_lifecycle.py` | 3 | 2 |
| `integration/test_undispatched_order.py` | 4 | 1 |
| `integration/test_verb_projection.py` | 4 | 2 |
| `integration/test_verify_child_promise.py` | 10 | deleted |
| `integration/test_worktree_cargo_build_base.py` | 3 | 2 |
| `integration/test_worktree_cleanup_merged.py` | 63 | 24 |
| `integration/test_worktree_ensure.py` | 23 | 11 |
| `integration/test_worktree_foreign_repo.py` | 3 | 2 |
| `integration/test_worktree_manager.py` | 11 | 4 |
| `journeys/test_company_function_conformance.py` | 13 | 8 |
| `journeys/test_growth_studio_pack.py` | 3 | 2 |
| `lint/test_agent_skill_loading.py` | 2 | 1 |
| `lint/test_append_lesson_candidate.py` | 4 | 2 |
| `lint/test_architect_agent_contract.py` | 4 | 2 |
| `lint/test_check_parity_test_provenance.py` | 13 | 6 |
| `lint/test_check_pitfalls.py` | 25 | 12 |
| `lint/test_check_preamble_budget.py` | 35 | 15 |
| `lint/test_drafting_lenses_contract.py` | 5 | 3 |
| `lint/test_execution_validation_wiring.py` | 8 | 4 |
| `lint/test_review_skill_freshness.py` | 4 | 3 |
| `lint/test_shellout_drift.py` | 18 | 8 |
| `lint/test_smoke_wheel_env_scrub.py` | 9 | 5 |
| `lint/test_think_skill_size.py` | 2 | 1 |
| `lint/test_think_workflow_contract.py` | 2 | 1 |
| `lint/test_verb_callers.py` | 11 | 5 |
| `lint/test_verb_ratchet.py` | 14 | 6 |
| `lint/test_verb_ratchet_source_scan.py` | 6 | 4 |
| `mcp/test_channel_wire.py` | 17 | 7 |
| `mcp/test_request_id.py` | 10 | 3 |
| `mcp/test_sidecar_lifecycle.py` | 15 | 6 |
| `observer/test_cli.py` | 30 | 13 |
| `observer/test_fold.py` | 23 | 10 |
| `observer/test_judge.py` | 5 | 3 |
| `relay/test_registry.py` | 19 | 1 |
| `relay/test_roundtrip.py` | 67 | deleted |
| `schemas/test_target_state_types.py` | 34 | 9 |
| `scoreboard/test_by_provider.py` | 3 | 2 |
| `scoreboard/test_by_skill.py` | 22 | 8 |
| `scoreboard/test_calibration.py` | 9 | 3 |
| `scoreboard/test_efficiency.py` | 21 | 9 |
| `scoreboard/test_lanes.py` | 3 | 2 |
| `scoreboard/test_plan_fidelity.py` | 37 | 16 |
| `scoreboard/test_scoreboard.py` | 34 | 15 |
| `skill_diff/test_cli_tick.py` | 32 | 14 |
| `skill_diff/test_engine.py` | 17 | 7 |
| `skill_diff/test_guards.py` | 10 | 5 |
| `skill_diff/test_synthesize.py` | 10 | 5 |
| `test_agents_watchdog.py` | 183 | 87 |
| `test_ambient_identity_scrub.py` | 12 | 4 |
| `test_autonomy_registry_ci.py` | 2 | 1 |
| `test_autonomy_status.py` | 13 | 5 |
| `test_config_schema_drift.py` | 12 | 5 |
| `test_done_cross_check.py` | 28 | 12 |
| `test_exit_codes.py` | 3 | 1 |
| `test_graph_concurrent_writers.py` | 2 | 1 |
| `test_help_shorthands.py` | 7 | 3 |
| `test_lazy_imports.py` | 38 | 17 |
| `test_mail_send_style.py` | 7 | 4 |
| `test_menu_caps_lint.py` | 11 | 4 |
| `test_mux_shell_integration.py` | 6 | 2 |
| `test_no_hardcoded_node_id.py` | 3 | 2 |
| `test_no_stale_deleted_capture_refs.py` | 2 | deleted |
| `test_node_claim_release_authority.py` | 5 | 2 |
| `test_plan_schema_drift.py` | 8 | 3 |
| `test_pr_read_budget.py` | 6 | 3 |
| `test_pr_watch_cli_adapters.py` | 36 | 14 |
| `test_pr_watch_decide.py` | 23 | 10 |
| `test_pr_watch_dispatch.py` | 114 | 50 |
| `test_pr_watch_draft_flip.py` | 2 | 1 |
| `test_pr_watch_evals_phase.py` | 6 | 2 |
| `test_pr_watch_fire_skill.py` | 10 | 4 |
| `test_pr_watch_install.py` | 32 | 14 |
| `test_pr_watch_merge_cleanup_mint.py` | 3 | 1 |
| `test_pr_watch_rest_listing.py` | 20 | 8 |
| `test_pr_watch_sigma2.py` | 23 | 8 |
| `test_pr_watch_sweep_clock.py` | 9 | 4 |
| `test_pr_watch_watermark_store.py` | 12 | 5 |
| `test_preflight_receipt_scopes.py` | 4 | 2 |
| `test_provider_exec_guard.py` | 3 | 1 |
| `test_provider_outage.py` | 34 | 15 |
| `test_quota_refusal_and_failover.py` | 16 | 7 |
| `test_recovery.py` | 128 | 61 |
| `test_shim_roadmap_tasks.py` | 2 | 1 |
| `test_short_flag_convention.py` | 11 | 5 |
| `test_short_flag_dispatch.py` | 3 | 2 |
| `test_spawn_flag_owners_lint.py` | 3 | 2 |
| `test_spawn_guard.py` | 26 | 11 |
| `test_state_root_inventory.py` | 3 | 2 |
| `test_status_fanout.py` | 98 | 45 |
| `test_truth_status.py` | 23 | 10 |
| `test_two_spelling_dispatch.py` | 2 | 1 |
| `test_v2_paths.py` | 1 | deleted |
