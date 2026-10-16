"""fno agents claim - Typer surface for the work-claim verbs.

Exit codes: 0 success; 1 held/contention retry exhaustion (also reap's own
archive-confirm failure, a distinct overload of 1, not a retry signal); 2
validation or input error; 3 corruption or race; 4 holder mismatch. The full
table lives in docs/architecture/claim-verbs-do-provenance.md. Structured
output uses --json on each verb; without it, output is a human-friendly
summary on stdout and errors always go to stderr. The claim verbs also write
the node's `do` provenance rows; that contract lives in the same doc.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import List, Mapping, Optional

import typer

# The core callables are resolved THROUGH the module at call time, never
# bound here: a module-level `from .core import acquire_claim` snapshot is
# frozen at this module's first import, and a worker whose first import lands
# while a test's core stub is active captures the stub permanently - every
# later `claim acquire` then prints success and writes nothing (the
# xdist-scheduling flake in test_dispatch_barrier; see the regression test in
# test_claims_cli.py). Constants and exception classes are safe to bind: the
# suite patches functions, not classes.
from . import roster as _roster
from .core import (
    HANDOVER_HOLDER_PREFIX as _HANDOVER_HOLDER_PREFIX,
    ClaimState,
)
from fno.tombstones import tombstone_group_cls

RosterReading = _roster.RosterReading
_finished_row_states = _roster._finished_row_states
classify_workers = _roster.classify_workers
read_roster = _roster.read_roster


cli = typer.Typer(
    name="claim",
    help="Work-claim coordination primitive",
    no_args_is_help=True,
    cls=tombstone_group_cls("claim"),
)


def _parse_ttl(value: Optional[str]) -> Optional[int]:
    """Convert "1h" / "30m" / "3600s" / "5000" into milliseconds.

    Empty string and None return None (caller decides default).
    Plain digits are interpreted as seconds, matching ``sleep`` convention.
    """
    try:
        return parse_ttl_ms(value)
    except ValueError as exc:
        raise typer.BadParameter(str(exc)) from exc


def _parse_metadata(value: str) -> dict:
    if not value:
        return {}
    try:
        parsed = json.loads(value)
    except json.JSONDecodeError as exc:
        raise typer.BadParameter(f"--metadata is not valid JSON: {exc}")
    if not isinstance(parsed, dict):
        raise typer.BadParameter("--metadata must be a JSON object")
    return parsed


#: Seconds the roster cross-check may spend shelling out to the harness.
#: A status read is interactive, so it must answer late-but-honestly
#: ("roster not consulted") rather than hang a lead mid-decision.
_ROSTER_CROSSCHECK_TIMEOUT_S = 10.0


def _node_aware_root(key: str):
    """Resolve the claims root for a key: the native leg's routing read.

    Global-id kinds (``node:``/``dispatch:``/``reconcile:``/``session:``) route to
    the global ``~/.fno/claims`` so operator commands work without the env var
; repo-local keys keep the cwd/env default. The prefix list lives once, in
    crates/fno-agents/src/claims_root.rs.
    """
    from .core import native_claims_root

    return native_claims_root(key)


@cli.command()
def acquire(
    key: Optional[str] = typer.Argument(
        None, help="Lock key, e.g. node:ab-1234abcd. Omit when using --lane."
    ),
    holder: str = typer.Option("", "--holder", help="Symbolic owner string"),
    lane: Optional[str] = typer.Option(
        None,
        "--lane",
        help=(
            "Acquire a LANE SLOT for this lane id instead of a keyed claim "
            "(was `claim lane-acquire --lane-id`). Requires --max-lanes; "
            "takes no KEY and no --holder."
        ),
    ),
    max_lanes: Optional[int] = typer.Option(
        None, "--max-lanes", help="With --lane: concurrency cap (>=1; 1 == sequential)."
    ),
    reason: str = typer.Option("", "--reason", "-R", help="Optional rationale recorded in audit"),
    ttl: str = typer.Option(
        "", "--ttl", help="TTL expression like 30m, 1h, 3600s (omit => PID-liveness)"
    ),
    metadata: str = typer.Option("{}", "--metadata", help="JSON object passed verbatim"),
    pid: Optional[int] = typer.Option(
        None,
        "--pid",
        help=(
            "PID-liveness anchor (omit => nearest agent session, else this process's "
            "PID). Pin the claim to a LONG-LIVED owner (e.g. a stream worker) rather "
            "than the transient acquiring process so PID-liveness does not mark it "
            "stale instantly."
        ),
    ),
    pid_unavailable: bool = typer.Option(
        False,
        "--pid-unavailable",
        help=(
            "Record explicit PID absence. Requires --ttl; the TTL is the claim's "
            "liveness authority."
        ),
    ),
    json_output: bool = typer.Option(False, "--json", "-J", help="Emit JSON to stdout"),
    verbose: bool = typer.Option(False, "--verbose", help="More detail on stderr"),
    harness: Optional[str] = typer.Option(
        None,
        "--harness",
        help=(
            "Pin the owning harness on the claim instead of resolving it from "
            "ambient markers. The init hook passes the PROVEN harness here so a "
            "session that inherited a foreign marker does not tag its claim with "
            "the precedence winner."
        ),
    ),
    handover_from: Optional[str] = typer.Option(
        None,
        "--handover-from",
        help=(
            "Take over a claim currently held by this EXACT prior holder, "
            "instead of failing as held-by-other. For the dispatch handover: "
            "`fno agents spawn --node` claims the node before its worker "
            "exists, and the worker names that holder here to inherit it. "
            "Falls through to a normal acquire when no such claim is on disk."
        ),
    ),
) -> None:
    """Acquire a claim on KEY for HOLDER. Idempotent re-acquire if HOLDER matches.

    With ``--lane <id> --max-lanes N`` this acquires a lane SLOT instead, which
    is the same operation against a different claim space; it was a separate
    ``lane-acquire`` verb whose name was its own flag.
    """
    return _forward_acquire(
        key, holder, lane, max_lanes, reason, ttl, metadata, pid,
        pid_unavailable, json_output, verbose, harness, handover_from,
    )


def _forward_acquire(
    key, holder, lane, max_lanes, reason, ttl, metadata, pid,
    pid_unavailable, json_output, verbose, harness, handover_from,
) -> None:
    """Forward to the bundled fno-agents binary, binary-direct.

    The wave-1 port moved the leaf's logic (validations, lane slot, the
    handover rebind, the do stamp, output and exit codes) into
    `crates/fno-agents/src/claim_cli/acquire.rs`; Python owns transport only,
    per the dual-implementation protocol.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv = []
    if key is not None:
        argv.append(key)
    if holder:
        argv.extend(("--holder", holder))
    if lane is not None:
        argv.extend(("--lane", lane))
    # A lone --max-lanes forwards too: the leaf owns the refusal (the cap
    # flag without its lane mode must exit 2, never be silently dropped).
    if max_lanes is not None:
        argv.extend(("--max-lanes", str(max_lanes)))
    if reason:
        argv.extend(("--reason", reason))
    if ttl:
        argv.extend(("--ttl", ttl))
    argv.extend(("--metadata", metadata))
    if pid is not None:
        argv.extend(("--pid", str(pid)))
    if pid_unavailable:
        argv.append("--pid-unavailable")
    if json_output:
        argv.append("--json")
    if verbose:
        argv.append("--verbose")
    if harness is not None:
        argv.extend(("--harness", harness))
    if handover_from is not None:
        argv.extend(("--handover-from", handover_from))
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim acquire: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer: the leaf's output IS the
    # operator surface, and a CliRunner-hosted caller (the tests) must see it.
    result = subprocess.run(
        [str(binary), "claim", "acquire", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@cli.command()
def release(
    key: Optional[str] = typer.Argument(None, help="Claim key. Omit when using --lane."),
    holder: str = typer.Option("", "--holder"),
    lane: Optional[str] = typer.Option(
        None,
        "--lane",
        help=(
            "Release the LANE SLOT held by this lane id instead of a keyed "
            "claim (was `claim lane-release --lane-id`). Takes no KEY."
        ),
    ),
    force: bool = typer.Option(
        False,
        "--force",
        "-F",
        help=(
            "Administratively drop the claim regardless of owner, archiving it "
            "to .expired/ (was `claim force-release`). Requires --reason and "
            "takes no --holder."
        ),
    ),
    reason: str = typer.Option(
        "", "--reason", "-R", help="With --force: required audit rationale."
    ),
    strict: bool = typer.Option(False, "--strict", help="Raise if holder does not match"),
    stamp_do: bool = typer.Option(
        False,
        "--stamp-do",
        help="Stamp a execute provenance row (started_at from this claim's acquire time, "
        "ended_at now). Set ONLY by a session releasing its OWN node claim at a "
        "finished terminal - never a handoff, which runs under a successor's "
        "identity and would mis-attribute the predecessor's window.",
    ),
    rollback_do: bool = typer.Option(
        False,
        "--rollback-do",
        help="Remove the open execute provenance row this claim's acquire opened. Set "
        "by a releaser whose POST-ACQUIRE validation refused it: it took the "
        "claim only to serialize, did no work, and must not leave the node "
        "reading as in progress. Only an open row (no ended_at) whose "
        "started_at matches this claim is removed. Mutually exclusive with "
        "--stamp-do.",
    ),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Release a claim we own. Silent success if already released.

    Three modes that were three verbs, because two of the names were flags:
    the default releases a claim we hold, ``--lane <id>`` releases a lane slot,
    and ``--force`` drops a claim regardless of owner.
    """
    return _forward_release(
        key, holder, lane, force, reason, strict, stamp_do, rollback_do, json_output
    )


def _forward_release(
    key, holder, lane, force, reason, strict, stamp_do, rollback_do, json_output
) -> None:
    """Forward to the bundled fno-agents binary, binary-direct.

    The wave-2 port moved the leaf's logic (the mode-validation matrix, the
    do-row close/rollback, lane and force release, output and exit codes)
    into `crates/fno-agents/src/claim_cli/release.rs`; Python owns transport
    only, per the dual-implementation protocol.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv = []
    if key is not None:
        argv.append(key)
    if holder:
        argv.extend(("--holder", holder))
    if lane is not None:
        argv.extend(("--lane", lane))
    if force:
        argv.append("--force")
    if reason:
        argv.extend(("--reason", reason))
    if strict:
        argv.append("--strict")
    if stamp_do:
        argv.append("--stamp-do")
    if rollback_do:
        argv.append("--rollback-do")
    if json_output:
        argv.append("--json")
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim release: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer: the leaf's output IS the
    # operator surface, and a CliRunner-hosted caller (the tests) must see it.
    result = subprocess.run(
        [str(binary), "claim", "release", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


def _owned_do_identity(claim, holder: str) -> "tuple[str, str, str | None]":
    """Resolve the (harness, session_id) a execute provenance row should be written
    under: the OWNED identity, not the ambient env.

    The harness the claim was pinned to (init passes the proven --harness;
    ``claim.harness`` carries it) and the session encoded in the holder
    (``target-session:<id>``). Ambient marker precedence would launder an
    inherited foreign marker into the row, so ambient is only a
    fallback when the owned values are absent. Shared by the acquire and
    release stamps so they always agree on the row key - release must fill the
    row acquire opened, not open a second one."""
    from fno.claims.self_identity import resolve_self_identity

    ident = resolve_self_identity()
    harness = (getattr(claim, "harness", None) or ident.harness or "").strip()
    session_id = ""
    if holder and holder.startswith("target-session:"):
        session_id = holder.split(":", 1)[1].strip()
    if not session_id:
        session_id = (ident.session_id or "").strip()
    return harness, session_id, _owned_registry_effort(harness, session_id)



def _do_row_coordinates(key: str, claim, holder: str, action: str):
    """The ``(node_id, harness, session_id, started_at)`` naming the do row for
    this claim, or ``None`` after printing the named skip.

    All three do-row writers (open at acquire, close at release, roll back at a
    refused acquire) must address the SAME row, so they derive its coordinates
    here rather than each re-deriving them. ``started_at`` is the claim's own
    acquire time - the honest phase start, which sat in the claim file all along.
    ``action`` names the caller in the skip line."""
    node_id = key.split(":", 1)[1] if ":" in key else ""
    if not node_id:
        return None
    harness, session_id, effort = _owned_do_identity(claim, holder)
    if not harness or not session_id:
        typer.echo(
            f"claim {action}: no owned identity for the execute provenance row of "
            f"{node_id}; the row is skipped. Skipped.",
            err=True,
        )
        return None
    from datetime import datetime, timezone

    started = datetime.fromtimestamp(claim.acquired_at / 1000, tz=timezone.utc).strftime(
        "%Y-%m-%dT%H:%M:%SZ"
    )
    return node_id, harness, session_id, started, effort


def _stamp_do_on_acquire(key: str, claim, holder: str) -> None:
    """Open the do lifecycle row at claim acquire, recording started_at from the
    claim's own acquire time and leaving ended_at open for the release path to
    fill. Best-effort: a graph failure or missing identity is a named stderr
    skip and never fails the acquire.

    A session killed mid-phase never reaches its release terminal, so the
    release/finalize stamps never fire and the row would be lost entirely -
    including a started_at that sat in the claim file the whole time. Opening
    the row at acquire guarantees it exists from the moment work starts;
    append_session_record's duplicate-fill lets release close it (ended_at) and
    is a no-op on a re-acquire or a retried stamp.

    Identity is the OWNED one, not the ambient env: the harness the claim was
    pinned to (init passes the proven --harness; ``claim.harness`` carries it)
    and the session encoded in the holder (``target-session:<id>``). Ambient
    marker precedence would launder an inherited foreign marker into the row
, so it is only a fallback when the owned values are absent.

    Two reachable acquire paths both call this: the CLI ``claim acquire`` verb
    (the init-script cold start) and the native target-start takeover, whose
    in-process ``claims::acquire`` emits its own audit event. Both must stamp,
    or a killed session on either path loses its row.

    A caller that acquires the claim as a serialization step and may still be
    refused by a post-acquire re-check owns the rollback: see ``release
    --rollback-do``.
    """
    if claim is None:
        # REACHABLE, not dead: the successor reacquire paths call this with
        # no claim object (free/stale claim, nothing to stamp yet). Skipping
        # is correct there - there is no row to open.
        return
    from fno.graph.store import append_session_record
    from fno.paths import graph_json

    coords = _do_row_coordinates(key, claim, holder, "acquire")
    if coords is None:
        return
    node_id, harness, session_id, started, effort = coords
    try:
        found, _added = append_session_record(
            graph_json(), node_id, phase="execute",
            harness=harness, session_id=session_id,
            started_at=started, effort=effort,
        )
    except (Exception, SystemExit) as exc:
        typer.echo(
            f"claim acquire: execute provenance open skipped for {node_id}: {exc}",
            err=True,
        )
        return
    # append_session_record returns (found=False, added=False) without raising
    # when the node id is absent from the graph (a superseded node whose claim
    # file lingers). The named-skip contract requires an explicit stderr line so
    # the operator knows provenance was not opened, not silently dropped.
    if not found:
        typer.echo(
            f"claim acquire: execute provenance open skipped for {node_id} "
            f"(node not in graph); the row was not written. Skipped.",
            err=True,
        )


@cli.command()
def refresh(
    key: str = typer.Argument(...),
    holder: str = typer.Option(..., "--holder"),
    ttl: str = typer.Option("", "--ttl"),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Extend a TTL claim's expires_at. No-op for PID-liveness claims."""
    return _forward_refresh(key, holder, ttl, json_output)


def _forward_refresh(
    key, holder, ttl, json_output,
) -> None:
    """Forward to the bundled fno-agents binary, binary-direct. The wave-3
    port put the leaf's logic in crates/fno-agents/src/claim_cli/refresh.rs
    (Python owns transport only, per the dual-implementation protocol)."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv = [key, "--holder", holder]
    if ttl:
        argv.extend(("--ttl", ttl))
    if json_output:
        argv.append("--json")
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim refresh: the fno-agents binary was not found "
            "(reinstall fno, run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN).",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer (a CliRunner caller must see it).
    result = subprocess.run(
        [str(binary), "claim", "refresh", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@cli.command()
def status(
    key: str = typer.Argument(...),
    json_output: bool = typer.Option(False, "--json", "-J"),
    roster: bool = typer.Option(
        True,
        "--roster/--no-roster",
        help=(
            "Cross-check the fleet roster before calling a node: key free "
            "(default on). --no-roster is for a hot-path caller that only "
            "reads the claim record, e.g. the claim heartbeat."
        ),
    ),
) -> None:
    """Inspect a single claim.

    On a ``node:<id>`` key that nobody holds, the roster is cross-checked
    before the answer is rendered. ``free`` had two explanations the output
    could not tell apart - nobody is working this, and the claim never got
    taken - and four leads read the first while getting the second, then
    staffed a duplicate onto a node that already had a live worker on it.

    Only ``node:`` keys, because only they have a fleet to cross-check
    against. Every other key renders exactly as before.

    Callers parsing the JSON keep reading ``state``; roster fields are additive and only appear when the cross-check applies.

    ``--no-roster`` exists for one shape of caller: a hot path that reads only the
    claim record. ``hooks/claim-heartbeat.sh`` runs on tool calls and reads
    ``.holder`` alone, and the cross-check's whole cost lands on exactly the
    branch it hits when a claim has lapsed. Paying a harness subprocess there to
    compute a field it discards would tax every tool call to answer a question it never asked.
    """
    return _forward_status(key, json_output, roster)


def _forward_status(key, json_output, roster) -> None:
    """Forward to the bundled fno-agents binary, binary-direct.

    The port moved the leaf's logic (the state machine, the roster
    crosscheck, the verdict line, the expiry clause) into
    `crates/fno-agents/src/claim_cli/status.rs`; Python owns transport plus
    the colon-less-key refusal, per the dual-implementation protocol.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv = []
    if key is not None:
        argv.append(key)
    if json_output:
        argv.append("--json")
    if not roster:
        argv.append("--no-roster")
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim status: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer: the leaf's output IS the
    # operator surface, and a CliRunner-hosted caller (the tests) must see it.
    result = subprocess.run(
        [str(binary), "claim", "status", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    # The colon-less-key refusal stays on the Python surface: it reads the
    # same dict the legacy body refused on, so the exit-2 contract holds
    # across the swap. A payload the transport garbled leaves info empty
    # and the refusal no-ops.
    try:
        info = json.loads(result.stdout) if result.stdout else {}
    except ValueError:
        info = {}
    _refuse_unknown_status_key(_normalize_status_key(key), info)
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@cli.command(name="list")
def list_cmd(
    prefix: str = typer.Option("", "--prefix", help="Filter keys starting with this prefix"),
    include_stale: bool = typer.Option(False, "--include-stale"),
    json_output: bool = typer.Option(False, "--json", "-J"),
    root: Optional[Path] = typer.Option(
        None, "--root", help="Explicit claims root (repo root); overrides the both-roots default"
    ),
    silent_after: Optional[str] = typer.Option(
        None,
        "--silent-after",
        help="Opt-in: report LIVE claims silent this long ('15m'). Harness-neutral.",
    ),
) -> None:
    """Enumerate claims under the claims directory.

    Both claims roots (global ~/.fno/claims and the cwd-local root) are
    always read and merged in one run, --prefix or not - a bare `fno agents
    claim list` used to resolve only whichever single root an empty
    prefix happened to fall to, silently missing the other store (measured:
    574 lockfiles in a root a bare `list` could never reach).
    A colon-less or unrecognized --prefix cannot tell which root its keys
    live in (the native read returns None for exactly that case), so
    narrowing to a single guessed root would
    silently reintroduce the same miss; only an explicit --root narrows.
    """
    return _forward_list(prefix, include_stale, json_output, root, silent_after)


def _forward_list(
    prefix, include_stale, json_output, root, silent_after,
) -> None:
    """Forward to the bundled fno-agents binary, binary-direct. The leaf's
    logic lives in crates/fno-agents/src/claim_cli/list.rs (Python owns
    transport only, per the dual-implementation protocol)."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv: list[str] = []
    if prefix:
        argv.extend(("--prefix", prefix))
    if include_stale:
        argv.append("--include-stale")
    if json_output:
        argv.append("--json")
    if root is not None:
        argv.extend(("--root", str(root)))
    if silent_after is not None:
        # An empty value IS forwarded: the refusal shapes are leaf behavior
        # (a frozen golden), not transport.
        argv.extend(("--silent-after", silent_after))
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim list: the fno-agents binary was not found "
            "(reinstall fno, run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN).",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer (a CliRunner caller must see it).
    result = subprocess.run(
        [str(binary), "claim", "list", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


#: Re-exported so every caller keeps its existing import site. The definition
#: moved to :mod:`fno.claims.core`, which is where the handover branch enforces
#: it - a second copy here would be a rule and its guard drifting apart.
HANDOVER_HOLDER_PREFIX = _HANDOVER_HOLDER_PREFIX

# The sweep instruments moved to :mod:`fno.rust_binary`, beside the claim
# transport they refine. Re-exported so every caller keeps its existing
# import site - graph/cli.py's reap flow shares them with the dispatch guard.
from fno.rust_binary import (  # noqa: E402,F401 - re-exported import sites
    _abandonment_probe,
    _holder_session_id,
    _mux_pane_absent_for,
    _node_settlement,
    _transcript_says_finished,
    parse_ttl_ms,
)


@cli.command(name="reap")
def reap_cmd(
    apply: bool = typer.Option(
        False,
        "--apply",
        help="Archive dead claims. Default is dry-run: report what would be reaped.",
    ),
    root: Optional[List[Path]] = typer.Option(
        None,
        "--root",
        help="Repeatable. Overrides the default both-roots sweep "
        "(global ~/.fno/claims + the cwd-local root).",
    ),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Archive every provably-dead claim, naming the instrument that proved it.
    Expiry and liveness are measured by the claim's holder shape. Dry-run is the
    default; `--apply` archives and verifies every move before counting it.
    """
    return _forward_reap(apply, list(root) if root else [], json_output)


def _forward_reap(apply, roots, json_output) -> None:
    """Forward to the bundled fno-agents binary, binary-direct. The claims-port
    epic put the leaf's logic in crates/fno-agents/src/claim_cli/reap.rs
    (Python owns transport only, per the dual-implementation protocol)."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    argv = []
    if apply:
        argv.append("--apply")
    for path in roots:
        argv.extend(("--root", str(path)))
    if json_output:
        argv.append("--json")
    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno agents claim reap: the fno-agents binary was not found "
            "(reinstall fno, run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN).",
            err=True,
        )
        raise typer.Exit(code=127)
    # Captured and re-emitted through typer (a CliRunner caller must see it).
    result = subprocess.run(
        [str(binary), "claim", "reap", *argv],
        capture_output=True,
        text=True,
        check=False,
    )
    if result.stdout:
        typer.echo(result.stdout.rstrip("\n"))
    if result.stderr:
        typer.echo(result.stderr.rstrip("\n"), err=True)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@cli.command(name="session-pid")
def session_pid(
    from_pid: Optional[int] = typer.Option(
        None,
        "--from-pid",
        help="Start the ancestor walk here (default: this process's parent).",
    ),
    json_output: bool = typer.Option(False, "--json", "-J"),
) -> None:
    """Resolve the durable session pid (nearest harness ancestor that is not
    Claude Code pool machinery) for the hybrid liveness pid-arm. Prints the
    pid on stdout, or nothing when uncapturable (plain-shell / no harness
    ancestor / a pooled bg-spare; the caller degrades to TTL-only liveness).
    Always exit 0 - a missing pid is a safe degrade, not an error.

    The walk is native: this command fronts the fno-agents binary, which owns
    the one implementation (`spawn_context::session_identity_ambient`). The
    Python module `session_pid` is a SHIM over this very verb, so importing it
    here would recurse.
    """
    import subprocess as _subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        # Uncapturable: empty stdout, exit 0 (the shim's degrade shape).
        if json_output:
            typer.echo(json.dumps({"session_pid": None, "harness": None}))
        return
    cmd = [str(binary), "claim", "session-pid"]
    if from_pid is not None:
        cmd += ["--from-pid", str(from_pid)]
    if json_output:
        cmd.append("--json")
    try:
        result = _subprocess.run(  # noqa: S603 - resolved binary, fixed verb
            cmd, capture_output=True, text=True, check=False
        )
    except OSError:
        if json_output:
            typer.echo(json.dumps({"session_pid": None, "harness": None}))
        return
    if json_output:
        try:
            payload = json.loads(result.stdout)
        except ValueError:
            payload = {"session_pid": None, "harness": None}
        typer.echo(json.dumps(payload))
    elif result.stdout.strip():
        typer.echo(result.stdout.strip())
    # Always exit 0 - this command always exits 0.


def _acquire_lane(*, lane: str, max_lanes: int, ttl: str, json_output: bool) -> None:
    """The former `claim lane-acquire`, now the --lane mode of `claim acquire`.

    Exit 1 when the cap is full (no free slot) - the same "retry later" code as
    a held claim. The cap is enforced by atomic slot acquisition, never a count.
    """
    import subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo("Error: no fno-agents binary", err=True)
        raise typer.Exit(code=1)
    argv = [str(binary), "claim", "lane-acquire", "--lane", lane,
            "--max-lanes", str(max_lanes), "--ttl", ttl or "1h"]
    if json_output:
        argv.append("--json")
    result = subprocess.run(argv, capture_output=True, text=True)
    if result.stdout.strip():
        typer.echo(result.stdout.strip())
    if result.returncode == 2 and result.stderr.strip():
        typer.echo(f"validation error: {result.stderr.strip()}", err=True)
        raise typer.Exit(code=2)
    if result.returncode != 0:
        typer.echo((result.stderr or f"lane cap full (max_lanes={max_lanes})").strip(), err=True)
        raise typer.Exit(code=result.returncode or 1)


__all__ = ["cli"]


def _normalize_status_key(key: str) -> str:
    from fno.graph._constants import is_wellformed_node_id

    return f"node:{key}" if is_wellformed_node_id(key) else key


def _refuse_unknown_status_key(key: str, info: Mapping) -> None:
    if ":" in key or info.get("state") != ClaimState.FREE.value:
        return
    typer.echo(
        f"claim key must include a recognized prefix; no claim exists for {key!r}", err=True
    )
    raise typer.Exit(2)


def _owned_registry_effort(harness: str, session_id: str) -> "str | None":
    """Read effort through the platform path without importing runtime agents.

    Claims are core (L1), while the registry is runtime (L5). The registry is
    an atomic JSON store, so this narrow best-effort read can match only the
    owned identity; unreadable or malformed state leaves effort unknown.
    """
    if not harness or not session_id:
        return None
    try:
        from fno.registry_door import RegistryDoorError, read_registry_document

        raw = read_registry_document()[0]
    except (OSError, TypeError, ValueError, RegistryDoorError):
        return None
    rows = raw.get("agents") if isinstance(raw, dict) else None
    if not isinstance(rows, list):
        return None
    legacy_session_fields = {
        "claude": "claude_session_uuid",
        "codex": "codex_session_id",
        "gemini": "gemini_session_id",
    }
    for row in rows:
        if not isinstance(row, dict):
            continue
        row_harness = row.get("harness") or row.get("provider")
        if row_harness != harness:
            continue
        row_session = row.get("harness_session_id")
        if not isinstance(row_session, str) or not row_session:
            legacy_key = legacy_session_fields.get(harness)
            row_session = row.get(legacy_key) if legacy_key else None
        if row_session != session_id:
            continue
        value = row.get("effort")
        return value.strip() if isinstance(value, str) and value.strip() else None
    return None
