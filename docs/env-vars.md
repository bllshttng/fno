# Environment variable registry

Every env var name either tree reads, one row each. The registry IS this table. `scripts/ci/check_env_registry.py` fails a read with no row, a row with no reader, and a wrong Read by. `--update` regenerates the table and preserves Meaning text.

A meaning not derivable from the read site stays `unclear: <file:line>`, never invented. A name read through an UPPER_SNAKE string constant resolves to its value. Any other variable stays unseen (scanner limit).

| Name | Read by | Meaning |
|------|---------|---------|
| `ANTHROPIC_API_KEY` | py | Anthropic API key; presence enables bare-key auth for the LLM lane. |
| `ANTHROPIC_BASE_URL` | rs | Overrides the Anthropic API base URL. |
| `ANTHROPIC_MODEL` | rs | Overrides the default Anthropic model. |
| `CARGO` | rs | Names the cargo binary the `cargo_build_dirs` lane runs `cargo metadata` through; the PATH scan and `$CARGO_HOME/bin/cargo` are the fallbacks. |
| `CARGO_BUILD_BUILD_DIR` | rs | unclear: crates/fno-agents/src/hook/stop.rs:519 |
| `CARGO_HOME` | py+rs | Cargo install root; the default is ~/.cargo. |
| `CARGO_MANIFEST_DIR` | rs | Cargo sets it on each process that it starts. The live-store fence then refuses to open a store under the operator home `.fno`. Only the admission call of the rustc wrapper removes it. |
| `CENSUS_DEFERRED_FILE` | rs | Overrides the census deferred set for a targeted run: crates/fno-agents/src/test_census.rs |
| `CENSUS_KILL_BOUND_S` | rs | Ceiling for each census entry before its group is killed: crates/fno-agents/src/test_census.rs |
| `CI` | py+rs | unclear: cli/src/fno/llm.py:40 |
| `CLAUDECODE` | rs | unclear: crates/fno-agents/src/hook/stop.rs:440 |
| `CLAUDECODE_SESSION_ID` | py+rs | unclear: crates/fno-agents/src/backlog/workflows.rs |
| `CLAUDE_CLI` | rs | unclear: crates/fno-agents/src/loop_dispatch.rs:186 |
| `CLAUDE_CODE_SESSION_ATTENDED` | rs | The Claude Code attended flag: `0` marks an unattended session, the second agent-cargo marker at the cargo admission doors. |
| `CLAUDE_CODE_SESSION_ID` | py+rs | unclear: cli/src/fno/carveout/core.py:202 |
| `CLAUDE_CODE_STOP_HOOK_BLOCK_CAP` | py+rs | Consecutive Stop-hook block cap the harness enforces before it force-ends the turn (Claude Code default 9). The run records whether the env override or the default bound. |
| `CLAUDE_CONFIG_DIR` | py+rs | Overrides the Claude config directory for managed provider lookups. |
| `CLAUDE_DIR_OVERRIDE` | rs | Redirects the Claude config root the corrections-verify reads the rule repo's git log from; mirrors the bash-side override in autocorrect-pack.sh. |
| `CLAUDE_EFFORT` | rs | The effort level an encounter vote records, on claude only. |
| `CLAUDE_JOB_DIR` | rs | Set by Claude Code in a `claude --bg` job. Its `state.json` names the job, and the SessionStart launch claim reads that name to match the launcher's record. The write gate also falls back to its tmp dir in a refusal when the payload carries no session id; the payload's session wins when present. |
| `CLAUDE_PLUGIN_ROOT` | rs | The plugin stage root the doctor's hook probes read. |
| `CLAUDE_SESSION_ID` | rs | unclear: crates/fno-agents/src/claims.rs:3690 |
| `CLI` | rs | unclear: crates/fno-agents/src/loop_dispatch.rs:198 |
| `CODEX_COMPANION_SESSION_ID` | rs | The Codex session id used as the owner-session fallback when a lazy mux server starts from an agent shell. |
| `CODEX_HOME` | py+rs | unclear: cli/src/fno/adapters/providers/managed.py:211 |
| `CODEX_SESSION_ID` | rs | unclear: crates/fno-agents/src/hook/lead_guard.rs:654 |
| `CODEX_THREAD_ID` | py+rs | The codex thread id: codex sets it per thread in child tool env (the root session keeps CODEX_SESSION_ID), never in its own process env. The rollout witness matches it against a daemon row at this cwd to complete a name_only pane's own identity. |
| `COLORFGBG` | rs | The rxvt-style `fg;bg` terminal color report; a background field of 7 or 15 reads as a light terminal, which picks the `footnote-paper` mux theme default, and anything else keeps `footnote-superscript`. |
| `COLORTERM` | rs | unclear: crates/fno/src/mux_cli.rs:1508 |
| `CRON_JOB` | py | unclear: cli/src/fno/agents/context.py:94 |
| `DATABASE_URL` | py | unclear: cli/src/fno/codemap_cli/db-schema.py:208 |
| `EDITOR` | rs | The editor the board's full description edit runs in; `vi` when unset. |
| `EVENTS_FILE` | rs | unclear: crates/fno-agents/src/verify_evidence.rs:932 |
| `FNO_A2A_NO_CONFIRM` | py | unclear: cli/src/fno/agents/dispatch.py:5376 |
| `FNO_AGENTS_ANSWER_MINUTES` | rs | The conversation mail-hold backstop in minutes: a hold armed on the user's last real message ends this long after it, whichever comes first with the Stop grace. Unset, the global config's `mail.hold.answer_minutes` reads; unset too, the default is 10. A value below 1 coerces to the default, one above 1440 clamps to 1440 (mail_hold.rs answer_minutes). |
| `FNO_AGENTS_BIN` | py+rs | Explicit override path to the fno-agents binary; unset, empty, or non-executable falls through to the search rather than failing. |
| `FNO_AGENTS_DAEMON_BIN` | rs | unclear: crates/fno-agents/src/client.rs:125 |
| `FNO_AGENTS_FAIL_STARTUP_RECONCILE` | rs | unclear: crates/fno-agents/src/daemon.rs:2093 |
| `FNO_AGENTS_FIXTURES` | rs | Overrides the scratch sweep fixtures directory. |
| `FNO_AGENTS_FRONT` | py+rs | unclear: cli/src/fno/graph/store.py:213 |
| `FNO_AGENTS_HOME` | py+rs | The agents home: the registry and each worker's socket directory. Default `~/.fno/agents`. |
| `FNO_AGENTS_IDLE_EXIT_SECS` | rs | unclear: crates/fno-agents/src/bin/daemon.rs:67 |
| `FNO_AGENTS_NAME_MODEL` | rs | Raw model string; the agent-name mint appends its short code to the worker name. |
| `FNO_AGENTS_NO_STARTUP_RECONCILE` | rs | unclear: crates/fno-agents/src/bin/daemon.rs:87 |
| `FNO_AGENTS_RESPONSE_DEADLINE_MS` | rs | unclear: crates/fno-agents/src/client.rs:68 |
| `FNO_AGENTS_RUNNER_BIN` | rs | Overrides the binary the detached `mail-hold --run-parked` runner execs (mail_hold.rs runner_bin); empty falls through to the process's own executable. A test suite that parks through the real gate pins a stub here so no detached runner escapes the sandbox. |
| `FNO_AGENTS_RUNTIME` | py+rs | unclear: cli/src/fno/doctor.py:564 |
| `FNO_AGENTS_SELF_SHORT_ID` | rs | The driven agent's short id, stamped by the PTY worker; the write gate matches it against open drive-authority windows so an operator drive scopes to that one session. |
| `FNO_AGENTS_STARTUP_RECONCILE_DELAY_MS` | rs | unclear: crates/fno-agents/src/daemon.rs:2082 |
| `FNO_AGENTS_TIMER_EXE` | rs | Overrides the binary the mail-hold release timer re-enters (mail_hold.rs spawn_release_timer); empty falls through to the process's own executable. A test suite that arms the standard release leg pins a stub here so no timer escapes the sandbox. |
| `FNO_AGENTS_WORKER` | py+rs | Marks the process as a footnote worker. |
| `FNO_AGENTS_WORKER_BIN` | py+rs | unclear: cli/src/fno/agents/dispatch.py:888 |
| `FNO_AGENT_HARNESS` | py+rs | unclear: cli/src/fno/harness_identity.py:77 |
| `FNO_AGENT_NAME` | rs | Worker name a note's thread row stamps as its agent_name when the writer's live claim carries no spawn-handover holder name (comment_identity, note_cli.rs). |
| `FNO_AGENT_NO_ROW` | rs | Set by the headless claude lane to the child's own `FNO_AGENT_SELF`. The child's SessionStart report then joins but writes no registry row, so a `claude -p` one-shot never shows in the mux. |
| `FNO_AGENT_PROVIDER` | rs | Pre-cutover name for `FNO_AGENT_HARNESS`. whoami reads it only when the new name is unset, and prints one rename line on stderr. |
| `FNO_AGENT_ROW_PENDING` | py+rs | unclear: cli/src/fno/agents/register_session.py:200 |
| `FNO_AGENT_SELF` | py+rs | unclear: cli/src/fno/agent/cli.py:363 |
| `FNO_AGENT_SESSION` | py+rs | unclear: cli/src/fno/agents/context.py:237 |
| `FNO_AGENT_SUBSTRATE` | py+rs | Spawn substrate of the calling agent (`pane`, `thread`, `headless`). The init door's work-start dispatch treats any value other than `pane` (unset included) as unattended. Read in crates/fno-agents/src/target_init.rs `substrate_marks_unattended`. |
| `FNO_ATTEST_DEBUG` | rs | Prints one stderr line naming the pull's slug, row count and primary url. Unset prints nothing; diagnostics only, never the data channel. Read in crates/fno-agents/src/attest.rs `run_pull`. |
| `FNO_ATTEST_ENV_ONLY` | rs | `1` makes the retract arm's retractor identity resolve as an environment-only witness (no process ancestry), skipping the CLAUDE_CODE_SESSION_ID conflict read; the identity-conflict unit test pins it. Read in crates/fno-agents/src/attest.rs `resolve_retractor_identity`. |
| `FNO_AUTO_CONTINUE` | rs | Env override of the merge-triggered auto-continue gate; truthy-set membership on the whole value, mirrored exactly from Python. |
| `FNO_BG` | rs | unclear: crates/fno-agents/src/target_init.rs:591 |
| `FNO_BIN` | py+rs | Overrides the Python fno porcelain path at the Rust/Python seam. |
| `FNO_BOARD_SCOPE` | rs | unclear: crates/fno/src/backlog_view.rs:333 |
| `FNO_BOOTSTRAP_WHEEL` | rs | unclear: crates/fno/src/bootstrap.rs:280 |
| `FNO_BUS_DIR` | py+rs | unclear: cli/src/fno/paths.py:1204 |
| `FNO_BUS_LOCK_TIMEOUT_SECS` | rs | Overrides the bus-append door's sidecar-lock wait; test escape hatch for the contended-lock refusal. |
| `FNO_BUS_MAX_BYTES` | rs | Size cap the bus-append door rotates the live log at; malformed degrades to 5 MB. |
| `FNO_BUS_RETAIN` | rs | Rotated bus segments the append door keeps; malformed degrades to 5. |
| `FNO_CALLER_KIND` | rs | The surface that shelled this fno-agents command; `mux` stamps `caller_kind` on its events. |
| `FNO_CAPABILITY_PARITY_DIR` | rs | unclear: crates/fno/src/agents_view.rs:3316 |
| `FNO_CAPABILITY_PARITY_JSON` | rs | unclear: crates/fno/src/agents_view.rs:3318 |
| `FNO_CARGO_CLASS` | rs | The lane an agent cargo asks for at the cargo admission doors: `repair` for a red PR or a main repair, `prepush` for the compile a push waits on. Unset is the normal lane. Both lanes queue ahead of normal, and a normal try-lock never takes a slot they wait for. |
| `FNO_CARGO_FREE_BYTES` | rs | Overrides the free-space read the `cargo_build_dirs` cap lane defends against; test escape hatch. |
| `FNO_CARGO_MAX_HOLD_SECS` | rs | The longest time in seconds that an agent cargo can hold an admission claim. After this time, the next cargo frees the claim and stops the holder. The default is 3600. A whole-suite run is exempt. |
| `FNO_CARGO_RUN_PROGRAM` | rs | The program cargo is about to start, which the rustc wrapper passes to the run door only. The door refuses an agent session's test binary or doctest with exit 86. The program itself never sees the variable. |
| `FNO_CARGO_TARGETS_BASE` | rs | Overrides the managed fno cargo build base the `cargo_build_dirs` lane sweeps and the tree-removal reclaim deletes under; test escape hatch. |
| `FNO_CC_DAEMON_RV_ROOT` | py+rs | unclear: cli/src/fno/agents/session_procs.py:40 |
| `FNO_CHROME` | rs | The Chrome or Chromium binary `fno mux serve --snapshot --format png` runs. |
| `FNO_CLAIMS_ROOT` | py+rs | The global claims tree root, else `$HOME`. `accounts::credentials::seal` pins it around a moved HOME so a worker's claims stay on the parent's root. |
| `FNO_CLAUDE_DAEMON_DIR` | py+rs | unclear: cli/src/fno/agents/discover.py:2353 |
| `FNO_CLAUDE_PROJECTS_DIR` | py+rs | Overrides the claude transcript projects root the announce status scan reads. |
| `FNO_CLAUDE_SESSIONS_DIR` | py | Overrides Claude Code's per-session registry directories on this host. |
| `FNO_CLAUDE_SESSION_RECENCY_SECONDS` | py | Transcript-mtime liveness window in seconds (positive only; default 600). |
| `FNO_CLAUDE_THEME` | py+rs | The Claude Code theme (`custom:fno:footnote-*`) the mux pins for a worker pane. A `claude --bg` spawn from that pane writes it into the thread's settings file. |
| `FNO_CODEX_ASK_WAIT_MS` | rs | unclear: crates/fno-agents/src/codex_thread.rs:56 |
| `FNO_CODEX_BIN` | rs | Overrides the codex CLI the readiness and upgrade paths resolve, for private roots and tests; PATH order otherwise. |
| `FNO_CODEX_INTERRUPT_BOUND_MS` | rs | unclear: crates/fno-agents/src/codex_thread.rs:80 |
| `FNO_CODEX_SESSIONS_DIR` | py+rs | Overrides the codex sessions root the announce status scan reads. |
| `FNO_CODEX_VERSION` | py+rs | Pins the codex version the hook-trust gate reads, so tests and private roots skip the `codex --version` probe. |
| `FNO_CONFIG` | py+rs | unclear: cli/src/fno/adapters/providers/loader.py:436 |
| `FNO_CONFIG_LOCK_TIMEOUT` | rs | Seconds `fno config set` waits for the config file lock before it refuses; unset waits without bound. The a2a first-use persist sets it. |
| `FNO_CONFIG_SEARCH_ROOT` | py | Test-only bound on config discovery, read by the Rust readers (`within_search_ceiling`). Python only widens it to the pytest basetemp (conftest). |
| `FNO_CONTEXT_OBSERVER_TIMEOUT_SECONDS` | rs | unclear: crates/fno-agents/src/context_run.rs |
| `FNO_CONTROL_PLANE_SCHEDULER` | py+rs | unclear: cli/src/fno/control_plane.py:22; native twin crates/fno-agents/src/backlog/reconcile_cli.rs |
| `FNO_CURSOR_AGENT_MODEL` | rs | unclear: crates/fno-agents/src/cursor_agent.rs:304 |
| `FNO_DEBUG` | py | unclear: cli/src/fno/agents/mux_spawn.py:1854 |
| `FNO_DISPATCH_ACCOUNT_ENV` | rs | The proven-credential carrier of a cutover spawn: a flat JSON map `accounts::launch` stages verbatim for `--dispatch-account`. The spawn door claims it, and a spawn without the flag refuses. |
| `FNO_DRIVER_LIB` | rs | unclear: crates/fno-agents/src/finalize.rs:849 |
| `FNO_DRIVER_LIB_DIR` | rs | unclear: crates/fno-agents/src/loop_target.rs:616 |
| `FNO_E2E` | rs | unclear: crates/fno/src/client.rs:539 |
| `FNO_E2E_CORE_WEDGE` | rs | unclear: crates/fno/src/server.rs:14343 |
| `FNO_E2E_DROP_PTY_EXIT` | rs | unclear: crates/fno/src/pty.rs:1862 |
| `FNO_E2E_DROP_RESIZE_FRAME` | rs | E2E fault seam: with `FNO_E2E` set, drops the first n (default 1) keeper resize frames the server sends, holding panes at their pre-change sizes the way a full keeper frame queue or a pre-ResizeAck keeper build would; the 1s grid reconciliation pass then converges the diff. |
| `FNO_E2E_PTY_OUTPUT_DELAY_MS` | rs | unclear: crates/fno/src/pty.rs:1873 |
| `FNO_EVENTS_PATH` | py+rs | Redirects the agents journal (events.jsonl) to one file; the hermetic guard honors it on both legs. |
| `FNO_FOOTNOTE_BIN` | rs | The `footnote` harness binary `-H footnote` launches, built from bllshttng/fnh; unset, fno-agents looks beside its own executable, then on PATH. |
| `FNO_FRONT_DOOR` | rs | Overrides the fno binary the footprint cause's rv-pane check runs, for tests; PATH order otherwise. |
| `FNO_GH_BUDGET_POINTS_PER_MIN` | rs | Overrides the fleet GitHub request budget cap in points per 60s window (default 450). |
| `FNO_GH_BUDGET_TEST_ARMED` | rs | Re-arms the in-process budget consult inside a unit-test build, which disarms it by default so the suite's gh-named stubs never stamp the machine-wide ledger; production builds ignore it. |
| `FNO_GH_CALLER` | rs | Pins the spend journal's caller label outright, checked per admission and ahead of the ancestry walk; a test or a wrapper that knows the caller better than the ancestor argv sets it. |
| `FNO_GH_CORE_RESERVE` | rs | The core bucket's reserve before exhaustion: below it, admission refuses only the recurring pollers (daemon arms, watch loops, status waits) so the last requests of the bucket land on merges and pushes, not watch loops. `0` disables the reserve refusals; the default is 500. Exhaustion (remaining 0) refuses every core request regardless. |
| `FNO_GH_FACTS_DIR` | rs | Overrides the directory the `gh-cache` command reads and writes its permanent fact rows from; default `<state_dir>/cache/gh-facts`. |
| `FNO_GH_PROXY_DEPTH` | rs | unclear: crates/fno-agents/src/pr_status/proxy.rs:146 |
| `FNO_GH_PROXY_DIR` | rs | Names the directory holding the `gh` proxy shim. The Python front sets it for its subprocesses (prepend the shim to PATH), and the Rust transport consult reads it to stand down when a spawned `gh` resolves into it (unknown always means charge). |
| `FNO_GLOBAL_SETTINGS_PATH` | py+rs | unclear: cli/src/fno/adapters/providers/loader.py:48 |
| `FNO_GRAPH_JSON` | rs | Names the stable graph store anchor; the SQLite store is the `.db` sibling. |
| `FNO_GUARD_TRACE` | rs | unclear: crates/fno-agents/src/hook/lead_guard.rs:25 |
| `FNO_HARNESS` | rs | The arming session's harness, the fallback a new lead manifest names when the holder row carries none. |
| `FNO_HARNESS_NAME` | py+rs | The caller's harness name in the canonical identity stamp; the hold gate's direct read pairs it with FNO_HARNESS_SESSION_ID to answer whether the caller is the recipient. |
| `FNO_HARNESS_SESSION_ID` | py+rs | The normalized full harness session id; native context hooks use it when the provider-specific id is absent. |
| `FNO_HEALTH_HISTORY` | rs | crates/fno-agents/src/backlog/triage_health.rs (history_path) |
| `FNO_HOME` | py+rs | Relocates fno's config-free sidecars (plugin-root pointer, push-stamps, corrections log, decision index) and the Rust defaults when no config sets state_dir. Does not move state_dir or the backlog; a store write under FNO_HOME that targets the default store ($HOME/.fno) is refused. Sandbox with FNO_CONFIG (docs/path-config.md). |
| `FNO_HOOK_CONTEXT` | rs | Set to 1 on every hook command; an fno call that carries it never joins its session to fno. Spawned children have it cleared. |
| `FNO_IDLE_EXIT_GRACE_MS` | rs | unclear: crates/fno/src/server.rs:14324 |
| `FNO_INBOX_ROOT` | py+rs | unclear: cli/src/fno/inbox/store.py:218 |
| `FNO_INIT_PROGRESS_FILE` | rs | Step-marker file `fno do target start` hands its init door; the door appends begin/done markers and the parent prints the last marker in the 540s timeout receipt. Unset (a bare `fno do target init`), the markers are skipped. See target_init.rs `init_progress`. |
| `FNO_INSTALL_BUILD` | rs | unclear: cli/src/fno/update.py:1748 |
| `FNO_KEEPER_ORPHAN_POLL_MS` | rs | Pane keeper poll interval in ms for the socket-dir-gone check; default 30000. Tests set it low. |
| `FNO_KEEP_GOING` | rs | Forces the retro keep-going engine on or off above `keep_going.enabled`; truthy words are 1, true, yes, on. |
| `FNO_KILLCHECK_GIT_BIN` | rs | unclear: crates/fno-agents/src/kill_criteria.rs:58 |
| `FNO_LAUNCH_ACCOUNT` | rs | Launch account the spawn seam stamps after it applies an account overlay: the Rust spawn arm, in process. Cleared on a spawn with no account so a stale value never mislabels the next row. |
| `FNO_LAUNCH_ACCOUNT_SOURCE` | py+rs | Provenance twin of FNO_LAUNCH_ACCOUNT: `caller` for a typed flag, `config` for an env-carried pick. |
| `FNO_LAW_DOOR_TIMEOUT_SECONDS` | py | Law-door timeout knob in seconds; an explicit `timeout=` argument outranks it, junk falls back to 60. |
| `FNO_LEAD_WALK_SESSION_KEY` | rs | Per-invocation key a lead walk is spawned with; tags the pass's terminal so the walk's resume guard never sees a prior lead's terminal (falls back to the manifest fno_id). |
| `FNO_LLM_STUB` | py+rs | unclear: cli/src/fno/llm.py:38; crates/fno-agents/src/backlog/triage_cli.rs (llm_one_shot) |
| `FNO_LOOPCHECK_FNO_BIN` | rs | Overrides the fno path the loopcheck shim calls. |
| `FNO_LOOPCHECK_GH_BIN` | rs | unclear: crates/fno-agents/src/loopcheck.rs:7891 |
| `FNO_LOOPCHECK_GIT_BIN` | rs | unclear: crates/fno-agents/src/loopcheck.rs:7892 |
| `FNO_LOOPCHECK_MIN_FIRE_GAP_SECS` | rs | unclear: crates/fno-agents/src/loopcheck.rs:7447 |
| `FNO_LOOPCHECK_NO_COMMENT` | rs | unclear: crates/fno-agents/src/loopcheck.rs:3533 |
| `FNO_LOOPCHECK_NO_NOTIFY` | rs | unclear: crates/fno-agents/src/loopcheck.rs:3514 |
| `FNO_LOOPCHECK_READ_TIMEOUT_MS` | rs | unclear: crates/fno-agents/src/loopcheck.rs:7900 |
| `FNO_LOOPS_MAIL_BIN` | rs | Overrides the mail-hold helper for `loops pause-all`/`resume-all`; status and arm default to `fno` beside the running binary, then PATH, while owner release uses the current `fno-agents` binary. Lets a test point it at a stub. |
| `FNO_MACHINE_BRAKE` | rs | Overrides the machine runaway brake file path (default `$HOME/.fno/machine-brake.json`, written by the machine arm, honored by spawn admission). Lets a test point it at a tempdir. An armed brake refuses agent-origin callers (a non-empty `FNO_AGENT_SELF`) and admits everyone else with one warning that names the fleet/machine core split the arm measured. |
| `FNO_MACHINE_LOAD_ASK` | rs | Overrides the machine arm's pending-ask file path (default `$HOME/.fno/machine-load-ask.json`, written by the machine arm, holding the open outside-load or budget question's qid, the groups an answer would act on, and the pids a pause holds). Lets a test point it at a tempdir. |
| `FNO_MAIL_FENCE` | rs | `1` on the pane-prepare child only (`mux_cli.rs`): the envelope render fences the delivered body (open fence marked `fno-pane`). Hook and mail lanes leave it unset and render unfenced. |
| `FNO_MAIL_HOP` | rs | The relay depth a `mail-remote` forward stamps on the process it starts (`mail_remote.rs`). The front door clears it on every route but the relay's own. An entry at depth 3 refuses with `relay hop limit`, exit 2, so a forwarding loop stops after two hops. |
| `FNO_MAIL_LANDED_SETTLE_S` | py+rs | Seconds a just-sent durable row waits before the send command re-reads the recipient for the `landed`/`NOT LANDED` verdict. Tests read 0. |
| `FNO_MAIL_LOCAL_FORWARD` | rs | `1` on the same-host forward `mail-remote` execs (`mail_remote.rs`). The front door's cross-host intercept skips any argv carrying it, so a forwarded send reaches the local command instead of re-entering the relay. |
| `FNO_MAIL_SUBJECT` | py+rs | The peeled `mail send --subject`; the front exports it, the envelope render and the bus-append door stamp it as `meta.subject`. |
| `FNO_MERGE_AUTHORITY` | rs | Selects the `fno do pr merge` authority arm: `durable_grant` swaps the manifest posture for the durable-grant resolution a parked worker rides; unset or any other value rides the manifest fields as written. |
| `FNO_MERGE_STEP_TRACE` | rs | Non-empty: each probe of the authorized-merge decide chain prints a began and a took line on stderr. `fno do pr merge` sets it for its child, so a timeout names the step the child was inside. Unset prints nothing. |
| `FNO_MUSE_BIN` | rs | The `muse` binary `-H muse` launches for `muse serve` and `muse exec`; unset, fno-agents looks on PATH. |
| `FNO_MUX_ADMISSION_NAMESPACE` | rs | unclear: crates/fno/src/process_admission.rs:733 |
| `FNO_MUX_DIR` | rs | unclear: crates/fno/src/mux_cli.rs:1533 |
| `FNO_MUX_IMAGE_PROTOCOL` | rs | Forces the mux client's image protocol for the file viewer: `kitty`, `iterm2`, `sixel` or `halfblocks`. Unset, the client asks the terminal once at start. |
| `FNO_MUX_MOUSE_TRACE` | rs | unclear: crates/fno/src/client.rs:11590 |
| `FNO_MUX_NATIVE_TEST_ADMISSION` | rs | unclear: crates/fno/src/process_admission.rs:416 |
| `FNO_MUX_OPENPTY_HANG_MS` | rs | unclear: crates/fno/src/pty.rs:1355 |
| `FNO_MUX_OPENPTY_TIMEOUT_MS` | rs | unclear: crates/fno/src/pty.rs:1341 |
| `FNO_MUX_PANE_GROUP_MAX` | rs | unclear: crates/fno/src/process_admission.rs:378 |
| `FNO_MUX_SCROLLBACK_LINES` | rs | unclear: crates/fno/src/vt.rs:68 |
| `FNO_MUX_SHELL_INTEGRATION` | rs | unclear: crates/fno/src/client.rs:577 |
| `FNO_MUX_STATS_JSON` | rs | The mux server's stats snapshot the front door hands the native scoreboard door (`server_stats::snapshot_for_scoreboard` sets it; `scoreboard/door.rs` reads it): the human_touch emission-failure counter and its measurement window as one JSON object. Unset, the door reports the counter unavailable, never zero. |
| `FNO_MUX_WINDOW_SIZE` | rs | Which window sizes a tab that several windows view: `smallest`, `latest` (default) or `largest`. The client latches `mux.window_size` into it at server spawn; see docs/architecture/mux-window-size.md. |
| `FNO_NODE` | py+rs | unclear: cli/src/fno/agents/harnesses/claude.py:725 |
| `FNO_NODE_CLAIM_HOLDER` | rs | unclear: crates/fno-agents/src/backlog/session_cli.rs:370 |
| `FNO_NODE_REASON` | py | unclear: cli/src/fno/agents/mux_spawn.py:4569 |
| `FNO_NOTIFY_SIGNALS` | rs | unclear: crates/fno-agents/src/operator_notice.rs:82 |
| `FNO_NO_CANONICAL_CONFIG` | py+rs | unclear: cli/src/fno/config/__init__.py:4677 |
| `FNO_NO_OPEN` | rs | unclear: crates/fno-agents/src/backlog/projections.rs:949 |
| `FNO_NUDGE_DISABLED` | rs | unclear: crates/fno-agents/src/nudge.rs:32 |
| `FNO_OBSERVER_GH_CAP` | rs | crates/fno-agents/src/observer/mod.rs `gh_cap` |
| `FNO_OBSERVER_PR_LIST_LIMIT` | rs | crates/fno-agents/src/observer/mod.rs `pr_list_limit` |
| `FNO_OPENCODE_BIN` | rs | Names the opencode binary an install classifies and asks for its config root; unset or empty falls back to `opencode` on PATH. |
| `FNO_OPENCODE_LIVE_TOKEN` | rs | unclear: crates/fno-agents/src/opencode_archive_tests.rs:340 |
| `FNO_OPENCODE_LIVE_URL` | rs | unclear: crates/fno-agents/src/opencode_archive_tests.rs:338 |
| `FNO_OPENCODE_STORAGE_DIR` | py+rs | Moves opencode's storage root, and with it the sibling `opencode.db` session store: the discover default and the dispatch brief's transcript tail read the store beside it. |
| `FNO_OPERATOR_CAPTURE_DIR` | rs | Moves the operator-capture dir that holds each session's ack ledger and scan cursor; unset, it is `<state dir>/operator-capture`. |
| `FNO_ORPHANS_SKIP_PROBE` | rs | unclear: crates/fno-agents/src/orphans.rs:706 |
| `FNO_OWNER_BIRTH` | rs | The owner process start token paired with FNO_OWNER_PID; protects sandbox process cleanup from PID reuse. |
| `FNO_OWNER_PID` | rs | The owner process for an owner-bound sandbox server and its descendants; shared fleet launches leave it unset. |
| `FNO_OWNER_SESSION` | rs | The harness session id that owns a sandbox server and descendants; required with FNO_OWNER_PID and FNO_OWNER_BIRTH. |
| `FNO_PANE` | rs | The pane id every inline (`pty.rs`) and keeper-hosted (`pane_keeper.rs`) pane exports. Read by `crates/fno-agents/src/pane_claim.rs` to bind a foreground SessionStart report's registry row to this pane, proven by an ancestry walk so a process that merely inherited it (a long-lived server born once inside a pane) is refused. |
| `FNO_PANE_EPOCH` | rs | The pane's restart epoch; with `FNO_PANE` and `FNO_SERVER` it names the turn-pin file whose first writer reports the leg's session state (session_state.rs). |
| `FNO_PANE_STATS_EMIT` | rs | unclear: crates/fno/src/server.rs:10563 |
| `FNO_PI_MODEL` | py+rs | unclear: cli/src/fno/agents/harnesses/pi.py:127 |
| `FNO_PI_PROVIDER` | py+rs | unclear: cli/src/fno/agents/harnesses/pi.py:122 |
| `FNO_PLANS_DIRS_CACHE_DIR` | rs | Overrides the plans-dirs cache directory the `state plans-dirs` command reads and writes; default `<state_dir>/cache/plans-dirs-v1.txt`. |
| `FNO_PLATFORM` | rs | unclear: crates/fno-agents/src/hook/lead_guard.rs:400 |
| `FNO_PROBE_FNO` | rs | Names the fno porcelain the isolated live journey drives; the default is the PATH `fno`, and the override exists so a probe run can exercise a specific checkout. |
| `FNO_PROBE_MODEL` | rs | The model the isolated live journey spawns with (`--model`). A model-borrowing harness such as footnote needs it, and a strict routing config refuses a spawn without it. |
| `FNO_PROBE_PROVIDER` | rs | The provider the isolated live journey spawns with: `--provider` on claude, the `--model` prefix (`<provider>/<model>`) on footnote and opencode. The probe copies only this provider's `model_routing.providers` record into the isolated config. |
| `FNO_PROCESS_ADMISSION` | rs | unclear: crates/fno/src/bootstrap.rs:1721 |
| `FNO_PROCESS_ADMISSION_MAX` | py+rs | unclear: cli/src/fno/agents/mux_spawn.py:1914 |
| `FNO_PR_BASE_LINEAGE_OK` | rs | Set to `stale-acknowledged` by an operator who has judged a stale-base lineage refusal: the base-lineage probe records a gate-escape event and clears instead of refusing. |
| `FNO_PR_BASE_OK` | rs | Set to `stale-acknowledged` to ship past a stale-base refusal on a deliberately old base; the batch-lane ship guard honors it (the preflight guard reaches it through its BYPASS_ENV constant, which the scanner does not see). |
| `FNO_PR_STATUS_CACHE_DIR` | rs | unclear: crates/fno-agents/src/gc.rs:2231 |
| `FNO_PR_STATUS_TTL` | rs | unclear: crates/fno-agents/src/pr_status/cache.rs:32 |
| `FNO_PY` | rs | Overrides the resolved fno-py console script path (tests and nonstandard installs); empty falls through to the resolver legs. The Rust front door honors it ABOVE the cached sentinel, so a study venv can repoint `fno` at its own env (gap-audit 9); set but unusable refuses naming the var. `fno version` names the resolved target on its `python-cli:` line, and `fno version --json` carries it as `python_script`. |
| `FNO_PY_HANDOFF` | py | Set to `1` on the child when fno-py hands a Rust-front command to the `fno` binary. A re-entry with it already set refuses: the PATH `fno` resolved back to fno-py, and without the marker each level spawns another (the 2026-10-02 mux-prune flood). |
| `FNO_REAL_GH` | rs | Overrides the gh the quota broker delegates to, bypassing shim detection when it is a real executable outside the proxy dirs. Read in crates/fno-agents/src/pr_status/quota.rs `resolve_real_gh`. |
| `FNO_RECLAIM_BACKUPS_AUDIT` | rs | Path to a state-recovery audit verdict JSON (`zero_missing` must read true) that authorizes the reclaim sweep's `backups_retention` lane to delete migration stamps past the retention. The audit has no persisted receipt, so the operator redirects one there; stamps newer than the file's mtime were never audited and always survive. Absent, unparsable, or `zero_missing` false keeps the lane report-only. |
| `FNO_RECLAIM_BACKUPS_DAYS` | rs | Overrides `reclaim.backups_retention_days` for the reclaim sweep's `backups_retention` lane: the days a `backups/state-root-migration` stamp dir may sit before the sweep reaps it, newest 5 always kept, and only past a zero-missing audit verdict (`FNO_RECLAIM_BACKUPS_AUDIT`). Zero reads as disabled; unset (or unparsable) falls through to the config knob and, when both are unset, the lane is report-only. Lets a test or an ops session arm the retention without editing global config. |
| `FNO_RECLAIM_STATE_ROOT` | rs | unclear: crates/fno-agents/src/plugin_install.rs:22 |
| `FNO_RECLAIM_TEMP_ROOT` | rs | unclear: crates/fno-agents/src/reclaim.rs:80 |
| `FNO_REGISTRY_ALLOW_ROW_LOSS` | rs | Set to 1 when a registry write that drops live rows is deliberate. The bulk-loss guard refuses otherwise. |
| `FNO_RELEASE_REPO` | rs | The `owner/repo` whose `bin-cache` pre-release `fno update` downloads the CI-built binaries from; the default is bllshttng/fno (`crates/fno/src/update_prebuilt.rs`, `crates/fno-agents/src/sync_canonical.rs`, `scripts/install/fno.sh`), pending the GitHub repo rename. The curl installer reads the same name for the nightly. |
| `FNO_REPO_ROOT` | py+rs | unclear: the storage-root ladder reads it first (crates/fno-agents/src/outstanding.rs); the law matcher reads it to place the project events journal (crates/fno-agents/src/law_match.rs). |
| `FNO_REVIEW_HOLD_GH` | rs | Overrides the gh binary the review-hold door's PR head read shells out to (review_hold.rs `pr_head_ref_and_oid`); unset resolves plain `gh`. A test suite pins a stub here so no real gh call escapes the sandbox. |
| `FNO_REVIEW_INVOCATION_ID` | rs | unclear: crates/fno/src/mux_cli.rs:6090 |
| `FNO_REVIEW_ROUND` | rs | The verified round a --verify-fixes pass declares, carried to the disposition comment when the shell producer emits; the attest flow's function argument outranks it. Set by skills/review/scripts/emit-attestation.sh, read in crates/fno-agents/src/review_findings.rs `resolve_round`. |
| `FNO_RM_ASSUME_YES` | py | Assume yes for the rm confirm prompt. |
| `FNO_RM_NOTICE_SHOWN` | py | Set to 1 once the resume-handle notice prints, so it shows once per process. |
| `FNO_ROLES_ROOT` | rs | The business-role definition root; unset means `<repo root>/.fno/roles`. Role discovery and the business-role tri-state read it in crates/fno-agents/src/roles/registry.rs `default_role_root`. |
| `FNO_ROUTE_MODEL` | rs | Fallback model a thread row stamps when the caller named no model and the writer session's observed model is absent (comment_create, api.rs). |
| `FNO_ROUTE_PROVIDER` | py+rs | unclear: cli/src/fno/agent/cli.py:303; the lead check-in's blueprint reading also reads it (crates/fno-agents/src/lead_checkin.rs r_blueprint) to pick the blueprint-subagent ceiling. |
| `FNO_ROUTE_SETTINGS_DIR` | rs | unclear: crates/fno-agents/src/claude_adopt.rs:112 |
| `FNO_ROUTE_SLOT_DEBUG` | py | unclear: cli/src/fno/rust_binary.py:224 |
| `FNO_RUNTIME_STATE_PATH` | py+rs | Overrides the provider runtime-state file (quota locks, usage); the default is ~/.fno/runtime-state.json. |
| `FNO_SCCACHE` | rs | Set to `1` to opt a build into sccache; the rustc wrapper compiles with bare rustc unless this or `build.sccache = true` in `.fno/config.toml` (project first, `~/.fno/config.toml` as the fallback) says so. The daemon's server keep-alive reads the same signal from the global config and this env. `SCCACHE_DISABLE=1` still wins over an opt-in. |
| `FNO_SCCACHE_WATCH` | rs | Pins the sccache-watch state file (last server pid + restart timestamps, written per machine-watch tick); tests use it to keep off the live machine's file. |
| `FNO_SERVER` | py+rs | Names the target mux server; the session-state entry also reads it as the namespace of the turn-pin file (session_state.rs). |
| `FNO_SESSION` | py+rs | Deprecated alias of FNO_SERVER; the Rust pane-send audit row also reads it as the calling session the send came from. |
| `FNO_SESSION_HARNESS` | rs | The launcher-stamped harness half of the session-proof pair; a known name beside a live `FNO_SESSION_PID` answers the harness before the census walk (spawn_context.rs stamp_pair_harness). |
| `FNO_SESSION_ID` | rs | Current session id. `fleet-incident` uses it for caller attribution; `loops pause-all` records a full UUID or `ses_` id as the mail-hold owner, falling back to `fno whoami --json` otherwise. |
| `FNO_SESSION_PID` | rs | The launcher-stamped pid half of the session-proof pair; must be a positive, live pid or the pair is ignored (spawn_context.rs stamp_pid_is_live). |
| `FNO_SIDECAR_SOCKET` | rs | Overrides the MCP sidecar's socket path (default `$XDG_RUNTIME_DIR/fno/sidecar.sock`, else the state dir's `sidecar.sock`). The client's lazy-start forwards it to the spawned sidecar, so a caller asking for a non-default path never races the default. Set by callers and tests; read at `crates/fno-agents/src/mcp/sidecar.rs` (`socket_path`). |
| `FNO_SKIP_MIGRATION` | py | unclear: cli/src/fno/cli.py:402 |
| `FNO_SOURCE` | rs | unclear: crates/fno/src/doctor_update.rs:388 |
| `FNO_SPACES_DIR` | py+rs | unclear: cli/src/fno/paths.py:299 |
| `FNO_SPAWN_GATE` | rs | unclear: crates/fno-agents/src/backlog/dispatch_resolve.rs:911 |
| `FNO_SPAWN_ORIGIN` | py+rs | Explicit dispatch-origin JSON the spawn door validates onto the request; malformed refuses. |
| `FNO_SPAWN_OWNER` | py+rs | Explicit dispatch-owner JSON the spawn door validates onto the request; must be exported together with FNO_SPAWN_ORIGIN. |
| `FNO_SPAWN_SHOW_ROW` | rs | Set to `1` by the front door's `fno agents spawn --mux`. The headless claude lane then keeps the run's registry row, so the one-shot shows in the mux while it runs. Never passed on to the child. |
| `FNO_SPAWN_TAKE_OVER` | rs | Refusal marker only: nothing sets it, so a read means a forgery or a stale leak, and the compose door refuses the spawn. The take-over reason rides the compose answer, never the environment. |
| `FNO_SPAWN_TRIGGER` | py+rs | unclear: cli/src/fno/agents/dispatch.py:860 |
| `FNO_STATE_DIR` | py+rs | Pins fno's config state root: `state_dir` and `locks_dir` read it ahead of their config/`$HOME` defaults, and the Rust default graph path and the backlog porcelain reads serve from it ahead of config. `accounts::credentials::seal` sets it around a forwarded HOME so a spawned worker's graph, ledger, and locks stay on the parent's root. |
| `FNO_STATE_ROOT_INVENTORY_DOC` | rs | Overrides the inventory doc the state-root drift reading parses (tests and custom installs); empty falls through to the plugin-stage copy beside the state root (crates/fno-agents/src/state_root_drift.rs). |
| `FNO_STORE_EXEC_LOG` | rs | Opt-in request log for sizing a shared store: set to a file path and each request appends one JSON line. Unset, the lane pays nothing. |
| `FNO_STORE_KEEPER_DRIFT_CHECK_SECS` | rs | unclear: crates/fno-agents/src/graph_keeper.rs:654 |
| `FNO_STORE_KEEPER_IDLE_SECS` | rs | unclear: crates/fno-agents/src/graph_keeper.rs:115 |
| `FNO_STORE_KEEPER_RSS_KB` | py | Store keeper resident-memory bound in KiB for the watchdog's over-bound reap verdict; overrides the 2 GiB default. |
| `FNO_STYLE_ENFORCE` | rs | Zero disables the mail style gate and the encounter prose check at their readers (send vet_body, send_raw, team, pane-prepare, encounter_cli.rs:115); any other value enforces. |
| `FNO_SUBAGENT_LIVE_SECONDS` | rs | Positive seconds a sidechain transcript counts as active on mtime (default 600); the Rust check-in held-subagents reading reads it directly (subagent_hold.rs), and the `fno agents fleet top --subagents` scan (agents_top.rs) reads it through the same `live_threshold`. |
| `FNO_SYNC_EVENTS_SCHEMA` | rs | Regenerate-on-build escape for the tracked events-schema copies: unset or any value but `1` keeps the build checking them (drift fails the build); `1` restores writing so a drift can be regenerated and committed. |
| `FNO_TASK_CONTEXT_FILE` | rs | Absolute path to the executing attempt's bound task-context binding; a declared value gates `fno do target init`, embeds into written handoff receipts, and rides spawn payloads (rendered natively). |
| `FNO_TEST_BUILD_IDLE_SECS` | rs | Test seam: seconds a build-admit waiter lets the `build:cargo` holder run no compile before it takes the slot (default 30), so admission tests need not wait out the real window. |
| `FNO_TEST_BUILD_SLOTS` | rs | Test seam: pins how many agent cargos may compile at once (0 to 3) in place of the load read, so admission tests do not depend on the load of the machine that runs them. |
| `FNO_TEST_CLAUDE_CONFIG_LOG` | rs | Test seam: records the config root passed to `claude agents --json --all` while testing a pinned account root. |
| `FNO_TEST_FOOTPRINT_PAYLOAD` | rs | Test seam: when set, the spawn gate's footprint probe returns this payload verbatim, so gate tests pin the CPU axis instead of reading the live machine. |
| `FNO_TEST_FOOTPRINT_PAYLOAD_SEQ` | rs | Test seam: newline-separated footprint probe results consumed once per read; `ERR <message>` simulates probe failure, and the last line sticks so gate tests can verify retries and sample counts. |
| `FNO_TEST_FULL` | rs | The whole-suite lane mark: the guard reads it typed on the command, and the test-run door stamps it into a whole run's cargo env, so its slot asks queue instead of try-locking. |
| `FNO_TEST_HERMETIC` | py+rs | unclear: cli/src/fno/hermetic.py:557 |
| `FNO_TEST_JOURNAL` | rs | Test seam: the global events journal the worktree overlap record and report ops read and write, so the parity goldens never touch the real state root. |
| `FNO_TEST_LIVE_CARGO_CWDS` | rs | Test seam: colon-separated cwd paths that stand in for a live `lsof` scan of running cargo processes, so cargo_build_dirs tests can drive the tree-to-shard mapping without a real cargo process. |
| `FNO_TEST_MARKER_HOLD_MS` | rs | unclear: crates/fno/src/proto/startup_guard.rs:97 |
| `FNO_TEST_MODE` | py | unclear: cli/src/fno/setup/doctor.py:229 |
| `FNO_TEST_NEVER_WAIT_AFTER_SECS` | rs | Test seam: seconds a cargo admission waiter waits before printing the CI-first push reminder. The default is 0, so a waiting agent hears "push now" from its first line. |
| `FNO_TEST_NOW` | rs | Test seam: an RFC3339 instant the worktree overlap ops use as now, so the parity goldens pin the fold window. |
| `FNO_TEST_ORPHAN_MIN_ELAPSED_SECONDS` | rs | Env override of `config.test.orphan_min_elapsed_seconds` (900). Lets a positive control plant a fresh orphan and still exercise the kill. |
| `FNO_TEST_OWNED_HOLD_MS` | rs | unclear: crates/fno/src/proto/startup_guard.rs:109 |
| `FNO_TEST_OWNER_BIRTH` | rs | unclear: crates/fno-agents/src/test_run.rs:145 |
| `FNO_TEST_OWNER_PID` | rs | unclear: crates/fno-agents/src/test_run.rs:144 |
| `FNO_TEST_PR_WATCH_LAUNCHCTL` | rs | Test seam: scripts the launchctl steps the `pr-watch refresh` bounce runs (a JSON `[[rc, timed], ...]` consumed in invocation order) so the parity goldens never touch the real tool. |
| `FNO_TEST_PR_WATCH_LAUNCH_AGENTS_DIR` | rs | Test seam: the LaunchAgents dir the `pr-watch status` command reads the plist from, standing in for `$HOME/Library/LaunchAgents` so the parity fixtures pin the plist the way the Python leg's `launch_agents_dir` parameter did. |
| `FNO_TEST_PR_WATCH_LOADED` | rs | Test seam: pins the launchctl load state the `pr-watch status` command reports (`1` loaded, anything else not loaded) so the parity goldens do not depend on the capture machine's own registration. |
| `FNO_TEST_PR_WATCH_TICK_PID` | rs | Test seam: pins the tick-in-flight answer the `pr-watch refresh` command defers on (`0` reads as no tick, a positive pid defers the bounce) so the parity goldens never probe the real launchd. |
| `FNO_TEST_TIMEOUT_SECONDS` | rs | Overrides the suite timeout the native test command passes the bounded owner: crates/fno-agents/src/test_command.rs |
| `FNO_THINK_SPAWN` | rs | Forces the born-with-why /think spawner on or off above `think_spawn.enabled`, below the autonomy master switch; `fno agents admit autonomy status` reports it as the `env` rank. The native test command pins it to 0 in every hermetic child: crates/fno-agents/src/test_env.rs. |
| `FNO_THINK_SPAWN_WAVE0` | rs | unclear: crates/fno-agents/src/backlog/decompose.rs:2382 |
| `FNO_THREAD_TURN_REFRESH_MS` | rs | unclear: crates/fno-agents/src/codex_thread.rs:105 |
| `FNO_TOUCH_EMIT` | rs | unclear: crates/fno/src/server.rs:10464 |
| `FNO_TRACKER_BACKEND` | py+rs | unclear: the store enumeration reads it first (crates/fno-agents/src/outstanding.rs). |
| `FNO_TRACKER_GITHUB_REPO` | rs | unclear: crates/fno-agents/src/tracker/github.rs:113 |
| `FNO_TRACKER_LINEAR_API_KEY` | rs | Linear API key for the linear tracker backend, read once at construction. Every backend op refuses without it, naming this var. Auth rides env only, no config key. |
| `FNO_TRACKER_LINEAR_TEAM` | rs | Linear team key (e.g. ENG) scoping the linear tracker backend's list_open and list_closed_since, the way FNO_TRACKER_GITHUB_REPO scopes github. Without it the listings warn and return empty. |
| `FNO_TURN_MARKER_TTY` | rs | Override sink for the OSC 133 turn markers (a file path in tests); unset, the session-state entry writes the markers to the controlling tty. |
| `FNO_UX_SHOTS` | rs | unclear: crates/fno/src/frame_html.rs:356 |
| `FNO_VERBOSE` | rs | Set to 1 to print non-fatal notes that are quiet by default: the registry provenance heal and the loop-runtime global mirror failure. |
| `FNO_VERIFY_GIT_BIN` | rs | unclear: crates/fno-agents/src/verify_evidence.rs:906 |
| `FNO_WHOAMI_SEAM_CHILD` | rs | Set by the front door on the child it runs for `fno whoami`, so the child takes the plain Python forward instead of the receipt seam (crates/fno/src/whoami_seam.rs). |
| `FNO_WORKER_ADD_DIRS` | rs | unclear: crates/fno-agents/src/claude_ask.rs:687 |
| `FNO_WORKER_NAME` | py+rs | The spawned worker's name: the dispatcher pins it (py), the task-grain claim metadata reads it back (crates/fno-agents/src/backlog/task.rs). |
| `FNO_WORKTREE_POLICY` | py+rs | Overrides the resolved worktree policy from env, above every config layer; the dispatcher sets it to never for a spawn into an undeclared repo. |
| `GEMINI_SANDBOX` | rs | unclear: crates/fno-agents/src/gemini_ask.rs:103 |
| `GEMINI_SESSION_ID` | rs | Gemini's provider session id, used by exact-session loop readiness. |
| `GITHUB_ACTIONS` | rs | Folds the smoke step headers into ::group:: in CI: crates/fno-agents/src/test_smoke.rs |
| `GITHUB_EVENT_BEFORE` | py | unclear: cli/src/fno/lint_cli.py:622 |
| `GIT_CONFIG_GLOBAL` | rs | the VCS tool's own ambient global-config channel; the birth display_name test guard redirects it beside HOME. |
| `GIT_CONFIG_SYSTEM` | rs | the VCS tool's own ambient system-config channel; the birth display_name test guard redirects it beside HOME. |
| `GLOBAL_EVENTS_PATH` | rs | unclear: crates/fno-agents/src/hook/stop.rs:462 |
| `GROK_HOME` | rs | grok's base directory, sessions live under its `sessions` child: crates/fno-agents/src/grok_store.rs:18 |
| `GROK_SESSION_ID` | rs | fallback session id when a grok Stop payload omits sessionId: crates/fno-agents/src/hook/stop.rs:150 |
| `HANDOFFS_DIR` | rs | unclear: crates/fno-agents/src/finalize.rs:1495 |
| `HELP_ROUTING_BLESS` | rs | Setting it to 1 rewrites the generated routing page from its renderer; the page-compare test blesses with it. |
| `HOME` | py+rs | The user's home directory. |
| `HOST` | rs | This machine's name for the join profile header: an agent session names itself through it when set; the hostname fallback follows. |
| `INVOCATION_ID` | py | unclear: cli/src/fno/agents/context.py:94 |
| `LOGNAME` | rs | The login name when `USER` is unset (a launchd daemon); the last fallback of the person (crates/fno-agents/src/identity.rs:218). |
| `MCP_CHANNEL_INBOUND_POKE` | py | unclear: cli/src/fno/agents/context.py:92 |
| `NO_COLOR` | rs | unclear: crates/fno/src/pty.rs:1976 |
| `OPENCODE_CONFIG` | rs | Points the plugin-array audit at one extra OpenCode config file; unset, the audit reads only the standard paths. |
| `OPENCODE_CONFIG_DIR` | rs | Moves OpenCode's config dir; the installer and the doctor's surface report read it. |
| `OPENCODE_DB` | rs | Points the intel fold at one opencode store; unset, every `opencode*.db` in the data dir is read. |
| `OPENROUTER_API_KEY` | rs | OpenRouter API key for `fno config accounts benchmarks refresh`; the refresh refuses without it, naming this var. |
| `OUT_DIR` | rs | unclear: crates/fno-agents/build.rs:51 |
| `PATH` | py+rs | Executable search path. |
| `PI_CODING_AGENT_DIR` | rs | pi's agent dir; fno installs the extension under it and resolves the session store from it. |
| `PI_CODING_AGENT_SESSION_DIR` | rs | pi's flat session store override; fno matches a session by the file header's cwd. |
| `POSTMORTEMS_DIR` | rs | unclear: crates/fno-agents/src/finalize.rs:2981 |
| `POSTMORTEM_CORRECTIONS_LOG` | rs | Overrides the corrections.log path; the finalize writer and the corrections-verify reader resolve it together. crates/fno-agents/src/finalize.rs:3098 |
| `POST_MERGE_NONINTERACTIVE` | rs | Scrubbed from tests: cli/src/fno/hermetic.py:167. Read in crates/fno-agents/src/post_merge/ritual.rs:1483: set to 1, the post-merge ritual runs noninteractive. |
| `PWD` | py+rs | unclear: cli/src/fno/adapters/providers/cli.py:54 |
| `PYTEST_CURRENT_TEST` | py+rs | unclear: cli/src/fno/cli.py:404 |
| `PYTHONPATH` | rs | unclear: crates/fno-agents/src/finalize.rs:1090 |
| `REDUCED_MOTION` | rs | A reduced-motion request (`1`/`true`/`yes`/`on`); the mux launch splash then prints its last frame only, once, instead of animating, and a Working row's status glyph stays a still `●` instead of spinning. |
| `RUSTC_WRAPPER` | rs | Cargo's own rustc wrapper. `fno update` sets it to the cargo admission wrapper for its compile fallback, since `cargo install` skips the checkout's `.cargo/config.toml`; a value already set is kept. |
| `SCCACHE_DIR` | rs | Where sccache keeps its compile cache; `fill_sccache_env` pins the fleet cache to `<fno build base>/sccache` when unset and sccache is installed, beside the build-dir shards the same lane sweeps. The footnote rustc wrapper sets the same default for an opted-in build. fno never exports it to a shell rc or harness env, so other Rust projects keep their own cache. |
| `SCCACHE_DISABLE` | rs | Set to `1` to force bare rustc everywhere; the rustc wrapper reads it ahead of every opt-in, and the daemon's sccache server keep-alive reads it the same way, so a disabled machine runs no server even with `build.sccache = true` or `FNO_SCCACHE=1` set. |
| `SCCACHE_IDLE_TIMEOUT` | rs | `fill_sccache_env` and the rustc wrapper default this to `0` (never stop on idle): a server that exits on idle mid-build fell compiles back to local rustc under fleet load. A shorter operator-set value survives both defaults. The daemon's `ensure_sccache_server` starts a replacement with `0` when it finds no live server on an opted-in machine. |
| `SHELL` | rs | The user's login shell. |
| `SMOKE_CHANGED_RECEIPT` | rs | Overrides the changed-mode receipt path: crates/fno-agents/src/test_changed.rs |
| `SMOKE_FAILURE_RECORD` | rs | Overrides the failure-record path: crates/fno-agents/src/test_smoke.rs |
| `SMOKE_REGISTRY_FILE` | rs | Test seam naming a JSON registry ({"steps": [[name, cwd, cmd], ...]}): crates/fno-agents/src/test_smoke.rs |
| `SSH_CONNECTION` | rs | Set by sshd to `client_ip client_port server_ip server_port`. `fno mux serve --observe` names the watcher `user from client_ip` from it. |
| `STATE_FILE` | rs | unclear: crates/fno-agents/src/kill_criteria.rs:57 |
| `TARGET_ABORT_REASON` | rs | unclear: crates/fno-agents/src/ledger_register.rs:617 |
| `TARGET_ALLOW_IN_REVIEW` | rs | Set to 1, it lets follow-up scope on a node that has a PR fork a child node. It never re-runs the parent. |
| `TARGET_CLAIM_TTL` | rs | unclear: crates/fno-agents/src/target_start.rs:1052 |
| `TARGET_INPUT` | rs | unclear: crates/fno-agents/src/target_gates.rs:473 |
| `TARGET_NO_MERGE` | rs | The merge-refusal carrier. The Rust spawn lane (`merge_posture::apply_env_from_message`) arms it when the command carries `--no-merge` and clears it otherwise, so every child it spawns inherits the verdict. |
| `TARGET_PLAN_PATH` | rs | unclear: crates/fno-agents/src/target_gates.rs:474 |
| `TARGET_SESSION_ID` | py+rs | Run id a driver pre-assigns its workers; init adopts it verbatim as session id and claim owner, and the decide door matches it against a live claim's `target-session:<id>` holder to say whether this caller holds the node. |
| `TARGET_SIZE` | rs | Size profile the init door hands the writer. The blast read also takes it from the caller's env as an operator pin that a low-blast downgrade must not strip. Read in crates/fno-agents/src/target_init.rs `run`. |
| `TARGET_SUMMARY_PATH` | rs | unclear: crates/fno-agents/src/ledger_register.rs:584 |
| `TARGET_TRANSCRIPT_ID` | rs | The hook's claude session id under its transcript spelling; the owned-identity gate reads it when CLAUDE_CODE_SESSION_ID is absent. |
| `TARGET_UNATTENDED` | rs | unclear: crates/fno-agents/src/target_orient.rs:329 |
| `TASK_DO_TTL_HOURS` | rs | unclear: crates/fno-agents/src/graph_store.rs:1046 |
| `TASK_LOCK_TTL_HOURS` | py+rs | unclear: cli/src/fno/graph/_constants.py:331 |
| `TERM` | rs | Terminal type; a Rust front terminal capability check reads it. |
| `TMPDIR` | py+rs | Temp base (events writer); the session-state entry falls back to it for the runtime pin dir when XDG_RUNTIME_DIR is unset (session_state.rs). |
| `USER` | py+rs | unclear: cli/src/fno/adapters/providers/managed.py:185 |
| `USERNAME` | py+rs | unclear: cli/src/fno/adapters/providers/managed.py:185 |
| `USERPROFILE` | rs | unclear: crates/fno-agents/src/publish_review.rs:195 |
| `UV_CACHE_DIR` | rs | Authoritative uv cache directory: the cache root a bounded codex worker is granted as a writable root; set but missing grants nothing (no fallback, uv reads exactly this variable). |
| `UV_PROJECT_ENVIRONMENT` | rs | The venv path an `uv sync` would write; the spawn-door venv guard holds a worktree whose value resolves into the canonical checkout, so a worktree install cannot rewrite the canonical cli venv's scripts. |
| `WORKTREE_STATUS_REGISTRY` | py+rs | Path of the worktree status registry file; tests pin it. Read in crates/fno-agents/src/worktree/status.rs. |
| `XDG_BIN_HOME` | rs | Where Astral's installer put uv; `fno uninstall` removes a fno-installed uv and uvx there, else from ~/.local/bin. |
| `XDG_CACHE_HOME` | rs | unclear: crates/fno/src/bootstrap.rs:1395 |
| `XDG_CONFIG_HOME` | rs | Relocates OpenCode's config dir ($XDG_CONFIG_HOME/opencode) ahead of the ~/.config/opencode default; unset reads the default. |
| `XDG_DATA_HOME` | rs | Relocates uv's tools dir where the fno-py console script is resolved; unset reads the default ~/.local/share/uv layout. |
| `XDG_RUNTIME_DIR` | rs | Runtime dir base (mcp sidecar); the session-state entry also parents its 0700 turn-pin rendezvous dir here when set (session_state.rs). |
| `XDG_STATE_HOME` | rs | Roots the sidecar log at `<state-home>/fno/sidecar.log` when set (default `~/.local/state`); read by the lazy-start in `crates/fno-agents/src/mcp/client.rs` (`state_log_path`). |
| `ZDOTDIR` | rs | unclear: crates/fno/src/pty.rs:1770 |
| `_FNO_MACHINE_MAIL_LOCK_TIMEOUT` | rs | Machine-mail lane's internal lock-timeout pop: internal dispatch shortens the recipient lock to prove the lane stays internal. Popped after read, a normal caller never sees it. |
