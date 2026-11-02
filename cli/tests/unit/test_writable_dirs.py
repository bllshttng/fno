"""fno's computed writable-directory grant: the set, and the three lanes that carry it.

The defect these cover: a spawned worker on a bounded posture cannot write
``~/.fno/claims``, so it holds no node claim and ``fno agents claim status`` answers
``free`` while it works - a duplicate-dispatch trap the standing "check the claim
first" rule cannot catch, because the check returns free.

One resolver, three funnels. Each lane gets its OWN assertion here rather than
one assertion standing in for the pair: a grant present on one of N reachable
paths is decorative.
"""
from __future__ import annotations

from pathlib import Path

import pytest

from fno.agents.writable_dirs import worker_writable_dirs


@pytest.fixture
def fake_state(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Point every state resolver at one existing tmp root."""
    state = tmp_path / "state"
    (state / "claims").mkdir(parents=True)
    agents_home = state / "agents"
    agents_home.mkdir()
    monkeypatch.setenv("FNO_AGENTS_HOME", str(agents_home))
    monkeypatch.setattr("fno.paths.state_dir", lambda: state)
    monkeypatch.setattr("fno.claims.io.global_claims_root", lambda: state)
    monkeypatch.setattr("fno.claims.io.claims_dir", lambda root=None: state / "claims")
    # No plan directory unless a test opts in: an unresolvable plans dir grants
    # nothing, which keeps the state-root assertions below about the state root.
    monkeypatch.setattr(
        "fno.paths.plans_content_dir", lambda project_root=None: tmp_path / "nope"
    )
    return state


def test_state_root_and_divergent_claims_root_both_granted(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """``locks_dir``/``global_claims_root`` are deliberately config-free while
    ``state_dir`` honors ``config.paths.state_dir``. When an override splits
    them, a grant for one leaves the worker unable to write the other."""
    configured = tmp_path / "configured"
    configured.mkdir()
    claims_home = tmp_path / "home" / ".fno"
    (claims_home / "claims").mkdir(parents=True)
    monkeypatch.setattr("fno.paths.state_dir", lambda: configured)
    monkeypatch.setattr("fno.claims.io.global_claims_root", lambda: tmp_path / "home")
    monkeypatch.setattr(
        "fno.claims.io.claims_dir", lambda root=None: claims_home / "claims"
    )
    monkeypatch.setattr(
        "fno.paths.plans_content_dir", lambda project_root=None: tmp_path / "nope"
    )

    assert worker_writable_dirs(tmp_path) == [str(configured), str(claims_home)]


def test_plan_dir_is_granted_but_never_the_vault_above_it(
    fake_state: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The grant is a WRITE grant, so granting the vault root to let a worker
    write one plan hands a code worker the operator's whole notes directory.
    Grant the plan's own directory instead."""
    vault = tmp_path / "vault"
    plans = vault / "fno" / "plans"
    plans.mkdir(parents=True)
    monkeypatch.setattr("fno.paths.vault_root", lambda **kw: vault)

    got = worker_writable_dirs(tmp_path, plan_path=plans / "p.md")
    assert got == [str(fake_state), str(plans)]
    assert str(vault) not in got


# --------------------------------------------------------------------------
# The three funnels. One assertion per lane.
# --------------------------------------------------------------------------


# --------------------------------------------------------------------------
# The provider with no additive grant. Two directions, and they differ.
# --------------------------------------------------------------------------


def test_computed_set_is_skipped_on_a_provider_with_no_additive_grant(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """Fail-closed is correct for an operator flag and catastrophic for a
    computed default: raising here would refuse every opencode spawn."""
    from fno.agents.mux_spawn import build_pane_argv

    import fno.agents.writable_dirs as wd

    wd._SKIP_NOTED.discard("opencode")
    argv = build_pane_argv("opencode", "t", tmp_path, False, None)
    assert "--add-dir" not in argv
    assert "--dir" not in argv
    err = capsys.readouterr().err
    assert "opencode" in err
    assert "claim" in err

    # Once per process: the set is non-empty on every machine, so an unguarded
    # print fires on every opencode spawn including argv-parity builds.
    build_pane_argv("opencode", "t", tmp_path, False, None)
    assert capsys.readouterr().err == ""


# The lane the Python token builders never see.
# --------------------------------------------------------------------------


