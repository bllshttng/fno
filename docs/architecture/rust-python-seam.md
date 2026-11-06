# The Rust/Python seam

Where the boundary between the Rust runtime and the Python CLI belongs, what a crossing is, and how to score moving one. The companion page [dual-implementation-inventory.md](dual-implementation-inventory.md) holds the dual set and the port protocol. This page holds the boundary rule and the budget a port must satisfy.

**Status: the Python CLI is deleted.** `cli/src/fno` and the wheel's Python package are gone, and `fno` on PATH is the Rust front door. The process-only ruling and the two-axis budget still bind every crossing between the `fno` front door and `fno-agents`. A row below that names a Python owner records where a decision lived before the deletion. Its port is done, and it is not a live crossing.

## The FFI ruling

**Locked Decision. The seam stays process-only. No in-process channel comes on the table.**

The measured evidence: the tree contains zero FFI. No `pyo3`, no `maturin`, no `cpython`, no `cbindgen` appears in either crate manifest. Every crossing in both directions is a process boundary today, and this ruling keeps it that way.

The reasoning. A `maturin` build step couples the Python wheel to a compiled artifact on every platform. The install contract of a pure-Python package becomes a build matrix. The repo already ships one generated cross-language artifact, `harness_capabilities.toml`, and its freshness needs its own CI tripwire. Adding an ABI to that surface buys fine-grained ownership at the price of a per-platform build. The seam's traffic does not pay for it: the crossing sites are low-frequency command shells and bounded reads, not hot in-process calls.

The socket of the store keeper does not weaken this ruling. It is a process boundary. A separate `fno-agents-worker` process serves one graph file over a local AF_UNIX socket. This is the pane-keeper model applied to the graph store. No library link, no shared address space, and no ABI exist between the sides. The socket is the one transport that does not spawn a command. The dependency direction requires it: `fno-agents` depends on `fno`, never the reverse. A consumer that lives in `fno` cannot link the store, so it must speak the socket protocol. That dependency is a dev-dependency only. Production code in `fno-agents` reaches the mux through the `fno` binary, never by linking the crate. That is why the attach command's portal reach is a crossing and not a call.

The consequence, and it is arithmetic, not preference. A subprocess has no cheap call, so a port must carry a decision's whole fact set or it adds a spawn. Moving half a decision across a process boundary trades one crossing for another and splits the fact set in two. Every later port proposal that splits a decision from its facts is refused by this paragraph, without re-arguing the build matrix.

## The measurement

Reproduce every number below on a clean checkout with `crates/fno/target/debug/fno doctor lint seam-crossings` (build the front door first). It prints, for each baselined set, the measured count beside the baseline count.

Rust reaches the `fno` porcelain through **133 baselined crossing sites in both crates**. The lint also ratchets **25 resolver functions**, every function whose body resolves the porcelain path. The Python direction is gone: no production Python execs the binaries.

The counting rule, in words, so a reader can audit it without reading the lint. A crossing site is a production Rust line that launches `Command::new("fno")` or calls a baselined resolver helper name. A resolver function is a production Rust function with a line reading the `FNO_BIN` or `FNO_LOOPCHECK_FNO_BIN` env key, or constructing the porcelain path with `join("fno")`. Inline test modules, `crates/*/tests/`, and comment lines are out of scope. The baseline keys on `(rule, path, line content)` as a multiset, never on the line number. A moved line does not churn it. A removed site still fails.

A resolver is detected by shape, never by name. Three helpers carry obvious spellings: `fno_bin` in `crates/fno/src/server.rs:2247`, `fno_bin` in `crates/fno-agents/src/scrape.rs:174`, and `loopcheck_fno_bin` in `crates/fno-agents/src/loopcheck.rs:3091`. Twenty-three more functions resolve the porcelain inline, among them `fno_cmd` in `loop_dispatch.rs:136`, `durable_session_pid` in `claims.rs:2670`, and `best_effort_notify` in `loopcheck.rs:3109`. A historical first count that grepped one literal and four helper names reported 29 sites across 10 files. The full scan then found 56 across 18. That 48 percent shortfall shows why the resolver rule matches the env-key shape instead of a name list.

The write path is the asymmetry that matters, and the store port moved where it lives. The durable store `graph.db` has one writer: the store publish pipeline in `crates/fno-agents/src/graph_store.rs`. The pipeline runs inside the keeper process and serializes every publish under the store's bounded lock. It writes the backup file together with the bytes. The `fno backlog` commands are the only mutation surface on the CLI side. They reach the pipeline over the keeper socket. The mux used to shell the porcelain for its reorder commands and read the verdict. Now the native store client (`crates/fno/src/store_client.rs`) speaks the same socket protocol, and the write itself adds no seam crossing. The one crossing a native write pays is the detached `fno backlog render-views` replay: graph.md is still Python-owned, so the write crosses back once for it. The board pages crossed the other way (2026-09-30): the Python HTML renderer is deleted, and the board render is native (`fno board-render` writes the served page with the rows embedded), so the Python replay resolves the configured targets and hands them to the front binary. Reads keep one deliberately duplicated leg. `crates/fno/src/backlog_view.rs` parses the file itself for its read-only snapshots. The direction law forbids linking the store, and a socket round-trip per snapshot read costs more than the duplicate. The seam is no longer duplicated reads on both sides with writes held by Python. It is one store, two socket clients, and one native read mirror.

Two interpreter spawns retired with the plan-doc writer port (2026-09-22). `finalize` used to shell `python3 -m` twice per ship, once to stamp the plan and once to graduate it. The writer's whole fact set is the graph rows plus the doc bytes. So the port moved the codec, the projection, and the stamp commands into `crates/fno-agents/src/plan_doc` behind the keeper's `plan_docs` method. `finalize` stamps in process, the Python commands are clients of the same method, and the crossings are gone rather than rebaselined. The plan-doc sidecar lock stays shared vocabulary with the surviving Python body-append writer. The two still serialize on one path scheme.

## The ownership rule

Rust owns what must not stop: the daemon, the PTY, the loop, liveness, and the claim protocol its own decisions read. Python owns what a user types and what reads the graph: the CLI command surface, the graph read commands, and config resolution.

When the caller does not own the decision the answer feeds, the crossing is legitimate. When the caller owns it, the crossing is not. `finalize` shelling `fno backlog` to record an outcome is legitimate: it writes a Python-owned record through the single writer. The daemon's former `claim list` shell-out was not: the daemon owned the sweep decision, so the claim fact set now lives natively beside it.

## The classification

The table classifies the crossing sites, one line of reason each. The pass that produced it validated the rule. Line numbers drift with every edit. The baseline keys on content, never on them. One conforming row is now retired: the graph reorder crossing. Its command rides the store socket as a native client today. The row stays to say so. Two sites came back violating and five are infrastructure, the resolver definitions and their delegation line. That count is under a third unclassifiable or violating, so the rule stands as law.

| Site | Verdict | Reason |
|---|---|---|
| `bin/client.rs:581` | conforming | process-replacement delegation of the spawn call, whose decision Python owns |
| `bin/client.rs:602` | conforming | process-replacement delegation so account resolution stays one implementation |
| `bin/client.rs:2889` | conforming | session discovery read through its Python owner, fail-open on a miss |
| `bin/client.rs:1346 place_thread_portal_after_spawn` | conforming | post-spawn portal placement through the same mux thread door as `attach.rs:28`; the server owns the thread pane, the fno crate is dev-only here, and a native ThreadPane client would carry a second PROTO_VERSION pin |
| `bin/client.rs:2164 exec_python_front` | conforming | process-replacement delegation of the spawn call to the Python front door, whose decision Python owns; the account arm calls it only when the seam did not stamp the requested account, and the route arm only for a routed one-shot or a route with an account; the keeper-thread arm names its own refusal and never crosses (the keeper rows route to `keeper_spawn.rs`) |
| `pane_spawn::run_pane_spawn` | conforming | the codex pane arm answers its own spawn, carrying the decision's whole fact set (lease, argv, bind wait, seed, row, receipt) so a pane seat pays no crossing; the Python pane leg stays the python-mode twin and the arms answer every explicit-pane shape, so no decision is split across the boundary |
| `bin/client.rs:2433 run_mux_sweep` | conforming | the reap sweep executes through the command that owns the buckets and guards |
| `bin/client.rs:3819 discovered-session read` | conforming | session discovery read through its Python owner, fail-open on a miss |
| `claude_ask.rs:139` | conforming | transcript truth probe, single implementation kept in Python on purpose |
| `claude_ask.rs:545` | conforming | batch spelling of the same probe, one interpreter for N handles |
| `client_commands.rs:1287` | conforming | ask-token mint through the Python resolver, one implementation |
| `client_commands.rs:3087` | conforming | resume delegation by exec, exit code and signals carried by the child |
| `client_commands.rs:3155` | conforming | pane launch through the one front door to the mux server |
| `client_commands.rs:3463` | conforming | recovery relaunch through the same single door |
| `attach.rs:28` | conforming | attach reaches the mux thread portal through the same door; the server owns the thread pane, and the fno crate is a dev-only dependency here |
| `daemon.rs:1468` | conforming | reapable predicate read from its one implementation, shared by three callers |
| `daemon.rs:3486` | conforming | the cleanup sweep executes through the command that owns the buckets and guards |
| `daemon.rs:3530` | conforming | stale-question reconcile routed through the command that owns it, no apply form |
| `daemon.rs:7876` | conforming | pane kill through the only path to the server that owns pane state |
| `daemon.rs:7935` | conforming | pane read probe, absence proved by the pane owner's own vocabulary |
| `spawn_gate_lanes.rs:95` | conforming | pane liveness for the provider count, read through the pane owner's own wait command like `daemon.rs:7935` |
| `daemon.rs:9010` | conforming | codex rollout walk reused from Python rather than reimplemented |
| `finalize.rs:472` | conforming | run summary pushed to the parent through the event registry owner |
| `finalize.rs:1858` | conforming | PR metadata read through the REST wrapper owner |
| `finalize.rs:1914` | conforming | PR stamp written to the graph through the only writer |
| `finalize.rs:2251` | conforming | merge-hold predicate read from the command that owns the policy, fail-closed |
| `finalize.rs:2298` | conforming | stacked-base lineage predicate read from its command, fail-open by design |
| `finalize.rs:2458` | conforming | session record added through the manifest owner |
| `finalize.rs:2695` | conforming | dedup read through the command an operator would run, so state is never re-derived |
| `finalize.rs:2715` | conforming | question filed through the durable channel owner |
| `loop_dispatch.rs:364` | conforming | account pick through the one implementation of billing truth |
| `loopcheck.rs:3934` | infrastructure | the resolver helper itself, the seam's plumbing |
| `loopcheck.rs:10956` | conforming | decision record read through `backlog decisions`, its owner |
| `loopcheck.rs:13614` | conforming | gate reads board and law state through the graph's single writer |
| `nudge.rs:35` | conforming | inbox nudge read through the durable channel owner |
| `provider.rs:544` | violating | a Rust-owned sandbox decision fed by a Python-owned plan-path fact, the split the FFI ruling refuses |
| `reentry.rs:120` | conforming | account binding read from the store, never reimplemented |
| `scratch.rs:1300` | conforming | node birth + hidden-leaf probe through the `fno_bin` resolver's porcelain; the sweep does not own the filing decision, `backlog idea` does |
| `scrape.rs:174` | infrastructure | the resolver helper itself |
| `scrape.rs:276` | conforming | pane title sweep read through the pane owner |
| `spawn_gate.rs:378` | conforming | gate-escape telemetry through the event emit path |
| `backlog_view.rs:69` | conforming | snapshot read through the only writer, schema owned by the source |
| `team_alarm.rs read_org_payload` | retired | the team read runs in process (`team_read::team_nodes_payload_at`); the front-binary spawn for the `--nodes` payload is gone, and the ledger arm's inner crossing retired with it |
| `client.rs:2470` | conforming | update probe through the update policy owner, bounded |
| `client.rs:2649` | conforming | workspace prune through the front door, counts owned by the command; an applied run sends `SquadReload` to every live server so the file and memory agree; an orphaned worker tab (its stored member judged Dead) closes by default, used shells stay opt-in |
| `client.rs:14413` | conforming | config write through the CLI, the same monopoly as the graph |
| `connections_view.rs:1240` | conforming | config and combo reads through the config owner, fail-open |
| `connections_view.rs:1278` | conforming | user-initiated commands dispatched through the CLI surface |
| `needs_overlay.rs:122` | conforming | open questions read through the durable store owner |
| `needs_overlay.rs:144` | conforming | mine list read through the same store |
| `needs_overlay.rs:209` | conforming | mine mutations written through the single writer |
| `needs_overlay.rs:257` | conforming | answer recorded through the same writer, errors surfaced |
| `notify_dispatch.rs mux_clients` (daemon copy) | conforming | the notice crosses to the mux server that owns attached clients, as the narrow `fno mux notify` one-shot; the decision table itself runs in-process beside its owner, and the two operator-notice spawn sites plus one drained loopcheck crossing retired with the port |
| `server.rs:2843` | infrastructure | the resolver helper itself |
| `server.rs:2884` | conforming | config read bounded through the config owner |
| `server.rs:2954` | conforming | spawn dispatched through the surface that owns provider resolution |
| `server.rs:3083` | conforming | mail sent through the bus owner |
| `server.rs:3111` | retired | graph reorder used to shell the porcelain. `store_client.rs` speaks the keeper socket now; the landed write detaches one `render-views` replay (graph.md stays Python; the board render is native) |
| `server.rs:3170` | conforming | respawn through the spawn surface owner |
| `server.rs:3202` | conforming | transcript peek read through the transcript reader owner |
| `server.rs:10818` | conforming | touch telemetry through the event emit path |
| `server.rs:10912` | conforming | pane counters through the same emit path |
| `spawn_gate_lanes.rs:123` | conforming | pane listing fallback for the same probe, the pane owner's authoritative enumeration |
| `doctor/report/ops.rs run_session_cost` | conforming | session cost through `fno-agents context-run --session-cost --json`, one answer per spawn |
| `doctor/mod.rs context_audit_run` | conforming | the context census through `fno-agents context-run --audit`, all its inputs in one spawn |

The interpreter direction is retired. The doctor rows that once exec'd the wheel's interpreter now exec `fno-agents`, and the installed-package probe is gone with the wheel.

## The refusal of consumer-driven scoping

Before this port, `claims.rs` stated the pattern this seam must stop minting. A consumer-driven Rust subset leaves `list` and the rest of the decision's facts in Python. The native claim list, sweep classification, and batch verdict door now close that gap.

The old scope produced a predictable regression. Today's caller needed today's commands, so the port moved today's commands. Tomorrow's caller needed `list`, and `list` was on the far side of the seam, so tomorrow's caller shelled back. The complete fact-set port removes that crossing and leaves Python with one batch door instead of a second classifier.

The replacement rule: port a decision's whole fact set, or do not port the decision. Under the process-only ruling this is arithmetic. A port that carries half a fact set adds a spawn per missing fact.

## The two-axis budget

A port must raise neither axis:

1. the crossing count that `fno doctor lint seam-crossings` ratchets, and
2. the dual-implementation count that the inventory page hand-maintains until the duplicate-discovery sweep lands.

The gaming path is real, and the tree already holds the specimen. `crates/fno/src/backlog_view.rs` is 2140 lines of native graph parsing whose docstrings name their Python oracles. It holds exactly one crossing. Replacing a shell-out with more of that file lowers axis one and raises axis two, and a one-axis budget scores the trade as an improvement. It is a worsening: the tree gained a second implementation of read logic and lost nothing. The budget reads both numbers, and a port that raises either is refused.

The store port is the counter-example, and it names its own costs. It removed the reorder crossing and deleted the Python store leg. Both axes fell. Two prices remain. First, the frame codec is hand-written three times: the Python client, the keeper, and the mux client. The codec is small: five frame tags and one request grammar. The direction law allows no shared crate. The unit tree exercises the Python spelling against the real keeper on every run. Second, the views stayed Python-owned for a while. A landed mux write detaches one `fno backlog render-views` subprocess to replay the post-publish pass. The write is native and atomic, but a native writer could not refresh graph.md without crossing back to the renderer. The board half of that price is gone now (the native `board-render` writes the board pages; the replay keeps graph.md), and the codec price stays. A budget that reads only counts scores the store port cheaper today than the day it landed. It was not free.

## Sequencing

Order ports topologically over the crossing dependency graph, not by risk. Risk ranks the claim classifier first on the inventory page because nothing pins it. Dependency says which port can land first, and the two orders disagree.

The first real edge landed: `claim list` and the sweep fact set became Rust-owned before the daemon's reap decision moved. The daemon now reads the native list directly, so the crossing count falls instead of rotating.

The remaining violating site is `provider.rs:544`, which waits for the plan-path fact set. The remedy is carrying that fact set, never a native re-implementation that raises axis two.

### Port order

The port-order table ranked the modules under `cli/src/fno`. That tree is deleted, so the table is retired. A future port between the two crates still runs leaves-first, and a dependency still outranks the score.
