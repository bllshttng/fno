"""Spawn flag passthrough: every harness flag rides the substrate its
spawn resolves, and a flag the harness's thread lane cannot carry demotes
the spawn to the pane instead of refusing. The carrier facts live in the
capability contract's ``[harness.<name>.thread]`` rows; one predicate
(``harness_map.fence_uncarried``) decides, and both front doors (the
Python CLI and the spawn seam) call it.
"""

import pytest
import typer
from typer.testing import CliRunner



def _run(monkeypatch: pytest.MonkeyPatch, argv: list[str]):
    import fno.agents.cli as agents_cli

    monkeypatch.setenv("FNO_AGENTS_RUNTIME", "python")
    return CliRunner().invoke(agents_cli.agents_app, argv)


def _capture_pane_dispatch(monkeypatch: pytest.MonkeyPatch) -> dict:
    """Record a demoted pane launch and stop there, so no mux session is
    touched."""
    import fno.agents.mux_spawn as mux_spawn

    calls: dict = {}

    def recorder(**kwargs):
        calls.update(kwargs)
        raise typer.Exit(code=0)

    monkeypatch.setattr(mux_spawn, "dispatch_spawn_bounded_pane", recorder)
    return calls


def _capture_thread_dispatch(monkeypatch: pytest.MonkeyPatch) -> dict:
    """Record a dispatch_spawn call (the thread/headless arm) and stop
    there."""
    import fno.agents.dispatch as dispatch

    calls: dict = {}

    def recorder(**kwargs):
        calls.update(kwargs)
        raise typer.Exit(code=0)

    monkeypatch.setattr(dispatch, "dispatch_spawn", recorder)
    return calls


# ---------------------------------------------------------------------------
# The predicate
# ---------------------------------------------------------------------------


def test_fence_uncarried_reads_the_carrier_rows() -> None:
    from fno.agents.harness_map import fence_uncarried

    # codex carries its config and grant spellings, value token included.
    assert fence_uncarried("codex", ["-c", "key=1"]) is None
    assert fence_uncarried("codex", ["--config", "key=1"]) is None
    assert fence_uncarried("codex", ["--add-dir", "/tmp/x"]) is None
    assert fence_uncarried("codex", ["-p", "profile"]) == "-p"
    assert fence_uncarried("codex", ["--profile", "p"]) == "--profile"
    # claude's thread row and pi's keeper row carry every fenced token.
    assert fence_uncarried("claude", ["--anything"]) is None
    assert fence_uncarried("pi", ["--whatever", "x"]) is None
    # opencode's thread row carries none.
    assert fence_uncarried("opencode", ["--foo"]) == "--foo"
    assert fence_uncarried("opencode", None) is None


# ---------------------------------------------------------------------------
# A thread never demotes to a pane: it refuses and names --as pane
# ---------------------------------------------------------------------------


def test_a_thread_meeting_an_uncarried_fence_token_refuses_naming_as_pane(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    sent = _capture_pane_dispatch(monkeypatch)
    res = _run(
        monkeypatch,
        ["spawn", "work", "--harness", "codex", "--", "--profile", "p"],
    )
    assert res.exit_code == 2, res.output
    assert sent == {}, res.output
    assert (
        "the codex thread lane has no carrier for --profile; add --as pane" in res.output
    ), res.output


def test_legacy_flag_shaped_seed_fence_rides_the_thread_as_the_prompt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # One fenced token with NO message before the fence is the seed idiom,
    # and the seam promotes it to the MESSAGE before the resolver runs: it
    # rides the thread lane as the prompt, never read as passthrough.
    sent = _capture_thread_dispatch(monkeypatch)
    pane = _capture_pane_dispatch(monkeypatch)
    res = _run(monkeypatch, ["spawn", "--harness", "codex", "--", "run the tests"])
    assert sent.get("harness") == "codex", res.output
    assert sent.get("message") == "run the tests", res.output
    assert not pane, res.output


# ---------------------------------------------------------------------------
# -c stays --cwd; codex config rides the fence (AC8)
# ---------------------------------------------------------------------------


def test_dash_c_config_value_stops_before_launch(monkeypatch: pytest.MonkeyPatch) -> None:
    res = _run(monkeypatch, ["spawn", "work", "-c", "model_reasoning_effort=high"])
    assert res.exit_code == 2, res.output
    assert "--cwd" in res.output, res.output
    assert "-- -c key=value" in res.output, res.output
    assert "no worker launched" in res.output, res.output
