"""Rust runtime routing for ``fno agents`` (Phase 6 W6 / cv-d28b266a).

The Rust daemon is the **default** runtime for the daemon-native commands: by
default ``fno agents <command> [args]`` execs the installed ``fno-agents`` client
binary for every Rust-backed command (``AUTO_ROUTE_COMMANDS`` = the client commands
minus ``PYTHON_AGENT_COMMANDS``). Python implementations stay registered as the
``FNO_AGENTS_RUNTIME=python`` fallback and serve when no installed binary is
present; ``role`` is Python-owned (it mutates the shared registry without a
daemon RPC).

``FNO_AGENTS_RUNTIME``: ``rust`` forces the binary (missing = hard 127);
``python`` forces Python dispatch; unset or anything else = ``auto``. In
``auto`` only *installed* binaries resolve (never the cargo dev target), so
a dev checkout opts into the local build with ``FNO_AGENTS_RUNTIME=rust``.
The retired twins ``ask`` and ``rm`` have NO Python leg: they run on the
binary or refuse through :func:`refuse_without_binary`, never a silent
fallback.

Design: internal/fno/design/2026-05-22-fno-pty-supervisor-and-drive.md
(line 136); plan: plans/2026-05-25-phase6-w6-distribution.md.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path

import yaml
from typing import IO, TYPE_CHECKING, Callable, NoReturn, Optional, Sequence

# The binary lookup itself is stdlib-only and has callers below this layer, so
# it lives at fno.rust_binary; this module keeps the dispatch/routing half.
from fno import rust_binary
from fno.agents.launch_provenance import LAUNCH_ACCOUNT_SOURCE_ENV

if TYPE_CHECKING:
    import click

#: Env var that selects the runtime. Recognized values: ``rust`` (force the
#: binary), ``python`` (force Python dispatch); anything else (incl. unset)
#: means ``auto`` -- the default, which prefers an installed binary per-command.
RUNTIME_ENV = "FNO_AGENTS_RUNTIME"
#: Exit code when the runtime is requested but the binary is absent. Distinct
#: from the daemon's own codes (1/2/13/14/15/18) so the failure is legible.
BIN_NOT_FOUND_EXIT = 127
#: Exit code when an operator answered no to ``rm``'s resume-handle prompt.
#: Distinct from every daemon code so "you declined" never reads as "it failed";
#: nothing was removed and nothing needs retrying.
RM_DECLINED_EXIT = 3


def refuse_without_binary(command: str) -> NoReturn:
    """Refuse a command whose Python leg was ported to Rust and deleted.

    Called when no installed binary resolved or ``FNO_AGENTS_RUNTIME=python``
    is set: there is nothing to fall back to, so the refusal names the
    binary, the flag, and how to get the binary. Exit :data:`BIN_NOT_FOUND_EXIT`.
    """
    forced = (
        f"{RUNTIME_ENV}=python is set, and there is no Python {command} left to "
        "force; unset it with the binary installed. "
        if runtime_mode() == "python"
        else ""
    )
    print(
        f"fno agents {command}: the Python {command} runtime was ported to the Rust "
        f"runtime, so {command} requires the '{rust_binary.BINARY_NAME}' binary, "
        f"which was not found. {forced}"
        "Get it via `pip install fno` (bundled wheel), `cargo install "
        "fno-agents`, or `cargo build --release -p fno-agents` plus "
        f"`export {rust_binary.BINARY_ENV}=<path>`.",
        file=sys.stderr,
    )
    raise SystemExit(BIN_NOT_FOUND_EXIT)

FOLDED_AGENT_SUBCOMMANDS: dict = {
    name: tuple(entry)
    for name, entry in yaml.safe_load(
        (Path(__file__).resolve().parent / "folded_subcommands.yaml").read_text(
            encoding="utf-8"
        )
    ).items()
}

#: Commands the bundled ``fno-agents`` client implements end-to-end: the daemon
#: request commands in ``client.rs`` ``build_request`` plus the directly-dispatched
#: commands (``drive``, ``status``, and the client-side ``drive-authority``,
#: ``trace``, ``ping``, ``resume``, ``attach``, ``logs`` ported in ).
#: The ``auto`` (default) runtime routes all of these to Rust except the commands
#: Python still owns (see :data:`PYTHON_AGENT_COMMANDS`). Kept in sync with
#: ``crates/fno-agents/src/bin/client.rs`` by a test that parses that file, so
#: drift fails CI rather than silently mis-routing.
RUST_CLIENT_COMMANDS = frozenset(
    {
        "loops",
        "spawn",
        "ask",
        "list",
        "status",
        "stop",
        "rm",
        "reconcile",
        "worked-nodes",
        # Daemon binary-version drift restart: a Rust-only command
        # dispatched directly in client.rs before build_request (no daemon RPC).
        # SIGTERMs a stale daemon and lazy-starts a fresh one from the current
        # binary; PTY workers survive (Outcome B).
        "restart",
        # Manual dead-row GC: the same sweep the daemon runs on its idle
        # tick, on demand. Dispatched directly in client.rs before build_request
        # (operates on the registry under the shared flock; no daemon RPC).
        "reap",
        # The ONE spawn gate (spawn_gate_command.rs): dispatched directly before
        # build_request, no daemon RPC. The Python gate transport shells it
        # with a stdin JSON payload and reads the answer envelope.
        "spawn-gate",
        # `drive` and `grid` (the WebSocket drive surface + the TUI compositor)
        # were retired at G4 when the mux became the agent-PTY
        # substrate; the binary intercepts them with a mux pointer.
        #
        # The three `*-channel` commands are gone from the ROUTABLE set for the same
        # reason: nothing shelled them. The `channel.*` daemon RPCs they wrapped
        # are live and untouched (protocol.rs, daemon.rs) - what went away is the
        # CLI surface in front of them, still reachable as `fno-agents <command>`
        # for daemon debugging.
        # Python-only commands ported to the Rust client (full thin-wrapper rewire).
        # These dispatch client-side in client.rs before build_request (no daemon
        # RPC, except `logs --follow` which upgrades to the agent.logs WS stream).
        "drive-authority",
        "trace",
        "resume",
        "adopt",
        "attach",
        "logs",
        # register: the /fno-me self-service join, ported to a daemon-free
        # client command (the owned-identity ladder plus the row mint under the
        # shared registry lock). Starts nothing.
        "register",
        # The registry and store leaves, daemon-free status arguments like
        # register: the canonical agent-name bridge (the daemon stays the name
        # validator, the client the generator), the caller's own mesh row, the
        # poisoned-registry schema repair, and the registry-miss healer.
        "name",
        "whoami",
        "registry-repair",
        "heal-token",
        # The pane/registry identity cross-check: a read, never a repair.
        "pane-identity",
        # The daemon-free revival decision the spawn path calls. Client-side
        # dispatch starts nothing.
        "revival-check",
        # registry-json: the daemon-free registry projection the hooks read.
        # Reads the registry file client-side (load_registry_entries) and
        # derives the served liveness pair with the vendored freshness rule.
        # A client-side dispatch starts nothing, so the Stop hook's
        # never-lazy-start promise still holds (the reason this command once
        # sat in the never-route set).
        "registry-json",
        # `host`/`promote` (interactive daemon PTY hosting) were retired at G4
        #; spawn a mux-hosted pane with `spawn` (the pane lane is the native default where no thread seats).
        # Stop-hook decision command (control-plane collapse wedge, ).
        # The bash shim in hooks/target-stop-hook.sh calls the binary DIRECTLY
        # (explicit resolution order, no Python routing); this entry exists so
        # `fno agents loop-check` also works for manual/diagnostic invocation
        # and so the client.rs<->router parity test stays in sync.
        "loop-check",
        # Unified driver loop command (step 5, ). Dispatched directly
        # in client.rs before build_request (no daemon RPC); this entry keeps
        # the client.rs<->router parity test in sync and lets `fno agents loop
        # run ...` route for manual invocation.
        "loop",
        # Terminal-only side-effect WRITER (control-plane step 6, ).
        # Like loop-check, the bash stop-hook shim calls the binary DIRECTLY on a
        # terminal-allow decision (no Python routing); this entry exists so the
        # client.rs<->router parity test stays in sync.
        "finalize",
        # Eliminate-don't-vendor folds (packaging EPIC, US1
        # ): Rust ports of the deleted scripts/lib/kill-criteria.sh
        # and scripts/lib/verify-event-evidence.sh. Both dispatch DIRECTLY in
        # client.rs before build_request (no daemon RPC). The Python `fno do phase
        # kill-check` wrapper resolves the binary and invokes its command
        # explicitly (not via `fno agents` routing); these entries exist so
        # the client.rs<->router parity test stays in sync.
        "kill-check",
        "verify-evidence",
        # Plan-closure outcome probes: the close commands shell out to
        # `fno-agents probe-run --key close_probes`, the same runner loop-check
        # uses for done_probes. Dispatched directly in client.rs before
        # build_request (no daemon RPC); this entry keeps the client.rs<->router
        # parity test in sync.
        "probe-run",
        # prove-it-verdicts: direct dispatch in client.rs; parity-test sync.
        "prove-it-verdicts",
        # honesty-sweep: direct dispatch in client.rs; parity-test sync.
        "honesty-sweep",
        # Native test-run process-group owner: admits under the
        # `test:suite` claim, spawns the suite leader in its own session, and
        # always cleans up the group. Internal dispatch only, matched the same
        # way as `probe-run`; this entry keeps the client.rs<->router parity
        # test in sync.
        "test-run",
        # Registry-label rename: `agent.rename` over the daemon RPC, the same
        # transport as rm/stop. The old label rides home as a persisted alias.
        "rename",
        # Inside-leg state push (inside-out E3.2): a per-turn hook calls
        # `fno agents report --session-id <uuid> --seq <n> --state <s>` and the
        # Rust client sends the agent.report RPC to an already-running daemon
        # (never lazy-starts). Dispatched directly in client.rs before
        # build_request (no Python impl); this entry keeps the
        # client.rs<->router parity test in sync and provides the help line.
        "report",
        # Agent-state wait + event subscription (mux roadmap wave 2).
        # Both dispatch DIRECTLY in client.rs before build_request (no daemon
        # RPC): `wait` polls registry.json until a row reaches idle|blocked|done;
        # `subscribe` follows the daemon's events.jsonl and streams state
        # transitions + pane exits as NDJSON. These entries keep the
        # client.rs<->router parity test in sync and provide the help lines.
        "wait",
        "subscribe",
        # Catch-up digest: read-only "while you were gone" fold over
        # events.jsonl + ledger.json for a session. Dispatched directly in
        # client.rs before build_request (no daemon RPC, no Python impl); this
        # entry keeps the client.rs<->router parity test in sync.
        "digest",
        # Needs-me queue: read-only fold over events.jsonl + ledger for
        # ALL sessions, emitting review_wedged / budget_stop items. Dispatched in
        # client.rs before build_request (no daemon RPC, no Python impl).
        "needs",
        # One "is this worker alive" answer per handle (the census rank-2
        # remedy): transcript age + pid probe + registry status through the
        # Rust verdict door, the caller's own row as positive control. No
        # Python leg: the Python scripts it replaced were the bug. A
        # status-arg leaf (_STATUS_ARG_LEAVES): the action list is
        # shrink-only, so the router rewrites `fno agents liveness <h>...`
        # into `fno-agents status --liveness <h>...`.
        "liveness",
        "select-read",
        # Activity feed projection: questions.jsonl + graph.json ->
        # ordered rows carrying the node id + session id the mux deep link
        # resolves. Dispatched in client.rs before build_request (no daemon
        # RPC, no Python impl).
        "feed",
        # Standalone review_coverage producer: the same resolver +
        # emitter the stop hook uses, so any path that can reach the merge gate
        # (``fno do pr merge``/``status`` recompute, a manifest-less session) can
        # also satisfy it. Dispatched directly in client.rs before
        # build_request (no daemon RPC, no Python impl).
        "review-coverage",
        # Review-attestation carrier between this machine's event journal and
        # the shared primary. Dispatched directly in client.rs before
        # build_request (no daemon RPC, no Python impl); this entry keeps the
        # client.rs<->router parity test in sync.
        "attest",
        # Pre-manifest <help> tag read: the stop hooks' visitor-allowed
        # exit calls this directly when no manifest names the session, so a
        # worker that dies before `target init` still gets its distress heard.
        # Dispatched directly in client.rs before build_request (no daemon RPC,
        # no Python impl); this entry keeps the client.rs<->router parity test
        # in sync and provides the help line.
        "distress-scan",
        # Manual session restoration: hidden-but-invocable operator
        # escape hatch that restores a recorded session under its account and
        # route, refusing until ``--session`` selects between a fork's two
        # recorded ids. Dispatched directly in client.rs before build_request
        # (no daemon RPC, no Python impl); this entry keeps the
        # client.rs<->router parity test in sync and provides the help line.
        "recover",
        # bash-census and session-start bytes dispatch directly in
        # client.rs before build_request, never `fno agents`; the batch
        # graph read and the note actions folded into `fno-agents backlog`.
        "bash-census",
        "session-start-bytes",
        "judge",
        # Orphan-role sweep for `fno agents team`: daemon-free read, never `fno agents`.
        "org-vacancies",
        # Role scope fold for `fno agents team --nodes`: daemon-free read;
        # graph.json and claims in, per-scope fold out; Python passes the roles.
        "org-fold",
        # Role-scope checkin readback: the front door execs it for `fno
        # agents org history` and `verdict`; escalate's verdict relay
        # invokes it directly. Never routed through `fno agents`.
        "lead-history",
        # Failure-pattern leaderboard fold for `fno doctor evals macro`:
        # daemon-free read; Python resolves the journal list and forwards
        # the flags, then invokes the binary (not via `fno agents` routing).
        "evals-macro",
        # The check-in beat: the front door execs it for `fno agents org
        # checkin`, and the beat resolves its scope and paths natively.
        "lead-checkin",
        # Rundown page renderer for `fno agents lead ledger`: team JSON and
        # the graph in, one HTML page out; Python resolves the team and the
        # paths, then invokes the binary (not via `fno agents` routing).
        "lead-rundown",
        # The delivery-slot resolver: payload JSON in, the answer out; Python
        # calls it via fno.rust_binary.command_call (keeps the parity test
        # in sync).
        "route-slot",
        # The harness-keyed spawn-defaults resolver: payload JSON in,
        # the answer out; Python calls it via fno.agents.spawn_overlay_client.
        "spawn-overlay",
        # The billing axes of the spawn seam (route/account/model): payload
        # JSON in, the {inject, applied, suppressed, messages} plan out;
        # Python calls it via fno.agents.spawn_axes_client. A `node_seed`
        # field routes the payload to the node-seed decision instead.
        "spawn-axes",
        # The failover chain walk: payload JSON in, the
        # {eligible} answer out; Python calls it via fno.rust_binary.command_call.
        "fallback-chain",
        # The one authorized merge operation: payload JSON in, one receipt out.
        # `fno do pr merge`, `fno do pr verify --kind merged` and the finalize
        # queue arm all ask it, so no two merge paths can answer "may this head
        # merge?" differently. Python calls it via fno.rust_binary.command_call.
        "authorized-merge",
        # Running-process census; the walker lives in census.rs.
        "census",
        # Durable fleet incident breaker: direct dispatch in client.rs
        # (no daemon RPC); public surface `fno agents incident`. The fleet
        # GitHub request budget rides this action as its gh-budget argument
        # (law d-fe66560a allows no new client action). Parity-synced.
        "fleet-incident",
        # Fleet announcements (one bus line, per-session cursor): direct
        # dispatch in client.rs (no daemon RPC); the mail shim and the hook
        # scripts invoke the binary directly. Parity-synced.
        "announce",
        # Provider-cap actor: status/decide/mark commands over the
        # daemon snapshot. Direct dispatch in client.rs (no daemon RPC for
        # status; decide records the operator's answer). Parity-synced.
        "provider-cap",
        # PreCompact stamp writer/reader: the hook invokes the
        # binary directly (`fno-agents compaction mark`). Parity-synced.
        "compaction",
        # The fmail read-command lesson state: the hook carriers invoke
        # the binary directly (`fno-agents mail-teach --self --mark`).
        # Parity-synced.
        "mail-teach",
        # Read leaves over the packaged capability table and the merge-posture
        # family table: the successors to the retired
        # dispatch capabilities/family query leaves (d-496680aa). Direct dispatch
        # in client.rs (no daemon RPC); hidden `fno agents` surface. Parity-synced.
        "capabilities",
        "target-family",
        # The permission-posture and sandbox-verdict owners behind the
        # spawn seam: payload JSON in via fno.rust_binary.command_call, one
        # answer envelope out; no Python impl, no `fno agents` routing.
        "permission-tokens",
        "sandbox-probe",
    }
)

#: Commands the Python ``agents`` app implements that do NOT auto-route to the
#: Rust client.
#:
#: ``send`` (G2 Task 2.1): async durable-first delivery command. Python owns it
#: in Group 2; Rust port deferred to Group 4. The command is NOT in
#: ``RUST_CLIENT_COMMANDS`` so it never auto-routes to the daemon.
#:
#: History: ``stop``/``rm`` (Task 2.1), ``list``/``reconcile`` (Task 3.1), and
#: the six former Python-only commands (``logs``/``ping``/``drive-authority``/
#: ``attach``/``resume``/``trace``, ) all reached Rust stdout/JSON
#: parity and left this set. ``ask`` was the last holdout: claude shipped
#: client-side in (PR #366), codex + the provider-conditional flip
#: in (PR #371), and gemini + this UNCONDITIONAL flip in.
#: The provider-conditional special case (``RUST_CLIENT_ASK_PROVIDERS`` +
#: ``_resolve_ask_provider``) is gone; the ``AUTO_ROUTE_COMMANDS`` identity below is
#: now the whole routing contract except for ``send``.
PYTHON_AGENT_COMMANDS: frozenset[str] = frozenset({
    # Human-attended in-place role grant. Pure shared-registry transaction;
    # spawn-time grant and succession remain on the Rust-backed spawn path.
    "role",
    #: the whole-team read (every live role + registry/graph
    # agreement). Pure Python (fno.agents.team reads the registry and the
    # graph); no Rust client port, so it must never auto-route to the daemon.
    "team",
    # Messaging commands are not direct agents actions. They live below the
    # Python-owned `fno agents mail` subgroup and therefore never auto-route as
    # direct send, inbox, or ack Rust commands. `incident` relays argv.
    "incident",
    # Epic G2 Task 4.3: the stream-json observe surface. Pure Python;
    # polls the worker's stream.read_frames directly. No Rust client port (the
    # `--watch` worker-binary surface noted in client.rs is a separate lane), so
    # it must never auto-route to the daemon.
    "watch",
    "distress-verdicts", #: blocked_child's verdict lookup; no Rust port.
    # The stop hook's read-only spawn-gate capacity probe. Pure Python (the
    # gate lives in fno.agents.spawn_gate); no Rust port, so it must never
    # auto-route to the daemon.
    "gate-status",
    # P1: internal helper the Rust `list` render path shells out to
    # for the discovered-live-sessions lane. Pure Python (reads
    # ~/.claude/sessions via fno.agents.discover); no Rust port, so it
    # must never auto-route - Rust invokes it with FNO_AGENTS_RUNTIME=python.
    "discovered-json",
    #: the shared bg-dispatch guard command. Pure-Python orchestration of
    # `fno agents claim` (Guard 1 node-claim probe + Guard 2 dispatch:<id> reservation)
    # called by both dispatch-node.sh and spawn.sh. There is NO `spawn-guard` on
    # the Rust client, so it must never auto-route to the daemon (it would 404 /
    # be shadowed for installed users). Python owns it.
    "spawn-guard",
    # The bundle transfer: the Python shim execs the native fno binary's
    # `agents transcript` role (the history pattern, not a client action).
    "transcript",
    "retask",
    # The cadence-deadline silence backstop. Pure Python: it reads the registry
    # and each row's transcript truth through fno.agents.sweep, writes nothing,
    # and has NO twin on the Rust client - so it must never auto-route to the
    # daemon.
    "sweep",
    # The session-history reader over live rows, reap receipts and the ledger.
    # Pure Python (fno.agents.history + the shared ledger matcher); there is no
    # Rust port, so it must never auto-route to the daemon.
    "history",
    #: the spawn-gate audit surface. The registered Python command is a shim
    # that execs `fno-agents census --workers`; the `fno` front execs it
    # directly. `top` is no client action, so it must never auto-route.
    "top",
    #: the read-only observe leg (twin of `fno agents mail send`). Reads a
    # peer's on-disk transcript / status events via fno.agents.peek. No Rust
    # client port, so it must never auto-route to the daemon.
    "peek",
    #: the transcript-tail supervision classifier (`fno agents truth`).
    # Read-only, pure Python (fno.agents.session_truth reads the transcript via
    # peek); no Rust client port, so it must never auto-route to the daemon.
    "truth",
    # The orphan sweep. The registered Python command is a shim that execs
    # `fno-agents census --orphans` (crates/fno-agents/src/orphans.rs); the
    # body routes in every runtime mode, and `orphans` is no client action, so
    # it must never auto-route.
    "orphans",
    #: the codex late-bind helper `late_bind_codex_sessions` (Rust,
    # fno-agents daemon) shells out to for one pane's rollout session id.
    # Pure Python (fno.agents.mux_spawn._codex_session_id_for_pid, reads the
    # pane's open rollout fd); no Rust port, so it must never auto-route to
    # the daemon.
    "codex-session-for-pid",
    #: the external fleet watchdog. Pure Python (transcript tail reads +
    # registry/graph joins; apply lanes shell out to fno commands); no Rust client
    # port, so it must never auto-route to the daemon.
    "watchdog",
    # The report-only question reconciles (`fno agents stale-escalate` and
    # `friction-escalate`). Pure Python: run_sweep's real seams plus the
    # durable-question fold in fno.agents.stale_lane; the DAEMON is the
    # scheduled caller, so routing the command back to the daemon would be a
    # shell-out cycle. No Rust client port, never auto-routes.
    "stale-escalate",
    "friction-escalate",
    # The provider-cap relay: argv passthrough to the Rust client's
    # provider-cap commands (status/decide). No parsing in Python; the Rust side
    # owns the snapshot, the decision ladder and the journal. Hidden on the
    # help surface (menu cap), so it is registered but never advertised.
    "provider-cap",
})

#: Commands the ``auto`` (default) runtime routes to Rust: the Rust client commands
#: MINUS the commands Python still owns. Every Rust-backed command (including ``ask``
#: for all providers) auto-routes when an installed binary is present; an
#: attended ``role`` always stays in Python. A forced
#: ``FNO_AGENTS_RUNTIME=rust`` still routes every command; a forced ``=python`` (or
#: no installed binary) keeps the mature Python dispatch.
AUTO_ROUTE_COMMANDS = RUST_CLIENT_COMMANDS - PYTHON_AGENT_COMMANDS

#: Short help for the commands that exist ONLY on the Rust client (no
#: ``@agents_app.command`` registration). Without these, ``fno agents --help``
#: -- which always renders the Python group help (a bare ``--help`` never execs
#: the binary) -- silently omits every Rust-only command, so an agent reading the
#: help has no way to discover ``host``/``grid``/``drive``/``spawn``/... This
#: dict is the discoverability source: :class:`_AgentsRuntimeGroup` injects each
#: entry into ``list_commands``/``get_command`` so the group help lists them with
#: a description, even though the actual dispatch happens in ``make_context``
#: (which execs the binary before Click ever resolves the sub-command).
#:
#: Insertion order is the help display order. The keys MUST equal
#: ``RUST_CLIENT_COMMANDS`` minus the Python-registered command names -- a test
#: (``test_rust_only_command_help_covers_unregistered_commands``) enforces that, so a
#: future Rust-only command cannot land without a help entry and re-introduce the
#: gap.
RUST_ONLY_COMMAND_HELP: dict[str, str] = {
    "worked-nodes": "The worked-overlay join: one JSON rows payload on stdin (--rows-file -); reads the graph, registry, and claims itself and answers the node-to-live-workers map with the role and provenance gate.",
    "spawn": "Spawn a worker. --as picks the lane: thread (the default), pane, headless, or cloud. Placement flags need --as pane; --once means headless. --model/-m, --effort, --permission-mode, --add-dir, --agent and --tools are the harness's own flags: typed before the fence they ride it, and a lane that cannot carry one refuses and names it. Declared harnesses: claude, codex, opencode, pi, agy, cursor-agent, grok (docs/HARNESSES.md); `fno agents spawn --help` lists every flag.",
    "status": "Report daemon liveness and the control-plane arms table (one row per scheduled arm, red when its last tick is stale); --json for the machine payload with `arms`.",
    "restart": "Restart running fno processes onto fresh binaries: the daemon, stale keepers and the mux leg (stale idle servers by default, every live server with --mux); --force for the break-glass daemon kill, --json for one machine line.",
    "reap": "Retire finished agent rows (every node the session is named on is done AND its transcript is quiet past agents.retire_grace_s); stages a resume receipt, prunes a clean merged worktree, --json for machine output, --dry-run to rehearse (names the gate keeping every held-back row, with its age and escalation), --release <row> to apply a ruling to one escalated hold (refuses a fresh hold or open work by name). Also sweeps the mux tab sideline via `fno mux workspace prune --tabs-only --include-used-shells` and reports it as the receipt's `mux` half (ran/unread/skipped). --no-mux skips that half.",
    "loop-check": "Stop-hook decision: external-truth done()/backstop check (read-only).",
    "loops": "Global loop pause control: paused|status --json, pause-all --who <w> [--ttl <dur>|--ttl-ms N] [--reason <text>] --json (also holds this session's mail for the same window via `fno agents mail hold`), or resume-all --json.",
    "loop": "Unified driver loop: run --driver target [options] (step 5).",
    "finalize": "Terminal-only side-effect writer: ledger record + (ship) plan stamp/handoff (step 6).",
    "kill-check": "Evaluate a plan's kill_criteria (folded from kill-criteria.sh); usually via `fno do phase kill-check`.",
    "verify-evidence": "Verify child-promise event evidence and non-Claude agent presence (folded from verify-event-evidence.sh).",
    "probe-run": "Evaluate a plan's named probe list (done_probes/close_probes); exit 0 only when every row is PASS - exit 0 with no output reads SKIP, not pass. Rows carry verdict (PASS FAIL BLOCKED SKIP), an optional ` # claim` comment from the declaration, and bounded captured output. Shelled by the close commands for close_probes and by prove-it for runtime evidence.",
    "honesty-sweep": "Declared-vs-measured sweep over declared populations: --population harness-capabilities (the default) sweeps the compiled capability table, --population lead-manifests sweeps the space's lead manifests, --rows-json <path|-> [--rows-key <key>] [--name <label>] sweeps piped or filed JSON rows. Four passes: uniform fields, uniform among declarers, negative-claim counts, and (harness-capabilities only) negative claims paired with harness-named exported definitions plus hardcoded harness-name lists. Exit 2 unmeasured (unreadable source, <2 rows, invalid JSON), 0 otherwise - a finding is a candidate, never a verdict.",
    "prove-it-verdicts": "Read terminal prove-it records: walks every node's `<plan>.artifacts/` tree, takes each report's LAST non-empty line when it carries the `fno-prove-it:` JSON record, and emits one row per unretired FAIL plus the newest PASS/FAIL record as the node's headline (a PASS retires a FAIL only when its claim states the FAIL claim, optionally scoped by a `retires` report path; SKIP/BLOCKED retire nothing). --json emits {read_at, rows, unreadable}; --route writes the one progress note per open, unrouted FAIL (never changes node status - a lead rules). Retire a FAIL with a newer PASS whose claim states the FAIL claim, or a decision naming the report.",
    "test-run": "Native test-suite process-group owner: --timeout SECS [--claims-root PATH] -- ARGV...; admits under the machine-wide test:suite claim, spawns ARGV as the leader of a fresh session, and always kills the group after. Each run journals test_run started/finished rows; `test-run stats` prints the median and p90 per command. Invoked directly by cli/src/fno/test_runner.py's run_suite_bounded, not `fno agents` routing.",
    "report": "Inside-leg state push (E3.2): store working|blocked|done on a claude row; called by the per-turn hook.",
    "wait": "Block until an agent's registry row reaches idle|blocked|done: --agent <name> --state <s> [--timeout-ms N] [--json].",
    "subscribe": "Stream registry state transitions + pane exits as NDJSON (follows events.jsonl): [--agent <name>] [--kinds state,exit] [--json].",
    "digest": "Catch-up 'while you were gone' fold over events + ledger for a session: --session <s> --since <ts> [--json].",
    "needs": "Needs-me queue fold over events + ledger across all sessions (review_wedged/budget_stop): [--since-epoch <secs>] [--fires-floor <n>] [--json].",
    "liveness": "One 'is this worker alive' answer per handle: <handle...> [-J|--json] reads the registry row, the transcript age (the one reader), and the pid through the one verdict door; the caller's own row (FNO_AGENT_SELF or a harness session env) rides along as the positive control. Exit 0 all live, 1 any finished, 2 any unknown, 3 no handle resolved. Daemon-free; starts nothing.",
    "select-read": "One bounded backlog read (next|undispatched) under [auto_continue] select_timeout_s; prints an ok/unmeasured/error receipt JSON.",
    "feed": "Activity feed projection over questions.jsonl + graph.json (questions, decisions, node lifecycle): [--since-epoch <secs>] [--limit <n>] [--node <id>] [--session <id>] [--json].",
    "adopt": "Register an orphaned session by its session id so it is addressable (peek/ask/resume/mail); resolves the registry, .fno/target-state.md, then harness stores.",
    "revival-check": "One JSON answer to whether a resume revives an existing row instead of forking: the row by name or by the resumed uuid, supervisor not live; consumed by the spawn path.",
    "review-coverage": "Emit the review_coverage event for a PR with the stop hook's own resolver/emitter: --cwd <dir> [--pr <n>] [--head <sha>]. No way to assert coverage without the reads.",
    "attest": "Review-attestation carrier over the shared primary: push reads one envelope line on stdin, pull prints this repo's rows (unset store.remote_url is a no-op).",
    "distress-scan": "Read a transcript for a <help> tag and append a blocked row on a hit: --transcript <path> --run <id> [--node <id>] [--harness <name>] [--cwd <dir>]. Best-effort, always exits 0.",
    "recover": "Restore a recorded claude session under its account and route: <agent> [--session <id>] names the id when the row holds two; --print-command prints the inspection form and touches nothing.",
    "rename": "Rename a registry row's label: <worker> --name <new-label>; the old label keeps resolving as an alias.",
    "bash-census": "Bash-call compound/cd/heredoc shares and top command/command tables over recent transcripts; invoked directly by `fno doctor bash-census`.",
    "session-start-bytes": "Session-start preamble byte total; invoked directly by `fno doctor`'s session-start byte report.",
    "judge": "Blueprint judge: grade a plan against the five product questions, --budget <secs> to bound the whole pass (default 600s), or --labels/--split to calibrate against evals/blueprint-judge/labels.yaml; invoked by the observer ops door (crate observer mod.rs) through its own subprocess round-trip (judge_via_rust), not `fno agents` routing.",
    "org-vacancies": "Roles whose registry row is gone but whose manifest holds them: --root <spaces-root> --held <scope> (repeatable, one flag per scope); invoked directly by `fno agents team`, not `fno agents` routing.",
    "org-fold": "The role scope fold: --graph <graph.json> --promotes-json <roles> --claims-dir <dir> --format json; invoked directly by `fno agents team`, not `fno agents` routing.",
    "lead-history": "The role-scope lead_checkin readback: --scope <scope> --events-path <events.jsonl> [--events-path ...] [--json]; --verdict selects the term tenure verdict read (assembles its own inputs natively: role, manifest, config, graph scope, window, delivery split); exec'd by the front door for `fno agents org history` and `verdict`, which resolve their own journals natively; escalate's verdict relay invokes it directly.",
    "evals-macro": "The macro-eval failure-pattern leaderboard fold: --events <journal.jsonl> [--events ...] [--since 30d] [--topic TYPE:LABEL] [--window 20] [--all] [--json]; invoked directly by `fno doctor evals macro`, which resolves the journal defaults.",
    "lead-checkin": "One command runs the term check-in body: --scope <scope> --events-path <events.jsonl> [--events-path ...] --graph <graph.json> --handoffs-dir <dir> [--faqs-dir <dir>] [--board-state <manifest>] [--emit-path <events.jsonl>] [--no-emit] [--json]; invoked directly by `fno agents lead checkin`, which resolves the role and the paths.",
    "lead-rundown": "The term ledger page renderer: --team-json <team.json> --graph <graph.json> --generated <ts> [--out <page>]; with no --out the command resolves the lead page through the layout table. Invoked directly by `fno agents lead ledger`, which resolves the team.",
    "route-slot": "Delivery-slot resolver: JSON payload on stdin, the {candidate, chain} answer on stdout; its payload modes carry the folded doors - {mode: resolve, explicit, env, flag} answers {harness, source} or {refusal}, {mode: validate-model, model, flag} answers {model} or {refusal}, {mode: configured, command} answers the stage-table rung {harness, note, route}, and {mode: config-route, argv} answers the config route family's reads and write plans for `fno config route`; invoked by the spawn seam, the dispatch defaults and the front's route spellings through rust_binary.command_call, not `fno agents` routing.",
    "spawn-gate": "The ONE spawn gate: reads one stdin JSON payload, writes one {status: admitted, gate/worker keys} or {status: refused, exit_code, receipt, event} answer; gate and probe modes; invoked by the fno.agents.spawn_gate transport.",
    "spawn-overlay": "Harness-keyed spawn-defaults resolver: JSON payload on stdin, the {refusal, effective, bundle} answer on stdout; invoked by fno.agents.spawn_overlay_client, not `fno agents` routing.",
    "spawn-axes": "Spawn-seam billing axes (route/account/model): JSON payload on stdin, the {inject, applied, suppressed, messages} plan on stdout; a `node_seed` field instead answers the node-command check. Invoked by fno.agents.spawn_axes_client and the spawn seam, not `fno agents` routing.",
    "fallback-chain": "Failover chain walk: JSON payload on stdin, the {eligible} answer on stdout; invoked by fno.recovery, not `fno agents` routing.",
    "authorized-merge": "The one authorized merge operation: JSON payload on stdin, one receipt (merged|armed|authorized|held|refused|head_changed|unknown|failed) on stdout; invoked by fno.rust_binary.command_call from the merge and verify commands, not `fno agents` routing.",
    "census": "One JSON row per long-lived process (daemon, keepers, mux servers) with its build-drift verdict; invoked by the native updater (crates/fno/src/doctor_update.rs), not `fno agents` routing. --workers [--json] [--subagents] [--pane-stats] answers `fno agents top`. --orphans [--reap] [--quiet-unless-new] [--json] is the orphan sweep (`fno agents orphans`).",
    "fleet-incident": "Durable fleet incident breaker: stop --reason T / clear --reason T write the machine-wide record; reserve stream|edit [--for 2h] arms a time-boxed preset on it; status [--json] reads it with its typed holds/admits reach (exit 0 clear, 1 stopped or unavailable); check [--scope spawns|tests|merges] is one scope's admission verdict (exit 0 clear, 90 stopped, 91 unavailable). The public surface is `fno agents incident`; the spawn/test/daemon gates read the file before their bypass branches, and the merge primitive refuses while merges are held. The fleet GitHub request budget rides this action as its gh-budget argument (one JSON payload on stdin, {op: admit|refused|status}; ledger in the machine locks folder; called via fno.rust_binary.command_call from pr/_quota.py).",
    "announce": "Fleet announcements: send --scope S [--subject T] [--expires 24h] [--urgent] reads the body on stdin and appends ONE kind=announce bus line (operator or promoted agent, 6/hour); read --session-id ID --boundary B renders unseen standing announcements once per session; status ID [--json] reads the sender's receipts. The public surface is `fno agents mail team`; hooks call the binary directly.",
    "compaction": "Compaction stamps: mark --session <id> writes the PreCompact stamp the provider-cap actor reads (best-effort, always exits 0); status --session <id> reads the stamp against the transcript's own boundary. The hook calls the binary directly.",
    "capabilities": "One harness's config-independent capability contract: <harness> [--json] prints map_version, harness, then that harness's table; an unknown harness exits 2 naming the declared list.",
    "target-family": "Merge-posture family test: --message <m> prints family when the message's first token is a /target-family spelling, other otherwise; exit 0 either way.",
    "permission-tokens": "The permission-posture owner: payload {provider, mode, substrate} in, the {mappable, tokens} answer out; invoked by the spawn seam via fno.rust_binary.command_call, not `fno agents` routing.",
    "sandbox-probe": "The codex sandbox verdict owner: payload {cwd, mode} in, the {verdict, blocked, note} envelope out; the spawn seam calls the same probe in process.",
    "mail-teach": "The fmail read-command lesson state: (--session <id> | --self) [--transcript <path>] [--mark] prints the teaching line when the lesson is due and stamps it taught; the hook carriers invoke the binary directly.",
}

#: The Rust-only commands the In-N-Out menu advertises. Every other
#: :data:`RUST_ONLY_COMMAND_HELP` command is display-hidden - stop-hook / runtime
#: plumbing (``loop-check``/``finalize``/``kill-check``/...) and daemon channel
#: commands a human never types - but stays invocable, listed by ``fno help --all``
#: and each command's own ``--help``. Membership is display-only; it never changes
#: dispatch or the RUST_CLIENT_COMMANDS routing set.
RUST_ONLY_ADVERTISED: frozenset[str] = frozenset({"spawn", "status"})

#: Commands retired at G4: the grid, the WebSocket ``drive`` surface, and
#: the interactive daemon PTY hosting behind ``host``/``promote`` moved to the
#: mux. They are NOT in :data:`RUST_CLIENT_COMMANDS` (no routable client command) and
#: NOT in :data:`RUST_ONLY_COMMAND_HELP` (not advertised in ``--help``), but
#: ``get_command`` still resolves them to a one-line mux pointer that exits
#: non-zero, so a script hitting a retired command gets a helpful error instead of a
#: bare "No such command" no-op (AC5-EDGE). The Rust binary carries the same
#: pointers for a raw ``fno-agents <command>`` / forced-rust call.
RETIRED_COMMAND_POINTERS: dict[str, str] = {
    "grid": "agent panes now live in the mux. Open `fno mux`, or script panes with `fno mux pane ls|read|run|send|wait|kill`.",
    "drive": "drive an agent pane in the mux. Use `fno mux pane send <pane> --raw ...` for keystrokes (without --raw the payload is wrapped in an <fno_mail> envelope), or open `fno mux` and type into the pane.",
    "host": "spawn a mux-hosted agent pane with `fno agents spawn --name <n> --substrate pane`.",
    "promote": "the mux hosts agent panes; spawn one with `fno agents spawn --name <n> --substrate pane`.",
}


def runtime_mode() -> str:
    """Resolve the runtime selection from ``FNO_AGENTS_RUNTIME``.

    Returns one of:

    - ``"rust"``   -- the caller forced the Rust binary (hard error if absent).
    - ``"python"`` -- the caller forced the Python dispatch (binary untouched).
    - ``"auto"``   -- the default (unset or any unrecognized value): Rust is the
      runtime for the commands it implements when an *installed* binary is present,
      and Python serves every other case.
    """
    val = os.environ.get(RUNTIME_ENV, "").strip().lower()
    if val == "rust":
        return "rust"
    if val == "python":
        return "python"
    return "auto"


#: Commands other than `spawn` that build a worker argv and therefore need the
#: writable-dir grant published before the route/fork.
_WORKER_DIR_COMMANDS = ("resume", "ask", "revive", "wake")

def _export_worker_dirs_at_seam(args: "Sequence[str]") -> None:
    """Publish fno's computed writable-dir set for the Rust spawn route.

    Best-effort: a failure here must never break a spawn, and the worst case is
    the pre-existing behavior (no grant), not a broken launch.
    """
    try:
        from pathlib import Path

        from fno.agents.writable_dirs import export_worker_writable_dirs

        from fno.agents.spawn_defaults import _flag_value

        # `_flag_value` stops at the `--argv` payload and the bare `--` seed
        # fence, and knows the `-c` short spelling plus the glued `-c<value>`
        # form click accepts. A hand-rolled scan missed both: it read `--cwd`
        # only, so `spawn -c /other/repo` computed the grant for the CALLER's
        # directory, and it ran past the fence, so `spawn w -- codex --cwd /x`
        # read a provider token as fno's flag.
        cwd = _flag_value(list(args), "--cwd", "-c")
        export_worker_writable_dirs(Path(cwd) if cwd else Path.cwd(), os.environ)
    except Exception:
        pass  # ponytail: a grant we cannot compute must never block the spawn

def _worktree_policy_pin_at_seam(args: "Sequence[str]") -> dict:
    """The worktree-policy pin for the Rust route, or ``{}``.

    The caller hands it to :func:`route_to_rust`, which merges it into the env
    right before the exec. Only an explicit ``--cwd`` can name a foreign repo."""

    try:
        if os.environ.get("FNO_WORKTREE_POLICY"):
            return {}
        from fno.agents.spawn_defaults import _flag_value
        from fno.rust_binary import call_binary_json

        cwd = _flag_value(list(args), "--cwd", "-c")
        if not cwd:
            return {}
        harness = _flag_value(list(args), "--harness", "-H") or None
        err, payload = call_binary_json(
            "worktree-dispatch-pin",
            [
                "--target", cwd,
                "--caller", os.getcwd(),
                "--harness", harness or "",
            ],
            timeout=30,
        )
        if err is not None or not isinstance(payload, dict):
            return {}
        pin = payload.get("pin") or {}
        receipt = payload.get("receipt")
        if pin and receipt:
            print(receipt, file=sys.stderr)
        return pin
    except Exception:
        return {}  # ponytail: a pin we cannot compute must never block the spawn

def _args_before_argv(args: Sequence[str]) -> Sequence[str]:
    """The fno-arg head, stopping at the ``--argv`` provider-payload boundary
    and at a bare ``--`` seed fence.

    A payload token (e.g. the child command's own ``--resume``/``--role``) and
    a fenced seed token must never be read as one of our routing flags - both
    click and the Rust client treat everything past the fence as prompt text -
    so every spawn-flag scan operates on this slice.
    """
    out: list[str] = []
    for a in args:
        if a in ("--argv", "--"):
            break
        out.append(a)
    return out


def _is_role_bearing_spawn(command: str, args: Sequence[str]) -> bool:
    """True for a ``spawn`` carrying ``--role`` (long form only).

    The client resolves the role route (``spawn_route::role_seam``); the
    seam's account pick reads this to leave a role spawn unpicked.
    """
    if command != "spawn":
        return False
    return _has_flag(args, longs=("--role",))


def _has_flag(
    args: Sequence[str], short: str = "", longs: tuple[str, ...] = ()
) -> bool:
    """True if the pre-argv args carry ``short`` or any of ``longs``.

    Covers every Click spelling: a short flag as ``-X``, ``-X=V``, and the
    attached ``-XV`` (the form per-detector copies kept missing, so a
    ``--substrate bg`` spawn spelled that way routed to the Rust binary and
    exited ``unknown flag``); a long flag as ``--foo`` and ``--foo=V``. One
    matcher for every routing detector so the attached-short blind spot cannot
    recur one flag at a time.
    """
    for a in _args_before_argv(args):
        if short and a.startswith(short):
            return True
        if any(a == lng or a.startswith(lng + "=") for lng in longs):
            return True
    return False


_RM_AUDIT_VALUE_FLAGS = frozenset(
    {"--audit-actor", "--audit-reason", "--audit-request-id", "--audit-reclaimed-bytes"}
)


def _rm_target_name(args: Sequence[str]) -> Optional[str]:
    """Find rm's name without mistaking an internal audit value for it."""
    tokens = list(_args_before_argv(args))
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token in _RM_AUDIT_VALUE_FLAGS:
            index += 2
            continue
        if any(token.startswith(flag + "=") for flag in _RM_AUDIT_VALUE_FLAGS):
            index += 1
            continue
        if token.startswith("-"):
            index += 1
            continue
        return token
    return None


def _gate_rm_at_seam(args: Sequence[str]) -> bool:
    """Warn that ``rm`` forfeits a resume handle; return False only on a declined prompt.

    Runs at the routing seam so the ONE implementation covers every route:
    ``rm`` is in :data:`AUTO_ROUTE_COMMANDS`, so an installed binary serves it and
    the Python twin that once lived in ``dispatch`` is gone (one command, one
    implementation). A notice below the seam would only reach the no-binary
    refusal this module also owns.

    A ``--help`` on the command, or a call with no name, is left entirely alone:
    the real parser owns those, and warning about a reap that is not about to
    happen would be noise. Any failure inside the notice degrades to True -- an
    advisory must never be what stops a reap.
    """
    name = _rm_target_name(args[1:])
    if name is None or _has_flag(args[1:], "", ("--help",)) or "-h" in args[1:]:
        return True
    try:
        from fno.agents.rm_notice import warn_and_confirm

        return warn_and_confirm(name, force=_has_flag(args[1:], "-F", ("--force",)))
    except Exception:
        return True


def _is_resume_bearing_spawn(command: str, args: Sequence[str]) -> bool:
    """True for a ``spawn`` carrying ``--resume`` / ``-r``.

    No longer a route: the Rust spawn grammar knows ``--resume`` and refuses
    a resume-bearing spawn itself with the retired-axis line, so the flag
    never reaches ``cmd_spawn``. The predicate now only reports the flag to
    the ``spawn-axes`` node-seed derivation. (``-r`` is matched too for a
    pre-normalization raw argv.)
    """
    if command != "spawn":
        return False
    return _has_flag(args, "-r", ("--resume",))


def _is_route_bearing_spawn(command: str, args: Sequence[str]) -> bool:
    """True for a ``spawn`` carrying ``--route`` or its retired spelling
    ``--provider``/``-P``. The client resolves the route; the seam's account
    pick reads this to leave a routed spawn unpicked."""
    if command != "spawn":
        return False
    return _has_flag(args, "-P", ("--route", "--provider"))


def _spawn_flag_value(args: Sequence[str], *names: str) -> Optional[str]:
    """The value of the first of ``names`` present in ``args`` (``--f v``,
    ``--f=v``, or the glued short ``-Hv``), or None. Scans only the fno-arg
    head, like every other flag scan here, so a payload token can never
    masquerade as our flag. Delegates to the ONE value-flag scanner
    (:func:`fno.agents.spawn_defaults._flag_value`): a local copy here missed
    the glued short form, so ``-Hgrok`` routed to the Rust client, whose own
    parser matches ``-H`` as an exact token only."""
    from fno.agents.spawn_defaults import _flag_value

    return _flag_value(args, *names)


def _pick_account_at_seam(args: Sequence[str]) -> list[str]:
    """Inject a headroom-picked ``--account`` for a spawn that named none.

    Runs at the seam because a bg/headless spawn auto-routes to the Rust
    client through an ``exec``, so neither Python spawn seam is ever reached. A
    picker wired only there would leave every such worker on the ambient -
    possibly exhausted - account while ``pick_on_launch`` read enabled.
    Injecting the flag here is the one edit BOTH runtimes see, and each then
    resolves the account's overlay exactly as for an explicit ``--account``.

    Six spawns are left alone: one that already named an account (explicit
    intent always wins), one carrying ``--role`` or ``--route``/``--provider``
    (the CLI refuses ``--account`` alongside either, because the route's
    ANTHROPIC_* would override the account's CLAUDE_CONFIG_DIR and mis-bill),
    one carrying ``--dispatch-account`` (a quota cutover already selected the
    destination account, and a second pick merges two overlays the same way),
    one carrying ``--resume`` (see below), and one pinned to a non-claude
    harness (``--account`` is claude-only).
    """
    out = list(args)
    # This function owns the provenance carrier: drop any previous spawn's
    # decision before every guard, so a stale "config" never outlives its pick.
    os.environ.pop(LAUNCH_ACCOUNT_SOURCE_ENV, None)
    if _spawn_flag_value(out, "--account") is not None:
        return out
    if _is_role_bearing_spawn("spawn", out) or _is_route_bearing_spawn("spawn", out):
        return out
    if _is_resume_bearing_spawn("spawn", out):
        # A revive continues an EXISTING transcript, and that transcript lives
        # under the config dir it was created in - so an injected --account
        # points CLAUDE_CONFIG_DIR at a directory where the uuid does not exist.
        # It also collides with the recorded-route restore in `dispatch_spawn`,
        # which reads any --account as operator intent and refuses the pair:
        # the operator would be told to drop a flag they never typed. Nothing to
        # pick - the transcript already decided where it lives.
        return out
    if _has_flag(out, longs=("--dispatch-account",)):
        # A cutover already SELECTED its account, and picking a second one here
        # merges two overlays: the destination's config_dir with the picked
        # account's api key, which is the mis-bill this function already refuses
        # for --route. Nothing to pick - the selector decided.
        return out
    harness = _spawn_flag_value(out, "--harness", "-H")
    if harness not in (None, "", "claude"):
        return out
    try:
        from fno.agents.dispatch import pick_account_id

        picked = pick_account_id()
    except Exception:  # noqa: BLE001 - picking is advisory; never block a spawn
        return out
    if not picked:
        return out
    # Insert BEFORE any --argv payload or bare -- seed fence: appended at the
    # end, the flag lands in the payload/seed (corrupting a fenced seed and
    # never pinning the account).
    boundary = len(out)
    for i, a in enumerate(out):
        if a in ("--argv", "--"):
            boundary = i
            break
    # Name the injection for what it is: without this carrier the Rust mint
    # stamps a config pick as "caller" - the misread this column ends.
    os.environ[LAUNCH_ACCOUNT_SOURCE_ENV] = "config"
    return [*out[:boundary], "--account", picked, *out[boundary:]]


#: Commands whose Rust-client handling launches a harness child that would inherit
#: the exec'd client's env verbatim. ``worker_environment`` scrubs identity for
#: every Python adapter seam, but a spawn/ask that execs the binary crosses no
#: Python seam, and the Rust spawn arms only scrub model vars
#: (model_env_scrub), so without this the identity floor is one of two
#: reachable implementations - decorative for the default installed-binary
#: route. Direct ``fno-agents`` invocations still need the Rust-side twin.
_WORKER_LAUNCHING_COMMANDS = frozenset({"spawn", "ask"})


def _scrub_ambient_identity_at_exec(command: str) -> None:
    """Drop ambient identity markers from ``os.environ`` right before the exec,
    so the Rust client (and the worker it launches) cannot inherit them.

    Only the exec branches call this: the process is replaced immediately, so
    the mutation never reaches a Python code path (parent-edge capture in
    ``cmd_spawn`` still sees the markers on the fall-through route). Scoped to
    worker-launching commands because the client itself resolves identity for
    other commands and must keep seeing the ambient markers."""
    if command not in _WORKER_LAUNCHING_COMMANDS:
        return
    from fno.harness_identity import scrub_ambient_identity

    scrub_ambient_identity()


#: d-fe66560a keeps the binary's action list shrink-only, so these read
#: leaves have no action of their own: the router rewrites each into an
#: argument of the `status` action before the exec.
_STATUS_ARG_LEAVES = {
    "capabilities": "--capabilities",
    "target-family": "--target-family",
    "register": "--register",
    "name": "--name",
    "whoami": "--whoami",
    "registry-repair": "--registry-repair",
    "heal-token": "--heal-token",
    "pane-identity": "--pane-identity",
    "liveness": "--liveness",
}


def _rust_leaf_argv(args: "Sequence[str]") -> "list[str]":
    """The binary argv for a routed leaf: plain passthrough, except a leaf in
    :data:`_STATUS_ARG_LEAVES` becomes ``status <flag> ...``."""
    if args:
        flag = _STATUS_ARG_LEAVES.get(args[0])
        if flag is not None:
            return ["status", flag, *args[1:]]
    return list(args)


def route_to_rust(
    args: Sequence[str],
    *,
    binary: Optional[Path] = None,
    env_pin: Optional[dict] = None,
    _exec: Callable[..., None] = os.execv,
    _resolve: Callable[[], Optional[Path]] = rust_binary.resolve_binary,
    _stderr: Optional[IO[str]] = None,
) -> NoReturn:
    """Exec ``fno-agents`` with ``args`` (the command + everything after ``fno agents``).

    On success ``os.execv`` replaces the current process, so this never returns.
    Both failure modes raise ``SystemExit(127)`` with an actionable message
    rather than letting a raw "binary not found" / exec error surface as a
    misleading spawn failure (design open-question #11):

    - the binary is absent (``_resolve`` returns ``None``); or
    - the binary resolves but ``os.execv`` fails (``OSError``: TOCTOU delete,
      lost execute bit, incompatible arch, ``ETXTBSY``, ...).

    When ``binary`` is supplied (the ``auto`` path has already resolved an
    installed binary), it is used directly and ``_resolve`` is skipped -- so the
    happy default path never double-resolves and never spuriously hits the
    missing-binary exit. When ``binary`` is ``None`` (the forced ``=rust`` path),
    ``_resolve`` runs and a missing binary is the hard 127 error.

    /d-450caaeb: this door carries NO carrier logic. The TARGET_NO_MERGE
    verdict for a rust-lane spawn is computed inside the binary itself
    (``merge_posture::apply_env_from_message`` in the spawn handler).

    The ``_exec`` / ``_resolve`` / ``_stderr`` hooks exist purely so the decision
    logic is unit-testable without actually replacing the test process.
    """
    err = _stderr if _stderr is not None else sys.stderr
    if binary is None:
        binary = _resolve()
    if binary is None:
        print(
            f"fno agents: {RUNTIME_ENV}=rust is set but the '{rust_binary.BINARY_NAME}' binary "
            f"was not found (looked at ${rust_binary.BINARY_ENV}, in the bundled "
            "wheel dir, beside the launcher, on PATH, and in the cargo dev target; "
            "a file present but not executable is also skipped - try `chmod +x`). "
            "Get it via `pip install fno` (bundled wheel), `cargo install fno-agents`, "
            f"or `cargo build --release -p fno-agents` plus "
            f"`export {rust_binary.BINARY_ENV}=<path>`.",
            file=err,
        )
        raise SystemExit(BIN_NOT_FOUND_EXIT)
    if env_pin:
        # Before the exec: the replacement inherits it; tests stubbing _exec never export.
        os.environ.update(env_pin)
    argv = [str(binary), *_rust_leaf_argv(args)]
    try:
        _exec(str(binary), argv)
    except OSError as exc:
        # execv raises (never returns) on failure. Convert to the same legible
        # surface as the missing-binary case instead of a raw traceback.
        print(f"fno agents: failed to exec '{binary}': {exc}", file=err)
        raise SystemExit(BIN_NOT_FOUND_EXIT) from exc
    # Only reached if a stubbed _exec returns (real os.execv never does).
    raise SystemExit(1)  # pragma: no cover


def _make_rust_only_command(
    command_name: str, help_text: str, *, hidden: bool = False
) -> "click.Command":
    """A placeholder Click command for a Rust-only command, used for help + fallback.

    The happy path never runs this body: :meth:`_AgentsRuntimeGroup.make_context`
    execs the ``fno-agents`` binary before Click resolves the sub-command whenever
    the command auto-routes and an installed binary is present. This command exists
    so the command (a) appears in ``fno agents --help`` with a description and (b)
    degrades to a legible message instead of a bare "No such command" when it is
    reached -- i.e. under ``FNO_AGENTS_RUNTIME=python`` (no Python implementation
    exists for these commands) or in a checkout with no *installed* binary.

    ``hidden`` keeps the command invocable but off the advertised
    ``fno agents --help`` listing - the display-only counterpart of the Python
    commands' ``@agents_app.command(..., hidden=True)``.
    """
    import click

    @click.command(
        name=command_name,
        help=f"{help_text} (Rust runtime).",
        hidden=hidden,
        # Don't choke on the command's real flags before we print the message --
        # we never act on them here, but a bare "no such option" would bury it.
        context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
        add_help_option=True,
    )
    def _placeholder() -> NoReturn:
        if runtime_mode() == "python":
            print(
                f"fno agents {command_name}: no Python implementation -- this command runs only on "
                f"the '{rust_binary.BINARY_NAME}' Rust runtime. Unset {RUNTIME_ENV} (auto) with the "
                f"binary installed, or set {RUNTIME_ENV}=rust to use a local cargo build.",
                file=sys.stderr,
            )
        else:
            print(
                f"fno agents {command_name}: requires the '{rust_binary.BINARY_NAME}' Rust runtime, which was "
                "not found (bundled wheel dir, launcher sibling, PATH -- the default "
                f"lookup deliberately ignores ${rust_binary.BINARY_ENV}). Get it via "
                f"`pip install fno` (bundled), `cargo install fno-agents`, or set "
                f"{RUNTIME_ENV}=rust (which does honor ${rust_binary.BINARY_ENV}) to use "
                "a local `cargo build --release -p fno-agents`.",
                file=sys.stderr,
            )
        raise SystemExit(BIN_NOT_FOUND_EXIT)

    return _placeholder


def _make_retired_command(command_name: str, pointer: str) -> "click.Command":
    """A Click command for a command retired at G4: print a one-line mux
    pointer to stderr and exit non-zero, never a silent no-op (AC5-EDGE)."""
    import click

    @click.command(
        name=command_name,
        help=f"(retired at G4) {pointer}",
        context_settings={"ignore_unknown_options": True, "allow_extra_args": True},
        add_help_option=True,
    )
    def _retired() -> NoReturn:
        print(f"fno agents {command_name} was retired at G4: {pointer}", file=sys.stderr)
        raise SystemExit(2)

    return _retired


def make_agents_group_cls() -> type:
    """Build the TyperGroup subclass that short-circuits to the Rust binary.

    Returned (not module-level) so importing this module never imports
    ``typer.core`` unless the agents sub-app is actually constructed - keeps the
    lazy-import startup budget intact.
    """
    from fno._lazy_group import LazyTypeGroup, _LazyStub

    class _AgentLazyStub(_LazyStub):
        def list_commands(self, ctx):  # type: ignore[no-untyped-def]
            real = self._load_real()
            if not hasattr(real, "list_commands"):
                return []
            return list(real.list_commands(ctx))

        def get_command(self, ctx, name):  # type: ignore[no-untyped-def]
            real = self._load_real()
            if not hasattr(real, "get_command"):
                return None
            return real.get_command(ctx, name)

    class _AgentsRuntimeGroup(LazyTypeGroup):
        """Intercept agent commands before Typer parses their subcommand."""

        def __init__(self, *args, **kwargs):  # type: ignore[no-untyped-def]
            super().__init__(
                *args,
                lazy_subcommands=FOLDED_AGENT_SUBCOMMANDS,
                **kwargs,
            )

        def _make_stub(self, name):  # type: ignore[no-untyped-def]
            import_path, short_help, options = FOLDED_AGENT_SUBCOMMANDS[name]
            return _AgentLazyStub(
                name=name,
                help=short_help,
                import_path=import_path,
                hidden=bool(options.get("hidden", False)),
            )

        def list_commands(self, ctx):  # type: ignore[no-untyped-def]
            """Keep help complete without registering Rust commands in Typer."""
            base = list(super().list_commands(ctx))
            seen = set(base)
            return base + [v for v in RUST_ONLY_COMMAND_HELP if v not in seen]

        def get_command(self, ctx, name):  # type: ignore[no-untyped-def]
            """Resolve Python commands or synthesize Rust-only help commands."""
            cmd = super().get_command(ctx, name)
            if cmd is not None:
                return cmd
            if name in RUST_ONLY_COMMAND_HELP:
                return _make_rust_only_command(
                    name,
                    RUST_ONLY_COMMAND_HELP[name],
                    hidden=name not in RUST_ONLY_ADVERTISED,
                )
            if name in RETIRED_COMMAND_POINTERS:
                return _make_retired_command(name, RETIRED_COMMAND_POINTERS[name])
            from fno.tombstones import refuse, tombstone_for

            full = f"agents {name}"
            if tombstone_for(full) is not None:
                raise refuse(full)
            return None

        def make_context(self, info_name, args, parent=None, **extra):  # type: ignore[no-untyped-def]
            if args and args[0] not in ("-h", "--help"):
                command = args[0]
                if command == "spawn":
                    # The client owns the whole spawn seam (spawn_seam.rs): the
                    # raw argv goes to it in every runtime mode.
                    route_to_rust(list(args))  # execs; does not return
                if command in _WORKER_DIR_COMMANDS:
                    _export_worker_dirs_at_seam(args)
                    args = _pick_account_at_seam(args)
                elif command == "rm":
                    if not _gate_rm_at_seam(args):
                        raise SystemExit(RM_DECLINED_EXIT)

                mode = runtime_mode()
                if mode == "rust":
                    _pin = _worktree_policy_pin_at_seam(args)
                    _scrub_ambient_identity_at_exec(command)
                    route_to_rust(list(args), env_pin=_pin or None)  # execs; does not return
                elif mode == "auto" and command in AUTO_ROUTE_COMMANDS:
                    binary = rust_binary.resolve_installed_binary()
                    if binary is not None:
                        _pin = _worktree_policy_pin_at_seam(args)
                        _scrub_ambient_identity_at_exec(command)
                        route_to_rust(list(args), binary=binary, env_pin=_pin or None)  # execs
                    # else: no installed binary -> Python dispatch below.
                # mode == "python", or no installed binary -> Python dispatch below.
            return super().make_context(info_name, args, parent=parent, **extra)

    return _AgentsRuntimeGroup
