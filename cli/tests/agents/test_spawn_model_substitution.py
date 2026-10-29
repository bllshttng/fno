"""v23 (x-2019): the spawn path names a model substitution instead of
printing the request back as though it were the effect.

Every assertion here is POSITIVE - a line present, a key present, both values
present. A test that passes on an absence proves nothing: the observed axis
already populates itself, and the failure this module guards against is
silence, not a wrong value.

Coverage:
  - REVIVE arm (deterministic): a resumed session whose transcript says
    glm-5.3-flash while the spawn asked glm-5.3[1m] produces the stderr line,
    the result marker, the event, and a row holding BOTH values.
  - FRESH arm with no sample yet: no line, no event, and the row labels
    its model token `requested` instead of asserting an unproven effect.
"""
from __future__ import annotations


import pytest

from fno.paths_testing import use_tmpdir

DEAD_UUID = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


@pytest.fixture
def workdir_claude(tmp_path, monkeypatch):
    """Isolated fno home with the fake claude on PATH (emits short_id 7c5dcf5d)."""
    from tests.agents._fake_claude import install_fake_claude

    use_tmpdir(monkeypatch, tmp_path)
    bin_dir = tmp_path / "bin"
    install_fake_claude(bin_dir)
    monkeypatch.setenv("PATH", str(bin_dir))
    return tmp_path


def _observe_as(monkeypatch, model: str) -> None:
    """Pin the transcript probe's answer: the session runs `model`."""
    from fno.provenance import observed as observed_mod

    monkeypatch.setattr(
        observed_mod,
        "observed_model_for_session",
        lambda agent, session_id, cwd: {"kind": "observed", "model": model, "samples": 3},
    )


def _dispatch(name: str, *, model: str, resume: str | None = None):
    """The dispatch door the respawn ladder uses: the spawn command's --model and
    --resume spellings are retired, so the request rides in as a parameter."""
    from pathlib import Path

    from fno.agents import dispatch

    return dispatch.dispatch_spawn(
        name=name, message="hi", harness="claude", cwd=Path.cwd(),
        model=model, resume_session_id=resume,
    )


def test_revive_with_contradicting_history_names_the_substitution(
    workdir_claude, monkeypatch, capsys
) -> None:
    """The operator's specimen shape: asked glm-5.3[1m], session runs flash."""
    from fno.agents import events as events_mod
    from fno.agents.registry import AgentEntry, load_registry, update_registry

    _observe_as(monkeypatch, "glm-5.3-flash")
    emitted: list[tuple] = []
    monkeypatch.setattr(
        events_mod, "emit", lambda kind=None, **kw: emitted.append((kind, kw))
    )

    update_registry(
        lambda entries: entries
        + [
            AgentEntry(
                name="rev-sub",
                harness="claude",
                cwd="/tmp",
                log_path="/tmp/rev-sub.log",
                short_id="deadbeef",
                harness_session_id=DEAD_UUID,
            )
        ]
    )

    result = _dispatch("rev-sub", model="glm-5.3[1m]", resume=DEAD_UUID)

    # POSITIVE marker 1: stderr names the substitution with BOTH values.
    err = capsys.readouterr().err
    assert "model substituted" in err
    assert "glm-5.3[1m]" in err
    assert "glm-5.3-flash" in err

    # POSITIVE marker 2: the result carries the machine-readable object the
    # receipt prints.
    assert result.model_substituted == {
        "requested": "glm-5.3[1m]",
        "observed": "glm-5.3-flash",
    }

    # POSITIVE marker 3: the event fired.
    subs = [kw for name, kw in emitted if name == "model_substituted"]
    assert subs, "model_substituted event must fire on a confirmed substitution"
    assert subs[0]["requested_model"] == "glm-5.3[1m]"
    assert subs[0]["actual_model"] == "glm-5.3-flash"

    # POSITIVE marker 4: the row stores BOTH values, request verbatim.
    row = next(r for r in load_registry() if r.name == "rev-sub")
    assert row.requested_model == "glm-5.3[1m]"
    assert row.model == "glm-5.3-flash"
    assert row.model_basis == "verified"


def test_fresh_spawn_without_a_sample_labels_the_request_and_says_nothing(
    workdir_claude, monkeypatch
) -> None:
    """No transcript sample yet: no verdict, and the receipt LABELS the token."""
    from fno.agents import events as events_mod
    from fno.agents.registry import load_registry

    emitted: list[tuple] = []
    monkeypatch.setattr(
        events_mod, "emit", lambda kind=None, **kw: emitted.append((kind, kw))
    )

    result = _dispatch("fresh-sub", model="glm-5.3[1m]")
    assert result.model_substituted is None

    # No sample means no verdict: no event, no row stamp.
    assert not [kw for name, kw in emitted if name == "model_substituted"]
    row = next(r for r in load_registry() if r.name == "fresh-sub")
    assert row.requested_model == "glm-5.3[1m]"
    assert row.model == "glm-5.3[1m]"
    assert row.model_basis == "requested"
    assert row.fno_id
    assert row.mux is None


