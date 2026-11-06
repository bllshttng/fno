# Reaping FAQ: why is this row still here?

A reaping sweep judged a session row and kept it. This page names the keep reason and tells you what to do. It covers every program that stops or removes a session or one of its parts. It states the order they run in. It names every key of `fno agents registry reap --json`.

Run `fno agents registry reap --dry-run` and find your row handle in the report. The dry run classifies every row, names one reason per row, and writes nothing. It stops no process, prunes no tree, and writes no receipt.

Counts move within the hour, so run your own dry run and date the result.

## Is this page for you?

A reaping sweep kept a session row and you want to know why. You want a row gone and reap refuses. You asked which condition acts first and got told no doc answers. This page owns the keep reasons and every program that stops or removes a session or one of its parts. It owns the order they run in. It owns every key of the reap JSON report. Misreading it makes you force a delete the machine will re-judge on the next sweep, or kill a worker whose row was telling the truth.

Not for: the worktree removal contract (which trees prune on merge and which never do). That is answered at [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) and owned by [worktree-mechanics.md](architecture/worktree-mechanics.md) (Placement).

## Read the answer

Every report line has four parts: the command, the row handle, the class in brackets, and the reason in parentheses.

```
kept fb6399d5 [unmarked] (brainstorm: no fno skill in the transcript)
```

The command names the verdict. `kept` means the sweep declined this pass. `held` means the rehearsal declined to promise an action. `would retire` is the dry-run remove verdict, and `retired` is the real one.

The first report lines carry the counts and the `fleet`, `unmarked`, `owned`, and `contested` class totals. The last lines carry the dry-run marker and the mux sweep line. A dry-run report also carries one census line: `session inventory: enumerated N session(s), M never judged (no registry row)`, plus one line per unread census source and one for a partial transcript-root read. An unread registry replaces the count with `never-judged count unknown (registry unreadable)`, because every session then lacks a registry row and a count would read as a diagnosis. The gap between N and the judged rows is where a scoping bug reads from outside. A retire line names its basis, for example `every named node done`, with the witnesses that agreed.

Every daemon tick with held rows also writes one journal row you can read without the command. The row carries one entry per held handle, with its reason, detail, age, and escalation flag. Read it with:

```
rg '"type":"retire_holds"' ~/.fno/agents/events.jsonl
```

A tick with zero holds writes no `retire_holds` row, so silence is the zero-reading.

## Who decides a session is live

**fno's rows decide. The vendor only checks.** Ruling d-e096c669 (2026-10-01) set the order. fno holds the provenance, so every attach, restore, liveness, and reap path reads fno's own evidence first. A vendor surface can confirm a decided verdict or raise a drift event. It never decides alone, and its silence never blocks fno.

**The one door is `crates/fno-agents/src/row_verdict.rs`.** `fno_verdict` reads fno's evidence in this order:

- a terminal registry status or finalize outcome
- an `inside_leg` report inside its TTL
- a recorded pid the ESRCH probe proves gone
- a live pid
- else `Unknown`

`reconcile` folds a vendor word in. A vendor word turns an `Unknown` into a verdict. A word that disagrees with a decided verdict keeps fno's verdict and names the disagreement. The reentry revive gate, the gc and rm death proofs, the liveness sweep, and the worked fold read through it (`reentry.rs`, `gc_sweep.rs`, `daemon/roster_death.rs`, `liveness_sweep.rs`, `backlog/worked.rs`). The Python legs in `advance.py` and `watchdog.py` read fno's truth probe and registry spine, with the vendor roster demoted to a state column.

## Why is my finished worker still on the roster

Three keeps used to hold finished rows with no way out. Each now reads a live reason, and each keeps refusing a specific wrong answer.

- An open-PR hold asks GitHub once the row is quiet past the grace. A closed or merged PR releases the row. An open PR keeps it, and so does an unread answer, under `pr state contradicts`. A fresh row is never read and never released by this arm.
- An adopted row keeps while its harness record says the session exists. A recorded pid that answers ESRCH releases it to the ordinary gates. So does a claude row missing from a known `claude agents` read. An unknown or partial roster read keeps the row: a failed instrument is never absence. The registry keep is not the whole story. An adopted, untitled row does not shield its listed session from the roster sweep.
- When its inside-leg report reads done and fno never stopped it, a no-node row releases once its transcript goes quiet. A row still working, blocked, or stopped by fno keeps. The keep now carries a clock, so `fno agents registry reap --release` reaches it. A no-node session you want kept takes a keep mark: `fno agents registry reap --keep <row>`.

Quiet is only ever the second conjunct. Every release rests on a positive marker: GitHub answering, a roster read answering, a dead pid, the worker's own done report. Silence alone releases nothing.

## Ten programs stop or remove a session

Ten programs stop, retire, or remove a session or one of its parts. A reader who watches one and concludes the others are broken has mixed them up. This exact confusion cost a real session: the arms readout showed `acted=0 skip=held` while the manual command retired 3 rows in the same minute. Each row cites its entry point as file plus symbol, because a line cite rots and a symbol cite survives a move.

| # | Program | Entry point | Trigger and cadence | Removes | Keeps | What it reads about an open PR |
|---|---|---|---|---|---|---|
| 1 | Registry sweep | `gc.rs` `gc_sweep`, policy `gc.rs` `gc_decide`; daemon arm `gc.rs` `maybe_retirement_sweep`; manual command `client.rs` `run_reap`; the dead-role step `role_reap.rs` `sweep` rides the same arm and command | daemon `agents.retire_interval_s`, default 300 s (`agents_config.rs` `DEFAULT_RETIRE_INTERVAL_SECS`), one pass in flight; grace `agents.retire_grace_s`, default 900 s (`agents_config.rs` `DEFAULT_RETIRE_GRACE_SECS`) | the session process, the native surface, the registry row, a clean-and-merged worktree, and a dead role's manifest (set aside as `<name>.md.vacated` so a boot revival can re-seat it) plus the cancel sentinel | a keep mark, the receipt with the resume command, the transcript, the branch, a dirty or unmerged tree, and every role whose holder is not proven dead | an open PR holds the row once it is quiet past the grace; `gc_sweep.rs` `open_pr_verdict` reads GitHub once per pass |
| 2 | Roster sweep | `roster_reap.rs` `roster_reap`; scheduled in the retire arm (`gc.rs` `maybe_retirement_sweep`); manual `fno-agents roster-reap` (`client.rs` `run_roster_reap`) | the retire cadence at scope `agents.reap.roster_scope`, default `Provenanced` (`agents_config.rs` `DEFAULT_ROSTER_SCOPE`); the manual command is a dry run until `--apply` | claude sessions no fno row owns: the native session, with a receipt | a keep mark, open work keeps unless the row is a finished planner whose `sessions[]` assignment is closed or a recorded territory blueprinter; both retire only when terminal and quiet past grace | reads only provenance: a `sessions[]` row fno wrote, a staged reap receipt, or a claimed launch record naming a launcher session makes the session owned; PR state is not read |
| 3 | Mux tab prune | `gc.rs` `mux_tab_sweep`; daemon default flags in the retire arm (`gc.rs` `maybe_retirement_sweep`); the manual command adds `--include-used-shells` (`client.rs` `run_reap`) | every registry pass; the manual command runs it too, `--no-mux` skips it there | the mux tab of a worker the mux server reads dead | a live pane, a used shell unless `--include-used-shells` | not read |
| 4 | Merge reaper | `merge_reap.rs` `consume_merge_cleanup_requests` | a recorded merge cleanup request. It has a 60 s floor (`merge_reap.rs` `MERGE_REAP_INTERVAL_SECS`) and expires 86400 s from the mint (`merge_reap.rs` `MERGE_REAP_EXPIRY_SECS`). The tree resolves from the branch when the request names none ([worktree mechanics](architecture/worktree-mechanics.md#the-dirty-bucket-under-a-done-node-law-d-cfcf5a8e)) | rows of the merged tree through the shared stage and commit, then the tree itself, with force | titled and operator rows, a keep mark, the branch, a process cwd inside the tree, or an unreadable cwd probe | reads each request node's `status` and `merge_status`. Additional PRs are not read |
| 5 | Worktree cleanup sweep | `daemon/worktree_sweep.rs` `worktree_sweep`; manual `fno agents workspace worktree cleanup --merged` (`scripts/lib/worktree-lifecycle.sh` `cleanup`) | a 21600 s interval (`daemon/worktree_sweep.rs` `WORKTREE_SWEEP_INTERVAL_SECS`), and it applies while a merge cleanup request waits (`merge_reap.rs` `merge_cleanup_requested`) or the pass's own dry-run judgment finds an eligible tree (an outside merge mints no request - the PR-watch tick mints one for a merged PR with none standing, so the grace is shared); the manual command is a dry run until `--apply` | the tree through the archive script, then dead job records | the branch, and every unpushed, unmerged, live-claim, or unborn-branch tree | reads git ancestry, not the PR state: merged means reachable from `origin/main` |
| 6 | Terminal-stop marker sweep | `daemon.rs` `terminal_stop_sweep`; markers `terminal_stop.rs` `write_marker` and `read_markers` | markers `finalize` writes on a terminal loop decision; read on every daemon tick | the session process through `claude stop`, with a 15 s bound, and the marker | the row, the tree, the node, and the claim; the roster state goes terminal, and the registry sweep retires the row on a later pass. A keep mark on the session or its lineage spends the marker with no stop and logs the keep | not read; the loop's finish line is before merge by design |
| 7 | `fno agents session rm` | `daemon.rs` `handle_rm` and `handle_rm_with` | a person, the post-merge ritual, or the watchdog sandbox lane | the row, the native session, the pane, and a reapable tree | the transcript; a keep mark when the caller names a machine reason with `--audit-reason`, `--force` included; the refusal names the mark and the unkeep command. A person's own rm names none and still removes | not read; rm reads no node state |
| 8 | Post-merge ritual row removal | `crates/fno-agents/src/post_merge/ritual.rs` (the archive leg, and the `reap-rows` leg when `self_reap` is on) | `/fno:ship pr merged` after `gh` reads `MERGED` | rows through `fno agents session rm --audit-reason post-merge-ritual` | the transcript, the branch, and a keep-marked row | merged by construction; additional PRs are not read |
| 9 | Watchdog and recovery lanes | lane sets `watchdog.py` `LANES`, apply `watchdog.py` `apply_verdict`, gate `watchdog.py` `_gate_reason`; keeper `keeper_lane.py` `reap_keepers`; stop-then-respawn `recovery.py` `recovery_sweep`, `_redispatch`, `_respawn_bg_resume`, `_revive_bg_thread` | the pr_watch tick when `recovery.watchdog.enabled`; keeper and recovery legs have their own gates | wake and silence remove nothing (they resume); the sandbox lane force-rms a codex row; a keeper reap group-kills; a recovery leg stops a stale worker and respawns one in the same tree | a live claim or owner keeps the row; the sandbox lane's rm holds on a keep mark; a failed spawn after a stop can leave the node driverless. The recovery stop and respawn is a rate-limit rescue and does not read the mark | checks only that the node is not done; it does not read the PR |
| 10 | Orphan process sweeps | fno-py orphans and orphaned stdio MCP servers (the codegraph family: a `serve --mcp` argv pair, or the codegraph `--liftoff-only` launcher flag paired with a codegraph path token) in the retire arm (`gc.rs` `unowned_sweeps`); test binaries `orphan_reap.rs` `maybe_sweep` on a 300 s cadence; keeper registry sweep at daemon start (`daemon.rs` `keeper_registry_sweep`); manual `fno agents fleet orphans --reap` | daemon cadences or a person | the process: ppid 1, age past its floor, pid not in the registry | a registry pid is excluded, so a tracked worker is safe; dead keepers' sockets are unlinked | not read |

The manual command and the registry arm run the same sweep body (`client.rs` `run_reap`, `gc.rs` `gc_sweep`). The registry arm runs one sweep at a time behind a one-in-flight gate (`gc.rs` `maybe_retirement_sweep`). A slow sweep holds the next request, so the effective cadence is not the interval. Run `fno-agents status` to read the arms table. The roster dry run reports each row's `class` as `fleet`, `unmarked`, `owned`, or `contested`.

The merge-request arm is a different program (`merge_reap.rs` `consume_merge_cleanup_requests`). It loops only over pending merge cleanup requests. With no pending request it does nothing. Its skip reasons are `no_requests`, `all_in_grace`, and `held`, and its detail line carries the held count beside the request count. So its `acted=0 skip=held` line says nothing about the registry sweep.

A fourth tool answers to a related name. `fno-agents roster-reap` removes claude rows that fno never registered. The daemon's retire arm schedules it after the registry sweep at the configured `agents.reap.roster_scope`, with `dry_run` false. The manual command stays a dry run by default, and `--apply` acts (`client.rs` `run_roster_reap`). The scheduled pass retires only on an ownership marker. The marker is a `sessions[]` row fno wrote, a recorded territory blueprinter name, or a reap receipt an earlier retirement staged. It now also reads a claimed launch record naming a launcher session. That record is the one `native_bg_mint` already reads as `origin: spawn`. Weak provenance, a name pattern or a transcript mention, keeps.

## Neighbors that remove no session

These five move or remove state around sessions. None stops or removes a session, so do not look for them in the table above.

- Claim reclaim: lazy, on a competing `acquire`. The old lockfile moves to `.expired/` (`claims.rs` `acquire`, `claims.rs` `classify`). No daemon arm reclaims claims.
- The nudge ladder: keeps the row and sends input instead (`pr_nudge.rs` `run_ladder`). It fires on the daemon arm only. The manual dry run prints its plan as `would nudge {id} ({action})` and takes no effect.
- The state-file sweep: removes expired claims, stale plan locks, agent locks, the pr-status cache, and claim tmp files (`gc.rs` `state_file_sweep`). No row is touched.
- The liveness sweep: bands the machine and writes status. It removes nothing (`daemon.rs` `liveness_sweep`).
- The daily janitor runs `cargo_build_dirs` and `codex_cache_quarantines`. The daemon also runs `cargo_build_dirs` alone every hour (`maybe_run_cargo_hourly`). A busy fleet can fill the build base in less than a day. It removes cargo artifacts and stale Codex backups under `<codex home>/footnote`, never sessions (`reclaim.rs` `maybe_run_daily`, `reclaim.rs` `cargo_build_dirs_lane`).

## In what order

**Across programs, no arbiter exists.** Each daemon arm runs off-loop behind its own one-in-flight gate. The tick's issue order is the retire arm, then the worktree task, then the orphan test-binary sweep, then liveness, then terminal-stop (`daemon.rs` select arm). That issue order is not a completion order. The first program to act wins and the others find the row gone. The merge reaper and the retire arm share one stage and commit. The merge reaper calls them with no release, so it is the stricter of the two.

**Inside the retire arm**, in this order: state-file sweep, dead-role sweep, registry sweep, nudge ladder, unowned sweeps, roster sweep, mux tab prune (`gc.rs` `maybe_retirement_sweep`). The dead-role sweep runs before the registry sweep, so a role it vacates frees the territory this tick. Its `roles=` counts ride the arm's detail line. The roster sweep runs after the registry sweep on purpose. A row the registry sweep retires this pass is already gone from the registry the roster sweep loads. A session the roster sweep removes becomes a corpse for the next registry pass.

**Inside the registry sweep, per row**, the first gate that answers decides. Derived from `gc_sweep.rs` `run_with_release` and `gc.rs` `gc_decide`:

1. lead role or team: `kept {id} (teamed)`
2. keep mark: `kept {id} (keep mark: ...)`. Set by `fno agents registry reap --keep <row>`, lifted by `--unkeep`
3. graph unreadable: `kept {id} (graph unreadable: never a retirement on a failed read)`
4. attended: `kept {id} (attended: ...)`
5. policy `gc_decide`, in order: a live claim, an open PR the session drives, a brainstorm, then the idle window
6. live descendant: `kept {id} (live descendant: {child})`, skipped for a terminal row
7. freshness re-check, in the dry run too: `kept {id} (active: ...)` or `kept {id} (probe unread: ...)`
8. the stop gate and receipt stage: the `needs live stop`, `stop refused` and `no resumable receipt` lines
9. the dry-run unverified gate: the `dry-run did not evaluate` line
10. the tree verdict, then the commit: `kept tree {id} (...)` lines, then the retire lines. At commit the open-PR guard keeps a driver row whose node still records an open PR (`gc_open_pr_guard.rs` `hold_open_pr_reaps`)

Origin and open work no longer keep a row. An operator row and an adopted row take the same gates as a spawn row. Only their idle window differs (the table above).

## A dry run and a real run answer different questions

A dry run classifies exactly as a real run does. It subtracts three things:

- It never stops a process. A rehearsal must not kill the worker it rehearses on (`gc_sweep.rs` `run_with_release`).
- It never prunes. Receipt retention is set to zero, because a rehearsal that prunes is not a rehearsal (`gc_sweep.rs` `run_with_release`).
- It subtracts the planned settle from the graph read. The report shows the outcome the real pass produces (`gc.rs` `gc_sweep`).

The `held` line is the trap. The line reads `held {id} (needs live stop: {reason})`. The condition is dry-run-only (`gc_sweep.rs` `run_with_release`). It fires on a claude row with no positive death evidence, such as a terminal roster state or a dead pid. So the rehearsal declines to promise a stop it cannot prove, and a real run can still take the row.

The guard exists because of one incident. On 2026-09-08 a dry run promised 9 retirements, and the real run retired 0 (`gc_sweep.rs` `needs_live_stop`).

The rehearsal has a second honest hold: `held {id} (dry-run did not evaluate: {gate}; apply may still refuse)`. A dry-run row whose remaining gate needs a mutation is named where it stands. It is never planted into `retired` or `pruned`, so neither count can read a promise the run did not evaluate (`gc_sweep.rs` `run_with_release`). The apply run can still refuse that gate: the dry-run report says `apply may still refuse`, naming the uncertainty. One row shape skips the hold. It is a claude row with no pane and no mux ref, absent from the claude roster union, with no live squad member. For that row the live surface removal reads already-absent and the mux retirement reads not-applicable, and neither acts. The dry run reads the same two sources without acting (`gc_native.rs` `surface_provably_clear`), so it lists the row under `retired`, as the live sweep will.

If the dry run prints a `held` line, run the real command and read its verdict.

## The rule: retire by default

Removal is reversible. A session never ends, the receipt holds the resume command, and `fno agents resume` restores the row. So the sweep retires a row unless a positive fact keeps it (`gc.rs` `gc_decide`). An unknown fact never keeps a row: `KeepReason` has no variant for one.

The sweep reads the transcript at both ends (`reap_evidence.rs`):

- The head names the skill. A `/fno:` call, a `$fno:` call or a `Skill` tool call marks a skill session (`session_backfill.rs` `bg_fno_invocation_in`). `using-fno` and `fno-me` never count: one is injected context and the other is a mesh join.
- The tail names the end. A `<promise>` with no newer user turn after it marks the work done.

Each row then has one idle window (`gc.rs` `idle_window`). The clock is the transcript's quiet age, else the report clock (last report, else creation):

| Row | Window |
|---|---|
| tail ends in a promise, or a finished eval attempt | the retire grace, 900 s by default |
| planner (blueprint or think) | 1200 s (law d-aecc0fdc) |
| origin `spawn` | 1 hour (law d-e3defbb7) |
| every other skill session, and a row with no transcript | 1 day (law d-defa43c7) |

A row whose every node is done waits at most 1800 s, whatever its origin (law d-b01ffede, `gc.rs` `DONE_NODE_IDLE_RETIRE_SECS`). A shorter window above still wins. An `idea` or `deferred` node the session is also named on is not active work, so it never blocks this window (`graph_store.rs` `work_state`).

A row with no transcript wrote nothing a brainstorm keeps. The sweep reads it as a failed start, and it ages on the report clock.

Measured on 2026-11-04 on the laptop: 63 stopped rows. 31 had no transcript jsonl, 4 ended in a promise, and 28 had no promise.

## Why a row stays

A kept row names one of these reasons. Each is a positive fact.

### teamed, keep mark

`kept {id} (teamed)`: the row holds a lead role or carries an orchestrator team.

`kept {id} (keep mark: ...)`: the user set a keep mark with `fno agents registry reap --keep <row>`. One command lifts it: `fno agents registry reap --unkeep <row>`. `fno agents registry reap --kept` lists every mark and flags one with no registry row. The mark is keyed by harness session id. It outlives the row it was set on and follows a resumed fork through its recorded lineage. It holds the registry sweep, the roster sweep and the merge reaper. The mark is the mux pin. Press `.` or `ctrl+t` on a sideline row in the row selector, or right-click or long-press the row and pick ★ Pin. `fno agents session rm` still removes a kept row.

### brainstorm

`kept {id} (brainstorm: no fno skill in the transcript)`. The transcript holds no fno skill call, so the session is a brainstorm between the user and an agent. Law d-defa43c7 keeps it whatever its age. A spawn row is never a brainstorm: fno started it.

### live claim

`kept {id} (live claim held: {detail})`, where detail is `{claim key} (holder {holder})`. A live or suspect work-claim names this session as its holder, and the holder process answered the pid probe. A claim on a done node does not count. The remedy for a stuck claim is `fno agents claim release`, never a reap.

### open pr

`kept {id} (open pr: {node} #{pr})`. The session drives a PR that is still open. It has an `execute` row on the node, the node carries `pr_number`, and `merge_status` is not `merged`. Once the row is quiet past the grace, the sweep asks GitHub once per pass, cached per PR (`gc_sweep.rs` `open_pr_verdict`). An open PR holds. A merged or closed PR releases the row. An unreadable answer keeps the row too, because the recorded PR is the positive fact. A done node whose merge outcome nothing records reads GitHub once for the row's own PR. A live newer peer that holds a `do` row on the node drives the PR instead, and this row's keep lifts.

The remedy is merge, not reap. Run `fno do pr status <N>` to see what blocks it.

### the nudge ladder

A kept open-PR row is a session that is not driving. The daemon's retire arm runs a nudge ladder over these rows (`pr_nudge.rs` `run_ladder`). The rules, in order:

1. **Reset.** Transcript activity newer than the last nudge clears the budget: the session answered.
2. **Red head.** A settled red at a new head leaves exactly one nudge in the budget. The same head never re-arms it.
3. **Wait.** The transcript is inside the grace window, or the last nudge is too young.
4. **Pause.** A live merge order holds the session. The only allowed pause. A lead records it with `fno inbox decide "merge-order:<held-node>:after:<lead-node>" "<lead-node> merges first"`. The ladder waits while the lead node is not done.
5. **Escalate.** After 3 nudges with no activity, one operator question is filed on the marker `pr-nudge:`. The ladder then waits for activity.
6. **Mail.** A live session gets mail only for an open PR with readable, settled status. Red gives the worker a fix action. Green gives it a merge action. When the claude roster reads the session as `working`, a durable receipt stays queued and the next turn delivers it.
7. **Resume.** A session without a live process resumes only for an open PR with readable, settled red or green status. The command is `fno agents session resume <full-session-id> --message "<text>"`.

Events: `pr_nudge_sent`, `pr_nudge_escalated`, `pr_nudge_paused`. State is one file per session under `~/.fno/agents/pr-nudge/`. `fno agents registry reap --dry-run` prints the plan as `would nudge {id} ({action})` and takes no effect.

### active

`kept {id} (active: transcript written {age}s ago)`. The clock is inside the row's idle window (the table above). Wait past the window.

### no clock

`kept {id} (no clock: neither transcript nor report age)`. Neither the transcript nor the row's report or creation stamp gives an age, so nothing can age the row.

### attended

`kept {id} (attended: typed user turn {age}s ago)`. The transcript holds a typed user turn inside the 15-minute window (`attended.rs` `WINDOW_SECS`). The classifier is the one `fno-agents intel` folds with, so keepalive pings, mail envelopes, and command machinery never count. Wait for the typing to stop. The row retires on a later pass.

### live descendant

`kept {id} (live descendant: {child})`. A live CHILD registry row names this row as its parent (`spawn_edge.rs` `live_child_of`). A CHILD is a join worker or a row a titled session spawned. A handoff, such as a blueprint's target or an advance dispatch, is a PEER and never holds its spawner. A parent whose own harness reports `done`, `stopped` or `failed` is not held.

### open do row

The graph re-check at stage and commit still exists for the merge reaper, which calls the same staging (`gc_sweep.rs` `stage_session_retirement`). The registry sweep passes its rows as released: a stale open do row no longer holds a row the policy retired. The settle pass still fills stale open do rows on done nodes before the row pass reads the graph (`gc_sweep.rs` `settle_stale_do_rows`).

## The row is waiting on evidence

### probe unread

The line reads `kept {id} (probe unread: {detail})`. The fresh re-read of the row's transcript age did not answer inside its bound, so the row is neither known quiet nor known active. An unread instrument is never reported as `active`, and the row never carries an invented age of 0.

The detail names the in-process witness that answered instead. The registry row's `inside_leg.seq` is the quiet witness: the daemon writes it in process on every turn report, so no subprocess can time it out. `no inside-leg report on the row` means the row carries no report at all, so the witness cannot speak. `inside-leg seq moved {old} -> {new}` means a new turn report landed between classification and the re-read: the session woke up, and the keep is real. `a live working report is on the row` means the last report reads `working` and its ttl agrees, a turn plausibly in flight, never quiet, until the badge ages out. When the seq is unchanged and the classification age measured quiet, the row retires, the witness answers where the probe starved (`gc_sweep.rs` `run_with_release`).

The cure is patience, not a ruling: the next sweep's probe usually answers, and the row retires then.

An unread probe is an unknown, and an unknown never keeps a row. When no pid, pane, live working report or seq move lifts the hold, the transcript file on disk decides (`gc_sweep.rs` `transcript_file_age`). A file written inside the grace holds, and the detail reads `the transcript on disk was written {n}s ago`. A quiet file retires the row. When the classify pass read no transcript either, a missing file retires it too: no witness is left that can ever lift the hold. The dry run makes the same read-only re-read, so it lists the same probe-unread rows the live sweep keeps.

### graph unreadable

The line reads `kept {id} (graph unreadable: never a retirement on a failed read)`. The graph read failed this pass (`gc_sweep.rs` `run_with_release`). Rerun the dry run. When the graph reads, the reason clears.

### no resumable receipt

The line reads `kept {id} (no resumable receipt: {reason})`. Every removal needs its recovery record on disk before any effect fires (`gc_sweep.rs` `commit_retirements`). The receipt failed to stage or persist, so the row stays. The line names the reason. Rerun the sweep.

## The row retired and its tree stayed

Tree lines start with `kept tree`. They are decided only after the row verdict is retire (`gc.rs` `tree_action`). A `kept tree` line never means the row survived. The row is gone, and only its checkout remains.

- `kept tree {id} (dirty: {path})`: uncommitted or untracked content waits for a human (`gc.rs` `tree_action`).
- `kept tree {id} (clean but the branch never merged: {path})`: real but unmerged work waits for a human judgment (`gc.rs` `tree_action`).
- `kept tree {id} (the cleanliness probe could not answer: {path})`: unknown never removes (`gc.rs` `tree_action`).
- `kept tree {id} (shared with {holder}, still live)`: another live row occupies the path (`gc_sweep.rs` `run_with_release`).

A clean and merged tree prunes, and the branch stays (`gc.rs` `tree_action`). The removal contract for worktrees lives in [worktree-mechanics.md](architecture/worktree-mechanics.md).

A `kept tree` keeps its source. When no session holds it, its build output is reclaimed after 6h of quiet (`cargo_build_dirs.rs` `reclaim_idle_trees`).

## The sweep tried and did not finish

Three lines mean the sweep acted on a retire decision and hit a refusal.

- `kept {id} (stop refused: {reason})`: the confirmed stop refused. The row stays, and the next pass retries it (`gc_sweep.rs` `run_with_release`). On a claude row with no death evidence, the reason names the missing evidence. After `claude stop` exits 0, the arm polls two witnesses for up to 15 s (`gc_claude_stop.rs` `stop_claude_confirmed`). The witnesses are the daemon roster and the `claude agents` state. A supervisor that tears the session down late no longer wedges the row for a whole tick. A pane row on a harness with no session reader (opencode, pi, agy) answers from its transcript age (`pane_stop.rs` `quiet_session_holder`). Quiet past 1200 s, with the row's process already proven dead, the stop goes through. Inside that window the row stays.
- `prune failed {id} ({reason})`: the tree removal did not confirm (`gc_sweep.rs` `RetireRefusal`).
- `settle refused {node} ({reason})`: the settle write refused, named and never silent (`gc_sweep.rs` `settle_stale_do_rows_with`).

Two receipt lines ride the same retention pass. `expired receipt {name}` names a receipt the retention window retired. `kept receipt {name} ({reason})` names a failed read, and a failed read is not evidence of age (`gc_sweep.rs` `commit_retirements`).

## Edge cases that cost a session real time

Each item here cost a real session time. One corrects an older belief.

1. **Open work no longer keeps a row.** A quiet row past its window retires whatever its node reads. The node keeps its record, and `fno agents resume` brings the session back. Deleting or hand-closing the node is still falsifying work.
2. **A second claude account holds rows the ambient account cannot see.** `no job matching <id>` from one root is a wrong-root absence.
3. **`claude rm` takes the short id.** The full session id answers `No job matching` and exits 0. The exit code reads as success.
4. **`claude rm` does not read the session cwd.** Measured 2026-10-25: 116 dirty-cwd sessions still left the list. One scratch session's cwd held a modified tracked file. `claude rm` removed that session with exit 0 and left the list. The refusal `claude rm --help` documents lives in its claude-managed worktree flags. Those flags (`--discard-unpushed`, `--force-remove-worktree`) name uncommitted changes to the worktree's own tracked files. A plain checkout cwd, dirty or clean, is not consulted.
5. **A keeper pane reads dead to a fresh server.** Its child lives. Death evidence decides the stop (`gc_sweep.rs` `claude_death_reason`).
6. **A daemon older than the installed binary reports every status as `unknown`.** Unknown keeps rows. Run `fno agents session list` and read its warning. Measured on 2026-09-10: one restart turned 57 unknown rows into 26 orphaned, 16 writing, 12 quiet, and 2 parked.
7. **The mux tab prune is scheduled, not manual-only.** An older belief held that nothing scheduled it. The manual command adds `--include-used-shells` (`client.rs` `run_reap`). The daemon retire arm uses default flags (`gc.rs` `maybe_retirement_sweep`). `--no-mux` skips it on the manual command.
8. **A row refused on every pass stays, and the report names the refusal each time.** Older builds discarded the reason. Today the render carries it (`reap_render.rs` `render_reap`).
9. **A retired row stays retired.** On 2026-10-22 all 50 eval rows in the registry had a reap receipt. A reaped worker's own Stop hook reported after the sweep, and the session report minted its row again. Now a reap receipt, or an rm tombstone, refuses that mint (`native_adopt.rs` `retired_session`). A tombstone lasts as long as a receipt is kept by default: 7 days. A refused claude session also gets `claude rm`, so it stops reporting and leaves Claude's list (`session_report.rs` `remove_retired_claude_session`). Claude sessions that never had an fno row are left alone unless a claimed launch record names them. Those the roster sweep takes. A launch record newer than the retirement is a relaunch and mints. A session you resume by hand still gets your own operator row. To bring a reaped session back on purpose, run `fno agents adopt <session-id>`.
10. **The retire tick names its build.** A daemon that outlives a binary install keeps sweeping with the old code. On 2026-10-22 a manual `reap --done --apply` retired 83 rows the older daemon kept. Each `retire` tick detail ends in `build=<rev>`. Compare it with the installed binary (`fno doctor`), and restart the daemon on a mismatch.

## Every JSON key

Every top-level key of `fno agents registry reap --json`, one row each. The dry run renders the same object with `dry_run` true. `render_reap` emits the summary keys (`reap_render.rs` `render_reap`). `render_reap_with_inventory` splices two more, `inventory` and `mux` (`reap_render.rs` `render_reap_with_inventory`). One JSON read carries the verdicts and the world they were judged against.

| Key | Report line it feeds | Explained at |
|---|---|---|
| `retired` | `retired {id} ({basis})` and the count line | [In what order](#in-what-order) |
| `pruned` | `pruned {id} (clean and merged: {path})` | [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) |
| `prune_failed` | `prune failed {id} ({reason})` | [The sweep tried and did not finish](#the-sweep-tried-and-did-not-finish) |
| `settled_do_rows` | `settled {id} (stale open do row filled on done node: {node})` | [open do row](#open-do-row) |
| `settle_refused` | `settle refused {node} ({reason})` | [open do row](#open-do-row) |
| `kept_promoted` | `kept {id} (promoted)` | [teamed, keep mark](#teamed-keep-mark) |
| `kept_open_do_row` | `kept {id} (open do row: {node}: {detail})` | [open do row](#open-do-row) |
| `kept_open_pr` | `kept {id} (open pr: {node} {detail})` | [open pr](#open-pr) |
| `kept_active` | `kept {id} (active: transcript written {age}s ago)` | [active](#active) |
| `kept_attended` | `kept {id} (attended: typed user turn {age}s ago)` | [attended](#attended) |
| `kept_other` | `kept {id} ({reason})`: `brainstorm` and `no clock` land here | [brainstorm](#brainstorm) |
| `kept_marked` | `kept {id} (keep mark: set {time} by {who})` | [teamed, keep mark](#teamed-keep-mark) |
| `kept_live_claim` | `kept {id} (live claim held: {detail})` | [live claim](#live-claim) |
| `kept_probe_unread` | `kept {id} (probe unread: {detail})` | [probe unread](#probe-unread) |
| `kept_graph_unreadable` | `kept {id} (graph unreadable: never a retirement on a failed read)` | [graph unreadable](#graph-unreadable) |
| `kept_dirty` | `kept tree {id} (dirty: {path})` | [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) |
| `kept_unmerged` | `kept tree {id} (clean but the branch never merged: {path})` | [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) |
| `kept_unprobed` | `kept tree {id} (the cleanliness probe could not answer: {path})` | [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) |
| `kept_shared_tree` | `kept tree {id} (shared with {holder}, still live)` | [The row retired and its tree stayed](#the-row-retired-and-its-tree-stayed) |
| `kept_live_descendants` | `kept {id} (live descendant: {child})` | [live descendant](#live-descendant) |
| `stop_refused` | `kept {id} (stop refused: {reason})` | [The sweep tried and did not finish](#the-sweep-tried-and-did-not-finish) |
| `needs_live_stop` | `held {id} (needs live stop: {reason})` | [A dry run and a real run answer different questions](#a-dry-run-and-a-real-run-answer-different-questions) |
| `dry_run_unverified` | `held {id} (dry-run did not evaluate: {gate}; apply may still refuse)` | [A dry run and a real run answer different questions](#a-dry-run-and-a-real-run-answer-different-questions) |
| `kept_no_receipt` | `kept {id} (no resumable receipt: {reason})` | [The sweep tried and did not finish](#the-sweep-tried-and-did-not-finish) |
| `expired_receipts` | `expired receipt {name}` | [The sweep tried and did not finish](#the-sweep-tried-and-did-not-finish) |
| `kept_receipts` | `kept receipt {name} ({reason})` | [The sweep tried and did not finish](#the-sweep-tried-and-did-not-finish) |
| `holds` | projection, no line: one entry per held handle; feeds the `retire_holds` journal row | [Read the answer](#read-the-answer) |
| `hold_escalate_after_s` | projection, no line: the threshold behind the escalation suffix `; past ...: fno agents registry reap --release` | [Why a row stays](#why-a-row-stays) |
| `release_refused` | the refusal lines a `--release` pass prints verbatim | [teamed, keep mark](#teamed-keep-mark) |
| `open_pr_rows` | projection, no line: one entry per kept open-PR row; the nudge ladder reads it | [the nudge ladder](#the-nudge-ladder) |
| `open_pr_nudge` | `would nudge {id} ({action})` | [the nudge ladder](#the-nudge-ladder) |
| `roles` | `vacated role {scope} (holder {session} dead: {evidence}; inherits: {inheritor})` and `kept role {scope} ({reason})`; `null` when no role sweep ran | [Manifest-only roles and the dead-role reaper](architecture/lead.md#manifest-only-roles-and-the-dead-role-reaper) |
| `schema_skew` | `registry schema v{on_disk} is ahead of the v{understood} this fno understands: ...` | [Read the answer](#read-the-answer) |
| `dry_run` | the `(dry-run: no changes made)` marker | [A dry run and a real run answer different questions](#a-dry-run-and-a-real-run-answer-different-questions) |
| `inventory` | dry run, text: `session inventory: enumerated N session(s), M never judged (no registry row)` plus one line per incomplete source and a partial-roots line; JSON: the census object, present only when `--dry-run` runs, and a live `--json` object omits it (`client.rs` `run_reap`) | [Read the answer](#read-the-answer) |
| `mux` | `mux sweep (ran|unread|skipped by --no-mux)` | [Ten programs stop or remove a session](#ten-programs-stop-or-remove-a-session) |

## One table: the reason, the act, the check

Every keep and hold reason from the sections above, one row each.

| Report line | Act or wait | The check that confirms it |
|---|---|---|
| `kept {id} (promoted)` | Wait. This one is permanent. | The line itself names the reason. |
| `kept {id} (brainstorm: no fno skill in the transcript)` | Wait. Law d-defa43c7 keeps a brainstorm; `fno agents session rm` removes it by hand. | The transcript holds no `/fno:` or `$fno:` call. |
| `kept {id} (no clock: neither transcript nor report age)` | Restore a parseable `created_at` or a report on the row. | Run `fno agents session list`. |
| `kept {id} (keep mark: set {time} by {who})` | When the session is no longer wanted: `fno agents registry reap --unkeep <row>`. `--kept` lists every mark. | The dry run line itself. |
| `kept {id} (open pr: {node} {detail})` | Drive the PR to merge, or record a merge order. | Run `fno do pr status <N>`. |
| `kept {id} (open do row: {node}: {detail})` | Wait. A real run settles it. Never close the node. | Run `fno backlog get <node>` and read `status`. |
| `kept {id} (live descendant: {child})` | Wait for the live CHILD row to go, unless the parent's roster state reads `done`, `stopped` or `failed`. A handoff row (`sob-t-`, `ac-t-`) never holds its spawner. | The same report carries the child line. |
| `kept {id} (active: transcript written {age}s ago)` | Wait past the grace window. Nothing retires the row early inside it. | Run the dry run again. Read the new age. |
| `kept {id} (live claim held: {detail})` | The claim's holder process is live. Release the claim if it is stuck: `fno agents claim release`. | Run `fno agents claim status <key>`, with the key from `{detail}`. |
| `kept {id} (probe unread: {detail})` | Wait for the next sweep. The probe usually answers then. A moved seq means a new turn: the keep is real. | Run the dry run again. Read the detail. |
| `kept {id} (graph unreadable: ...)` | Rerun when the graph reads. | Run the dry run again. |
| `kept {id} (no resumable receipt: {reason})` | Rerun the sweep. | The line names the reason. |
| `held {id} (needs live stop: {reason})` | Run the real command. The row can still retire. | Run `fno agents registry reap`. |
| `held {id} (dry-run did not evaluate: {gate}; apply may still refuse)` | Run the real command. The apply can still refuse the named gate. | Run `fno agents registry reap`. |
| `kept {id} (stop refused: {reason})` | Read the reason. The sweep retries next pass. | Run the dry run again. |
| `kept tree {id} (dirty: {path})` | Judge the tree. The row is already gone. | Run `git status` in `{path}`. |
| `kept tree {id} (clean but the branch never merged: {path})` | Judge the branch. The tree waits. | Run `git log` on the branch. |
| `kept tree {id} (the cleanliness probe could not answer: {path})` | Rerun the sweep. | Run the dry run again. |
| `kept tree {id} (shared with {holder}, still live)` | Wait for the holder row to go. | The holder appears in the same report. |
| `prune failed {id} ({reason})` | Read the reason. | Run the sweep again. |
| `settle refused {node} ({reason})` | Read the reason. | Run the sweep again. |
| `expired receipt {name}` | Nothing. Retention ran. | The line names the receipt. |
| `kept receipt {name} ({reason})` | Nothing. A failed read is not age. | The line names the reason. |
