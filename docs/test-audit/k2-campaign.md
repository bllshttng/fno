# K2 campaign ledger: spawn and dispatch

## Second pass, 2026-10-18

Base 06b3f4efb6 (origin/main at the start of the pass). Scope: the 158 files `campaign-map.tsv` keys K2. Eight of them were gone before the base, so 150 files hold tests. The pass deleted two more whole files.

| Measure | Before | After | Ratio |
|---|---:|---:|---:|
| Python declarations | 1,663 | 1,188 | 0.71 |
| Rust declarations | 462 | 374 | 0.81 |
| All declarations | 2,125 | 1,562 | 0.74 |
| Python collected cases | 1,908 | 1,244 | 0.65 |
| Rust collected cases | unmeasured | unmeasured | none |

Both Python collections ran `pytest --collect-only -q` over the K2 Python files and exited 0. The before-collection read a `git archive` export of the base, so no edit in flight could touch it. Rust collection is unmeasured: `build.agent_freeze` bars local cargo, so CI is the first compile and the first run of every Rust edit.

The pass does not reach half. Python cases land at 0.65 and Python declarations at 0.71. Rust lands at 0.81. The Rust gate and slot files were already folded into per-branch row tests by an earlier pass. What is left in them is mostly one guard per fail-closed branch, and the keep rule keeps those.

### Method

Nine auditors each took one disjoint group of files: the pane spawn family, think and footprint, route and capture, dispatch, gate and seed, spawn axes and lanes, mux and pane send, and two Rust groups. Each deleted tests that repeat a branch a kept test reaches, and folded per-value rows into one row per distinct branch. Each kept every sole guard of a fail-closed path. Each ran its own files after the edit. The Python pane spawn back half is live: the Rust client re-execs the Python front for pane spawns, `--account` spawns and unmarked spawns. So no completed-port deletion was open there.

### Production code retired

Each symbol had no caller outside tests. The proof is a search over cli/src, hooks, scripts, skills, crates, commands and agents that finds nothing after the delete.

- `spawn_with_provider_snapshot`, `_default_settings_path` and `FNO_PROVIDER_ENV_KEYS` in cli/src/fno/adapters/providers/dispatch.py. The K1 ledger named the first one as dead and left it for this campaign.
- `available_providers` in cli/src/fno/agents/dispatch.py.
- The review arm of `_mux_pane_send` (`review`, `review_invocation_id`, `origin`, `self_send` and the code behind them). Its only caller was the raw-send leaf, which moved to Rust. The `fno_review_invocation_id` row left `scripts/ci/reachable-paths-baseline.txt` with it.
- `mux_server_env` and its re-export from mux_spawn.py.
- The no-child-pid fallback of `_backfill_codex_session_id`, with `discover.codex_session_ids_started_in` and `discover._codex_started_ms`. Both production callers pass a child pid.
- `think_spawn_on_decompose_wave0`, `on_node_work_start`, `_on_node_lifecycle` and `_subflag_on` in cli/src/fno/provenance/spawn_think.py.
- `LiveCensus.slot_count` and its `fno_slot_workers` feeder in cli/src/fno/agents/spawn_gate.py, and `should_emit_spawn_cap` in cli/src/fno/events/gate_escape.py. The spawn gate dual retired to Rust, so `test_spawn_gate_agreement.py` held a Python mirror equal to the owner (principle 9). The harness and the mirror went together. The Rust tests read their fixtures alone, and their doc comments now say so.

### Preservation spot-check

Owed. A fleet-incident order (2026-10-18, machine overloaded) stopped every local run before the mutations ran. The five mutations are ready. Each names the keeper it turns red.

| Keeper | Mutation |
|---|---|
| `test_mux_inject.py::test_held_writer_claim_past_the_wait_demotes_without_pasting` | agents/dispatch.py `_mux_pane_send`: the `return False` after the held-claim print becomes `pass` |
| `test_thread_identity_from_spawn_record.py::test_duplicate_cwd_refuses_rather_than_picks` | harness_identity.py `live_thread_row_for_cwd`: `if len(matches) == 1:` becomes `if matches:` |
| `test_harness_map_dispatch_command.py::test_non_boolean_auto_merge_never_grants_merge[1]` | harness_map.py `resolve_dispatch`: `cfg.get("auto_merge") is True` becomes `== True` |
| `test_spawn_permission_mode.py::test_mutual_exclusion_yolo_and_permission_mode` | agents/cli.py: `if permission_mode is not None and yolo:` becomes `if False:` |
| `spawn_gate::tests::lead_share_refuses_when_the_caller_holds_its_full_share` | spawn_gate_lanes.rs `check_lead_share`: `if held < share {` becomes `if held <= share {` |

Every group ran its own files after its edits. Most runs needed `FNO_AGENTS_BIN` pointed at the installed binary, because this checkout has no dev build. Without it, tests that stub PATH cannot find the binary and fail the same way on the untouched base. Every run exited 0 but one: the route and capture group ran without the pin, and five untouched tests failed there on the missing binary. A whole-scope run was killed by the overload at 3 percent. CI decides both.

### Open items for the lead

These were found and not fixed, because each one reaches past this campaign's files or needs a ruling.

- `maybe_spawn_think` has unreachable branches now that `dispatch_conversational` is its only production caller: the unforced-birth guard, the design-warranting gate, the per-run cap, the relevance filter and the attended-offer branch. One sole guard of each stays until the branches retire.
- The config leaf `think_spawn.on_work_start` has no reader. `scripts/ci/retired-config-leaves.txt` says to keep it, but nothing wires it.
- `_backfill_codex_session_id` and `_make_codex_bind_probe` still take `cwd`, `since_ms` and `sessions_dir`, now unread. The plumbing runs through doctor, peek and other campaigns' tests.
- Python `reconcile_agents` is a full second implementation of the Rust `liveness_sweep.rs` reconcile, used as the no-binary fallback. The dual-implementation inventory does not list it.
- The route, admission and tier-remap guards exist twice, in agents/dispatch.py and in mux_spawn.py. Each copy keeps its own test row until one leg goes.
- No production caller passes `guarded=True` to `_mux_pane_send`, though it is the default. The Python `mail pane-prepare` forward looks unreachable behind the Rust front door.
- `FailoverController` is dead but `test_failover.py` (K4) still imports it. `read_active_provider_atomic` is now called only from `test_loader.py`.
- The Rust raw-send lane has no twin of the codex `/review` queue classification the retired Python review arm held.
- Stale comments in other campaigns' files: `cli/tests/agents/test_discover.py:2192` names a test that does not exist, and `cli/tests/unit/test_keeper_lane.py:377` says test_spawn_pane.py builds the keeper binary.
- `campaign-map.tsv` still lists the ten K2 files that are gone.

### Files

One row per K2 file. Deleted files read 0 after.

| File | Before | After | Contracts kept |
|---|---:|---:|---|
| `cli/src/fno/adapters/providers/test_dispatch.py` | 27 | 15 | Claude and HOME oauth env, api_key keychain argv, route env from repo settings, unknown provider, unstaged oauth and unresolvable keychain refusals, each `resolve_env_value` branch, config_dir pin wins, identity-mismatch refusal. The snapshot tests went with the retired symbol. |
| `cli/src/fno/test_dispatch_flags.py` | 14 | 6 | Explicit beats marker, empty `--harness` refused, ambiguous markers fall to builtin, inferred branch, empty `--model` refused, None and exact `--model` pass through. Marker rows live in test_harness_identity.py. |
| `cli/src/fno/test_dispatch_target.py` | 8 | 6 | Per-agent pin, env combo, unknown combo falls through, `accounts` spelling, no settings is unresolved, global active_combo fallback. |
| `cli/tests/agents/test_agy_spawn_journey.py` | 2 | 1 | The spawn seam journey. The test of the file's own skip logic is gone. |
| `cli/tests/agents/test_ask_e2e_dispatch.py` | 5 | 4 | Python vs Rust once parity, short flags through the binary, failure propagates, follow-up through a shared registry. Unknown agent 16 lives in spawn_routing.rs. |
| `cli/tests/agents/test_autonomous_dispatch_census.py` | 4 | 3 | Blueprint dispatches nothing, deprecated-key migration text, think substrate kept for compatibility. |
| `cli/tests/agents/test_business_role_launch_journeys.py` | 4 | 4 | Unchanged: one refusal per lane, each a separate code path. |
| `cli/tests/agents/test_codex_fatal_error_dispatch.py` | 4 | 3 | Business-role refusal exit 2, invocation error exit 1 with event and no row, no-session exit 11 with event. |
| `cli/tests/agents/test_codex_pane_daemon.py` | 11 | 10 | Every daemon branch but one repeat. |
| `cli/tests/agents/test_codex_thread_effort_journey.py` | 1 | 1 | Unchanged: live journey. |
| `cli/tests/agents/test_dispatch.py` | 12 | 6 | `which` hit, select_provider: registered, mismatch refusal, new agent needs provider, unknown refused, retired gemini row skipped. |
| `cli/tests/agents/test_dispatch_attribution.py` | 2 | 2 | Env-corrupting name refused (one row per branch), spawn create events share request_id and from_name. |
| `cli/tests/agents/test_dispatch_codex.py` | 19 | 12 | Create registers row and effort, identity collision exit 12 detaches the lock, provider exit code propagates, timeout 15, yolo in events, claude yolo maps to bypassPermissions, from_name validator, assign-project argv and fail-open, thread spawn forwards `--node`, sealed spawn refuses a stale binary. |
| `cli/tests/agents/test_dispatch_harness_stage_table.py` | 13 | 9 | Agreeing keys give no note, unset is None, verb picks profile, the door's stage table, losing-key note, legacy-only key, route, empty route, both doors agree. |
| `cli/tests/agents/test_dispatch_lifecycle.py` | 36 | 31 | Reconcile: every claude and codex pane arm (pending, unconfirmed, liveness unknown, incarnation unreadable, dead pane orphans, heal, in-pass duplicate, under-lock race, terminal rows), logs probe flip, backfill, CLI missing, probe error, codex index errors, key schema. |
| `cli/tests/agents/test_dispatch_lifecycle_polish.py` | 3 | 1 | Reconcile apply keeps a concurrent `last_message_at`. |
| `cli/tests/agents/test_dispatch_spawn.py` | 33 | 27 | Once teardown, plugin-command normalization, collision refusal, teardown failure, claude receipt bytes and cwd escaping, codex no-runtime 87, unknown harness 2, vendor-axis hint, pi, agy and keeper lanes, carrier demotion, absent-stance refusal, opencode serve lane and refusals, spawn-trigger scrub. |
| `cli/tests/agents/test_dispatch_workdir.py` | 8 | 6 | `--cwd` wins, default goes to canonical with a note, `--here`, empty cwd is absent, caller-is-canonical no-op, resolution failure falls back. |
| `cli/tests/agents/test_gate_escape_spawn_cap.py` | 9 | 3 | Distinct dedup keys count separately, fail-open on an unwritable log, placeholder pr normalized. The parity row went with `should_emit_spawn_cap`. |
| `cli/tests/agents/test_grok_spawn_journey.py` | 1 | 1 | Unchanged: live journey. |
| `cli/tests/agents/test_harness_map_dispatch_command.py` | 54 | 28 | Builtin command per surface, gemini refusal at both doors, config and explicit rungs, `/fno:` canonicalization, missing `{id}`, autonomous pane rules, thread lane and retired `bg`, codex verb rewrite, auto_merge `is True` identity, lifecycle doors, stage-table profile. |
| `cli/tests/agents/test_keeper_thread_routing.py` | 6 | 5 | Roster matches the capability table, grok diverts, glued `-Hgrok`, non-diverting cases, fenced tokens. |
| `cli/tests/agents/test_lane_b_thread_spawn.py` | 28 | 22 | Agy keeper seat, gemini thread refusal, lane A refused, pi and grok argv, socket and symlinked home, collision, exit 13 and 12, Identify mismatch, real keeper journey, cursor-agent and agy mint rules, frame decoder, modal before the marker. |
| `cli/tests/agents/test_mux_inject.py` | 23 | 13 | Undeclared harness paste plus CR, guarded paste shape, unguarded claim sequence, claim fail-open, held claim waited and demoted, held-claim marker pin, submit delay, unpinned submit refused, five codex read-back outcomes, control.sock delay, no own audit row, mux tried before the daemon RPC. |
| `cli/tests/agents/test_mux_server_axis.py` | 9 | 7 | Flag beats both envs, FNO_SERVER beats FNO_SESSION, one warning, empty env is unset, dedup treats one axis, session-state.sh passes `bash -n`, alias refusal. |
| `cli/tests/agents/test_pane_binding_receipt.py` | 66 | 48 | Binding outcomes (bound, died with tail, still-booting, zero window, death on tick 2), best-effort evidence, unbound reason resolution, every `_submit_spawn_seed` arm, `_mux_pane_alive` tri-state and bad enumerations, pid path never falls back, daemon-candidate and bind diagnostics, refusals never route to `--codex-bind`. |
| `cli/tests/agents/test_pane_identity_crosscheck.py` | 14 | 11 | stamp_failure emit, both directions with their skips, spare rules, counts rendered, transitive process tree. |
| `cli/tests/agents/test_pane_send_audit.py` | 3 | 1 | `--source` rides the paste only, never the CR. |
| `cli/tests/agents/test_pi_spawn_journey.py` | 1 | 1 | Unchanged: live journey. |
| `cli/tests/agents/test_reconcile_mux_ref_heal.py` | 6 | 6 | Same tests; one repeated parametrize row dropped. |
| `cli/tests/agents/test_spawn_account_flag.py` | 14 | 11 | `--dispatch-account` threads, proven carrier overrides, unreadable carrier refused, non-claude refused, account plus route, composed receipts, codex thread sealing, codex one-shot refuses a pinned account. |
| `cli/tests/agents/test_spawn_codex_session_capture.py` | 35 | 17 | Pid oracle (metadata id, unrelated file, descendants, double open, id set gives None), daemon accepts one new id, `_codex_session_ids_loaded`, bind probe repeat and restart, rollout witness and its four fail-closed branches. |
| `cli/tests/agents/test_spawn_defaults.py` | 4 | 3 | Manual node name mint, retired `bg` refusal, real-binary compose end to end. |
| `cli/tests/agents/test_spawn_effort.py` | 16 | 11 | Agy and claude or codex pane effort, opencode variant degrade and cleanup, `_build_argv`, `codex.create`, claude headless, CLI effort to pane and bg. |
| `cli/tests/agents/test_spawn_existing_pane.py` | 5 | 4 | Existing pane keeps its id through a guarded send, `--pane` threads, missing vs occupied target, `--pane` with `--split` refused. |
| `cli/tests/agents/test_spawn_flag_passthrough.py` | 18 | 15 | `thread_uncarried` arms, loud demotion message, carried flags stay, seed as prompt, codex `--agent` refused, `-c` refusals, operator template, gemini effort refusal, fenced tokens, duplicate refusal. |
| `cli/tests/agents/test_spawn_fno_node_export.py` | 7 | 6 | Node export, scoped to the dispatch, nodeless exports nothing, real resolver yields nothing, provenance keys move together, inherited worker name overwritten. |
| `cli/tests/agents/test_spawn_gate.py` | 35 | 17 | `_load_snapshot` None on unreadable load, `_pid_alive` start-token rules, census includes roster rows, unreadable incarnation stays counted, admitted answer becomes a guard, refused answer raises with one event, probe on an unanswered gate reads unknown, exit 87 refuses gate_unavailable, QoS wrap, unlinked-parent notice. |
| `cli/tests/agents/test_spawn_gate_agreement.py` | 5 | 0 | Deleted. It held the Python census slot count equal to the Rust gate (principle 9). The Python leg is retired; the Rust test reads the fixture alone. |
| `cli/tests/agents/test_spawn_gate_refusal_events.py` | 6 | 3 | `_refuse` merges the receipt into the journal event, receipt None still emits, an unwritable journal never changes the refusal. |
| `cli/tests/agents/test_spawn_gate_wiring.py` | 8 | 6 | Thread spawn gates as `bg` and releases, `--once` gates as headless, flags reach the gate, refusal exit propagates, sandbox refusal comes before the gate, pane gates as pane. |
| `cli/tests/agents/test_spawn_lost_verb.py` | 6 | 5 | Real-shell rows (bash colon, zsh dropped letter, mid-sentence), message names verb and fix, intact rows, target family with an empty roster, refusal before the Rust route. |
| `cli/tests/agents/test_spawn_model_substitution.py` | 3 | 3 | Unchanged: substitution named, no-sample labels the request, a match stays quiet. |
| `cli/tests/agents/test_spawn_node_seed.py` | 30 | 20 | Node seed carries verb and brief, unencoded node refused before any pane, message plus node, `--mux-session`, account stamp, worktree ensure and its hold, seam unavailability exits 2, derive arm, refused spawn reaches no lane. |
| `cli/tests/agents/test_spawn_normalize.py` | 52 | 33 | Pass-through cases, autogen collisions, `-r` widening, five resume and substrate refusals, positional rules, AST pin of `_SPAWN_VALUE_FLAGS`, `extract_existing_pane`, fence handling, node mint. |
| `cli/tests/agents/test_spawn_opencode_session_capture.py` | 11 | 6 | Banner lines are not ids, exact directory and time SQL with escaping, ambiguity gives None, bounded retry, OSError never raises, clean-empty vs failure. |
| `cli/tests/agents/test_spawn_pane.py` | 174 | 142 | Opencode capture and racing panes, codex binding backfill, claude happy path, mux failure leaves no row, collision before any pane, `-p` billing guard, argv builder per provider, undeclared-harness refusals and journey, env identity scrub, placement (fit, max-panes, tab id, lease loser, overflow, group failure), readiness, permission prompts, reap paths, happy monitor branches, seed outcomes, six unanswered `pane run` branches. |
| `cli/tests/agents/test_spawn_pane_codex_receipt.py` | 1 | 1 | Unchanged: unbound codex pane exits 1, reaps it, emits the uncaptured event. |
| `cli/tests/agents/test_spawn_pane_codex_route.py` | 1 | 1 | Unchanged: codex successor uses bounded dispatch with the openai, account and model axes. |
| `cli/tests/agents/test_spawn_pane_codex_seed.py` | 8 | 8 | Unchanged: turn-start seed after bind, yolo argv seed, delivery fallbacks, late bind, unbound, dead pane, `deliver_seed` shell. |
| `cli/tests/agents/test_spawn_pane_node_provenance.py` | 1 | 1 | Unchanged: `--node` resolves provenance with the claim holder. |
| `cli/tests/agents/test_spawn_pane_placement_verify.py` | 4 | 4 | Unchanged: absent row, empty listing, one re-list, receipt disagreement. |
| `cli/tests/agents/test_spawn_parent_edge.py` | 24 | 11 | Row and event carry the edge, trigger stamp and no leak, codex create stamps edge, adopted row stamps adopted_by only, no self-stamp, lineage_reason, process walk, a contradicted marker is not overturned, resume inherits the edge. |
| `cli/tests/agents/test_spawn_payload.py` | 5 | 5 | Unchanged: bg marker, opencode pane marker, constants equal schemas/spawn-brevity.json, idempotent, marker mention. |
| `cli/tests/agents/test_spawn_permission_mode.py` | 24 | 19 | Bridge rows Rust does not own (gemini plan, opencode auto, opencode and empty refusals), codex refusal names the key, pane defaults, mutual exclusion and `--resume` refusals, receipts, `_build_argv`, spawn_think branches, spawn.sh forwarding. |
| `cli/tests/agents/test_spawn_portal.py` | 7 | 4 | `--portal` not advertised on the Python lane, placement on a thread refused, bounded placement refused, bare spawn in a mux seats a thread. |
| `cli/tests/agents/test_spawn_provider_optional.py` | 8 | 5 | Builtin harness with absent provider and model, exit 22 on unverified seed and unreadable pane, blank pane exits 0, empty `--model` refused. |
| `cli/tests/agents/test_spawn_request.py` | 8 | 6 | Session record shape, daemon origin needs an owner, retired `sob`, unknown cause, carrier env beats ambient, half carrier refuses. |
| `cli/tests/agents/test_spawn_revive.py` | 20 | 17 | Revive in place, uuid mismatch collision, rename on resume, explicit `--node`, axes carried, held session claim exits 11, writer-claim lifetime rows, wake takes no outer claim, handed parent stamped. |
| `cli/tests/agents/test_spawn_role_routing.py` | 7 | 4 | `--promote` keeps a thread spawn in Python, `_has_flag` attached-short forms, long-only detectors and the `--argv` boundary, pane detector stops at the fence. |
| `cli/tests/agents/test_spawn_role_wiring.py` | 16 | 12 | Role route reaches create, pane composes the route, each duplicated guard keeps one worker row and one pane row, single-use and forged admission, adapter scrub floor, resolve-once, `--route` under the managed marker. |
| `cli/tests/agents/test_spawn_route_override.py` | 43 | 31 | Codex thread lane argv and overlay, codex `--resume` refused, non-claude refusal names `--route`, pane and bg receipts carry three axes, `route_on_pane` fail-closed, `--print` transport, axis-name refusals, one composition across seams (the account token loses). |
| `cli/tests/agents/test_spawn_seed_receipt.py` | 4 | 3 | Empty intent names the state path, receipt reads live once the prompt is recorded, unrecorded prompt reads unverified. |
| `cli/tests/agents/test_spawn_seedless_refusal.py` | 6 | 5 | Seed or role not refused, only claude thread judged, seam refuses an explicit thread, `--resume` left alone, unreadable row refuses before dispatch. |
| `cli/tests/agents/test_spawn_session_row.py` | 26 | 18 | Review label refuses 89, prose plus node composes, duplicate stamp keeps one row, pending row, unlabelable verb fails closed, phase rules, roster order, reap-open, merge grant rows, think row. |
| `cli/tests/agents/test_spawn_unfireable_verb.py` | 15 | 12 | Wrong-channel refusal, absolute path never judged, unreadable codex state fails open, marker rows, seam refusal, render_seed, receipt fields, unresolvable word names the nearest, unreadable graph fails open. |
| `cli/tests/agents/test_spawn_uuid_capture.py` | 7 | 6 | Retry sleeps only between probes, bounded exhaustion, resolver raise gives None, empty short-id never probed, CLI persists the uuid, a miss still launches. |
| `cli/tests/agents/test_spawn_wait.py` | 5 | 4 | Waitable reason retries, other reasons exit at once, `--wait` with `--no-wait` refused, duration parsing. |
| `cli/tests/agents/test_thread_keeper_journey.py` | 1 | 1 | Unchanged: live journey. |
| `cli/tests/agents/test_thread_reference.py` | 2 | 2 | Unchanged. |
| `cli/tests/integration/test_lane_slots_concurrency.py` | 2 | 1 | The cap holds under the race (three trials for detection). |
| `cli/tests/integration/test_native_lane_bundles.py` | 4 | 4 | Unchanged: the only end-to-end guards of `profiles.*.lanes[].args`. |
| `cli/tests/integration/test_undispatched_order.py` | 4 | 2 | Queue order equals drain order, id breaks ties. |
| `cli/tests/test_mux_shell_integration.py` | 6 | 2 | One table with one row per coercer branch plus the default, non-mapping block degrades. |
| `cli/tests/test_short_flag_dispatch.py` | 3 | 3 | `doctor event emit` registers, backlog find and accounts add short-vs-long parity. |
| `cli/tests/test_spawn_flag_owners_lint.py` | 3 | 3 | Unchanged: live tree passes, a removed row fails by name, a stale row fails. |
| `cli/tests/test_spawn_guard.py` | 26 | 18 | Dispatchable takes the reservation, bareword line parses, live claim, corrupted verdict, suspect claim not stolen, racing reservation, `--no-reserve`, guard before the gate, substrate failure releases, probe crash and malformed TTL fail closed, manifest-bound states, authority outage reads unknown. |
| `cli/tests/test_two_spelling_dispatch.py` | 2 | 2 | Worker review registers, legacy spelling hidden in help. |
| `cli/tests/unit/test_agent_spawned_log.py` | 9 | 5 | Daemon log path, envelope shape, resume coordinates, spawn_failed row, pane birth joins death on pid. |
| `cli/tests/unit/test_config_process_admission.py` | 2 | 2 | Unchanged: default 400 independent of `agents.max_live`, bad values coerce to the default. |
| `cli/tests/unit/test_dispatch_barrier.py` | 37 | 32 | One dispatchable when two run at once, peer seen after acquire, no-reserve, ordering, the full handover set, early exits ahead of the guard, once releases and pane holds, release warning. |
| `cli/tests/unit/test_dispatch_mux_send.py` | 19 | 17 | Envelope wrap and id, raw send verbatim and never reads the pane, prompt, detector and OSError refusals, undeclared-harness note, no double wrap, `--raw` verbs, rebound thread, confirm through Rust mail-inject. |
| `cli/tests/unit/test_dispatch_no_merge_env.py` | 17 | 16 | All four `apply_merge_posture_env` branches, guard-flag boundary, resolver posture rows (no-merge, namespaced, present, prose, allow, config, unknown value refused). |
| `cli/tests/unit/test_dispatch_settings_path.py` | 6 | 0 | Deleted. It tested `_default_settings_path`, retired with `spawn_with_provider_snapshot`. test_route_survival.py covers the restore path. |
| `cli/tests/unit/test_dispatch_undeclared_repo.py` | 11 | 10 | Foreign-repo pin, target's own policy, own worktree, outside git, env override, pane wrapper set and clear, seam pin, silent seam, exec export, ambient override. |
| `cli/tests/unit/test_dispatch_verb_registry.py` | 15 | 11 | Config parse, builtins dropped, native spelling, fno-qualified form, undeclared harness refused, scalar invocation, missing skill refused, probe argv, bare allowlist, neither-surface refusal, takes_node_id false. |
| `cli/tests/unit/test_footprint.py` | 31 | 18 | Transitive attribution, detached and excluded roots, pid 1 root, legacy format, python plus fno-py arm, `is_test_runner` table, `top_consumers`, spare-pool decoys, bad-row reasons, masking and caps. |
| `cli/tests/unit/test_footprint_verb.py` | 100 | 74 | Each fail-closed branch of `_live_root_pids`, each gap branch, codex and opencode serve oracles, `cause_reading` refusal arms, exit precedence 3 over 5 then 4, roster degrade, bounded output, masking, capacity axes, shared Rust and Python admission fixture, census counts. |
| `cli/tests/unit/test_inbox_store_threads.py` | 23 | 16 | Thread write and append, slug and date rules, find by reply id, unread filter, optional fields round trip, `inbox_dir_for` refusals, read_at reset, concurrent writers, best-effort render, Kind and deprecated-kind pins, resolve_project. |
| `cli/tests/unit/test_lane_experiment_contract.py` | 4 | 4 | Unchanged. |
| `cli/tests/unit/test_launch_pick.py` | 19 | 17 | Headroom pick with receipt, every seam, routed and role skips, quota death, verb parity, defer. |
| `cli/tests/unit/test_mail_name_lane_reply.py` | 20 | 18 | Every routing and refusal branch. |
| `cli/tests/unit/test_managed_slot_blob.py` | 12 | 6 | Residue is not a login, refresh token alone is, top-level access token, empty is not, default slot falls through scoped residue, alternate dir never falls to the default. |
| `cli/tests/unit/test_mission_not_dispatchable.py` | 1 | 1 | Unchanged. |
| `cli/tests/unit/test_mux_pane_guarded.py` | 16 | 12 | Reap refusals and recycled-pid confirm, not-idle paste stalls, Rust confirm lane, not-delivered reasons, timeout never retries, claim timeout releases, no transcript fails closed, occupant mismatch refuses, `--fno-id`, non-claude confirm verdict. |
| `cli/tests/unit/test_org_spelling_dispatch.py` | 2 | 2 | Unchanged. |
| `cli/tests/unit/test_pane_prepare_bridge.py` | 3 | 2 | Bridge verb and flags declared by the Python forward, conditional `--style-exception`. |
| `cli/tests/unit/test_spawn_account_provenance.py` | 9 | 9 | Unchanged: bg sources, pane and bg receipts, pane row decider, Rust carrier byte pin. |
| `cli/tests/unit/test_spawn_defaults.py` | 14 | 13 | `claim_store_writable` tri-state, `is_verb_seed`, `canonical_verb_key`, lane vendor `argv_head`, transport pass-through, scan projection, exit 78 payload, events, composed argv, degrade open. |
| `cli/tests/unit/test_spawn_phase.py` | 5 | 3 | Verb table mapping, unlabeled branches, `_stage_profile` canonical key. |
| `cli/tests/unit/test_spawn_sandbox_settings.py` | 3 | 3 | Unchanged. |
| `cli/tests/unit/test_spawn_think.py` | 101 | 61 | `dispatch_conversational` pointer, forced spawn, no-origin skip, dedup; spawn-tail failures; daily cap; bulk-intake skip; one sole guard per still-reachable `maybe_spawn_think` branch; presence precedence; output-path reuse order; substrate; pane and headless receipts; `on_node_born` gate and door argv; naming; config coercion; `_parse_short_id`. |
| `cli/tests/unit/test_think_dispatch_cli.py` | 14 | 11 | No live session exit 2, node not found, empty `--model`, non-claude provider refused, ambient codex posture, live pointer and cwd, overlays, skipped exits 1, real spawn journey. |
| `cli/tests/unit/test_thread_identity_from_spawn_record.py` | 22 | 15 | Sibling cwd row, duplicate cwd refuses, exited rows own nothing, remote codex row, unidentified rows never answer, absent and unreadable registry give None, symlinked worktree, proven identity short-circuits, contradiction stays ambiguous, cross-family markers fail closed, verb end to end. |
| `crates/fno-agents/src/agy_launch.rs` | 6 | 6 | Unchanged. |
| `crates/fno-agents/src/bounded_spawn.rs` | 5 | 3 | Classifier, no unretried `.spawn()` static guard, stable error ends the retries. |
| `crates/fno-agents/src/daemon/tests/codex_thread_lane.rs` | 45 | 35 | One `plan_reconcile` table (one row per branch), entry posture stamps, pane-row ask refusal, gone-cwd refusal, five thread-spawn refusals, unmapped stop and unsettled interrupt, switchboard acks, episode gate, recovery config, launch cwd and both ensure refusals. |
| `crates/fno-agents/src/lane_heal.rs` | 8 | 8 | Unchanged: one test per verdict branch. |
| `crates/fno-agents/src/lanes.rs` | 6 | 6 | Unchanged: one test per branch. |
| `crates/fno-agents/src/launch_workdir.rs` | 5 | 5 | Unchanged: the only real guards, Python mocks this verb. |
| `crates/fno-agents/src/loop_dispatch.rs` | 16 | 9 | ETXTBSY retry bound, pick declined by env and ambient export, clear-list as empty values, last stderr line, unparseable output declined, fixed wrapper binaries. |
| `crates/fno-agents/src/pane_keeper.rs` | 9 | 8 | Frame codec, NeedMore, unknown tag is a Violation, ring cap, escape cut, session_id_from_argv, full-UUID rule. |
| `crates/fno-agents/src/pane_relaunch.rs` | 4 | 4 | Unchanged. |
| `crates/fno-agents/src/pane_stop.rs` | 24 | 20 | Kill confirmed by child pid, SIGTERM-only, unconfirmed survivor, pidless and foreign-pid refusals, NotHeld vs Unmeasured, recycled pid, unproven incarnation never signalled, `holder_from_lsof` table. |
| `crates/fno-agents/src/planning_lane.rs` | 2 | 2 | Unchanged. |
| `crates/fno-agents/src/posture_self_lane_tests.rs` | 4 | 3 | GitHub App review clears the default self lane, `declare` fails, non-Reviewed verdicts fail. |
| `crates/fno-agents/src/route_gather.rs` | 5 | 5 | Unchanged. |
| `crates/fno-agents/src/route_slot.rs` | 26 | 25 | Journal rows, audit flags, grid, tier, evidence, policy, lane-config, capacity, strict-pin and filter row tests, audit boundaries, since parser, loader, refresh-gate cases. |
| `crates/fno-agents/src/route_slot_capacity_tests.rs` | 10 | 9 | States-mode refresh, failing refresh fails open, explicit capacity never probes, all-unknown refuses, full lane skipped, unreadable registry refuses, explicit vendor caps. |
| `crates/fno-agents/src/slot_cutover.rs` | 9 | 8 | Project-only target and source, stale target, identity and cooldown fail closed, threshold gate, use failure, disabled arm, health notify. |
| `crates/fno-agents/src/spawn_axes.rs` | 12 | 10 | repr quoting, reentry refusal, route skip reasons, account skip, route owns the model, home-scoped model, degrade, bg maps to thread. |
| `crates/fno-agents/src/spawn_compose.rs` | 2 | 2 | Unchanged. |
| `crates/fno-agents/src/spawn_context.rs` | 10 | 9 | Dotted harness stem, identity rows, witness set, pool classifier, session pid, spare refusal, shell degrade, ambient stamp, harness walk. |
| `crates/fno-agents/src/spawn_contract.rs` | 13 | 8 | Session compat triple, blank id Malformed, wrong-harness id shape, sob retired, owner validity, Python round-trips. |
| `crates/fno-agents/src/spawn_edge.rs` | 11 | 8 | lineage_kind table, live child, peer never holds, exited releases, teamed parent, stamps, birth_event. |
| `crates/fno-agents/src/spawn_gate.rs` | 28 | 24 | Review-session refusal, verdict wire format, RAM floor and swap, parsers, fleet-stop and brake refusals, corrupt incident record, state-root grant, spawn-cap and CPU parity fixtures, unreadable CPU instrument refuses, busy mutex, walled lane, lead-share refusal, qos wrap, territory, blueprint cap. |
| `crates/fno-agents/src/spawn_gate_blind_tests.rs` | 6 | 6 | Unchanged: the unreadable instrument never admits through the full re-read budget. |
| `crates/fno-agents/src/spawn_gate_lanes.rs` | 27 | 23 | Provider count rules, awaiting-operator table, parks and their dedup, unreadable journals and registry refuse, suspect claims count, reservations, lead share, schema floor, quota locks, lane-cap coercion table, login lane rules. |
| `crates/fno-agents/src/spawn_gate_reservations.rs` | 3 | 3 | Unchanged. |
| `crates/fno-agents/src/spawn_gate_slot_tests.rs` | 19 | 14 | Succession admits and plain spawn refuses, `caller_not_sole_holder`, `no_caller`, cap+1, ambiguity, lead row takes no slot, learned cap, burst, release remedy, dead holder frees, live holder keeps. |
| `crates/fno-agents/src/spawn_gate_territory_tests.rs` | 1 | 1 | Unchanged: unreadable claims refuse as territory_unknown. |
| `crates/fno-agents/src/spawn_gate_verb.rs` | 22 | 18 | Probe mirrors schema refusal, walled lane, slot rows, fleet stop, reservations, lead share, burst cap; `hold: false` releases; missing holder pid refuses; review seed refuses before the bypass; reserve rules. |
| `crates/fno-agents/src/spawn_overlay.rs` | 10 | 10 | Unchanged: already per-surface tables plus distinct print branches. |
| `crates/fno-agents/src/spawn_payload.rs` | 4 | 3 | Empty and idempotent, incidental marker appends, cross-language fixture pin. |
| `crates/fno-agents/src/spawn_phase.rs` | 2 | 2 | Unchanged. |
| `crates/fno-agents/src/subprocess_ask.rs` | 1 | 1 | Unchanged: kill_pgrp refuses its own group. |
| `crates/fno-agents/src/team_spawn.rs` | 4 | 4 | Unchanged. |
| `crates/fno-agents/src/wake_name.rs` | 4 | 4 | Unchanged. |
| `crates/fno-agents/tests/claude_ask_dispatch.rs` | 18 | 15 | Validation table, unknown name 16 creates nothing, seed-unverified receipt, Python-readable row, identity never guessed, yolo mapping, socket follow-up and null 13, poll timeout 15, flock, missing CLI 14, corrupt registry 12, short-id refusal. |
| `crates/fno-agents/tests/codex_ask_dispatch.rs` | 24 | 14 | Verb normalization, once-path journey, resume carries effort and stamps the row, exit 11 check order, empty ids fail closed, validate_inputs, soft-error promotion, mid-stream read error, timeout 15. |
| `crates/fno-agents/tests/convert_pane_to_thread.rs` | 4 | 2 | Strategy coherence, keeper rebind refuses without a keeper. |
| `crates/fno-agents/tests/lane_fill_rss_guard.rs` | 1 | 1 | Unchanged: sole RSS bound. |
| `crates/fno-agents/tests/loop_dispatch_census.rs` | 4 | 2 | Fixture titles count as zero, real from iteration 2, malformed lines skipped. |
| `crates/fno-agents/tests/loop_readiness_pre_launch.rs` | 1 | 1 | Unchanged. |
| `crates/fno-agents/tests/route_gather_parity.rs` | 1 | 1 | Unchanged: characterization with no other keeper. |
| `crates/fno-agents/tests/spawn_compose_parity.rs` | 1 | 1 | Unchanged golden. |
| `crates/fno-agents/tests/spawn_contract_journey.rs` | 6 | 5 | Accepted before birth, origin refusal has zero effects, launch failure releases everything, RecoveryRequired, admission refusal before the backend. |
| `crates/fno-agents/tests/spawn_lane_journey.rs` | 5 | 2 | Routed-lane table, refusal table. |
| `crates/fno-agents/tests/spawn_phase_table.rs` | 2 | 1 | Valid phase keys, no duplicate verb. The copy check lives in cli-ci.yml. |
| `crates/fno-agents/tests/spawn_routing.rs` | 32 | 23 | Unknown-name exits, bg receipt key order, collisions exit 2, headless timeout 124, permission-mode refusals, headless and thread lanes, daemon-owned receipt, first-worker bootstrap, retired `host` pointer. |
