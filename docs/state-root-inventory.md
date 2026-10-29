# State-root inventory

Every writer that targets the top level of the state root (`config.state_dir`, default `~/.fno/`), with an owner and a lifetime.

A file nobody deletes is a file nobody owns. This page exists so a new root writer has somewhere to declare itself. It also lets the next person who finds an unexplained file look up whether anyone meant it.

Measured 2026-08-13 against one real install: 527 top-level entries, 395 of them context-nudge latches. The hook had landed six days earlier and deleted nothing. That is the failure this page is meant to prevent a second time.

## The rule

The root holds a closed set of folders, plus the rows this page already has. Nothing new lands at the top level: state goes under a folder the page already names. A new folder is not the remedy for a new root file. An existing folder is.

Two gates hold that line. At runtime, the write gate (`hooks/write-gate.sh`, policy in `crates/fno-agents/src/hook/write_gate.rs`) refuses a new top-level entry, in the state root or the Claude config dir. It also refuses a write under a top-level folder that does not exist. The refusal names the session's job tmp dir. A `sqlite3` open of a missing state-root entry refuses too: the shell creates the file it opens. In CI, `scripts/ci/check-state-root-rows.sh` fails any PR that adds a root pattern to this page against the checked-in baseline. An added folder row refuses like any other. The root rows below are SHRINK-ONLY. A writer that moves into a subfolder or dies takes its row with it. A row never enters.

"Belongs at the root" used to mean one durable file per install, named for what it is: `graph.db`, `ledger.json`, `config.toml`. That set closed on 2026-09-27. A family of files keyed by session, band, or timestamp never belonged there, however small each one is. The cost is legibility, not bytes: all 395 context-nudge latches together were 14,625 bytes and made the directory unreadable.

Every location resolves through `fno.paths`. Adding a hardcoded `$HOME/.fno/<newdir>` repeats the bug one directory down, so route new paths through the resolver: `from fno import paths` in Python, `source "$(fno config paths shell-stub)"` in bash. `scripts/ci/check-no-hardcoded-paths.sh` gates this.

## Durable singletons

One file per install. These belong at the root.

| Entry | Writer | Lifetime |
|---|---|---|
| `ledger.json` | `paths.ledger_json()` | permanent |
| `config.toml`, `.lock` | `paths.config_toml()` | permanent |
| `settings.yaml`, `.lock` | `fno/config/__init__.py` loader | permanent |
| `decisions.jsonl` | `crates/fno-agents/src/backlog/decisions_cli.rs` | permanent |
| `questions.jsonl` | `paths.questions_jsonl()`, written by `fno inbox outstanding` | permanent; a question does not expire |
| `decisions.jsonl.corrupt` | `decide/__init__.py::_compact_index` | permanent; the only copy of a row whose source journal is gone |
| `decisions.jsonl.compact` | `decide/__init__.py::_compact_index` | transient; replaced onto `decisions.jsonl` in the same call |
| `.canary` | `scripts/ci/check-state-canary.sh` `plant` | permanent, and written ONLY into a root with no live graph, which in practice means a CI runner. A live operator root is watched read-only and never receives it. One byte of content; its job is to give the walk a file it can prove it saw. |
| `opencode-install-<hash>.json` | `crates/fno-agents/src/opencode_install.rs` (the `plugin-install opencode` arm) | permanent; one per `OPENCODE_CONFIG_DIR` (the hash is of the config dir's canonical path), rewritten per install with one entry per written path and its content hash; removed last by `plugin-install opencode --uninstall`, so an interrupted uninstall is resumable |
| `recovery/provider-outages.json`, `.lock`, `.provider-outages-*.tmp` | `agents/provider_outage.py` | permanent breaker/evidence journal; lock and atomic temp sidecars live only for one write and stale temps are safe to remove when no writer holds the lock; stores fingerprints, bounded raw refusal text, and explicit route IDs, never credentials or full transcripts |
| `recovery/provider-canaries/*.json` | `agents/watchdog.py` | bounded health proofs for audit; exact marker, provider/account IDs, pane ID, and timestamp only, never pane dumps or credentials |
| `recovery/canary-work/` | `agents/watchdog.py` | permanent empty neutral cwd reused by canaries; owns no node claim or project data |
| `claims/dispatch%3A*.lock`, `.recovery.d/` | `claims/core.py` for provider handoff | transaction lease for one attempt; released at terminal return, recovery mutex removed by the claim primitive; contains holder/process metadata only |
| `state/claim-aux/<key>.lock.queue.d/`, `state/claim-aux/<key>.lock.priority.d/`, `state/claim-aux/<key>.lock.full.d/` | `crates/fno-agents/src/claim_queue.rs` owns the ticket format; waiters live in `crates/fno-agents/src/test_run.rs` and `scripts/ci/preflight.sh` | one ticket dir per live waiter on one admission door, named `NNNNNN/holder`; removed by its own waiter on every exit path and reaped by the next scan when its recorded pid is gone. The three dirs are the lanes: `priority` (a live `test:priority` claim names the checkout), `queue` (arrival order), `full` (whole-suite runs, suite door only) |
| `state/claim-aux/build-waiters/` | `crates/fno-agents/src/test_run.rs` `CargoWait` | one marker per waiting checkout, removed when the wait ends and by the reader when its pid is gone; read by the stop hook to allow a stop during a held build |
| `agents/squads.json` | no writer in this build: a historical store location beside the live agent files (`registry.json` there IS authoritative, which is what makes the dead file read as real). `fno mux doctor` names it and its live replacement. | dead: nothing reads it, so it can only mislead; delete on sight (`fno mux doctor` prints the `rm`) |
| `agents/reap-receipts/<harness>-<session id>.json` | `crates/fno-agents/src/receipt.rs` (`write_reap_receipt`; the four writer doors are `gc_sweep.rs`, `roster_reap.rs`, the `update_registry` choke point in `receipt.rs`, and the Python choke point in `cli/src/fno/agents/registry.py`; `FNO_AGENTS_HOME` overrides) | retained `config.agents.reap_receipts.retain_days` days (default 7); past the window the sweep strips the EXPENDABLE detail (ledger copy, per-effect rows, log path) and keeps the identity core (who, native locator, resume argv) forever, because deleting it would strand the only recovery record for a resumable session. Receipts carry a schema version, the writer build, and per-effect outcomes; `fno agents registry reap --verify --since 24h --json` audits them against the current build. A receipt whose `reaped_at` cannot be read is kept and named in the sweep summary, never deleted on a failed read |
| `agents/keep/<session id>.json` | `crates/fno-agents/src/keep_mark.rs` (`AgentsHome::keep_dir`; `FNO_AGENTS_HOME` overrides) | permanent until `fno agents registry reap --unkeep`; never swept; the mark is keyed by harness session id, so it outlives its row and follows a resumed fork; `fno agents registry reap --kept` lists every mark and flags one with no registry row |
| `agents/fleet-stop.json` | `crates/fno-agents/src/fleet_incident.rs` via `AgentsHome::fleet_stop_json()` (written by `fno agents audit incident stop|clear`; `FNO_AGENTS_HOME` overrides) | permanent machine-wide circuit breaker: `{version, state: stopped|clear, generation, changed_at, changed_by, reason, holds, target, expires_at, origin, mail, mail_session_id}`, temp-plus-rename atomic. `incident stop|clear` and `loops pause-all|resume-all` use the same writer. `clear` is a positive record, never a deletion; every stop and clear increments `generation`. Read by the spawn gates, the active-backlog daemon, and the test-run owner BEFORE their bypass branches; a present-but-unreadable file refuses admission (`fleet-stop-unavailable`), and a writer refuses to replace an unreadable record - remove it by hand to start a fresh generation. Mail is not gated by the fleet breaker; pause-all records whether it armed a clock and the full owning session id, while the registry delivery_policy flag enforces a mail hold. A failed owner release stays `armed` for a later `resume-all` retry; success marks it `lifted` |
| `agents/launches/pending/*.json`, `agents/launches/claimed/<session id>.json`, `agents/launches/backfill.jsonl` | `crates/fno-agents/src/launch_record.rs` (the PreToolUse Bash hook writes, the SessionStart report claims) and `crates/fno-agents/src/backlog/provenance_cli.rs` (the backfill journal); `FNO_AGENTS_HOME` overrides | a pending record lives 10 minutes and the next write prunes it; a claimed record is the durable launcher of one session, read by the claude adopt path; the backfill journal is append-only, one line per edge written |
| `agents/machine-process-snapshot.txt`, `.active` | `crates/fno-agents/src/machine_sample.rs::maybe_capture_process_snapshot` via the daemon's `machine_watch` arm (`FNO_AGENTS_HOME` overrides) | newest successful full process snapshot, capped at 1 MiB and mode 0600, retained across reboots and overwritten on the next pressure episode; `.active` marks the current episode and is removed after a measured sample falls below both process thresholds |
| `agents/fleet-stop.d/<target>.json` | `crates/fno-agents/src/fleet_incident.rs` (same writer as `agents/fleet-stop.json`; `FNO_AGENTS_HOME` overrides) | one 0600 session or territory record per target under a 0700 directory; expiry releases enforcement but records remain visible in status until clear or re-arm; session identity uses the full session id |
| `agents/releases/<release>.json`, `.json.tmp` | `crates/fno-agents/src/release_record.rs` (the `authorized-merge` `release-pin` and `release-join` ops; `FNO_AGENTS_HOME` overrides) | permanent: one record per release `{schema, release, repo, base_tag, pins: [{sha, at, by, reason}], included, joiners: [{pr, at, by}]}`, temp-plus-rename atomic. A pin appends, never overwrites. Read by `release-show` and `scripts/release/draft-release-notes.py --record`. Nothing deletes it |
| `agents/test-pause.json`, `.lock` | `crates/fno-agents/src/test_hold.rs::reconcile` via `AgentsHome::test_pause_json()` (run by `fno agents audit incident stop|clear` and the daemon's `machine_watch` tick; `FNO_AGENTS_HOME` overrides) | lives only while the machine-wide breaker holds `tests`: `{generation, paused: [[pid, birth]], announced}`. New holds end fleet test runs and record an empty `paused` list; the list keeps the `(pid, birth)` pairs an OLDER build paused, which the lift still resumes with SIGCONT. The lift announces the all-clear and deletes the file; the lock sidecar persists |
| `agents/first-checks/<birth-hash>.json` | `crates/fno-agents/src/first_check.rs`, armed by the native birth-event write and consumed by the daemon | One deadline and at most two accepted notices per birth. Completion survives daemon restarts. Finished records expire after one day. A replaced worker row cancels the old check. |
| `agents/otel/` (`port`, transient `port.tmp`, `otel.db`) | `crates/fno-agents/src/otel_ingest.rs` through the daemon receiver (`FNO_AGENTS_HOME` overrides) | The endpoint record survives daemon shutdown. Atomic publication removes the temporary file. The database retains deduplicated, sanitized log events and typed request costs under the canonical OTEL schema. No automatic retention cutoff. The telemetry off switch stops new listener/birth injection. |
| `~/.fno/loops-paused.json` | read by `crates/fno-agents/src/loops_pause.rs`; no active writer | legacy pause-all sentinel, read-only during migration and deleted by `loops resume-all`; `pause-all` refuses to stack a bounded breaker pause over an active or corrupt sentinel |
| `agents/compacting/<session>.json` | `crates/fno-agents/src/compaction.rs` via `compaction::mark` (the `PreCompact` hook's best-effort call to `fno-agents compaction mark --session`; `FNO_AGENTS_HOME` overrides) | session-keyed, overwritten per compaction, never cleaned: a stamp is tiny and self-expiring (the reader returns `stamp-past-ceiling` after 45 minutes), so no sweep is owed. Read by `compaction::compaction_state` and the provider-cap actor, which holds any member whose state is `Compacting` or a live-stamp `Unknown` |
| `agents/friction/<harness session id>.json`, `.offer` | `crates/fno-agents/src/friction.rs::record` (the Stop path, the spawn, join and done gate refusals, and the loop-check PR read; `FNO_AGENTS_HOME` overrides) | session-keyed and tiny: the `.json` ledger holds at most three offers and the fingerprints whose match found nothing. Written only when `friction.match.enabled` is on. `hooks/friction-offer-inject.sh` prints the `.offer` once and deletes it; `fno backlog encounter --friction` reads the ledger. Never swept: one small file per session that hit friction |
| `agents/first-turn-bind/<harness session id>` | `hooks/node-bind-prompt-submit.sh` (the `UserPromptSubmit` shim; `FNO_STATE_DIR` overrides the state root) | session-keyed empty marker, the shim's exactly-once gate, written BEFORE the `node-bind` door so one session pays at most one bind attempt. Never swept: one empty file per claude session whose first prompt reached the hook; any existing marker reads as fired. The door itself is stateless |rprints whose match found nothing. Written only when `friction.match.enabled` is on. `hooks/friction-offer-inject.sh` prints the `.offer` once and deletes it; `fno backlog encounter --friction` reads the ledger. Never swept: one small file per session that hit friction |
| `agents/provider-cap/<lane>-<epoch>.jsonl` | `crates/fno-agents/src/provider_cap.rs` (`FNO_AGENTS_HOME` overrides) | per-move journal: one line per migration step (`decided`, `destination`, `spawn-confirmed`, `stopped`, `unknown`, the wave-4 return steps `canary-resumed`, `canary-verdict`, `trickle-resumed`, `announced`, `return`), written append-only at actor time. A step the code could not prove records `unknown`, never `moved` (the unmeasured-state rule). The per-epoch canary state `return-<lane>.json` (keyed by the reset it belongs to) and the `decide` answers live beside it under the same folder |
| `mux/` (`<session>.sock`, `.ver`, `.pid`, `.detach`, `.log`, `mux-view.json`, `mux-view.json.lock`, `squads.json`, `squads.json.lock`, `session-names.json`, `session-names.json.lock`) | `crates/fno/src/proto.rs::mux_dir()` owns the sockets; the three sidecars move here at the server's own start (`crates/fno/src/state_layout.rs::migrate_mux_sidecars_at`, wired into `server.rs::serve` before the first store opens); `crates/fno/src/view_store.rs` and `squad_store.rs` resolve through `proto::mux_sidecar_path`, the agents crate reads `squads.json` and `session-names.json` through `place`, and `agents/discover.py` points at `mux/session-names.json`. `FNO_AGENTS_HOME` overrides both stores | server-managed; `kill-server` owns socket removal |
| `mux/command-receipts/<request-id>.json` | `crates/fno/src/mux_cli/harness_command.rs` via the mux directory | one idempotent native-action receipt per request; retained for seven days, then removed by the next command invocation |
| `mux/panes/<session>-<pane>.sock` | `crates/fno/src/pty.rs::keeper_dir()`, written by each `fno-agents-worker --pane` keeper | unlinked by the keeper when its child exits; a server-start sweep unlinks leftovers whose keeper is gone, and `fno mux pane keeper list` names them |
| `mux/themes/<name>.toml` | `crates/fno/src/theme_import.rs` (the Settings importer) or the user by hand | permanent, user-owned theme; delete the file to remove it |
| `mux/threads/<agent>.sock` | `cli/src/fno/agents/keeper_thread.py::_lane_b_keeper_socket()`, written by each `fno-agents-worker --keeper` it launches (the pane-less lane-B thread keeper; a session-keyed subfolder, never a top-level write) | unlinked by the keeper when its child exits; no server-start sweep yet - the restart journey that owns re-adoption is a later group of the same epic, so until then a crashed keeper's leftover is named by the registry row's `messaging_socket_path`. The state root follows the daemon's derivation, not just `state_dir()`: the Rust registry-side keeper sweep derives the threads dir from the agents root's parent (`FNO_AGENTS_HOME`'s parent when set, else `state_dir()`), so the spawn must write the socket where that sweep reads it or a restart rebind silently finds nothing. The override arm keeps `FNO_AGENTS_HOME`'s literal spelling - no `resolve()`: the sweep matches the row's socket path byte-for-byte against a dir built from the raw `--home` string, and resolving repoints it through symlinked components (macOS `/var` -> `/private/var`), leaving the socket orphaned at every restart |
| `my-priorities.md`, `.lock` | the operator, by hand or with their own `~/.fno/board.py` scratch script (not a repo file); read via `paths.operator_lane()` | permanent |
| `sidecar/wizard-report.json` | `crates/fno/src/wizard_term.rs` (the wizard host, written at run end) | per run; the next run overwrites it; safe to delete when no run is live; the setup skill reads it to report |
| `sidecar/wizard-answers.toml` | `crates/fno/src/wizard_term.rs` (best-effort save of the screen answers) | per run, like the report; safe to delete when no run is live |
| `sidecar/wizard-lock` | `crates/fno/src/wizard_term.rs` (`LockGuard`) | exists only while a wizard run is live; a readable pid refuses a second run; a stale file is safe to delete |
| `sidecar/setup-backup/` | `crates/fno/src/setup_profile.rs` (`backup_dir`, `mark_in_progress`): `config.toml.<utc stamp>` copies of the global config taken before each applying write, plus the `in-progress` flag | consumed by the automatic restore on a refused write and the next-start heal; otherwise kept until `fno uninstall` or a hand delete |
| `sidecar/setup-profile.toml` | `crates/fno/src/setup_profile.rs::save_profile` (the join lane, best-effort) | kept so the export and check lanes can diff and re-export; safe to delete, the next join rewrites it |

`graph.json` is retired. A former file moves to `backups/graph.json.retired.*`; an unimported nonempty file makes the store refuse to open so its rows remain recoverable.

`paths.locks_dir()` answers from the Rust `dir` door (`machine_locks_dir()`), and a `config.state_dir` override deliberately does not move it. The config-free plan-stamp path and the config-loading append path have to agree on one directory, and moving it desyncs them. Its docstring says so. Do not "fix" it to load settings. The root chain stays `FNO_STATE_DIR` or `$HOME/.fno`, and the layout table's `locks` row owns the folder itself.

## Frozen root rows (2026-09-27 backfill)

The 2026-09-27 sweep found 71 undocumented top-level entries on one real root. The role verified and deleted the backups whose data a `.db` provably holds. The ephemeral writers below learned to clean up after themselves. Every remaining real entry got its row here. The section is FROZEN: shrink-only, like every root row. When one of these writers moves into a subfolder, delete its row in the same PR.

| Entry | Writer | Lifetime |
|---|---|---|
| `config.toml.bak*` (5 files, 2026-09-08 through 2026-09-22) | the operator, by hand, during config and model-routing edits | permanent: no `.db` holds their data, so deletion waits on the operator's yes |
| `ruleset-21074865-before-smoke-hold.json` | the operator, by hand, before a smoke-hold change | permanent, same rule as the `config.toml.bak*` row |
| `ntfy/` | an operator-run ntfy server (its `cache.db`) plus `ntfy.out.log` / `ntfy.err.log` | server-managed; not written by this repo |
| `intel/` | the intel fold behind `fno intel` | regenerated per run; safe to delete |
| `sidecar/` | `cli/src/fno/paths.py::sidecar_dir()`; `crates/fno/src/setup_run.rs` writes `sidecar/setup-done` (the run-once setup's global marker) | per-item sidecar files owned by their writers; the setup marker lives until `fno uninstall` or a hand delete |
| `blueprinters/` | the blueprinter sessions (one hash dir per session) | session-keyed; a dead session's dir is inert |
| `term-watch/` | the lead-watch tool deploy (its `bin/`, `src/`, and `fno-mux` copy) | operator-managed |
| `jobs/` | event-snapshot dirs from isolation and repro runs (`events-global-<date>/`) | repro residue; safe to delete once a run ends |
| `backup/` | the operator, by hand: pre-store graph exports (`fno.json`, `etl.json`, node-list jsons) | permanent until the operator rules on them |
| `stable-bin/` | the operator, by hand: a stable-channel binary copy (`bin/`, `src/`) | operator-managed |
| `internal/` | the operator, by hand: vault-adjacent exports (`etl.json`, `fno.json`) | operator-managed |
| `handoff-evidence/` | the rank goldens capture (`crates/fno-agents/src/backlog/rank_cli.rs` names the goldens) | permanent evidence for the rank CLI |
| `repro-isolated/` | `crates/fno-agents/src/observer/isolation.rs`: the observer's isolated repro root | repro residue; safe to delete once a run ends |
| `.codegraph/` | codegraph (foreign tool): its index beside the state root | tool-managed; never swept |
| `.gitignore` | the operator, by hand | permanent |

## Named root exceptions

The migration moves every movable file into a named subfolder (`docs/state-root-layout.tsv` is the table, and `fno-agents state migrate` is the mover). These stay at the top level, each because a move is blocked on a port, an operator ruling, or a platform:

| Entries | Why they cannot move yet |
|---|---|
| `config.toml`, `config.toml.lock`, `config.toml.bak*`, `settings.yaml`, `settings.yaml.lock` | The config contract every harness, doc and OSS install names at `~/.fno/config.toml`. The `.bak*` files await the operator's ruling on their deletion. |
| `my-priorities.md`, `my-priorities.md.lock`, `ruleset-21074865-before-smoke-hold.json` | Operator-owned, edited by hand at that path. |
| `.env`, `.gitignore` | Operator-owned, edited by hand at that path. |
| `.DS_Store`, `.metadata_never_index` | macOS Finder and Spotlight; Spotlight reads the opt-out marker only at the folder root. |
| `.path-migration-done` | The Python startup migration's sentinel (`cli/src/fno/cli.py`). Parking it re-fires that migration, which rewrites root files. It moves when that migration is deleted or ported. |
| `events.jsonl` | The retired journal family's anchor and stray-hunt root (`global_events_json`): writers committed to the routed store, so it is read-only, and the doctor's stall fold reads it for the retained rotations beside it. |
| `posture.json` | The removed `fno posture` command's advisory stamp; the doctor reads it until the file ages out of every machine it landed on. |
| `ledger.json`, `ledger.md`, `decisions.jsonl`, `decisions.jsonl.compact`, `decisions.jsonl.corrupt`, `questions.jsonl` | Python locators. `paths.py` resolves the journals from the state root, and the Python event sites that once hardcoded `state_dir() / "events.jsonl"` now pass that address to the store, which derives the `.db` from it. `ledger.md` has no writer and parks with `ledger.json` when that slice lands. Over the 30-line budget; they move when those callers port. Their `.db` stores DO move (Rust derives them). |

## Owned subfolders and remaining root state

Every subfolder and file below was found in the real root unnamed at the 2026-09-08 backfill and given a writer and a lifetime. The top-level folder set is closed: the CI row gate refuses a row naming a folder the baseline does not hold. What reads the real root is the drift reading in the lead check-in and the reclaim receipt, not a test. `cli/tests/test_state_root_inventory.py` builds its root from this page and runs one Python writer. It cannot see a Rust or shell writer.

| Entry | Writer | Lifetime |
|---|---|---|
| `state/attention/items.json` | `crates/fno-agents/src/attention_arm.rs` (the `attention` arm) | the attention projection cache plus `questions_dir`, rewritten every beat; the lead check-in reads it and refuses when it is missing or over 600 s old, safe to delete, next beat rebuilds it |
| `state/question_bus/status.json` | `crates/fno-agents/src/question_bus.rs` (the question bus beat, inside the mail arm) | the arm's last beat (`at`, `published`, `peer_hosts`, `last_error`), rewritten every beat; safe to delete, next beat rebuilds it |
| `state/attention/questions.json` | `crates/fno-agents/src/attention_arm.rs` | page settle state (body hashes and since-stamps); deleting it restarts every settle window and cannot double-deliver, because a page's existence proves delivery |
| `state/attest/` | `hooks/attest-model.sh`, `hooks/review-hold.sh` | one attestation sidecar per reviewed session |
| `agents/quiet-recovery/` | `crates/fno-agents/src/codex_watch/quiet.rs` | one episode per native session; resets on new transcript activity; retained as recovery evidence after retirement; deleting a file can repeat its nudge |
| `backups/` | `crates/fno-agents/src/graph_store.rs` backup rotation (pruned to `GRAPH_BACKUP_KEEP`), and `cli/src/fno/setup/migrate_paths.py` (`settings.yaml.bak.<ts>`) | graph rotation prunes itself; migration backups are one-shot per install. A backup at most a tenth the size of its predecessor moves that predecessor to `backups/pre-shrink.<name>`, and pins are never pruned. |
| `briefs/` | `paths.briefs_dir()` | permanent sidecar discovery briefs |
| `fleet/` | `fleet_beacon.rs` (`fleet/takes-work-off/` per-machine empty markers, agents-home spelling); the mission `00-INDEX.md` indexes have no writer in `cli/src` or the crates today; resolved by `paths.fleet_dir()` (config `paths.fleet_dir` overrides, default `<state_dir>/fleet/` in [path-config](path-config.md)); readers: the `fno whoami` fleet panel and `fleet_beacon.rs` | permanent legacy mission indexes plus live per-machine markers; a deleted index never regenerates |
| `bus/` | `paths.bus_dir()`, written through `crates/fno-agents/src/announce.rs`'s appender and read by `chats bus-read` (`messages.jsonl`, `cursors/`) | append-only mail log; each consumer's cursor is overwritten |
| `chats/chat-<hex>/messages.jsonl` | `crates/fno-agents/src/mail_threads.rs` (thread append), read by `crates/fno/src/messages_model.rs` | durable; the JSONL is the source and `db/chats.db` is an index `crates/fno-agents/src/chats.rs` rebuilds from it |
| `cache/` | `cli/src/fno/pr/_cache.py` (`cache/pr-status`), `crates/fno-agents/src/lead_ops.rs` (`cache/lead-drain.json`), `crates/fno/src/model_catalog.rs` (`cache/models-dev.json`), `crates/fno-agents/src/pr_admission.rs` (`cache/pr-origin`), `crates/fno-agents/src/pr_steward.rs` (`cache/pr-steward`), `crates/fno-agents/src/accounts/benchmarks.rs` (`cache/benchmarks.json`, the OpenRouter snapshot; `route_gather::load_snapshot` still reads the legacy root spelling), `hooks/context-run.sh` (`cache/context`: one file per project dir and group, plus its `.ts` epoch sibling and the detached pass's `.log`; `FNO_CONTEXT_CACHE_DIR` moves the folder), the page writers through `place` (`cache/pages`: `backlog/projections.rs` graph.md and graph.html, `agents_menu.rs` + `rundown.rs` rundown.html and lead.html via the `term.html` row; `web.rs` reads the same paths), the transcript fold (`cache/fleet`: `fleet_load.rs` + `transcript_activity.rs` activity.json plus its `.lock`) | regenerated PR-status cache; lead-drain counts keyed on the graph store version, rewritten per fresh drain read; the models.dev catalog cache, refreshed on a composer open when older than 24 h; the per-PR origin facts (head repo, base repo, author association, login) the outside-PR probe caches, never expire, safe to delete; the PR steward's per-PR stage files, one action remembered per pass, rebuilt from PR-comment markers on demand, safe to delete; the context cache replays a file written inside `FNO_CONTEXT_CACHE_TTL_SECONDS` (default 86400), safe to delete; the pages are regenerated per render; the fleet activity cache is the cursor and hour cache, safe to delete, rebuilt from the window on the next run |
| `db/` | the graph and archive stores and their kin: `crates/fno-agents/src/backlog/` (`graph.db`, schema in `mod.rs`, one owning module per aggregate; reached from the `paths.graph_json()` anchor through `docs/state-root-layout.tsv`'s `place` resolver, the legacy root spelling still readable), the archive store (`graph-archive.db`, populated by the operator's import; `read_archive` in `graph_store.rs` reads the projection), `graph_store.rs::BoundedLock` (`graph.json.lock`, the publish cycle's bounded lock; the name is frozen, see [path-config](path-config.md)), `graph_keeper.rs::store_socket_for` (each store's `*.store.sock` IPC socket and its bounded `.lock`, server-managed, unlinked by the keeper on exit and the daemon's `store_socket_sweep`), `backlog/note_history.rs::history_path` (`graph.db.history/notes.jsonl`: PERMANENT node-prose history, every replaced or cleared `current_state` pre-image and every evacuated note, append-only, hash-verified, never rotates; the only copy of evacuated prose, safe to copy with the graph, fatal to delete), `fno doctor graph export --now` (`graph.json`, the on-demand JSON snapshot), `crates/fno-agents/src/backlog/cutover.rs` (`relatedness.json`, regenerated), `crates/fno-agents/src/backlog/create_cli.rs` (`id-tombstones.json`: every id the native mint issued, permanent, read with the legacy root spelling as a fallback), `cli/src/fno/plan/cli.py` (`.plan-sync-watermark-v2`, the sweep's shared gate), `fno-event-store` (`events.db`, `decisions.db`, `questions.db`: the AUTHORITATIVE event stores, schema v2 seq/event_id/retention_class/identity; every writer commits here, every reader queries here; resolved through `place`, and the straggler lane imports a reappeared legacy store by `event_id` with INSERT OR IGNORE, then parks it, and parks a zero-length legacy store without opening it), `crates/fno-agents/src/approvals.rs` and `effect_gate.rs` (`approvals.db`, permanent SQLite store for approvals and effect attempts) | durable row store; WAL sidecars are SQLite-managed |
| `heal/pr-heal.pid` | `crates/fno-agents/src/heal_pid.rs::pid_file` (the pr-heal drive loop, beside the global events journal) | server-managed pid file; unlinked by the loop on a clean exit and by the pid scan when the pid is dead |
| `migrations/role-vocabulary-v2.done`, `migrations/role-registry-table-v1.done` | `crates/fno-agents/src/role_migration.rs` and `crates/fno/src/role_migration.rs` | install lifetime; one marker per ported migration, written once; deleting one only re-runs that migration's one-time work |
| `handoffs/` | `paths.handoffs_dir()` | handoff payloads; `scripts/handoffs-migrate-to-vault.sh` moves aged ones to the vault |
| `install/` | `update.py` (`installed-rev`, `source-path`, `source-pin.json`), `hooks/session-start.sh` (`plugin-root`, `.worktree-hook-root`), every reader resolving through the layout table (`crates/fno-agents/src/state_layout.rs`, `crates/fno/src/state_layout.rs`) | permanent install markers; the state-root migration moved them off the root (law d-8ddaba56), and `fno-agents state migrate` keeps the legacy names readable until it runs |
| `backups/state-root-migration/<stamp>/` | `crates/fno-agents/src/state_layout.rs::migrate` | parked legacy copies from the state-root migration, one stamp folder per apply run; the layout table's park rows and every conflict-parked legacy file land here. Deletion requires a recovery receipt with zero missing event identities and the operator's yes: the reclaim sweep's `backups_retention` lane reads the pile into every receipt, and reaps a stamp past `reclaim.backups_retention_days` only when an audit verdict proving `zero_missing` (`FNO_RECLAIM_BACKUPS_AUDIT`) predates it, newest 5 always kept |
| `backups/state-recovery/<digest>/` | `crates/fno/src/state_recovery.rs::apply` | verified SQLite snapshots, consumer sidecar snapshots and the immutable recovery batch manifest. Retain until the operator approves a separate rollback or removal; open attention items still resolve `recovery_batch` here, so the reclaim sweep reports the pile but never deletes it (the legacy `state-root-recovery/` spelling is in the same report-only set) |
| `.interrupted-writes/` | `crates/fno-agents/src/daemon.rs` (quarantine) | writes caught mid-flight; released after the write settles |
| `logs/` | `cli/src/fno/agents/mux_spawn.py` (spawn logs), `pr_watch/_install.py` and `crates/fno-agents/src/backlog/groom.rs` (the launchd plist `StandardOutPath`/`StandardErrorPath`), `hooks/git-protection.py` (`logs/merge-gate-overrides.log`), the corrections hook family (`logs/corrections.log`, resolved through `scripts/lib/corrections-lock.sh` and `crates/fno-agents/src/finalize.rs::corrections_log_path`), `health_monitor.py` (`logs/health-alerts.log` via `paths.logs_file()`, renamed from the root spelling on first resolve) | unrotated spawn logs plus the moved writers |
| `logs/cargo-fallback-writers.log` | `scripts/lib/cargo-rustc-wrapper.sh` | one line per cargo that builds without the build-dir env; self-trims to its last 500 lines at 1,000 |
| `mail-escalations/` | `cli/src/fno/mail/cli.py` (debounce markers via `O_CREAT|O_EXCL`) | one empty marker per sender/recipient pair inside the debounce window; safe to delete, the next escalation re-creates it |
| `state/notice-route/sent.json`, `state/notice-route/fold.json`, `state/notice-route/q-<fingerprint>.md` | `crates/fno-agents/src/notice_route.rs` | runtime; the sent store prunes its old entries on every write, and nothing in the router deletes the `q-` pages - the outstanding-ask intake owns a page once it is filed |
| `notes/` | `crates/fno-agents/src/research/engine.rs` (`notes/research`) | permanent research notes |
| `nudge-cursors/` | `cli/src/fno/agents/nudge.py` | one cursor per nudge target, overwritten |
| `state/announce-cursors/` | `fno-agents announce` (`crates/fno-agents/src/announce.rs`) | one seen-id set per session; pruned to ids still on retained bus segments |
| `law-edit-seen/` | `fno inbox law` edit read (`crates/fno-agents/src/law_match.rs` `edit_answer`, via the shared cursor in `announce.rs`) | one seen-id set per session; tiny, never cleaned |
| `observer-reports/` | the observer fold, via the `paths` accessor | one report per observation run |
| `operator-capture/` | `cli/src/fno/inbox/operator_turns.py` writes `<session-id>.jsonl`, the ack ledger and receipt; `fno-agents compaction operator-turns` (`crates/fno-agents/src/operator_turns.rs`) writes `<session-id>.scan.json` | per session, the ack ledger is permanent. The scan cursor cache is safe to delete, the next read rescans from byte 0 |
| `postmortems/` | the retro routine and stuck-terminal postmortem writer, via the `paths` accessor | permanent |
| `providers/` | `cli/src/fno/adapters/providers/managed.py`, `staging.py` | permanent managed provider configs |
| `cargo-build/<h2>/<hash>/` | cargo itself, via the tracked `build.build-dir` and the `CARGO_BUILD_BUILD_DIR` value `fno_build_base` in `crates/fno-agents/src/cargo_build_dirs.rs` resolves | per-workspace intermediates, one hash dir per workspace root; final binaries stay in the checkout's `crates/*/target`; reclaimed by `worktree cleanup --cargo-targets` (live workspaces' hash dirs are protected via `cargo metadata`) |
| `plugin-stage/fno/` | `fno config plugin install` via the `fno-agents plugin-install` command (`crates/fno-agents/src/plugin_install.rs::build_stage`); restaged by `fno doctor update` whenever the directory exists | a filtered copy of the checkout (git-tracked + untracked-but-not-ignored files only, no target/, worktrees or venvs); rebuilt wholesale on every install, so nothing in it outlives the next one; the swap is atomic by rename and hook scripts the old stage's config references are carried forward; `fno-agents plugin-install --check` is the drift verdict and `fno doctor` exits 1 on a stale stage |
| `state/reclaim/last-run.json` | `fno doctor reclaim` via `crates/fno-agents/src/reclaim.rs::write_receipt` (the Python doctor command shells to `fno-agents reclaim`) | overwritten per `--apply` run; records bytes reclaimed per lane; the daemon's daily sweep gates on its mtime |
| `state/push-stamps/` | `crates/fno-agents/src/pr_push.rs`, `hooks/git_protection_impl.py` | one stamp per protected push |
| `relay-claude/` | Claude Code itself, via a `CLAUDE_CONFIG_DIR` account alias | operator-managed harness home. Never fno state, never swept. |
| `state/retro-pending/` | written by `crates/fno-agents/src/backlog/reconcile_cli.rs`, read by `crates/fno-agents/src/retro/cli.rs` through the `place` ladder | per-PR retro evidence awaiting harvest |
| `state/review-invocations/` | `crates/fno/src/review_invocation_core.rs` (vendored byte-identical into `crates/fno-agents/src/review_invocation_owner.rs`), `hooks/code-review-attest.sh` | one invocation record per review round |
| `saved-sessions/` | `scripts/save-session.py` | one transcript per saved session, written on demand |
| `sessions/<project-slug>/` | `crates/fno-agents/src/footnote_harness/transcript.rs` (`sessions_root()`); the store predates the module and was first written by the fnh harness | one record + sidecar per `-H footnote` session, every file id-named: `<fno_id>.jsonl` (the record, beside the dir), `<fno_id>/` holding `<fno_id>.lock`, `<fno_id>.index.db` (journal `<fno_id>.index.jsonl`), `<tool_call_uid>.out` spill, `<fno_id>.diag.log`. The slug is the canonical checkout's space slug, `_none` outside a repo. Permanent; never holds credentials |
| `spaces/` | the project-space layout (`cli/src/fno/paths.py`, `crates/fno-agents/src/state_path.rs`) | permanent; detailed in the project-space section below |
| `worktree-salvage/` | `hooks/worktree-salvage-ref.sh`, `scripts/setup/setup-worktree.sh` | salvage-mirror state per worktree |
| `state/` | `hooks/git-protection.py` (`state/git-protection.json`), `plugins/buddy/hooks/register.ts` (`state/buddy/`, or `~/.local/state/buddy/` without fno: the status line wrapper copy, the saved `statusLine` in `inner.json`, and per-session `frames/<id>.json` plus `.seen`, rewritten while a session draws and swept by the wrapper a day after their last write), `crates/fno-agents/src/operator_notice.rs` (`state/notify-signals.json` via `place`), `hooks/worktree-peers-session-start.sh` (`state/.worktree-stranded-cache.json` + refresh stamp in the ambient branch), `crates/fno-agents/src/machine_watch.rs` (`state/machine-brake.json` via `place`; spawn admission reads the same table row in `crates/fno/src/process_admission.rs`), and the 2026-10-08 batch: `pr_watch/_state.py` (`state/pr-watcher-state.json` + its `.lock` spellings), `pr_watch/_dispatch.py` (`state/pr-watcher-state-delivery.json` + `.lock`), `pr_watch/_install.py` (`state/pr-watch-bounce.json`, via `bounce_sidecar_path()`), `crates/fno-agents/src/fleet_state.rs` (`state/fleet-sweep-state.json` + `.lock`), `crates/fno-agents/src/accounts/state.rs` (the one writer; `adapters/providers/runtime_state.py` reads it and writes through it) resolved by `provider_cap.rs::runtime_state_path` (`state/provider-runtime-state.json` + `.update.lock`), `crates/fno-agents/src/backlog/triage_health.rs` (`state/health-throttle.json`), `recovery.py` (`state/recovery-nudges.json`), `agents/watchdog.py` and its Rust reader `pr_watch/render.rs` (`state/watchdog-sweep.json`), `agents/dispatch.py` (`state/a2a-confirmed`), `crates/fno-agents/src/active_backlog.rs::nudge_sentinel_path` (the keeper's publish touches it) (`state/active-backlog-nudge`), `crates/fno-agents/src/think_daily.rs` (`state/think-spawn-daily.json`). Python resolves all of them through `paths.state_runtime_file()` (rename-on-first-resolve, never clobbers); Rust readers resolve through the `docs/state-root-layout.tsv` rows; one table, both languages | rewritten runtime state; newer bytes win, and every value here is safe to delete (the next write rebuilds it) |
| `history/` | `paths.evals_history()` (`history/evals-history.jsonl`), the native triage health door (`crates/fno-agents/src/backlog/triage_health.rs`, `history/health-history.jsonl`), `crates/fno-agents/src/think_inspect.rs` and `scripts/memory/append-lesson-candidate.sh` (`history/lesson-candidates.jsonl`) | append-only jsonl journals |
| `worktrees/` | `fno agents workspace worktree ensure` under the worktree policy (`.claude/rules/worktrees.md`) | fno-managed external worktree base, `<repo>/<name>`; reaped on merge |
| `board.py`, `board.sh`, `board_ids.py`, `board_render.py` | the operator, by hand (not repo files; the `my-priorities.md` row above already names `board.py`) | permanent operator tools |
| `validity-decks/` | `crates/fno-agents/src/backlog/maintenance_validity.rs::write_validity_deck` (via `fno backlog maintain`) | one deck per validity run, keyed by timestamp and node; permanent record |
| `.env` | the operator, by hand: the file's own header says it holds global fno secrets (model-routing keys) | until the operator rotates the key. Never echo its contents into a log, a receipt, a fixture, or a PR body, and read it only when the task requires it. |

The root journal family is retired. The name `events.jsonl` survives as a store address only: writers commit to `db/events.db` and readers query the store. The daemon's migrate pass imports any `events.jsonl*` file that reappears at the root and parks it under `backups/state-root-migration/`. `fno doctor event signals --check` reports a reappearance as a `legacy_journal` finding.

## Machine and foreign junk

Not written by anything in this repo. Named so the gate can tell known junk from new drift.

| Entry | Writer | Lifetime |
|---|---|---|
| `.DS_Store`, `.metadata_never_index` | macOS Finder and Spotlight | regenerates on view; safe to delete |
| `.claude`, `.fno`, `.impeccable` | foreign plugins and nested workspaces whose cwd was the state root | leave in place, per the foreign-debris section below |

## Unclassified (follow-up filed)

Present in the real root, no writer found, not confirmable as dead inside this task's budget. The gate allowlists exactly these names. A follow-up node owns the verdict.

| Entry | What is known | Follow-up |
|---|---|---|
| `tailscale-migration/` | Operator's 2026-08-22 tailscale migration scripts and status snapshot. Infrastructure work, not an fno writer. | follow-up node "Rule on two operator-owned state-root leftovers", filed 2026-09-08 |
| `tools/` | One operator script, `wake.sh` (2026-08-15). Not written by the repo. | follow-up node "Rule on two operator-owned state-root leftovers", filed 2026-09-08 |

## Unrotated logs

Real writers, no rotation, no deleter. The pr-watcher and groom logs moved under `logs/`. Their rotation is still unclaimed work.

| Entry | Writer | State |
|---|---|---|
| `logs/pr-watcher.out.log`, `logs/pr-watcher.err.log` | `pr_watch/_install.py` | unbounded |
| `logs/groom.out.log`, `logs/groom.err.log` | `crates/fno-agents/src/backlog/groom.rs` | unbounded |

## Session-scoped state

One file per session, per day, or per throttle window.

| Entry | Writer | Lifetime |
|---|---|---|
| `latches/.context-nudge-*` | `hooks/context-nudge.sh` | pruned by the same hook at `-mtime +2` |
| `latches/.spend-drift-no-node-<session-id>` | `hooks/spend-drift-monitor.sh` | written once per session when Node.js is absent, so the notice prints once. Pruned by the same hook at `-mtime +2` |
| `latches/.worktree-create-<session-id>` | `hooks/worktree-setup.sh` (and its /speculate copy) | create-attempt counter, deleted by the same hook on a successful create, stale copies pruned at `-mtime +2` |
| `.a2a-confirmed` | `agents/dispatch.py` | single file, overwritten |
| `.active-backlog-nudge` | `crates/fno-agents/src/active_backlog.rs` (the keeper's publish touches it) | single file |
| `.think-spawn-daily.json` | `crates/fno-agents/src/think_daily.rs` | daily, overwritten |
| `.preflight-cancel` | `scripts/ci/preflight.sh` | consumed by the reader (one-shot; stale after one hour) |
| `.path-migration-done` | `setup/migrate_paths.py` | one-shot sentinel |
| `.preflight-receipt-locks/` | `scripts/ci/preflight.sh` | live lock dirs |
| `mail-hold/<handle>.json` | `fno/mail/hold.py` via `paths.state_dir()`, and `crates/fno-agents/src/mail_hold.rs` via its `state_root()` (byte-identical dialect, inventory in `docs/architecture/dual-implementation-inventory.md`) | one file per held session; a clock the conversation rules wrote adds an optional `"source": "conversation"` key; deleted by the release timer (the conversation grace lapses through the same timer), by the Rust command's turn-boundary tidy of lapsed clocks, and by `fno agents mail admin hold --off` |
| `mail-hold/<handle>.parked/` | `crates/fno-agents/src/mail_hold.rs` (`--park` writes `<ms>-<pid>.txt` payloads, `--run-parked` drains them) | parked raw payloads for one held session, waiting out its hold; each file is deleted by the runner on a sent payload and left in place on a failed send for the next park to pick up; lifetime ends with the hold plus one send |
| `state/route-settings/<sha16>.json` | `crates/fno-agents/src/route_settings.rs::write_atomic_0600` via the write-settings door (`cli/src/fno/agents/model_routing.py` forwards) (content-addressed, 0600, carries a live auth token) | one file per distinct route overlay, shared by every session on that route; the row drains rather than folds, so a file a registry row still names stays at its recorded path until the prune; a resume re-resolves provider-default tiers (`refresh_provider_default_tiers`), so a moved default yields a new file rather than a served stale one; files no registry row references and older than 14 days are pruned by `fno config route settings ls --prune` |
| `route-settings/<sha16>.json` (draining) | no new writes: the writer publishes into `state/route-settings/`; these are pre-fold files a registry row may still name | the drain keeps them until the 14-day prune empties the folder (`DRAIN_ONLY_FOLDERS`); the row and its baseline entry bank in the slice that removes the guard |
| `<claims root>/.fno/state/flight/<encoded key>.json` | `crates/fno-agents/src/single_flight.rs`, beside the claims dir it locks in (`$FNO_CLAIMS_ROOT`, else `$HOME`) | one file per distinct fno invocation; rewritten by each flight and read only inside the freshness window (`config.agents.single_flight_ttl_seconds`, default 10 s), so a leftover is inert rather than stale. Pruned past `ttl + join budget`, doubled, by `single_flight::prune_records` on the daemon's GC tick. Holds the child's stdout, which is the same text the command prints on a terminal |
| `state/locks/github-graphql-quota.lock` | `crates/fno-agents/src/pr_status/quota.rs` | permanent empty sidecar; flock lives only for the probe-plus-command critical section |
| `state/locks/github-request-budget.json` (+ `.lock`) | `crates/fno-agents/src/gh_budget.rs` via the `fno-agents fleet-incident gh-budget` door | rewritten per admitted request; stamps pruned past 60 s |
| `state/locks/github-request-spend.jsonl` (+ `.1`) | `crates/fno-agents/src/gh_spend.rs` | one line per admitted or refused request: caller, shape, bucket, points, verdict; rotated at 8 MiB to one previous generation |
| `state/locks/plan-<sha1>.lock` | `crates/fno-agents/src/plan_doc/lock.rs` (`PlanDocLock`) and `cli/src/fno/plan/locking.py` (`plan_doc_lock`), keyed on the plan doc's resolved path | advisory flock sidecar, one per plan doc the two writers rewrite; a held lock keeps the daemon's locks fold pending, and the takers re-resolve after the flock so the fold cannot split one; stale locks are reaped past `locks_retain_days` by the gc sweep |
| `bin/github-cli/gh`, `gh.pre-fno` | `setup/github_cli.py` via `paths.github_cli_proxy_dir()` | permanent proxy; one backup is retained only when an unrelated wrapper was present |

`.preflight-receipt-locks/` is the shape to copy. It derives from `dirname "$GLOBAL_EVENTS_PATH"`, so it already follows the resolver, and it sits at the root because the events file does.

`latches/` is the shape that had to be fixed. The hook that writes a latch also deletes it, because a latch keyed to a session id has no reader once that session ends. The lifetime lives with the writer, which is the only place it cannot go stale.

## Permanent repo-local directories

The rule above governs the state root, but its shape repeats inside a checkout: a directory nobody deletes is a directory nobody owns. A worktree that a script resets on every run is permanent by design. The sweep must keep it explicitly or churn its warm caches every pass. Recorded here so a sweep author checks this table before reaching for a blanket rule. A reader who finds the directory knows it is intentional.

| Entry | Writer | Lifetime |
|---|---|---|
| `.claude/worktrees/preflight` (or `<worktrees_base>/<repo>/preflight` when configured) | `scripts/ci/preflight.sh` | permanent; hard-reset to the candidate SHA each run, caches deliberately preserved |

The sweep's matching keep rule is `kept (permanent)` in `scripts/lib/worktree-lifecycle.sh`, keyed on the basename so it follows the worktree base wherever config puts it.

## Unattributed entries

Files present in one real install with no writer anywhere in the checkout. Recorded rather than deleted: an unexplained file is a finding, not a deletion, and a finding nobody wrote down gets rediscovered.

If you are cleaning up an install and hit one of these, find the writer first. If you confirm it is dead, delete the row here in the same change. The 2026-08-13 rows were confirmed dead and cleared by the 2026-09-08 backfill. `.env` graduated to a real row above, so no rows remain. The table returns here the next time an install produces one.

## Foreign and cwd-relative debris

An unexpected plugin or project-state directory nested *inside* the state root is not a root writer. Each holds project-relative paths written by a process whose working directory happened to be the state root. When `FNO_REPO_ROOT` is unset and `git rev-parse` fails, `paths.resolve_repo_root()` falls back to `Path.cwd()`. Foreign plugins do the same with their own literals.

Leave them. The finding is the cwd fallback, not the directories it produced.

## The project journal and `FNO_EVENTS_PATH`

The per-repository journal `<space>/events.jsonl` resolves through `paths.project_events_json()`. `FNO_EVENTS_PATH` overrides it. The override exists because repo-root resolution cannot be sandboxed. `fno.hermetic.neutralise` deliberately leaves `FNO_REPO_ROOT` unset. So an unpathed `append_event` under test writes a real row into the developer's space, and both operator readers fold that file. On 2026-08-17 six test fixtures sat in the needs panel beside two genuine operator questions.

`neutralise` pins the override at one line, and that env reaches the pytest, shell, and cargo trees. All three writers read it: the Python resolver, `scripts/lib/events.sh`, and `claim_events_path` in the Rust claims module. Rust has to read it because the two implementations share that journal and its `.lock.d` mutex as a wire contract. A pin one side ignores splits the writers apart. The loop-journal writers in `fno-agents` still build their path by hand and remain outside the pin. Reach for the pin, not for a marker the fold recognises. A fold that must know about test data carries an exception list, and the next fixture that misses the list refills the queue in silence. A test that sets `FNO_REPO_ROOT` itself and reads the journal back must name the same file in `FNO_EVENTS_PATH`. The pin outranks the root.

## Event retention and its readers

Each `events.db` store prunes by event kind: the project journal, the global journal, and the agents lifecycle journal. The prune mechanics live in [event-log-storage](architecture/event-log-storage.md). This table names who reads each pruned kind, and how far back. A **GAP** row has a reader whose window is longer than the retention. That reader sees a short or skewed history, and nothing tells it so.

Every pruned row leaves a count in the `event_rollup` table, per UTC day, kind, and subject. `fno doctor event signals` prints those counts as its `pruned` list. So a kind that still fires often stays visible after its rows are gone.

| Event type | Retention | Readers and their window | Status |
|---|---|---|---|
| control_plane_tick, machine_watch arm | 720h | `fno-agents intel --fleet`: 30 days of load readings. `fno doctor evals macro --all`: 30 days. | Fixed. The prune keeps these rows for the full 30 days. |
| control_plane_tick, other arms | 168h (telemetry) | `fno agents loop control table`: 7 days (`ARM_WINDOW_FLOOR_S`). `fno do pr watch status`: newest rows. `fno doctor event signals`: 24h plus six prior days. `fno doctor evals macro --all`: 30 days. | Fixed for the loops table. The config reader caps `notify.arm_starved_after_s` at the 168h retention. **GAP** for `evals macro --all`: it sees 7 of its 30 days. The rollup keeps the tick count per arm. |
| guard_decision, allow rows | 24h | [hook-budget-audit](architecture/hook-budget-audit.md) block rate, `block / (allow + block)`. `fno doctor event find guard_decision`: its coverage proves only 24h. `fno doctor evals macro --all`: 30 days. | Fixed for the block rate. Take allows and blocks from the rollup plus the live rows, or count both inside the last 24h. A raw row count over a longer window still reads high. |
| guard_decision, block rows | 168h (telemetry) | The same hook-budget audit. `fno doctor evals macro --all`: 30 days. | Fixed. The rollup keeps the block count per guard. |
| inside_leg_report | 168h (telemetry) | `fno-agents subscribe`: live, from connect time. The spawn-journal scan skips it as noise. | None. A same-state repeat now journals no row at all. |
| codex_thread_inside_leg | 168h (telemetry) | No reader in the tree. | None. A same-state repeat now journals no row at all. |
| store_seat_lock_unlinked, store_socket_unlinked | 168h (telemetry) | `fno doctor event signals`: 24h `burst` and `spike`, with six prior days as the baseline. | None at the default window. |

Retros read none of these kinds. `fno backlog retro` folds `gate_escape` only, and that kind is durable. The self-improvement loops in [self-improvement-loops](architecture/self-improvement-loops.md) read none of them either: autocorrect, the S2 corrections path, the evals regression banks, and pr-watch heal. The one evals reader is the `evals macro` fold. By default it drops `guard_decision` and `control_plane_tick` as noise, so only `--all` meets the one open gap.

## Store contracts

The rows above say where a file lives. A contract says how a store behaves. The SQL cutover kept every caller-visible interface and lost the properties nobody wrote down. The registry sat at revision 0 for days. Mail serialized on one registry lock. One graph write held its lock 16 to 26 s while it re-read the whole export. The Stop hook loaded 1.3M event rows. The tests ran against a 34-row registry while the live one held 227.

Each store gets one row. A PR that changes a store's writer, reader, lock or read pattern accounts for each cell in its body. A port PR proves each cell still holds. An "unmeasured" size is a debt: measure it on a real install before the port lands, and write the number here.

| Store | Owner and writers | Readers and their read pattern | Lock scope | Expected size | Health signal |
|---|---|---|---|---|---|
| Backlog graph (`db/graph.db`) | `crates/fno-agents/src/backlog/`; every write goes through `fno backlog` | board, groom, dispatch: indexed row reads. No reader loads the whole export on a hot path | one write transaction per mutation, never a whole-export re-read under the lock | unmeasured | no budget yet; the live-scale check sets one |
| Agent registry (the `registry` table in `db/graph.db`) | `crates/fno-agents/src/registry_store.rs`, compare-and-set through `registry_commit.rs` | `fno agents status`, mux doctor, the stop gate: `registry_read::registry_text` reads the rows once per call | the write transaction only. Mail and claims must not take the registry lock | 227 rows on one live install; fixtures must match that scale | `registry_meta` `revision` grows while sessions run. `fno mux doctor` warns on revision 0 with rows, and on live sessions with no row stamp in the last day |
| Event journals (the project, global and agents events.db stores) | `crates/fno-agents/src/event_store/` | the stop hook, `fno doctor event`, evals: windowed reads by kind and time, never every row | one append transaction per event | 1.3M rows seen on one live install | rows keep arriving while sessions run; the prune leaves its counts in `event_rollup` |
| Claims (`claims/*.lock`) | `crates/fno-agents/src/claims.rs` | claim status, the spawn gate: one lockfile per key | one atomic lockfile per key | one file per live claim | no claim outlives its holder pid past its TTL |

The hot-path budgets these stores serve live in `crates/fno-agents/tests/hook_latency.rs`: Stop, Bash PreToolUse and the other hooks, each with a p90 budget. CI enforces them on Linux. Their fixtures still use small stores, so a budget there does not yet prove the live scale above.

## Adding a new root writer

You do not. The root rows are shrink-only (see The rule). The runtime guard refuses a new top-level write, and the CI gate fails a PR that adds a row. The remedy for a new surface is a subfolder:

1. Put the state under a folder this page already names. Reach for the root only through a row that already exists.
2. Add the accessor to `cli/src/fno/paths.py` so the location follows `config.state_dir`. When a bash caller needs it, export it from `crates/fno-agents/src/paths_cli.rs` `emit_paths_sh_defaults`, then run `fno config paths emit-shell` to regenerate `scripts/lib/paths.sh`.
3. Name the deleter. Ephemeral state gets its lifetime in the code that writes it, not in a separate janitor. A janitor drifts from the writer and goes unrun. `scripts/prune-fno-dir.sh` was deleted for exactly that: never once invoked, while every file on its delete list sat in the root.
4. If a row above became wrong, fix or delete that row in the same PR. Shrinking is the one direction the gate allows.

## The project space (`~/.fno/spaces/<slug>/`)

Project state left the checkout. One space per repository, keyed on the CANONICAL repo root (the git common dir's checkout), slug = the canonical path with `/` swapped for `-` (Claude's project-dir shape: read the directory name, see the path). Every worktree of a repo resolves to ONE space, so cross-worktree state needs no symlink. Per-worktree state sits at `<space>/worktrees/<worktree basename>/`. A checkout keeps only `.fno/config.toml` (committed project config) and the sandbox breadcrumb below. The first resolve of a moved file renames the legacy `<repo>/.fno/<file>` into the space and leaves a `<repo>/.fno/MOVED-TO` pointer naming it.

The deleter: a whole space (or a `<space>/worktrees/<name>` slice) whose source dir no longer exists is pruned by the `stale_spaces` lane of `fno doctor reclaim` once its newest entry is quiet past 24 h. The `/`-to-`-` slug swap is lossy, so the lane judges a source gone only when NO segmentation of the slug names an existing dir; git's worktree list is the one authority for slices; a failed read or an entry inside the window keeps the space.

| Entry | Writer | Lifetime |
|---|---|---|
| `<space>/events.jsonl` | `paths.project_events_json()`; legacy bytes only since the event-store cutover | import source |
| `<space>/events.db`, `.db-wal`, `.db-shm` | the `fno-event-store` crate, the authoritative event store beside each journal | durable and gate rows forever, ephemeral 672 h |
| `<space>/claims/` | `fno.claims` for repo-local keys (`walker:`, `review:`, `reap:`); global-id keys (`node:`, `dispatch:`, ...) stay at the global root | re-acquirable leases |
| `<space>/leads/<scope>.md` | `crates/fno-agents/src/lead_state.rs` via promotion or `fno agents org init` | one loop-state file per live role scope; stale files are inert without a live registry role and cleanup is best-effort (`fno agents org done` on departure) |
| `<space>/leads/<scope>.md.lock`, `.md.tmp` | `lead_state.rs` / `loop_lead.rs` / `lead_ops.rs` over the manifest lock | lock lives only for the critical section; tmp is replaced on every locked write |
| `<space>/leads/<scope>.wake.json` | `lead_wake_triggers.rs` (the daemon's wake arm) | arm-local trigger cache with no lead meaning: `board_hash` + `board_rows` (the board-change trigger) and `answered_cursor` (the answered-escalation trigger); refreshed only when a wake fires, so it never outlives the manifest beside it |
| `<space>/leads/<scope>.wake.json.lock`, `.json.<pid>.tmp` | `lead_wake_triggers.rs` over `lead_ops.rs`' manifest-lock helper | lock lives only for the sidecar's read-modify-write critical section; the pid-suffixed tmp is replaced on every locked write |
| `<space>/leads/<scope>.md.wake.log` | `lead_wake_triggers.rs` (detached wake-mode walk) | append-only stdout of the walks the arm spawned; the events journal is the receipt, this log is diagnosis |
| `<space>/plans/` | `paths.plans_dir()` default (a configured vault template still wins) | permanent plan docs |
| `<space>/status-sinks/` | `fno-agents state path status-sinks` | per-sink cursors + error logs |
| `<space>/carveouts.jsonl`, `.lock.d/` | `cli/src/fno/carveout/core.py` via `paths.project_log` | append-only ledger; consumed by the retro-triage harvest |
| `<space>/worktree-log.jsonl` | `hooks/worktree-setup.sh` | append-only worktree lifecycle log |
| `<space>/wake-signals/` | `cli/src/fno/wake/signal.py` | one-shot records; drained by the wake readers |
| `<space>/artifacts/consolidated/` | `scripts/lib/consolidate-artifacts.sh` | per-PR consolidated gate artifacts, shared across worktrees |
| `<space>/scratchpad-adjacent diagnostics`: `loop-check.stderr.log`, `finalize.stderr.log`, `.loop-check-unavail-*`, `.lead-resolve-unavail-*`, `.think-offer-cursor` | `hooks/target-stop-hook.sh`, `hooks/footnote-agy-target-stop-hook.sh`, `hooks/born-with-why-offer-inject.sh` | bounded retries/diagnostics; counters self-heal on the first clean decision |
| `<space>/worktrees/<name>/target-state.md` | `hooks/helpers/init-target-state.sh` via `fno do target init` | write-once per target session; archived on a terminal |
| `<space>/worktrees/<name>/run-log.jsonl` | `crates/fno-agents/src/loopcheck.rs` through `run_state::append_transition` | append-only per worktree; retained as the lifecycle fold and deleted with a disposable worktree |
| `<space>/worktrees/<name>/.claim-heartbeat.stamp` | `hooks/claim-heartbeat.sh` | one key=value outcome ledger, replaced each renewal window; its mtime is the 20-minute throttle |
| `<space>/worktrees/<name>/scratchpad/` | `/target` sessions, per the manifest's `scratchpad_path` | live session scratch; archived at session end |

Target state is write-once after init. Lead state is atomically refreshed at promotion and both gate a stop hook.

A lead runs in the canonical checkout. A target manifest can sit there too. So the lead gets its own file rather than a `driver:` field on the target one. A manifest whose name says target and whose contents say lead is how two sessions come to share one discriminator.

## The checkout-local exceptions

Two files stay inside a checkout, each because moving it breaks the thing that writes it:

| Entry | Writer | Lifetime |
|---|---|---|
| `.fno/config.toml` | `fno config project init` / the operator | committed project config, the same class as `.claude/settings.json` |
| `.fno/state-root-denied.json` | the DENIED worker, through `crates/fno-agents/src/claims.rs::write_state_root_breadcrumb` and its Python twin `cli/src/fno/claims/io.py::_breadcrumb_path` | until the next successful claim on this repo, which deletes it |
| `.fno/branch-provenance.json` | the pr-watch tick's stranded leg, via `cli/src/fno/branch_provenance_cache.py::write_cache` | overwritten every tick, never appended; safe to delete (the next tick rewrites it); the Kanban board's Branch Provenance section reads it |

The breadcrumb is here because it is the only thing a mute worker can still say.

A worker whose sandbox denies the fno state root has lost the claim store, the mail bus and the spawn mutex in one move. It cannot report that, because reporting is the capability it lost. It reports into its own transcript, which nothing reads, and the fleet meanwhile sees a live row with progress advancing. Five workers died that way in one night. Two of them finished real work nobody heard about.

The same sandbox that took the state root left the repo writable. So the refusal is written here, inside the one root the worker demonstrably has, and the operator reads it from outside the sandbox. Moving it into the space puts it behind the very grant that was denied. The write is best effort and never raises: a breadcrumb that cannot be written must not become a second failure stacked on the first.

It carries the denied absolute root, the harness session id, and a UTC timestamp. A successful claim write clears it, because a stale breadcrumb reads as a live problem forever.

## Recover parked history

Run `fno doctor event recover --root ~/.fno` for a read-only audit. The command reports each parked store, missing identities, conflicts and a packet digest. Unsupported schemas or changing sources refuse recovery. It never removes a backup.

Event families use `event_id` rather than the source sequence. Recovery preserves every non-sequence value and assigns destination sequences. A `recovery_history` ledger commits with each imported family. Historical reads remain visible. Activity readers suppress recovered rows without advancing across genuine concurrent controls.

For copied stores, use `--apply --copy-proof --packet-digest <digest>`. The physical root must be under an OS temporary directory. Each destination must have a different inode from its live counterpart. Pass copied consumer files with repeatable `--sidecar <path>` arguments. Sidecar bytes are captured before the audit, so a later genuine edit differs from its recovery baseline.

Live apply requires `--apply --packet-digest <digest> --packet <file> --approval <decision-id>`. The packet names the absolute root, audit digest, zero historical effects, genuine controls handled once, consumer inventory, verified artifacts and consumer sidecars. An active operator law must state `approve state recovery <absolute-root> <audit-digest> <packet-file-sha256>`. Agent or role coordination cannot grant apply. A worktree binary cannot apply to operator stores.

Before any family writes, recovery snapshots all five stores through SQLite's backup API and checks integrity. Snapshot receipts include hashes, inode identities, schema versions, row counts and sequence high-water marks. Required sidecar failures abort. Resuming the same packet verifies its original sources and snapshots, then imports only its remaining approved identities. Conflicting live identities refuse instead of overwriting concurrent writes.

Include existing question pages in the sidecar inventory. Recovery does not deliver a new page for an old ask. An unchanged historical page cannot record an answer. A later page edit remains actionable. A missing or altered baseline holds that page rather than replaying its old tick.

Approvals and archive stores are audited by owning primary keys. Derived archive FTS tables are excluded from identity counts. Missing owning rows in either family hold apply until their replay-safe recovery exists. `zero_missing` requires every audited family to be complete and readable.

Keep migration backups, recovery snapshots and the ledger. Pruning needs a fresh zero-missing audit and separate operator approval; for the migration stamp dirs that approval is `reclaim.backups_retention_days` (or `FNO_RECLAIM_BACKUPS_DAYS`) plus an audit verdict proving `zero_missing` (`FNO_RECLAIM_BACKUPS_AUDIT`) that predates the stamp, bounded to the newest 5 always kept. Recovery snapshots are never swept by the janitor. Never restore a snapshot over live stores automatically. Such a restore can discard intervening writes. Rollback requires an approved identity-based plan that accounts for those writes.

### Claim table transition

`graph.db` owns active claims and `claim_history` retains deleted generations for audit. The native claim store imports legacy lockfiles once, retains their snapshot under `backups/claims-table/`, and replaces `claims/` with a migration marker that refuses older file writers. `claim-aux/` holds host-local admission tickets and held-request diagnostics, owned by `claim_queue`, `test_run`, and `flight_gate`. Waiters remove their tickets on exit, readers reap dead tickets, and a fresh flight clears its diagnostic count. Upgrade clients before resuming claims after this transition.

Role vocabulary upgrade receipts live in `migrations/role-vocabulary-v2.done` under each migrated state root, owned by the native role migration. A `v1` receipt is stale: its walk skipped a symlinked spaces root, so the migration runs again past it. The receipt lasts for the installation lifetime and is written only after every selected file has migrated successfully. No new top-level state file is created. The parent of `FNO_AGENTS_HOME` is a migrated root too, because readers find spaces there. When new code has already written a current role directory such as `leads/`, the legacy directory folds into it. A name in both keeps the current file and parks the legacy file beside it as `<name>.superseded`.

## Native join and reconciliation writers

`backlog/advance_join.rs` writes join briefs under `<worktree>/.fno/join-briefs/` and visitor policies under `<worktree>/.fno/join-partition/` before launching visitors. These existing named subfolders retain state for the held plan's join run and the visitor's enforced band. `backlog/advance_contract.rs` uses a `reconcile:<node>` key in the existing claim store for its six-hour pending lease, released after an accepted or redundant reconciliation launch. The key is a database record; it creates no state-root file.
