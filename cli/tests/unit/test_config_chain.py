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
