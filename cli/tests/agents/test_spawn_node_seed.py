"""CLI wiring for the node-seeded spawn: `fno agents spawn --node <id>` with no
typed message reads the node's encoded verb and brief (x-e53e change 1).

The seed render and the brief chain are unit-covered upstream; this file pins
the front door: a node-seeded pane carries verb + brief, the receipt names both
sources, an unencoded node refuses before any peer exists, and a typed message
wins over the node.
"""
from __future__ import annotations
from tests.fixtures.graph_seed import seed_graph

import json
import os

import pytest
from typer.testing import CliRunner


requires_rust = pytest.mark.dev_build


@pytest.fixture
def runner() -> CliRunner:
    return CliRunner()


@pytest.fixture(autouse=True)
def _isolated(monkeypatch, tmp_path):
    for m in ("CODEX_THREAD_ID", "CLAUDE_CODE_SESSION_ID", "CODEX_SESSION_ID"):
        monkeypatch.delenv(m, raising=False)
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path / "claims"))
    monkeypatch.setenv("FNO_STATE_ROOT", str(tmp_path / "state"))


_ENCODED = {
    "id": "x-1",
    "slug": "feat",
    "dispatch_verb": "/target",
    "difficulty": "low",
}


def _stub_pane_path(monkeypatch, *, rec=None, brief=("the brief", "explicit")):
    """Stub the graph read, the brief chain, the gate, provenance, and the pane
    spawn; return the kwargs the pane spawn received."""
    from fno.agents import mux_spawn, spawn_gate

    node_id = (rec or {}).get("id")
    if rec is not None:
        monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: {"entries": [rec], "missing": []})
    if brief is not None:
        monkeypatch.setattr(
            "fno.provenance.autobrief.resolve_dispatch_brief", lambda n: brief
        )

    class _Gate:
        def release(self) -> None:
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())

    def dispatchable(node, holder, **kwargs):
        return {"verdict": "dispatchable", "reservation_key": f"dispatch:{node}",
                "reservation_holder": holder}, 0

    monkeypatch.setattr("fno.agents.cli._spawn_guard_decision", dispatchable)
    monkeypatch.setattr(
        mux_spawn,
        "resolve_provenance",
        lambda *a, **k: {"FNO_NODE": node_id} if node_id else {},
    )

    received: dict = {}

    def fake_pane(**kwargs):
        received.update(kwargs)
        return mux_spawn.MuxSpawnResult(
            name=kwargs["name"], provider=kwargs["provider"], session="s",
            pane_id=1, child_pid=None, session_uuid=None,
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_pane)
    return received


def _invoke(runner, *args):
    from fno.agents.cli import agents_app

    return runner.invoke(agents_app, ["spawn", "--name", "w1", *args])


def test_node_seeded_pane_carries_verb_and_brief(monkeypatch, runner, loop_admission_ready):
    """AC1-HP: no typed message + encoded node -> the pane seed is the node's
    rendered verb command, the brief rides TARGET_BRIEF, and the receipt names
    both sources."""
    received = _stub_pane_path(monkeypatch, rec=dict(_ENCODED))
    result = _invoke(runner, "--node", "x-1", "--here", "--as", "pane", "--split", "left")
    assert result.exit_code == 0, result.output
    assert received["message"] == "/target --no-merge x-1"
    assert received["provenance"]["TARGET_BRIEF"] == "the brief"
    receipt = json.loads(
        next(ln for ln in result.output.splitlines() if ln.startswith("{"))
    )
    assert receipt["verb_source"] == "declared"
    assert receipt["brief_source"] == "explicit"


def test_unencoded_node_refuses_before_any_peer(monkeypatch, runner):
    """AC1-ERR: a node with no dispatch_verb and no typed message exits
    non-zero naming the node and the encode remedy; no pane is created."""
    received = _stub_pane_path(monkeypatch, rec={"id": "x-2", "slug": "bare", "difficulty": "low"})
    result = _invoke(runner, "--node", "x-2", "--here", "--as", "pane", "--split", "left")
    assert result.exit_code != 0
    assert "x-2" in result.output
    assert "fno backlog update x-2 dispatch_verb=" in result.output
    assert received == {}


def test_typed_message_with_a_node_composes_the_nodes_command(
    monkeypatch, runner, loop_admission_ready
):
    """with `--node`, a prose message gains the node's derived
    command in front; the node row is read and the brief rides along."""
    received = _stub_pane_path(monkeypatch, rec=dict(_ENCODED))
    result = _invoke(runner, "--node", "x-1", "say hi directly", "--here", "--as", "pane", "--split", "left")
    assert result.exit_code == 0, result.output
    assert received["message"] == "/fno:target x-1\n\nsay hi directly"
    assert "brief_source" not in result.output


def test_mux_session_forwards_to_the_pane_and_refuses_off_pane(
    monkeypatch, runner, loop_admission_ready
):
    """The dispatch-next porcelain pins its lane: --mux-session reaches
    dispatch_spawn_bounded_pane as `session`, and a non-pane substrate refuses."""
    received = _stub_pane_path(monkeypatch, rec=dict(_ENCODED))
    result = _invoke(
        runner, "--node", "x-1", "--here", "--as", "pane", "--split", "left", "--mux-session", "work"
    )
    assert result.exit_code == 0, result.output
    assert received["session"] == "work"

    result = _invoke(runner, "--name", "w2", "hi", "--once", "--mux-session", "work")
    assert result.exit_code != 0
    assert "needs --as pane" in result.output


# ---- x-3873 change 1: the door ensures the worktree (AC1-*) ----------------


def test_node_seeded_spawn_launches_in_the_ensured_worktree(monkeypatch, runner, tmp_path, loop_admission_ready):
    """AC1-HP: no typed message and no explicit cwd source -> the ensure seam
    runs once with the node's recorded cwd, the NODE id and the resolved
    harness, and the worker launches in the path it printed."""
    received = _stub_pane_path(monkeypatch, rec=dict(_ENCODED, cwd="/repo"))
    seen: dict = {}

    def fake_verb_call(verb, payload, unavailable_cls, **kw):
        seen["verb"] = verb
        seen["payload"] = dict(payload)
        return {"workdir": str(tmp_path / "wt")}

    import fno.rust_binary as rb

    monkeypatch.setattr(rb, "verb_call", fake_verb_call)
    result = _invoke(runner, "--node", "x-1", "--as", "pane", "--split", "left")
    assert result.exit_code == 0, result.output
    assert seen["verb"] == "launch-workdir"
    assert seen["payload"] == {"recorded_cwd": "/repo", "node": "x-1", "harness": "claude"}
    assert received["cwd"] == (tmp_path / "wt").resolve()


def test_ensure_refusal_holds_the_node(monkeypatch, runner):
    """AC1-ERR: a hold answer exits 2 naming the node and the hold; no peer
    is created."""
    received = _stub_pane_path(monkeypatch, rec=dict(_ENCODED))
    import fno.rust_binary as rb

    monkeypatch.setattr(rb, "verb_call", lambda *a, **k: {"hold": "refused"})
    result = _invoke(runner, "--node", "x-1", "--as", "pane", "--split", "left")
    assert result.exit_code == 2
    assert "worktree ensure refused or misconfigured for x-1" in result.output
    assert received == {}


# ---------------------------------------------------------------------------
# the seam projects facts to `fno-agents node-seed` and applies the
# answer before any lane is chosen. The transport is stubbed here; the
# requires_rust test drives the real binary.
# ---------------------------------------------------------------------------


def _row(**fields):
    row = {"id": "x-1", "slug": "seed", "dispatch_verb": "/blueprint", "difficulty": "medium"}
    row.update(fields)
    return row


def _stub_row(monkeypatch, row):
    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: {"entries": [row] if row else [], "missing": []})


def _stub_verb(monkeypatch, answer=None, *, unavailable=None):
    import fno.rust_binary as rb

    seen: list = []

    def _call(verb, payload, unavailable_cls, **kw):
        seen.append(payload.get("node_seed", payload))
        if unavailable is not None:
            raise unavailable_cls(str(unavailable))
        return answer

    monkeypatch.setattr(rb, "verb_call", _call)
    return seen


def _seed_args(*extra):
    # Real spawn argv: the name rides --name/--cwd, so the first positional
    # (when present) is the message seed.
    return ["spawn", *extra]


def test_seam_names_the_binary_when_the_verb_is_unavailable(monkeypatch, capsys):
    """AC3-HP: VerbUnavailable exits 2 naming the transport, never a silent
    fallback to the role profile."""
    _stub_row(monkeypatch, _row())

    class _Unavailable(Exception):
        pass

    _stub_verb(monkeypatch, unavailable="fno-agents: no binary")
    import fno.rust_binary as rb

    monkeypatch.setattr(rb, "VerbUnavailable", _Unavailable)
    from fno.agents.rust_runtime import _node_seed_at_seam

    with pytest.raises(SystemExit) as exc:
        _node_seed_at_seam(_seed_args("port it", "--node", "x-1"))
    assert exc.value.code == 2
    assert "spawn-axes unavailable" in capsys.readouterr().err


def test_seam_profile_returns_the_derived_verb(monkeypatch):
    _stub_row(monkeypatch, _row())
    _stub_verb(monkeypatch, {"action": "profile", "verb": "blueprint"})
    from fno.agents.rust_runtime import _node_seed_at_seam

    args, node_verb = _node_seed_at_seam(_seed_args("--node", "x-1", "--substrate", "thread"))
    assert node_verb == "blueprint"
    assert args[1] == "--node"


# ---- x-8d88: the seed names the node; the seam binds it ---------------------


@pytest.fixture
def _target_row(tmp_path):
    """A node whose lifecycle answers /target: a readable plan (no status
    scalar reads READY) beside a declared /target verb."""
    (tmp_path / "p.md").write_text("---\n---\n", encoding="utf-8")
    row = _row(dispatch_verb="/target", plan_path=str(tmp_path / "p.md"), cwd=str(tmp_path))
    return row


@pytest.fixture
def _rust_graph(tmp_path, monkeypatch):
    """Point the binary's own store read (FNO_HOME -> graph.json) at a fixture
    naming x-1, so the nodeless derive arm resolves it without the machine."""
    monkeypatch.setenv("FNO_HOME", str(tmp_path))
    seed_graph(tmp_path / "graph.json", json.dumps({"entries": [{"id": "x-1"}]}))


def _stub_verb_seq(monkeypatch, answers):
    """Like _stub_verb but each call pops the next canned answer; later calls
    answer pass, which is what an agreeing /target derivation meets."""
    import fno.rust_binary as rb

    seen: list = []

    def _call(verb, payload, unavailable_cls, **kw):
        seen.append(payload.get("node_seed", payload))
        return answers.pop(0) if answers else {"action": "pass"}

    monkeypatch.setattr(rb, "verb_call", _call)
    return seen


def test_seam_derive_names_an_unresolvable_node_as_a_receipt(monkeypatch):
    """x-8d88 Task 4: the seed names a node the verb's store read cannot
    resolve, so the receipt-compose answer hands the reason to the mint
    through FNO_NODE_REASON and the spawn proceeds WITHOUT a node binding;
    the receipt is never a parser flag."""
    _stub_row(monkeypatch, None)
    receipt_argv = [
        "spawn",
        "/fno:target x-gone",
        "--node-reason",
        "x-gone names no readable backlog row (derived from the seed)",
    ]
    _stub_verb_seq(monkeypatch, [{"action": "compose", "argv": receipt_argv}])
    from fno.agents.rust_runtime import _node_seed_at_seam

    args, node_verb = _node_seed_at_seam(_seed_args("/fno:target x-gone"))
    assert node_verb is None
    assert "--node-reason" not in args
    assert "--node" not in args
    assert "x-gone" in os.environ.get("FNO_NODE_REASON", "")
    monkeypatch.delenv("FNO_NODE_REASON", raising=False)


@requires_rust
def test_seed_only_pane_spawn_mints_the_nodes_row_binding(
    monkeypatch, runner, _target_row, _rust_graph, loop_admission_ready
):
    """x-8d88 Task 2: a /fno:target x-1 seed and no --node reaches the pane
    mint with the node resolved: provenance carries FNO_NODE and the seed
    passes through unchanged (agreement, so no compose rewrite)."""
    from fno.agents import mux_spawn, spawn_gate

    monkeypatch.setattr("fno.graph.store.read_nodes_by_ids", lambda path, tokens: {"entries": [_target_row], "missing": []})

    class _Gate:
        def release(self) -> None:
            pass

    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.setattr(
        mux_spawn,
        "resolve_provenance",
        lambda node=None, *a, **k: {"FNO_NODE": node} if node else {},
    )
    received: dict = {}

    def fake_pane(**kwargs):
        received.update(kwargs)
        return mux_spawn.MuxSpawnResult(
            name=kwargs["name"], provider=kwargs["provider"], session="s",
            pane_id=1, child_pid=None, session_uuid=None,
        )

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", fake_pane)
    result = _invoke(runner, "/fno:target x-1", "--here", "--as", "pane", "--split", "left")
    assert result.exit_code == 0, result.output
    assert received["provenance"]["FNO_NODE"] == "x-1"
    assert received["message"] == "/fno:target x-1"


# ---- the payload names the node; the resolver answers with a source --------


@requires_rust
def test_seam_real_binary_refuses_a_payload_named_unknown_node(monkeypatch, capsys):
    """AC7-ERR on the real transport: the compiled resolver names x-dead from
    the payload, and the refusal names the payload, never --node."""
    _stub_row(monkeypatch, None)
    from fno.agents.rust_runtime import _node_seed_at_seam

    with pytest.raises(SystemExit) as exc:
        _node_seed_at_seam(_seed_args("/fno:target x-dead"))
    assert exc.value.code == 2
    err = capsys.readouterr().err
    assert "the payload names x-dead, but no readable backlog row" in err


@requires_rust
def test_seam_real_binary_payload_disagreement_names_the_payload(monkeypatch, tmp_path, capsys):
    """AC4-ERR on the real transport: a payload-named node whose verb
    disagrees refuses naming the payload and the spawn to run instead."""
    (tmp_path / "p.md").write_text("---\n---\n", encoding="utf-8")
    _stub_row(
        monkeypatch,
        _row(id="x-1be2", dispatch_verb="/blueprint", plan_path=str(tmp_path / "p.md"), cwd=str(tmp_path)),
    )
    from fno.agents.rust_runtime import _node_seed_at_seam

    with pytest.raises(SystemExit) as exc:
        _node_seed_at_seam(_seed_args("/fno:target x-1be2"))
    assert exc.value.code == 2
    err = capsys.readouterr().err
    assert "the payload names /target x-1be2" in err
    assert "spawn /fno:blueprint x-1be2" in err
    assert "--node" not in err


def test_refused_spawn_reaches_no_lane(monkeypatch, runner):
    """A payload-verb refusal exits 2 at the front door, before any lane is
    routed: nothing mints a registry row or takes the node claim."""
    from fno.agents import rust_runtime as rr
    from fno.cli import app

    _stub_row(monkeypatch, _row())
    _stub_verb(monkeypatch, {"action": "refuse", "message": "--node x-1 derives /blueprint; the payload names /target."})
    routed: list = []
    monkeypatch.setenv(rr.RUNTIME_ENV, "rust")
    monkeypatch.setattr(rr, "route_to_rust", lambda args, **kw: routed.append(list(args)))

    result = runner.invoke(app, ["agents", "spawn", "-H", "claude", "--name", "w1", "--node", "x-1", "/fno:target x-1"])

    assert result.exit_code == 2, result.output
    assert "derives /blueprint" in result.output
    assert routed == []
