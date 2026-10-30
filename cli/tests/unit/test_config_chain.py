from __future__ import annotations

from pathlib import Path

from fno import config


def test_repo_loader_preserves_a_real_worktree_config_file(monkeypatch, tmp_path: Path):
    worktree = tmp_path / "worktree"
    canonical = tmp_path / "canonical"
    worktree.mkdir()
    (worktree / ".git").touch()
    (worktree / ".fno").mkdir(parents=True)
    (canonical / ".fno").mkdir(parents=True)
    (worktree / ".fno" / "config.toml").write_text(
        "[review]\nmax_rounds = 7\n",
        encoding="utf-8",
    )
    (canonical / ".fno" / "config.toml").write_text(
        "[review]\ngithub_approval_satisfies = false\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    # The hermetic env drops the canonical candidate this test reads.
    monkeypatch.delenv("FNO_NO_CANONICAL_CONFIG", raising=False)
    monkeypatch.setattr(
        "fno.paths.resolve_canonical_worktree",
        lambda root=None, timeout=None: canonical,
    )

    settings = config.load_settings_for_repo(worktree)

    assert settings.review.max_rounds == 7
    assert settings.review.github_approval_satisfies is False


def test_merge_read_climbs_to_the_canonical_project_from_a_worktree(
    monkeypatch, tmp_path: Path
):
    # The PR 1628 shape: the merge runs inside a worker worktree that carries
    # no config of its own. The read must climb to the canonical project tier,
    # never fall straight through to the global default - the global key is
    # the fleet default, never a kill switch over a project decision.
    worktree = tmp_path / "worktree"
    canonical = tmp_path / "canonical"
    worktree.mkdir()
    (worktree / ".git").touch()
    (canonical / ".fno").mkdir(parents=True)
    (tmp_path / "global.toml").write_text(
        "[auto_merge]\nenabled = false\n", encoding="utf-8"
    )
    (canonical / ".fno" / "config.toml").write_text(
        "[auto_merge]\nenabled = true\n", encoding="utf-8"
    )
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_NO_CANONICAL_CONFIG", raising=False)
    monkeypatch.setattr(
        "fno.paths.resolve_canonical_worktree",
        lambda root=None, timeout=None: canonical,
    )

    settings = config.load_settings_for_repo(worktree)

    assert settings.auto_merge.enabled is True


def test_a_pinned_config_is_the_global_settings_path(monkeypatch, tmp_path: Path):
    from fno.config_io import _global_settings_path

    pin = tmp_path / "sandbox" / "config.toml"
    pin.parent.mkdir()
    pin.write_text("schema_version = 1\n", encoding="utf-8")
    monkeypatch.setenv("FNO_CONFIG", str(pin))
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))

    assert _global_settings_path() == pin


def test_a_seeded_root_load_under_a_pin_never_reads_the_machine_global(
    monkeypatch, tmp_path: Path
):
    # A sandbox that pins FNO_CONFIG alone must not inherit the machine
    # global's share posture: the pin outranks the global settings path for
    # the chain's global candidate, so a machine global with share_backlog on
    # stays invisible to the sandbox.
    worktree = tmp_path / "sandbox-repo"
    (worktree / ".fno").mkdir(parents=True)
    pin = tmp_path / "sandbox" / "config.toml"
    pin.parent.mkdir()
    pin.write_text("schema_version = 1\n", encoding="utf-8")
    (tmp_path / "global.toml").write_text(
        "[store]\nremote_url = \"http://primary.example\"\nshare_backlog = true\n",
        encoding="utf-8",
    )
    monkeypatch.setenv("FNO_CONFIG", str(pin))
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))
    monkeypatch.delenv("FNO_NO_CANONICAL_CONFIG", raising=False)
    monkeypatch.setattr(
        "fno.paths.resolve_canonical_worktree",
        lambda root=None, timeout=None: tmp_path / "canonical",
    )

    settings = config.load_settings_for_repo(worktree)

    assert settings.store.share_backlog is False
