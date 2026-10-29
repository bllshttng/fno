"""Per-provider runtime state for exponential-backoff failover.

Plan A of provider failover hardening. Distinct from the
phase-scoped ``failover-state.json`` (storm-cap, no-swap-back): this
file owns per-provider exponential backoff state that survives target
spawns within a megawalk campaign.

State path: machine-wide, ``fno.paths.runtime_state_json()``.
Override via ``FNO_RUNTIME_STATE_PATH`` env var (used by tests).

Concurrency: every write goes through the Rust writer
(``fno-agents spawn-overlay accounts state <op>``), which serializes on a
``<file>.update.lock`` flock and commits via an atomic rename. Reads here are
lock-free.

TTL: ProviderHealth entries are dropped lazily on the next state read or locked
write when ``last_error_at`` is older than ``PROVIDER_HEALTH_TTL_SECONDS`` (1h).
The lock deadline does not extend health indefinitely; a fresh usage observation
can replace an old reactive verdict before either clock expires.

Plan B (Spec 4) will add ``combo_cursors: dict[str, ComboCursor]`` to
the same top-level state dataclass; ``schema_version: int`` is bumped
when that lands.
"""
from __future__ import annotations

import dataclasses
import json
import logging
import os
import time
from pathlib import Path
from typing import Any

from fno.adapters.providers.error_taxonomy import ErrorRule
from fno.adapters.providers.usage import UsageSnapshot, UsageWindow, _clamp_pct

logger = logging.getLogger(__name__)


MAX_BACKOFF_LEVEL = 15
PROVIDER_HEALTH_TTL_SECONDS = 60 * 60
COMBO_CURSOR_TTL_SECONDS = 24 * 60 * 60  # Plan B (Spec 4): cursor goes stale after 24h
DEFAULT_USAGE_TTL_SECONDS = 300  # quota.probe_ttl_seconds default; snapshot freshness
SCHEMA_VERSION = 2  # Plan B: combo_cursors added; v1 files migrate transparently on read


@dataclasses.dataclass(frozen=True)
class ProviderHealth:
    """Per-provider backoff state.

    ``backoff_level`` starts at 0 and increments by 1 on each consecutive
    backoff-class error, capped at ``MAX_BACKOFF_LEVEL`` (15).

    ``model_locks`` (Plan A1) is a dict mapping
    model identifier -> unix-epoch-seconds cooldown expiry. It lets a
    quota error on one model lock only that model while leaving the
    provider record (and its other models) usable. Provider-level
    ``rate_limited_until`` and per-model ``model_locks`` are written
    independently: ``update_provider_health(model=X)`` writes only
    ``model_locks[X]``; ``update_provider_health(model=None)`` writes
    only ``rate_limited_until``. The TTL applies at the record level -
    when ``last_error_at`` ages out, every ``model_locks`` entry under
    the same record goes with it.
    """

    provider_id: str
    backoff_level: int = 0
    rate_limited_until: float | None = None
    last_error_at: float | None = None
    model_locks: dict[str, float] = dataclasses.field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.provider_id:
            raise ValueError(
                "ProviderHealth.provider_id must be a non-empty string"
            )
        if not 0 <= self.backoff_level <= MAX_BACKOFF_LEVEL:
            raise ValueError(
                f"ProviderHealth.backoff_level={self.backoff_level} "
                f"out of [0, {MAX_BACKOFF_LEVEL}]"
            )
        for model_id, until_ts in self.model_locks.items():
            if not isinstance(model_id, str) or not model_id:
                raise ValueError(
                    "ProviderHealth.model_locks keys must be non-empty "
                    f"strings, got {model_id!r}"
                )
            if not isinstance(until_ts, (int, float)) or until_ts <= 0:
                raise ValueError(
                    "ProviderHealth.model_locks[%r] must be a positive "
                    "timestamp, got %r" % (model_id, until_ts)
                )


@dataclasses.dataclass(frozen=True)
class ComboCursor:
    """Per-combo round-robin cursor state.

    Plan B (Spec 4). One entry per round-robin combo, keyed by
    combo name in ``ProviderRuntimeState.combo_cursors``. The cursor sat on
    ``cursor_index`` for ``sticky_limit`` advances before rolling to the next
    index; nothing advances it today, so it is read-only.

    ``providers_hash`` invalidates the cursor when the user edits the combo
    mid-session (add/remove/reorder providers): on a hash mismatch the cursor
    resets to (idx=0, count=1) cleanly, with no stored intermediate state.

    ``last_rotated_at`` is the unix epoch seconds of the last advance and
    drives the 24h TTL: a quiescent combo's cursor is dropped on the next
    locked write.
    """

    combo_name: str
    cursor_index: int
    consecutive_use_count: int
    providers_hash: str
    last_rotated_at: float

    def __post_init__(self) -> None:
        if not self.combo_name:
            raise ValueError("ComboCursor.combo_name must be a non-empty string")
        if self.cursor_index < 0:
            raise ValueError(
                f"ComboCursor.cursor_index={self.cursor_index} must be >= 0"
            )
        if self.consecutive_use_count < 0:
            raise ValueError(
                f"ComboCursor.consecutive_use_count="
                f"{self.consecutive_use_count} must be >= 0"
            )
        if not self.providers_hash:
            raise ValueError("ComboCursor.providers_hash must be a non-empty string")


@dataclasses.dataclass(frozen=True)
class ProviderRuntimeState:
    """Top-level runtime state for the provider rotation substrate.

    Plan A populates ``provider_health`` (per-provider exponential backoff).
    Plan B adds ``combo_cursors`` (per-combo round-robin position). v1 files
    parse cleanly with an empty ``combo_cursors`` dict; the next write
    rewrites the file as v2.
    """

    provider_health: dict[str, ProviderHealth]
    combo_cursors: dict[str, ComboCursor] = dataclasses.field(default_factory=dict)
    usage: dict[str, UsageSnapshot] = dataclasses.field(default_factory=dict)
    # When each provider's rolling usage window OPENED, as
    # ``{provider_id: {label: {"opened_at": float, "warned": bool}}}``. The one
    # thing footnote can know about a provider with no usage probe: z.ai is
    # UNKNOWN to the quota layer (usage.py registers probes for claude and codex
    # only), so nothing could ever say how close its five-hour window was to
    # closing. A recorded open plus a configured length is a PROJECTION, never a
    # utilization reading - it deliberately does not become a UsageWindow,
    # because a headroom verdict would then render a confident percentage no
    # probe measured. Additive, so v2 files read forward with no migration.
    windows_opened: dict[str, dict[str, dict[str, Any]]] = dataclasses.field(
        default_factory=dict
    )
    schema_version: int = SCHEMA_VERSION


def _resolve_state_path(repo_root: Path | None = None) -> Path:
    """Return the active runtime-state path, honoring env override.

    Machine-wide by design (M5): quota is a property of an ACCOUNT at a vendor,
    so the same account rate-limited in one repo is rate-limited everywhere and
    a per-cwd file cannot hold it. ``repo_root`` no longer moves the file. The
    env override wins (it is how tests pin a state file). The Rust writer folds
    any legacy per-cwd file into this one before it writes.
    """
    override = os.environ.get("FNO_RUNTIME_STATE_PATH")
    if override:
        return Path(override)
    from fno.paths import runtime_state_json

    return runtime_state_json()


def _state_write(op: str, payload: dict[str, Any]) -> dict[str, Any]:
    """One call to ``fno-agents spawn-overlay accounts state <op>``.

    The Rust writer is the one writer of the file: it takes the
    ``<file>.update.lock`` flock, re-reads under it, drops stale entries and
    replaces the file atomically. Raises ``CommandUnavailable`` when the
    binary is missing or the write fails.
    """
    from fno.rust_binary import command_call

    return command_call(["spawn-overlay", "accounts", "state", op], payload)


WINDOW_LABEL = "window"


def record_reset_timezone(provider_id: str) -> str | None:
    """``accounts.<id>.reset_timezone``, or None.

    Only a NAIVE reset stamp needs it. Fail-open: a config read that raises on
    the lock-write path would turn "we could not resolve a timezone" into "the lock was never
    written", which is strictly worse than falling back to the backoff.
    """
    try:
        from fno.adapters.providers.cli import _load

        for rec in _load().records:
            if rec.id == provider_id:
                return getattr(rec, "reset_timezone", None) or None
    except Exception:  # noqa: BLE001 - a config miss refuses the stamp, not the write
        return None
    return None


def stamp_window_open(
    provider_id: str,
    opened_at: float,
    *,
    label: str = WINDOW_LABEL,
    repo_root: Path | None = None,
) -> bool:
    """Record when ``provider_id``'s current window opened. True iff written.

    Idempotent against a window still in force: a second stamp inside the same
    window is a no-op, so a repeated dispatch never resets the countdown.
    """
    from fno.rust_binary import CommandUnavailable

    try:
        out = _state_write("stamp-window", {
            "provider_id": provider_id,
            "opened_at": opened_at,
            "label": label,
            "repo_root": None if repo_root is None else str(repo_root),
        })
    except CommandUnavailable as exc:
        logger.warning("runtime_state: could not stamp window open for %r: %s", provider_id, exc)
        return False
    return bool(out.get("written"))


def _parse_windows_opened(
    raw: dict[str, Any]
) -> dict[str, dict[str, dict[str, Any]]]:
    """Best-effort parse of the ``windows_opened`` block.

    Same drop-the-bad-entry discipline as :func:`_parse_usage_payload`: one
    corrupt record must never cost the whole file. A projection is advisory, so
    a dropped entry costs a printed ``unknown`` and nothing else.
    """
    out: dict[str, dict[str, dict[str, Any]]] = {}
    block = raw.get("windows_opened") or {}
    if not isinstance(block, dict):
        return out
    for pid, labels in block.items():
        if not isinstance(labels, dict):
            logger.warning(
                "runtime_state: dropping windows_opened for %r (not a dict)", pid
            )
            continue
        kept: dict[str, dict[str, Any]] = {}
        for label, entry in labels.items():
            if not isinstance(entry, dict):
                continue
            try:
                kept[str(label)] = {
                    "opened_at": float(entry["opened_at"]),
                    "warned": bool(entry.get("warned", False)),
                }
            except (KeyError, TypeError, ValueError):
                logger.warning(
                    "runtime_state: dropping window %r for %r (malformed)",
                    label, pid,
                )
        if kept:
            out[str(pid)] = kept
    return out


def _parse_usage_payload(raw: dict[str, Any]) -> dict[str, UsageSnapshot]:
    """Best-effort parse of the ``usage`` block into UsageSnapshot entries.

    Mirrors ``_parse_state_payload``: a partially-corrupt entry (string
    ``used_pct``, non-dict window) is dropped with a warning, never raised.
    ``used_pct`` is re-clamped on disk-read so a hand-corrupted value can never
    escape the [0, 100] invariant. An empty ``windows`` list is preserved (it
    reads as UNKNOWN, never OK).

    ``resets_at`` is nullable and may be absent, so a window written before
 and a reset-less window written after it both load. ``partial``
    defaults False and ``confidence`` defaults ``unknown``: a row already on
    disk was written by a probe that recorded neither, and claiming ``exact``
    for it would assert precision nobody measured.
    """
    out: dict[str, UsageSnapshot] = {}
    block = raw.get("usage") or {}
    if not isinstance(block, dict):
        return out
    for pid, entry in block.items():
        if not isinstance(entry, dict):
            logger.warning("runtime_state: dropping malformed usage for %r (not a dict)", pid)
            continue
        raw_windows = entry.get("windows")
        if not isinstance(raw_windows, list):
            logger.warning("runtime_state: dropping usage for %r (windows not a list)", pid)
            continue
        windows: list[UsageWindow] = []
        bad = False
        for w in raw_windows:
            if not isinstance(w, dict):
                bad = True
                break
            try:
                raw_reset = w.get("resets_at")
                windows.append(
                    UsageWindow(
                        label=str(w["label"]),
                        used_pct=_clamp_pct(float(w["used_pct"])),
                        resets_at=None if raw_reset is None else float(raw_reset),
                    )
                )
            except (KeyError, TypeError, ValueError):
                bad = True
                break
        if bad:
            logger.warning("runtime_state: dropping usage for %r (malformed window)", pid)
            continue
        try:
            out[pid] = UsageSnapshot(
                provider_id=str(pid),
                windows=tuple(windows),
                probed_at=float(entry["probed_at"]),
                source=str(entry.get("source", "")),
                partial=bool(entry.get("partial", False)),
                confidence=str(entry.get("confidence") or "unknown"),
            )
        except (KeyError, TypeError, ValueError) as exc:
            logger.warning("runtime_state: dropping malformed usage for %r: %s", pid, exc)
    return out


def _parse_cursors_payload(raw: dict[str, Any]) -> dict[str, ComboCursor]:
    """Best-effort parse of combo_cursors block. Drops malformed entries.

    Treats absence-of-providers_hash, missing required fields, and bad types
    as "skip silently". Same invariant as ``_parse_state_payload``: a
    partially-corrupt file degrades to "missing entry", never raises.
    """
    cursors: dict[str, ComboCursor] = {}
    block = raw.get("combo_cursors") or {}
    if not isinstance(block, dict):
        return cursors
    for name, entry in block.items():
        if not isinstance(entry, dict):
            logger.warning(
                "runtime_state: dropping malformed combo_cursor for %r (not a dict)",
                name,
            )
            continue
        # All four runtime fields must be present and well-typed; legacy
        # entries from a future schema cleanup that lack providers_hash
        # are dropped here so callers see "no cursor" and start fresh.
        try:
            cursors[name] = ComboCursor(
                combo_name=str(name),
                cursor_index=int(entry["cursor_index"]),
                consecutive_use_count=int(entry["consecutive_use_count"]),
                providers_hash=str(entry["providers_hash"]),
                last_rotated_at=float(entry["last_rotated_at"]),
            )
        except (KeyError, TypeError, ValueError) as exc:
            logger.warning(
                "runtime_state: dropping malformed combo_cursor for %r: %s",
                name,
                exc,
            )
    return cursors


def _drop_stale_cursors(
    cursors: dict[str, ComboCursor], now: float
) -> tuple[dict[str, ComboCursor], int]:
    """Return (kept_entries, dropped_count) using COMBO_CURSOR_TTL_SECONDS."""
    cutoff = now - COMBO_CURSOR_TTL_SECONDS
    kept: dict[str, ComboCursor] = {}
    dropped = 0
    for name, c in cursors.items():
        if c.last_rotated_at < cutoff:
            dropped += 1
            continue
        kept[name] = c
    return kept, dropped


def _parse_state_payload(raw: dict[str, Any]) -> dict[str, ProviderHealth]:
    """Best-effort parse of the on-disk payload into ProviderHealth entries.

    Drops entries whose shape we don't recognize (logs warning); never
    raises so that a partially-corrupt file degrades to "empty state".
    """
    health_map: dict[str, ProviderHealth] = {}
    block = raw.get("provider_health") or {}
    if not isinstance(block, dict):
        return health_map
    for pid, entry in block.items():
        if not isinstance(entry, dict):
            logger.warning(
                "runtime_state: dropping malformed entry for %r (not a dict)",
                pid,
            )
            continue
        try:
            # Clamp backoff_level to the valid range. A hand-edited or
            # legacy file with an out-of-range integer is repaired in
            # memory rather than dropped; the next write rewrites disk
            # with the clamped value.
            raw_level = int(entry.get("backoff_level", 0))
            clamped_level = max(0, min(raw_level, MAX_BACKOFF_LEVEL))
            if raw_level != clamped_level:
                logger.warning(
                    "runtime_state: clamped backoff_level %d -> %d for %r",
                    raw_level, clamped_level, pid,
                )
            raw_locks = entry.get("model_locks")
            model_locks: dict[str, float] = {}
            if raw_locks is not None and not isinstance(raw_locks, dict):
                # Falsy non-dict (e.g. JSON `[]`) and truthy non-dict
                # (e.g. JSON `[1, 2]`) both reach this branch. Earlier
                # `or {}` short-circuit silently rewrote falsy non-dict
                # to `{}` without logging, so this branch is the single
                # warning site for any non-dict on-disk shape.
                logger.warning(
                    "runtime_state: model_locks on %r is not a dict "
                    "(type=%s); treating as empty",
                    pid, type(raw_locks).__name__,
                )
                raw_locks = {}
            elif raw_locks is None:
                raw_locks = {}
            for m_id, m_ts in raw_locks.items():
                if not isinstance(m_id, str) or not m_id:
                    logger.warning(
                        "runtime_state: dropping model_lock with bad "
                        "key %r on provider %r",
                        m_id, pid,
                    )
                    continue
                try:
                    ts = float(m_ts)
                except (TypeError, ValueError):
                    logger.warning(
                        "runtime_state: dropping model_lock %r=%r on "
                        "provider %r: not numeric",
                        m_id, m_ts, pid,
                    )
                    continue
                if ts <= 0:
                    logger.warning(
                        "runtime_state: dropping model_lock %r on "
                        "provider %r: non-positive timestamp %r",
                        m_id, pid, ts,
                    )
                    continue
                model_locks[m_id] = ts
            health_map[pid] = ProviderHealth(
                provider_id=str(pid),
                backoff_level=clamped_level,
                rate_limited_until=(
                    float(entry["rate_limited_until"])
                    if entry.get("rate_limited_until") is not None
                    else None
                ),
                last_error_at=(
                    float(entry["last_error_at"])
                    if entry.get("last_error_at") is not None
                    else None
                ),
                model_locks=model_locks,
            )
        except (TypeError, ValueError) as exc:
            logger.warning(
                "runtime_state: dropping malformed entry for %r: %s", pid, exc
            )
    return health_map


def _read_disk_payload(state_path: Path) -> dict[str, Any] | None:
    """Read raw JSON dict from disk, or None if absent / unreadable.

    Empty file and JSON-decode error both return None (treated as
    empty state by callers). The corrupt file is preserved so a
    post-mortem can inspect it - the Failure Modes block locks this.
    """
    if not state_path.is_file():
        return None
    try:
        text = state_path.read_text(encoding="utf-8")
    except OSError as exc:
        logger.warning("runtime_state: read failed at %s: %s", state_path, exc)
        return None
    if not text.strip():
        return None
    try:
        raw = json.loads(text)
    except json.JSONDecodeError as exc:
        logger.warning(
            "runtime_state: JSON parse failed at %s: %s. Treating as empty.",
            state_path,
            exc,
        )
        return None
    if not isinstance(raw, dict):
        logger.warning(
            "runtime_state: top-level payload is not a dict at %s; treating as empty.",
            state_path,
        )
        return None
    return raw


def _drop_stale(
    health: dict[str, ProviderHealth], now: float
) -> tuple[dict[str, ProviderHealth], int]:
    """Return (kept_entries, dropped_count).

    The health TTL bounds a reactive observation independently of the lock
    deadline. A harvested reset can describe a long vendor window, but an old
    lock alone cannot keep an account classified forever when no fresh usage
    observation confirms it.
    """
    cutoff = now - PROVIDER_HEALTH_TTL_SECONDS
    kept: dict[str, ProviderHealth] = {}
    dropped = 0
    for pid, h in health.items():
        # The TTL is record-level on purpose: when last_error_at ages out,
        # every model_locks entry and the provider-level lock go with it.
        if h.last_error_at is not None and h.last_error_at < cutoff:
            dropped += 1
            continue
        kept[pid] = h
    return kept, dropped


def read_state(now: float | None = None) -> ProviderRuntimeState:
    """Load runtime state from disk, dropping stale entries in memory.

    Returns an empty state when the file is missing, empty, or
    malformed. Stale entries (``last_error_at`` older than
    ``PROVIDER_HEALTH_TTL_SECONDS``) are filtered from the returned
    state but NOT rewritten to disk - that would be a lock-free write
    that could clobber a concurrent write. Disk cleanup happens in the
    Rust writer instead, which holds the exclusive lock.

    This means a long-quiet provider's stale entry can persist on disk
    until the next write touches the file. That is acceptable: the
    returned in-memory state never exposes the stale entry to callers,
    and the file is bounded by the active provider count.
    """
    if now is None:
        now = time.time()
    state_path = _resolve_state_path()
    raw = _read_disk_payload(state_path)
    if raw is None:
        return ProviderRuntimeState(provider_health={})

    health_map = _parse_state_payload(raw)
    kept, _dropped = _drop_stale(health_map, now)
    cursors = _parse_cursors_payload(raw)
    cursors_kept, _ = _drop_stale_cursors(cursors, now)
    usage = _parse_usage_payload(raw)
    schema_version = int(raw.get("schema_version", SCHEMA_VERSION))
    return ProviderRuntimeState(
        provider_health=kept,
        combo_cursors=cursors_kept,
        usage=usage,
        windows_opened=_parse_windows_opened(raw),
        schema_version=schema_version,
    )


def update_provider_health(
    provider_id: str,
    rule: ErrorRule,
    model: str | None = None,
    now: float | None = None,
    resets_at: float | None = None,
) -> ProviderHealth:
    """Atomically increment backoff state for ``provider_id``.

    Returns the freshly written ``ProviderHealth``. On lock contention past
    five seconds, or when the writer cannot run, the write is skipped and the
    last-known-good entry (or a zero state) comes back.

    When ``model`` is provided the cooldown lands in ``model_locks[model]`` and
    ``rate_limited_until`` is untouched; ``backoff_level`` still ramps per
    provider. ``resets_at``, the epoch harvested from the refusal body, becomes
    ``rate_limited_until`` when it lies in the future, so the lock holds for the
    real window. It also stamps the window it closed when the record has a
    ``window_minutes``.
    """
    from fno.rust_binary import CommandUnavailable

    try:
        return ProviderHealth(**_state_write("update-health", {
            "provider_id": provider_id,
            "backoff": rule.backoff,
            "cooldown_ms": rule.cooldown_ms,
            "model": model,
            "now": now,
            "resets_at": resets_at,
        }))
    except CommandUnavailable as exc:
        # The recovery sweep calls this with no guard of its own, so a writer
        # that cannot run degrades like lock contention instead of raising.
        logger.warning("runtime_state: health write skipped for %r: %s", provider_id, exc)
        raw = _read_disk_payload(_resolve_state_path()) or {}
        return _parse_state_payload(raw).get(provider_id, ProviderHealth(provider_id=provider_id))


def is_in_cooldown(
    provider_id: str,
    model: str | None = None,
    now: float | None = None,
) -> bool:
    """Lock-free read: is ``provider_id`` currently in cooldown?

    Two-level lookup (Plan A1):

    1. If ``model`` is provided and ``model_locks[model] > now``, True.
    2. If ``rate_limited_until > now``, True (provider-level lock).
    3. Otherwise False.

    The model-specific check fires first so a provider-wide ``rate_limited_until``
    that happens to also cover the queried model still returns True
    via the provider-level branch even if no entry exists for that
    model. A model-specific lock is a strictly finer signal: when set
    it blocks queries for that exact model and any sibling model on
    the same record is only affected if the provider-level lock is
    also set.

    Eventual consistency is acceptable here: a stale "yes" makes us
    skip a provider that may have recovered (cheap), and a stale "no"
    makes us hit a provider that's still cooling off (the next call
    will re-classify and update state).
    """
    if now is None:
        now = time.time()
    state_path = _resolve_state_path()
    raw = _read_disk_payload(state_path)
    if raw is None:
        return False
    health_map, _dropped = _drop_stale(_parse_state_payload(raw), now)
    health = health_map.get(provider_id)
    if health is None:
        return False
    if model is not None:
        until_ts = health.model_locks.get(model)
        if until_ts is not None and until_ts > now:
            return True
    if health.rate_limited_until is not None and health.rate_limited_until > now:
        return True
    return False


# ---------------------------------------------------------------------------
# Usage snapshot: read (lock-free, TTL) + write (locked) + refresh (probe).
#
# Quota-aware dispatch. The snapshot is advisory-only: absence or
# staleness reads as "no data" and the caller proceeds fail-open. Writes carry
# health + cursors through the same lock so a usage write never drops a
# concurrent health/cursor mutation (and vice versa) - the same last-writer-wins
# invariant the health side already relies on.
# ---------------------------------------------------------------------------


def read_usage(
    provider_id: str,
    ttl_seconds: float = DEFAULT_USAGE_TTL_SECONDS,
    now: float | None = None,
    repo_root: Path | None = None,
) -> UsageSnapshot | None:
    """Lock-free read of the cached usage snapshot for ``provider_id``.

    Returns None when no snapshot exists or the snapshot is older than
    ``ttl_seconds`` (treated as absent, bounding probe frequency to one call
    per provider per TTL across all consumers). Eventual consistency is fine:
    the snapshot only makes dispatch smarter, never gates it.
    """
    if now is None:
        now = time.time()
    state_path = _resolve_state_path(repo_root)
    raw = _read_disk_payload(state_path)
    if raw is None:
        return None
    usage = _parse_usage_payload(raw)
    snap = usage.get(provider_id)
    if snap is None:
        return None
    if snap.probed_at < now - ttl_seconds:
        return None
    return snap


def write_usage_snapshot(
    snapshot: UsageSnapshot,
    now: float | None = None,
    repo_root: Path | None = None,
) -> bool:
    """Persist ``snapshot`` under ``usage[provider_id]``; True if it landed.

    On lock timeout, or when the writer could not run, the write is skipped
    (last-known-good stays on disk) - a stale snapshot degrades to UNKNOWN on
    the next read, never an error.

    The bool exists so a caller can report persistence degradation WITHOUT
    discarding the snapshot it already holds in memory: losing the write race
    is a cache outcome, not a probe outcome, and collapsing the two is how a
    successful live probe turns into a displayed ``unknown``.
    """
    from fno.rust_binary import CommandUnavailable

    try:
        out = _state_write("write-usage", {
            "provider_id": snapshot.provider_id,
            "snapshot": dataclasses.asdict(snapshot),
            "now": now,
            "repo_root": None if repo_root is None else str(repo_root),
        })
    except CommandUnavailable as exc:
        logger.warning(
            "runtime_state: usage write skipped for %r (snapshot degrades to "
            "UNKNOWN until the next successful probe): %s",
            snapshot.provider_id,
            exc,
        )
        return False
    return bool(out.get("persisted"))


# ---------------------------------------------------------------------------
# Combo cursor: read (lock-free, eventual consistency).
#
# Plan B (Spec 4). Lazy cleanup of stale entries happens ONLY in the locked
# Rust writer. read_cursor never writes; it returns None when the entry is
# stale, hash-mismatched, or absent.
# ---------------------------------------------------------------------------


def read_cursor(
    combo_name: str,
    providers_hash: str,
    now: float | None = None,
) -> ComboCursor | None:
    """Lock-free read: return the cursor for ``combo_name`` if valid.

    Returns None when:
      * No on-disk entry exists for this combo,
      * The stored ``providers_hash`` mismatches the input (combo edited),
      * The stored entry is stale (>``COMBO_CURSOR_TTL_SECONDS``),
      * The on-disk entry is malformed (missing fields, bad types).

    Eventual consistency is acceptable here: the cursor is a hint for
    rotation order.
    """
    if now is None:
        now = time.time()
    state_path = _resolve_state_path()
    raw = _read_disk_payload(state_path)
    if raw is None:
        return None
    cursors = _parse_cursors_payload(raw)
    cursor = cursors.get(combo_name)
    if cursor is None:
        return None
    if cursor.providers_hash != providers_hash:
        return None
    if cursor.last_rotated_at < now - COMBO_CURSOR_TTL_SECONDS:
        return None
    return cursor
