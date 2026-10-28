"""Spawn gate : global concurrency cap + free-RAM floor + queue loop.

Called at the top of ``cmd_spawn`` before the substrate fan-out. Mirrors
``crates/fno-agents/src/spawn_gate.rs`` — the two gates sit on mutually
exclusive execution paths (the front door execs the binary for bg/headless;
the Rust ``pane`` arm re-execs this CLI), so every spawn passes exactly one.

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
import json
import os
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, NamedTuple, NoReturn, Optional, cast


class Admission(NamedTuple):
    """The CPU axis's decision, computed once and read by both gates (LD1).

    ``verdict`` is ``admit`` | ``hold`` | ``undecidable`` | ``refuse``;
    ``axis`` names what decided: the fleet's CPU share or an unreadable
    instrument. ``reason`` is the full sentence
    the gates print verbatim. An attribution gap widens the share to an
    interval: ``share_low`` is the attributed share, ``share_high`` the
    whole machine's, and ``bound`` records that the verdict read the upper
    bound. The native census computes it; this type carries it.
    """

    verdict: str
    axis: str
    reason: str
    share_low: float
    share_high: float
    bound: str
    fleet_cores: float
    machine_cores: float
    capacity_cores: float
    ceiling: float
    gap: str | None = None
    top_holder: str | None = None

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


#: The refusal reasons a caller may outlast by retrying (spawn --wait). Owned
#: HERE because these tokens are the gate's vocabulary; the CLI imports this
#: set rather than re-spelling it. no_wait/no_wait_mutex_held surface only
#: when an attempt runs no_wait (spawn --wait forces that), so the CLI
#: deadline - not this gate's 600s queue - bounds the wait.
WAITABLE_REFUSAL_REASONS = frozenset(
    {
        "ram_floor", "swap_pressure",
        "cpu_instrument_unreadable",
        "cpu_share_undecidable", "fleet_cpu_share", "provider_cap",
        "max_live", "no_wait", "no_wait_mutex_held",
    }
)

QUEUE_POLL_S = 2.0
QUEUE_PROGRESS_EVERY_S = 30.0
QUEUE_TIMEOUT_S = 600.0
#: LD4: hold re-sample gap and admission debounce (the macOS `ps`
#: CPU column is a decaying average; two reads 2s apart are one sample).
CPU_HOLD_POLL_S = 15.0
CPU_ADMIT_SAMPLES = 2
#: : a slow bg-socket census names its own wait instead of silence.
SLOW_SCAN_WARN_S = 5.0
#: The mutex claim key. Prefixed so the native leg routes it to the global
#: root the gate writes; the old colon-less `spawn-gate` key unrouted, so
#: `claim status`/`release --force` read `<space>/claims/spawn-gate.lock`
#: while the gate held `~/.fno/claims/spawn-gate.lock` and both lied.
GATE_CLAIM_KEY = "gate:spawn"
WORKER_CLAIM_TTL_MS = 4 * 60 * 60 * 1000
CLAIM_RELEASE_ATTEMPTS = 3

#: Registry statuses that can hold a live process. `idle` counts when the pid
#: is alive (an unreaped idle process still holds RAM); a reaped pid drops out
#: via the liveness check — the reaper is our slot-release mechanism.
LIVE_STATUSES = frozenset(
    {"spawning", "ready", "idle", "busy", "live", "restarting"}
)


def _warn(msg: str) -> None:
    print(msg, file=sys.stderr)


# ---------------------------------------------------------------------------
# Layer 2: available RAM
# ---------------------------------------------------------------------------

def available_ram_gb() -> Optional[float]:
    """Available system RAM in GB, or None when unreadable (guard skipped)."""
    try:
        import psutil

        return psutil.virtual_memory().available / (1024.0**3)
    except Exception:
        return None


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


@dataclass(frozen=True)
class LoadSnapshot:
    # display/trend only; nothing gates on these.
    load_1m: float | None
    load_cpu_count: int
    load_5m: float | None = None
    load_15m: float | None = None


class ProviderCountUnavailable(RuntimeError):
    """A provider count cannot be proved from registry/liveness evidence."""


_KNOWN_UNROUTED_PROVIDER = "__uncapped__"
_PROVIDER_ADMISSION_TOKEN = object()


def _gate_claims_root() -> Path:
    from fno.claims.io import global_claims_root

    return global_claims_root()


# ---------------------------------------------------------------------------
# The gate
# ---------------------------------------------------------------------------

@dataclass
class GateGuard:
    """Held gate state. The caller keeps this across dispatch and calls
    ``release()`` when the dispatch result (registry row / roster receipt)
    exists — for headless, the worker slot claim outlives the mutex."""

    _gate_holder: Optional[str] = None
    _worker_key: Optional[str] = None
    _worker_holder: Optional[str] = None
    _route_provider: Optional[str] = None
    _spawn_name: Optional[str] = None
    _substrate: Optional[str] = None
    _admission_token: object | None = None
    _consumed: bool = False
    _released: bool = False

    def _consume_provider(self, provider: str, name: str, substrate: str) -> bool:
        authorized = (
            self._admission_token is _PROVIDER_ADMISSION_TOKEN
            and self._route_provider == provider
            and self._spawn_name == name
            and self._substrate == substrate
            and not self._consumed
            and not self._released
        )
        if authorized:
            self._consumed = True
        return authorized

    def retain_revived_worker(
        self,
        short_id: str,
        *,
        worker_name: Optional[str] = None,
        worker_pid: Optional[int] = None,
        positive_marker: str = "claude-respawn-ok",
    ) -> None:
        """Convert a BG admission into durable fail-closed worker evidence."""
        if self._route_provider is None or self._spawn_name is None:
            raise ProviderCountUnavailable("provider admission identity unavailable")
        holder = self._gate_holder or f"spawn-gate:{os.getpid()}:{self._spawn_name}"
        _acquire_worker_slot(
            self,
            worker_name or self._spawn_name,
            holder,
            self._route_provider,
            fail_closed=True,
            worker_pid=worker_pid,
            metadata={
                "session_short_id": short_id,
                "positive_marker": positive_marker,
            },
        )

    def release_worker_reservation(self) -> None:
        if self._worker_key is None:
            return
        key = self._worker_key
        if not _release_claim_bounded(key, self._worker_holder or ""):
            return
        self._worker_key = None
        self._worker_holder = None

    def release_gate_mutex(self) -> None:
        if self._gate_holder is None:
            return
        holder = self._gate_holder
        if not _release_claim_bounded(GATE_CLAIM_KEY, holder):
            return
        self._gate_holder = None

    def release(self) -> None:
        self._released = True
        self.release_gate_mutex()
        self.release_worker_reservation()


def consume_provider_admission(
    guard: object, provider: str, name: str, substrate: str
) -> bool:
    """Consume one genuine opaque admission; duck-typed substitutes never pass."""
    return isinstance(guard, GateGuard) and guard._consume_provider(
        provider, name, substrate
    )


def _release_claim_bounded(key: str, holder: str) -> bool:
    """Release a gate claim with a short retry budget for transient store faults."""
    from fno.claims.core import release_claim

    last_error: Exception | None = None
    for attempt in range(CLAIM_RELEASE_ATTEMPTS):
        try:
            release_claim(key, holder, root=_gate_claims_root())
            return True
        except Exception as exc:
            last_error = exc
            if attempt + 1 < CLAIM_RELEASE_ATTEMPTS:
                time.sleep(0.01)
    label = "gate mutex" if key == GATE_CLAIM_KEY else f"worker reservation {key}"
    _warn(f"spawn-gate note: could not release {label}: {last_error}")
    return False


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


#: `(None, "error")` is a real reading ("unreadable"), so the "not supplied"
#: case needs a value that cannot be confused with it.
_NOT_PREFETCHED: object = object()


def _prefetch_fleet_reading() -> tuple[Optional[Any], Optional[str]]:
    """Take the footprint reading OUTSIDE the gate mutex, ALWAYS.

 LD2: every spawn takes the reading. A trigger that fires on a
    number the node proved does not track the work is not a cost
    optimisation, it is a second decider. The read is one `ps` snapshot
    behind a deadline, and the gate mutex serializes every spawner on the
    machine, so it is taken before the lock exactly as before - the band
    check that sometimes skipped it is what died.

    Returns ``(payload, error)``; exactly one side is usable. A ``None``
    payload is a REFUSAL on ``cpu_instrument``, never a skip (LD3).
    """
    try:
        from fno.rust_binary import resolve_binary

        binary = resolve_binary()
        if binary is None:
            return None, "footprint unavailable: the fno-agents binary was not found"
        result = subprocess.run(
            [str(binary), "census", "--cause", "--json"],
            capture_output=True,
            text=True,
            timeout=30,
            check=False,
        )
    except Exception as exc:  # noqa: BLE001 - an import fault is an unreadable instrument
        return None, f"footprint unavailable: {exc}"
    if result.returncode != 0:
        lines = (result.stderr or "").strip().splitlines()
        tail = lines[-1] if lines else ""
        return None, f"footprint unavailable: exit {result.returncode}{tail}"
    try:
        return json.loads(result.stdout or "{}"), None
    except ValueError as exc:
        return None, f"footprint unavailable: {exc}"


def _cpu_axis(prefetched: object = _NOT_PREFETCHED) -> Admission:
    """The CPU axis's verdict for THIS spawn: one decider, no second opinion.

    Maps an unreadable instrument to ``refuse`` on ``cpu_instrument`` (LD3:
    the sensor blinds under exactly the load it measures, and an unreadable
    process table is itself a symptom) and otherwise reads the admission
    the native census already computed (one decider: the census op).

    Shared with the ``--explain`` preview, so a dry run answers the question
    the real spawn will.
    """
    payload, error = (
        _prefetch_fleet_reading()
        if prefetched is _NOT_PREFETCHED
        else cast("tuple[Optional[Any], Optional[str]]", prefetched)
    )
    if payload is None:
        why = (error or "the reading failed").strip()
        return Admission(
            verdict="refuse",
            axis="cpu_instrument",
            reason=(
                f"spawn-gate: the CPU instrument is unreadable ({why}); "
                "refusing to spawn (--force to bypass)"
            ),
            share_low=0.0,
            share_high=0.0,
            bound="exact",
            fleet_cores=0.0,
            machine_cores=0.0,
            capacity_cores=0.0,
            ceiling=0.0,
            gap=None,
        )
    admission = payload.get("admission") or {}
    return Admission(
        verdict=str(admission.get("verdict") or "refuse"),
        axis="fleet_cpu",
        reason=str(admission.get("reason") or "the reading failed"),
        share_low=float(admission.get("share_low") or 0.0),
        share_high=float(admission.get("share_high") or 0.0),
        bound=str(admission.get("bound") or "exact"),
        fleet_cores=float(admission.get("fleet_cores") or 0.0),
        machine_cores=float(admission.get("machine_cores") or 0.0),
        capacity_cores=float(admission.get("capacity_cores") or 0.0),
        ceiling=float(admission.get("ceiling") or 0.0),
        gap=admission.get("gap"),
    )


def _load_cpus() -> int:
    """The CPU denominator for the DISPLAY load snapshot (nothing gates on
    it). The native census applies its quota-aware capacity itself; the
    display denominator falls back to the interpreter's count.
    """
    return getattr(os, "process_cpu_count", os.cpu_count)() or 1


def _load_snapshot(max_load_per_cpu: float) -> LoadSnapshot:
    """The display/trend load reading. The retired per-cpu argument stays so
    footprint's degraded-snapshot path keeps its shape; nothing gates here."""
    del max_load_per_cpu
    cpus = _load_cpus()
    try:
        load1, load5, load15 = os.getloadavg()
    except (OSError, AttributeError):
        return LoadSnapshot(load_1m=None, load_cpu_count=cpus)
    return LoadSnapshot(
        load_1m=load1,
        load_cpu_count=cpus,
        load_5m=load5,
        load_15m=load15,
    )


def _acquire_worker_slot(
    guard: GateGuard,
    name: str,
    holder: str,
    route_provider: Optional[str] = None,
    *,
    fail_closed: bool = False,
    worker_pid: Optional[int] = None,
    metadata: Optional[dict[str, object]] = None,
) -> None:
    key = f"worker:{name}"
    try:
        from fno.claims.core import acquire_claim

        claim_metadata: dict[str, object] = {
            "model_provider": route_provider or _KNOWN_UNROUTED_PROVIDER
        }
        if metadata:
            claim_metadata.update(metadata)
        acquire_claim(
            key,
            holder,
            ttl_ms=WORKER_CLAIM_TTL_MS,
            metadata=claim_metadata,
            pid=worker_pid,
            root=_gate_claims_root(),
        )
        guard._worker_key = key
        guard._worker_holder = holder
    except Exception as exc:
        if fail_closed:
            raise ProviderCountUnavailable(
                f"worker reservation {key} unavailable: {exc}"
            ) from exc
        # Fail open: a slot claim is count VISIBILITY, not a correctness gate.
        _warn(f"spawn-gate note: worker slot claim {key} unavailable; proceeding uncounted")


def _call_gate_command(payload: dict) -> dict:
    """One round trip to the Rust gate (``fno-agents spawn-gate``, mode
    ``gate``). The child's stderr stays attached to this process, so a gate
    that queues streams its ``spawn queued: ...`` prose live. The timeout
    bounds the command's own 600s queue plus prose and startup.
    """
    from fno.rust_binary import command_call

    return command_call(
        "spawn-gate",
        payload,
        timeout=QUEUE_TIMEOUT_S + 100.0,
        passthrough_stderr=True,
    )


def run_gate(
    name: str,
    substrate: str,
    *,
    force: bool = False,
    no_wait: bool = False,
    route_provider: Optional[str] = None,
    account: Optional[str] = None,
    caller: object = ...,
    seed: Optional[str] = None,
    session_phase: Optional[str] = None,
    succession_scope: Optional[str] = None,
) -> GateGuard:
    """Run the full gate - by asking the ONE gate in the binary. Returns a
    :class:`GateGuard` to hold across dispatch on pass; raises
    :class:`GateRefused` (a SystemExit) on refusal/timeout.

    This is a TRANSPORT, not a second gate: the axes (fleet incident, schema,
    quota lock, provider cap, CPU, slots, RAM, lead share) are decided inside
    ``crates/fno-agents/src/spawn_gate.rs``. This side carries the caller's
    identity and raw seed/phase inputs in, then carries refusal data out. The
    refusal event still emits from here (locked decision 5), so journal
    population is unchanged for
    spawns that enter Python.
    """
    # Set before the first branch that can refuse, so every refusal event in
    # this run names the spawn it refused (see _CURRENT_SPAWN).
    _CURRENT_SPAWN.set((name, substrate))
    # The calling lead's session id (W4), resolved through the same
    # self-identity source that stamps `spawned_by_session` onto the spawned
    # row, so the gate attributes a spawn exactly the way the row will.
    try:
        from fno.claims.self_identity import resolve_self_identity

        caller_session = resolve_self_identity().session_id
    except Exception:  # noqa: BLE001 - no identity, no share check (an
        # operator-run spawn is not competing for the commons)
        caller_session = None
    caller_session = caller_session if caller is ... else caller  # revival names its row's parent
    payload = {
        "mode": "gate",
        "name": name,
        "substrate": substrate,
        "force": force,
        "no_wait": no_wait,
        "route_provider": route_provider,
        "account": account,
        "seed": seed,
        "session_phase": session_phase,
        "succession_scope": succession_scope,
        "caller_session": caller_session,
        "holder_pid": os.getpid(),
    }
    try:
        answer = _call_gate_command(payload)
    except Exception as exc:  # noqa: BLE001 - an unanswered gate never admits
        _warn(f"spawn-gate: refused on gate (gate_unavailable, exit {EXIT_GATE_UNAVAILABLE}): {exc}")
        _refuse(
            EXIT_GATE_UNAVAILABLE,
            {
                "status": "refused",
                "reason": "gate_unavailable",
                "error": str(exc),
            },
        )
    if answer.get("status") == "admitted":
        return GateGuard(
            _gate_holder=answer.get("gate_holder"),
            _worker_key=answer.get("worker_key"),
            _worker_holder=answer.get("worker_holder"),
            _route_provider=route_provider,
            _spawn_name=name,
            _substrate=substrate,
            _admission_token=_PROVIDER_ADMISSION_TOKEN,
        )
    # The command produced an answer, and the answer is a refusal: the exit code
    # and the receipt travelled inside the answer (a refusal is data).
    _refuse(
        int(answer.get("exit_code", EXIT_GATE_UNAVAILABLE)),
        answer.get("receipt"),
        **(answer.get("event") or {}),
    )


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


def qos_demote_pid(pid: int) -> None:
    """Best-effort post-hoc demotion of an already-running pid. Non-fatal."""
    if not _qos_enabled():
        return
    import subprocess

    if sys.platform == "darwin":
        cmd = ["/usr/sbin/taskpolicy", "-b", "-p", str(pid)]
    elif sys.platform.startswith("linux"):
        cmd = ["/usr/bin/renice", "10", "-p", str(pid)]
    else:
        return
    try:
        rc = subprocess.run(
            cmd, capture_output=True, timeout=10, check=False
        ).returncode
        if rc != 0:
            raise RuntimeError(f"exit {rc}")
    except Exception:
        _warn(f"spawn-gate note: QoS demotion of pid {pid} failed (non-fatal)")


def qos_demote_bg_worker(job_id: str, *, poll_s: float = 10.0) -> None:
    """After a ``--substrate thread`` dispatch, poll the roster briefly for the
    new worker's pid and demote it post-hoc. ``job_id`` is the claude bg jobId
    (the registry ``short_id``). Bounded; one warning on miss."""
    if not job_id or not _qos_enabled():
        return
    deadline = time.monotonic() + poll_s
    while True:
        try:
            raw = json.loads(_roster_path().read_text(encoding="utf-8"))
            workers = raw.get("workers", {}) if isinstance(raw, dict) else {}
            for w in workers.values():
                if not isinstance(w, dict):
                    continue
                sid = str(w.get("sessionId") or "")
                if sid.split("-")[0] == job_id and isinstance(
                    w.get("pid"), int
                ):
                    qos_demote_pid(w["pid"])
                    return
        except Exception:
            pass
        if time.monotonic() >= deadline:
            _warn(
                f"spawn-gate note: bg worker {job_id} pid not in roster "
                f"within {int(poll_s)}s; QoS demotion skipped (non-fatal)"
            )
            return
        time.sleep(0.5)
