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


@pytest.fixture
def one_grant(monkeypatch: pytest.MonkeyPatch) -> str:
    """Pin the computed set to one known token across all three funnels."""
    token = "/grant/state-root"
    for target in (
        "fno.agents.mux_spawn.worker_writable_dirs",
        "fno.agents.harnesses.claude.worker_writable_dirs",
        "fno.agents.writable_dirs.worker_writable_dirs",
    ):
        monkeypatch.setattr(target, lambda *a, **k: [token])
    return token


def test_pane_lane_carries_the_grant(one_grant: str, tmp_path: Path) -> None:
    from fno.agents.mux_spawn import build_pane_argv

    argv = build_pane_argv("claude", "t", tmp_path, False, None)
    # Located by PAIR, not by the first --add-dir: codex's own git and plan
    # carveouts ride the same repeatable flag ahead of this one.
    pairs = [(argv[i], argv[i + 1]) for i, tok in enumerate(argv) if tok == "--add-dir"]
    assert ("--add-dir", one_grant) in pairs


def test_codex_pane_lane_drops_the_grant_on_the_remote_launch(
    one_grant: str, tmp_path: Path
) -> None:
    """codex >= 0.156.1 refuses `--add-dir` paired with `--remote`, so riding
    the grant killed the pane before it painted (x-0a75). The codex pane is
    the one pane arm whose launch is remote today, so its grant rides out with
    the strip; claude and agy keep carrying theirs."""
    from fno.agents.mux_spawn import build_pane_argv

    argv = build_pane_argv("codex", "t", tmp_path, False, None)
    assert "--remote" in argv
    assert ("--add-dir", one_grant) not in [
        (argv[i], argv[i + 1]) for i, tok in enumerate(argv) if tok == "--add-dir"
    ]


def test_claude_headless_lane_carries_the_grant(
    one_grant: str, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Asserted separately from the bg lane: they are two argv builders and a
    grant on one of them is decorative."""
    import fno.agents.harnesses.claude as claude_mod

    seen: dict[str, list[str]] = {}

    def fake_run(argv, **kwargs):  # type: ignore[no-untyped-def]
        seen["argv"] = list(argv)
        raise SystemExit(0)

    monkeypatch.setattr(claude_mod, "_subprocess_run", fake_run)
    with pytest.raises(SystemExit):
        claude_mod.headless_create(cwd=tmp_path, message="m")
    argv = seen["argv"]
    assert argv[:2] == ["claude", "-p"]
    assert argv[argv.index("--add-dir") + 1] == one_grant


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


def test_seam_export_publishes_the_set_for_the_rust_route(
    fake_state: Path, tmp_path: Path
) -> None:
    """Only the PANE substrate keeps Python; every other spawn runs in the Rust
    binary, which builds the harness argv itself and forwards only the operator's
    --add-dir. So the three token builders cover one reachable lane, and
    `--substrate bg` - what the shipped stage table uses for its own delivery
    lane - launched a worker that could not write the claim store.
    """
    from fno.agents.writable_dirs import (
        WORKER_ADD_DIRS_ENV,
        export_worker_writable_dirs,
    )

    env: dict[str, str] = {}
    published = export_worker_writable_dirs(tmp_path, env)
    assert published == [str(fake_state)]
    assert env[WORKER_ADD_DIRS_ENV] == str(fake_state)


def test_seam_export_clears_an_inherited_value(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The pane lane publishes on os.environ and a pane shell inherits it, so a
    spawn made from inside a worker starts with the PARENT's value set. An empty
    computation must clear it, not leave another project's directories standing
    as write grants."""
    from fno.agents.writable_dirs import (
        WORKER_ADD_DIRS_ENV,
        export_worker_writable_dirs,
    )

    for name in ("state_dir",):
        monkeypatch.setattr(f"fno.paths.{name}", lambda: tmp_path / "nope")
    monkeypatch.setattr("fno.claims.io.global_claims_root", lambda: tmp_path / "nope")
    monkeypatch.setattr(
        "fno.claims.io.claims_dir", lambda root=None: tmp_path / "nope" / "claims"
    )
    monkeypatch.setattr(
        "fno.paths.plans_content_dir", lambda project_root=None: tmp_path / "nope"
    )

    env = {WORKER_ADD_DIRS_ENV: "/some/other/project"}
    assert export_worker_writable_dirs(tmp_path, env) == []
    assert WORKER_ADD_DIRS_ENV not in env


def test_resume_lane_reads_the_published_value_rather_than_recomputing(
    fake_state: Path, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """`codex exec resume` is built by BOTH runtimes and Rust cannot run the
    resolver, so both must read one published value. A Python side that computed
    instead would emit a different roots list whenever the env is unset, which is
    what the rust/py argv-parity tests catch."""
    from fno.agents.harnesses.codex import git_writable_config_args
    from fno.agents.writable_dirs import (
        WORKER_ADD_DIRS_ENV,
        published_worker_writable_dirs,
    )

    monkeypatch.delenv(WORKER_ADD_DIRS_ENV, raising=False)
    assert published_worker_writable_dirs() == []
    # Unset env: the resume roots carry no state grant, matching Rust.
    unset = git_writable_config_args(tmp_path)
    assert str(fake_state) not in "".join(unset)

    # Published: it rides, because writable_roots is a WHOLE-VALUE override and
    # a list without the state root leaves a resumed worker unable to claim.
    monkeypatch.setenv(WORKER_ADD_DIRS_ENV, str(fake_state))
    assert published_worker_writable_dirs() == [str(fake_state)]
    published = git_writable_config_args(tmp_path)
    assert str(fake_state) in "".join(published)
