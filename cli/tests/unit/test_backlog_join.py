"""The roster-name rung in ``resolve_task_holder``.

A daemon-forked worker loses the ``FNO_WORKER_NAME`` env export; the
spawn-time registry row proves the same name for the session it bound.
"""

from __future__ import annotations

import json
from types import SimpleNamespace


def _registry(monkeypatch, tmp_path, session_id, agents):
    reg = tmp_path / "registry.json"
    reg.write_text(json.dumps({"schema_version": 2, "agents": agents}))
    monkeypatch.setattr("fno.paths.agents_registry_path", lambda: reg)
    monkeypatch.setattr(
        "fno.claims.self_identity.resolve_self_identity",
        lambda env, **_k: SimpleNamespace(
            session_id=session_id, harness="claude", disposition="proven"
        ),
    )


def test_task_holder_reads_roster_binding_when_env_missing(tmp_path, monkeypatch):
    from fno.claims.self_identity import resolve_task_holder

    sid = "721b0775-8b99-4f8f-a067-b9642e8ced8a"
    _registry(monkeypatch, tmp_path, sid, [
        {"name": "j-x-9734-2", "harness_session_id": sid, "status": "live"},
    ])
    holder, why = resolve_task_holder({})
    assert (holder, why) == ("j-x-9734-2", "")


def test_task_holder_ambiguous_registry_falls_back(tmp_path, monkeypatch):
    """Two rows bind one session: the name is unproven, so the session id holds."""
    from fno.claims.self_identity import resolve_task_holder

    sid = "aaaa0000-0000-0000-0000-000000000000"
    _registry(monkeypatch, tmp_path, sid, [
        {"name": "row-a", "harness_session_id": sid},
        {"name": "row-b", "harness_session_id": sid},
    ])
    holder, _why = resolve_task_holder({})
    assert holder == sid
