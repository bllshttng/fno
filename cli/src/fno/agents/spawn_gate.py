"""Spawn gate readers shared with the pane lane, watchdog and reachability.

``crates/fno-agents/src/spawn_gate.rs`` gates every ``fno agents spawn``;
this module keeps the Python readers and the provider admission the pane
lane still consumes.

The gate is READ-ONLY: the ``max_live`` slot cap counts fno registry rows
(worker provenance) and the RAM floor reads real system RAM. The claude daemon
roster is never a population to count toward the slot cap (: only a row
ALSO in the fno registry counts). Its only writes are its own claims under the
GLOBAL claims root - the RAM budget is machine-wide. Global guards fail OPEN
on read errors; the per-provider cap is stricter: an unreadable live count
refuses, never assumes zero.
"""
from __future__ import annotations

import contextvars
import os
import sys
from pathlib import Path
from typing import Any, NoReturn, Optional


# THE exit-code allocation table for the gate band across both trees (this
# file and crates/fno-agents/src/spawn_gate.rs). A value >= 64 claims its
# number once; the same NAME at the same number in both trees is byte-parity,
# and cli/tests/unit/test_exit_code_allocation.py fails any duplicate. The
# convention band stays outside the claim: small ints 0-5 and 13-25 repeat per
# command by design, and 124/127/137/143 mirror the standard timeout/signal codes.
#   75-77, 79   capacity refusals, both gates (queue, no-wait, RAM, load)
#   78          provider cap; the quota lock and the lane faults keep it so
#               exit-code consumers are unaffected
#   80, 81      lead share, registry schema
#   82, 83      fleet incident stop pair, both gates (byte-parity)
#   84          state root ungranted. Permanent until a human grants.
#   85          Python sandbox probe: sandbox unreachable.
#   86          territory cap (Rust gate only)
#   87          the spawn-gate transport could not get an answer at all (the
#               gate command missing, failed, or timed out); fail closed, never
#               admit on an unreadable gate.
#   88          blueprint thread cap (Rust gate only)
#   90, 91      Rust fleet-incident check command (fleet_incident.rs).
#   92          node-occupancy refusal: a live worker is on the spawn's
#               node, claimed or not (Rust gate only).
EXIT_QUEUE_TIMEOUT = 75
EXIT_NO_WAIT = 76
EXIT_RAM_REFUSED = 77
EXIT_PROVIDER_CAP = 78
EXIT_LOAD_REFUSED = 79
EXIT_LEAD_SHARE = 80
EXIT_REGISTRY_SCHEMA = 81
EXIT_FLEET_STOP = 82
EXIT_FLEET_STOP_UNAVAILABLE = 83
# Rust gate only (crates/fno-agents/src/spawn_gate.rs): the lane declares
# nothing about how it stands toward the fno state root.
EXIT_STATE_ROOT_UNGRANTED = 84
# Rust gate only (crates/fno-agents/src/spawn_gate.rs)
EXIT_TERRITORY_CAP = 86
EXIT_BLUEPRINT_CAP = 88
EXIT_GATE_UNAVAILABLE = 87
# Rust gate only (crates/fno-agents/src/spawn_gate.rs): fleet top shows a
# live worker on the spawn's node.
EXIT_NODE_OCCUPIED = 92
# Rust gate only (crates/fno-agents/src/spawn_gate.rs): the rule engine's
# door:spawn read refused the spawn.
EXIT_RULE_REFUSED = 93


#: Registry statuses that can hold a live process. `idle` counts when the pid
#: is alive (an unreaped idle process still holds RAM); a reaped pid drops out
#: via the liveness check — the reaper is our slot-release mechanism.
LIVE_STATUSES = frozenset(
    {"spawning", "ready", "idle", "busy", "live", "restarting"}
)


# ---------------------------------------------------------------------------
# Layer 2: available RAM
# ---------------------------------------------------------------------------

# ---------------------------------------------------------------------------
# Layer 1: the union live-count
# ---------------------------------------------------------------------------

def _process_start_time(pid: int, _psutil=None) -> Optional[int]:
    """Return the process-incarnation token in the Rust registry's units."""
    if sys.platform.startswith("linux"):
        try:
            stat = Path(f"/proc/{pid}/stat").read_text(encoding="utf-8")
            return int(stat.rsplit(")", 1)[1].split()[19])
        except (OSError, ValueError, IndexError):
            return None
    if sys.platform == "darwin":
        try:
            if _psutil is None:
                import psutil as _psutil
            return int(round(_psutil.Process(pid).create_time() * 1_000_000))
        except Exception:
            return None
    return None


def _pid_alive(
    pid: Optional[int], recorded_start: Optional[int], *, _psutil=None
) -> Optional[bool]:
    """Return process liveness, or ``None`` when incarnation proof is unreadable.

    A recorded process-start token makes PID reuse fail closed. Legacy rows
    without a token retain the existence check and rely on their hosting
    substrate for the additional incarnation proof.
    """
    # Reject an out-of-range pid outright. Beyond being absurd, it is the value
    # that turns a signal into a broadcast once it reaches a signed pid_t (the
    # Rust probe's twin guard): 4294967295 wraps to -1, "every process I may
    # signal". Nothing downstream should get the chance.
    if not pid or pid <= 1 or pid > 0x7FFFFFFF:
        return False
    try:
        if _psutil is None:
            import psutil as _psutil

        proc = _psutil.Process(pid)
        if not proc.is_running() or proc.status() == _psutil.STATUS_ZOMBIE:
            return False
        current_start = _process_start_time(pid, _psutil)
        if recorded_start is not None:
            if current_start is None:
                return None
            return current_start == recorded_start
        return True
    except Exception as exc:
        # "Gone" is the ONLY confident death. Anything else - psutil missing,
        # AccessDenied on a process this uid cannot inspect - is unreadable, and
        # returning False there would present an undecidable case to callers as a
        # decided death, which reconcile writes through as `orphaned`.
        if _psutil is not None and isinstance(exc, _psutil.NoSuchProcess):
            return False
        try:
            import psutil

            if isinstance(exc, psutil.NoSuchProcess):
                return False
        except Exception:  # noqa: BLE001 -- no psutil means no verdict, not death
            pass
        return None


def _roster_path() -> Path:
    override = os.environ.get("FNO_CLAUDE_DAEMON_DIR")
    base = Path(override) if override else Path.home() / ".claude" / "daemon"
    return base / "roster.json"


_KNOWN_UNROUTED_PROVIDER = "__uncapped__"
# ---------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------

class GateRefused(SystemExit):
    """Raised (as SystemExit subclass) when the gate refuses the spawn."""

    def __init__(self, code: int, receipt: Optional[dict[str, object]] = None) -> None:
        super().__init__(code)
        self.receipt = receipt


#: The spawn `run_gate` is currently deciding, as ``(name, substrate)``.
#: Set once at gate entry and read by :func:`_refuse`, so a refusal event can
#: name the spawn it refused without threading `name` through four helper
#: signatures that have no other use for it. A leftover value is harmless: the
#: next `run_gate` overwrites it, and only a refusal inside a gate run reads it.
_CURRENT_SPAWN: "contextvars.ContextVar[tuple[Optional[str], Optional[str]]]" = (
    contextvars.ContextVar("fno_spawn_gate_current", default=(None, None))
)

#: AC13: axes read so far plus the axis being decided, stamped onto
#: refusals that carry no explicit axis fields.
_CURRENT_AXES_READ: "contextvars.ContextVar[dict[str, str]]" = (
    contextvars.ContextVar("fno_spawn_gate_axes_read", default={})
)
_CURRENT_AXIS: "contextvars.ContextVar[Optional[str]]" = (
    contextvars.ContextVar("fno_spawn_gate_axis", default=None)
)


def _refuse(
    exit_code: int,
    receipt: Optional[dict[str, object]] = None,
    **event: Any,
) -> NoReturn:
    """The one seam every gate refusal exits through: emit, then raise.

    Before this existed a refusal lived only in the stderr of a process that
    had already exited, so nobody could ask afterward why a node did not
    launch. Measured 2026-09-01: the global journal carried 4815
    ``claim_acquired`` rows (the positive control that the file is read) and
    zero rows of any kind naming a gate refusal.

    ``receipt`` is the caller-facing shape ``fno agents spawn`` prints on
    stdout and is passed through untouched. Every event carries
    ``gate: "python"`` and the spawn's ``substrate``, so a reader can tell
    this journal's population (the pane substrate, the sole leg this gate
    covers) from the Rust gate's refusals, which owns.

    ``event`` is extra telemetry for
    the refusals that deliberately carry no receipt, so a refusal can name
    its measured value against its threshold in the log without changing
    what stdout has always printed.

    The emit is best-effort - ``_emit_gate_event`` swallows everything - so
    telemetry can never change a gate outcome.
    """
    spawn_name, substrate = _CURRENT_SPAWN.get()
    event_data = {**(receipt or {}), **event}
    # AC13: an explicit axis field wins; the contextvar is the best
    # effort a non-CPU refusal site can supply.
    axes_read = _CURRENT_AXES_READ.get()
    if axes_read and "axes_read" not in event_data:
        event_data["axes_read"] = dict(axes_read)
    axis = _CURRENT_AXIS.get()
    if axis and "axis" not in event_data:
        event_data["axis"] = axis
    # The seam-owned fields win on collision: a future receipt carrying `name`
    # or `gate` must not silently rewrite the identity this journal entry is
    # attributed by.
    event_data.update(
        exit_code=exit_code, name=spawn_name, substrate=substrate, gate="python"
    )
    _emit_gate_event("spawn_gate_refused", **event_data)
    refusal = GateRefused(exit_code, receipt)
    raise refusal


def _emit_gate_event(kind: str, **data: Any) -> None:
    """Best-effort agents-log event. Never raises, never blocks a spawn."""
    try:
        from fno.agents import events

        events.emit(kind, **data)
    except Exception:  # noqa: BLE001 - telemetry never changes a gate outcome
        pass


def probe_capacity(only: Optional[list[str]] = None) -> dict:
    """Answer "would a dispatch be admitted right now" - by asking the ONE
    gate's read-only probe mode. No mutex, no reservations, no refusal events.
    Never raises: an unanswered gate returns ``verdict: unknown``, never
    saturation, and the measurement blocks (``lanes``/``share``/``rows``)
    carry whatever the probe managed to read. ``only=["lanes"]`` skips the
    CPU and RAM reads for callers already on the spawn path (route
    resolution), where a footprint probe costs seconds under load.
    """
    try:
        from fno.claims.self_identity import resolve_self_identity

        caller = resolve_self_identity().session_id
    except Exception:  # noqa: BLE001 - no identity, no share check
        caller = None
    from fno.rust_binary import command_call

    try:
        return command_call(
            "spawn-gate", {"mode": "probe", "caller_session": caller, "only": only}
        )
    except Exception as exc:  # noqa: BLE001 - an unanswered gate is unknown
        return {"verdict": "unknown", "reason": "gate_unavailable", "error": str(exc)}



# ---------------------------------------------------------------------------
# Layer 3: background QoS
# ---------------------------------------------------------------------------

def _qos_enabled() -> bool:
    try:
        from fno.config import load_settings

        return load_settings().agents.worker_qos != "off"
    except Exception:
        return True


def qos_wrap(argv: list[str]) -> list[str]:
    """Exec-wrap a child command at background priority when
    ``config.agents.worker_qos`` is ``utility``. Identity on ``off``.

    Absolute wrapper paths + existence check: a missing wrapper degrades to
    an unwrapped exec (fail open), never a spawn failure.
    """
    if not argv or not _qos_enabled():
        return argv
    # Don't wrap a command that won't resolve: a missing provider CLI must
    # surface as its own NotFound, not the wrapper's error.
    import shutil

    target = argv[0]
    if ("/" in target and not os.path.exists(target)) or (
        "/" not in target and shutil.which(target) is None
    ):
        return argv
    if sys.platform == "darwin" and os.path.exists("/usr/sbin/taskpolicy"):
        return ["/usr/sbin/taskpolicy", "-c", "utility", "--"] + argv
    if sys.platform.startswith("linux") and os.path.exists("/usr/bin/nice"):
        return ["/usr/bin/nice", "-n", "10"] + argv
    return argv

