"""Resolve a session token against its harness's OWN store and adopt store-only
sessions when the fno registry misses.

The registry is a cache of reality, not a gate in front of it. A session with no
roster row -- reaped after a terminal stop, or never spawn-created -- was
unreachable by every fno command even though the harness itself resumes it fine.
This module owns the store probes behind ``registry.resolve_agent``. Every short
session-shaped registry hit is checked against them for cross-source ambiguity;
on a registry miss, exactly one match is registered so the command proceeds AND the
session returns to the roster.

Three rules keep it from guessing:

- **Shape gate** -- only a session-shaped token (8-hex, UUID, ``ses_...``) is
  probed, so a plain unknown name still fails exactly as before.
- **Exactly one match** -- two stores (or two sessions in one store) matching
  refuses with the candidate list; git's ambiguous-short-SHA posture.
- **Never live** -- a store row proves the session EXISTS, never that it is
  running, so the adopted row is ``orphaned``. Store membership must not
  resurrect a dead session into lane caps or live anycast (the lesson).
- **Machine-wide, not project-confined** -- the probes scan machine-wide (a
  transcript store is global) and adoption resolves from any project; the
  retired ``--cross-project`` flag had authorized exactly that. The one guard
  left is uniqueness: more than one store hit refuses with the candidate list.
  ``resume`` does not adopt store hits (it
  matches loaded registry entries via ``resolve_agent_in``), so it is uncovered
  by design.
"""
from __future__ import annotations

import datetime
import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Optional

from fno.agents.fs_scan import path_exists_strict, scan_files
from fno.harness_identity import claude_transport_short_id, session_handle_tier

if TYPE_CHECKING:
    from fno.agents.registry import AgentEntry

# `fno agents rm` stamps removed sessions in rm_tombstones.json beside the
# registry; the healer refuses to re-adopt them inside this window.
RM_TOMBSTONE_FILENAME = "rm_tombstones.json"
RM_TOMBSTONE_GRACE_SECS = 86_400

# Session-shaped tokens only. Eight alphanumeric characters can be an OpenCode
# tail even when they look like a friendly name, so those tokens share the store
# ambiguity check. Names outside these shapes never pay for a store read.
_SHORT_RE = re.compile(r"^[A-Za-z0-9]{8}$")
_UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    re.IGNORECASE,
)
_OPENCODE_RE = re.compile(r"^ses_[A-Za-z0-9]+$")

# Transcript lines before the session's `cwd` is recorded (line 1 is a summary /
# meta record on current claude). Bounded so a probe never streams a 100 MB log.
_CWD_SCAN_LINES = 50


@dataclass(frozen=True)
class StoreHit:
    """One harness store's answer for a token: the session it names."""

    harness: str
    session_id: str
    cwd: str

    @property
    def short_id(self) -> str:
        """Claude's legacy first-eight transport key, not a mailbox address."""
        return claude_transport_short_id(self.session_id)


def _normalize(token: str) -> str:
    """Trim only; the shared identity owner applies harness-safe case rules."""
    return (token or "").strip()


def is_session_shaped(token: str) -> bool:
    """True for a token worth probing a harness store with."""
    t = _normalize(token)
    return bool(_SHORT_RE.match(t) or is_full_session_id(t))


def is_full_session_id(token: str) -> bool:
    """True when ``token`` is a complete, collision-free harness session id."""
    t = _normalize(token)
    return bool(_UUID_RE.match(t) or _OPENCODE_RE.match(t))


def _claude_projects_dir() -> Path:
    """Delegated, not re-derived: ``discover`` already owns the store roots and
    their ``FNO_*_DIR`` test overrides, so a probe here cannot drift from what
    discovery reads (and needs no placement-rule allowlist entry of its own)."""
    from fno.agents.discover import default_projects_dir

    return default_projects_dir()


def _codex_sessions_dir() -> Path:
    from fno.agents.discover import default_codex_sessions_dir

    return default_codex_sessions_dir()


def _transcript_cwd(path: Path) -> str:
    """The session's own recorded cwd, or "" when it never recorded one."""
    # errors="replace" so an invalid UTF-8 byte mid-transcript cannot raise
    # UnicodeDecodeError from the ITERATION itself, outside the per-line guard.
    # A mangled line simply fails to parse as JSON and is skipped.
    try:
        with open(path, encoding="utf-8", errors="replace") as fh:
            for _, line in zip(range(_CWD_SCAN_LINES), fh):
                try:
                    rec = json.loads(line)
                except ValueError:
                    continue
                cwd = rec.get("cwd") if isinstance(rec, dict) else None
                if isinstance(cwd, str) and cwd:
                    return cwd
    except (OSError, ValueError):
        return ""
    return ""


def _probe_claude(token: str) -> list[StoreHit]:
    """Claude's canonical transcript store -- the same one ``claude -r`` reads.

    The filename IS the session UUID, so an 8-hex short matches by prefix. The
    ``<pid>.json`` sidecar is deliberately not consulted: it covers only live bg
    supervisors, and the session this fallback exists for is precisely the one
    with no live supervisor left.
    """
    if _OPENCODE_RE.match(token):
        return []
    needle = token.lower()
    hits: dict[str, StoreHit] = {}
    root = _claude_projects_dir()
    found = scan_files(
        root,
        max_depth=1,
        include=lambda name: name.endswith(".jsonl") and needle in name.lower(),
    )
    for path in sorted(found):
        name = path.name
        if ".sync-conflict-" in name:
            continue
        sid = name[: -len(".jsonl")]
        if session_handle_tier(token, sid) is not None and sid not in hits:
            hits[sid] = StoreHit("claude", sid, _transcript_cwd(path))
    return list(hits.values())


def _probe_codex(token: str) -> list[StoreHit]:
    """Codex rollouts (``rollout-<ts>-<uuid>.jsonl``); cwd from ``session_meta``."""
    if _OPENCODE_RE.match(token):
        return []
    from fno.agents.discover import _codex_meta

    hits: dict[str, StoreHit] = {}
    root = _codex_sessions_dir()
    found = scan_files(
        root,
        include=lambda name: name.startswith("rollout-")
        and name.endswith(".jsonl"),
    )
    for path in sorted(found):
        meta = _codex_meta(path)
        if meta is None:
            raise OSError(f"unreadable codex session candidate: {path}")
        sid, cwd = meta
        if session_handle_tier(token, sid) is None:
            continue
        hits.setdefault(sid, StoreHit("codex", sid, cwd))
    return list(hits.values())


def _probe_opencode(token: str) -> list[StoreHit]:
    """opencode's SQLite store. Its ids are ``ses_``-prefixed, so a hex token
    never reaches here -- and a `ses_` token never reaches the other two."""
    if not (_OPENCODE_RE.match(token) or _SHORT_RE.match(token)):
        return []
    from fno.agents.discover import (
        _opencode_session_info,
        default_opencode_db_path,
        default_opencode_storage_dir,
        opencode_query,
    )

    db = default_opencode_db_path()
    if not path_exists_strict(db):
        paths = scan_files(
            default_opencode_storage_dir() / "session",
            max_depth=1,
            include=lambda name: name.endswith(".json") and token in name,
        )
        hits: list[StoreHit] = []
        for path in paths:
            info = _opencode_session_info(path)
            if info is None:
                raise OSError(f"unreadable opencode session candidate: {path}")
            sid, cwd = info
            if session_handle_tier(token, sid) is not None:
                hits.append(StoreHit("opencode", sid, cwd))
        return hits
    rows = opencode_query(
        db,
        "SELECT id, directory FROM session "
        "WHERE id = ? OR substr(id, -8) = ? OR substr(id, 1, 8) = ?",
        (token, token, token),
        raise_on_error=True,
    )
    return [
        StoreHit("opencode", sid, directory if isinstance(directory, str) else "")
        for sid, directory in rows
        if isinstance(sid, str) and sid and session_handle_tier(token, sid) is not None
    ]


_PROBES = (_probe_claude, _probe_codex, _probe_opencode)


def probe_stores(token: str, *, require_complete: bool = True) -> list[StoreHit]:
    """Every harness store's answer for ``token``.

    Callers that explicitly pass ``require_complete=False`` retain the
    historical partial-result behavior. Identity selection uses the strict
    default because a partial answer cannot prove a short token unique across
    the shared namespace.
    """
    token = _normalize(token)
    if not is_session_shaped(token):
        return []
    hits: list[StoreHit] = []
    failed: list[str] = []
    for probe in _PROBES:
        try:
            hits.extend(probe(token))
        except Exception:  # noqa: BLE001 - collect every readable store before refusing
            failed.append(probe.__name__.removeprefix("_probe_"))
            continue
    matched = [
        hit
        for hit in hits
        if session_handle_tier(token, hit.session_id) is not None
    ]
    distinct = list({(hit.harness, hit.session_id): hit for hit in matched}.values())
    if failed and require_complete:
        from fno.agents.registry import AgentResolutionError

        candidates = ""
        if distinct:
            candidates = " Visible candidates: " + ", ".join(
                f"{hit.session_id} ({hit.harness})"
                for hit in sorted(distinct, key=lambda hit: (hit.harness, hit.session_id))
            ) + "."
        raise AgentResolutionError(
            f"token {token!r} could not be checked for cross-store uniqueness "
            f"because these harness stores were unreadable: {', '.join(failed)}. "
            f"{candidates} Use the full session id.",
            ambiguous=True,
        )
    return distinct


def complete_store_hits(token: str) -> list[StoreHit]:
    """Return a complete cross-harness answer or refuse short-token selection."""
    return probe_stores(token, require_complete=True)


def _transcript_last_write(hit: "StoreHit") -> Optional[str]:
    """The adopted session's last transcript write, as a UTC stamp, or None.

    None means the store held no transcript for this session, and ONLY that. It
    is the reason the row's ``last_message_at`` was null for every non-claude
    adoption: nothing here used to look, and the receipt then reported the
    transcript unreadable, which is an absence read as an answer.

    ``resolve_transcript`` is the shared per-harness resolver (it handles codex
    and opencode as well as claude), so adopting a codex rollout does not need a
    second copy of codex's date-partitioned path shape.

    Only a per-session file (``kind == "jsonl"``) yields a stamp. opencode
    resolves to its SHARED SQLite store, whose mtime is the last write by ANY
    session; stamping that would be a plausible, near-always-wrong answer that
    nothing downstream could falsify. A null the receipt explains is better.

    The stamp is the newest TIMESTAMPED entry over the whole file, not the
    mtime : ``row_contradiction`` cross-checks it against truth's
    ``last_event_at``, so both must come from the same field; no timestamped
    entry falls back to the mtime, truth's own labelled fallback.
    """
    from fno.agents.session_truth import newest_entry_epoch
    from fno.provenance.resolver import resolve_transcript

    try:
        # Pass the SAME store roots the probes used. resolve_transcript's claude
        # arm otherwise falls back to its import-time default projects root, so
        # under FNO_CLAUDE_PROJECTS_DIR it reads a different tree than the one
        # that produced this hit and the stamp is null every time - including in
        # every test, where it is the one read that escapes the fixture roots.
        resolved = resolve_transcript(
            hit.harness,
            hit.session_id,
            hit.cwd,
            projects_root=_claude_projects_dir(),
            codex_sessions_dir=_codex_sessions_dir(),
        )
        if not resolved.resolved or not resolved.transcript_path:
            return None
        if resolved.kind != "jsonl":
            return None
        epoch = newest_entry_epoch(Path(resolved.transcript_path), tail_bytes=None)
        if epoch is None:
            epoch = Path(resolved.transcript_path).stat().st_mtime
    except Exception:  # noqa: BLE001 - a stamp is a nicety; adoption still lands
        return None
    return (
        datetime.datetime.fromtimestamp(epoch, tz=datetime.timezone.utc)
        .replace(microsecond=0)
        .isoformat()
        .replace("+00:00", "Z")
    )


def _recent_rm_tombstone(harness, session_id, registry_path) -> Optional[int]:
    """The session's in-window rm stamp (epoch secs), else None."""
    path = registry_path
    if path is None:
        from fno.paths import agents_registry_path
        path = agents_registry_path()
    try:
        raw = json.loads((path.parent / RM_TOMBSTONE_FILENAME).read_text(encoding="utf-8"))
    except (OSError, ValueError, TypeError):
        return None
    now = int(datetime.datetime.now().timestamp())
    stamps = [r.get("removed_at") for r in raw if isinstance(raw, list) and isinstance(r, dict)
              and r.get("harness") == harness and r.get("session_id") == session_id]
    return next((int(t) for t in stamps if isinstance(t, (int, float))
                 and now - t <= RM_TOMBSTONE_GRACE_SECS), None)


def adopt_store_hit(
    hit: StoreHit,
    registry_path: Optional[Path] = None,
    *,
    token: Optional[str] = None,
    log_path: str = "",
) -> "AgentEntry":
    """Register one store hit as an adopted orphan.

    ``log_path`` is the follow-up transcript path for the adopted row; a
    caller that adopts for a lane which will resume the thread by full
    session id passes it HERE so the row is complete at the moment it
    lands, not patched in a second write the caller must survive.

    This function does not scan harness stores. Registry write failures
    return a synthetic row so the caller can still
    address the real harness session.
    """
    from fno.agents.registry import (
        AgentResolutionError,
        mint_agent_entry,
        register_existing_session,
    )

    if _recent_rm_tombstone(hit.harness, hit.session_id, registry_path) is not None:
        raise AgentResolutionError(
            f"session {hit.session_id} ({hit.harness}) was removed by `fno agents rm` "
            f"inside the grace window ({RM_TOMBSTONE_GRACE_SECS // 3600}h); it is not "
            "re-adopted from the harness store. Re-spawn the work, or wait out the "
            "window to adopt it deliberately.",
            ambiguous=True,
        )
    # claude's transport key is the 8-hex jobId (`claude attach <jobId>`), NOT
    # the full UUID that HARNESS_SESSION_ID_FIELDS would otherwise write there.
    short_id = hit.short_id if hit.harness == "claude" else ""
    last_message_at = _transcript_last_write(hit)
    try:
        return register_existing_session(
            provider=hit.harness,
            session_id=hit.session_id,
            cwd=hit.cwd,
            short_id=short_id,
            status="orphaned",
            # This row is ADOPTED from the harness store, not a spawn receipt:
            # nothing here proves footnote started the session, and it is
            # routinely an operator's own terminal that no SessionStart hook
            # registered. `origin` carries that because it is durable - `status`
            # is a liveness stamp `reconcile` flips back to "live" the moment
            # the session answers a probe, so a reader keying on it gets one
            # pass of protection and then none. Lanes that stop a session read
            # this field to answer "footnote-spawned?" with unknown rather than
            # with the absence of an operator marker.
            origin="adopted",
            log_path=log_path,
            last_message_at=last_message_at,
            registry_path=registry_path,
        )
    except AgentResolutionError:
        raise
    except Exception as exc:  # noqa: BLE001 - reaching the session beats the roster row
        display_token = token if token is not None else hit.session_id
        sys.stderr.write(
            f"WARN: resolved {display_token!r} from the {hit.harness} store but "
            f"could not register it ({exc}); the row will appear on a later "
            "resolution.\n"
        )
        # The adopting session VOUCHED for this row (LD3); this
        # fallback copy skips register_session, stating the same split itself.
        from fno.agents.spawn_lineage import _capture_parent_edge

        _sb_session = _capture_parent_edge()[0]
        return mint_agent_entry(
            harness_session_id=hit.session_id,
            spawned_by_session=None,
            spawned_by_harness=None,
            spawned_by_cwd=None,
            lineage_reason=None,
            name=_fallback_name(hit.session_id),
            cwd=hit.cwd,
            log_path=log_path,
            harness=hit.harness,
            status="orphaned",
            short_id=short_id,
            adopted_by_session=_sb_session,
            # Same fact as the registered row above, and it has to be stated
            # here too: this one is handed straight back to the caller when
            # registration fails, so it reaches a reader without ever passing
            # the path that would have marked it.
            origin="adopted",
            # Adoption observed nothing about the lane; the substrate stays
            # unknown (never "pane").
            substrate=None,
        )


def heal_from_harness_store(
    token: str, *, registry_path: Optional[Path] = None,
) -> Optional["AgentEntry"]:
    """Adopt the session ``token`` names into the registry and return its row.

    ``None`` when the token is not session-shaped or no store knows it -- the
    caller then raises its own unchanged not-found error. Raises
    :class:`~fno.agents.registry.AgentResolutionError` naming the candidates when
    more than one session matches: an ambiguous token is refused, never guessed.

    Adoption resolves from any project (the retired ``--cross-project`` flag had
    authorized exactly that); uniqueness across stores is the one guard.

    Registration is best-effort. If the registry write fails, the synthesized row
    is still returned so the command reaches the session anyway -- reaching it wins,
    and the row appears on the next resolution.
    """
    from fno.agents.registry import AgentResolutionError

    hits = complete_store_hits(token)
    if not hits:
        return None
    if len(hits) > 1:
        cands = ", ".join(f"{h.session_id} ({h.harness})" for h in sorted(
            hits, key=lambda h: (h.harness, h.session_id)
        ))
        raise AgentResolutionError(
            f"token {token!r} matches {len(hits)} sessions across harness stores: "
            f"{cands}. Disambiguate with the full session id.",
            ambiguous=True,
        )

    return adopt_store_hit(hits[0], registry_path, token=token)


def _fallback_name(session_id: str) -> str:
    from fno.harness_identity import canonical_handle

    return canonical_handle(session_id)
