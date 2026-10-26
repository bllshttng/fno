//! The smoke registry: the structural steps plus the deferred quarantine.
//! GENERATED from crates/fno-agents/tests/golden/test_verb/smoke_structural.tsv
//! (captured from the Python leg before deletion); regenerate with the capture
//! script, never hand-edit the tables. The full behavioral
//! documentation lives in test_smoke.rs beside the runner.

/// (name, cwd, cmd). cwd is repo-relative ("." = root). A multi-line cmd
/// runs under `bash -eo pipefail -c`, matching GitHub's default shell.
pub(super) const STRUCTURAL_STEPS: &[(&str, &str, &str)] = &[
    ("Sync + build", "cli", "uv sync\nuv build"),
    ("Pytest (unit + integration)", "cli", "uv run pytest --tb=short -q -n auto --maxprocesses=4 --dist=loadgroup -m 'not slow_e2e' --ignore=tests/unit/test_ambient_canary.py"),
    ("Bash events-validate harness", ".", "bash tests/events/test-bash-validator.sh"),
    ("frontend-craft gate harness", ".", "bash tests/lib/test_frontend_surface.sh\nbash tests/lib/test_infer_has_ui.sh\nbash tests/lib/test_resolve_plan_executor.sh"),
    ("config global-precedence harness", ".", "bash tests/lib/test_config_global_precedence.sh\nbash tests/lib/test_config_plans_chain.sh"),
    ("cost-accuracy harness", ".", "uv run --project cli python cli/tests/unit/test_session_cost_dedup.py"),
    ("loop-check shim + immutable manifest harness", ".", "bash tests/hooks/test_loop_check_shim.sh\nbash tests/hooks/test_manifest_immutable.sh\nbash tests/hooks/test_graph_write_protect.sh\nbash tests/hooks/test_worktree_write_protect.sh\nbash tests/hooks/test_plan_location_guard.sh\nbash tests/hooks/test_worktree_live_peers.sh\nbash tests/hooks/test_setup_nudge_session_start.sh\nbash tests/hooks/test_init_target_session_id.sh\nbash tests/hooks/test_agy_stop_hook.sh\nbash tests/hooks/test_check_impl_location.sh"),
    ("Python hook harnesses (outside both test trees)", ".", "uv run --project cli python -m pytest -q tests/hooks/test_git_protection_push.py tests/hooks/test_merge_guard_worktree.py tests/hooks/test_merge_guard_stacked_base.py"),
    ("worktree lifecycle: remove-hook contract + cwd-anchored liveness + ttyless archive + job reap", ".", "bash tests/hooks/test_worktree_remove_lifecycle.sh"),
    ("in_review dispatch guard", ".", "bash tests/hooks/test_init_in_review_gate.sh"),
    ("born-with-why offer inject harness", ".", "bash tests/hooks/test_born_with_why_offer_inject.sh"),
    ("eval-sweep hygiene harness", ".", "bash tests/hooks/test_eval_sweep_session_start.sh"),
    ("ship-phase PR->node link verify guard", ".", "bash tests/target/test_ship_phase_link_verify.sh"),
    ("docs-before-ship phase-ordering guard", ".", "bash tests/test-docs-before-ship.sh"),
    (".fno/ dir-hygiene harness", ".", "python3 tests/metrics/test_completion_summary_path.py\nbash tests/lib/test_rotate_append_log.sh"),
    ("state-dir path gate self-test", ".", "bash scripts/tests/check-no-hardcoded-paths-selftest.sh"),
    ("corrections.log placement migration", ".", "bash scripts/tests/test_corrections_migrate.sh"),
    ("corrections fixture-row filter in the packet", ".", "bash scripts/tests/test_autocorrect_pack_fixture_rows.sh"),
    ("autocorrect packet skill-file leg", ".", "bash scripts/tests/test_autocorrect_pack_skill_files.sh"),
    ("autocorrect prompt contract", ".", "bash scripts/tests/test_autocorrect_prompt_contract.sh"),
    ("corrections skill-commit SOURCE resolution", ".", "bash tests/hooks/test_corrections_skill_commit.sh"),
    ("placement-rule lint self-test", ".", "bash scripts/tests/test_check_placement_rule.sh"),
    ("Build fno-agents debug binary (for journey tests)", "crates/fno-agents", "CARGO_BUILD_BUILD_DIR=\"$PWD/target\" cargo build"),
    ("Build the fno front door (for the hash gate)", "crates/fno", "CARGO_BUILD_BUILD_DIR=\"$PWD/target\" cargo build --quiet --bin fno"),
    ("paths.sh hash gate", ".", "FNO_AGENTS_WORKER=\"$PWD/crates/fno-agents/target/debug/fno-agents-worker\" crates/fno/target/debug/fno config paths verify scripts/lib/paths.sh"),
    ("Scoped @requires_rust parity suites (binary present; stubbed providers)", "cli", "FAKE_BIN=\"$(mktemp -d)\"\nfor p in codex gemini opencode; do printf \"%s\\n%s\\n\" \"#!/bin/sh\" \"exit 0\" > \"$FAKE_BIN/$p\"; chmod +x \"$FAKE_BIN/$p\"; done\nPATH=\"$FAKE_BIN:$PATH\" uv run pytest --tb=short -q tests/agents/test_rust_command_parity.py tests/agents/test_ask_e2e_dispatch.py tests/unit/test_claims_core.py::TestSessionWitnessVerdicts"),
    ("Keyless dispatch-to-terminal smoke (binary present)", "cli", "uv run pytest --tb=short -q tests/unit/test_keyless_smoke.py"),
    ("registry-miss heal across the Rust/Python seam", ".", "bash tests/test-agents-heal-token.sh"),
    ("loop-check journey tests (e2e + emission-schema + backstop-subprocess)", ".", "bash tests/hooks/test_loop_check_e2e.sh\nbash tests/events/test-loop-check-emission-schema.sh\nbash tests/hooks/test_loop_check_backstop_subprocess.sh"),
    ("Target self-handoff harness", ".", "bash tests/test-handoff.sh\nbash tests/target/test_handoff_ledger_record.sh"),
    ("Plan Mode front door harness", ".", "bash tests/hooks/test_capture_plan_mode.sh\nbash tests/hooks/test_pending_plan_wipe.sh\nbash tests/target/test_backfill_plan.sh\nbash tests/target/test_detect_pending_plan.sh\nbash tests/target/test_plan_mode_e2e.sh"),
    ("bg-dispatch harness", ".", "bash tests/test-bg-dispatch.sh"),
    ("init claim-wait harness", ".", "bash tests/test-init-claim-wait.sh"),
    ("dispatch grant posture harness", ".", "bash tests/target/test_dispatch_grant_posture.sh"),
    ("agent skill harness", ".", "bash tests/skills/test_agent_normalize.sh\nbash tests/skills/test_agent_receipt.sh\nbash skills/agent/tests/test_normalize.sh\nbash skills/agent/tests/test_confirm.sh\nbash skills/agent/tests/test_auto_worktree.sh\nbash skills/agent/tests/test_spawn_guard.sh"),
    ("mail skill harness", ".", "bash skills/mail/tests/test_normalize.sh"),
    ("events-discipline lint", ".", "bash scripts/lint/events-discipline.sh"),
    ("events-discipline lint self-test", ".", "bash tests/lint/test-events-discipline.sh"),
    ("No quarantined events.invalid.jsonl rows", ".", "bash scripts/lint/no-invalid-events.sh"),
    ("Smoke tests", ".", "bash cli/tests/smoke/run-all.sh"),
    ("no hardcoded paths", ".", "bash scripts/ci/check-no-hardcoded-paths.sh"),
    ("placement rule", ".", "bash scripts/ci/check-placement-rule.sh"),
    ("No residual old-name patterns (rename guard)", ".", "bash scripts/rename/residual-check.sh"),
    ("No stale skill refs (consolidation audit)", ".", "bash scripts/ci/check-no-stale-skill-refs.sh"),
    ("Company function-agnosticism check", ".", "bash scripts/ci/check-company-function-agnostic.sh"),
    ("Plugin function-agnosticism check", ".", "bash scripts/ci/check-plugins-function-agnostic.sh"),
    ("Company module boundary self-test", ".", "bash tests/ci/test_check_company_boundaries.sh"),
    ("Company module boundary check", ".", "bash scripts/ci/check-company-boundaries.sh --baseline"),
    ("Skill snippet hazard lint", ".", "bash scripts/ci/check-skill-snippets.sh"),
    ("Skill snippet lint self-test", ".", "bash tests/ci/test_check_skill_snippets.sh"),
    ("Pitfalls corpus cap", ".", "bash scripts/ci/check-pitfalls.sh"),
    ("No stale /spec refs (blueprint rename audit)", ".", "bash scripts/ci/check-no-stale-spec-refs.sh"),
    ("Config schema docs freshness", ".", "bash scripts/ci/check-config-schema-drift.sh"),
    ("Skill bundles freshness check", ".", "bash scripts/lint/check-skill-bundles-fresh.sh"),
    ("Hook exec bits", ".", "bash scripts/ci/check-hook-exec-bit.sh"),
    ("No ${REPO_ROOT}/scripts in skills", ".", "bash scripts/lint/no-repo-root-scripts-in-skills.sh"),
    ("Marketplace-readiness lint (no Skill calls, no path escapes, fno declared)", ".", "bash scripts/lint/no-cross-skill-runtime-calls.sh"),
    ("No unwrapped lib scripts", ".", "bash scripts/lint/no-unwrapped-lib-scripts.sh"),
    ("pin-skill generator self-test", ".", "bash tests/test-pin-skill.sh"),
    ("Agent flock-pattern lint", "cli", "uv run fno-py doctor lint flock-pattern"),
    ("Provider stderr-merge lint", "cli", "uv run fno-py doctor lint provider-stderr-merge"),
    ("Repo-root shell-out drift guard (clone-only allowlist)", "cli", "uv run fno-py doctor lint shellout-drift"),
    ("Spawn-shape lint (single-line argv guard)", "cli", "uv run fno-py doctor lint spawn-paths"),
    ("State-roots ratchet (declared root, R4/R5)", "cli", "uv run fno-py doctor lint state-roots"),
    ("Agent field coverage accounting", "cli", "uv run fno-py doctor lint field-coverage"),
    ("Menu-cap guard (cap 12 at three levels, hidden counts)", ".", "cargo test --quiet --manifest-path crates/fno/Cargo.toml --lib menu_caps"),
    ("Spawn flag-ownership ratchet (owner + provenance per flag)", "cli", "uv run fno-py doctor lint spawn-flag-owners"),
    ("Command-surface ratchet (real count, both binaries)", "cli", "uv run fno-py doctor lint command-ratchet"),
    ("Schema parity self-test", ".", "bash scripts/tests/check-event-schema-parity-selftest.sh"),
    ("Schema parity check (Python side)", ".", "bash scripts/check-event-schema-parity.sh"),
    ("Provider vocabulary parity selftest", ".", "bash tests/ci/test_provider_vocabulary_parity.sh"),
    ("Provider vocabulary parity check", ".", "bash scripts/ci/check-provider-vocabulary-parity.sh"),
    ("Reviewer descriptor parity selftest", ".", "bash scripts/ci/check-reviewer-descriptor-parity.sh --selftest"),
    ("Reviewer descriptor parity check", ".", "bash scripts/ci/check-reviewer-descriptor-parity.sh"),
    ("smoke mode machinery self-test", ".", "bash tests/ci/test_smoke_modes.sh"),
    ("hermetic lanes positive control (the dirty lane must be able to go red)", ".", "bash tests/ci/test_hermetic_lanes.sh"),
    ("preflight orchestration self-test", ".", "bash tests/ci/test_preflight.sh"),
    ("changed/full CI job-boundary guard", ".", "bash tests/ci/test_changed_smoke_workflow.sh"),
    ("smoke duration reporter self-test", ".", "bash tests/ci/test_smoke_duration_report.sh"),
    ("workflow timeout-comment guard", ".", "bash scripts/ci/check-workflow-timeout-comments.sh"),
];

/// Shell harnesses discovery finds but smoke must not run yet (the
/// quarantine). Drain by deleting a line: discovery then covers it, and
/// both consumers subtract this set.
pub(super) const DISCOVERY_DEFERRED: &[&str] = &[
    "tests/events/test-check-pr-emits-polling.sh",
    "tests/hooks/test_init_claim_stderr_and_modern_claim.sh",
    "tests/hooks/test_init_contested_steal_guard.sh",
    "tests/hooks/test_init_node_guard_tokenize.sh",
    "tests/smoke-megatron-e2e.sh",
    "tests/smoke-target-shim.sh",
    "tests/test-backlog-triage.sh",
    "tests/test-worktree-inside-checkout-redirect.sh",
    "tests/test_emit_gate_transition.sh",
    "tests/test_provider_substrate_e2e.sh",
];

/// Machine constants the smoke runner pins (from smoke_constants.json).
/// The exit-code trio is i32: they are process exit codes, and the runners
/// return i32; the goldens test casts them up for the captured-JSON read.
pub(super) const TAIL_LINES: usize = 40;
pub(super) const AMBIENT_MODES: &[&str] = &["clean", "dirty", "both"];
pub(super) const STATE_MODES: &[&str] = &["clean", "populated", "both"];
pub(super) const STATE_CONTROL_TEST: &str = "test_state_canary_detects_populated_state";
pub(super) const SMOKE_FAILURE_RECORD_DEFAULT: &str = ".fno/preflight-last-failures.txt";
pub(super) const CHANGED_RC_UNSELECTED: i32 = 20;
pub(super) const CHANGED_RC_UNEVALUATED: i32 = 21;
pub(super) const CHANGED_RC_PREREQ: i32 = 22;
pub(super) const CHANGED_RECEIPT_DEFAULT: &str = ".fno/changed-last-receipt.json";
pub(super) const INFRA_EXACT: &[&str] = &[
    "cli/src/fno/test_cmd.py",
    "scripts/ci/preflight.sh",
    "cli/pyproject.toml",
];
pub(super) const INFRA_PREFIX: &[&str] = &[".github/workflows/"];
pub(super) const INFRA_BASENAME: &[&str] = &["conftest.py"];
pub(super) const INFRA_SELECTS: &[&str] = &[
    "cli/tests/unit/test_test_cmd.py",
    "tests/ci/test_smoke_modes.sh",
    "tests/ci/test_preflight.sh",
    "tests/ci/test_changed_smoke_workflow.sh",
];
pub(super) const ALWAYS_LINTS: &[(&str, &str)] = &[
    ("placement rule", "scripts/ci/check-placement-rule.sh"),
    (
        "No stale /spec refs (blueprint rename audit)",
        "scripts/ci/check-no-stale-spec-refs.sh",
    ),
    (
        "Skill bundles freshness check",
        "scripts/lint/check-skill-bundles-fresh.sh",
    ),
    ("Hook exec bits", "scripts/ci/check-hook-exec-bit.sh"),
];
pub(super) const BUNDLE_TEST_TARGETS: &[&str] = &[
    "cli/tests/unit/test_skill_bundles.py",
    "cli/tests/unit/test_bundle_cli.py",
];
pub(super) const ORCHESTRATED_SUBTREES: &[(&str, &str)] = &[("cli/tests/smoke/", "Smoke tests")];
pub(super) const RUST_BIN_MARKERS: &[&str] = &[
    "target/debug/fno-agents",
    "target/release/fno-agents",
    "FNO_AGENTS_BIN",
];
pub(super) const RUST_BUILD_STEP: &str = "Build fno-agents debug binary (for journey tests)";
pub(super) const CLAIM_DOOR_NAME: &str = "fno-agents-claim-door";
pub(super) const TARGET_BIN_RELS: &[&str] = &[
    "crates/fno-agents/target/debug/fno-agents",
    "crates/fno-agents/target/release/fno-agents",
];
pub(super) const HARNESS_GLOBS: &[&str] = &[
    "tests/*.sh",
    "tests/hooks/*.sh",
    "tests/lib/*.sh",
    "tests/target/*.sh",
    "tests/skills/*.sh",
    "tests/events/*.sh",
    "tests/lint/*.sh",
    "scripts/tests/*.sh",
    "skills/*/tests/*.sh",
];
pub(super) const CENSUS_TRANCHE_BOUND_S: usize = 60;
pub(super) const CENSUS_KILL_BOUND_S_DEFAULT: usize = 300;
