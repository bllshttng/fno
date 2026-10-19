"""Integration tests for skill discovery membership and source selection.

One intentional skill source per harness; unrelated local exposure curated
without deleting shared sources; discovery metadata diagnosed.

AC1-HP: installed plugin + aliases -> exactly one selected source, named.
AC2-HP: foreign links removed, shared source bytes and growth skills kept.
AC2-EDGE: user-owned dir preserved.
AC3-HP: load audit names the loaded path, digest and repair for a stale cache.

The real scripts are COPIED into a temp repo so ROOT_DIR resolves there and
the operator's own checkout is never touched. FNO_PYTHON points the scripts
at pytest's interpreter so the pyyaml-backed metadata checks are real.
"""
from __future__ import annotations

import hashlib
import os
import stat
import subprocess
import sys
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[3]
SCRIPTS = ["setup.sh", "doctor.sh", "preflight.sh", "ensure-global-dir.sh"]
LIBS = ["codex_utils.sh", "skill_discovery.py"]
DIAGNOSTICS = ["codex-skill-load-audit.py"]

GOOD_DESC = "description: Run a target end to end"


def _write_skill(skill_dir: Path, name: str, description: str) -> None:
    skill_dir.mkdir(parents=True)
    (skill_dir / "SKILL.md").write_text(
        f"---\nname: {name}\n{description}\n---\n\n# {name}\n"
    )


@pytest.fixture()
def repo(tmp_path: Path) -> Path:
    """A fixture checkout: copied scripts + a three-skill source tree."""
    root = tmp_path / "repo"
    (root / "scripts" / "lib").mkdir(parents=True)
    for s in SCRIPTS:
        (root / "scripts" / s).write_bytes((REPO_ROOT / "scripts" / s).read_bytes())
    for lib in LIBS:
        (root / "scripts" / "lib" / lib).write_bytes(
            (REPO_ROOT / "scripts" / "lib" / lib).read_bytes()
        )
    (root / "scripts" / "diagnostics").mkdir(parents=True, exist_ok=True)
    for d in DIAGNOSTICS:
        (root / "scripts" / "diagnostics" / d).write_bytes(
            (REPO_ROOT / "scripts" / "diagnostics" / d).read_bytes()
        )
    for name in ("target", "term", "growth-launch"):
        _write_skill(root / "skills" / name, name, GOOD_DESC)
    # doctor.sh requires the soft-hook scripts; stub them executable.
    hooks = root / "scripts" / "hooks"
    hooks.mkdir(exist_ok=True)
    for hook in ("session-start", "pre-compact", "pre-tool-use", "session-end"):
        (hooks / f"{hook}.sh").write_text("#!/bin/sh\nexit 0\n")
        (hooks / f"{hook}.sh").chmod(0o755)
    return root


@pytest.fixture()
def store(tmp_path: Path) -> Path:
    """A foreign skill-store outside the repo (the shared source)."""
    store = tmp_path / "store"
    for name in ("readyrule-inspect", "loci"):
        _write_skill(store / name, name, "description: Owned elsewhere")
    return store


@pytest.fixture()
def env(tmp_path: Path, store: Path) -> dict[str, str]:
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    codex_stub = bin_dir / "codex"
    codex_stub.write_text("#!/bin/sh\nexit 0\n")
    codex_stub.chmod(codex_stub.stat().st_mode | stat.S_IEXEC)
    # setup.sh probes `fno mux ls`; without a stub the probe fails and the
    # fixture repo (no scripts/install/) tries a live install.
    fno_stub = bin_dir / "fno"
    fno_stub.write_text("#!/bin/sh\nexit 0\n")
    fno_stub.chmod(fno_stub.stat().st_mode | stat.S_IEXEC)
    return {
        **os.environ,
        "HOME": str(tmp_path),
        "STATE_DIR": str(tmp_path / "fno-state"),
        "CODEX_PLUGIN_CACHE": str(tmp_path / "cache"),
        "FNO_PYTHON": sys.executable,
        "PATH": f"{bin_dir}{os.pathsep}{os.environ['PATH']}",
    }


def _link(root: Path, name: str, target: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    root.joinpath(name).symlink_to(target)


def _plugin_cache(tmp_path: Path, skills: dict[str, str], version: str = "0.3.2") -> Path:
    """A fake installed-plugin cache: footnote/fno/<version>/skills/."""
    cache = tmp_path / "cache" / "footnote" / "fno" / version
    (cache / "skills").mkdir(parents=True)
    (cache / "plugin.json").write_text('{\n  "name": "fno",\n  "skills": "./skills/"\n}\n')
    for name, desc in skills.items():
        _write_skill(cache / "skills" / name, name, desc)
    return cache.parent.parent.parent


def _digests(tree: Path) -> dict[str, str]:
    return {
        str(p.relative_to(tree)): hashlib.sha256(p.read_bytes()).hexdigest()
        for p in sorted(tree.rglob("SKILL.md"))
    }


def _run(repo: Path, env: dict, script: str, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", f"scripts/{script}", *args], cwd=repo, env=env,
        capture_output=True, text=True, timeout=120,
    )


def _sroot(repo: Path) -> Path:
    return repo / ".agents" / "skills"


# ---------------------------------------------------------------------------
# AC1-HP: one selected source when an installed plugin is present
# ---------------------------------------------------------------------------


def test_setup_auto_with_plugin_picks_one_source(repo: Path, env: dict, tmp_path: Path) -> None:
    _plugin_cache(tmp_path, {"target": "description: Stale packaged copy", "blueprint": GOOD_DESC})
    _link(_sroot(repo), "plugin--fno--target", repo / "skills" / "target")

    r = _run(repo, env, "setup.sh", "--provider", "codex")
    assert r.returncode == 0, r.stderr
    assert "0.3.2" in r.stdout and "single skill source" in r.stdout

    aliases = list(_sroot(repo).glob("plugin--fno--*"))
    assert aliases == [], f"source aliases must not be advertised: {aliases}"
    sys.path.insert(0, str(REPO_ROOT / "scripts" / "lib"))
    import skill_discovery as sd  # noqa: PLC0415
    rows = {r_.name: r_ for r_ in sd.inventory(repo, _sroot(repo), sd.find_installed_plugin(tmp_path / "cache"))}
    assert rows["target"].status == "ok"
    assert rows["blueprint"].status == "ok"
    assert rows["target"].alias is None
    # term is not shipped by the plugin: named, never silent.
    assert "[gap] term" in r.stdout


# ---------------------------------------------------------------------------
# AC2-HP: foreign exposure curated; shared bytes and growth skill kept
# ---------------------------------------------------------------------------


def test_curate_removes_foreign_links_keeps_source_bytes(repo: Path, env: dict, store: Path) -> None:
    before = _digests(store)
    _link(_sroot(repo), "readyrule-inspect", store / "readyrule-inspect")
    _link(_sroot(repo), "loci", store / "loci")
    user_dir = _sroot(repo) / "my-own-skill"
    _write_skill(user_dir, "my-own-skill", "description: A real user-owned directory")

    r = _run(repo, env, "setup.sh", "--provider", "codex")
    assert r.returncode == 0, r.stderr
    assert (user_dir / "SKILL.md").is_file()
    assert not (_sroot(repo) / "readyrule-inspect").exists()
    assert not (_sroot(repo) / "loci").exists()
    assert "restore: ln -sfn" in r.stdout
    # Shared source bytes untouched.
    assert _digests(store) == before
    # The footnote-owned growth pack stays exposed in development mode.
    assert (_sroot(repo) / "plugin--fno--growth-launch").resolve().is_dir()


# ---------------------------------------------------------------------------
# Doctor findings: broken / stale / duplicate (named, failing)
# ---------------------------------------------------------------------------


def test_doctor_names_broken_stale_duplicate(repo: Path, env: dict, tmp_path: Path) -> None:
    _plugin_cache(tmp_path, {"target": GOOD_DESC})
    _link(_sroot(repo), "plugin--fno--target", repo / "skills" / "target")  # duplicate
    ghost = tmp_path / "gone"
    ghost.mkdir()
    _link(_sroot(repo), "plugin--fno--ghost", ghost)  # stale: no SKILL.md inside
    (ghost / "SKILL.md").unlink(missing_ok=True)
    _link(_sroot(repo), "plugin--fno--dead", tmp_path / "never-existed")  # broken

    r = _run(repo, env, "doctor.sh")
    assert r.returncode == 1, r.stdout
    assert "[broken] dead" in r.stdout
    assert "[stale] ghost" in r.stdout
    assert "[duplicate] target" in r.stdout


# ---------------------------------------------------------------------------
# AC3-HP: the load audit names loaded path, digest and repair
# ---------------------------------------------------------------------------


def test_load_audit_discovery_names_stale_cache_and_repair(repo: Path, env: dict, tmp_path: Path) -> None:
    _plugin_cache(tmp_path, {"target": "description: Packaged copy from an older release"})
    _run(repo, env, "setup.sh", "--provider", "codex", "--skills-source", "development")

    r = subprocess.run(
        [sys.executable, "scripts/diagnostics/codex-skill-load-audit.py",
         "--discovery", "--repo", str(repo), "--skills-root", str(_sroot(repo)),
         "--plugin-cache", str(tmp_path / "cache")],
        cwd=repo, env=env, capture_output=True, text=True, timeout=120,
    )
    assert r.returncode == 0, r.stderr
    assert "stale-cache" in r.stdout
    assert str(tmp_path / "cache" / "footnote" / "fno") in r.stdout
    assert "reinstall" in r.stdout and "HARNESSES" in r.stdout
    assert "unmeasured" in r.stdout


# ---------------------------------------------------------------------------
# installed source with no usable plugin refuses before any removal
# ---------------------------------------------------------------------------


def test_installed_source_without_plugin_refuses_and_keeps_aliases(repo: Path, env: dict) -> None:
    _link(_sroot(repo), "plugin--fno--target", repo / "skills" / "target")
    r = _run(repo, env, "setup.sh", "--provider", "codex", "--skills-source", "installed")
    assert r.returncode == 1, r.stdout + r.stderr
    assert "no usable plugin" in r.stderr
    # The refusal must come BEFORE any alias removal.
    assert (_sroot(repo) / "plugin--fno--target").is_symlink()
