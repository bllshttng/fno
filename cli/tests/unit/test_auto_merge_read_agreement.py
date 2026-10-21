"""The auto_merge read the merge path uses.

The chain is project > canonical > global; the first file carrying the key
wins, so the global key is the fleet default, never a kill switch.
"""

from __future__ import annotations

from pathlib import Path

from fno.config import load_settings_for_repo
from fno.config_cli import get_cmd


def _patch_chain(monkeypatch, worktree: Path, canonical: Path, tmp_path: Path):
    worktree.mkdir(parents=True, exist_ok=True)
    (worktree / ".git").touch()
    monkeypatch.setenv("FNO_GLOBAL_SETTINGS_PATH", str(tmp_path / "global.toml"))
    monkeypatch.delenv("FNO_CONFIG", raising=False)
    monkeypatch.delenv("FNO_NO_CANONICAL_CONFIG", raising=False)
    monkeypatch.setattr("fno.paths.resolve_repo_root", lambda: worktree)
    monkeypatch.setattr(
        "fno.paths.resolve_canonical_worktree",
        lambda root=None, timeout=None: canonical,
    )
    monkeypatch.setattr("fno.paths.resolve_canonical_repo_root", lambda: canonical)


def _write_enabled(path: Path, enabled: bool) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        f"[auto_merge]\nenabled = {str(enabled).lower()}\n", encoding="utf-8"
    )


def test_project_false_vetoes_a_global_true_for_both_readers(
    monkeypatch, tmp_path: Path, capsys
):
    worktree = tmp_path / "worktree"
    canonical = tmp_path / "canonical"
    _write_enabled(tmp_path / "global.toml", True)
    _write_enabled(canonical / ".fno" / "config.toml", False)
    _patch_chain(monkeypatch, worktree, canonical, tmp_path)

    get_cmd("auto_merge.enabled", False)

    captured = capsys.readouterr()
    assert captured.out == "False\n"
    assert load_settings_for_repo(worktree).auto_merge.enabled is False


def test_seeded_merge_read_climbs_to_the_canonical_project_from_a_worktree(
    monkeypatch, tmp_path: Path
):
    # The PR 1628 shape: the merge runs inside a worker worktree that carries
    # no config of its own. The read must climb to the canonical project tier,
    # never fall straight through to the global default.
    worktree = tmp_path / "worktree"
    canonical = tmp_path / "canonical"
    _write_enabled(tmp_path / "global.toml", False)
    _write_enabled(canonical / ".fno" / "config.toml", True)
    _patch_chain(monkeypatch, worktree, canonical, tmp_path)

    assert load_settings_for_repo(worktree).auto_merge.enabled is True
