from __future__ import annotations

import json
import subprocess as _subprocess
from types import SimpleNamespace

import pytest

from fno.agents import naming
from fno.agents.registry import AgentEntry

# The mint is a real pre-spawn subprocess; fakes route it here.
_REAL_SUBPROCESS_RUN = _subprocess.run

_NAME_VERBS = frozenset({"name-mint", "name-codes", "name-parse"})


def _is_name_verb(command) -> bool:
    return bool(_NAME_VERBS & {str(p) for p in command})


@pytest.fixture(autouse=True)
def _cold_name_codes_cache():
    """A fake that answers name-codes with pane text must fail on every run.

    ``naming._codes`` is an lru_cache'd shellout, so a fake that skips the
    name-verb route passes only when an earlier test in the same xdist worker
    already warmed the cache. Clearing it around every test makes that fake
    fail deterministically; routing through ``_is_name_verb`` is the fix, this
    fixture is the detector. The routed fake still reaches the real binary,
    which is what the sibling fakes already pay.
    """
    naming._codes.cache_clear()
    yield
    naming._codes.cache_clear()


def _row(**overrides) -> AgentEntry:
    values = {
        "name": "bp-xbdb9-retask",
        "cwd": "/repo",
        "log_path": "",
        "harness": "codex",
        "provider": None,
        "model": "gpt-5.6-sol",
        "effort": "high",
        "harness_session_id": "old-session",
        "substrate": "pane",
        "mux": {"session": "main", "pane_id": 12},
    }
    values.update(overrides)
    return AgentEntry(**values)


def _config_env(monkeypatch, tmp_path, body):
    """The compose gathers its own config from disk; FNO_CONFIG is the sole
    candidate, so the profile a test supplies lives in this file."""
    cfg = tmp_path / "config.toml"
    cfg.write_text(body)
    monkeypatch.setenv("FNO_CONFIG", str(cfg))


@pytest.fixture(autouse=True)
def _graph_with_target_node(monkeypatch):
    """Verb resolution loads the node record; default it to a planless low
    node so the probe resolves the target verb. A test overrides this by
    monkeypatching read_nodes_by_ids again inside its own body."""
    monkeypatch.setattr(
        "fno.graph.store.read_nodes_by_ids",
        lambda path, tokens: {"entries": [{"id": "x-bdb9", "difficulty": "low"}], "missing": []},
    )


def test_retask_node_resolution_canonicalizes_slug_and_bare_hex(monkeypatch):
    import fno.agents.retask as retask

    monkeypatch.setattr(
        "fno.graph.load.load_graph",
        lambda: [{"id": "x-bdb9", "slug": "retask-destination"}],
    )
    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: None)
    monkeypatch.setattr("fno.graph._constants.node_id_prefix", lambda: "x-")

    assert retask._resolve_retask_node("retask-destination") == "x-bdb9"
    assert retask._resolve_retask_node("bdb9") == "x-bdb9"
    monkeypatch.setattr(
        "fno.graph.store.read_nodes_by_ids",
        lambda path, tokens: {"entries": [{"id": "x-old", "slug": "retask-destination", "archived_at": "2026-09-01"}], "missing": []},
    )
    assert retask._resolve_retask_node("retask-destination") == "x-bdb9"


def _patch_rename_call(monkeypatch, sink, receipt):
    """Intercept only the transaction hand-off; the spawn-defaults resolver
    makes its own verb_call rounds that must reach the real binary."""
    from fno.rust_binary import verb_call as real_verb_call

    def fake(verb, payload, unavailable, *, timeout=30):
        if verb == "rename":
            sink.append(payload)
            return receipt
        return real_verb_call(verb, payload, unavailable, timeout=timeout)

    monkeypatch.setattr("fno.rust_binary.verb_call", fake)


def test_run_retask_hands_the_transaction_to_fno_agents(tmp_path, monkeypatch):
    """The thin front resolves row, coordinate, target_command and the pane
    ref, then hands the whole transaction to the fno-agents rename payload."""
    import fno.agents.retask as retask

    _config_env(
        monkeypatch,
        tmp_path,
        '[agents.profiles.target]\nprovider = "codex"\n'
        'model = "gpt-5.6-sol"\neffort = "high"\n',
    )
    row = _row()
    monkeypatch.setattr(
        retask, "resolve_agent", lambda *_args, **_kwargs: SimpleNamespace(entry=row)
    )
    calls: list[dict] = []
    receipt = {"status": "retasked", "switch": "skipped_same_tier"}
    _patch_rename_call(monkeypatch, calls, receipt)

    got = retask.run_retask(
        "bp-xbdb9-retask",
        node="x-bdb9",
        env={},
    )

    assert got is receipt
    assert len(calls) == 1
    payload = calls[0]
    assert payload["op"] == "retask"
    assert payload["worker"] == "bp-xbdb9-retask"
    assert payload["node"] == "x-bdb9"
    assert payload["mux"] == {"session": "main", "pane_id": 12}
    # The profile's provider no longer picks the harness: the lane owns it,
    # so the coordinate rides the ambient harness.
    assert payload["target"]["harness"] == "claude"
    assert payload["target"]["verb"] == "target"
    assert payload["target_command"] == "/target --no-merge x-bdb9"


def test_run_retask_renders_a_non_target_verb_command(tmp_path, monkeypatch):
    import fno.agents.retask as retask

    monkeypatch.setattr(
        "fno.graph.store.read_nodes_by_ids",
        lambda path, tokens: {"entries": [{
            "id": "x-bdb9",
            "difficulty": "medium",
            "dispatch_verb": "/fno:blueprint",
        }], "missing": []},
    )
    _config_env(monkeypatch, tmp_path, '[agents.profiles.target]\nprovider = "codex"\n')
    row = _row()
    monkeypatch.setattr(
        retask, "resolve_agent", lambda *_args, **_kwargs: SimpleNamespace(entry=row)
    )
    payloads: list[dict] = []
    _patch_rename_call(monkeypatch, payloads, {"status": "retasked"})

    retask.run_retask(
        "bp-xbdb9-retask",
        node="x-bdb9",
        env={},
    )

    assert payloads[0]["target"]["verb"] == "blueprint"
    assert payloads[0]["target_command"] == "/blueprint x-bdb9"


def test_run_retask_converts_a_transport_failure_into_a_refused_receipt(monkeypatch):
    from fno.agents.retask import RetaskTransportError
    import fno.agents.retask as retask

    row = _row()
    monkeypatch.setattr(
        retask, "resolve_agent", lambda *_args, **_kwargs: SimpleNamespace(entry=row)
    )

    def fail(verb, payload, unavailable, *, timeout=30):
        raise RetaskTransportError("pane_send_timeout", detail="pane 12 went away")

    monkeypatch.setattr("fno.rust_binary.verb_call", fail)
    receipt = retask.run_retask("bp-xbdb9-retask", node="x-bdb9", env={})

    assert receipt["status"] == "refused"
    assert receipt["reason"] == "pane_send_timeout"
    assert receipt["detail"] == "pane 12 went away"
    assert receipt["cleared"] is False
    assert receipt["target_submit_confirmed"] is False


def test_run_retask_converts_thread_viewport_transport_error_to_structured_refusal(monkeypatch):
    """A resolve_thread_viewport failure must not escape run_retask as a bare exception."""
    import fno.agents.retask as retask

    row = _row(harness="claude", substrate="thread", mux=None, fno_id=None)
    monkeypatch.setattr(
        retask, "resolve_agent", lambda *_args, **_kwargs: SimpleNamespace(entry=row)
    )

    receipt = retask.run_retask("bp-thread-retask", node="x-bdb9", env={})

    assert receipt["status"] == "refused"
    assert "worker_has_no_thread_ref" in receipt["reason"]
    assert receipt["target_submit_confirmed"] is False


def _fake_parse_many(names):
    rows = []
    for name in names:
        parts = (name or "").split("-")
        verb = parts[1] if len(parts) > 2 else ""
        node = parts[2] if len(parts) > 2 else ""
        rows.append(naming.DispatchName(name, parts[0] if parts else "", verb, node, ""))
    return rows


def _bp_graph() -> dict:
    return {
        "x-e1": {"parent": None},
        "x-aaaa": {
            "parent": "x-e1",
            "sessions": [{"phase": "blueprint", "ended_at": "2026-09-15T00:31:00Z"}],
        },
        "x-bbbb": {"parent": "x-e1"},
        "x-e2": {"parent": None},
        "x-cccc": {"parent": "x-e2", "sessions": [{"phase": "blueprint", "ended_at": "2026-09-15T00:31:00Z"}]},
    }


def _eligible_row(**overrides) -> AgentEntry:
    values = dict(
        name="ac-bp-x-aaaa-slug",
        substrate="thread",
        inside_leg={"state": "done", "received_at": "2026-09-15T00:30:00Z"},
        node="x-aaaa",
    )
    values.update(overrides)
    return _row(**values)


def _pick(graph, rows, node_id="x-bbbb"):
    import fno.agents.retask as retask

    return retask.finished_planner(
        rows,
        node_id=node_id,
        graph=graph,
        project_id="proj",
        project_of=lambda cwd: "proj" if cwd == "/repo" else "other",
    )


def test_finished_planner_picks_the_earliest_finished_row_on_the_epic(monkeypatch):
    monkeypatch.setattr("fno.agents.retask.parse_many", _fake_parse_many)
    late = _eligible_row(
        name="ab-bp-x-aaaa-late",
        inside_leg={"state": "done", "received_at": "2026-09-15T01:00:00Z"},
    )
    early = _eligible_row()
    exited = _eligible_row(
        name="aa-bp-x-aaaa-gone",
        status="exited",
        inside_leg={"state": "done", "received_at": "2026-09-15T00:00:00Z"},
    )
    got = _pick(_bp_graph(), [late, exited, early])
    assert got is not None and got.name == "ac-bp-x-aaaa-slug"


def test_run_retask_refuses_when_the_dispatch_verb_cannot_resolve(monkeypatch):
    import fno.agents.retask as retask

    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: {"entries": [], "missing": []})
    row = _row()
    monkeypatch.setattr(retask, "resolve_agent", lambda *_a, **_k: SimpleNamespace(entry=row))

    receipt = retask.run_retask("bp-xbdb9-retask", node="x-bdb9", env={})

    assert receipt["status"] == "refused"
    assert receipt["reason"] == "dispatch_verb_unresolved"
    assert receipt["detail"]


def test_thread_viewport_reaches_by_registry_name_and_joins_the_opened_pane(
    monkeypatch,
) -> None:
    """AC1-HP: the door is keyed by the row name; the join still matches fno_id."""
    import fno.agents.retask as retask

    entry = _row(harness="claude", substrate="thread", mux=None, fno_id="F", name="bp-x")
    calls: list[list[str]] = []

    def run(argv, **_kwargs):
        calls.append(list(argv))
        if "thread" in argv:
            return SimpleNamespace(returncode=0, stdout="", stderr="")
        return SimpleNamespace(
            returncode=0,
            stdout=json.dumps([{"name": "bp-x", "fno_id": "F", "pane_id": 7}]),
            stderr="",
        )

    monkeypatch.setattr(retask, "resolve_mux_session", lambda *_args, **_kwargs: "main")
    monkeypatch.setattr(retask.subprocess, "run", run)

    assert retask.resolve_thread_viewport(entry) == ("main", 7)
    # The reach carries no baked placement: no flag tunes the row's open
    # portal, else the door's own portal 0 serves it.
    assert calls[0] == [
        "fno", "mux", "thread", "bp-x", "--server", "main",
    ]
