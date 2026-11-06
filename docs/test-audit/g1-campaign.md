# G1 cut ledger: graph, backlog, claims and the Python remainder

Base 7ed2466c8c (origin/main at start). Scope: the 389 files the path rule in the [README](README.md#campaigns) gives G1. That is every `.py` test file outside `cli/src/fno`. It also holds the `crates/fno-agents` files that match the G1 Rust words and no G3 word.

Before: 1,838 declarations (532 Python, 1,306 Rust) and 1,845 collected cases at base in the cli-ci run 42021471780 logs (540 Python smoke cases, 1,305 Rust). After: 819 declarations (243 Python, 576 Rust). Declarations fell 55 percent. Python collected cases over the scope, read with `fno doctor test <files> --collect-only -q` before and after each slice, fell from 557 to 246. The Rust collected count after is read from the first green CI run of this PR, and the Counts section records it. The target is 922 collected cases or fewer.

Two Python folds keep every check and count as no cut under law d-e016ab8e rule 1. `test_smoke_wheel_env_scrub.py` loops over `WHEEL_CHANNEL_SMOKES`, and `test_protocol_conformance.py` loops over its valid and invalid fixtures. Without them the Python slice reads about 260 cases, still under half of 557.

## Method

Seven workers cut disjoint slices: Python unit, the other Python trees, the Rust integration binaries, and four slices of `crates/fno-agents/src`. Each ran the keep rule in the README. Deleted: parity replays whose Python leg is gone, and value repeats in one branch. Also deleted: unit tests under a real-command keeper, and refusal cases past the one each fail-closed guard keeps. A guard's only test stays. The workers ran no cargo. The lead ran one `cargo check --all-targets` over the crate after the cut and removed the test helpers the cut left dead.

The lead put back one case the cut took: `session_origin::a_path_shaped_session_id_is_refused_before_any_write`. It is the one test of `write_if_absent` refusing a path-shaped session id. The parity module that held it before is gone.

Files deleted with their tests: thirteen golden dirs under `crates/fno-agents/tests/golden/` read only by deleted parity binaries, `schemas/fleet-law.json`, `crates/fno-agents/tests/accounts_state_read_golden_driver.py`, and `cli/scripts/check-prompt-drift.sh` with its CI job. That job compared `cli/src/fno/review/prompts/`, a dir that no longer exists, so it checked nothing. `cli/tests/unit/test_command_collapse_map.py` is unchanged because open PR 4515 edits it.

## Counts

| Measure | Before | After |
|---|---:|---:|
| Declarations, Python | 532 | 243 |
| Declarations, Rust | 1,306 | 576 |
| Collected cases, Python (scope collect) | 557 | 246 |
| Collected cases, Rust (CI logs) | 1,305 | 586 (cli-ci run 42087237752, the x-809b note's count re-read from the same run) |

## Files

| File | Before | After | Contracts kept (credible failure) |
|---|---:|---:|---|
| `crates/fno-agents/src/state/tests/mod.rs` | 32 | 14 | one live ref; succession keeps two rows; rename and its refusal; a corrupt registry is never wiped; a removed row leaves a receipt; newer schema reads forward; writer floor keeps unknown fields; source-ahead bump refuses; a handleless row refuses; pty state wire bytes; schema field inventory; uuid heal; snapshot pins the last good copy |
| `crates/fno-agents/src/claims.rs` | 27 | 12 | renew resets deadline, keeps acquired_at; renew re-anchors a dead pid to the session pid; renew refuses a stale verdict; validation rejects bad input before write; reader rejects newer schema and garbage as corrupted; liveness classify table incl. hybrid arm; fresh acquire writes table and emits; live other holder refused with identity; stale claim reclaimed and audited; release semantics (missing, other, owned); release after steal leaves new holder intact (owner-token guard); simultaneous acquire has one winner |
| `crates/fno-agents/src/active_backlog.rs` | 26 | 10 | held mission dropped and question named (held work dispatched); supervisor stops fanout loops (orphan loops run); hung child killed at wall clock (drain hangs); done records success (done not marked); failed done is a failure (false success); breaker trips at limit (crash loop); incident unavailable is not no_work (fail-open); fleet stop refuses minting (work during stop); real null-bearing receipt dispatches (receipt parse break); one slot serializes (double dispatch) |
| `crates/fno-agents/src/graph_store.rs` | 26 | 10 | python-shaped json bytes (graph.json format drift); evidence-less close refused; leaky title refused; backup rotation pins last good (collapse loses backup); presence wipe refused; one lock per store spelling (two locks race); recompute ladder+rollup (wrong status); mutate retries on interleaved row (lost write); epic cap refused; settle prunes/rewires blocked_by (dangling edges) |
| `crates/fno-agents/src/needs.rs` | 25 | 9 | review-wedged block; one batch call per refused leg (N calls); question index source + ask as evidence; done node holds question until PR closed; sweep closes only terminal rungs; newer head supersedes older question; mux door records durable answer; door refusal names reason; archive door withdraws and refuses |
| `cli/tests/integration/test_worktree_cleanup_merged.py` | 24 | 9 | dry-run default (sweep deletes on bare call); prefix apply keeps branch (pruned upstream read as unverifiable); removal emits event row (removal unattributable); every keep-reason (dirty/unmerged/live/app-owned tree reaped); fetch failure aborts; salvage failure keeps tree; archive refuses unproven detached head; age sweep removes only clean detached; done-node salvage then remove |
| `crates/fno-agents/src/orphans.rs` | 23 | 8 | the daemon is not claimed (a reap would kill it); census workers skipped (a reap kills the fleet); an unreadable census breaks the scan; the reap kills an old renamed process; the three-arm reap gate; render and JSON bytes; seen-file filter; attribution arms |
| `crates/fno-agents/src/org_board.rs` | 22 | 7 | held partition moves row; unreadable ready never read held-free; unplanned queue unreadable when worked source fails; budget kill vs failed exit tally; unheld progress named; mergeable PR scoped by binding node; board answers inside tight budget with every queue |
| `crates/fno-agents/src/outstanding_cmds_tests.rs` | 22 | 9 | add, done and link lane bytes (CRLF kept); argument refusal before the lock; out-of-range refusal leaves the lane byte-identical; reindex fold is idempotent; an unreadable index refuses; provenance refusal texts; delivery line bytes |
| `crates/fno-agents/src/law_match.rs` | 20 | 8 | token+nearby refusal (uncited law passes); validator refusal strings table (bad law recorded); no project fails closed; record door splits echoes/refuses paths; autonomy ask gate needs user words; one scope flag + wide needs certificate; chat cannot supersede operator row; code fact needs a read |
| `crates/fno-agents/src/state_layout.rs` | 20 | 8 | closed top-level set; a dir row folds and parks the clash; a held child lock waits; the root event journal imports then parks; newer-mtime and sqlite move branches; a failed rename refuses; reverse and its refusal; vendored table freshness |
| `crates/fno-agents/src/backlog_share.rs` | 19 | 8 | flushed write lands on primary and peer syncs it; page over reply limit halves until it fits (sync wedges on big page); stale write refused, unreachable one waits; two writers minting one decision seq both land; refused flush keeps statements and notice names revert; pruned change log resnapshots a lagging replica; update ships only moved columns; stale-row guard text names the row (refusal text drift) |
| `crates/fno-agents/src/node_seed.rs` | 19 | 6 | disagreeing family command refuses naming both; unknown node refuses; flag wins over payload/env; quoted title never binds node word; foreign repo caller refused; client refuses payload command node does not derive |
| `crates/fno-agents/src/question_intake.rs` | 18 | 7 | the question file records context and options; a missing readiness section refuses; a retraction ask stamps its subject; a second ask names the open id; a law refusal records nothing; the append door writes both sinks; a failed journal append is fatal |
| `crates/fno-agents/src/backlog_ready.rs` | 17 | 6 | global config answers first for a store in db (precedence flip); outside vote counts nowhere (ranking gamed); verdict walks parent and contained owner (held child dispatched); planless medium derives target under lean floor (wrong command); served effective command and its refusal (refusal swallowed); unknown sort and missing values refused (bad sort accepted) |
| `crates/fno-agents/src/graph_keeper.rs` | 17 | 6 | same-row commit conflicts naming id (lost update); read never sees half a commit (torn read); read_ids overlay order + missing (wrong rows); sqlite write drops cache (stale read); unversioned graph unreadable (bad schema served); rules ask answers assembled set (wrong rules) |
| `crates/fno-agents/src/org_board/queues.rs` | 17 | 6 | cross-territory mergeable PR stays visible; review hold renders blockers; undriven PR counts binding driver; unread gate never filed undriven; prove-it fail becomes failed-verdict row; broken store errors |
| `crates/fno-agents/src/question_clear.rs` | 17 | 7 | answered clear records one decision; a different answer refuses; an agent answer that differs from the user answer refuses; a merge-grant answer becomes the operator grant; only the user or the asking team closes; a negated retraction never reads yes; a retraction answer retracts the law |
| `crates/fno-agents/src/session_report.rs` | 15 | 7 | payload session id wins; a resumed codex id corrects the primary; a third id refuses and writes nothing; an unknown session mints an operator row; a reaped session mints no row; a launch-record child adopts; a promoted successor binds the manifest |
| `crates/fno-agents/tests/backlog_advance_doors.rs` | 15 | 6 | disabled receipt, flag refusals exit 2, explain emits nothing (a bad flag dispatches); join refusal codes 4/2/3 with one join_refused event and no launch (a refused join launches); join writes the brief and policies before each launch (a worker launches with no brief); a dispatch-lanes failure frees the dispatch claim (a leaked claim); closed advance is single-flight (two advances dispatch at once); advance reads real selection and never launches a live node twice (a double launch) |
| `cli/tests/test_lazy_imports.py` | 14 | 5 | advertised menu only (hidden root leaks into --help); whole app builds; missing module names reinstall window; one retry when tree changed; namespace refuser blocks empty-namespace cache |
| `crates/fno-agents/src/attention_arm.rs` | 14 | 5 | spent budget defers (overrun); lost copy of open question is false close (wrong close); two questions deliver pages and index (delivery break); unreadable graph delivers nothing (fail-open); ticked option settles (answer lost) |
| `crates/fno-agents/src/org_fold.rs` | 14 | 5 | fold counts whole scope; unresolved names scope; sweep not run reads unreadable; old unclaimed ready node stuck, held never; ok fold carries epic load and cap |
| `crates/fno-agents/tests/graph_keeper_socket.rs` | 14 | 5 | serves reads, ops and shutdown over the socket (protocol break); concurrent spawns settle on one keeper, losers exit 3 (two keepers on one store); concurrent idea commits survive note writes (lost update); shutdown mid-commit never loses an ok reply (an acked write is lost); ready board refuses unreadable claims (unreadable reads as free) |
| `cli/tests/unit/test_paths.py` | 13 | 3 | unknown {var} rejected (fails if the template silently passes); traversal project.id rejected (fails if a vault path escapes its root); FNO_STATE_DIR moves the state root (fails if a sealed worker reads a foreign graph) |
| `crates/fno-agents/src/backlog/api/tests.rs` | 13 | 5 | cursor paging (page skip); failed mutation keeps version (version drift); status via patch door (door bypass); primary PR stamp mismatch refused (wrong PR bound); comment thread rules (state break) |
| `cli/tests/lint/test_check_pitfalls.py` | 12 | 3 | over-cap + unreadable budget verdict reported; title naming a shipped command refused; graduates-to with no node refused |
| `cli/tests/lint/test_check_preamble_budget.py` | 12 | 4 | ceiling is strict > (off-by-one); missing fixed root fails loud; every frontmatter parses strictly (agent description lost); unbanked file cut refused |
| `crates/fno-agents/src/backlog/decisions_cli.rs` | 12 | 4 | lane derivation (wrong lane); coord row unknown on unreadable graph (fail-open); current law conflict/none (wrong law); scoped read degrades when store absent (crash) |
| `crates/fno-agents/src/backlog/workflows.rs` | 12 | 5 | release twins free only their own children; contain-release lifts a child, refuses an uncontained one; reopen demotes the completed PR; cycle guard walks the parent chain (infinite loop); cascade closes an ancestor whose children all closed |
| `cli/tests/events/test_event_emit_cli.py` | 11 | 5 | canonical envelope + nonce; refusals write no row; attester forgery refused; model only from observed transcript; global mirror repo-scoped |
| `cli/tests/unit/test_retired_command_gate.py` | 11 | 2 | refusal names the path, replacement, ruling and the retired-ok escape (byte pin); a lost canary fixture is refused as vacuous |
| `cli/tests/unit/test_rust_binary.py` | 11 | 3 | FNO_AGENTS_BIN outranks the search; an unusable override falls through; command_call puts returncode on the error as data |
| `crates/fno-agents/src/backlog/autobrief.rs` | 11 | 4 | explicit rung verbatim (override clamped); sidecar clamps on codepoint (UTF-8 split); synth names provenance (fabrication); window keeps near records (stale tail) |
| `crates/fno-agents/src/claim_commands.rs` | 11 | 5 | native claim doors answer over the claim table; session-absent requires a readable registry (fail-open release); handover witness never answers from minter session; served dead word never reads absent; rollout rung fails closed |
| `cli/tests/hooks/test_init_target_state_skip_flags.py` | 10 | 5 | size-profile skip flags (S,L rows); cancelled claimless session archived; no-binary refuses hold lookup; env grant honored for operator; env grant scrubbed for agent |
| `cli/tests/unit/test_validate_plan_consolidation.py` | 10 | 2 | a post-gate plan with no block errors (and the summary still runs); a plan created on the gate date is grandfathered |
| `crates/fno-agents/src/backlog/decompose.rs` | 10 | 4 | cap over ceiling refuses (oversized decompose); validate refuses overflow/dup/cycle (bad plan); slice waves and blockers (wrong graph); dead owner refusal (child on dead owner) |
| `crates/fno-agents/src/backlog/maintenance_tests.rs` | 10 | 3 | unread roster reaps nothing (fail-open reap); apply prunes/rescopes skipping claimed (claimed row touched); unreadable claim state refuses (fail-open) |
| `crates/fno-agents/src/decision_index.rs` | 10 | 4 | live laws keep three rows newest first; missing path errs, bad lines count as damaged; foreign project row hides and count names it; certified machine reads only wide law its user key signed (unsigned law binds) |
| `crates/fno-agents/src/fleet_incident.rs` | 10 | 4 | stop bumps generation (lost generation); corrupt state is unavailable (reads clear); unreadable state refuses write (overwrite); pause-all alias cannot clear incident (race clears stop) |
| `crates/fno-agents/src/fleet_state.rs` | 10 | 4 | heartbeat keeps chain memory (memory wiped); links accumulate without dupes (dupe links); TTL forgets old walk (stale walk); unknown flag refuses (flag becomes node id) |
| `cli/tests/unit/test_hooks_json_validity.py` | 9 | 2 | codex hook names itself when its file is gone, and exit 2 still blocks; every manifest hook script exists |
| `crates/fno-agents/src/backlog/mod.rs` | 9 | 3 | concurrent first writes land (lost write); unrepresentable row carried verbatim (data loss); concurrent single-row writes persist (lost write) |
| `crates/fno-agents/src/backlog/patch.rs` | 9 | 5 | idea on ready plan refuses naming plan (plan orphaned); field policy covers every key once (new field unguarded); derived/owned fields refuse naming owner (door bypass); deferred clears lock, needs reason (stuck lock); retry refuses after three (livelock) |
| `crates/fno-agents/src/backlog/scorecard.rs` | 9 | 3 | grouping and cost fallback (wrong cell or cost); unattributed rows never dropped (rows vanish); unread outcome feeds stay null, never zero (fake zero rate) |
| `crates/fno-agents/src/claims_identity_tests.rs` | 9 | 4 | resolve identity resolves session and harness together; canonical partial and vendor contradictions refuse; scrub identity env removes every identity name, keeps routing; ambient parent edge resolves and refuses mixing |
| `crates/fno-agents/src/hook/session_state/tests.rs` | 9 | 3 | claude/codex row map (event wired to wrong state); traversal server name stays in pin dir (path escape); stop-gate block report sends working and marks record (stuck blocked) |
| `crates/fno-agents/src/org_board/classify.rs` | 9 | 3 | holder reading maps shared verdict; advancing holder never stalled; failed roster read is unmeasured not none |
| `crates/fno-agents/src/outstanding_tests.rs` | 9 | 4 | JSON key order; a missing store reads empty but an unreadable one fails; question fold rank; capture-leg dedupe |
| `crates/fno-agents/tests/backlog_node_state.rs` | 9 | 4 | one current state with priors in history (a prior is dropped); budget boundary (an oversized body lands); stale writer refused (lost update); a write keeps another writer's row (a clobbered row) |
| `crates/fno-agents/tests/backlog_session_golden.rs` | 9 | 4 | open writes the 2h lease naming the passed identity (wrong TTL or holder); a live foreign holder is refused and left intact (a stolen claim); no identity refuses and claims nothing (an anonymous claim); close releases the holder and stamps the row (a leaked claim) |
| `cli/tests/goldens/test_lifecycle_golden.py` | 8 | 3 | done receipt; done-without-evidence refusal bytes; narrower successor needs leftover |
| `cli/tests/unit/test_hatch_build.py` | 8 | 2 | all binaries bundle as scripts; a partial binary set hard-fails the build |
| `crates/fno-agents/src/backlog/closures.rs` | 8 | 4 | sweep to fixpoint (epic stays open); unmet promise holds rollup (premature close); declared child closes on either evidence (stuck child); orphan sweep to live ancestor (orphans) |
| `crates/fno-agents/src/backlog/node_ref.rs` | 8 | 3 | flag-shaped token refused (flag as id); unique prefix vs ambiguous (wrong node); blocker validation refuses unknown/self/cycle (bad graph) |
| `crates/fno-agents/src/backlog/projections.rs` | 8 | 3 | graph.md cards carry lane/blockers/done links (card field dropped); bases refuse a hand-authored file (user file overwritten); render targets refuse unknown keys and relative paths (bad target accepted) |
| `crates/fno-agents/src/backlog/quota_route.rs` | 8 | 3 | observation off short-circuits before any probe (probe runs when off); exhaustion defers and cuts over (exhausted lane dispatched); unreadable state stays unknown and eligible (corrupt state blocks work) |
| `crates/fno-agents/src/claim_cli/manifest_refresh.rs` | 8 | 3 | heal drops blocked line, appends claim pair, consumes init sentinels; heal spares a user cancel sentinel (user cancel lost); heal skips a holder that is not the manifest's session (foreign rewrite) |
| `crates/fno-agents/src/org_board/prs.rs` | 8 | 3 | mergeable filter drops pending/failed; binding reads body trailer and warns; two nodes carrying PR refuses binding |
| `crates/fno-agents/src/reclaim.rs` | 8 | 4 | apply removes old and spares fresh; a converge lock keeps backups; a live symlink and source dir are spared; retention without --yes is report-only |
| `crates/fno-agents/src/session_truth.rs` | 8 | 4 | a quoted promise tag does not signal; warming until the cursor exists; a missing stamped transcript falls back; a claude rebuild stops at the compact boundary |
| `crates/fno-agents/tests/pi_session_identity.rs` | 8 | 4 | attach argv is identical in both crates (crate drift); argv is the TUI with provider and model pinned (falls to Bedrock or rpc mode); cwd encoding matches real dirs (lookup misses); a duplicate id refuses and names every file (a random session resumes) |
| `cli/tests/unit/test_mutex_steal.py` | 7 | 2 | an aged corpse is stolen; a live lock swapped in mid-steal is restored, not stolen |
| `cli/tests/unit/test_validate_plan_decisions_acknowledged.py` | 7 | 2 | a live ruling with no acknowledgment errors; a clean plan names its checked count |
| `crates/fno-agents/src/attention.rs` | 7 | 3 | worktree asker routes to repo (wrong repo); open question projects with missing fields named (silent gap); answer row flips state (answer lost) |
| `crates/fno-agents/src/backlog/promise.rs` | 7 | 3 | plan promise gate reads or fails closed (unreadable plan passes); rollup hold gates only plan-having candidates (planless parent held); split hold keeps node open until every listed PR merges (early close) |
| `crates/fno-agents/src/claims_gate_tests.rs` | 7 | 3 | expired gate claim reads the pid's verdict; unexpired gate claim with refused probe stays suspect (fail-open); make_claim stamps pid provenance by origin |
| `crates/fno-agents/src/claude_sessions.rs` | 7 | 3 | working transcript does not outrank a recorded exit; gone, recycled, unparseable and unreadable each answer; holder JSON never reads unmeasured as held (false held) |
| `crates/fno-agents/src/node_branch.rs` | 7 | 3 | ids never match partial hex prefix (wrong node); mint round-trips (unresolvable branch); resolve order local>origin>mint (wrong branch) |
| `crates/fno-agents/tests/backlog_findings.rs` | 7 | 3 | create, read back, resolve, list (a finding is lost or not cleared); refuses empty, over 5000 chars and unknown node (junk lands); a second resolve reads already_resolved (resolve time overwritten) |
| `crates/fno-agents/tests/backlog_note_migration.rs` | 7 | 4 | originals and positions survive verbatim (lossy migration); stale, oversized and missing-manifest entries refuse (a bad entry migrates); a rerun is idempotent (double import); history prints bodies and trailer (history command breaks) |
| `crates/fno-agents/tests/carveout_cli.rs` | 7 | 4 | add records the session, prints only the id, refuses bad values; list scopes to the session with its banner; update keeps the id and refuses what it cannot apply; resolve records only the ids that went (each: that command misbehaves at the binary) |
| `tests/spec/test_impeccable_stages_validator.py` | 7 | 2 | valid stages pass; unknown stage refused |
| `tests/spec/test_product_md_check.py` | 7 | 2 | PRODUCT.md present: no warning; missing: prerequisites written |
| `cli/benchmarks/test_measure_fno.py` | 6 | 1 | decision-rule boundaries |
| `cli/tests/unit/test_gate_escape_command.py` | 6 | 2 | unknown reason fails loud and writes nothing; a manual tag lands one row |
| `cli/tests/unit/test_inbox_path.py` | 6 | 2 | default resolves to the vault parking-lot.md; obsidian on with no vault link stays in .fno |
| `cli/tests/unit/test_prove_it.py` | 6 | 2 | the validator selftest (14 cases) passes; a PASS with no marked probe is REFUSED and named |
| `cli/tests/unit/test_target_denominator.py` | 6 | 2 | --deliverables stamps the manifest; a newline-injected value is refused before the write |
| `cli/tests/unit/test_workflow_authority_contract.py` | 6 | 2 | agent skill obeys confirm-decision.sh; triage mutations sit after the approval boundary |
| `crates/fno-agents/src/attention_file.rs` | 6 | 2 | full question renders (page shape break); answer precedence (wrong answer wins) |
| `crates/fno-agents/src/attention_http.rs` | 6 | 2 | permanent 4xx never resends (resend storm); loopback answer URL refused (SSRF/loop) |
| `crates/fno-agents/src/backlog/binding.rs` | 6 | 2 | unknown claim refuses before writes (partial write); primary fill plus additional append (binding loss) |
| `crates/fno-agents/src/backlog/drift_scan.rs` | 6 | 3 | closed only when every ref closed (false close); merged PR closes only named node (wrong close); repoless query refused (wrong repo) |
| `crates/fno-agents/src/backlog/epic_cap.rs` | 6 | 2 | sixteenth child refused (cap bypass); cap config parse (bad cap) |
| `crates/fno-agents/src/backlog/settings.rs` | 6 | 2 | state-dir carrier outranks config and home (wrong root); detect matches normalized work-map path (project not detected) |
| `crates/fno-agents/src/claim_queue.rs` | 6 | 3 | fresh contender queues behind live older ticket (queue jump); dead ticket reaped, never blocks; foreign machine ticket skipped, never ordered |
| `crates/fno-agents/src/fleet_law.rs` | 6 | 3 | armed hold renders line (hold silent); targeted hold binds only its subject (hold leaks); unreadable breaker fails closed (holds dropped) |
| `crates/fno-agents/src/session_activity.rs` | 6 | 3 | one message id counts once; a zai-routed row never votes a claude label; codex parsers |
| `crates/fno-agents/src/session_truth/cursor.rs` | 6 | 4 | appended-bytes-only parse; truncate or replace rebuilds; rebuild cap; an unchanged relist reads no dir |
| `crates/fno-agents/src/team_alarm.rs` | 6 | 2 | an empty team over ready work raises; a sweep that did not run reads unreadable |
| `crates/fno-agents/src/team_names.rs` | 6 | 3 | checkin naming and theme flows (one folded case); carry succession; a malformed store is an error |
| `crates/fno-agents/tests/backlog_note_delivery.rs` | 6 | 3 | a corrupt store names the read failure, never absence; positional, stdin and file bodies append to the thread (a dropped body); machine and wave records go to history only (thread noise) |
| `crates/fno-agents/tests/graph_store_parity.rs` | 6 | 0 | deleted: the inventory marks the port complete; concurrent writers stay covered in graph_write_contention and graph_keeper_socket |
| `crates/fno-agents/tests/run_state.rs` | 6 | 3 | every legal transition matches the plan table; a rejected transition appends nothing (an illegal step is journaled); a corrupt log is an error, never Open |
| `cli/tests/events/test_producer_equivalence.py` | 5 | 3 | each SKILL.md producer emits one attestation (3 rows); findings report attests fail with record; resolver reads code-review attestation at HEAD |
| `cli/tests/lint/test_check_parity_test_provenance.py` | 5 | 2 | characterization with live oracle fails; differential with dead oracle fails |
| `cli/tests/lint/test_command_callers.py` | 5 | 2 | rust argv sweep credits array (live caller dropped); broken sweep emits no list |
| `cli/tests/lint/test_dsplit.py` | 5 | 2 | worker subtree = work, chores = maintenance; pick_daemon refuses zero/ambiguous |
| `cli/tests/lint/test_execution_validation_wiring.py` | 5 | 1 | target validates quick plans |
| `cli/tests/lint/test_smoke_wheel_env_scrub.py` | 5 | 3 | tracker sees through col-0 nesting; every smoke classified; every wheel smoke scrubs-verifies-installs in order (per-script rows folded into one loop over the registry) |
| `cli/tests/test_short_flag_convention.py` | 5 | 1 | global register never carries a local meaning |
| `cli/tests/unit/test_ambient_surface.py` | 5 | 2 | every source env read is classified; no receipt and no binary refuses (exit 3) |
| `cli/tests/unit/test_events_schema.py` | 5 | 1 | validate refuses a missing required field and a bad span_kind |
| `cli/tests/unit/test_hermetic_prod_writes.py` | 5 | 2 | append_event refuses a write outside the sandbox; an undeclared root refuses under pytest and names the declarations |
| `cli/tests/unit/test_product_qualification_manifest.py` | 5 | 2 | five scenario families with stable ids; every expected case has a unique identity |
| `cli/tests/unit/test_session_satisfied.py` | 5 | 1 | an unknown session_satisfied trigger is refused |
| `cli/tests/unit/test_skill_bundles.py` | 5 | 2 | no skill references ${REPO_ROOT}/scripts/; the generator handles a mixed file/reference/agent manifest |
| `cli/tests/unit/test_validate_plan_stub_markers.py` | 5 | 2 | an unfilled scaffold is rejected and its marker named; a filled child passes |
| `crates/fno-agents/src/backlog/birth.rs` | 5 | 2 | rank resolution (wrong rank); hook door refuses malformed argv (bad birth) |
| `crates/fno-agents/src/backlog/create_cli.rs` | 5 | 2 | tombstone round trip and mint exclusion (id reuse); legacy archive exhaustion refuses (id reuse) |
| `crates/fno-agents/src/backlog/maintenance_archive.rs` | 5 | 2 | hard vs soft edges hold (archive of referenced row); superseded sweep unless named (over-archive) |
| `crates/fno-agents/src/backlog/maintenance_validity.rs` | 5 | 2 | contained paths never escape root (path escape); unsafe verdicts downgrade (unsafe apply) |
| `crates/fno-agents/src/backlog/note_cli.rs` | 5 | 2 | note appends thread row, state unchanged (state clobber); comment thread CLI contract (CLI break) |
| `crates/fno-agents/src/backlog/note_public.rs` | 5 | 2 | author named never mailed (self-mail); role walk stops at first live scope (wrong recipient) |
| `crates/fno-agents/src/backlog/style_check.rs` | 5 | 2 | message-cap refusal names the enforced number (refusal text drift); glue rule fires on specimens, spares controls (rule misfires) |
| `crates/fno-agents/src/backlog/supersession.rs` | 5 | 2 | narrower successor refused until leftover filed (scope silently lost); settlement applies and summarizes (settlement not applied) |
| `crates/fno-agents/src/backlog/update_cli.rs` | 5 | 2 | unknown flag refuses with retired message (refusal text drift); removing a blocker a held hold names refuses (hold bypass) |
| `crates/fno-agents/src/fleet_beacon_tests.rs` | 5 | 2 | machine replaces only its own rows on the real primary (cross-machine wipe); stale machine reads offline + cross-machine parent (dead machine shown live) |
| `crates/fno-agents/src/node_origin.rs` | 5 | 2 | explicit request vs evidenced discovery under one recorder (wrong origin); unreadable queue refuses (fake operator origin) |
| `crates/fno-agents/src/pane_claim.rs` | 5 | 2 | binds to a pane; a nested harness refuses |
| `crates/fno-agents/src/question_judge.rs` | 5 | 2 | only a ruling is citable; a run writes only its ledger |
| `crates/fno-agents/src/session_backfill.rs` | 5 | 1 | apply fills gaps and never overwrites |
| `crates/fno-agents/src/session_role.rs` | 5 | 2 | a lead names its scope; a worker takes its live parent's lead |
| `crates/fno-agents/src/team_settle.rs` | 5 | 3 | the succession plan table; a superior takeover and its refusal; the takeover apply |
| `crates/fno-agents/src/team_split.rs` | 5 | 2 | two live rows are a double rule; a dead call is unread without a transcript or id |
| `crates/fno-agents/tests/backlog_get_exits.rs` | 5 | 3 | a hit prints the field raw; a clean miss is exit 1 naming the store; an unreadable store is exit 3, never absence |
| `crates/fno-agents/tests/claim_status_join.rs` | 5 | 3 | a live worker flips an unheld node to unknown (a worked node reads free); the graph join resolves an unresolved row; unrelated rows still read free (a false unknown) |
| `crates/fno-agents/tests/cursor_agent_session_identity.rs` | 5 | 3 | a full uuid resumes with the pinned argv; an fno handle is refused as a chat id, with its message bytes; the reaper selector stays inside the owner's process tree (reaps a foreign process) |
| `crates/fno-agents/tests/fleet_task_reconcile_parity.rs` | 5 | 0 | deleted: the inventory marks the port complete; src/fleet_task.rs keeps a reconcile life-cycle unit test |
| `crates/fno-agents/tests/graph_write_contention.rs` | 5 | 3 | a stale base three commits behind still commits disjoint rows (a false conflict); a moved row conflicts and names the row (lost update); concurrent disjoint writers all land |
| `crates/fno-agents/tests/question_intake_parity.rs` | 5 | 0 | deleted: replay of goldens from the deleted Python leg; question_intake.rs keeps 18 unit tests, and quote_refusals keeps the refusal bytes |
| `cli/tests/events/test_emit_attestation_actor.py` | 4 | 3 | actor session/harness recorded; routed env model + secret dropped; flag-shaped reviewer emits nothing |
| `cli/tests/events/test_migration.py` | 4 | 1 | legacy rows migrate to canonical shape |
| `cli/tests/goldens/test_session_golden.py` | 4 | 2 | close receipt + launch line; non-owner session add refused with reap door |
| `cli/tests/hooks/test_operator_capture_nudge.py` | 4 | 2 | literal count at depth 3; failed read reported, never silent |
| `cli/tests/hooks/test_register_session_start.py` | 4 | 2 | disagreeing codex ids report nothing; hand-started session reports for daemon mint |
| `cli/tests/integration/test_consolidation_audit.py` | 4 | 1 | planted stale skill ref fails audit |
| `cli/tests/integration/test_worktree_manager.py` | 4 | 1 | migrate --auto removes stale, keeps live and unreadable |
| `cli/tests/unit/test_pytest_shard_partition.py` | 4 | 2 | a shard selects one disjoint slice; a malformed spec fails loud (one row) |
| `cli/tests/unit/test_rust_front_handoff.py` | 4 | 2 | a second hand-off entry refuses as recursive; an old binary cannot recurse through the backlog forward |
| `cli/tests/unit/test_scope_growth.py` | 4 | 2 | a suppressed figure is null, not 0; the reportable line renders |
| `crates/fno-agents/src/backlog/advance_fill.rs` | 4 | 2 | peer lane holds node (double work); no-surface fail-open token (silent drop) |
| `crates/fno-agents/src/backlog/batch.rs` | 4 | 2 | domain guard refuses traversal (path escape); join idempotent, ceiling refuses (overfull batch) |
| `crates/fno-agents/src/backlog/cli.rs` | 4 | 2 | grouped spellings resolve (command break); unknown command keeps name for tombstone (bad error) |
| `crates/fno-agents/src/backlog/find_cli.rs` | 4 | 2 | search order and token match (wrong results); ab-prefix tier (wrong match) |
| `crates/fno-agents/src/backlog/freshness.rs` | 4 | 2 | changed path drifted plus event (missed drift); no main sha unmeasured, exit 0 (false fail) |
| `crates/fno-agents/src/backlog/intake.rs` | 4 | 2 | title ladder (wrong title); depends key precedence (lost blockers) |
| `crates/fno-agents/src/backlog/rank_cli.rs` | 4 | 2 | column ladder routes like render (rank column drifts from board); find_node_index ambiguity names candidates (ambiguous prefix picks one) |
| `crates/fno-agents/src/backlog/reconcile_cli.rs` | 4 | 2 | close records its path and labels an outside merge (close path lost); cascade-closed parent still gets merge stamp (stamp skipped) |
| `crates/fno-agents/src/backlog/requests.rs` | 4 | 2 | sanitizers quarantine visitor bytes, envelope holds the warn (hostile bytes reach store); import dedupes and holds ceilings over one snapshot (duplicate import or ceiling overrun) |
| `crates/fno-agents/src/backlog/schema_v4.rs` | 4 | 2 | schema 3 store migrates to 4 with wire JSON kept (migration loses data); bad timestamp refused by constraint name (bad row stored) |
| `crates/fno-agents/src/backlog/title_gate.rs` | 4 | 2 | every leak class refuses (leaky title accepted); unchanged legacy leaky title passes unrelated update (legacy rows uneditable) |
| `crates/fno-agents/src/backlog/trace.rs` | 4 | 2 | phase spans fold, open phase leaves node open (wrong span); counts fold per phase, unread transcript reads null (fake zero) |
| `crates/fno-agents/src/backlog/update_ops.rs` | 4 | 2 | parse splits on first equals (value with = truncated); kinds refuse operators they do not take (bad op applied) |
| `crates/fno-agents/src/backlog/wave_notes.rs` | 4 | 2 | disposal refuses a missing target, writes nothing (phantom write); disposal round-trips through the store (note lost) |
| `crates/fno-agents/src/claim_store.rs` | 4 | 2 | lost store under a published marker refuses to reimport claims (resurrected claims); remote shared claims decide on the primary (split brain) |
| `crates/fno-agents/src/claims_reservation_tests.rs` | 4 | 2 | expired reservation resolves through its pid; unexpired review hold with dead pid still protects (hold lost) |
| `crates/fno-agents/src/claims_shared_host_tests.rs` | 4 | 2 | codex node claim, no ttl, live host pid without session suspects; per-session host claim with live pid stays live |
| `crates/fno-agents/src/fleet_task.rs` | 4 | 2 | file twice holds one row (dupe rows); unreadable store errors (silent empty) |
| `crates/fno-agents/src/graph_get.rs` | 4 | 2 | state-dir carrier outranks FNO_HOME (wrong graph read); external backend refuses default store (local store read under remote) |
| `crates/fno-agents/src/law_filters.rs` | 4 | 2 | every filter must match, not-form excludes (law over-fires); missing fact/filter passes (law never fires) |
| `crates/fno-agents/src/law_recheck.rs` | 4 | 2 | date guard expires only after instant (early expiry); until grammar parses kinds and refuses count (bad guard accepted) |
| `crates/fno-agents/src/node_route.rs` | 4 | 2 | route_rows table (misroute); verdict_rows table (wrong verdict) |
| `crates/fno-agents/src/org_board/budget.rs` | 4 | 2 | sources share one deadline, exhaustion names last source; nonzero exit is failure not budget kill |
| `crates/fno-agents/src/state/tests/x4c87_row_counts.rs` | 4 | 1 | an unrepresentable row fails the read naming both counts |
| `crates/fno-agents/src/team_identity.rs` | 4 | 2 | the carried pair matches; a rebound name does not match |
| `crates/fno-agents/src/worked_nodes.rs` | 4 | 2 | a promoted lead is never the worker; a closed session frees the node |
| `crates/fno-agents/tests/active_backlog_drain.rs` | 4 | 3 | dispatch records pending and forwards the epic seam; all done retires the mission; an advance failure is journaled with no pending (a phantom pending node) |
| `crates/fno-agents/tests/backlog_write_goldens.rs` | 4 | 4 | update, rank, add and idea golden replays (each pins one command's user-facing bytes and rows); idea stays because its fold and wave-of cases differ from add |
| `crates/fno-agents/tests/manifest_for_session.rs` | 4 | 3 | resolves the matching manifest from another worktree; an unknown session exits 1 with no output; an unreadable worktree list exits 2 |
| `cli/tests/events/test_event_find.py` | 3 | 2 | rotated rows counted; zero beyond coverage refused |
| `cli/tests/events/test_protocol_conformance.py` | 3 | 2 | every valid fixture conforms; every invalid fixture rejected (rows folded into loops) |
| `cli/tests/events/test_python_validator.py` | 3 | 1 | every declared source legal for envelope |
| `cli/tests/events/test_retention_routing.py` | 3 | 1 | ephemeral row lands in store as ephemeral |
| `cli/tests/events/test_schema_files.py` | 3 | 1 | events-v3 single envelope |
| `cli/tests/events/test_schema_manifest.py` | 3 | 1 | lead_checkin required/forbidden keys |
| `cli/tests/hooks/test_session_start_plugin_root.py` | 3 | 2 | canonical checkout writes pointer; linked worktree never does |
| `cli/tests/integration/test_first_run_documentation.py` | 3 | 2 | documented fno commands resolve; config default promises match schema |
| `cli/tests/integration/test_marketplace_lint.py` | 3 | 2 | toolbag omitting a skill fails; real repo passes lint (only runner of the lint) |
| `cli/tests/integration/test_product_delivery_journey.py` | 3 | 2 | full pack journey; incompatible pack refused, writes nothing |
| `cli/tests/lint/test_drafting_lenses_contract.py` | 3 | 2 | lens table rows resolve; lenses attributed in NOTICE |
| `cli/tests/lint/test_review_skill_freshness.py` | 3 | 1 | freshness preflight runs before router |
| `cli/tests/test_help_shorthands.py` | 3 | 1 | legend names no dead command |
| `cli/tests/test_import_time_env_reads.py` | 3 | 2 | probe positive control; no module reads ambient env at import |
| `cli/tests/unit/test_agy_plugin_payload.py` | 3 | 2 | payload holds exactly the allowlist; a missing allowlist entry fails and leaves no payload |
| `cli/tests/unit/test_backlog_album.py` | 3 | 2 | JSON is newest-first, superseded excluded, pr_url only when recorded; text names "no gift" |
| `cli/tests/unit/test_backlog_intake_door.py` | 3 | 2 | intake mints a node readable through the door; a surfaceless plan is refused (exit 2) |
| `cli/tests/unit/test_command_collapse_map.py` | 3 | 3 | UNCHANGED (open PR 4515 edits it) |
| `cli/tests/unit/test_event_log.py` | 3 | 2 | a locked store is retried; a newer store schema is refused |
| `cli/tests/unit/test_flag_aliases.py` | 3 | 2 | legacy-only warns and returns; both spellings raise |
| `cli/tests/unit/test_flag_axis_tombstones.py` | 3 | 2 | no runtime string promises a 0.4.0 removal; retired --provider exits 2 with the axis message |
| `cli/tests/unit/test_join_partition_hook.py` | 3 | 2 | no policy dir approves; a peer-band edit is blocked naming the band |
| `cli/tests/unit/test_law_command_form_gate.py` | 3 | 2 | a missing form names laws and path; --live flags unregistered forms and an unreadable store |
| `cli/tests/unit/test_mail_withdraw.py` | 3 | 2 | scan_unread omits a withdrawn message; a forged tombstone cannot retract other mail |
| `cli/tests/unit/test_plugin_root_resolution.py` | 3 | 2 | falls to the persisted pointer; a worktree pointer self-heals to canonical |
| `cli/tests/unit/test_post_merge_inbox_idempotency.py` | 3 | 2 | missing inbox exits 1; PR 12 does not match a PR 123 marker |
| `cli/tests/unit/test_prompt_drift.py` | 3 | 0 | file deleted (see 3) |
| `cli/tests/unit/test_resolve_canonical_worktree.py` | 3 | 2 | linked worktree resolves to main; separate-git-dir never returns the sibling |
| `cli/tests/unit/test_resolve_executor_emit.py` | 3 | 2 | a bogus executor falls back to tdd with warn_fallback; no fno on PATH never breaks routing |
| `cli/tests/unit/test_state_fence.py` | 3 | 2 | a source-ahead write is refused naming target and remedy; a worktree .git file counts as a checkout |
| `cli/tests/unit/test_task_rows.py` | 3 | 2 | in_progress takes the claim and writes the row; a second holder is refused (exit 3) |
| `cli/tests/unit/test_text_or_file.py` | 3 | 2 | file-only read; both inline and file refused |
| `cli/tests/unit/test_worktree_reap_equivalence.py` | 3 | 2 | a deletions-only worktree is actually removed; the helper fails closed when the gate cannot answer |
| `cli/tests/unit/test_worktree_reap_ordering.py` | 3 | 2 | zero-ahead is never a reap key; the daemon sweep is report-only |
| `crates/fno-agents/src/attention_reply.rs` | 3 | 2 | delivered answer never reenters ladder (double delivery); durable receipt resumes (lost confirm) |
| `crates/fno-agents/src/attention_route.rs` | 3 | 2 | teamed epic routing (wrong lead); parent cycle bounded (infinite loop) |
| `crates/fno-agents/src/backlog/advance.rs` | 3 | 1 | leadless key marks render (marker lost) |
| `crates/fno-agents/src/backlog/autolink.rs` | 3 | 2 | linked receipt score and undo (wrong parent); exemptions (bulk relinks) |
| `crates/fno-agents/src/backlog/calibration.rs` | 3 | 1 | aggregate by domain/difficulty/size (wrong calibration) |
| `crates/fno-agents/src/backlog/capture.rs` | 3 | 2 | add mints once then dedups (dupes); dismiss strikes and archives (data loss) |
| `crates/fno-agents/src/backlog/collision.rs` | 3 | 2 | high overlap supersede (missed dup); partition and unevaluated (silent skip) |
| `crates/fno-agents/src/backlog/done_evidence.rs` | 3 | 2 | bare close refused with repairs (evidence-less done); every evidence kind passes (false refusal) |
| `crates/fno-agents/src/backlog/done_rich.rs` | 3 | 2 | rollup fills empty only, dedups cost (overwrite); stamp refuses mismatch (wrong URL) |
| `crates/fno-agents/src/backlog/drift_emit.rs` | 3 | 2 | satisfied event for live matching PR (missed event); gate escape needs PR and bot (false escape) |
| `crates/fno-agents/src/backlog/encounter_cli.rs` | 3 | 2 | refusal exit codes and markers (bad exit); one vote per session (vote stuffing) |
| `crates/fno-agents/src/backlog/idea_cap.rs` | 3 | 1 | 26th idea refused naming oldest (cap bypass) |
| `crates/fno-agents/src/backlog/node_match.rs` | 3 | 2 | exact id wins, open preferred (wrong node); ambiguity and flag never match (wrong node) |
| `crates/fno-agents/src/backlog/nodes.rs` | 3 | 1 | resave keeps owned rows and rowids (row churn/loss) |
| `crates/fno-agents/src/backlog/publish.rs` | 3 | 2 | exec targets refuse with no slot or on nonzero exit (silent publish failure); receipt check fails closed (bad receipt accepted) |
| `crates/fno-agents/src/backlog/reid_cli.rs` | 3 | 2 | reid renames row and satellites, aliases old id (orphaned satellite rows); collision refused, store unchanged (existing id overwritten) |
| `crates/fno-agents/src/backlog/render.rs` | 3 | 1 | grouped output matches captured bytes (output bytes drift) |
| `crates/fno-agents/src/backlog/scorecard_router.rs` | 3 | 2 | cheaper close candidate proposes with accept line (no proposal); pricier candidate holds, blueprint stage never sees glm (wrong route for blueprint) |
| `crates/fno-agents/src/backlog/worked.rs` | 3 | 2 | fleet rows start from registry, name vendor-only rows (rows missed); unattributable no-sid row skips advisory, never blocks (false block) |
| `crates/fno-agents/src/claim_cli/mod.rs` | 3 | 1 | release stamp fills the open row acquire opened (row not closed) |
| `crates/fno-agents/src/claims_long_holds.rs` | 3 | 2 | long-holds row shape, threshold and order; op prints JSON and refuses missing dir flag |
| `crates/fno-agents/src/claims_release_stopped.rs` | 3 | 1 | release for stopped session releases owned dead, keeps live (live claim released) |
| `crates/fno-agents/src/claims_session_absence_tests.rs` | 3 | 1 | session absent does not turn unresolved into early release (fail-open release) |
| `crates/fno-agents/src/convert/claim_repin.rs` | 3 | 1 | failed repin reported by key, never hidden (silent repin loss) |
| `crates/fno-agents/src/decision_trace.rs` | 3 | 1 | mail record leaf writes origin row, ask span and route parent (span or parent missing) |
| `crates/fno-agents/src/fleet_load.rs` | 3 | 2 | threshold curve cap from slowdowns (wrong cap); unrecognized detail never counts zero (false clean) |
| `crates/fno-agents/src/law_when.rs` | 3 | 2 | flags parse and bad ones refused (bad window accepted); every part must pass, unread part fails open (law wrongly gated) |
| `crates/fno-agents/src/node_bind.rs` | 3 | 2 | one open node binds once, held node respected (steals held node); two open nodes stay silent (wrong bind) |
| `crates/fno-agents/src/node_liveness.rs` | 3 | 2 | verdict needs live run/bounded wait/young recovery (dead read live); sweep files past grace, reader refuses stale (stale verdict) |
| `crates/fno-agents/src/node_reading.rs` | 3 | 2 | full row order state>plan>stale (wrong reading); attach puts marker first, no-op when absent |
| `crates/fno-agents/src/org_board/stranded.rs` | 3 | 2 | porcelain parses linked trees, skips main/bare; candidates take lead priority, live nodes only |
| `crates/fno-agents/src/question_ladder.rs` | 3 | 1 | a climb adds a via hop and lands on the next rung or the board |
| `crates/fno-agents/src/session_cost.rs` | 3 | 1 | transcript parse accumulates, dedups and classifies |
| `crates/fno-agents/src/session_start_bytes.rs` | 3 | 2 | byte sum with import; missing gate is a usage error |
| `crates/fno-agents/src/state_path.rs` | 3 | 2 | space paths from a worktree; the state-dir door refuses unknown names |
| `crates/fno-agents/src/state_root_drift.rs` | 3 | 2 | undocumented names are reported; a failed doc read is a refusal |
| `crates/fno-agents/src/team_read.rs` | 3 | 2 | team payload bytes; an epic rung needs a readable graph |
| `crates/fno-agents/src/team_spawn.rs` | 3 | 2 | captured decision goldens; a rebound successor cannot clear the predecessor |
| `crates/fno-agents/tests/attention_arm_journey.rs` | 3 | 2 | deliver, settle, record, close, move (a page is lost or sent twice); a routing failure delivers nothing, then recovers |
| `crates/fno-agents/tests/backlog_model_roundtrip.rs` | 3 | 2 | every golden row round-trips (the typed model drops a field); an unknown key lands in extras in order |
| `crates/fno-agents/tests/claims_cross_store_journey.rs` | 3 | 2 | a stale owner cannot release a successor's claim; a bad legacy claim refuses the whole migration (a partial import) |
| `crates/fno-agents/tests/claims_unreadable_root.rs` | 3 | 1 | claim list exits nonzero on an unreadable root (reads as an empty fleet); the one refusal case for this fail-closed guard |
| `crates/fno-agents/tests/discovered_sessions_parity.rs` | 3 | 0 | deleted: replay of goldens from the deleted Python leg; discovery keeps its src unit tests |
| `crates/fno-agents/tests/graph_readers_follow_backend.rs` | 3 | 1 | moved callers resolve the store node (a reader reads the frozen json mirror) |
| `cli/tests/events/test_ephemeral_set_parity.py` | 2 | 1 | Rust ephemeral const equals schema.yaml |
| `cli/tests/events/test_rust_events_documented.py` | 2 | 1 | Rust events name their producer source |
| `cli/tests/goldens/test_advance_golden.py` | 2 | 1 | explain report skeleton, declines to act |
| `cli/tests/goldens/test_triage_golden.py` | 2 | 1 | apply stamps priority change |
| `cli/tests/hooks/test_codex_stop_output_contract.py` | 2 | 2 | Stop group output codex-valid (exit-1 row only); nudge markdown rejected (positive control) |
| `cli/tests/integration/test_mail_hold_release.py` | 2 | 1 | held mail drains after expiry with no operator |
| `cli/tests/integration/test_rank_operator_only.py` | 2 | 1 | agent rank --top refused |
| `cli/tests/integration/test_worktree_cargo_build_base.py` | 2 | 1 | link outside both bases never deleted |
| `cli/tests/journeys/test_growth_studio_faucet.py` | 2 | 1 | packaged agent tool lists bounded |
| `cli/tests/journeys/test_growth_studio_pack.py` | 2 | 1 | overlay lowering approval floor refused |
| `cli/tests/lint/test_append_lesson_candidate.py` | 2 | 1 | valid candidate appends one line |
| `cli/tests/lint/test_architect_agent_contract.py` | 2 | 1 | architect keeps write access |
| `cli/tests/test_node_claim_release_authority.py` | 2 | 2 | no unsanctioned node release; matcher still sees sanctioned sites |
| `cli/tests/unit/plan/test_handoff_session_ids.py` | 2 | 1 | block-form append keeps old ids and later keys parseable |
| `cli/tests/unit/plugins/test_bundling.py` | 2 | 1 | the generator refuses to clobber a non-pack destination |
| `cli/tests/unit/test_backlog_decompose_door.py` | 2 | 1 | a plan-less epic is refused and names fno backlog contain |
| `cli/tests/unit/test_blueprint_batching_rule.py` | 2 | 1 | the lead 3d block states the number three |
| `cli/tests/unit/test_cargo_job_trigger_paths.py` | 2 | 1 | every cargo job input is a pull_request trigger |
| `cli/tests/unit/test_cleanup_lane.py` | 2 | 1 | cleanup writes zero attestations with a proven instrument |
| `cli/tests/unit/test_cli_help.py` | 2 | 1 | fno help matches --help; --all reaches hidden commands |
| `cli/tests/unit/test_cli_lint.py` | 2 | 1 | the lint hard-fails on an unwrapped lib script |
| `cli/tests/unit/test_command_collapse_dispatch.py` | 2 | 1 | a folded action reaches the original typed command |
| `cli/tests/unit/test_epic_status.py` | 2 | 1 | epic status JSON rollup (totals, receipt, merge flag) |
| `cli/tests/unit/test_leak_census.py` | 2 | 1 | reap_rooted takes the whole process tree within 5 s |
| `cli/tests/unit/test_llm.py` | 2 | 1 | an unstubbed llm_call under pytest is refused |
| `cli/tests/unit/test_partition_gate.py` | 2 | 1 | the partition holds on real models with overlap exactly {id} |
| `cli/tests/unit/test_paths_migration.py` | 2 | 1 | a foreign env spaces root never takes the checkout journal |
| `cli/tests/unit/test_sync_codex_agents.py` | 2 | 1 | codex agents are generated, fresh and parse |
| `cli/tests/unit/test_task_context_binding.py` | 2 | 1 | the gate refuses when a required source changed |
| `crates/fno-agents/src/backlog/carveout.rs` | 2 | 1 | ownerless row rides only default scope (scope leak) |
| `crates/fno-agents/src/backlog/cutover.rs` | 2 | 1 | shared plan file names open PR (collision missed) |
| `crates/fno-agents/src/backlog/decision_subject.rs` | 2 | 1 | old row split and stamped kind kept (kind lost) |
| `crates/fno-agents/src/backlog/decisions.rs` | 2 | 1 | import rejects orphan node ref (dangling ruling) |
| `crates/fno-agents/src/backlog/entities.rs` | 2 | 1 | empty harness refused by name (bad row) |
| `crates/fno-agents/src/backlog/fields.rs` | 2 | 2 | canonical write drains model tier (field drift); command gate refuses non-bare token (injection) |
| `crates/fno-agents/src/backlog/get_cli.rs` | 2 | 1 | id/slug/hex tiers (wrong node) |
| `crates/fno-agents/src/backlog/groom.rs` | 2 | 1 | daily claim once, failed launch hands back (double or lost groom) |
| `crates/fno-agents/src/backlog/migrations.rs` | 2 | 1 | model tier drain refuses divergent (data loss) |
| `crates/fno-agents/src/backlog/note_stale.rs` | 2 | 1 | newer note stale on commit basis (missed stale) |
| `crates/fno-agents/src/backlog/relatedness.rs` | 2 | 1 | dedup excludes epic bonus and own lineage (self/lineage reported as dup) |
| `crates/fno-agents/src/backlog/search.rs` | 2 | 1 | FTS triggers keep the index in step on save, retitle and delete (stale search hits) |
| `crates/fno-agents/src/backlog/session_cli.rs` | 2 | 1 | origin first-fill writes once and names the holder (origin overwritten) |
| `crates/fno-agents/src/claims_root.rs` | 2 | 1 | root routing requires colon and known prefix (bad key routed) |
| `crates/fno-agents/src/discovered_sessions.rs` | 2 | 1 | identity is harness+id (two harnesses with one id collapse) |
| `crates/fno-agents/src/hook/save_session.rs` | 2 | 1 | save writes once per window with safe names (duplicate or traversal filename) |
| `crates/fno-agents/src/org_board/claims.rs` | 2 | 1 | unreadable dir reads unreadable not empty (blind board reads clean) |
| `crates/fno-agents/src/org_remote.rs` | 2 | 1 | plan_handoff refusals and the ssh argv (a missing session id would plan a broken handoff) |
| `crates/fno-agents/src/pending_session_row.rs` | 2 | 1 | open appends one row, clears the park, stays idempotent |
| `crates/fno-agents/src/real_session.rs` | 2 | 1 | no home is never real |
| `crates/fno-agents/src/session_names_fold.rs` | 2 | 2 | the fold appends the missing alias; an alias another row holds refuses |
| `crates/fno-agents/src/session_receipt.rs` | 2 | 1 | a relay turn counts its edits and its classifier denial |
| `crates/fno-agents/src/state_lookup_tests.rs` | 2 | 1 | a keyed write hits only the resolved row; an ambiguous label refuses |
| `crates/fno-agents/src/team_widen.rs` | 2 | 2 | join table; drop table |
| `crates/fno-agents/tests/check_supersession.rs` | 2 | 2 | corpus matches the named rows; generated source is fresh (the freshness guard) |
| `crates/fno-agents/tests/claim_handover_manifest_journey.rs` | 2 | 2 | a vanished prior claim falls through to a fresh acquire; a suspect prior claim is adopted as a handover (each: the worker freezes on claim_held_by_other) |
| `crates/fno-agents/tests/claim_list_parity.rs` | 2 | 0 | deleted: replay of goldens from the deleted Python leg; claim list's nonzero exit stays in claims_unreadable_root |
| `crates/fno-agents/tests/claim_status_parity.rs` | 2 | 0 | deleted: replay of goldens from the deleted Python leg; claim status stays covered end to end in claim_status_join |
| `crates/fno-agents/tests/needs_items.rs` | 2 | 1 | an unreadable question store is named in sources with a nonzero exit (a failed read shows as no items) |
| `crates/fno-agents/tests/question_intake_quote_refusals.rs` | 2 | 1 | refusals quote the line they read and the shape they want (the refusal bytes change) |
| `crates/fno-agents/tests/session_truth_parity.rs` | 2 | 0 | deleted: replay of goldens from the deleted Python leg; session_truth keeps 14 src unit tests |
| `cli/tests/events/test_protocol_family.py` | 1 | 1 | public schema matches validator allowlist |
| `cli/tests/goldens/test_reconcile_golden.py` | 1 | 1 | no repo context refuses PR query |
| `cli/tests/graph/test_backlog_search_fts.py` | 1 | 1 | find uses native FTS index |
| `cli/tests/integration/test_e2e_hook_block.py` | 1 | 1 | Write to graph.json blocked (Edit row dropped) |
| `cli/tests/integration/test_graph_cli.py` | 1 | 1 | p0 birth needs acknowledgment |
| `cli/tests/integration/test_install_frontdoor_journey.py` | 1 | 1 | authorized target init journey |
| `cli/tests/integration/test_lane_slots_concurrency.py` | 1 | 1 | slot cap holds under race |
| `cli/tests/integration/test_law_skill_contract.py` | 1 | 1 | law skill requires under metadata |
| `cli/tests/integration/test_no_hardcoded_paths.py` | 1 | 1 | no hardcoded paths |
| `cli/tests/lint/test_agent_skill_loading.py` | 1 | 1 | no agent ships skills key |
| `cli/tests/lint/test_blueprint_discovery_contract.py` | 1 | 0 | deleted (prose pin, no credible failure) |
| `cli/tests/lint/test_codex_workflow_posture.py` | 1 | 1 | codex posture line byte pin |
| `cli/tests/lint/test_think_skill_size.py` | 1 | 1 | think payload byte pin |
| `cli/tests/lint/test_think_workflow_contract.py` | 1 | 0 | deleted (prose pin, no credible failure) |
| `cli/tests/test_autonomy_registry_ci.py` | 1 | 1 | unregistered spawner fails CI |
| `cli/tests/test_provider_exec_guard.py` | 1 | 1 | real provider blocked outside basetemp |
| `cli/tests/test_quota_refusal_and_failover.py` | 1 | 1 | failover abandonment without reason refused |
| `cli/tests/test_short_flag_dispatch.py` | 1 | 1 | backlog find short == long |
| `cli/tests/unit/test_ambient_canary.py` | 1 | 1 | dirty-lane positive control (run by path from tests/ci/test_hermetic_lanes.sh) |
| `cli/tests/unit/test_backlog_capture_tombstone.py` | 1 | 1 | legacy inbox_add rows still count in capture-pass |
| `cli/tests/unit/test_cli_wrappers_review_fixes.py` | 1 | 0 | file deleted |
| `cli/tests/unit/test_context_nudge_prescribed_commands.py` | 1 | 1 | hook output commands resolve against the baseline |
| `cli/tests/unit/test_dev_source_pin.py` | 1 | 1 | bootstrap.rs reads [dev].source (path named in config_schema.json text) |
| `cli/tests/unit/test_do_cli.py` | 1 | 0 | file deleted |
| `cli/tests/unit/test_docs_fences.py` | 1 | 1 | every docs toml/yaml fence parses as its language |
| `cli/tests/unit/test_evidence_refusal_remedy.py` | 1 | 1 | the unmeasured refusal names only remedies the commands support |
| `cli/tests/unit/test_exit_code_allocation.py` | 1 | 1 | gate-band exit codes are unique across both trees |
| `cli/tests/unit/test_graph_db_sealed.py` | 1 | 1 | no Python module opens graph.db |
| `cli/tests/unit/test_graph_worked_cli.py` | 1 | 1 | worked refuses when its authority is unavailable |
| `cli/tests/unit/test_init_harness_space_stub.py` | 1 | 1 | no new legacy state readers without the stub |
| `cli/tests/unit/test_keyless_smoke.py` | 1 | 1 | keyless dispatch-to-terminal smoke (a CI step runs it by path) |
| `cli/tests/unit/test_org_spelling_dispatch.py` | 1 | 0 | file deleted |
| `cli/tests/unit/test_paths_hermetic_fence.py` | 1 | 1 | graph_json refuses a path outside the sandbox |
| `cli/tests/unit/test_postcompact_reinject.py` | 1 | 1 | codex postcompact reads its own PLUGIN_ROOT and emits systemMessage (claude row cut) |
| `cli/tests/unit/test_prod_tripwire.py` | 1 | 1 | find_leaks names marked entries only; declared sandboxes unwatched |
| `cli/tests/unit/test_pytest_timeout_configured.py` | 1 | 1 | timeout 120, no fork Pool, marker bound |
| `cli/tests/unit/test_registry_mint_guard.py` | 1 | 1 | no direct AgentEntry( mint in src |
| `cli/tests/unit/test_state_canary.py` | 1 | 1 | populated-state canary control (named by the Rust smoke registry) |
| `cli/tests/unit/test_term_skill_links.py` | 1 | 1 | every relative link under skills/lead resolves |
| `cli/tests/unit/test_terminal_integrations.py` | 1 | 1 | DoneDelivery archives a finalized manifest |
| `cli/tests/unit/test_termination_event.py` | 1 | 1 | unknown termination reason fails loud |
| `cli/tests/unit/test_transition_rejected_event.py` | 1 | 1 | unknown transition_rejected kind fails loud |
| `cli/tests/unit/test_unknown_subcommand_exit.py` | 1 | 1 | unknown subcommand exits nonzero with a diagnostic |
| `cli/tests/unit/test_validate_plan_project_warning.py` | 1 | 1 | missing project: warns, exit 0 |
| `crates/fno-agents/src/attention_chat/bluebubbles.rs` | 1 | 1 | post, inline done, close (transport break) |
| `crates/fno-agents/src/attention_chat/discord.rs` | 1 | 1 | 429 retry, thread reply, close (transport break) |
| `crates/fno-agents/src/attention_chat/mod.rs` | 1 | 1 | only a threaded reply answers (stray message answers) |
| `crates/fno-agents/src/attention_chat/telegram.rs` | 1 | 1 | post, tap mapping, close (transport break) |
| `crates/fno-agents/src/backlog/commands.rs` | 1 | 1 | every legacy name maps once (alias loss) |
| `crates/fno-agents/src/backlog/edges.rs` | 1 | 1 | edge append, read, close (edge store break) |
| `crates/fno-agents/src/backlog/note_migrate.rs` | 1 | 1 | short JSON spelling parses (migration input rejected) |
| `crates/fno-agents/src/backlog/receipt.rs` | 1 | 1 | broken pipe on receipt neither panics nor propagates (EPIPE panic) |
| `crates/fno-agents/src/backlog/tombstones.rs` | 1 | 1 | removed name teaches its replacement, unknown name does not (wrong hint) |
| `crates/fno-agents/src/board_render_arm.rs` | 1 | 1 | arm renders a moved version once and backs off failures (render storm) |
| `crates/fno-agents/src/cargo_orphans.rs` | 1 | 1 | owner chain judges dead links and cap, stops below daemon (live cargo reaped) |
| `crates/fno-agents/src/claim_cli/help.rs` | 1 | 1 | every leaf answers help, removed leaf names its replacement (help text drift) |
| `crates/fno-agents/src/claim_cli/list.rs` | 1 | 1 | fmt_g byte pin (duration bytes drift) |
| `crates/fno-agents/src/discovered_sessions/aliases.rs` | 1 | 1 | failed write exposes no alias (half-written alias readable) |
| `crates/fno-agents/src/fleet_lanes.rs` | 1 | 1 | census splits roster into leads and workers (misclassified seat) |
| `crates/fno-agents/src/fleet_workspace.rs` | 1 | 1 | fresh file answers, stale/broken answers nothing (stale workspace served) |
| `crates/fno-agents/src/incident_claim.rs` | 1 | 1 | claim command names one owner end to end (two owners) |
| `crates/fno-agents/src/law_firings.rs` | 1 | 1 | counts only named law since cutoff (miscount) |
| `crates/fno-agents/src/law_renew.rs` | 1 | 1 | renew supersedes live law, refuses rest (renew of dead law) |
| `crates/fno-agents/src/law_restore.rs` | 1 | 1 | restore re-records retracted chat law, refuses rest (restore of operator law) |
| `crates/fno-agents/src/session_origin.rs` | 1 | 1 | parity test (fno-agents mirror vs fno crate module) deleted; parity_tests module removed |
| `crates/fno-agents/src/state.rs` | 1 | 1 | model substitution specimen table |
| `crates/fno-agents/src/state/tests/registry_lock_queue.rs` | 1 | 1 | a registry write queues behind a held graph lock |
| `crates/fno-agents/tests/accounts_state_read_parity.rs` | 1 | 0 | deleted: the read side runs through the accounts pick, usage, window and required_bot parity files |
| `crates/fno-agents/tests/accounts_state_write_parity.rs` | 1 | 1 | the one runtime-state writer, including the locked timeout branch (a clobbered file or a hang); src/accounts/state.rs has no unit tests |
| `crates/fno-agents/tests/backlog_ready_parity.rs` | 1 | 0 | deleted: the inventory marks the port complete; src/backlog_ready.rs keeps 17 unit tests |
| `crates/fno-agents/tests/claim_acquire_parity.rs` | 1 | 0 | deleted: replay of goldens from the deleted Python leg; acquire runs in the advance-doors, cross-store and handover journeys |
| `crates/fno-agents/tests/claim_classifier_parity.rs` | 1 | 1 | classifier state and basis over the frozen corpus (a live claim reads stale); src/claims_classify.rs has no unit tests |
| `crates/fno-agents/tests/claim_refresh_parity.rs` | 1 | 0 | deleted: replay of goldens from the deleted Python leg |
| `crates/fno-agents/tests/claim_release_parity.rs` | 1 | 0 | deleted: replay of goldens from the deleted Python leg; release runs in the advance-doors and cross-store journeys |
| `crates/fno-agents/tests/claim_session_pid_parity.rs` | 1 | 0 | deleted: a freeze of the native command's own output, with no second leg |
| `crates/fno-agents/tests/claims_root_parity.rs` | 1 | 0 | deleted: src/claims_root.rs keeps its routing unit tests |
| `crates/fno-agents/tests/fleet_top_leaf_argv.rs` | 1 | 1 | census --workers --machines --json answers the fleet map (the generic parser refuses --machines) |
| `crates/fno-agents/tests/team_parity.rs` | 1 | 0 | deleted: replay of goldens from the deleted Python leg; team render keeps src unit tests |
| `evals/fixtures/stage-tasks/review-decorative-test/test_clamp.py` | 1 | 1 | kept: eval task data read by evals/bank/capability-stage-review-decorative-test.yaml |
