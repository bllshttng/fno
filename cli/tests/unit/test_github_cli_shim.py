"""The gh shim execs ``fno-agents gh`` by absolute path, with no helper link."""
from __future__ import annotations

from pathlib import Path

from fno.setup import github_cli


def _exe(path: Path, body: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(body)
    path.chmod(0o755)
    return path


def test_an_old_helper_shim_is_rewritten_to_exec_fno_agents(tmp_path, monkeypatch):
    # Specimen 2026-10-22: a reinstall without the bundled helper binaries
    # left the shim exec'ing a fno-gh-proxy that no longer existed.
    root = tmp_path / "github-cli"
    agents = _exe(tmp_path / "bin dir" / "fno-agents", "#!/bin/sh\nexit 0\n")
    real_gh = _exe(tmp_path / "real" / "gh", "#!/bin/sh\nexit 0\n")
    _exe(root / "gh", '#!/bin/sh\nexec fno-gh-proxy "$@"\n')
    (root / "fno-gh-proxy").symlink_to(tmp_path / "gone")
    monkeypatch.setattr(github_cli, "resolve_binary", lambda: agents)

    result = github_cli.ensure_proxy(directory=root, real_gh=real_gh)

    assert result.changed and result.backup is None
    assert (root / "gh").read_text() == f"#!/bin/sh\nexec '{agents}' gh \"$@\"\n"
    assert github_cli._is_wrapper(root / "gh")
    assert not (root / "fno-gh-proxy").is_symlink()
    assert not github_cli.ensure_proxy(directory=root, real_gh=real_gh).changed
