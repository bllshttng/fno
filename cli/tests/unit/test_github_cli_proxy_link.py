"""The gh shim's ``fno-gh-proxy`` link follows the current helper."""
from __future__ import annotations

import os
from pathlib import Path

from fno.setup import github_cli


def _exe(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(0o755)
    return path


def test_a_link_left_on_a_retired_console_script_is_repointed(tmp_path, monkeypatch):
    # Specimen 2026-10-22: the link still named a dev venv's console script
    # after pr port 7 deleted fno.pr, so every gh read died in an import.
    root = tmp_path / "github-cli"
    stale = _exe(
        tmp_path / "venv" / "fno-gh-proxy",
        "#!/usr/bin/python3\nfrom fno.pr.gh_proxy import main\n",
    )
    native = _exe(tmp_path / "cargo" / "fno-gh-proxy", "#!/bin/sh\nexit 0\n")
    real_gh = _exe(tmp_path / "real" / "gh", "#!/bin/sh\nexit 0\n")
    root.mkdir()
    (root / "fno-gh-proxy").symlink_to(stale)
    # The link's own dir comes first on PATH and must not answer for itself.
    monkeypatch.setenv(
        "PATH", os.pathsep.join([str(root), str(stale.parent), str(native.parent)])
    )
    monkeypatch.setattr(github_cli, "_bundled_helper", lambda _name: None)

    result = github_cli.ensure_proxy(directory=root, real_gh=real_gh)

    assert os.path.realpath(root / "fno-gh-proxy") == os.path.realpath(native)
    assert result.changed
    assert result.proxy == root / "gh"
