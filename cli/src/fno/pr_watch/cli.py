"""fno do pr watch CLI surface.

Four commands:
  tick      - the launchd entry; builds real adapters and calls tick()
  install   - render + gate-confirm + write global LaunchAgent plist
  uninstall - unload (best-effort) + remove plist; preserve watermark store
  status    - report loaded/unloaded, last tick, open-PR count, parked PRs

Logic lives in _install.py; this module stays thin (Typer glue only).
"""
from __future__ import annotations

import json
import logging
import os
import shutil
import signal
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Optional

import typer

log = logging.getLogger(__name__)

cli = typer.Typer(
    name="pr-watch",
    help="PR-state watcher: auto-fire /fno:ship pr check + /fno:ship pr merged for open-PR backlog nodes.",
    no_args_is_help=True,
)

def _resolve_fno_binary() -> str:
    """Return the absolute path to the fno binary.

    Tries shutil.which first; falls back to the console-script alongside
    the current interpreter (handles ``uv run --project cli fno-py`` dev use).
    Resolves `fno-py` (the console script); the Rust mux binary owns `fno`.
    """
    found = shutil.which("fno-py")
    if found:
        return os.path.abspath(found)
    # Fallback: the entry-point next to the running Python interpreter
    candidate = Path(sys.executable).parent / "fno-py"
    if candidate.exists():
        return str(candidate)
    return "fno-py"  # last resort: bare name (launchd may still find it via PATH)


# ---------------------------------------------------------------------------
# Module-level adapter callables (extracted for testability)
# ---------------------------------------------------------------------------


def _emit_event(
    event_type: str, data: dict[str, Any], *, events_path: Optional[Path] = None
) -> bool:
    """Append a canonical event envelope to events.jsonl.

    Uses fno.events._build + fno.events.append_event (the same path the
    ``fno doctor event emit`` CLI command uses internally).  On failure, logs a warning
    instead of silently passing so the failure is observable.

    When no explicit ``events_path`` is given, defaults to
    ``state_dir()/events.jsonl`` -- the same global path that the status
    command's watermark scan reads from and that the watermark
    store anchors to.  This makes the daemon cwd-independent: launchd
    starts the daemon in ``/`` with no WorkingDirectory, so any cwd-relative
    path (e.g. ``Path(".fno/events.jsonl")``) would be silently lost.
    """
    if events_path is None:
        try:
            from fno.paths import state_dir
            events_path = state_dir() / "events.jsonl"
        except Exception as exc:
            log.warning("pr-watch: could not resolve state_dir for events path: %s", exc)
            return False
    try:
        from fno.events import _build, append_event
        event = _build(event_type, "daemon", data)
        append_event(event, events_path)
        return True
    except Exception as exc:
        log.warning("pr-watch: emit %s failed: %s", event_type, exc)
        return False


def _bounce_sender(window_s: float = 15.0) -> str:
    """The bounce that just sent this SIGTERM, from its receipt; else unrecorded.

    A hand ``kill -TERM``, or a sidecar older than ``window_s`` from an earlier
    bounce, reads ``unrecorded`` rather than blaming the wrong cure. Never raises.
    """
    try:
        import time
        from fno.pr_watch._install import bounce_sidecar_path

        raw = json.loads(bounce_sidecar_path().read_text(encoding="utf-8"))
        if time.time() - float(raw["ts"]) <= window_s:
            return f"{raw['caller']} pid {raw['pid']} via {raw.get('parent', '')}".rstrip()
    except Exception:  # noqa: BLE001 - evidence, never a gate
        pass
    return "unrecorded"


#: The launchd label every phase in this tick rides on; each tick row names it.
_PR_WATCH_SCHEDULER = "launchd:sh.fno.pr-watcher"


def _emit_tick_row(arm: str, *, interval_s: int, acted: int = 0,
                   skip_reason: Optional[str] = None, detail: Optional[str] = None) -> None:
    """One arm row per phase outcome (never raises); rides ``_emit_event`` so
    the ``_no_global_tick_events`` fixture captures it."""
    data: dict[str, Any] = {"arm": arm, "scheduler": _PR_WATCH_SCHEDULER,
                            "acted": acted, "interval_s": int(interval_s)}
    if skip_reason is not None:
        data["skip_reason"] = skip_reason
    if detail is not None:
        # Same bound as fno.control_plane.emit_tick: a refusal survives whole,
        # a pathological stderr cannot write a giant journal row.
        data["detail"] = detail[:4000]
    _emit_event("control_plane_tick", data)


def _notify_parked(message: str) -> None:
    """Send an OS notification for a parked PR.

    Calls notify_operator with (title, message) -- two positional args.
    On failure, logs a warning instead of silently passing.
    """
    try:
        notify_operator("pr-watch", message)
    except Exception as exc:
        log.warning("pr-watch: notify failed: %s", exc)


def _reviewers_for(repo_dir: Path) -> list[str]:
    """Return the configured external reviewers for a given repo root.

    Loads settings scoped to ``repo_dir`` so each candidate PR uses its own
    repo's ``config.review.github_apps`` (aka the legacy ``required_bots``)
    rather than the daemon's cwd.  Falls back to [] when none are configured
    (review-dispatch skipped; merge-dispatch still works).  Logs a warning on
    error so a broken settings.yaml is visible rather than silently disabling
    review-dispatch.
    """
    try:
        s = load_settings_for_repo(repo_dir)
        bots = s.review.github_apps
        return list(bots) if bots else []
    except Exception as exc:
        log.warning(
            "pr-watch: reviewer resolution failed (%s); review-dispatch disabled this tick",
            exc,
        )
        return []


def _post_merge_readiness(repo_dir: Path) -> Any:
    """The post-merge verdict for ``repo_dir``, from the native oracle.

    The oracle lives in the front door (`fno config doctor --post-merge
    --json`); the tick reads only ``is_ready``. A missing front door or a bad
    answer raises, and the tick already maps a raise to not-ready.
    """
    import subprocess
    from types import SimpleNamespace

    from fno.rust_binary import resolve_front_binary

    binary = resolve_front_binary()
    if binary is None:
        raise RuntimeError("the fno front door is not installed")
    proc = subprocess.run(
        [str(binary), "config", "doctor", "--post-merge", "--json"],
        cwd=repo_dir,
        # The door reads this pin before the cwd; an inherited one names
        # another repo.
        env={**os.environ, "FNO_REPO_ROOT": str(repo_dir)},
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    return SimpleNamespace(is_ready=json.loads(proc.stdout).get("status") == "ready")


def _catchup_roots() -> list[Path]:
    """Distinct project roots the canonical-sync catch-up should sweep.

    launchd starts this daemon in ``/`` with no WorkingDirectory, so there is no
    ambient project to read config from. The roots come from every sidecar's
    cwd, regardless of the node's tracker state - a project whose backlog is
    all done/closed must still get swept, so this is ``load_all()`` (one scan,
    every id), never ``list_open()`` plus a per-id ``load()`` loop.
    """
    try:
        from fno import rust_binary

        sidecars = rust_binary.sidecar_all()
    except Exception as exc:  # noqa: BLE001 - no sidecar store means nothing to sweep
        log.warning("pr-watch: could not read sidecars for catch-up roots: %s", exc)
        return []
    roots: dict[str, Path] = {}
    for sc in sidecars.values():
        cwd = sc.get("cwd")
        if cwd and str(cwd) not in roots:
            roots[str(cwd)] = Path(cwd)
    return [p for p in roots.values() if p.is_dir()]


def _run_notify_watch_phase(
    roots: "Optional[list[Path]]" = None,
    *,
    timeout_s: Optional[float] = None,
    interval_s: int = 300,
) -> None:
    """Run the Rust notify_watch arm and turn its receipt into the tick row.

    The arm lives in fno-agents (``notify-watch``); the sampler, the signal
    store and the ``[notify]`` config are all read in Rust, so this phase is
    only spawn, parse and emit. ``interval_s`` rides the tick's own bucket:
    the row claims the cadence it really runs at, so the reader's stale bound
    is honest. The subprocess runs inside the first catch-up
    root: launchd starts this daemon in ``/``, where a board read would read
    an empty world. An absent binary, a non-zero run and an unparseable
    receipt all land as ``notify_failed`` - a dead notice lane never raises
    out of the tick. ``roots`` rides the tick's one-scan memo; the two tick
    phases are named apart so a cut says which half stalled. The subprocess
    bound comes from the phase slice (minus a 2s reserve) when the caller
    passes it, else from the armed phase deadline, and only falls back to a
    literal when no phase is armed at all.
    """
    from fno.pr_watch._dispatch import phase_seconds_left, set_tick_phase

    set_tick_phase("notify_watch")
    if roots is None:
        set_tick_phase("notify_watch:roots")
        roots = _catchup_roots()
    try:
        import subprocess

        from fno.rust_binary import resolve_binary

        binary = resolve_binary()
        if binary is None:
            _emit_tick_row("notify_watch", interval_s=interval_s,
                           skip_reason="notify_failed", detail="rust binary absent")
            return
        set_tick_phase("notify_watch:arm")
        argv = [str(binary), "notify-watch", "--json"]
        for root in roots:
            argv += ["--root", str(root)]
        if timeout_s is None:
            left = phase_seconds_left()
            timeout_s = max(1.0, left - 2.0) if left is not None else 240.0
        proc = subprocess.run(
            argv, capture_output=True, text=True, check=False, timeout=timeout_s,
            cwd=str(roots[0]) if roots else None,
        )
        payload = json.loads(proc.stdout or "{}")
        _emit_tick_row("notify_watch", interval_s=interval_s,
                       acted=int(payload.get("acted") or 0),
                       skip_reason=payload.get("skip_reason"),
                       detail=(payload.get("detail") or "")[:200])
    except subprocess.TimeoutExpired as exc:
        # A bound overrun is a budget cut; "notify_failed" would read FAIL.
        _emit_tick_row("notify_watch", interval_s=interval_s,
                       skip_reason="starved",
                       detail=f"arm pass exceeded its {exc.timeout:.0f}s bound"[:200])
    except Exception as exc:  # noqa: BLE001 - never let a notice break the tick
        log.warning("pr-watch: notify_watch phase failed: %s", exc)
        _emit_tick_row("notify_watch", interval_s=interval_s,
                       skip_reason="notify_failed", detail=str(exc)[:200])


def _run_evals_arm_phase(settings: Any, *, seconds_left_fn) -> None:
    """The eval bank's demand leg: guards and the receipt parse here;
    the due read, gate, detached run and journal live in native evals-arm.
    Every failure lands as ``arm_failed``, never out of the tick."""
    evals_cfg = getattr(settings, "evals", None)
    days = int(getattr(evals_cfg, "schedule_days", 0) or 0)
    interval_s = days * 86400

    def row(skip: Optional[str], detail: str, acted: int = 0) -> None:
        _emit_tick_row("evals", interval_s=interval_s, acted=acted,
                       skip_reason=skip, detail=detail[:400])

    if days <= 0:
        row("evals_off", "evals.schedule_days 0")
        return
    try:
        from fno.config import autonomy_master_enabled
        armed = autonomy_master_enabled()
    except Exception:  # noqa: BLE001 - an unreadable master switch reads off
        armed = False
    if not armed:
        row("autonomy_off", "config.autonomy.enabled is false")
        return
    try:
        import subprocess

        from fno.paths import evals_history, state_dir
        from fno.rust_binary import command_call, resolve_binary

        binary = resolve_binary()
        if binary is None:
            raise RuntimeError("fno-agents binary not found")
        stale_days = int(getattr(evals_cfg, "stale_days", 7) or 7)
        # The native health projection (evals-trend op health); None when no
        # history or no rows, which json.dumps passes through as null.
        health = command_call(
            "evals-trend",
            {"op": "health", "history": str(evals_history()), "stale_days": stale_days},
        )
        argv = [
            str(binary), "evals-arm",
            "--summary-json", json.dumps(health),
            "--history", str(evals_history()),
            "--events", str(state_dir() / "events.jsonl"),
            "--fno-bin", _resolve_fno_binary(),
            "--schedule-days", str(days),
            "--stale-days", str(stale_days),
        ]
        proc = subprocess.run(argv, capture_output=True, text=True, check=False,
                              timeout=max(1.0, seconds_left_fn() or 30.0))
        if proc.returncode != 0:
            raise RuntimeError(f"evals-arm exited {proc.returncode}: {proc.stderr[:160]}")
        answer = json.loads(proc.stdout.strip().splitlines()[-1]) if proc.stdout.strip() else {}
        row(answer.get("skip_reason"), str(answer.get("detail") or ""), int(answer.get("acted") or 0))
    except Exception as exc:  # noqa: BLE001 - never let the arm break the tick
        row("arm_failed", f"{type(exc).__name__}: {exc}")


def _watchdog_arming() -> dict:
    """The lane's arming read; an unread answer arms nothing."""
    from fno.rust_binary import call_binary_json

    err, answer = call_binary_json("census", ["watchdog-arming"], timeout=15)
    return answer if err is None and isinstance(answer, dict) else {}


class ClaimAdapter:
    """Thin adapter that maps the tick() claim protocol to fno.claims."""

    def acquire_tick_lock(self, key: str, holder: str) -> None:
        from fno.claims import acquire_claim
        acquire_claim(key, holder=holder)

    def release_tick_lock(self, key: str, holder: str) -> None:
        try:
            from fno.claims import release_claim
            release_claim(key, holder=holder)
        except Exception:
            pass

    def acquire_pr_lock(self, key: str, holder: str) -> None:
        from fno.claims import acquire_claim
        acquire_claim(key, holder=holder)

    def release_pr_lock(self, key: str, holder: str) -> None:
        try:
            from fno.claims import release_claim
            release_claim(key, holder=holder)
        except Exception:
            pass

    def is_node_live(self, node_id: str) -> bool:
        """Return True when the node has a live session claim.

        The read routes by key to the global root, so a live node claim is
        seen from any cwd; only an exception falls back to True (treat as
        live) to avoid double-dispatch onto a node a live /target session
        owns.
        """
        try:
            info = claim_status(f"node:{node_id}")
            # live OR suspect: a suspect claim (TTL-unexpired, dead pid)
            # is a respawned worker's slot - treat as occupied, never re-dispatch.
            return info.get("state") in ("live", "suspect")
        except Exception as exc:
            log.warning(
                "pr-watch: claim_status failed for node %s (%s); treating as live (fail-safe)",
                node_id,
                exc,
            )
            return True


# ---------------------------------------------------------------------------
# Module-level imports used by adapters (importable at test-patch time)
# ---------------------------------------------------------------------------

from fno.claims.core import claim_status  # noqa: E402
from fno.config import load_settings, load_settings_for_repo  # noqa: E402
from fno.rust_binary import notify_operator  # noqa: E402


# ---------------------------------------------------------------------------
# tick
# ---------------------------------------------------------------------------

# EX_TEMPFAIL: launchd logs the non-zero exit but does not respawn a
# StartInterval job early, so a timed-out tick surfaces without suppressing
# the successor it was bounded to protect.
_TICK_TIMEOUT_EXIT = 75

_ENV_TICK_TIMEOUT = "FNO_PR_WATCH_TICK_TIMEOUT"

#: A stranded sweep is one batched git fetch plus a rev-list and a
#: last-commit-age call per worktree - cheap, but not free at 60+
#: worktrees. Skipping under this floor costs nothing: the next tick
#: sweeps from scratch, there is no partial state to lose.
_STRANDED_FLOOR_S = 10.0

#: Merge's 245s cap reserves the 150s sweep after the worst settings + wake
#: caps on a 480s tick, while leaving room for the 90s queue read + merge floor.
_EVERY_TICK_CAP_S: dict[str, float] = {
    "settings": 10,
    "sweep": 150,
    # The notify phase pays the arm subprocess over every catch-up root
    # (armed pass: 23.1s measured over 12 roots 2026-09-27). 15s still cut
    # it mid-arm; 30s fits at the 0.85x deadline.
    "notify_watch": 30,
    "heal": 10,
    "evals": 10,
}
_FLEET_CAP_S: dict[str, float] = {
    "stranded": 60,
    "recovery": 90,
    "watchdog": 30,
}
_PHASE_CAP_S: dict[str, float] = {**_EVERY_TICK_CAP_S, **_FLEET_CAP_S, "merge": 245.0}

_GRANT_QUEUE_READ_TIMEOUT_S = 90.0
_MERGE_WINDOW_S = _PHASE_CAP_S["merge"] + _EVERY_TICK_CAP_S["sweep"]


class _StrandedBudgetSpent(Exception):
    """The tick has too little left to sweep. Not a failure of the sweep."""


def _merge_budget_for_load(load_per_core: float, drain_floor_s: float) -> tuple[float, float]:
    timeout_s = min(_MERGE_WINDOW_S - drain_floor_s - 35.0,
                    _GRANT_QUEUE_READ_TIMEOUT_S * max(1.0, load_per_core / 2.0))
    return timeout_s, max(_PHASE_CAP_S["merge"], timeout_s + drain_floor_s + 5.0)


class TickDeadlineExceeded(BaseException):
    """The tick's wall-clock deadline fired; the phase marker names where.

    BaseException on purpose: every broad `except Exception` seam in the tick
    path (the sweep, recovery) exists to degrade one leg without
    stopping the others, and the deadline is the one signal that must stop
    everything. The alarm is one-shot, so a seam that swallowed it would leave
    the rest of the tick unbounded - the exact stall class this deadline ends.
    """


def _on_deadline(signum, frame) -> None:  # noqa: ARG001 - signal handler signature
    raise TickDeadlineExceeded()


def _resolve_tick_deadline(cfg) -> int:
    """Env seam first, then config, then 0.85x the interval (min 60s).

    Config and derived values are clamped BELOW interval_seconds: launchd
    never runs a StartInterval job concurrently, so a deadline at or above
    the interval would let an overrun suppress the successor tick - the exact
    failure mode this ceiling exists to prevent. The env seam stays
    unclamped; it is an operator escape hatch, not a durable setting.
    """
    env = (os.environ.get(_ENV_TICK_TIMEOUT) or "").strip()
    if env.isdigit() and int(env) > 0:
        return int(env)
    ceiling = max(1, int(cfg.interval_seconds) - 5)
    if cfg.tick_timeout_seconds:
        return min(int(cfg.tick_timeout_seconds), ceiling)
    derived = max(60, int(cfg.interval_seconds * 0.85))
    return min(derived, ceiling)


def _tick_outcome(result, tick_failed: Optional[str], timed_out: bool) -> str:
    """Map one tick run to its end-record outcome (AC table in the plan)."""
    if timed_out:
        return "timeout"
    if tick_failed is not None:
        return "error"
    if result is None:
        return "error"
    if result.disabled:
        return "disabled"
    if result.lock_held:
        return "lock_held"
    if getattr(result, "quota_skip", False):
        return "quota_skip"
    if getattr(result, "sweep_failures", 0):
        return "degraded"
    return "ok"



def _replay_views() -> None:
    """Replay the native view pass. The provenance cache moves no store
    version, so the keeper's render pass would never see the change."""
    import subprocess

    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is not None:
        subprocess.run(
            [str(binary), "backlog", "render-views"],
            capture_output=True, check=False, timeout=600,
        )

@cli.command()
def tick() -> None:
    """Poll open-PR backlog nodes and fire /fno:ship pr check or /fno:ship pr merged.

    This is the command the LaunchAgent's ProgramArguments points at.
    It builds the real adapters (claims, emit, reviewers_for, etc.) and
    calls tick() from fno.pr_watch._dispatch.
    """
    import time

    from fno.pr_watch._dispatch import (
        current_tick_phase,
        phase_seconds_left,
        set_phase_deadline,
        set_tick_phase,
    )
    from fno.pr_watch._dispatch import SCAN_PROGRESS as sweep_progress
    from fno.pr_watch._dispatch import tick as _tick
    from fno.pr_watch._install import tick_end_bits

    started = time.monotonic()
    # Entry is recorded before anything that can hang: settings load, imports,
    # and the graph read all precede any other record, so a tick that dies
    # mid-bootstrap is still attributable (AC8). This is NOT the liveness
    # watermark: only a completed sweep mints pr_watch_tick (AC9).
    set_tick_phase("entry")
    _emit_event(
        "pr_watch_tick_attempt",
        {"pid": os.getpid(), "phase": "entry"},
    )

    outcome = "error"
    result = None
    tick_failed = None
    timed_out = False
    settings = None
    cfg = None
    tick_enabled = False
    sweep_started = False
    alarm_ok = True
    cut: list[str] = []
    phase_s: dict[str, float] = {}
    # Per-phase partial-work notes a body writes as it goes; the runner
    # clears one per phase run and emits it on the cut path, so a cut phase
    # hands back what it did. The sweep's note lives across the _dispatch
    # boundary (SCAN_PROGRESS); this dict holds the rest.
    progress: dict[str, str] = {}
    ceiling_box: dict[str, Optional[int]] = {"v": None}
    arm_interval: dict[str, int] = {"notify_watch": 300, "watchdog": 600}
    roots_box: dict[str, Optional[list]] = {"v": None}
    phase_caps = dict(_PHASE_CAP_S)
    grant_queue_timeout_s = _GRANT_QUEUE_READ_TIMEOUT_S

    # One sidecar scan per tick: four phases each swept the same roots.
    def _tick_roots() -> list:
        roots = roots_box["v"]
        if roots is None:
            roots = _catchup_roots()
            roots_box["v"] = roots
        return roots

    try:
        try:
            signal.signal(signal.SIGALRM, _on_deadline)
        except ValueError:
            # Not the main thread (tests embedding the command): no alarm
            # available, run unbounded like before.
            alarm_ok = False
            log.debug("pr-watch: SIGALRM unavailable outside main thread")

        # A bootout kills this process by signal; without a handler the tick
        # dies with no record.
        def _on_sigterm(signum, frame) -> None:  # noqa: ARG001 - handler signature
            signal.signal(signum, signal.SIG_IGN)
            phase = current_tick_phase()
            sender = _bounce_sender()
            _emit_event("pr_watch_tick_end", {
                "outcome": "error", "why": "killed",
                "duration_s": round(time.monotonic() - started, 3),
                "phase": phase, "pid": os.getpid(), "sender": sender,
            })
            _emit_tick_row(
                "pr_watch_merge",
                interval_s=int(getattr(cfg, "interval_seconds", 600)) if cfg is not None else 600,
                skip_reason="error",
                detail=(f"killed by a signal mid-tick; started and did not complete, "
                        f"phase={phase}; sender={sender}"),
            )
            signal.signal(signum, signal.SIG_DFL)
            os.kill(os.getpid(), signum)

        try:
            signal.signal(signal.SIGTERM, _on_sigterm)
        except ValueError:
            pass

        # One alarm per phase: every body runs under its own slice, so
        # a slow phase loses its turn instead of aborting the phases after it.
        # The runner is the only place that catches TickDeadlineExceeded.
        def _run_phase(
            name: str,
            body: Callable[[float], None],
            *,
            arm: Optional[str] = None,
            cadence: int = 1,
            slot: int = 0,
        ) -> bool:
            # Fleet-tail cadence: a phase on a cadence above 1 runs one tick
            # in `cadence`, on its slot of the interval bucket. A skipped
            # phase is not a failure: it mints an off_cadence arm row naming
            # the tick it next runs on, and its phase_s reads 0.0 without a
            # cut entry, so the tick outcome never reads timeout for a phase
            # that never ran.
            if cadence > 1 and cfg is not None:
                interval = max(1, int(getattr(cfg, "interval_seconds", 600)))
                tick_no = int(time.time() // interval)
                if tick_no % cadence != slot:
                    phase_s[name] = 0.0
                    progress.pop(name, None)
                    sweep_progress.pop(name, None)
                    if arm is not None:
                        next_tick = tick_no + (slot - tick_no) % cadence
                        _emit_tick_row(arm, interval_s=arm_interval.get(arm, interval),
                                       skip_reason="off_cadence",
                                       detail=(f"fleet-tail cadence {cadence}: "
                                               f"next tick {next_tick}"))
                    return False
            # A stale note from a previous tick must never ride this run's
            # receipt: a cut before the body ran reports zero progress.
            progress.pop(name, None)
            sweep_progress.pop(name, None)
            left: Optional[float] = None
            if ceiling_box["v"] is not None:
                left = ceiling_box["v"] - (time.monotonic() - started)
            if left is not None and left <= 0:
                cut.append(name)
                phase_s[name] = 0.0
                if arm is not None:
                    # starved, not broken: timeout is a FAILURE_SKIPS token.
                    _emit_tick_row(arm, interval_s=arm_interval.get(arm, 600),
                                   skip_reason="starved",
                                   detail=f"deadline exceeded before phase {name}")
                return False
            if ceiling_box["v"] is None:
                # The settings phase runs before a ceiling exists: its slice
                # is its measured cap, min(the env seam) when the seam is set.
                slice_s = float(phase_caps.get("settings", 60.0))
                env = (os.environ.get(_ENV_TICK_TIMEOUT) or "").strip()
                if env.isdigit() and int(env) > 0:
                    slice_s = min(slice_s, float(env))
            else:
                assert left is not None
                slice_s = min(phase_caps.get(name, left), left)
            # Which budget fired if the alarm does: a cap below the
            # remaining wall starves one phase; the wall is the tick deadline.
            cap = phase_caps.get(name)
            wall_limited = (
                ceiling_box["v"] is None or cap is None or cap >= (left or 0.0)
            )
            slice_s = max(1.0, slice_s)
            phase_start = time.monotonic()
            try:
                if alarm_ok:
                    signal.alarm(max(1, int(slice_s)))
                set_tick_phase(name)
                set_phase_deadline(time.monotonic() + slice_s)
                body(slice_s)
            except TickDeadlineExceeded:
                cut.append(name)
                if arm is not None:
                    # Name the sub-step the alarm caught: a phase that reports
                    # its halves reads as one stall, not a black box. And hand
                    # back what the phase did before the cut - the body's
                    # progress note or the sweep's scan counter - so the row
                    # says "scanned 21 of 39", never a bare timeout.
                    step = current_tick_phase()
                    at = f" at {step}" if step.startswith(name + ":") else ""
                    base = (f"deadline exceeded in phase {name} at "
                            f"{int(slice_s)}s" if wall_limited else
                            f"phase slice {int(slice_s)}s spent") + at
                    note = progress.get(name) or sweep_progress.get(name) or ""
                    # The slice was the binding budget: starved, not FAIL.
                    _emit_tick_row(arm, interval_s=arm_interval.get(arm, 600),
                                   skip_reason="starved",
                                   detail=f"{base} {note}" if note else base)
            finally:
                if alarm_ok:
                    try:
                        signal.alarm(0)
                    except ValueError:
                        pass
                set_phase_deadline(None)
                phase_s[name] = round(time.monotonic() - phase_start, 1)
            return True

        def _phase_settings(_slice_s: float) -> None:
            nonlocal settings, cfg, tick_enabled, grant_queue_timeout_s
            from fno.pr_watch._dispatch import _MERGE_FLOOR_S

            settings = load_settings()
            cfg = settings.pr_watch
            # wave 3: the master panic switch outranks pr_watch's own gate too.
            tick_enabled = cfg.enabled and settings.autonomy.enabled
            ceiling_box["v"] = _resolve_tick_deadline(cfg)
            try:
                load_per_core = os.getloadavg()[0] / (os.cpu_count() or 1)
            except (AttributeError, OSError):
                load_per_core = None
            if load_per_core is not None and ceiling_box["v"] is not None and ceiling_box["v"] >= 480:
                grant_queue_timeout_s, phase_caps["merge"] = _merge_budget_for_load(
                    load_per_core, _MERGE_FLOOR_S + 15.0
                )

        _run_phase("settings", _phase_settings)

        # Capped PR and fleet-health legs run before the uncapped merge phase.
        def _phase_recovery(_slice_s: float) -> None:
            assert settings is not None and cfg is not None
            set_tick_phase("recovery")
            _fleet_candidates = 0
            _fleet_refused = 0
            _fleet_silent = 0
            _fleet_swept = False
            if settings.recovery.enabled and settings.autonomy.enabled:
                try:
                    from fno.rust_binary import call_binary_json

                    err, answer = call_binary_json(
                        "census", ["recovery-sweep"], timeout=max(5.0, phase_seconds_left() or 5.0)
                    )
                    if err is not None or not isinstance(answer, dict):
                        raise RuntimeError(f"recovery sweep door failed: {err}")
                    _fleet_candidates = int(answer.get("candidates") or 0)
                    events = answer.get("events") or []
                    _fleet_refused = sum(1 for event in events if event.get("type") == "worker_refused")
                    for event in events:
                        _emit_event(str(event.get("type")), event.get("data") or {})
                    # A disarmed sweep measured nothing, so it stamps no heartbeat.
                    _fleet_swept = answer.get("swept") is True
                    typer.echo(f"recovery sweep: candidates={_fleet_candidates}")
                except Exception as exc:  # noqa: BLE001 - never let recovery break pr-watch
                    log.warning("pr-watch: recovery sweep failed: %s", exc)

                # The cadence-deadline backstop, for a refusal the taxonomy does
                # not recognise. Report only, and wrapped separately from the
                # recovery sweep so neither takes the other down.
                try:
                    from fno.rust_binary import call_binary_json

                    err, answer = call_binary_json("census", ["sweep", "--tick"], timeout=60)
                    if err is not None or not isinstance(answer, dict):
                        raise RuntimeError(f"silence sweep door failed: {err}")
                    _fleet_silent = int(answer.get("silent") or 0)
                    if _fleet_silent:
                        typer.echo(f"silence sweep: silent={_fleet_silent}")
                except Exception as exc:  # noqa: BLE001 - a backstop never breaks the tick
                    log.warning("pr-watch: silence sweep failed: %s", exc)

                # The fleet leg's own watermark and its liveness proof. Written
                # only when the sweep COMPLETED: a failed sweep that still stamped
                # a watermark would render as a healthy quiet fleet.
                if _fleet_swept:
                    try:
                        from fno.rust_binary import command_call

                        command_call(
                            ["fleet-incident", "fleet-state", "heartbeat"],
                            {
                                "candidates": _fleet_candidates,
                                "refused": _fleet_refused,
                                "silent": _fleet_silent,
                            },
                            timeout=5,
                        )
                    except Exception as exc:  # noqa: BLE001 - never fatal to the PR legs
                        log.warning("pr-watch: fleet heartbeat write failed: %s", exc)

        def _phase_watchdog(_slice_s: float) -> None:
            assert settings is not None and cfg is not None
            set_tick_phase("watchdog")
            # The whole leg is native. The answer is one JSON object; this wrap
            # keeps the tick's own arm row and its non-fatal contract.
            wd_i = settings.pr_watch.interval_seconds
            arm_interval["watchdog"] = wd_i
            try:
                from fno.rust_binary import call_binary_json

                left = phase_seconds_left() or 0.0
                err, answer = call_binary_json(
                    "census",
                    ["watchdog-tick", "--budget-s", f"{left:.1f}"],
                    timeout=max(5.0, left),
                )
                if err is not None or not isinstance(answer, dict):
                    raise RuntimeError(f"watchdog door failed: {err}")
            except Exception as exc:  # noqa: BLE001 - never let the watchdog break pr-watch
                log.warning("pr-watch: watchdog sweep failed: %s", exc)
                _emit_tick_row("watchdog", interval_s=wd_i, skip_reason="sweep_failed",
                               detail=str(exc)[:200])
                return
            if not answer.get("armed"):
                # An unarmed lane still ticks: "why it did nothing" is the readout's job.
                _emit_tick_row("watchdog", interval_s=wd_i, skip_reason="watchdog_off",
                               detail=str(answer.get("off_detail") or ""))
                return
            if answer.get("skip_reason"):
                log.info("pr-watch: watchdog leg skipped: %s", answer.get("detail"))
                _emit_tick_row("watchdog", interval_s=wd_i,
                               skip_reason=str(answer["skip_reason"]),
                               detail=str(answer.get("detail") or "")[:200])
                return
            acted = int(answer.get("acted") or 0)
            counts = str(answer.get("counts") or "")
            applied = int(answer.get("recoverable_applied") or 0)
            remaining = int(answer.get("recoverable_remaining") or 0)
            typer.echo(
                f"watchdog report: {counts} acted={acted} "
                f"recoverable_applied={applied} recoverable_remaining={remaining}"
            )
            _emit_tick_row("watchdog", interval_s=wd_i, acted=acted,
                           detail=f"{counts} recoverable_applied={applied} "
                                  f"recoverable_remaining={remaining}")

        def _phase_sweep(slice_s: float) -> None:
            nonlocal result, tick_failed
            assert settings is not None and cfg is not None
            interval = int(getattr(cfg, "interval_seconds", 600))
            set_tick_phase("sweep")
            # A dead tick must not kill the legs below. The receipt contract makes
            # _tick raise on a failed emission even though state is already persisted,
            # so a broken events path would otherwise crash-loop recovery and sync
            # catch-up, which ride this same launchd cadence. Fail the exit code at
            # the end instead, mirroring how those legs wrap their own failures.
            try:
                result = _tick(
                    claim=ClaimAdapter(),
                    emit=_emit_event,
                    reviewers_for=_reviewers_for,
                    notify=lambda message, **_kw: _notify_parked(message),
                    post_merge_readiness_fn=_post_merge_readiness,
                    now_iso=datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ"),
                    max_age_days=cfg.max_age_days,
                    max_retries=cfg.retries,
                    graphql_min_remaining=cfg.graphql_min_remaining,
                    enabled=tick_enabled,
                    dispatch_deadline=time.monotonic() + slice_s,
                )
            except TickDeadlineExceeded:
                raise
            except Exception as exc:  # noqa: BLE001 - a dead events path must not stop recovery
                tick_failed = str(exc)
                log.warning("pr-watch: tick failed: %s", exc)
                typer.echo(f"pr-watch tick: failed: {exc}", err=True)
                _emit_tick_row("pr_watch_sweep", interval_s=interval, skip_reason="error", detail=tick_failed)
                result = None

            if result is not None:
                if result.disabled:
                    reason = "config.autonomy.enabled" if not settings.autonomy.enabled else "config.pr_watch.enabled"
                    typer.echo(f"pr-watch tick: {reason} is false - skipped")
                elif result.lock_held:
                    typer.echo(f"pr-watch tick: {result.lock_holder} - skipped")
                elif result.quota_skip:
                    reset = f", resets {result.quota_reset}" if result.quota_reset else ""
                    # The skip can follow a sweep with failed repos, and this stdout
                    # line is what an operator tails during an outage: the failure
                    # count rides the skip line too, matching the end record.
                    degraded = (
                        f" (degraded: {result.sweep_failures} sweep failure(s))"
                        if result.sweep_failures
                        else ""
                    )
                    typer.echo(
                        f"pr-watch tick: graphql remaining {result.quota_remaining} below floor"
                        f" - dispatch pass skipped{reset}{degraded}"
                    )
                elif result.sweep_failures:
                    typer.echo(
                        f"pr-watch tick: degraded: {result.sweep_failures} sweep failure(s)"
                    )
                else:
                    typer.echo(
                        f"pr-watch tick: open_prs={result.open_prs} acted={result.acted} skipped={result.skipped}"
                    )
                # One row per run like the merge arm: the skip word names a no-work tick.
                skip = ("disabled" if result.disabled else "lock_held" if result.lock_held else "quota_skip" if result.quota_skip else None)
                _emit_tick_row("pr_watch_sweep", interval_s=interval, acted=result.acted,
                               skip_reason=skip,
                               detail=f"open_prs={result.open_prs} acted={result.acted} skipped={result.skipped}")

        def _phase_merge(slice_s: float) -> None:
            assert cfg is not None
            set_tick_phase("merge")
            interval = int(getattr(cfg, "interval_seconds", 600))
            head = "merge ok"
            if not tick_enabled:
                _emit_tick_row("pr_watch_merge", interval_s=interval, skip_reason="disabled",
                               detail=f"{head} pr_watch disabled")
                return
            from fno.pr_watch._discover import PrCandidate
            from fno.pr_watch._dispatch import run_execute_queue
            from fno.pr_watch._state import make_watermark_key
            from fno.rust_binary import CommandUnavailable, command_call

            roots = _tick_roots()
            try:
                # The read's bound travels to the Rust op as deadline_ms, so
                # its bounded git reads answer inside it.
                grant_timeout = min(grant_queue_timeout_s, max(1.0, slice_s - 10.0))
                out = command_call("authorized-merge", {"op": "grant-queue",
                                "rotate": int(time.time() // interval),
                                "cwd": str(roots[0] if roots else Path.cwd()),
                                "deadline_ms": int(grant_timeout * 1000)},
                                timeout=grant_timeout)
                if out.get("error"):
                    raise CommandUnavailable(str(out["error"]))
                queue = [
                    (PrCandidate(node_id=r["node_id"], pr_number=int(r["pr"]), pr_url=None,
                                 repo_dir=Path(r["cwd"]), repo_slug=r["repo_slug"]),
                     make_watermark_key(repo_slug=r["repo_slug"], pr_number=int(r["pr"])),
                     r.get("grant") or {})
                    for r in out.get("queue") or []
                ]
                progress["merge"] = f"queue={len(queue)}"
            except (CommandUnavailable, KeyError, TypeError, ValueError) as exc:
                reason = "grant_queue_timeout" if "timed out after" in str(exc).lower() else "error"
                _emit_tick_row("pr_watch_merge", interval_s=interval, skip_reason=reason,
                               detail=f"{head} grant queue unreadable ({exc})")
                return
            counts = run_execute_queue(
                queue, emit=_emit_event,
                notify=lambda message, **_kw: _notify_parked(message),
                max_retries=cfg.retries, claim=ClaimAdapter(),
            )
            starved = counts.get("budget", 0) > 0 and not (counts["executed"] + counts["held"] + counts["failed"])
            verdicts = out.get("verdicts") or {}
            detail = (f"{head} candidates={out.get('candidates', 0)} granted={verdicts.get('granted', 0)} "
                      + " ".join(f"{k}={v}" for k, v in counts.items())
                      + f" read_ms={out.get('elapsed_ms', -1)}")
            _emit_tick_row("pr_watch_merge", interval_s=interval,
                           acted=counts["executed"], skip_reason="budget_spent" if starved else None,
                           detail=detail)


        # Stranded-worktree recovery, same arming gate as the fleet
        # watchdog above: this is a second read of the same "is recovery
        # armed" decision, not a second dispatcher - config.recovery.watchdog
        # + recovery.enabled + autonomy.enabled all still gate whether
        # anything here acts. Report, never reap: only STRANDED rows get
        # pushed and filed; only UNKNOWN rows get recorded; every other
        # class, LIVE included, is quiet and untouched.
        # The operator-notice sampler: one phase, always run; the
        # Rust arm answers notify_off itself when the [notify] signals list
        # is empty, so the readout shows the arm whether or not it is armed.
        def _phase_notify(slice_s: float) -> None:
            # The row carries the arm's real cadence - one run per launchd
            # bucket, not the 300s literal - so the reader's stale bound is
            # honest about a healthy rotation.
            nw_interval = (int(getattr(cfg, "interval_seconds", 600))
                           if cfg is not None else 600)
            _run_notify_watch_phase(_tick_roots(), timeout_s=max(1.0, slice_s - 2.0),
                                    interval_s=nw_interval)

        # The heal drive loop: nothing called the healer on a timer, so every
        # red open PR waited for a hand. The loop lives in Rust; this phase is
        # only the gate. The arm guard lives inside run_heal_phase, and every
        # gate answer lands in the journal as a control_plane_tick row so the
        # status line can say why nothing ran.
        def _phase_heal(_slice_s: float) -> None:
            set_tick_phase("heal")
            try:
                from fno.pr_watch._heal_phase import run_heal_phase

                answer = run_heal_phase(settings, _tick_roots())
            except Exception as exc:  # noqa:BLE001 - never let heal break the tick
                log.warning("pr-watch: heal phase failed: %s", exc)
                return
            typer.echo(f"pr heal: {answer}")
            if answer != "ran":
                # The same arm row the detached spawn writes; this covers the
                # gate answers that never reach the binary.
                _emit_tick_row(
                    "heal",
                    interval_s=int(getattr(cfg, "interval_seconds", 600)),
                    acted=0,
                    skip_reason=answer.replace("-", "_"),
                    detail=f"auto_heal gate: {answer}",
                )

        def _phase_evals(_slice_s: float) -> None:
            set_tick_phase("evals")
            _run_evals_arm_phase(settings, seconds_left_fn=phase_seconds_left)

        def _phase_stranded(slice_s: float) -> None:
            assert settings is not None and cfg is not None
            set_tick_phase("stranded")
            # The sweep feeds the board's provenance cache; the lane only arms acting.
            arming = _watchdog_arming()
            lane_armed = bool(arming.get("armed"))
            try:
                left = phase_seconds_left() or 0.0
                if left < _STRANDED_FLOOR_S:
                    raise _StrandedBudgetSpent(
                        f"{left:.1f}s left, under the {_STRANDED_FLOOR_S:.0f}s "
                        "a stranded sweep costs"
                    )
                from fno.branch_provenance_cache import write_cache
                from fno.rust_binary import call_binary_json

                wake = lane_armed and bool(arming.get("wake_armed"))
                changed, stranded_n, unknown_n, acted_n, failed_n, roots_done = False, 0, 0, 0, 0, 0
                # Rotate the starting root by run: one stranded run every
                # three interval buckets (the fleet-tail cadence), so
                # consecutive runs - not buckets - start at consecutive
                # roots and a cap cut never replays the same prefix.
                roots = _tick_roots()
                if roots:
                    k = (int(time.time() // max(1, int(cfg.interval_seconds)))
                         // 3) % len(roots)
                    roots = roots[k:] + roots[:k]
                for root in roots:
                    # Re-check per root, not just once before the loop: a
                    # code-review finding caught that the floor above only
                    # bounded the FIRST root - a multi-repo tick with several
                    # catch-up roots could blow well past the shared tick
                    # deadline after the first root's own check passed.
                    left = phase_seconds_left() or 0.0
                    if left < _STRANDED_FLOOR_S:
                        log.info(
                            "pr-watch: stranded leg stopped after %d root(s), "
                            "%.1fs left, under the %.0fs a sweep costs - "
                            "remaining roots retry next tick",
                            roots_done, left, _STRANDED_FLOOR_S,
                        )
                        break
                    try:
                        door_args = ["--repo", str(root), "--json", "--apply"]
                        if not wake:
                            # The report posture still records UNKNOWN rows;
                            # only the STRANDED push+file waits for the arm.
                            door_args.append("--no-wake")
                        door_err, payload = call_binary_json(
                            "worktree-stranded", door_args, timeout=120
                        )
                        if door_err is not None or not isinstance(payload, dict):
                            raise RuntimeError(
                                f"worktree-stranded door failed: {door_err}"
                            )
                        stranded_rows = payload.get("rows") or []
                        outcomes = payload.get("outcomes") or []
                        changed |= write_cache(root, stranded_rows)
                    except Exception as exc:  # noqa: BLE001 - one bad repo never stops the rest
                        log.warning("pr-watch: stranded sweep failed for %s: %s", root, exc)
                        continue
                    stranded_n += sum(1 for r in stranded_rows if r.get("class") == "STRANDED")
                    unknown_n += sum(1 for r in stranded_rows if r.get("class") == "UNKNOWN")
                    acted_n += len(outcomes)
                    failed_n += sum(1 for o in outcomes if o["stopped_at"])
                    roots_done += 1
                typer.echo(
                    f"stranded sweep ({'wake' if wake else 'report'}): "
                    f"stranded={stranded_n} unknown={unknown_n} "
                    f"acted={acted_n} failed={failed_n}"
                )
                if changed:
                    _replay_views()
            except _StrandedBudgetSpent as exc:
                log.info("pr-watch: stranded leg skipped: %s", exc)
            except Exception as exc:  # noqa: BLE001 - never let the stranded sweep break pr-watch
                log.warning("pr-watch: stranded sweep failed: %s", exc)

        # Canonical-sync catch-up does NOT run on the tick: it duplicated
        # `fno backlog reconcile`'s SessionStart leg, and its sync shell is
        # where ticks died. Reconcile owns the outcome-keyed leg and surfaces
        # a proven-stale canonical through its SessionStart hook.
        # Value order, not cost order: behind a fleet-loaded sweep that
        # saturates its cap, the wake evaluated no role and the merge read
        # 38s of grant queue and executed nothing. Both arms read durable
        # state, never the sweep's result, so they run first; the sweep
        # follows as the one arm that resumes per-PR across ticks.
        sweep_started = True
        _run_phase("merge", _phase_merge, arm="pr_watch_merge")
        _run_phase("sweep", _phase_sweep, arm="pr_watch_sweep")
        _run_phase("notify_watch", _phase_notify, arm="notify_watch")
        _run_phase("heal", _phase_heal)
        _run_phase("evals", _phase_evals)
        # The fleet tail: each runs one tick in three, on its slot of the
        # interval bucket (the same rotation _phase_stranded uses for its
        # root start), so the PR lane never pays all three p90s in one tick.
        _run_phase("stranded", _phase_stranded, arm="stranded", cadence=3, slot=0)
        _run_phase("recovery", _phase_recovery, arm="recovery", cadence=3, slot=1)
        _run_phase("watchdog", _phase_watchdog, arm="watchdog", cadence=3, slot=2)
    except TickDeadlineExceeded:
        # Backstop: the per-phase runner catches its own cuts. Reaching here
        # means a cut escaped between phases; phase names where. This is the
        # one remaining timeout: the wall ceiling aborting the tick.
        timed_out = True
        typer.echo(
            f"pr-watch tick: deadline exceeded in phase {current_tick_phase()} - aborted",
            err=True,
        )
    finally:
        try:
            signal.alarm(0)
        except ValueError:
            pass
        # A cut phase no longer aborts the tick and no longer reads timeout:
        # its own row names the slice it spent, and a tick that ran to this
        # finally is not a timeout whatever a cap spent. Only the backstop
        # above - the wall ceiling firing between phases - is, and it still
        # exits 75 so launchd logs it without suppressing the successor.
        outcome = _tick_outcome(result, tick_failed, timed_out)
        end_data: dict[str, Any] = {
            "outcome": outcome,
            "duration_s": round(time.monotonic() - started, 3),
            "phase": cut[0] if cut else current_tick_phase(),
            "pid": os.getpid(),
        }
        # timed_out is the wall backstop now; slice overruns stay on the
        # cut/saturated fields and never reach why.
        if timed_out:
            end_data["why"] = "deadline_exceeded"
        if cut:
            end_data["cut"] = list(cut)
        if phase_s:
            end_data["phase_s"] = dict(phase_s)
        # Saturated = the alarm fired while the body ran, so the phase spent
        # its whole slice. A phase cut before its body ran spent zero: cut
        # names it, but it was starved, not saturated.
        end_data["saturated"] = [name for name in cut if phase_s.get(name, 0.0) > 0.0]
        if result is not None:
            end_data["sweep_failures"] = getattr(result, "sweep_failures", 0)
            if getattr(result, "quota_skip", False):
                end_data["quota_remaining"] = result.quota_remaining
                end_data["quota_reset"] = result.quota_reset
            if getattr(result, "quota_unknown", False):
                end_data["quota_unknown"] = True
        # The end record always fires - including on timeout and error - so the
        # attempt/end pair brackets every invocation; only outcome=ok/degraded
        # corresponds to a pr_watch_tick (the liveness watermark) having fired.
        _emit_event("pr_watch_tick_end", end_data)
        # Arms-readout row for the dispatch legs: the sweep phase writes it at
        # its own end now, so this finally covers only the ticks where the
        # sweep phase never started (a settings error, say) and an error tick
        # stays visible.
        if not sweep_started:
            cfg_interval = int(getattr(cfg, "interval_seconds", 600)) if cfg is not None else 600
            bits = tick_end_bits(end_data)
            _emit_tick_row("pr_watch_merge", interval_s=cfg_interval,
                           acted=int(getattr(result, "acted", 0) or 0),
                           skip_reason=outcome if outcome in
                           ("disabled", "lock_held", "quota_skip", "error", "timeout") else None,
                           detail=f"outcome={outcome}" + (f" ({', '.join(bits)})" if bits else ""))

    if timed_out:
        raise typer.Exit(code=_TICK_TIMEOUT_EXIT)
    if tick_failed is not None:
        raise typer.Exit(code=1)


# ---------------------------------------------------------------------------
# install
# ---------------------------------------------------------------------------


@cli.command()
def install(
    dry_run: bool = typer.Option(False, "-N", "--dry-run", help="Print plist; write nothing."),
    interval: int = typer.Option(0, "--interval", help="Poll interval in seconds (0 = use config)."),
    model: str = typer.Option("", "--model", help="Model for headless fires (empty = use config)."),
    no_activate: bool = typer.Option(
        False,
        "--no-activate",
        help="Write the plist but do NOT launchctl load it (packaging/CI escape).",
    ),
) -> None:
    """Render and install the global PR-state watcher LaunchAgent, then load it.

    Prints the full plist before writing.  Requires explicit confirmation
    before writing to ~/Library/LaunchAgents/, then bootout/bootstrap/kickstart
    bounces the agent so enabled means running.  Pass ``--no-activate`` to
    write only.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno do pr watch install: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    argv = [str(binary), "pr-watch", "install", "--fno-binary", _resolve_fno_binary()]
    if dry_run:
        argv.append("--dry-run")
    if interval > 0:
        argv += ["--interval", str(interval)]
    if no_activate:
        argv.append("--no-activate")
    result = subprocess.run(argv, check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


@cli.command()
def refresh() -> None:
    """Re-render the plist onto the current binary and bounce the watcher.

    Non-interactive, no confirm prompt: this is the tail of ``fno doctor update`` (so
    an update leaves an enabled watcher running the freshly-installed binary),
    and is safe to run by hand. A no-op when ``pr_watch.enabled`` is false, so
    an install that does not use the watcher gets nothing. Never fails loud:
    the update chain calls it best-effort and a refresh failure must not fail
    the update. Force and caller identity (doctor --fix) are command-level
    flags on ``fno-agents pr-watch refresh`` itself; a new flag is a new
    command and belongs in crates.
    """
    argv = ["refresh", "--fno-binary", _resolve_fno_binary(), "--caller", "refresh"]
    _forward_to_binary("pr-watch", argv)


def _forward_to_binary(command: str, args: list[str]) -> None:
    """Forward to the bundled fno-agents binary, binary-direct. The shared
    body of the pr-watch binary commands (refresh, heal); the pr-front-door
    copy this once was died with pr/cli.py (pr port 7)."""
    import subprocess

    import typer

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            f"fno do pr watch {command.replace('pr-', '')}: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    result = subprocess.run([str(binary), command, *args], check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


# Single-flight window for the SessionStart self-heal: long enough to cover the
# render + bounce round-trip, short enough that a crashed heal recovers soon.
# The window (and the gate itself) live in the heal command now.

@cli.command()
def heal() -> None:
    """Revive a previously-enabled-but-dead watcher (idempotent, race-guarded).

    Forward to the fno-agents pr-watch heal command: the disabled gate, the
    bounce-pending guard and the single-flight live in crates now, and the
    cure is the same re-render + bounce the SessionStart hook always ran.
    Command-level flags (--caller, --fno-binary) belong to the command itself; a
    new flag is a new command and belongs in crates.
    """
    argv = ["heal", "--fno-binary", _resolve_fno_binary(), "--caller", "heal"]
    _forward_to_binary("pr-watch", argv)


# ---------------------------------------------------------------------------
# uninstall
# ---------------------------------------------------------------------------


@cli.command()
def uninstall() -> None:
    """Unload (best-effort) and remove the global PR-state watcher LaunchAgent.

    Preserves ~/.fno/pr-watcher-state.json so a reinstall does not re-fire
    previously handled PRs.
    """
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno do pr watch uninstall: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    argv = [str(binary), "pr-watch", "uninstall"]
    result = subprocess.run(argv, check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))


# ---------------------------------------------------------------------------
# status
# ---------------------------------------------------------------------------


@cli.command()
def status(
    json_out: bool = typer.Option(
        False,
        "--json", "-J",
        help="Emit the liveness verdict as one JSON object (for hooks/scripts).",
    ),
) -> None:
    """Report watcher status: loaded, last tick time, open-PR count, parked PRs."""
    import subprocess

    from fno._subprocess_util import propagate_returncode
    from fno.rust_binary import resolve_binary

    binary = resolve_binary()
    if binary is None:
        typer.echo(
            "fno do pr watch status: the fno-agents binary was not found. "
            "It ships in the `pip install fno` wheel and with the plugin; "
            "reinstall fno or run `fno doctor update --rust`, or set "
            "FNO_AGENTS_BIN to its path.",
            err=True,
        )
        raise typer.Exit(code=127)
    argv = [str(binary), "pr-watch", "status"]
    if json_out:
        argv.append("--json")
    result = subprocess.run(argv, check=False)
    raise typer.Exit(code=propagate_returncode(result.returncode))
