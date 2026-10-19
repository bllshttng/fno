"""FNO_NODE reaches bg and headless workers, not only mux panes (x-d157, AC6-HP).

``resolve_provenance`` has always built the FNO_NODE/FNO_SLUG/FNO_PLAN map, but
only the pane path called it. bg and headless build their child env from
``os.environ``, so the export has to land there for the origin-capture fallback
to fire anywhere outside a pane -- which is where the capture miss concentrates.
"""
from __future__ import annotations

from pathlib import Path
from typing import Any, Dict

import pytest
from typer.testing import CliRunner

runner = CliRunner()


class _Gate:
    def release(self) -> None:
        pass


@pytest.fixture
def spawned_env(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> Dict[str, Any]:
    """Invoke cmd_spawn with the dispatch faked; capture the env the child sees."""
    from fno.agents import dispatch, spawn_gate

    # A node-driven spawn now takes the node claim itself and leaves it for the
    # worker to inherit, so these cases share one live claim on the id they all
    # reuse. Root the claim store per test: otherwise the first case's handover
    # window refuses every later one, and a local run writes real claims for
    # fake nodes into the user's global store.
    monkeypatch.setenv("FNO_CLAIMS_ROOT", str(tmp_path))
    monkeypatch.setattr(spawn_gate, "run_gate", lambda *a, **k: _Gate())
    monkeypatch.delenv("FNO_NODE", raising=False)
    monkeypatch.delenv("FNO_SLUG", raising=False)
    monkeypatch.delenv("FNO_PLAN", raising=False)

    seen: Dict[str, Any] = {}

    def fake_dispatch_spawn(**kwargs: Any) -> Any:
        import os

        # The provider builds the child env from os.environ at this point, so
        # reading it here is what the worker would actually inherit.
        seen["env"] = dict(os.environ)
        return dispatch.SpawnResult(
            kind="created", name=kwargs["name"], provider="claude", short_id="abcd1234"
        )

    monkeypatch.setattr("fno.agents.dispatch.dispatch_spawn", fake_dispatch_spawn)
    # Resolve without touching the real graph: the export wiring is the subject.
    monkeypatch.setattr(
        "fno.agents.mux_spawn.resolve_provenance",
        lambda node, slug=None, plan=None: (
            {"FNO_NODE": node, "FNO_SLUG": "a-slug"} if node else {}
        ),
    )
    return seen


def test_export_does_not_outlive_the_dispatch(spawned_env: Dict[str, Any]) -> None:
    """The export is scoped to the dispatch call, not left on the process.

    cmd_spawn is called in-process (tests, and any caller that spawns twice), so
    a surviving FNO_NODE would attribute the second spawn's worker to the first
    spawn's node.
    """
    import os

    from fno.agents.cli import agents_app

    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "/fix hi", "--harness", "claude",
         "--substrate", "thread", "--node", "x-aaaa"],
    )
    assert result.exit_code == 0, result.output
    assert spawned_env["env"]["FNO_NODE"] == "x-aaaa"
    assert "FNO_NODE" not in os.environ


def test_provenance_keys_are_set_or_cleared_as_a_group(
    spawned_env: Dict[str, Any], monkeypatch: pytest.MonkeyPatch
) -> None:
    """A parent's FNO_PLAN must not ride along with a child's FNO_NODE.

    A worker dispatching a child for a plan-less node would otherwise hand down
    its own plan path beside someone else's node id.
    """
    from fno.agents.cli import agents_app

    monkeypatch.setenv("FNO_PLAN", "/parent/plan.md")
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w1", "/fix hi", "--harness", "claude",
         "--substrate", "thread", "--node", "x-aaaa"],
    )
    assert result.exit_code == 0, result.output
    assert spawned_env["env"]["FNO_NODE"] == "x-aaaa"
    assert "FNO_PLAN" not in spawned_env["env"]
    # and the parent's own env is put back afterwards
    import os

    assert os.environ["FNO_PLAN"] == "/parent/plan.md"


# -- per-worker identity: FNO_WORKER_NAME (task-claim holder) --


def test_spawn_overwrites_an_inherited_worker_name(
    spawned_env: Dict[str, Any], monkeypatch: pytest.MonkeyPatch
) -> None:
    """A dispatcher that spawned this spawn must not leak ITS name: the child's
    own roster name is the only provable one, so an inherited value loses."""
    from fno.agents.cli import agents_app

    monkeypatch.setenv("FNO_WORKER_NAME", "parent-otter")
    result = runner.invoke(
        agents_app,
        ["spawn", "--name", "w2", "hi", "--harness", "claude", "--substrate", "thread"],
    )
    assert result.exit_code == 0, result.output
    assert spawned_env["env"]["FNO_WORKER_NAME"] == "w2"
