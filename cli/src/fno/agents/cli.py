"""`fno agents` Typer subapp.

``ask`` resolves its recipient and execs the Rust client binary (the Python
adapters are ported, parity frozen); ``list`` and ``logs`` are live.
"""

from __future__ import annotations

import enum
import json
import os
import sys
from pathlib import Path
from typing import Optional

import typer

from fno.agents.rust_runtime import make_agents_group_cls

agents_app = typer.Typer(
    name="agents",
    help=(
        "Cross-CLI agent lifecycle (claude / codex / gemini / cursor-agent): "
        "spawn / watch / list / logs / stop. "
        "To message a peer, use `fno agents mail send <name>` (or the `/mail` skill)."
    ),
    no_args_is_help=True,
    # Default Rust runtime (Phase 6 W6 / cv-d28b266a): by default this group
    # execs the installed `fno-agents` binary for the commands it implements, and
    # falls back to the Python dispatch below otherwise. FNO_AGENTS_RUNTIME=rust
    # forces the binary; =python forces this Python path. See rust_runtime.py.
    cls=make_agents_group_cls(),
)

class AgentStatusFilter(str, enum.Enum):
    """Served-activity words accepted by ``list --status`` (AC7): what the
    session is DOING, never a `live` token. See fno.agents.reachability."""

    writing = "writing"
    quiet = "quiet"
    parked = "parked"
    refused = "refused"
    orphaned = "orphaned"
    unknown = "unknown"


class AgentProgressFilter(str, enum.Enum):
    """Progress-axis values accepted by ``list --progress``.

    A SECOND axis beside ``--status``: the status word answers "what is it
    doing right now"; progress answers "is it advancing, awaiting the
    operator, parked, or refused". The two filter independently.
    """

    advancing = "advancing"
    awaiting_operator = "awaiting-operator"
    parked = "parked"
    refused = "refused"
    unknown = "unknown"


def _spawn_guard_decision(
    node_id: str,
    holder: str,
    *,
    ttl: str = "3m",
    no_reserve: bool = False,
    cwd: str | None = None,
    handover_holder: str | None = None,
) -> tuple[dict[str, object], int]:
    """The family-2 pre-birth verdict, decided by the Rust guard (spawn_guard.rs).

    ``handover_holder`` also takes ``node:<id>`` for the launch window, and
    ``no_reserve`` makes the call a pure probe. This process's pid rides the
    payload, so the keys name the spawner and not the short-lived command
    process. A refusal carries the rendered stderr line as ``refusal_line``.
    """
    from fno.rust_binary import CommandUnavailable, command_call

    try:
        # With store.remote_url set each claim read is a round trip that may
        # wait 15s on the primary, so the 30s local-resolver bound is too short.
        answer = command_call(["backlog", "advance", "--stdin"], {
            "op": "spawn-guard", "node_id": node_id, "holder": holder, "ttl": ttl,
            "no_reserve": no_reserve, "cwd": cwd, "handover_holder": handover_holder,
            "pid": os.getpid(),
        }, timeout=90)
    except CommandUnavailable as exc:
        # Fail closed: an unanswered guard must never read as dispatchable.
        import subprocess

        timed_out = isinstance(exc.__cause__, subprocess.TimeoutExpired)
        cause = (
            "; the guard reads node claims on the shared primary when "
            "store.remote_url is set, so a slow primary is the likely cause"
            if timed_out else ""
        )
        detail = f"spawn guard unavailable: {exc}{cause}; not dispatching"
        line = (
            f"node dispatch refused: node={node_id} verdict=error "
            f"reason={'guard-timeout' if timed_out else 'error'}; "
            f"no worker launched detail={detail!r}"
        )
        return {"verdict": "error", "detail": detail, "refusal_line": line}, 3
    verdict = dict(answer.get("verdict") or {})
    if verdict.get("verdict") != "dispatchable":
        verdict["refusal_line"] = answer.get("line") or ""
    return verdict, int(answer.get("exit") or 0)


def _resolve_dispatch_workdir(cwd: str | None, fresh: bool, here: bool) -> Path:
    """Worker launch dir honoring --cwd > --here (caller) > default canonical.

    Mirrors the Rust client's ``effective_worker_cwd`` precedence.
    inverted the default (was 's caller-cwd): a spawn with NO explicit
    cwd source now resolves to the canonical (main) checkout, so the identical
    command behaves the same regardless of where the launcher happens to stand.
    ``--here``/``--in-place`` is the explicit opt-in to keep the caller's cwd.
    ``--fresh`` survives as an accepted no-op alias (the default already resolves
    canonical). A canonical that lands on the caller's own dir is a no-op (no
    redirect note). Only the Python fallback runtime reaches this -- when an
    installed binary auto-routes the command, the Rust client owns the identical
    precedence.
    """
    del fresh  # accepted no-op alias: the default already resolves canonical.
    if cwd:
        return Path(cwd).resolve()
    caller = Path(os.getcwd()).resolve()
    if here:
        return caller
    from fno.paths import resolve_canonical_repo_root

    # Best-effort: any resolution error (missing git, odd environment) falls
    # back to the caller cwd, the safe side, rather than crashing the dispatch.
    try:
        canonical = resolve_canonical_repo_root().resolve()
    except Exception:
        return caller
    if canonical != caller:
        # Never silent: the redirect note fires on every actual move, default
        # path included.
        print(
            f"fno agents: dispatching from canonical main (default) ({canonical}); "
            "pass --here to stay in this worktree",
            file=sys.stderr,
        )
    return canonical




@agents_app.command("watch")
def cmd_watch(
    name: str = typer.Argument(..., help="Agent name (a held stream-json thread)."),
    poll_interval: float = typer.Option(
        0.25, "--interval", "-i", help="Seconds between frame polls."
    ),
) -> None:
    """Observe a held stream-json thread's turns in real time (read-only).

    Renders delivered -> streaming -> reply -> complete per turn by polling the
    worker's frame log. Ctrl-C to stop. Exits 0 when the thread is no longer
    live, 1 when no live worker exists, 2 when the name is unknown. The watch
    is native (crates/fno-agents/src/watch.rs); with an installed binary the
    runtime router execs it before this function runs, so this front keeps
    only its signature for --help and refuses without one.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (name, poll_interval)
    refuse_without_binary("watch")


@agents_app.command("role", hidden=True)
def cmd_role(
    handle: str = typer.Argument(
        "",
        help=(
            "Existing registered session handle to role in place, or the "
            "current successor when --reclaim is run from an attended shell."
        ),
    ),
    scopes: list[str] = typer.Option(
        [],
        "--scope",
        help=(
            "Territory to grant. Repeat for a multi-project portfolio or a "
            "set of epics; the role level is derived and cannot be supplied."
        ),
    ),
    reclaim: bool = typer.Option(
        False,
        "--reclaim",
        help=(
            "Return the current holder's role to its recorded grantor without "
            "creating a session."
        ),
    ),
) -> None:
    """Role an existing session from an attended shell, from an agent whose
    own role strictly contains the requested scope, or add to your own
    epic-set role an epic your session created.

    Run `fno agents register` inside the target session, then run this command
    with its printed handle. Same-scope succession stays on the spawn-time
    transfer path. A row already holding a role is re-scoped rather
    than refused: the new territory replaces the old in one atomic write, the
    level is derived from the new scope, the registry records the actual
    grantor, and the receipt reports what was vacated. `--reclaim` returns a
    transferred role to its recorded grantor and never creates a session.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (handle, scopes, reclaim)
    refuse_without_binary("role")


# The team command moved to fno.agents.team (file budget); the
# composition stays on the agents app here.
from fno.agents.team import register_team_command  # noqa: E402

register_team_command(agents_app)




@agents_app.command("name", hidden=True)
def cmd_name(
    prefix: Optional[str] = typer.Argument(None, help="Legacy operation prefix (target|think|...); omit with --command."),
    node_id: Optional[str] = typer.Argument(None, help="Full backlog node id; never abbreviated."),
    slug: str = typer.Option("", "--slug", help="Human-readable tail; the only expendable part."),
    qualifier: str = typer.Option("", "--qualifier", help="Lifecycle reason, e.g. retro."),
    discriminator: str = typer.Option("", "--discriminator", help="Uniqueness token; never shaved."),
    source: str = typer.Option("", "--source", help="Dispatch source code; omit when attended."),
    command: str = typer.Option("", "--command", "--verb", help="Command code (t|bp|r|th|f) or a work command the bridge maps."),  # verb-wording: allow (old flag, one release)
) -> None:
    """Mechanical bridge to the canonical agent-name owner, for shell dispatchers.

    The Rust client owns the bridge (one name on stdout; exit 3 is the naming
    refusal, 2 a usage error) and reads the model from $FNO_AGENTS_NAME_MODEL;
    with an installed binary the runtime router execs it before this function
    runs. There is no Python name left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (prefix, node_id, slug, qualifier, discriminator, source, command)
    refuse_without_binary("name")


# `rename` moved to the Rust client (`agent.rename` over the daemon RPC; rust_runtime's router
# entry resolves it). The retask transaction is native too: the empty-argv rename payload door
# in fno-agents runs it, and run_retask is the thin front that builds the payload.


@agents_app.command("retask", hidden=True)
def cmd_retask(
    worker: str = typer.Argument(..., help="Blueprint worker registry label or session id."),
    node: str = typer.Option(..., "--node", help="Target backlog node."),
    model: Optional[str] = typer.Option(None, "--model"),
    effort: Optional[str] = typer.Option(None, "--effort"),
    json_out: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Retask one finished worker (pane or thread) onto the node's next command."""
    from fno.agents.retask import run_retask

    try:
        receipt = run_retask(
            worker,
            node=node,
            model=model,
            effort=effort,
        )
    except (OSError, RuntimeError, ValueError) as exc:
        typer.echo(f"agents retask: {exc}", err=True)
        raise typer.Exit(code=2)
    if json_out:
        typer.echo(json.dumps(receipt))
    else:
        # Same JSON either way (sorted keys are the only difference): the
        # receipt is machine-parsed by the lead loop, so there is no human
        # rendering to switch to.
        typer.echo(json.dumps(receipt, sort_keys=True))
    if receipt.get("status") != "retasked":
        raise typer.Exit(code=1)


@agents_app.command("spawn-guard", hidden=True)
def cmd_spawn_guard(
    node_id: str = typer.Argument(
        ..., help="Backlog node id; the node:<id> claim is probed (Guard 1)."
    ),
    holder: str = typer.Option(
        ...,
        "--holder",
        help=(
            "Reservation holder string. On a `dispatchable` verdict the command "
            "acquires dispatch:<id> for this holder; the caller releases it on a "
            "spawn failure and lets it TTL-expire on success."
        ),
    ),
    ttl: str = typer.Option("3m", "--ttl", help="TTL for the dispatch:<id> reservation (Guard 2)."),
    no_reserve: bool = typer.Option(
        False,
        "--no-reserve",
        help=(
            "Run Guard 1 (the node-claim probe) ONLY and never acquire the "
            "dispatch:<id> reservation. Side-effect-free; for a --dry-run / "
            "read-only verdict."
        ),
    ),
    cwd: str | None = typer.Option(
        None,
        "--cwd",
        help="Node project root for project-local failure policy and defer.",
    ),
    json_output: bool = typer.Option(
        False, "--json", "-J", help="Emit the verdict as a JSON object."
    ),
) -> None:
    """Shared bg-dispatch guard: the single source of truth for the dispatch mutex.

    Runs Guard 1 (the ``node:<id>`` claim probe, fail-closed) then Guard 2 (the
    create-only ``dispatch:<id>`` reservation) in one process, so the
    probe-then-reserve window is no wider than the two ``fno agents claim`` shell-outs it
    replaces. Both ``/target bg`` (``dispatch-node.sh``) and ``/agent spawn``
    (``spawn.sh``) call this so the two can never disagree about whether a node is
    dispatchable.

    Emits ONE verdict on stdout (a ``verdict=<v> key=value`` line, or a ``--json``
    object) in ``{dispatchable, already-running, unknown, refused, corrupted, error}``:

    \b
    - dispatchable    node free/stale. On a reserving call ``dispatch:<id>`` is
                      now held by ``--holder`` (the line carries reservation_key +
                      reservation_holder); under ``--no-reserve`` no reservation is
                      taken.
    - already-running a live ``node:<id>`` claim with a target init behind it
                      (reason=live-claim, holder=<owner>), a held claim with NO
                      target init behind it (reason=unproven-claim: a launch
                      window, or a hand ``claim acquire`` - a holder exists and
                      a worker is unproven),
                      a suspect claim (reason=suspect-claim: TTL-unexpired dead pid,
                      a respawned worker - the caller maps this to skipped-contested,
                      ), a worker ROW on the node while the claim itself does
                      not hold it (reason=worker-row, worker=<names>: the receipt
                      names the row and no claim holder, because peeking and
                      stopping that worker is what frees the node - a release
                      frees nothing), OR a racing dispatcher already holds
                      ``dispatch:<id>`` (reason=reservation-held). No reservation
                      acquired.
    - unknown         the worked authority could not answer (the roster read
                      failed), so the guard cannot say whether the node is
                      running (reason=worked-authority-unavailable; detail
                      names the reader that failed). This is NOT occupancy:
                      retry. No reservation acquired.
    - refused        the durable dead-dispatch limit blocked another birth
                      (reason=auto-deferred|defer-failed). No reservation acquired.
    - corrupted       the ``node:<id>`` claim is corrupted; launch nothing.
    - error           the claim probe failed or the reservation could not be
                      acquired (fail-closed); launch nothing.

    Exit 0 for every answered verdict (incl. already-running, unknown and
    corrupted). Exit non-zero ONLY for a usage error or a fail-closed guard
    error (verdict=error), so a stale ``fno`` without this command (Typer "No such
    command") also fails closed in the caller.
    """
    obj, exit_code = _spawn_guard_decision(
        node_id,
        holder,
        ttl=ttl,
        no_reserve=no_reserve,
        cwd=cwd,
    )
    obj.pop("refusal_line", None)
    if json_output:
        line = json.dumps(obj)
    else:
        parts = [f"verdict={obj['verdict']}"]
        for key, value in obj.items():
            if key == "verdict":
                continue
            # Booleans render lowercase, matching the --json lane. Python's
            # str(True) is "True", and this text line is parsed with sed by
            # shell callers, so a capitalised token would be a second spelling
            # of the same fact for anything that learns to read it.
            if key == "detail":
                parts.append(f'{key}="{value}"')
            elif isinstance(value, bool):
                parts.append(f"{key}={'true' if value else 'false'}")
            else:
                parts.append(f"{key}={value}")
        line = " ".join(parts)
    sys.stdout.write(line + "\n")
    sys.stdout.flush()
    raise typer.Exit(code=exit_code)


@agents_app.command("list")
def cmd_list(
    cwd: str = typer.Option(None, "--cwd", help="Filter by working directory."),
    harness: str = typer.Option(
        None,
        "--harness",
        "-H",
        help="Filter by harness (claude | codex | gemini | cursor-agent).",
    ),
    _provider_tombstone: str = typer.Option(
        None,
        "--provider",
        hidden=True,
        help="Retired: filter by --harness.",
    ),
    status: AgentStatusFilter = typer.Option(
        None, "--status", help="Filter by served activity (writing | quiet | parked | refused | orphaned | unknown); process liveness is the liveness field on fno agents list --json."
    ),
    progress: AgentProgressFilter = typer.Option(
        None,
        "--progress",
        help="Filter by the SECOND axis, progress -- not a finer --status "
        "(advancing | awaiting-operator | parked | refused | unknown).",
    ),
    json_out: bool = typer.Option(False, "--json", "-J", help="Emit JSON regardless of TTY."),
    discovered: bool = typer.Option(
        True,
        "--discovered/--no-discovered",
        help="Include the host-local live-session lane (default on; "
        "--no-discovered skips the ~/.claude/sessions scan).",
    ),
) -> None:
    """List registered agents: ROW NAME SESSION HARNESS MODEL EFFORT PR AGE
    LAST MESSAGE STATUS, JSON when stdout is not a TTY or ``--json`` is passed.
    The JSON envelope's top-level key is ``agents``.

    The ``fno-agents`` client owns this command; the options above document its
    surface. There is no Python list lane left: a missing binary is refused.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    refuse_without_binary("list")


@agents_app.command("sweep", hidden=True)
def cmd_sweep(
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit one JSON row per worker, healthy ones included."
    ),
    deadline: int = typer.Option(
        None, "--deadline",
        help="Seconds of silence that make a worker a finding "
             "(default config.agents.silence_deadline_seconds, else 600).",
    ),
    budget: float = typer.Option(
        None, "--budget",
        help="Wall-clock budget in seconds; rows past it report unread rather "
             "than being dropped (default 20, which is what the daemon tick uses).",
    ),
) -> None:
    """Report workers whose transcripts have gone quiet past their deadline.

    The backstop for a refusal the taxonomy does not recognise. A harness can
    reword "usage limit reached" tomorrow, and a cap can arrive as a hang
    rather than a sentence; a clock closes what a marker list cannot.

    It reads the FULL registry rather than recovery's candidate set, because
    that set drops every non-claude row and every row with no live bg socket -
    so a codex successor spawned by failover is invisible to the very sweep
    that would catch ITS cap.

    It never acts. No stop, no spawn, no claim mutation, no row write. Silence
    has two explanations and a component that ACTS on the wrong one loses work,
    while one that merely reports it raises a false alarm. A worker whose
    transcript age is unknowable emits nothing at all: absence of evidence must
    not become a finding.
    """
    import json as _json

    from fno.agents.sweep import DEFAULT_SWEEP_BUDGET_S, run_sweep

    rows, silent = run_sweep(
        deadline_s=deadline,
        budget_s=DEFAULT_SWEEP_BUDGET_S if budget is None else float(budget),
        # A hand-run report is not a daemon observation, and the dedup memo
        # belongs to the daemon's cadence: a human running this twice wants two
        # answers, not one answer and a silence.
        source="cli",
        dedup=False,
    )
    unread = sum(1 for r in rows if r.unread)

    if json_out or not bool(getattr(sys.stdout, "isatty", lambda: False)()):
        typer.echo(_json.dumps([r.as_dict() for r in rows]))
        return

    if not rows:
        typer.echo("no registered workers")
        return
    for row in rows:
        age = "unknown" if row.age_s is None else f"{row.age_s}s"
        mark = "SILENT" if row.silent else "ok"
        typer.echo(
            f"{row.handle}  [{row.harness}]  {mark:<6} age={age} "
            f"deadline={row.deadline_s}s"
        )
    # Never a silent cap: a truncation nobody can see reads as full coverage.
    tail = f", {unread} unread (budget)" if unread else ""
    typer.echo(f"{silent} silent of {len(rows)}{tail}")


@agents_app.command("discovered-json", hidden=True)
def cmd_discovered_json(
    cwd: str = typer.Option(None, "--cwd", help="Filter discovered rows by cwd."),
    harness: str = typer.Option(
        None, "--harness", help="Filter discovered rows by harness."
    ),
    _provider_tombstone: str = typer.Option(
        None,
        "--provider",
        hidden=True,
        help="Retired: filter by --harness.",
    ),
) -> None:
    """Internal: emit the discovered-live-sessions lane as JSON.

    The real ``fno agents list`` auto-routes to the Rust client, which owns
    the rendered surface; that path shells out to THIS command to merge the P1
    host-local live-session lane. Output is
    ``{"discovered_sessions": [...]}``. Fail-open: any error prints an empty
    lane and exits 0 so ``agents list`` is never broken by discovery.
    """
    import json as _json

    from fno._flag_aliases import refuse_retired_provider

    refuse_retired_provider(_provider_tombstone)

    out: dict = {"discovered_sessions": []}
    try:
        from pathlib import Path as _Path

        from fno.agents import discover as discover_mod
        from fno.agents.registry import load_registry

        try:
            entries = load_registry()
            exclude = {e.short_id for e in entries if e.short_id}
            # Projects-store rows key on full session_id.
            exclude_sids = {e.cc_session_id for e in entries if e.cc_session_id}
        except Exception:  # noqa: BLE001 — discovery never depends on a clean registry
            exclude = set()
            exclude_sids = set()

        rows = [
            s.to_row()
            for s in discover_mod.discover_live_sessions(
                exclude_short_ids=exclude, exclude_session_ids=exclude_sids
            )
            if s.is_alive
        ]
        if harness:
            rows = [r for r in rows if r.get("agent") == harness]
        if cwd:
            try:
                resolved = str(_Path(cwd).resolve())
            except OSError:
                resolved = cwd
            kept = []
            for r in rows:
                rc_raw = r.get("cwd") or ""
                # An empty cwd must NOT resolve to the process cwd and then
                # spuriously match the --cwd filter (gemini review).
                if not rc_raw:
                    continue
                try:
                    rc = str(_Path(rc_raw).resolve())
                except OSError:
                    rc = rc_raw
                if rc == resolved:
                    kept.append(r)
            rows = kept
        out["discovered_sessions"] = rows
    except Exception:  # noqa: BLE001 — fail-open: empty lane, never crash list
        pass
    sys.stdout.write(_json.dumps(out))


@agents_app.command("registry-json", hidden=True)
def cmd_registry_json() -> None:
    """Internal: emit registry rows DAEMON-FREE, with the served liveness pair.

    Hooks need stored role, spawn-edge, and origin fields plus the served
    liveness verdict, derived by the freshness rule. Output is
    ``{"agents": [...]}`` via a client-side registry read. There is no Python
    registry-json left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    refuse_without_binary("registry-json")


@agents_app.command("registry-repair", hidden=True)
def cmd_registry_repair(
    to: int = typer.Option(
        ..., "--to", help="Schema version to roll the file back DOWN to."
    ),
    path: str = typer.Option(
        None, "--path", help="Registry file. Defaults to the process-global one."
    ),
    apply: bool = typer.Option(
        False, "--apply", help="Perform the repair. Omit to preview it."
    ),
) -> None:
    """Internal: roll a poisoned registry's schema_version back down.

    The Rust client owns the repair: it holds the registry lock, refuses unless
    the on-disk version is strictly above ``--to``, refuses if any row carries a
    newer-schema field with a real value, backs the file up, then drops the
    empty unknown keys and replaces atomically. Dry run by default. There is no
    Python registry-repair left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (to, path, apply)
    refuse_without_binary("registry-repair")


#: The not-found exit the lifecycle commands share.
HEAL_TOKEN_MISS_EXIT = 13


@agents_app.command("heal-token", hidden=True)
def cmd_heal_token(
    token: str = typer.Argument(..., help="Session-shaped token (8-hex, UUID, ses_...)."),
    registry: str = typer.Option(
        None,
        "--registry",
        help="Adopt into THIS registry file (default: the configured one).",
    ),
    all_sources: bool = typer.Option(
        False,
        "--all-sources",
        hidden=True,
        help="Resolve against the registry and stores as one namespace.",
    ),
    _cross_project: bool = typer.Option(
        False,
        "--cross-project",
        hidden=True,
        help="Retired: healing is machine-wide; accepted as a silent no-op.",
    ),
) -> None:
    """Internal: adopt the session TOKEN names from its harness store, as JSON.

    The Rust client owns the healer (crates/fno-agents/src/store_heal.rs) and
    the lifecycle commands call it in process. Exit 0 with the row; 13 on a
    miss; 3 with the candidates when ambiguous; 12 when identity evidence is
    unreadable. There is no Python heal-token left: a missing binary is
    refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (token, registry, all_sources, _cross_project)
    refuse_without_binary("heal-token")


@agents_app.command("codex-session-for-pid", hidden=True)
def cmd_codex_session_for_pid(pid: int = typer.Argument(..., help="Pane pid to probe.")) -> None:
    """Internal: resolve a codex pane's session id from its open rollout.

    Wraps ``mux_spawn._codex_session_id_for_pid`` (the pane-tree rollout walk
    already used at spawn time) so the Rust reconcile tick can late-bind a row
    whose spawn-time bind window expired, without a second implementation of
    the walk. Prints ``session_id=<id>`` and exits 0 on an unambiguous match;
    exits 13 with no stdout when the pid is gone, no rollout is open yet, or
    the tree holds more than one distinct session.
    """
    from fno.agents.mux_spawn import _codex_session_id_for_pid

    sid = _codex_session_id_for_pid(pid)
    if not sid:
        raise typer.Exit(code=HEAL_TOKEN_MISS_EXIT)
    sys.stdout.write(f"session_id={sid}\n")
    sys.stdout.flush()


@agents_app.command("logs")
def cmd_logs(
    name: str = typer.Argument(..., help="Agent name (from `fno agents list`)."),
    tail: int = typer.Option(
        100,
        "--tail",
        "-n",
        help="Show only the last N lines of output (default 100; pass 0 for none).",
    ),
    follow: bool = typer.Option(
        False, "--follow", "-f", help="Stream output as the agent emits new lines."
    ),
    json_out: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Emit JSON-Lines (codex/gemini only; Claude is raw passthrough).",
    ),
) -> None:
    """Tail or follow an agent's log output.

    The fno-agents client owns the command; with an installed binary the runtime
    router execs it before this function runs. A tail N slices the last N
    lines, `--follow` streams over the daemon's WebSocket `agent.logs`
    transport, and `--json` is JSON-Lines for codex/gemini rows while claude
    passes through raw with a WARN.

    There is no Python logs left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (name, tail, follow, json_out)
    refuse_without_binary("logs")


@agents_app.command("whoami", hidden=True)
def cmd_whoami(
    json_out: bool = typer.Option(False, "--json", "-J", help="Emit JSON regardless of TTY."),
) -> None:
    """Print THIS mesh worker's own registered name (+ registry enrichment).

    The Rust client owns whoami: identity from ``FNO_AGENT_SESSION``, else the
    active harness's session marker, matched against the registry; read-only.
    Exit 0 when a name resolves, 3 for a session with no mesh identity. There
    is no Python whoami left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = json_out
    refuse_without_binary("whoami")


@agents_app.command("register", hidden=True)
def cmd_register(
    json_out: bool = typer.Option(False, "--json", "-J", help="Emit JSON."),
    delivery_policy: str | None = typer.Option(
        None,
        "--delivery-policy",
        help=(
            "This session's mail delivery policy. 'bus-only' forbids prompt-line "
            "injection: mail to this session never pastes into its input buffer "
            "and always queues durable, surfaced at each turn boundary. 'off' "
            "clears back to the default injectable policy. A delivery-policy "
            "fact, never a liveness verdict. Omitted: leave the row unchanged "
            "(a re-firing SessionStart hook must not clobber a stamp)."
        ),
    ),
) -> None:
    """Join THIS session to the mesh roster so peers can `fno agents mail send` to it.

    The Rust client command owns register (the owned-identity ladder plus the row
    mint under the shared registry lock); with an installed binary the runtime
    router execs it before this function runs. There is no Python register
    left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = delivery_policy
    refuse_without_binary("register")


@agents_app.command("top", hidden=True)
def cmd_top(
    as_json: bool = typer.Option(
        False, "--json", "-J", help="Emit the same rows as JSON (script parity)."
    ),
    show_subagents: bool = typer.Option(
        False,
        "--subagents",
        help="Also list harness-native subagents (sidechain limbs) the census "
        "cannot see: read-only, claude-only, not slot-counted.",
    ),
    show_pane_stats: bool = typer.Option(
        False,
        "--pane-stats",
        help="Append per-pane mux server counters, differenced over the last "
        "two 30s snapshots in the global events journal.",
    ),
) -> None:
    """Show every live worker process - fno-spawned and foreign claude bg
    alike - with pid, tree RSS (MB), served status and node.

    Answered by the Rust census (`fno-agents census --workers`); the `fno`
    front execs it directly, so this shim only serves a Python-front call.
    """
    from fno.agents.rust_runtime import route_to_rust

    flags = [
        flag
        for flag, on in (
            ("--json", as_json),
            ("--subagents", show_subagents),
            ("--pane-stats", show_pane_stats),
        )
        if on
    ]
    route_to_rust(["census", "--workers", *flags])


@agents_app.command("orphans", hidden=True)
def cmd_orphans(
    reap: bool = typer.Option(
        False,
        "--reap",
        help="Kill findings that are BOTH fno-named and older than 10 minutes. "
        "Everything else is reported and left alone.",
    ),
    quiet_unless_new: bool = typer.Option(
        False,
        "--quiet-unless-new",
        help="Print nothing when every finding was already reported by a "
        "previous run. For the SessionStart nudge; a broken scan still speaks.",
    ),
    as_json: bool = typer.Option(
        False, "--json", "-J", help="Emit the same content as JSON."
    ),
) -> None:
    """Report processes that outlived whatever started them, with a control.

    The counterpart to ``hooks/bg-process-guard.py``: the guard refuses a
    process that can never end, this finds the ones that already survived. It
    is the only path-agnostic layer in that design, so a test fixture, a
    non-Claude harness and a leaking hook all land here.

    Before counting anything it plants two orphans of its own, one per arm of
    the attribution predicate, and must find both. When it cannot, it prints
    ``verdict withheld (scan-broken)`` and exits 2 WITHOUT an orphan count: a
    clean machine and a half-blind instrument must never print the same line.
    Break an arm on purpose with ``FNO_ORPHANS_SKIP_PROBE=name|cwd``.

    ``--reap`` kills only what we named ourselves. Attribution is a heuristic,
    and a heuristic must not hold a kill signal.
    """
    # The sweep answers natively (crates/fno-agents/src/orphans.rs, behind the
    # census action as `fno-agents census --orphans`); the Python body is gone.
    from fno.agents.rust_runtime import route_to_rust

    flags = []
    if reap:
        flags.append("--reap")
    if quiet_unless_new:
        flags.append("--quiet-unless-new")
    if as_json:
        flags.append("--json")
    route_to_rust(["census", "--orphans", *flags])


# The pane-identity command moved to fno.agents.pane_identity (file
# budget); the composition stays on the agents app here.
from fno.agents.pane_identity import cmd_pane_identity  # noqa: E402

agents_app.command("pane-identity", hidden=True)(cmd_pane_identity)




@agents_app.command("truth", hidden=True)
def cmd_truth(
    handle: str | None = typer.Argument(
        None, help="Worker handle / short id / session id (as in `fno agents list`)."
    ),
    handles: str | None = typer.Option(
        None,
        "--handles",
        help=(
            "BATCH MODE. Comma-separated handles, answered in ONE process. "
            "Always exits 0: every entry carries its own state and reason."
        ),
    ),
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit a single JSON object instead of a line."
    ),
) -> None:
    """Classify a worker's supervision state from its transcript TAIL.

    done | watching | your-move | working | stalled | unknown. Read-only;
    exits 13 on an unresolvable handle, 0 otherwise. The read is native
    (crates/fno-agents/src/truth_cli.rs, over the daemon's own truth
    reader); with an installed binary the runtime router execs it before
    this function runs, so this front keeps only its signature for --help
    and refuses without one.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = (handle, handles, json_out)
    refuse_without_binary("truth")


def _run_unfinished_report(
    *,
    now: float,
    json_out: bool,
    mail_to: Optional[str] = None,
) -> None:
    """The default watchdog surface: build and publish the unfinished-work
    report. Recovery internals stay behind --apply/--only; this path never
    renders a session verdict as the operator's answer."""
    from fno.agents import unfinished_work as uw
    from fno.agents import watchdog as wd
    from fno.paths import resolve_repo_root

    roots = uw.report_roots() or [Path(resolve_repo_root())]
    snapshot = uw.build_report(roots, now_s=now)

    recipient = mail_to
    if recipient is None:
        try:
            from fno.config import load_settings

            recipient = str(load_settings().recovery.watchdog.mail_to or "")
        except Exception:  # noqa: BLE001 - config read miss means no mail
            recipient = ""

    def _note(line: str) -> None:
        print(line, file=sys.stderr)

    payload = uw.publish_report(
        snapshot, source="manual", now_s=now, mail_to=recipient or "", log=_note
    )
    # The keeper lane rides the default report: each finding names its own
    # clearing command, and the lane here is read-only whatever the flags -
    # collection is `--only keeper --apply-all`, and the row lanes below never
    # see a keeper.
    lane_lines: list[str] = []
    try:
        from fno.agents import keeper_lane

        lane_result = keeper_lane.discover()
        lane_lines = keeper_lane.render(lane_result).splitlines()
        payload["keepers"] = lane_result.to_json()
    except Exception as exc:  # noqa: BLE001 - the report outlives one lane's crash
        lane_lines = [f"keeper lane: crashed: {exc!r}"]
    # The outage instrument rides the default report: open breakers
    # and every refusal reason with its count are part of the operator's
    # answer, and a -J reader must not need --only to learn a lane is down.
    payload["provider_outages"] = wd.measure_provider_outages_safe(now)
    if json_out:
        sys.stdout.write(json.dumps(payload) + "\n")
        sys.stdout.flush()
        return
    typer.echo(uw.snapshot_digest(snapshot))
    for line in lane_lines:
        typer.echo(line)
    for line in wd.provider_outage_lines(payload.get("provider_outages")):
        typer.echo(line)
    for warning in payload["warnings"]:
        print(f"warning: {warning}", file=sys.stderr)


def _watchdog_only_help() -> str:
    from fno.agents import watchdog as wd

    return (
        "DIAGNOSTIC: filter the internal session-verdict table to one verdict "
        f"({'|'.join(sorted(wd.VERDICTS))}). Recovery internals, not the "
        "operator report."
    )


@agents_app.command("watchdog")
def cmd_watchdog(
    json_out: bool = typer.Option(
        False, "--json", "-J", help="Emit the machine-readable payload."
    ),
    apply: bool = typer.Option(
        False,
        "--apply",
        help=(
            "Execute the wake lane only - the one action that cannot destroy "
            "work. A ghost never auto-acts at any level."
        ),
    ),
    apply_all: bool = typer.Option(
        False,
        "--apply-all",
        help=(
            "Execute every lane: wake plus reroute (which stops and respawns "
            "a session), plus keeper collection, which kills an orphaned "
            "keeper process and its hosted children. Row retirement is the "
            "daemon sweep's question (`fno agents reap`). Implies --apply."
        ),
    ),
    only: Optional[str] = typer.Option(
        None, "--only",
        help=_watchdog_only_help(),
    ),
    since: str = typer.Option(
        "24h",
        "--since",
        help="Codex recovery age for --only recoverable (1s through 30d).",
    ),
    cwd: Optional[str] = typer.Option(
        None,
        "--cwd",
        help="Exact checkout scope for --only recoverable. Defaults to cwd.",
    ),
    session_id: Optional[str] = typer.Option(
        None,
        "--session-id",
        help=(
            "For --only recoverable, select one canonical full Codex UUID "
            "before dry-run or apply."
        ),
    ),
    mail_to: Optional[str] = typer.Option(
        None,
        "--mail",
        help=(
            "Mail the digest to this handle (an agent name, short id, or "
            "project:<slug>). Defaults to config.recovery.watchdog.mail_to. "
            "Skipped when the non-leave verdict set is unchanged."
        ),
    ),
) -> None:
    """Report unfinished work: started nodes nobody holds, done branches
    ahead of origin/main, dirty ownerless worktrees, and ownerless open PRs
    (a PR whose owner reads unknown waits 24h). Every finding names the one
    command that clears it.

    The transcript is the truth source (keyed by session id); the registry
    and claude's agent view are hints. The default output is the
    unfinished-work report; --apply/--only reach the internal recovery
    lanes, which stay explicit.
    """
    import time as _time

    from fno.agents import watchdog as wd

    if only is not None and only not in wd.VERDICTS:
        print(f"fno agents watchdog: unknown verdict {only!r}", file=sys.stderr)
        raise typer.Exit(code=2)

    now = _time.time()
    if only == wd.RECOVERABLE:
        selected_session_id = session_id if isinstance(session_id, str) else None
        if selected_session_id:
            from fno.agents.discover import _is_canonical_full_uuid

            if not _is_canonical_full_uuid(selected_session_id):
                print(
                    "fno agents watchdog: --session-id requires one canonical full UUID",
                    file=sys.stderr,
                )
                raise typer.Exit(code=2)
        try:
            recency_seconds = wd.parse_recovery_since(since)
            scope_cwd = wd.resolve_recovery_cwd(cwd)
        except ValueError as exc:
            print(f"fno agents watchdog: {exc}", file=sys.stderr)
            raise typer.Exit(code=2) from exc

        payload, rows, scan = wd.run_recoverable_sweep(
            cwd=scope_cwd,
            recency_seconds=recency_seconds,
            now_s=now,
            session_id=selected_session_id,
        )
        pairs = [
            (wd.Verdict(**data), row)
            for data, row in zip(payload["verdicts"], rows)
        ]

        def _print_verdicts():
            for verdict, row in pairs:
                typer.echo(
                    f"{verdict.verdict:11} {verdict.row_id} "
                    f"handle={verdict.name} cwd={row.cwd}"
                )

        def _apply_and_emit():
            # One move shared by both lanes: apply, then the JSON emit.
            results = wd.apply_recoverable(scan)
            if json_out:
                sys.stdout.write(
                    json.dumps(
                        {
                            **payload,
                            "results": results,
                            "result_counts": wd.recovery_result_counts(results),
                        }
                    )
                    + "\n"
                )
            return results

        if not scan.complete:
            if apply or apply_all:
                results = _apply_and_emit()
                print(results[0]["detail"], file=sys.stderr)
                raise typer.Exit(code=3)
            if json_out:
                sys.stdout.write(json.dumps(payload) + "\n")
            else:
                _print_verdicts()
                for warning in payload["warnings"]:
                    print(f"warning: {warning}", file=sys.stderr)
                typer.echo(
                    f"recoverable=unknown complete=false "
                    f"scanned={payload['scanned_count']}"
                )
            return

        if selected_session_id and not scan.recoverable:
            payload["selection_error"] = "selected session is not recoverable"
            if json_out:
                sys.stdout.write(json.dumps(payload) + "\n")
            else:
                print(
                    f"fno agents watchdog: session {selected_session_id} is not "
                    "one recoverable candidate",
                    file=sys.stderr,
                )
            raise typer.Exit(code=3)

        previous_events = wd._last_events_signature()
        signature = wd.verdict_signature(payload)
        wd.write_sweep_file(
            "manual",
            payload["counts"],
            now,
            signature,
            events_signature=signature,
            recoverable_count=payload["recoverable_count"],
        )
        for verdict, _row in pairs:
            if verdict.row_id in wd.fresh_non_leave(payload, previous_events):
                wd.emit_event(
                    "watchdog_verdict",
                    {
                        "row_id": verdict.row_id,
                        "name": verdict.name,
                        "verdict": verdict.verdict,
                        "basis": verdict.basis,
                    },
                )
        if not apply and not apply_all:
            if json_out:
                sys.stdout.write(json.dumps(payload) + "\n")
            else:
                _print_verdicts()
                typer.echo(
                    f"recoverable={payload['recoverable_count']} "
                    f"usable={payload['usable_recoverable_count']} "
                    f"unusable={payload['unusable_recoverable_count']} complete=true "
                    f"scanned={payload['scanned_count']}"
                )
            return

        results = _apply_and_emit()
        if not json_out:
            for result in results:
                line = f"{result['outcome']:9} {result['detail']}"
                print(line, file=sys.stderr if result["outcome"] != "applied" else sys.stdout)
            result_counts = wd.recovery_result_counts(results)
            typer.echo(
                f"applied={result_counts['applied']} "
                f"refused={result_counts['refused']} "
                f"deferred={result_counts['deferred']}"
            )
        return

    if only == wd.KEEPER:
        # The keeper lane's own surface: keepers have no registry row, so the
        # per-row sweep has nothing to classify. Dry run names every keeper
        # with its verdict and reason; ONLY --apply-all collects, because
        # killing a keeper destroys work - the bar --apply's help text
        # promises the wake lane never crosses.
        from fno.agents import keeper_lane

        try:
            lane_result = keeper_lane.discover()
        except Exception as exc:  # noqa: BLE001 - a crashed lane withholds, never half-acts
            print(f"fno agents watchdog: keeper lane crashed: {exc!r}", file=sys.stderr)
            raise typer.Exit(code=3) from exc
        if json_out:
            from datetime import datetime, timezone

            payload = {
                "generated_at": datetime.fromtimestamp(
                    now, tz=timezone.utc
                ).strftime("%Y-%m-%dT%H:%M:%SZ"),
                "lane": wd.KEEPER,
                **lane_result.to_json(),
            }
            if apply_all:
                keepers, children = keeper_lane.reap_keepers(lane_result)
                payload["reaped_keepers"] = keepers
                payload["reaped_children"] = children
            sys.stdout.write(json.dumps(payload) + "\n")
            sys.stdout.flush()
            raise typer.Exit(code=3 if lane_result.broken else 0)
        typer.echo(keeper_lane.render(lane_result))
        if lane_result.broken:
            raise typer.Exit(code=3)
        if apply and not apply_all:
            typer.echo(
                "--apply never kills a keeper (wake lane only); "
                "the collect flag is --apply-all"
            )
        elif apply_all:
            keepers, children = keeper_lane.reap_keepers(lane_result)
            typer.echo(
                f"reaped {len(keepers)} keeper(s), {len(children)} hosted child(ren)"
            )
        return

    if only is None and not apply and not apply_all:
        # The default surface: the unfinished-work report. Session verdicts
        # (and their counts) are recovery internals behind --apply/--only,
        # never the operator's answer.
        _run_unfinished_report(now=now, json_out=json_out, mail_to=mail_to)
        return

    payload, rows = wd.run_sweep(now_s=now)
    if payload.get("refused"):
        #: a zero-row roster is an unreadable instrument, not an empty
        # fleet. Write no sweep file and advance no gate, so staleness reads
        # loud instead of certifying a healthy quiet fleet that was never read.
        print(f"fno agents watchdog: {payload['refused']}", file=sys.stderr)
        # The refusal says the roster was unreadable; the warnings say WHY
        # (timed out, binary missing, non-zero exit, budget headroom).
        # Dropping them leaves the one actionable line on the floor.
        for warning in payload.get("warnings") or []:
            print(f"  {warning}", file=sys.stderr)
        raise typer.Exit(code=3)
    pairs = [
        (wd.Verdict(**d), r) for d, r in zip(payload["verdicts"], rows)
    ]
    shown_counts = payload["counts"]
    if only is not None:
        pairs = [p for p in pairs if p[0].verdict == only]
        # A filtered view must not report the full sweep's counts: anything
        # cross-checking the rows it was handed against the counts would
        # disagree with both.
        shown_counts = {}
        for v, _row in pairs:
            shown_counts[v.verdict] = shown_counts.get(v.verdict, 0) + 1

    # Push, not pull: mail before writing the sweep file, so only a delivered
    # digest advances the change gate - a transient send failure must not
    # permanently swallow the verdict behind an unchanged signature.
    recipient = mail_to
    if recipient is None:
        try:
            from fno.config import load_settings

            recipient = str(load_settings().recovery.watchdog.mail_to or "")
        except Exception:  # noqa: BLE001 - config read miss means no mail
            recipient = ""
    signature = ""
    try:
        ok, receipt, signature = wd.mail_gate(payload, recipient or "")
        if not ok:
            print(f"watchdog mail: {receipt}", file=sys.stderr)
    except Exception as exc:  # noqa: BLE001 - mail never breaks the sweep
        print(f"watchdog mail failed: {exc}", file=sys.stderr)
    # A filtered run publishes only its own rows: never stamp the whole
    # non-leave set, and stamp the UNION with what was already published,
    # or the next tick re-emits every filtered-out row.
    events_payload = (
        payload if only is None
        else {**payload, "verdicts": [v._asdict() for v, _ in pairs]}
    )
    prev_events_sig = wd._last_events_signature()
    signature_to_stamp = wd.union_signature(
        prev_events_sig, wd.verdict_signature(events_payload)
    ) if only is not None else wd.verdict_signature(events_payload)
    wd.write_sweep_file(
        "manual", payload["counts"], now, signature,
        events_signature=signature_to_stamp,
        terminal_harness_rows=payload.get("terminal_harness_rows", 0),
        provider_outages=payload.get("provider_outages") or wd._unknown_provider_report(
            "provider_outage_payload_missing"
        ),
    )

    # Classification events ride every mode (a dry-run-only verdict once left
    # apply modes with no event record), gated on fresh_non_leave so a filtered
    # hand-run neither diverges from the tick's record nor re-emits the fleet.
    fresh_ids = wd.fresh_non_leave(events_payload, prev_events_sig)
    for v, _row in pairs:
        if v.verdict != wd.LEAVE and v.row_id in fresh_ids:
            wd.emit_event(
                "watchdog_verdict",
                {"row_id": v.row_id, "name": v.name,
                 "verdict": v.verdict, "basis": v.basis},
            )

    if not apply and not apply_all:
        if json_out:
            filtered = {
                **payload,
                "verdicts": [v._asdict() for v, _ in pairs],
                "counts": shown_counts,
            }
            sys.stdout.write(json.dumps(filtered) + "\n")
            sys.stdout.flush()
            return
        for v, _row in pairs:
            typer.echo(f"{v.name:34} {v.state:9} {v.verdict:8} {v.basis}")
        for warning in payload["warnings"]:
            print(f"warning: {warning}", file=sys.stderr)
        counts = " ".join(f"{k}={v}" for k, v in sorted(shown_counts.items()))
        typer.echo(f"{len(pairs)} row(s): {counts}")
        typer.echo(
            f"terminal harness rows: {payload.get('terminal_harness_rows', 0)}"
        )
        for line in wd.provider_outage_lines(payload.get("provider_outages")):
            typer.echo(line)
        return

    lanes = "all" if apply_all else "wake"
    results = []
    for v, row in pairs:
        try:
            outcome, detail = wd.apply_verdict(
                v, lanes=lanes, cwd=row.cwd, node=row.node
            )
        except Exception as exc:  # noqa: BLE001 - one broken row never aborts the rest
            outcome, detail = "refused", f"{v.verdict} action crashed: {exc!r}"
        results.append({"row_id": v.row_id, "verdict": v.verdict,
                        "outcome": outcome, "detail": detail})
        if outcome == wd.SKIPPED:
            # The ONE silent outcome: a verdict outside the lane (every leave
            # row on a bare --apply). Printing one line per healthy row
            # drowns the few that acted. Everything else surfaces, so a new
            # outcome cannot go silent by not being listed here.
            continue
        line = f"{outcome:9} {v.name:34} {detail}"
        if outcome != "applied":
            print(line, file=sys.stderr)
        elif not json_out:
            # Human lines on stdout ahead of the JSON object make the whole
            # document unparseable; the dry-run path already guards this.
            typer.echo(line)
        # Every non-skipped outcome emits: the `outcome` field carries which
        # one it was, so no list here decides what is worth recording.
        wd.emit_event(
            wd.outcome_event(outcome),
            {"row_id": v.row_id, "verdict": v.verdict, "detail": detail,
             "outcome": outcome},
        )
    keeper_reaped: dict = {}
    if apply_all and only is None:
        # Keeper collection rides --apply-all, never --apply: killing a keeper
        # destroys work, the bar the flag help draws. It also rides the FULL
        # surface only: `--only <verdict>` is a diagnostic filter over the
        # session table, and a filtered run must not widen its destructive
        # scope to a lane the filter did not name. A crashed or broken lane is
        # named, never silently skipped.
        from fno.agents import keeper_lane

        try:
            lane_result = keeper_lane.discover()
            keepers, children = keeper_lane.reap_keepers(lane_result)
        except Exception as exc:  # noqa: BLE001 - one lane's crash never ends the sweep
            print(f"keeper lane crashed: {exc!r}", file=sys.stderr)
        else:
            keeper_reaped = {"keepers": keepers, "keeper_children": children}
            if lane_result.broken:
                print(f"keeper lane: {lane_result.broken_reason}", file=sys.stderr)
            elif keepers or children:
                typer.echo(
                    f"reaped {len(keepers)} keeper(s), "
                    f"{len(children)} hosted child(ren)"
                )
    if json_out:
        sys.stdout.write(json.dumps({"results": results, **keeper_reaped}) + "\n")
        sys.stdout.flush()


@agents_app.command("stale-escalate", hidden=True)
def cmd_stale_escalate(
    json_out: bool = typer.Option(False, "--json", "-J", help="Machine-readable output."),
) -> None:
    """Reconcile the durable stale-row question to the measured fleet."""
    from fno.agents import stale_lane as se

    se.run(json_out=json_out)


@agents_app.command("friction-escalate", hidden=True)
def cmd_friction_escalate(
    json_out: bool = typer.Option(False, "--json", "-J", help="Machine-readable output."),
) -> None:
    """Reconcile ONE [watchdog-friction:*] question to the measured fleet."""
    from fno.agents import friction_lane as fl

    fl.run(json_out=json_out)


@agents_app.command("drive-authority", hidden=True)
def cmd_drive_authority(
    json_out: bool = typer.Option(False, "--json", "-J", help="Machine-readable output."),
) -> None:
    """Report whether an operator holds a gate-hardening drive window.

    Exits 0 when at least one agent has an interactive/step/paranoid drive
    window open, 1 when none -- so a hook can branch with
    ``if fno agents drive-authority --json >/dev/null; then ...``. Read-only.
    Gate-hardening consumers (stop hook, PreToolUse) use this to treat a
    ``<promise>`` or gate edit during a drive as operator-initiated (LD3).
    """
    import json as _json

    from fno.drive_authority import active_drive_sessions

    sessions = active_drive_sessions()
    if json_out:
        typer.echo(_json.dumps({"active": bool(sessions), "sessions": sessions}))
    elif sessions:
        for s in sessions:
            typer.echo(f"{s['short_id']} {s['mode']} {s['session_id']}")
    else:
        typer.echo("no active drive authority")
    raise typer.Exit(0 if sessions else 1)


@agents_app.command("stop")
def cmd_stop(
    name: str = typer.Argument(..., help="Agent name (from `fno agents list`)."),
) -> None:
    """Stop an agent's underlying session.

    The ``fno-agents`` client owns this command; the argument above documents
    its surface. There is no Python stop lane left: a missing binary is
    refused.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    refuse_without_binary("stop")


@agents_app.command("rm", hidden=True)
def cmd_rm(
    name: str = typer.Argument(..., help="Agent name (from `fno agents list`)."),
    force: bool = typer.Option(
        False,
        "--force",
        "-F",
        help=(
            "Drop the registry entry even when the row is LIVE or the harness "
            "teardown fails or refuses (e.g. uncommitted worktree changes). "
            "The Rust route (the default) kills a mux-hosted pane with it, "
            "but still refuses a live pane worker it cannot stop. A Claude "
            "row also removes its harness session and worktree under Claude's "
            "guards; non-Claude bg and headless processes survive. WARNING: leaves an orphan session record in that "
            "harness's own store, named on stderr, for you to clean manually."
        ),
    ),
    audit_actor: str | None = typer.Option(None, "--audit-actor", hidden=True),
    audit_reason: str = typer.Option("operator-requested", "--audit-reason", hidden=True),
    audit_request_id: str | None = typer.Option(None, "--audit-request-id", hidden=True),
    audit_worktree_touched: bool = typer.Option(
        False, "--audit-worktree-touched", hidden=True
    ),
    audit_reclaimed_bytes: int | None = typer.Option(
        None, "--audit-reclaimed-bytes", hidden=True
    ),
) -> None:
    """Remove an agent: harness or mux session first, registry row after.

    rm runs on the Rust runtime only: it requires the `fno-agents` binary,
    and `FNO_AGENTS_RUNTIME=python` cannot force it onto Python. With a
    binary installed, `auto` routing (the default) execs it directly, so
    this body is reached only when no binary resolved.

    Per-harness teardown (all on the Rust side):

    \b
      claude    bg session: drops the claude session record; pane session:
                kills the mux pane
      codex     drops the session's entry from ~/.codex/session_index.jsonl
      opencode  registry-only; `rm` will not delete an opencode session,
                because that also deletes its child sessions and its whole
                message history. Run `opencode session delete <id>` if you
                want the conversation gone.
      gemini    registry-only (no teardown arm for a deprecated provider)

    Your history is never removed here -- teardown drops the harness's
    index record, not the conversation. On teardown failure the registry
    row is kept so you can retry; ``--force`` drops it anyway and names
    the orphan in the receipt. rm needs no prior stop: it ends the
    session's process itself as part of the removal. The one exception is
    a claude background thread, where rm runs `claude stop` first - and
    runs it itself, so no caller composes one. When the ask does not end
    the session, rm ends the proven process set itself (a glm worker's
    `claude stop` returns without ending it); only an end rm cannot
    confirm keeps the row, and then it says why. Do not tear a
    session down by hand: the harness session record IS the resume
    handle, and dropping it directly spends that handle for nothing this
    command has not already done. If one is already orphaned, use the
    retained full ``harness_session_id`` with ``fno agents adopt``. A
    linked worktree is removed only when the shared guarded predicate
    says it is clean, merged, and unowned; otherwise the row is removed
    and the worktree receipt names the refusal.
    """
    from fno import rust_binary
    from fno.agents.rust_runtime import refuse_without_binary, runtime_mode

    binary = rust_binary.resolve_installed_binary()
    if runtime_mode() == "python" or binary is None:
        # There is no Python rm to fall back to: the twin was deleted
        # (d-e11b2b3e: one command, one implementation). Refuse by name.
        refuse_without_binary("rm")


@agents_app.command("reconcile", hidden=True)
def cmd_reconcile(
    json_out: bool = typer.Option(
        False,
        "--json",
        "-J",
        help="Emit JSON regardless of TTY (mirrors `fno agents list --json`).",
    ),
) -> None:
    """Sync registry status with provider reality.

    For each registered agent, probe the underlying provider:

    - claude: ``claude logs <short_id> --tail 1`` exit code decides
      reachability.
    - codex: presence in ``~/.codex/session_index.jsonl`` decides.
    - gemini: skipped until US4-gemini ships.

    Status flips bidirectionally (``live`` ↔ ``orphaned``) and never
    deletes a row - operator decides removal via ``fno agents rm``.
    Output is human-readable by default, JSON when ``--json`` is passed
    or stdout is not a TTY (Locked Decision 4 mirror from ``list``).
    """
    import json

    from fno.agents.dispatch import DispatchAskError, reconcile_agents

    is_tty = bool(getattr(sys.stdout, "isatty", lambda: False)())
    emit_json = json_out or not is_tty

    try:
        result = reconcile_agents()
    except DispatchAskError as exc:
        print(str(exc), file=sys.stderr)
        raise typer.Exit(code=exc.exit_code) from exc

    if emit_json:
        payload = {
            "scanned": result.scanned,
            "orphaned": result.orphaned,
            "recovered": result.recovered,
            "skipped": result.skipped,
            "errors": result.errors,
            # Always present (empty when nothing healed) so "ran, nothing to heal"
            # is distinguishable from "healed w1" in the JSON.
            "backfilled": result.backfilled,
        }
        sys.stdout.write(json.dumps(payload, sort_keys=False) + "\n")
        sys.stdout.flush()
        return

    render_reconcile_human(result, out=sys.stdout)
    sys.stdout.flush()


def render_reconcile_human(result, *, out) -> None:
    """Write one human-readable line per status change, then a roll-up.

    Extracted so test_cli_lifecycle can exercise the format without
    fighting Typer's CliRunner stdout capture (which never reports
    isatty=True). The aggregate counts mirror the JSON payload's keys
    so operators see the same numbers in both render modes.
    """
    for entry in result.orphaned:
        sid = entry.get("id") or "?"
        out.write(f"{entry['name']} ({entry['provider']}/{sid}): live → orphaned\n")
    for entry in result.recovered:
        sid = entry.get("id") or "?"
        out.write(f"{entry['name']} ({entry['provider']}/{sid}): orphaned → live\n")
    for entry in result.skipped:
        out.write(
            f"{entry['name']} ({entry['provider']}): skipped "
            f"({entry.get('reason', 'unspecified')})\n"
        )
    for entry in result.errors:
        out.write(
            f"{entry['name']} ({entry['provider']}): error ({entry.get('reason', 'unspecified')})\n"
        )
    for entry in getattr(result, "backfilled", []):
        sid = entry.get("harness_session_id") or "?"
        out.write(f"{entry['name']} ({entry['provider']}): harness_session_id backfilled ({sid})\n")

    out.write(
        f"{result.scanned} entries scanned: "
        f"{len(result.orphaned)} orphaned, "
        f"{len(result.recovered)} recovered, "
        f"{len(result.skipped)} skipped"
    )
    if result.errors:
        out.write(f", {len(result.errors)} errors")
    if getattr(result, "backfilled", []):
        out.write(f", {len(result.backfilled)} backfilled")
    out.write("\n")


@agents_app.command("attach")
def cmd_attach(
    name: str = typer.Argument(..., help="Agent name (from `fno agents list`)."),
) -> None:
    """Attach to a running agent session interactively.

    The Rust client command owns attach; with an installed binary the runtime
    router execs it before this function runs. With a live mux server it
    drives the one dedicated thread pane, which routes every harness by
    capability. With no mux server: claude execs ``claude attach
    <short_id>``, a codex thread execs its declared attach form, pi joins
    its own session, and every other row is asked of the capability table.
    A row whose harness reads ``features.attach = native`` names the
    daemon-kept lane it needs (exit 24); any other state refuses by name
    with the key, the state and the probe that settles it (exit 13).

    There is no Python attach left: a missing binary is refused here.
    """
    from fno.agents.rust_runtime import refuse_without_binary

    _ = name
    refuse_without_binary("attach")


# Observability commands: trace + resume (Tasks 3.3 / 3.4 / 3.5)
# Both commands live in their own modules so this CLI file stays focused
# on shape + wiring. The cmd_<command> functions are re-bound here as
# Typer subcommands; tests can still monkeypatch cli.cmd_<command> for
# spy injection.

from fno.agents.trace_cli import cmd_trace as _cmd_trace  # noqa: E402
from fno.agents.resume_cli import cmd_resume as _cmd_resume  # noqa: E402
from fno.agents.history import history_command  # noqa: E402
from fno.agents.transcript import transcript_command  # noqa: E402

agents_app.command("trace", hidden=True)(_cmd_trace)
agents_app.command("resume")(_cmd_resume)
# Advertised: the one command over live rows, reap receipts and the
# ledger. `fno whoami ledger` stays as its hidden alias.
agents_app.command("history")(history_command)
agents_app.command("transcript", hidden=True)(transcript_command)


#: instruments over the capability table. `probe` measures the live
# harness against the (config-merged) row. Four verdicts and UNKNOWN never
# acts: a missing binary or a timeout is UNKNOWN with its reason, never a
# disagreement, because an absent instrument is not a measurement.
harness_app = typer.Typer(
    help="Instruments over the harness capability table.",
    no_args_is_help=True,
)


@harness_app.command("probe")
def harness_probe(
    harness: str = typer.Argument(...),
    live: bool = typer.Option(False, "--live"),
    write: bool = typer.Option(False, "--write"),
    as_json: bool = typer.Option(False, "--json", "-J"),
) -> None:
    from fno.agents.rust_runtime import refuse_without_binary, route_to_rust
    from fno.rust_binary import resolve_installed_binary
    binary = resolve_installed_binary() or refuse_without_binary("harness probe")
    argv = ["harness-probe", "fields", harness] + ["--live"] * live + ["--write"] * write + ["--json"] * as_json
    route_to_rust(argv, binary=binary)


agents_app.add_typer(harness_app, name="harness", hidden=True)

from fno.agents import (  # noqa: E402,F401
    ask_cli,
    distress_reads,
    gate_reads,
    peek_cli,
)


@agents_app.command(
    "incident",
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)
def incident(ctx: typer.Context) -> None:
    """Durable fleet incident breaker.

    stop --reason T [--by X] | clear --reason T [--by X] | status [--json] | check [--scope S] | cutover save|restore.
    """
    import subprocess

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        typer.secho(
            "fno agents incident: fno-agents binary not found; `fno doctor update --rust` or set FNO_AGENTS_BIN",
            err=True,
        )
        raise typer.Exit(code=1)
    proc = subprocess.run([str(binary), "fleet-incident", *ctx.args])
    raise typer.Exit(code=proc.returncode if proc.returncode >= 0 else 128 - proc.returncode)


@agents_app.command(
    "provider-cap",
    hidden=True,
    context_settings={"allow_extra_args": True, "ignore_unknown_options": True},
)
def provider_cap(ctx: typer.Context) -> None:
    """The provider-cap actor.

    status [--json] [--max-age-s N] | decide <lane> --answer all|some:<id,id>|wait.
    """
    import subprocess

    from fno.rust_binary import find_dev_binary, resolve_binary

    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        typer.secho(
            "fno agents provider-cap: fno-agents binary not found; `fno doctor update --rust` or set FNO_AGENTS_BIN",
            err=True,
        )
        raise typer.Exit(code=1)
    proc = subprocess.run([str(binary), "provider-cap", *ctx.args])
    raise typer.Exit(code=proc.returncode if proc.returncode >= 0 else 128 - proc.returncode)
