"""Provider selection and ask-dispatch orchestrator for fno agents.

Phase 1 surface:

- ``KNOWN_PROVIDERS`` - roster-backed tuple of supported provider names.
- ``is_provider_available(name)`` — wraps ``shutil.which`` for a single CLI.
- ``select_provider(name, requested_provider)`` — registry-aware selection
  that catches the "wrong provider on follow-up" mistake before any
  subprocess fires.

The spawn subprocess invocation per provider lives in
``fno.agents.harnesses.{claude,codex}``. Gemini is a legacy readable identity,
not a maintained Python dispatch provider.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field, replace
from pathlib import Path
from fno.agents.dispatch_errors import DispatchAskError
from typing import Any, Mapping, Optional

from fno import paths
from fno.agents import events
from fno.agents.harnesses.base import ReachabilityProbeError
from fno.agents.reachability import mux_ref_names_a_pane
from fno.agents.registry import (
    AgentEntry,
    AgentStatus,
    RegistryVersionError,
    load_registry,
    update_registry,
)


class ProviderMismatchError(RuntimeError):
    """Raised when a follow-up ``ask`` passes a provider that disagrees with the registry."""


def _check_known_provider(name: str) -> None:
    from fno.agents.harnesses import KNOWN_PROVIDERS  # call-time read (import-time crashes stale binaries)

    if name not in KNOWN_PROVIDERS:
        raise ValueError(f"unknown provider {name!r}; supported: {', '.join(KNOWN_PROVIDERS)}")


def is_provider_available(name: str) -> bool:
    """Return True iff the named provider CLI is on PATH.

    Raises ``ValueError`` if ``name`` is not in :data:`KNOWN_PROVIDERS`.
    """
    _check_known_provider(name)
    return shutil.which(name) is not None


def select_provider(name: str, requested_provider: Optional[str]) -> str:
    """Select the provider for ``fno agents ask <name>``.

    Logic:
      - If ``requested_provider`` is given, validate it against
        :data:`KNOWN_PROVIDERS`.
      - If the agent already exists in the registry:
        - No request: return the recorded provider.
        - Request matches: return it.
        - Request mismatches: raise :class:`ProviderMismatchError` with a
          message that names the agent, recorded provider, and requested
          provider. This catches the mistaken-reuse failure mode that a
          silent "ignored" path would mask.
      - If the agent is new:
        - Request given: return it.
        - No request: raise ``ValueError`` because there is nothing to
          select for a brand-new agent.
    """
    if requested_provider is not None:
        _check_known_provider(requested_provider)

    existing = next(
        (entry for entry in load_registry() if entry.name == name),
        None,
    )

    if existing is not None:
        if requested_provider is None or requested_provider == existing.harness:
            return existing.harness
        raise ProviderMismatchError(
            f"agent {name!r} is provider={existing.harness}, "
            f"refusing to follow-up as provider={requested_provider}"
        )

    if requested_provider is None:
        from fno.agents.harnesses import KNOWN_PROVIDERS  # call-time read (import-time crashes stale binaries)

        raise ValueError(
            f"provider is required for new agent {name!r}; "
            f"pass --provider one of: {', '.join(KNOWN_PROVIDERS)}"
        )
    return requested_provider


# ---------------------------------------------------------------------------
# Shared dispatch surface (validation, locking, events)
# ---------------------------------------------------------------------------


# Absorbing states: once a row reaches one, no probe may move it and no identity
# may be backfilled onto it. Named once because reconcile tests it on three
# separate paths and a set that drifts between them reopens the resurrection bug.
_TERMINAL_AGENT_STATUSES = frozenset({"orphaned", "failed", "exited", "permanent_dead"})

_NAME_MAX_LEN = 128
_SHORT_ID_NAME_SHAPE = re.compile(r"^[0-9a-f]{8}$")


def _derive_log_path(name: str) -> Path:
    """Stable fno-side log path for `fno agents logs <name>` (US3 plumbing)."""
    return paths.state_dir() / "agents" / "logs" / f"{name}.log"


def _touch_log_path(name: str) -> Optional[Path]:
    """Create (or reuse) the log file a mint site is about to record as a
    registry row's ``log_path`` (AC4): a ``log_path`` pointing at
    nothing is a claim, not evidence, so the file must exist before the row
    does. Returns ``None`` on a failed create (disk full, EROFS, a
    permission error) instead of raising or returning a path nothing backs,
    mirroring ``claude_ask.rs``'s ``log_file_created`` gate, so a caller
    records this leg only when it is real. Shared by every mint site
    (dispatch.py, mux_spawn.py) so the mkdir+touch idiom lives in exactly
    one place.
    """
    log_path = _derive_log_path(name)
    try:
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.touch(exist_ok=True)
    except OSError:
        return None
    return log_path


def _capture_spawn_trigger() -> Optional[str]:
    """The CAUSE of this spawn, distinct from :func:`_capture_parent_edge`.

    A dispatcher (think-spawn, or a machine advance) sets ``FNO_SPAWN_TRIGGER``
    in the subprocess env before shelling ``fno agents spawn``; a human running
    the command directly never sets it, so absence reads as "an operator asked
    for this." Never raises. Pops the var after reading it: a child never
    inherits its parent's spawn cause, and the worker env floor drops it too.
    """
    return (os.environ.pop("FNO_SPAWN_TRIGGER", "") or "").strip() or None


# ---------------------------------------------------------------------------
# Task 1.2: spawn command (US2 Python fallback runtime)
# ---------------------------------------------------------------------------


def validate_spawn_name(name: str) -> None:
    """Spawn-name rules for the mux-pane back half (``fno.agents.mux_spawn``).
    Raises :class:`DispatchAskError` (exit 2) on every violation.
    """
    if not name:
        raise DispatchAskError("agent name must not be empty", exit_code=2)
    if "/" in name or "\\" in name or ".." in name:
        raise DispatchAskError(
            f"agent name must not contain path separators or '..': {name!r}",
            exit_code=2,
        )
    if len(name) > _NAME_MAX_LEN:
        raise DispatchAskError(
            f"name must be <={_NAME_MAX_LEN} chars (got {len(name)})",
            exit_code=2,
        )
    if _SHORT_ID_NAME_SHAPE.match(name):
        raise DispatchAskError(
            f"agent name {name!r} must not match short-id shape "
            f"^[0-9a-f]{{8}}$ (prevents name/id collision)",
            exit_code=2,
        )
    _forbidden_env_chars = ("\x00", "\n", "\r", "=")
    bad = next((ch for ch in _forbidden_env_chars if ch in name), None)
    if bad is not None:
        raise DispatchAskError(
            f"agent name {name!r} contains a forbidden character "
            f"({bad!r} would corrupt subprocess env injection)",
            exit_code=2,
        )
def _picked_headroom_note(account_id: str) -> str:
    """The picked account's worst window, for the receipt. Never raises."""
    try:
        from fno.adapters.providers.runtime_state import read_usage

        snap = read_usage(account_id)
        if snap is not None and snap.windows:
            worst = max(snap.windows, key=lambda w: w.used_pct)
            return f"{worst.label} {worst.used_pct:.0f}%"
    except Exception:  # noqa: BLE001 - a receipt detail must never break a spawn
        pass
    return "headroom unknown"


def _pick_account_overlay(
    *,
    role: Optional[str] = None,
    route_env: Optional[Mapping[str, str]] = None,
) -> Optional[Any]:
    """Consult the picker for a spawn that named no account, or None.

    Returns the overlay's ``env`` AND ``account_id``, because the
    spawn seams stamp the picked id on the row's ``launch_account`` ;
    the env alone drops the one fact re-entry needs.

    Advisory in every direction (see :func:`pick_account_id`): a routed spawn
    is never picked for, and the printed receipt keeps a silent re-accounting
    from becoming a billing surprise.
    """
    picked = pick_account_id(role=role, route_env=route_env)
    if picked is None:
        return None
    try:
        from types import SimpleNamespace

        from fno.agents.spawn_overlay_client import account_overlay

        return SimpleNamespace(**account_overlay(picked))
    except Exception as exc:  # noqa: BLE001 - picking is advisory; never block a spawn
        print(f"account: default (pick unavailable: {exc})", file=sys.stderr)
        return None


def pick_account_id(
    *,
    role: Optional[str] = None,
    route_env: Optional[Mapping[str, str]] = None,
) -> Optional[str]:
    """The account id a spawn that named none should launch on, or None.

    THE picking decision, with two consumers: the in-process spawn seams turn it
    into an env overlay, and the runtime seam turns it into an injected
    ``--account`` flag so the Rust client inherits the same choice. One decision,
    so those two can never disagree about which account a worker is billing.

    Advisory in every direction: opt-in via ``accounts.quota.pick_on_launch``,
    and any refusal or failure returns None so the spawn proceeds exactly as it
    does today. A routed spawn is never picked for: it bills the route's
    vendor, not an Anthropic account, so there is no quota to manage.
    """
    if route_env or role is not None:
        return None
    try:
        from fno.adapters.providers.cli import PICK_EXIT_NOT_ARMED, pick_account

        # `--if-armed` so the opt-in is read in exactly ONE place, the same call
        # the Rust loop makes. Checking `pick_on_launch` here as well would be a
        # second answer to one question, which is the shape this whole feature
        # exists to remove.
        verdict = pick_account(if_armed=True)
        if verdict.exit_code == PICK_EXIT_NOT_ARMED:
            return None  # off by default: say nothing, change nothing
        if verdict.account is None:
            print(
                f"account: default (pick unavailable: {verdict.reason})",
                file=sys.stderr,
            )
            return None
        print(
            f"account: {verdict.account} (picked by accounts.quota.pick_on_launch, "
            f"{_picked_headroom_note(verdict.account)})",
            file=sys.stderr,
        )
        return verdict.account
    except Exception as exc:  # noqa: BLE001 - picking is advisory; never block a spawn
        print(f"account: default (pick unavailable: {exc})", file=sys.stderr)
        return None


# ---------------------------------------------------------------------------
# US4-lifecycle: stop / rm / reconcile / attach (write + read commands)
# ---------------------------------------------------------------------------


_DEFAULT_CLAUDE_LOGS_TAIL_TIMEOUT = 10.0


@dataclass(frozen=True)
class ReconcileResult:
    """Return shape for :func:`reconcile_agents`.

    Lists are JSON-friendly: each entry is a dict with ``name``, ``provider``,
    optional ``id`` (short_id or session_id), optional ``reason``. The
    CLI emits this verbatim under ``--json`` (Locked Decision 4 mirror).
    """

    scanned: int
    orphaned: list[dict] = field(default_factory=list)
    recovered: list[dict] = field(default_factory=list)
    skipped: list[dict] = field(default_factory=list)
    errors: list[dict] = field(default_factory=list)
    # Live rows whose null canonical harness_session_id reconcile healed from the
    # harness store. Empty list (not absent) distinguishes "ran, nothing
    # to heal" from "healed": each entry is {name, provider, harness_session_id}.
    backfilled: list[dict] = field(default_factory=list)
    # rows whose structurally impossible mux ref reconcile cleared. Each
    # entry is {name, provider, harness, cleared_mux}; empty list = ran, nothing
    # to clear.
    mux_cleared: list[dict] = field(default_factory=list)


def reconcile_agents(
    *,
    claude_logs_timeout: float = _DEFAULT_CLAUDE_LOGS_TAIL_TIMEOUT,
    codex_session_index_path: Optional[Path] = None,
) -> ReconcileResult:
    """Walk the registry, sync statuses against provider reality, report.

    Read-mostly: each entry's status flip goes through ``update_registry``'s
    atomic load+filter+write cycle. No per-agent flock (Locked Decision 8):
    concurrent reconcile + ask is safe because ask mutates ``last_message_at``
    via the same atomic cycle and last-writer-wins on the timestamp; the
    status field updated by reconcile is independent.

    For each entry:

    - **claude**: ``claude logs <short_id> --tail 1`` (10s timeout) decides
      reachability. Exit 0 → live; anything else → orphaned.
    - **codex**: presence in ``~/.codex/session_index.jsonl`` decides
      reachability. Missing index → skip with an ``errors`` entry, leave
      status untouched (AC3-EDGE: fresh install must NOT trigger false
      orphan flags).
    - **gemini**: legacy registry rows are warned and skipped because the
      Python provider adapter is retired; new work routes to agy.

    Emits ``reconcile_done`` once at the end with the aggregate counts.
    """
    try:
        entries = load_registry()
    except (OSError, ValueError, RegistryVersionError) as exc:
        raise DispatchAskError(
            f"registry read failed: {exc}",
            exit_code=12,
        ) from exc

    entry_by_name = {entry.name: entry for entry in entries}
    # The PRIMARY index is (harness, harness_session_id): a label can be
    # renamed mid-flight; the pair cannot.
    entry_by_sid = {
        entry.harness_session_id: entry for entry in entries if entry.harness_session_id
    }

    def _entry_for(name: str, session_id: Optional[str] = None) -> Optional["AgentEntry"]:
        """Identity-first: a probed session id answers before the label."""
        if session_id:
            entry = entry_by_sid.get(session_id)
            if entry is not None:
                return entry
        return entry_by_name.get(name)

    def _vendor_for_name(name: str, session_id: Optional[str] = None) -> Optional[str]:
        entry = _entry_for(name, session_id)
        return entry.provider if entry is not None else None

    def _harness_for_name(name: str, session_id: Optional[str] = None) -> Optional[str]:
        entry = _entry_for(name, session_id)
        return entry.harness if entry is not None else None

    orphaned: list[dict] = []
    recovered: list[dict] = []
    skipped: list[dict] = []
    errors: list[dict] = []
    backfilled: list[dict] = []
    # rows whose structurally impossible mux ref was cleared (each
    # entry carries the cleared ref), with one ``mux_ref_cleared`` event each.
    mux_cleared: list[dict] = []
    # name -> (probed short_id, resolved harness_session_id) for a live row
    # whose canonical id never landed. Folded into the SAME batched
    # update_registry write as the status flips, so no new write cycle or lock
    # scope appears. The probed short_id is retained so the write only stamps a row
    # that STILL matches what we probed: a slow reconcile can race a rm + same-name
    # re-register, and stamping by name alone would put the old row's claude uuid
    # onto a replacement (possibly codex/gemini) row and misroute its mail.
    pending_backfill: dict[str, tuple[Optional[str], str]] = {}
    # name -> (pid, mux ref, resolved Codex thread id). Unlike the Claude
    # backfill above, every field is rechecked under the registry write lock:
    # the pane process is the identity authority and a same-name replacement
    # must never inherit its rollout.
    pending_codex_backfill: dict[
        str,
        tuple[Optional[int], Optional[int], dict, int, Optional[int], str],
    ] = {}

    # ``pending_updates`` accumulates per-name status flips across the
    # probe loop; at the end we apply ALL of them via a SINGLE
    # ``update_registry`` call. The dict[str, AgentEntry] shape (vs
    # list[tuple]) makes last-writer-wins explicit when the same name
    # could appear twice (Locked Decision 5; reconcile shouldn't probe
    # the same name twice today, but a future stale-cache or duplicate
    # row would silently collapse here instead of writing twice).
    #
    # Atomicity contract (AC3-ERR / Locked Decision 1): SIGINT mid-loop
    # discards ``pending_updates`` because the propagated KeyboardInterrupt
    # bypasses the post-loop ``update_registry`` call. The on-disk
    # registry mtime never changes.
    pending_updates: dict[str, tuple[AgentEntry, AgentStatus]] = {}

    # a mux ref that names no pane (a default-zero, an empty dict) is a
    # wrong value, and a wrong value is worse than no value -- 49 of 51 live
    # rows carry ``mux: None`` and every reader handles that, while the broken
    # shape made six live workers unreachable in every mux path. Queue the
    # clear for the SAME batched write as the status flips. Keyed by name with
    # the probed ref so the under-lock apply can compare-and-set against a
    # same-name replacement, exactly like the backfills above.
    pending_mux_clear: dict[str, dict] = {}

    # Read codex's session index ONCE outside the loop so a registry with
    # N codex agents only pays the I/O cost once. Mirror the same one-shot
    # capability check for claude so a host without `claude` on PATH does
    # NOT mass-flip every claude row to orphaned (sigma-review C1 finding:
    # the false-orphan storm is the worst kind of silent failure — it
    # rewrites the registry on insufficient evidence).
    from fno.agents.harnesses import codex as codex_mod
    from fno.agents.codex_pane import codex_backfill_stamp_applies

    # Tri-state per-codex-side capability: True (readable + present),
    # False (file missing, fresh install), None (file present but
    # unreadable, a permission or device error). Lumping "unreadable" with
    # "fresh install" would mass-orphan every codex agent on a host
    # with a permission glitch.
    codex_index_state: Optional[str] = None  # "ready" | "missing" | "unreadable"
    known_codex_ids: set[str] = set()
    claude_path_present: Optional[bool] = None
    for entry in entries:
        if entry.harness == "codex" and codex_index_state is None:
            # Probing the index path can raise PermissionError on hosts
            # where the parent directory is unreadable. Without this
            # catch a codex-local permission glitch would abort the
            # entire reconcile_agents call (Codex P1 round-4 finding
            # on PR #315). Treat any stat-time OSError as
            # "unreadable" — same operator outcome as a load-time
            # codex ReachabilityProbeError: route codex agents to errors.
            try:
                index_present = codex_mod.session_index_exists(
                    session_index_path=codex_session_index_path
                )
            except OSError as exc:
                codex_index_state = "unreadable"
                sys.stderr.write(
                    f"WARN: codex session index path unreadable: {exc}; "
                    "codex agents will be skipped (no reachability data)\n"
                )
            else:
                if not index_present:
                    codex_index_state = "missing"
                    missing_path = (
                        codex_session_index_path or codex_mod.default_session_index_path()
                    )
                    sys.stderr.write(
                        f"WARN: codex session index missing at {missing_path}; "
                        "codex agents will be skipped (no reachability data)\n"
                    )
                else:
                    try:
                        known_codex_ids = codex_mod.load_known_session_ids(
                            session_index_path=codex_session_index_path
                        )
                        codex_index_state = "ready"
                    except ReachabilityProbeError as exc:
                        # Catch the lifted base class so any codex-side probe
                        # error routes through the same path. The ``provider``
                        # attribute is "codex" by construction; the reason
                        # carries the underlying OSError detail.
                        codex_index_state = "unreadable"
                        sys.stderr.write(
                            f"WARN: codex session index unreadable: {exc.reason}; "
                            "codex agents will be skipped (no reachability data)\n"
                        )
        if entry.harness == "claude" and claude_path_present is None:
            claude_path_present = is_provider_available("claude")
            if not claude_path_present:
                sys.stderr.write(
                    "WARN: claude CLI not on PATH; claude agents will be "
                    "skipped (no reachability data — statuses will NOT be "
                    "flipped to orphaned)\n"
                )

    for entry in entries:
        new_status: AgentStatus
        if entry.mux is not None and not mux_ref_names_a_pane(entry.mux):
            # Structurally impossible ref: queue the heal, then probe this row
            # as the null-mux row it is about to become. Only an unreconcilable
            # ref is cleared here -- a ref that names a real pane is never touched,
            # alive or dead, and neither is one that merely could not be
            # probed (that distinction belongs to the pane falsifier's
            # False-vs-None split, not to validity).
            pending_mux_clear[entry.name] = entry.mux
        if entry.harness == "gemini":
            sys.stderr.write(
                f"WARN: agent {entry.name!r} references retired provider 'gemini'; "
                "skipping reachability probe (route new work to agy)\n"
            )
            errors.append(
                {
                    "name": entry.name,
                    "provider": entry.provider, "harness": entry.harness,
                    "id": entry.harness_session_id,
                    "reason": "retired-provider",
                }
            )
            continue
        elif entry.harness == "codex":
            # An id-less persistent pane can be healthy while Codex is still
            # creating its rollout. Heal it from the pane's own process tree;
            # the session index is not needed for this correlation. Gated on
            # ref VALIDITY, not truthiness: a ref that names no pane
            # must not send the row down the pane arm.
            if not entry.harness_session_id and mux_ref_names_a_pane(entry.mux):
                if entry.status in _TERMINAL_AGENT_STATUSES:
                    continue

                from fno.agents.mux_spawn import (
                    _codex_session_id_for_pid, _lookup_child_pid, _mux_pane_alive,
                )
                from fno.agents.spawn_gate import _pid_alive, _process_start_time

                pane_state = _mux_pane_alive(entry.mux)
                if pane_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "mux-pane-liveness-unavailable",
                        }
                    )
                    continue
                probe_pid = entry.pid
                probe_start = entry.pid_start_time
                # An incarnation token is what makes a recorded pid trustworthy:
                # without one `_pid_alive` degrades to bare existence, so a
                # recycled pid could bind a stranger's rollout onto this row. Rows
                # written before this stamping have no token, and they are exactly
                # the population this heal exists to repair - so re-derive the
                # pane's real child and require agreement. `_mux_pane_alive` proves
                # the PANE is up, never that this pid is still its child.
                confirmed = probe_start is not None
                if pane_state is not False and not confirmed:
                    live_pid = _lookup_child_pid(
                        str(entry.mux["session"]),
                        int(entry.mux["pane_id"]),
                        subprocess.run,
                    )
                    if live_pid is not None and probe_pid in (None, live_pid):
                        probe_pid = live_pid
                        probe_start = _process_start_time(probe_pid)
                        confirmed = True
                    elif probe_pid is None:
                        errors.append(
                            {
                                "name": entry.name,
                                "provider": entry.provider, "harness": entry.harness,
                                "id": None,
                                "reason": "codex-pane-pid-pending",
                            }
                        )
                        continue

                pid_state = (
                    _pid_alive(probe_pid, probe_start)
                    if pane_state is True and probe_pid is not None
                    else False
                )
                if pid_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "codex-process-incarnation-unavailable",
                        }
                    )
                    continue
                if pid_state is True:
                    assert probe_pid is not None
                    # Live, but we could not prove this pid is the pane's child.
                    # Never stamp an identity on that; a DEAD unconfirmed pid still
                    # takes the orphan path below, which binds nothing.
                    if not confirmed:
                        errors.append(
                            {
                                "name": entry.name,
                                "provider": entry.provider, "harness": entry.harness,
                                "id": None,
                                "reason": "codex-pane-pid-unconfirmed",
                            }
                        )
                        continue
                    probe_failed = False
                    try:
                        healed = _codex_session_id_for_pid(probe_pid)
                    except Exception as exc:  # noqa: BLE001 -- stays pending, never guesses
                        # The correlator already absorbs every EXPECTED failure
                        # (gone / access-denied / no rollout open) and returns None
                        # for them, so anything landing here is unexpected. Keep the
                        # row pending, but never render it as the benign "codex has
                        # not opened its rollout yet" case with the cause dropped -
                        # that reads as "still starting" forever.
                        healed = None
                        probe_failed = True
                        sys.stderr.write(
                            f"WARN: codex identity probe failed for {entry.name} "
                            f"(pid {probe_pid}): {exc}\n"
                        )
                    if healed:
                        # Scan the rows AND the ids this same pass already decided
                        # to stamp. Two id-less rows pointing at one pane both see
                        # the other as `harness_session_id=None`, so a rows-only
                        # check clears both and stamps the duplicate identity this
                        # guard exists to prevent.
                        duplicate = any(
                            other.name != entry.name
                            and other.harness_session_id == healed
                            for other in entries
                        ) or any(
                            claimed[-1] == healed
                            for name, claimed in pending_codex_backfill.items()
                            if name != entry.name
                        )
                        if duplicate:
                            if entry.status != "spawning":
                                pending_updates[entry.name] = (entry, "spawning")
                            errors.append(
                                {
                                    "name": entry.name,
                                    "provider": entry.provider, "harness": entry.harness,
                                    "id": None,
                                    "reason": "duplicate-codex-session-id",
                                }
                            )
                        else:
                            pending_codex_backfill[entry.name] = (
                                entry.pid,
                                entry.pid_start_time,
                                dict(entry.mux),
                                probe_pid,
                                probe_start,
                                healed,
                            )
                        continue

                    if entry.status != "spawning":
                        pending_updates[entry.name] = (entry, "spawning")
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": (
                                "codex-session-probe-failed"
                                if probe_failed
                                else "codex-session-id-pending"
                            ),
                        }
                    )
                    continue

                # The pane process is gone. Preserve terminal rows above; a
                # nonterminal pending/live row follows the existing orphan path.
                new_status = "orphaned"
            elif entry.status in {"failed", "exited", "permanent_dead"}:
                continue
            elif not entry.harness_session_id and entry.status == "orphaned":
                continue
            elif not entry.harness_session_id:
                events.emit(
                    "agent_inconsistent",
                    name=entry.name,
                    provider="codex",
                )
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": None,
                        "reason": "missing-codex-session-id",
                    }
                )
                continue
            elif codex_index_state != "ready":
                # AC3-EDGE: cannot probe codex reachability; report as
                # error but do NOT flip status. The reason discriminator
                # distinguishes "fresh install" (operator action: ignore)
                # from "permission glitch" (operator action: fix perms).
                if codex_index_state == "unreadable":
                    reason = "codex-session-index-unreadable"
                else:
                    reason = "codex-session-index-missing"
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": entry.harness_session_id,
                        "reason": reason,
                    }
                )
                continue
            else:
                reachable = entry.harness_session_id in known_codex_ids
                new_status = "live" if reachable else "orphaned"

        elif entry.harness == "claude":
            # EVERY claude PANE row reconciles from the pane, with or without a
            # session id. A mux row's short_id is deliberately empty (one live ref
            # per row), so the `claude logs` probe below - which is keyed on
            # short_id - can never reach one; without this arm a pane row falls to
            # `missing-claude-short-id`, which only reports and never changes
            # status, so a dead pane holds its name against every future spawn of
            # that name, forever.
            #
            # Gating this on `not harness_session_id` was the earlier shape and it
            # was the AGENTS.md path-uniqueness trap in miniature: it covered the
            # pane only until its worker restamped, and a pane that then died was
            # unreachable by any arm. The pane is the reachability oracle for a
            # pane-hosted row for its whole life, not just before it has an id.
            #
            # Deliberately ahead of the claude-on-PATH guard: this probes the mux
            # and the pid, never the claude CLI, so a host where claude was
            # removed can still retire a provably dead pane. Gated on ref
            # VALIDITY, not truthiness: a ref that names no pane is
            # not a pane row and must not be pane-probed.
            if mux_ref_names_a_pane(entry.mux):
                if entry.status in _TERMINAL_AGENT_STATUSES:
                    continue

                from fno.agents.mux_spawn import _lookup_child_pid, _mux_pane_alive
                from fno.agents.spawn_gate import _pid_alive, _process_start_time

                pane_state = _mux_pane_alive(entry.mux)
                if pane_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "mux-pane-liveness-unavailable",
                        }
                    )
                    continue
                probe_pid = entry.pid
                probe_start = entry.pid_start_time
                # `_mux_pane_alive` proves the PANE is up, never that the recorded
                # pid is still its child. Without an incarnation token `_pid_alive`
                # degrades to bare existence, so re-derive the pane's real child
                # before trusting a live answer. Unlike the codex arm this never
                # stamps an identity from the pid, so an unconfirmed pid only ever
                # keeps the row waiting - it can never bind a stranger.
                # ALWAYS re-derive the pane's current child, even when the row
                # carries an incarnation token. That token proves only that the
                # stored pid's incarnation is alive; it says nothing about which
                # pane that process now belongs to. A mux restart can hand
                # `(session, pane_id)` to a different child while the original is
                # still running, and then `_mux_pane_alive` and `_pid_alive` are
                # both true about DIFFERENT processes - which preserves the row as
                # live and points delivery at the stranger's pane.
                confirmed = False
                if pane_state is not False:
                    live_pid = _lookup_child_pid(
                        str(entry.mux["session"]),
                        int(entry.mux["pane_id"]),
                        subprocess.run,
                    )
                    if live_pid is not None and probe_pid in (None, live_pid):
                        probe_pid = live_pid
                        # Keep a stored incarnation token: it is stronger than a
                        # fresh read, which cannot see a pid recycled since.
                        if probe_start is None:
                            probe_start = _process_start_time(probe_pid)
                        confirmed = True
                # A live pane with no usable pid is INCONCLUSIVE, never dead.
                # `_lookup_child_pid` is best-effort, so folding its miss into
                # `False` would orphan a healthy worker on absent evidence - and
                # `orphaned` is terminal here while a later restamp only promotes
                # `spawning`, so that mistake is permanent. Stay pending, exactly
                # as the codex arm does with `codex-pane-pid-pending`.
                if pane_state is True and probe_pid is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-pane-pid-pending",
                        }
                    )
                    continue
                # An uncorrelated pid may not be this pane's at all. A legacy row
                # carries no incarnation token, so `_pid_alive` degrades to bare
                # existence there; a mux restart can hand `(session, pane_id)` to
                # a different child while the recorded pid has been RECYCLED and
                # is alive. Trusting that keeps the row `live` and points
                # name-based delivery at a stranger. Only a DEAD pane is decided
                # without correlation, since nothing can be bound to it.
                if pane_state is True and not confirmed:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-pane-pid-unconfirmed",
                        }
                    )
                    continue
                pid_state = (
                    _pid_alive(probe_pid, probe_start)
                    if pane_state is True and probe_pid is not None
                    else False
                )
                if pid_state is None:
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": None,
                            "reason": "claude-process-incarnation-unavailable",
                        }
                    )
                    continue
                if pid_state is True:
                    # The pane is up. Leave the status alone: a row still owed
                    # its restamp is correctly `spawning`, and one that already
                    # got it was promoted to `live` by the restamp itself. Either
                    # way this pass has nothing to correct.
                    continue
                new_status = "orphaned"

            elif not claude_path_present:
                # Mirror the codex-index-missing pattern: when claude is
                # not installed we cannot probe reachability, so we route
                # the entry to `errors` with status untouched. Anything
                # else would mass-flip every claude row to orphaned on a
                # host where claude was removed mid-day.
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": entry.short_id,
                        "reason": "claude-cli-not-on-path",
                    }
                )
                continue
            elif not entry.short_id:
                events.emit(
                    "agent_inconsistent",
                    name=entry.name,
                    provider="claude",
                )
                errors.append(
                    {
                        "name": entry.name,
                        "provider": entry.provider, "harness": entry.harness,
                        "id": None,
                        "reason": "missing-claude-short-id",
                    }
                )
                continue
            else:
                from fno.agents.harnesses import claude as claude_mod

                # Phase 5: MCP-backed claude agents probe via the sidecar
                # instead of `claude logs`. Same tri-state contract:
                # True/False/raise. Socket-only agents (mcp_channel_id is
                # None) keep the legacy claude_logs_reachable path.
                # NOTE: probe_label is assigned BEFORE the probe call so a
                # ReachabilityProbeError from the probe still has the
                # label in scope for the error route.
                probe_label = (
                    "claude-mcp-probe-failed" if entry.mcp_channel_id else "claude-probe-failed"
                )
                try:
                    if entry.mcp_channel_id:
                        reachable = claude_mod.mcp_channel_reachable(entry.mcp_channel_id, timeout=0.25)
                    else:
                        reachable = claude_mod.claude_logs_reachable(
                            entry.short_id, timeout=claude_logs_timeout
                        )
                except ReachabilityProbeError as exc:
                    # Catch the lifted base class (US4-gemini Wave 1.1) so
                    # both the claude-side timeout/OSError probe error and the
                    # Phase 5 ``mcp_channel_disconnected`` probe error are routed
                    # identically. Probe inconclusive -> preserve status,
                    # route to errors with a per-provider reason
                    # discriminator. Mirrors the codex-side
                    # codex-session-index-unreadable treatment so transient
                    # CLI slowness or sidecar I/O hiccups don't mass-orphan
                    # healthy agents (Codex P1 round-5 on PR #315).
                    events.emit(
                        "agent_inconsistent",
                        name=entry.name,
                        provider="claude",
                        reason=exc.reason,
                    )
                    errors.append(
                        {
                            "name": entry.name,
                            "provider": entry.provider, "harness": entry.harness,
                            "id": entry.short_id,
                            "reason": f"{probe_label}: {exc.reason}",
                        }
                    )
                    continue
                new_status = "live" if reachable else "orphaned"

                # US4 heal: a live claude row whose canonical id never landed
                # (the uuid resolution raced at spawn) is unroutable-but-live. Resolve
                # it from claude's own store -- the same jsonl the liveness probe just
                # read -- and fold the write into reconcile's single batched cycle. A
                # miss leaves it null (the durable queue stays the floor); never fatal.
                if reachable and not entry.harness_session_id and entry.short_id:
                    try:
                        healed = claude_mod.resolve_session_uuid(entry.short_id)
                    except Exception:  # noqa: BLE001 — a resolver error is a tolerated miss
                        healed = None
                    if healed:
                        # Queue only. The `backfilled` claim is made AFTER the write,
                        # against the names the write actually stamped: the under-lock
                        # guard can refuse this row (same-name rm + re-register), and
                        # claiming success here reported a heal that never landed.
                        pending_backfill[entry.name] = (entry.short_id, healed)

        else:
            errors.append(
                {
                    "name": entry.name,
                    "provider": entry.provider, "harness": entry.harness,
                    "id": None,
                    "reason": f"unknown-provider-{entry.harness}",
                }
            )
            continue

        if entry.status == new_status:
            continue  # no change; do not write

        # Status drifted — queue the updated entry for the batched
        # single-cycle write at the end of the loop. ``dataclasses.replace``
        # preserves every other field automatically (Gemini medium on
        # PR #317), which is more robust against future AgentEntry
        # schema additions than manual field-by-field reconstruction.
        pending_updates[entry.name] = (entry, new_status)

        change = {
            "name": entry.name,
            "provider": entry.provider, "harness": entry.harness,
            # Codex P2 on PR #317: include gemini_session_id so reconcile
            # records carry an identifier for every provider. Pre-fix
            # gemini agents flipped between live/orphaned with "id": null
            # which rendered as "?" in human output and broke follow-up
            # tooling.
            "id": (entry.short_id or entry.harness_session_id),
        }
        if new_status == "orphaned":
            orphaned.append(change)
        else:
            recovered.append(change)

    # Single atomic write for ALL queued flips (AC3-HP: at most one
    # update_registry call per reconcile). Empty pending_updates
    # short-circuits with no write at all (AC3-UI). On disk-write
    # failure, every queued change moves from orphaned/recovered into
    # errors so the operator sees a single coherent failure rather than
    # a partial split. The all-or-nothing atomicity is enforced by
    # update_registry's own atomic-rename semantics — the closure is
    # pure, so an OSError mid-write leaves the registry untouched.
    if pending_updates or pending_backfill or pending_codex_backfill or pending_mux_clear:

        codex_backfill_applied: set[str] = set()
        status_updates_applied: set[str] = set()
        codex_ids_claimed: set[str] = set()
        claude_backfill_applied: set[str] = set()
        mux_clears_applied: set[str] = set()

        def _apply(current_entries: list[AgentEntry]) -> list[AgentEntry]:
            # Reset per call: update_registry may re-run _apply against a fresh
            # snapshot after lock contention, and carrying a previous attempt's
            # names over would report a row as applied that this attempt skipped.
            codex_backfill_applied.clear()
            status_updates_applied.clear()
            codex_ids_claimed.clear()
            claude_backfill_applied.clear()
            mux_clears_applied.clear()
            # Build the new entries from the CURRENT (under-lock) entries,
            # overriding only the ``status`` field from pending_updates.
            # Pre-fix this returned ``pending_updates.get(e.name, e)`` which
            # substituted the entire snapshot AgentEntry captured at probe
            # time — silently losing any ``last_message_at`` bump that
            # dispatch_ask wrote during the probe loop (US4-gemini handoff:
            # concurrent reconcile + ask data loss).
            out: list[AgentEntry] = []
            for e in current_entries:
                updates: dict = {}
                if e.name in pending_mux_clear:
                    # Compare-and-set against the probed ref: a same-name
                    # re-register during the probe loop may have written a
                    # fresh hosting ref, and this stale heal must never wipe
                    # it. Change NO other field here -- a heal that becomes a
                    # second bug is the exact failure this narrowness prevents.
                    if e.mux == pending_mux_clear[e.name]:
                        updates["mux"] = None
                        mux_clears_applied.add(e.name)
                if e.name in pending_updates:
                    probed, target_status = pending_updates[e.name]
                    if (
                        e.harness == probed.harness
                        and e.pid == probed.pid
                        and e.pid_start_time == probed.pid_start_time
                        and e.mux == probed.mux
                        and e.harness_session_id == probed.harness_session_id
                        and e.short_id == probed.short_id
                        and e.status == probed.status
                    ):
                        updates["status"] = target_status
                        status_updates_applied.add(e.name)
                if e.name in pending_backfill:
                    probed_short, hsid = pending_backfill[e.name]
                    # Only stamp a row that STILL matches the row we probed: a
                    # same-name rm+re-register during the probe loop would put this
                    # claude uuid onto a replacement row (misrouting its mail).
                    if e.harness == "claude" and e.short_id == probed_short:
                        # Canonical wins: set harness_session_id; the legacy
                        # claude uuid is synced from it on the next load's backfill.
                        updates["harness_session_id"] = hsid
                        updates["harness"] = e.harness
                        claude_backfill_applied.add(e.name)
                if e.name in pending_codex_backfill:
                    (
                        expected_pid,
                        expected_start,
                        probed_mux,
                        resolved_pid,
                        resolved_start,
                        hsid,
                    ) = pending_codex_backfill[e.name]
                    # `codex_ids_claimed` carries ids stamped EARLIER IN THIS APPLY,
                    # which `current_entries` cannot show because it is a consistent
                    # pre-update snapshot. Without it two rows racing for one pane
                    # both read "nobody owns this id" and both take it.
                    duplicate = hsid in codex_ids_claimed or any(
                        other.name != e.name
                        and other.harness_session_id == hsid
                        for other in current_entries
                    )
                    # ONE guard, branch inside; the conjuncts are the shared
                    # codex_pane.codex_backfill_stamp_applies helper the spawn-time
                    # late-bind stamp uses, so the two stampers cannot drift.
                    if codex_backfill_stamp_applies(
                        e, e.name, hsid, expected_pid, expected_start, probed_mux
                    ):
                        if duplicate:
                            updates["status"] = "spawning"
                        else:
                            updates["harness_session_id"] = hsid
                            updates["status"] = "live"
                            updates["pid"] = resolved_pid
                            updates["pid_start_time"] = resolved_start
                            codex_ids_claimed.add(hsid)
                            codex_backfill_applied.add(e.name)
                out.append(replace(e, **updates) if updates else e)
            return out

        try:
            update_registry(_apply)
        except (OSError, RegistryVersionError) as exc:
            # Re-classify every queued change as a write failure. Move
            # them out of orphaned/recovered into errors so callers don't
            # see a recovered/orphaned record that never actually committed.
            # A backfill that never committed must not claim it healed either.
            write_error = f"registry-write-failed: {exc}"
            failed_names = set(pending_updates.keys()) | set(pending_codex_backfill.keys())
            for change in list(orphaned):
                if change["name"] in failed_names:
                    orphaned.remove(change)
                    errors.append({**change, "reason": write_error})
            for change in list(recovered):
                if change["name"] in failed_names:
                    recovered.remove(change)
                    errors.append({**change, "reason": write_error})
            for change in list(backfilled):
                backfilled.remove(change)
                errors.append({**change, "id": None, "reason": write_error})
            for name in (*pending_backfill, *pending_codex_backfill, *pending_mux_clear):
                errors.append(
                    {
                        "name": name,
                        "provider": _vendor_for_name(name), "harness": _harness_for_name(name),
                        "id": None,
                        "reason": write_error,
                    }
                )
        else:

            def _record_backfill(
                name: str, hsid: Optional[str], applied: set, raced: str
            ) -> None:
                if name in applied:
                    backfilled.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name, hsid), "harness": _harness_for_name(name, hsid),
                            "harness_session_id": hsid,
                        }
                    )
                else:
                    errors.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name, hsid), "harness": _harness_for_name(name, hsid),
                            "id": None,
                            "reason": raced,
                        }
                    )

            for name, (_probed_short, hsid) in pending_backfill.items():
                _record_backfill(name, hsid, claude_backfill_applied, "claude-session-id-backfill-raced")
            for name, (_epid, _estart, _mux, _pid, _start, hsid) in pending_codex_backfill.items():
                _record_backfill(name, hsid, codex_backfill_applied, "codex-session-id-backfill-raced")
            raced_updates = set(pending_updates) - status_updates_applied
            for change in list(orphaned):
                if change["name"] in raced_updates:
                    orphaned.remove(change)
            for change in list(recovered):
                if change["name"] in raced_updates:
                    recovered.remove(change)
            for name in raced_updates:
                probed, _target = pending_updates[name]
                errors.append(
                    {
                        "name": name,
                        "provider": probed.provider, "harness": probed.harness,
                        "id": probed.short_id or probed.harness_session_id,
                        "reason": "registry-status-update-raced",
                    }
                )
            for name, probed_mux in pending_mux_clear.items():
                if name in mux_clears_applied:
                    # AC6: the heal is spoken. One event per cleared ref, naming
                    # exactly what was removed, so the heal is auditable from
                    # events.jsonl alone.
                    mux_cleared.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name),
                            "harness": _harness_for_name(name),
                            "cleared_mux": probed_mux,
                        }
                    )
                    events.emit(
                        "mux_ref_cleared",
                        name=name,
                        provider=_vendor_for_name(name),
                        harness=_harness_for_name(name),
                        cleared_mux=probed_mux,
                    )
                else:
                    errors.append(
                        {
                            "name": name,
                            "provider": _vendor_for_name(name),
                            "harness": _harness_for_name(name),
                            "id": None,
                            "reason": "mux-ref-clear-raced",
                        }
                    )

    events.emit(
        "reconcile_done",
        scanned=len(entries),
        orphaned=len(orphaned),
        recovered=len(recovered),
        skipped=len(skipped),
        errors=len(errors),
        backfilled=len(backfilled),
        mux_cleared=len(mux_cleared),
    )
    return ReconcileResult(
        scanned=len(entries),
        orphaned=orphaned,
        recovered=recovered,
        skipped=skipped,
        errors=errors,
        backfilled=backfilled,
        mux_cleared=mux_cleared,
    )


# ---------------------------------------------------------------------------
# G2 Task 2.1 — send command (async, durable-first)
# ---------------------------------------------------------------------------


# Re-exported: the envelope identity context moved to its own module, and
# `fno.agents.dispatch` stays the import path callers already use.


# ---------------------------------------------------------------------------
# Project-destination addressing (anycast) - Group 3 Task 3.3 (US6)
# ---------------------------------------------------------------------------
# Project/cwd is demoted from address to resolver. `send --to-project X` (and
# `ask --to-project`) resolves over the registry: cwd->project mapping plus the
# config.inbox.peers `project:` hint. Rule: exactly one live peer -> deliver
# live; none -> durable queue to project X; many -> error listing the live
# candidates unless `--any` breaks the tie (most recent last_message_at wins,
# lexicographic registry name as the final tiebreak). One log underneath.


