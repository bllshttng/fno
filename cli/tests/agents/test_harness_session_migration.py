"""Registry + routing migration to harness_session_id (x-ec59).

Covers the shared sync rule, the Python/Rust round-trip via the registry loader,
and harness-scoped whoami. The reconcile
heal (AC1-FR / AC1-UI) lives with the other reconcile tests in
test_dispatch_lifecycle.py.
"""
import json
from pathlib import Path

from fno.agents.registry import (
    REGISTRY_LEGACY_SESSION_KEYS,
    SCHEMA_VERSION,
    AgentEntry,
    load_registry,
)
from fno.harness_identity import sync_harness_aliases

M = REGISTRY_LEGACY_SESSION_KEYS


# ---------------------------------------------------------------------------
# sync_harness_aliases — the one shared rule
# ---------------------------------------------------------------------------


def test_sync_canonical_wins_on_conflict():
    """AC2-EDGE: a set canonical id overwrites a conflicting legacy value."""
    d = {"harness": "claude", "harness_session_id": "CANON", "claude_session_uuid": "STALE"}
    sync_harness_aliases(d, M)
    assert d["claude_session_uuid"] == "CANON"


# ---------------------------------------------------------------------------
# Registry round-trip (Python side of the cross-language contract)
# ---------------------------------------------------------------------------


def _write_registry_json(tmp: Path, rows: list[dict]) -> Path:
    p = tmp / "registry.json"
    p.write_text(json.dumps({"schema_version": SCHEMA_VERSION, "agents": rows}))
    return p


def _row(**over) -> dict:
    base = dict(
        name="w1",
        harness="claude",
        cwd="/x",
        log_path="/x/l",
        status="live",
        created_at="2026-01-01T00:00:00Z",
        host_mode="exec",
    )
    base.update(over)
    return base


def test_legacy_row_backfills_canonical(tmp_path):
    """AC1-EDGE: a pre-migration row (provider + claude_session_uuid) gains the
    canonical fields on load."""
    p = _write_registry_json(tmp_path, [_row(claude_session_uuid="U")])
    e = load_registry(p)[0]
    assert e.harness == "claude"
    assert e.harness_session_id == "U"


def test_alien_harness_row_loads_without_bricking(tmp_path):
    """An alien harness string must degrade to a plain row, never a load error
    (harness is identity-only, not validated against KNOWN_PROVIDERS)."""
    p = _write_registry_json(
        tmp_path, [_row(provider="claude", harness="opencode", harness_session_id="X")]
    )
    e = load_registry(p)[0]
    assert e.harness == "opencode"
    assert e.harness_session_id == "X"


# ---------------------------------------------------------------------------
# whoami on any harness (AC2-FR)
# ---------------------------------------------------------------------------


def test_whoami_does_not_cross_match_another_harness():
    """A codex id must NOT resolve a claude row that happens to share the id:
    session ids are provider-local, so matching is scoped to the row's harness."""
    from fno.agents import whoami

    claude_row = AgentEntry(
        name="cl",
        cwd="/x",
        log_path="/x/l",
        harness="claude",
        harness_session_id="SHARED",
    )
    res = whoami.resolve_self(
        env={}, registry=[claude_row], session_uuid="SHARED", harness="codex"
    )
    assert not res.registered
