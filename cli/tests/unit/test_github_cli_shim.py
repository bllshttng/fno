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
    # A durable shim never pins a temp-dir binary; it degrades instead.
    monkeypatch.setattr(github_cli, "_in_temp", lambda path: path == agents)
    durable = github_cli.ensure_proxy(directory=tmp_path / "durable", real_gh=real_gh)
    assert durable.proxy == real_gh and not (tmp_path / "durable" / "gh").exists()


def test_a_worker_refreshes_an_inherited_old_shim_in_place(tmp_path, monkeypatch):
    root = tmp_path / "github-cli"
    agents = _exe(tmp_path / "bin" / "fno-agents", "#!/bin/sh\nexit 0\n")
    real_gh = _exe(tmp_path / "real" / "gh", "#!/bin/sh\nexit 0\n")
    _exe(root / "gh", '#!/bin/sh\nexec fno-gh-proxy "$@"\n')
    monkeypatch.setattr(github_cli, "resolve_binary", lambda: agents)
    path = f"{root}:{real_gh.parent}"

    env = github_cli.worker_environment({"PATH": path, "FNO_GH_PROXY_DIR": str(root)})

    assert env["FNO_GH_PROXY_DIR"] == str(root.resolve())
    assert (root / "gh").read_text() == f"#!/bin/sh\nexec {agents} gh \"$@\"\n"


def test_in_temp_judges_the_path_not_the_callers_tempdir():
    # Specimen 2026-10-28: a parity driver stripped TMPDIR from its own env,
    # so the writer's gettempdir() fell to /tmp, the /var/folders fixture
    # fake read as non-temp, and the durable shim was rewritten to exec it.
    # The verdict must ride the path's own spellings, never the caller's env.
    fixture = Path("/var/folders/x5/T/tmp.ABCDEF/fake_bin/fno-agents")
    assert github_cli._in_temp(fixture)
    assert github_cli._in_temp(fixture.as_posix().replace("/var/", "/private/var/", 1))
    assert not github_cli._in_temp(Path("/tmpextra/fake_bin/fno-agents"))
    assert not github_cli._in_temp(Path("/Users/bb/repo/target/debug/fno-agents"))


def test_ensure_proxy_refuses_a_binary_that_no_longer_exists(tmp_path, monkeypatch):
    root = tmp_path / "github-cli"
    real_gh = _exe(tmp_path / "real" / "gh", "#!/bin/sh\nexit 0\n")
    agents = _exe(tmp_path / "fake_bin" / "fno-agents", "#!/bin/sh\nexit 0\n")
    monkeypatch.setattr(github_cli, "resolve_binary", lambda: agents)
    agents.unlink()

    result = github_cli.ensure_proxy(directory=root, real_gh=real_gh)

    assert not result.changed
    assert not (root / "gh").exists()
