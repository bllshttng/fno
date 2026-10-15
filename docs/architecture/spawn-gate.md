# The spawn gate: one gate, one answer

There is ONE spawn gate: `crates/fno-agents/src/spawn_gate.rs`, with the lane axes in `spawn_gate_lanes.rs`. Every door asks the same gate: pane, routed, account, and the native bg/headless arms. Python's `cli/src/fno/agents/spawn_gate.py` is a TRANSPORT over the `fno-agents spawn-gate` verb. It carries the caller's identity in. It carries the refusal out, as data. This doc carries the long stories from the docstrings. The code keeps its prose short. The mux's own revival door asks through `crates/fno/src/server/revival_gate.rs`: the resume gesture and the held-pane focus ask in `gate` mode with `hold: false` (the verb releases the mutex before it answers), and the workspace restore asks once in `probe` mode and spends the headroom it grants.

## The verb contract

`fno-agents spawn-gate` reads one JSON payload on stdin. It writes one JSON answer on stdout. The verb exits 0 whenever it produced an ANSWER, including a refusal. A refusal is data. The caller decides what to do with it. The gate's own prose (`spawn queued: ...` during a 240-second queue) streams on stderr.

Two modes:

- `gate` runs the full admission gate. Its payload is `{mode, name, substrate, force, no_wait, route_provider, account, caller_session, succession_scope, holder_pid, seed, session_phase}`. An admitted answer is `{status: "admitted", gate_key, gate_holder, worker_key, worker_holder}`. When a claim is not held, its key is null. A refusal is `{status: "refused", exit_code, receipt, event}`. When the seed first verb or explicit `session_phase` label names review, the gate refuses. Seed verbs use `spawn_phase.toml`.
- `probe` is the read-only capacity reading. `fno agents gate-status`, the lane readouts, `explain` and the advance width all consume it. The payload is `{mode: "probe", caller_session, only}`. Set `only: ["lanes"]` to skip the CPU and RAM reads. This is for callers already on the spawn path. The answer keeps the probe's verdict shape. It adds three blocks every reader consumes. `lanes` covers every capped provider and every provider a live row names, each `{cap, live, counted}`. `share` is `{leads, share, held, held_rows, unattributed}`. `rows` holds the explain Gate dicts in `{name, measured, threshold, verdict, key, note}` shape.

## Claims cross the boundary owned by the caller

Every claim the verb takes in gate mode is stamped with the PYTHON caller's pid. The holder reads `spawn-gate:<holder_pid>:<name>`. The native claim verdict therefore judges the real holder. It never judges the verb process, which has already exited by the time dispatch returns. The verb hands the held keys back in the admitted answer. The Python `GateGuard` releases them with the ordinary release path.

## Leads take no worker slot

The slot count is the live worker rows plus the live `worker:<name>` reservations. A lead row takes no worker slot. A lead is a row with a role (`role_level`), the same test the lead-share axis counts teams by. On 2026-10-14, seven leads held seven of the 24 slots on one machine, so each new lead shrank the worker pool. The probe answer carries `leads`, and its `cap_clause` ends `; 7 leads take no slot`. The lead check-in capacity line shows that clause.

## Role succession can sit one row over the cap

A successor holds a worker slot until its role lands. Role settlement can confirm that the caller will vacate its sole live row for `succession_scope`. Then the gate lets the successor sit one row over the worker cap. The gate reads the caller's own lead row from the full live rows, because a lead row is not in the worker count. A second successor at the cap plus one still refuses.

## The learned cap yields to the machine gates

The learned cap is a lagging guess. The CPU and RAM gates read the machine now. The slot count can reach the learned cap while the CPU axis admits and the RAM floor reads room. Then the learned cap yields, and the user's `agents.max_live` binds. An unread RAM figure is no room. The admitted receipt reads `slots: "5/8 ok (learned cap 3 yields: CPU and RAM have room)"`. At `agents.max_live` the gate still holds, and the refusal names `agents.max_live` and `--force`. The probe applies the same rule after its RAM and CPU rows. Its `cap_clause` then ends `; the learned cap yields: CPU and RAM have room`.

On 2026-10-14 the gate refused at a load of 8 on 16 cores with 90 percent of RAM free. The learned cap held, and nothing else did.

## A waiting spawn names its place

A spawn that waits holds one `gate-wait:<holder>` ticket in the gate's global claims root. Its place is one plus the live tickets that joined before it. On its first wait, and every 30 seconds after, it prints `spawn queued: place 2 of 3 waiting; waiting on the slot cap, 8 live of 8; waited 30s`. The reason is the slot cap, the fleet CPU hold, or the gate mutex. On 2026-10-14, 5 of 54 attempts launched in four hours, and no wait named its place or its reason. A spawn behind the gate mutex printed nothing at all.

The queue gives up after 240 seconds with exit 75. It used to wait 600 seconds. The first-turn watch tells a caller where its spawn is stuck by 5 minutes after the call, so the gate answers first ([spawn-watch](spawn-watch.md)).

A `--no-wait` pass takes no ticket. Its refusal receipt carries `queue_place`, `queue_size` and `waiting_on`, where the place is the back of the queue. A queue timeout carries them too. A refused native launch line names the place: `spawn gate refused the native launch (no_wait, place 3 of 3 waiting on max_live): ...`. The ticket leaves the queue as the gate returns.

## The exit-code allocation table

One table is shared by both trees. It lives in `cli/src/fno/agents/spawn_gate.py` and is mirrored in `spawn_gate.rs`. `cli/tests/unit/test_exit_code_allocation.py` keeps it unique. Values >= 64 claim a number once. The same NAME at the same number in both trees is byte-parity.

- 75 queue timeout
- 76 no-wait
- 77 RAM floor
- 78 provider cap. The quota lock and the lane faults keep this number, so exit-code consumers are unaffected. The receipt's `reason` discriminates `provider_cap`, `provider_quota_lock`, `gate_mutex_unavailable`, and `lane_reservation_unavailable`.
- 79 load
- 80 lead share
- 81 registry schema
- 82 and 83, the fleet incident pair, byte-parity
- 82 also carries the machine admission door (`process_admission_gate`). It holds an agent-origin spawn on an unexpired runaway brake. A caller with no `FNO_AGENT_SELF` admits. The default is admit. The census and the process ceiling live in the fno crate's own agent-spawn door, which the `fno` front runs before any forwarding.
- 84 state root ungranted. Permanent until a human grants.
- 85 the Python sandbox probe
- 86 the per-territory team cap. Exit 86 is the permanent, non-queueable territory-cap refusal. Unreadable territory attribution uses the same exit. Callers do not retry it as capacity.
- 87 gate unavailable. The gate verb is missing, failed, or timed out. Fail closed: never admit on an unreadable gate.
- 88 blueprint thread cap. More than `agents.profiles.blueprint.max_live` live `bp` threads, or more than one per territory, refuses the spawn and teaches the subagent path.
- 89 review session. A seed or label that names review causes a permanent refusal. The refusal runs before `--force` and the `FNO_SPAWN_GATE=0` bypass. Run the inline fno review lane.

## Overriding the learned cap

`agents.max_live` is the ceiling. Under it, the machine watch learns a working cap per machine. A first run starts the cap at the ceiling. A long run queue cuts the cap by 25 percent. Calm adds one slot per 900 seconds. The learned cap lives in `<agents home>/capacity/<machine-id>.json`. Every cap display reads it through `capacity::effective`.

Only a measured cut binds below the ceiling. An older first run stamped a cap it guessed from the machine budget (`seeded from machine budget`), and a cap can sit at an older and lower ceiling. The reader treats both as no measurement, so `agents.max_live = 32` with no floor reads an effective cap of 32 at once, not 24. The next tick writes the same answer to the file.

`[agents.capacity] floor` is the lowest cap the loop can learn. The default is 1. Set it with `fno agents spawn --floor N`. Alone, the line writes the user config and prints the old and new values. The user config is `~/.fno/config.toml`. A set `$FNO_CONFIG` replaces it. With a name and a payload after it, the line sets the floor and then spawns. It works in any chat, because it reads no caller key. The gate reads the project `.fno/config.toml` and the canonical checkout's copy before the user config. If either sets the floor, the line refuses and names that file. `fno config set` still refuses this key. An agent's own Bash tool call cannot set it: the PreToolUse hook refuses `--floor` on an `agents spawn` line. The user types the same line in a shell, where no hook runs. The loop never cuts below the floor. The reader raises a lower stored cap to the floor at once, with no wait for the next tick. A floor above `agents.max_live` clamps to the ceiling. The cap line then reads `effective cap 8 of 24 (held at agents.capacity.floor 8, learned 5)`. Once a tick or a cut has stored the floor as the learned cap, the line reads `effective cap 8 of 24 (at agents.capacity.floor 8)`. The floor stays named.

The gate reads the cap and the floor on every queue pass. A floor raised while a spawn waits releases that spawn on its next pass.

Every refusal on a soft axis prints a hint line. The line names the learned cap, the user's `agents.max_live`, the one-line floor setter, and `--force` for one spawn the user ordered. A native launch line has no `--force`, so its refusal names `fno agents spawn NAME PAYLOAD --force` instead. The slot receipt keeps `max_live`, `axis`, `held_on` and `reason`. It adds `learned_cap`, `ceiling` and a `remedy`. Below the ceiling the remedy names the one-line floor setter. At the ceiling the cap is the user's own `agents.max_live`, so the remedy and the hint name that key and `--force`. The memory refusal says the opposite: the floor does not lift it. The probe answer carries `cap_clause` and `floor`. The lead check-in capacity line ends with ` | <cap_clause>`.

`--force` is the user's word for one spawn. It passes every soft limit: `agents.max_live`, the learned cap, the provider lane cap and the fleet CPU hold. It never passes the memory axis. A forced spawn still meets the RAM floor and the swap cap, because a spawn that ignores them slows every session on the machine. The refusal names `agents.min_free_gb` and `agents.max_swap_pct` as the keys to change. The incident brake and the process admission door sit ahead of `--force` and hold too. A forced spawn prints one line. The line names each soft limit it passed, then the cap the loop reads, then says the RAM floor still applies. The RAM readings line prints just before it.

## Reading a refusal

The last `spawn-gate: refused on <axis> (<reason>, exit <code>): <figures>` line on stderr is the verdict. It is the WHOLE refusal: one line naming the reason and what to do, the exit code carrying the class. When the receipt carries a `remedy`, the verdict ends `; remedy: <what to do>`. The figures are the receipt's scalar fields. With `--json` the caller also gets the receipt as one JSON object. Every other gate line starts `spawn-gate note:` and passed. The Python transport's pass lines carry that prefix too. If the gate verb itself is unreachable, the transport prints the same verdict shape for exit 87 before refusing. The Python reader `cli/src/fno/backlog/advance.py` (`_gate_refusal_detail`) keys on the `spawn-gate: ` prefix and reads the last such line, so it gets the verdict, never a passing reading.

A stderr with no `spawn-gate:` line means the gate admitted and something after it failed. The real error is the first non-note line there, never a `spawn-gate note:` line.

## Refusal events: Python via the seam, routing from Rust

Some spawns ENTER Python: pane, routed, and account. For them the verb returns the event fields, and the Python transport emits `spawn_gate_refused` through `_refuse`. The journal vocabulary is unchanged. The native unrouted arms now emit too. The route-slot verb appends one `spawn_gate_refused` row, `gate: "routing"`, at the verb entry, so a refused native spawn leaves a trace beside the Python rows. The Rust side has no config-resolved state-dir parity yet, so the row lands in the space journal rather than the global one. The verb read is the answer: `fno doctor event find spawn_gate_refused --field gate=routing` scans every journal and rotation. The emit and its guard test live in `crates/fno-agents/src/route_slot.rs` (`journal_routing_refusal` and the `journal_rows` tests). See `scripts/ci/check-gate-refusals-emit.sh` for the Python seam guard.

## The registry schema check refuses writes it cannot understand

`check_registry_schema` refuses a spawn into a fleet whose shared registry this binary cannot write. A node claim and a mail stamp are both WRITES. A registry ahead of this fno therefore blocks the whole spawn path. On 2026-08-28 a worker reported "Claim store is not writable for this Codex session". The worker blamed a sandbox permission profile, and the report was believed. Nothing in that chain named the registry.

So the check refuses and does not warn. The message carries both integers, the file, and the repair verb. On the edges it matches the other guards. An unreadable file skips the check, because a spawn is not the place to adjudicate a torn registry. A refusal there can make the repair verb itself unspawnable. Both trees read the version from `src/registry_schema.toml`. build.rs projects the same file into the wheel. The two sides of the check therefore cannot disagree about a number.
