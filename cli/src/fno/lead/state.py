"""Scope-keyed lead manifests and the freshness read.

One manifest per role scope at ``<space>/leads/<scope>.md``, separate from
any target manifest: one file naming both drivers is how two sessions share
a discriminator. The manifest is the durable role record and a registry
row its cache, so a leftover file is never inert: it is the record heal
restores a lost row from. ``last_run_is_fresh`` reads the events journal,
not file mtimes.
"""

from __future__ import annotations

import fcntl
import json
import re
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

#: Respawns allowed before the walk refuses another lead session.
DEFAULT_RESPAWN_CEILING = 4

#: Both lead arms end a walk: the walk arm emits ``loop_terminated``, the
#: in-session stop arm ``termination``.
_TERMINAL_TYPES = frozenset({"loop_terminated", "termination"})

_WINDOW = re.compile(r"^(\d+)([smhd]?)$")
_UNIT_S = {"s": 1, "m": 60, "h": 3600, "d": 86400, "": 1}


def lead_loop_enabled() -> bool:
    """Resolve ``config.lead.enabled``, fail-safe to OFF like every gate here."""
    try:
        from fno.config import load_settings

        return bool(load_settings().lead.enabled)
    except Exception:  # noqa: BLE001
        return False


class LeadManifestExists(RuntimeError):
    """Raised when init would overwrite a manifest that is already there."""


def lead_state_root(cwd: Path | None = None) -> Path:
    """The canonical-keyed coordination root, so a role never lands in a
    disposable linked worktree however ``cwd`` is spelled."""
    from fno.paths import space_dir

    return space_dir(cwd)


def lead_manifest_path(scope: str, *, state_root: Optional[Path] = None) -> Path:
    """The manifest path for one scope. Path syntax refuses (never
    normalizes): two spellings must never select two files."""
    scope = scope.strip()
    if not scope or ".." in scope or "/" in scope or "\\" in scope or "\0" in scope:
        raise ValueError(f"unsafe lead scope for manifest path: {scope!r}")
    root = state_root if state_root is not None else lead_state_root()
    return Path(root) / "leads" / f"{scope}.md"


def arm_lead_manifest(
    scope: str,
    harness_session_id: str,
    *,
    state_root: Optional[Path] = None,
    owner_cwd: Optional[str] = None,
    role_level: Optional[int] = None,
    role_scope: Optional[str] = None,
    role_grantor: Optional[str] = None,
    model: Optional[str] = None,
    harness: Optional[str] = None,
    row: Any = None,
) -> Optional[Path]:
    """Refresh loop state at the moment a role becomes authoritative. The
    Rust creator (team_spawn.rs op ``arm``) checks the id and writes."""
    if row is not None:
        owner_cwd = owner_cwd or getattr(row, "cwd", None)
        role_level = role_level if role_level is not None else getattr(row, "role_level", None)
        role_scope = role_scope if role_scope is not None else getattr(row, "role_scope", None)
        role_grantor = (
            role_grantor if role_grantor is not None else getattr(row, "role_grantor", None)
        )
        # The promoted row's own request is the pin the wake must repeat; a
        # manifest without it wakes the lead on the account default model.
        model = model or getattr(row, "requested_model", None)
        # The holder's own harness, never the arming caller's: a claude lead
        # promoting a codex successor must not write `harness: claude` beside a
        # codex session id, or every transcript reader resolves blind.
        harness = harness or getattr(row, "harness", None)
    if state_root is None:
        state_root = _owner_state_root(owner_cwd)
    if not lead_loop_enabled():
        remove_lead_manifest(scope, state_root=state_root)
        return None
    answer = _manifest_call({
        "op": "arm", "scope": scope, "harness_session_id": harness_session_id,
        "state_root": str(state_root), "lead_enabled": True, "owner_cwd": owner_cwd,
        "role_level": role_level, "role_scope": role_scope, "role_grantor": role_grantor,
        "model": model, "harness": harness,
    })
    return Path(answer["path"])


def _manifest_call(payload: dict[str, Any]) -> dict[str, Any]:
    """One round-trip to the Rust manifest creator. Every refusal, a missing
    binary included, raises ValueError, which the arming callers degrade on."""
    from fno.agents.spawn_overlay_client import SpawnOverlayUnavailable, spawn_overlay_call

    try:
        answer = spawn_overlay_call({"kind": "spawn-team", **payload})
    except SpawnOverlayUnavailable as exc:
        raise ValueError(f"lead manifest: {exc}") from exc
    if answer.get("error"):
        raise ValueError(answer["error"])
    return answer


def _owner_state_root(owner_cwd: Optional[str]) -> Path:
    return lead_state_root(Path(owner_cwd) if owner_cwd else None)


@contextmanager
def _manifest_lock(path: Path):
    lock = path.with_suffix(path.suffix + ".lock")
    lock.parent.mkdir(parents=True, exist_ok=True)
    with lock.open("a+") as handle:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        try:
            yield
        finally:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def remove_lead_manifest(
    scope: str,
    *,
    state_root: Optional[Path] = None,
    owner_cwd: Optional[str] = None,
    expected_harness_session_id: Optional[str] = None,
) -> bool:
    """Best-effort cleanup; live registry authority never depends on this."""
    try:
        if state_root is None:
            state_root = _owner_state_root(owner_cwd)
        path = lead_manifest_path(scope, state_root=state_root)
        with _manifest_lock(path):
            if expected_harness_session_id:
                current = parse_manifest(path).get("harness_session_id")
                if current != expected_harness_session_id:
                    return False
            path.unlink(missing_ok=True)
        return True
    except (OSError, ValueError):
        return False


def write_manifest(
    path: Path,
    *,
    scope: str,
    harness_session_id: str = "",
    max_iterations: int = 40,
    respawn_ceiling: int = DEFAULT_RESPAWN_CEILING,
    force: bool = False,
    owner_cwd: Optional[str] = None,
    shape: str = "pass",
    role_level: Optional[int] = None,
    role_scope: Optional[str] = None,
    role_grantor: Optional[str] = None,
    model: Optional[str] = None,
    harness: Optional[str] = None,
) -> dict[str, str]:
    """Write the manifest once through the Rust creator (team_spawn.rs op
    ``manifest``); raises LeadManifestExists if it is there."""
    answer = _manifest_call({
        "op": "manifest", "path": str(path), "scope": scope, "force": force,
        "harness_session_id": harness_session_id, "max_iterations": max_iterations,
        "respawn_ceiling": respawn_ceiling, "owner_cwd": owner_cwd, "shape": shape,
        "role_level": role_level, "role_scope": role_scope, "role_grantor": role_grantor,
        "model": model, "harness": harness,
    })
    if answer.get("exists"):
        raise LeadManifestExists(answer["exists"])
    return dict(answer["fields"])


def parse_manifest(path: Path) -> dict[str, str]:
    """Read the frontmatter fields. An unreadable manifest reads as absent."""
    try:
        text = Path(path).read_text(encoding="utf-8")
    except OSError:
        return {}
    out: dict[str, str] = {}
    for line in text.splitlines():
        if line.strip() == "---":
            continue
        if ":" not in line:
            continue
        key, _, raw = line.partition(":")
        raw = raw.strip()
        if raw.startswith('"'):
            try:
                raw = json.loads(raw)
            except json.JSONDecodeError:
                raw = raw.strip('"')
        out[key.strip()] = raw
    return out


def manifest_role_for_session(
    session_id: str, *, owner_cwd: Optional[str] = None
) -> Optional[dict[str, str]]:
    """The lead manifest naming ``session_id`` as holder, else ``None``: the
    durable record a register re-stamps from when a restore stripped the row."""
    try:
        files = sorted((_owner_state_root(owner_cwd) / "leads").glob("*.md"))
    except OSError:
        return None
    for path in files:
        manifest = parse_manifest(path)
        if manifest.get("harness_session_id", "").strip() == session_id:
            return manifest | {"role_scope": path.stem}
    return None


def _manifest_int(manifest: dict[str, str], key: str, default: int) -> int:
    raw = manifest.get(key)
    if not raw:
        return default
    try:
        return int(raw)
    except (TypeError, ValueError):
        return default


def respawn_ceiling(path: Path) -> int:
    """Read the manifest ceiling with the Rust walk's default semantics."""
    return _manifest_int(parse_manifest(path), "respawn_ceiling", DEFAULT_RESPAWN_CEILING)


def at_respawn_ceiling(path: Path) -> bool:
    """Whether the respawn budget is spent; the Rust walk is the authority."""
    manifest = parse_manifest(path)
    ceiling = _manifest_int(manifest, "respawn_ceiling", DEFAULT_RESPAWN_CEILING)
    return ceiling > 0 and _manifest_int(manifest, "respawn_count", 0) >= ceiling


@dataclass
class TermState:
    """One read of who terms, over what, and is it live. Unknowns answer
    None with ``unknown_reason``, never a clean False."""

    promoted: Optional[bool] = None
    scope: Optional[str] = None
    shape: Optional[str] = None
    manifest_session: Optional[str] = None
    manifest_path: Optional[str] = None
    role_on_manifest: Optional[bool] = None
    registry_session: Optional[str] = None
    live: Optional[bool] = None
    split: Optional[bool] = None
    unknown_reason: Optional[str] = None


def _unknown_state(scope: Optional[str], reason: str) -> TermState:
    return TermState(scope=scope, unknown_reason=reason)


def term_state(
    scope: Optional[str] = None,
    session_id: Optional[str] = None,
    *,
    state_root: Optional[Path] = None,
) -> TermState:
    """Ask the Rust term reader (``fno-agents lead-state``) who terms;
    Python never derives its own answer. Failures answer unknown, named.
    """
    import subprocess

    from fno import paths
    from fno.rust_binary import resolve_binary

    harness = None
    if scope is None and not session_id:
        from fno.agents.self_stamp import resolve_self_identity

        ident = resolve_self_identity()
        session_id = ident.session_id or ""
        harness = ident.harness or None

    root = state_root if state_root is not None else _owner_state_root(None)
    # The Rust reader joins <root>/leads itself; a suffixed root is a phantom path.
    binary = resolve_binary()
    if binary is None:
        return _unknown_state(
            scope,
            "fno-agents binary not found: the term reader lives in Rust. "
            "Reinstall fno, run `fno doctor update --rust`, or set FNO_AGENTS_BIN.",
        )
    argv = [str(binary), "lead-state", "--root", str(root)]
    # Alias map resolves from the checkout, not the space dir (no config.toml
    # there); without this a repo-declared short_name misses its own lead.
    argv += ["--cwd", str(Path.cwd())]
    if scope:
        argv += ["--scope", scope]
    if session_id:
        argv += ["--session", session_id]
    if harness:
        argv += ["--harness", harness]
    try:
        argv += ["--registry", str(paths.agents_registry_path())]
        proc = subprocess.run(argv, capture_output=True, text=True, check=False, timeout=30)
    except (OSError, subprocess.SubprocessError) as exc:
        return _unknown_state(scope, f"term reader failed to run: {exc}")
    if proc.returncode != 0:
        detail = proc.stderr.strip() or proc.stdout.strip()
        return _unknown_state(scope, f"term reader exited {proc.returncode}: {detail}")
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return _unknown_state(scope, f"term reader emitted no JSON: {exc}")
    keys = (
        "promoted",
        "shape",
        "manifest_session",
        "manifest_path",
        "role_on_manifest",
        "registry_session",
        "live",
        "split",
        "unknown_reason",
    )
    return TermState(
        **{k: payload.get(k) for k in keys},
        scope=payload.get("scope") or scope,
    )


def parse_window(window: str) -> int:
    """``24h`` / ``90m`` / ``7d`` / ``30s`` / a bare second count -> seconds."""
    match = _WINDOW.match(window.strip())
    if not match:
        raise ValueError(f"unparseable window {window!r}; use e.g. 24h, 90m, 7d")
    return int(match.group(1)) * _UNIT_S[match.group(2)]


def _parse_ts(raw: object) -> Optional[float]:
    if not isinstance(raw, str):
        return None
    try:
        return datetime.fromisoformat(raw.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def last_run_is_fresh(
    events_path: Path,
    *,
    since_s: int,
    now_iso: Optional[str] = None,
) -> bool:
    """True when a lead ``loop_terminated`` landed inside the window. Every
    failure answers False (no evidence); a corrupt line skips itself."""
    now = _parse_ts(now_iso) if now_iso else datetime.now(timezone.utc).timestamp()
    if now is None:
        return False
    # The store commit is the write boundary: committed rows carry the
    # history, raw bytes are only the pre-store fallback.
    from fno.events.store_client import native_rows

    lines = native_rows(Path(events_path), types=list(_TERMINAL_TYPES))
    if lines is None:
        try:
            text = Path(events_path).read_text(encoding="utf-8")
        except OSError:
            return False
        lines = text.splitlines()
    newest: Optional[float] = None
    for line in lines:
        line = line.strip()
        if not line or not any(t in line for t in _TERMINAL_TYPES):
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        if event.get("type") not in _TERMINAL_TYPES:
            continue
        if (event.get("data") or {}).get("driver") != "lead":
            continue
        ts = _parse_ts(event.get("ts"))
        if ts is not None and (newest is None or ts > newest):
            newest = ts
    if newest is None:
        return False
    return (now - newest) <= since_s
