"""Door-shaped spawn provenance from the Python side.

The Rust side pins the same dict shapes deserialize into ``SpawnProvenance``
(``spawn_contract::tests::python_shaped_provenance_round_trips``); these
tests pin that the Python builder emits exactly those shapes and enforces
the door's rules.
"""

from __future__ import annotations

import json

import pytest

from fno.agents.spawn_lineage import build_spawn_provenance


def _with_env(monkeypatch, markers: dict[str, str]) -> None:
    monkeypatch.setenv("PWD", "/repo")
    for name in (
        "CODEX_THREAD_ID",
        "CODEX_SESSION_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
        "GEMINI_SESSION_ID",
        "OPENCODE_SESSION_ID",
        "FNO_HARNESS_NAME",
        "FNO_HARNESS_SESSION_ID",
    ):
        monkeypatch.delenv(name, raising=False)
    for name, value in markers.items():
        monkeypatch.setenv(name, value)


def test_session_markers_build_a_door_shaped_record(monkeypatch):
    _with_env(monkeypatch, {"CLAUDE_CODE_SESSION_ID": "0f0e7865-86b8-4a9e-8d99-6bd94b0ea9c9"})
    provenance = build_spawn_provenance()
    assert provenance is not None
    origin = provenance["origin"]
    owner = provenance["owner"]
    assert origin["kind"] == "session"
    assert origin["parent"]["session_id"] == "0f0e7865-86b8-4a9e-8d99-6bd94b0ea9c9"
    assert origin["parent"]["harness"] == "claude"
    assert origin["parent"]["cwd"] == "/repo"
    assert owner["kind"] == "session"
    assert owner["session_id"] == origin["parent"]["session_id"]
    # JSON round-trip keeps the shape a Rust reader deserializes.
    reparsed = json.loads(json.dumps(provenance))
    assert reparsed == provenance


def test_explicit_daemon_origin_requires_mission_or_role_owner(monkeypatch):
    monkeypatch.setattr(
        "fno.agents.naming.dispatch_sources",
        lambda: frozenset({"ab", "ac", "rd", "th", "pw", "jn"}),
    )
    origin = {
        "kind": "non_session",
        "source": {
            "kind": "daemon",
            "exe": "fno-agents-daemon",
            "arm": "active-backlog",
            "cause": "ab",
        },
    }
    with pytest.raises(ValueError, match="mission or role owner"):
        build_spawn_provenance(
            explicit_origin=origin,
            explicit_owner={"kind": "operator", "tty": "/dev/ttys001"},
        )
    owner = {"kind": "mission", "project": "fno", "mission": "epic-x"}
    provenance = build_spawn_provenance(explicit_origin=origin, explicit_owner=owner)
    assert provenance["origin"]["source"]["cause"] == "ab"
    assert provenance["owner"]["kind"] == "mission"


def test_carrier_env_outranks_ambient(monkeypatch):
    _with_env(monkeypatch, {"CLAUDE_CODE_SESSION_ID": "0f0e7865-86b8-4a9e-8d99-6bd94b0ea9c9"})
    monkeypatch.setenv(
        "FNO_SPAWN_ORIGIN",
        json.dumps(
            {
                "kind": "non_session",
                "source": {
                    "kind": "daemon",
                    "exe": "fno-agents-daemon",
                    "arm": "active-backlog",
                    "cause": "ab",
                },
            }
        ),
    )
    monkeypatch.setenv(
        "FNO_SPAWN_OWNER",
        json.dumps({"kind": "role", "project": "fno", "scope": "epic-x"}),
    )
    provenance = build_spawn_provenance()
    assert provenance is not None
    assert provenance["origin"]["kind"] == "non_session"
    assert provenance["origin"]["source"]["arm"] == "active-backlog"
    assert provenance["owner"] == {"kind": "role", "project": "fno", "scope": "epic-x"}


def test_half_carrier_refuses(monkeypatch):
    _with_env(monkeypatch, {})
    monkeypatch.setenv(
        "FNO_SPAWN_ORIGIN",
        json.dumps({"kind": "non_session", "source": {"kind": "daemon", "exe": "d", "arm": "a", "cause": "ab"}}),
    )
    monkeypatch.delenv("FNO_SPAWN_OWNER", raising=False)
    with pytest.raises(ValueError, match="together"):
        build_spawn_provenance()




def _identity_stub(monkeypatch, markers, session_id=None, harness=None):
    from types import SimpleNamespace

    from fno.claims import self_identity as self_identity_mod

    stub = SimpleNamespace(
        session_id=session_id,
        harness=harness,
        markers_present=markers,
        disposition="ambiguous" if session_id is None else "single",
    )
    monkeypatch.setattr(
        self_identity_mod, "resolve_self_identity", lambda *a, **k: stub
    )
    return stub


def test_capture_parent_edge_vouches_hosted_session_from_live_registry_row(
    monkeypatch,
):
    """A marker naming a live registry row of the same harness vouches the
    parent edge for a hosted session whose shells have no harness ancestor
    for the prover to walk."""
    from types import SimpleNamespace

    from fno.agents import registry as registry_mod
    from fno.agents import whoami as whoami_mod
    from fno.agents.spawn_lineage import _capture_parent_edge

    monkeypatch.setenv("PWD", "/repo")
    _identity_stub(
        monkeypatch,
        [("CLAUDE_CODE_SESSION_ID", "claude", "5eb990ff-0000")],
    )
    monkeypatch.setattr(
        registry_mod, "load_registry", lambda: [SimpleNamespace(status="live")]
    )
    monkeypatch.setattr(
        whoami_mod,
        "_find_by_session",
        lambda registry, token, harness: registry[0],
    )
    session, harness, cwd = _capture_parent_edge()
    assert session == "5eb990ff-0000"
    assert harness == "claude"
    assert cwd == "/repo"


def test_capture_parent_edge_keeps_refusal_without_live_row(monkeypatch):
    """No live row, no voucher: the capture stays None and the spawn keeps
    the dispatcher path."""
    from fno.agents import registry as registry_mod
    from fno.agents import whoami as whoami_mod
    from fno.agents.spawn_lineage import _capture_parent_edge

    monkeypatch.setenv("PWD", "/repo")
    _identity_stub(
        monkeypatch,
        [("CLAUDE_CODE_SESSION_ID", "claude", "dead0000-0000")],
    )
    monkeypatch.setattr(registry_mod, "load_registry", lambda: [])
    monkeypatch.setattr(whoami_mod, "_find_by_session", lambda *a, **k: None)
    session, harness, cwd = _capture_parent_edge()
    assert session is None
    assert cwd == "/repo"
