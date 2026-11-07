"""Shared pytest fixtures for the dev-only Python tests.

These tests check hooks, skill scripts and CI scripts that stay in the repo.
They shell the shipped binaries and never import a runtime package. Run them
with: uv run --with pytest --with pytest-timeout --with pyyaml --with psutil pytest tests/py
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import warnings
from pathlib import Path

import pytest

from _bins import find_dev_binary, resolve_binary

REPO_ROOT = Path(__file__).resolve().parents[2]


@pytest.fixture(autouse=True)
def _no_send_settle_wait(monkeypatch):
    """The post-send landed verify reads immediately under test."""
    monkeypatch.setenv("FNO_MAIL_LANDED_SETTLE_S", "0")


@pytest.fixture(autouse=True)
def _operator_shell_identity(monkeypatch):
    """Run every test as a bare operator shell, never as a spawned agent.

    A test process inherits the harness markers of the session that ran
    pytest, so an identity-gated refusal passes in CI and fires on a
    developer's machine. The whole ambient marker set goes.
    """
    import tomllib

    vocab = REPO_ROOT / "crates/fno-agents/src/ambient_vocab.toml"
    markers = tomllib.loads(vocab.read_text(encoding="utf-8"))["identity_markers"]
    for name in markers:
        monkeypatch.delenv(name, raising=False)
    monkeypatch.delenv("FNO_CLAUDE_THEME", raising=False)


@pytest.fixture(autouse=True)
def _sandbox_project_spaces(tmp_path, monkeypatch):
    """Keep project space state out of the developer's real ~/.fno/spaces."""
    monkeypatch.setenv("FNO_SPACES_DIR", str(tmp_path / "spaces"))


_SERIAL_TEST_FILES = frozenset({"hooks/test_init_target_state_skip_flags.py"})


def _parse_pytest_shard(spec: str) -> tuple[int, int]:
    try:
        index_text, total_text = spec.split("/", 1)
        index, total = int(index_text), int(total_text)
    except (ValueError, TypeError):
        raise pytest.UsageError(
            f"FNO_PYTEST_SHARD must be I/N with 1 <= I <= N, got {spec!r}"
        ) from None
    if total < 1 or index < 1 or index > total:
        raise pytest.UsageError(
            f"FNO_PYTEST_SHARD must be I/N with 1 <= I <= N, got {spec!r}"
        )
    return index, total


def partition_pytest_nodeids(nodeids: list[str], spec: str) -> list[str]:
    """Return the sorted nodeids assigned to one ``I/N`` pytest shard."""
    index, total = _parse_pytest_shard(spec)
    ordered = sorted(nodeids)
    selected = [n for position, n in enumerate(ordered) if position % total == index - 1]
    if not selected:
        raise pytest.UsageError(
            f"FNO_PYTEST_SHARD={spec} selected no tests from {len(nodeids)} collected"
        )
    return selected


@pytest.hookimpl(tryfirst=True)
def pytest_collection_modifyitems(items: list[pytest.Item]) -> None:
    """Keep filed parallel racers on one worker, skip dev-build tests where
    this checkout has no build, and take one I/N shard when asked."""
    no_build = find_dev_binary() is None
    skip_no_dev_build = pytest.mark.skip(
        reason="no fno-agents dev build (cargo build --manifest-path crates/fno-agents/Cargo.toml)"
    )
    for item in items:
        nodeid = item.nodeid.replace("\\", "/")
        if nodeid.split("::", 1)[0] in _SERIAL_TEST_FILES:
            item.add_marker(pytest.mark.serial)
            item.add_marker(pytest.mark.xdist_group(name="serial"))
        if "native_backlog_door" in getattr(item, "fixturenames", ()):
            item.add_marker(pytest.mark.dev_build)
        if item.get_closest_marker("dev_build") is not None and no_build:
            item.add_marker(skip_no_dev_build)
    spec = os.environ.get("FNO_PYTEST_SHARD", "").strip()
    if spec:
        keep = set(partition_pytest_nodeids([item.nodeid for item in items], spec))
        items[:] = [item for item in items if item.nodeid in keep]
        # Nested pytest probes must collect their own target, not this slice.
        os.environ.pop("FNO_PYTEST_SHARD", None)


def _pytest_of_ancestor(basetemp) -> Path | None:
    current = Path(basetemp)
    for candidate in (current, *current.parents):
        if candidate.name.startswith("pytest-of-"):
            return candidate
    return None


@pytest.fixture(autouse=True, scope="session")
def _reap_session_processes(tmp_path_factory):
    """Every process this session rooted in its tmp tree dies with it.

    Store keepers the binaries spawn are immortal by design, and a provider
    daemon can setsid away from every group cleanup. Setup reaps leaks of
    earlier runs and warns; teardown reaps this session's own tree and
    asserts nothing stayed rooted there.
    """
    os.environ.setdefault("FNO_STORE_KEEPER_IDLE_SECS", "5")
    from _leak_census import census_rooted, reap_rooted

    basetemp = tmp_path_factory.getbasetemp()
    ancestor = _pytest_of_ancestor(basetemp)
    if ancestor is not None:
        for hit in reap_rooted([str(ancestor)], match_component="garbage-"):
            warnings.warn(
                pytest.PytestWarning(
                    f"reaped a process leaked by an EARLIER pytest run: "
                    f"pid {hit['pid']} rooted at {hit['cwd']}"
                ),
                stacklevel=1,
            )
    yield
    grace_end = time.monotonic() + float(os.environ["FNO_STORE_KEEPER_IDLE_SECS"]) + 3.0
    while census_rooted([str(basetemp)]) and time.monotonic() < grace_end:
        time.sleep(0.5)
    rooted = reap_rooted([str(basetemp)], reaper=os.getpid())
    assert not rooted, (
        f"{len(rooted)} process tree(s) stayed rooted in this session's tmp tree: "
        + "; ".join(f"pid {r['pid']} at {r['cwd']}" for r in rooted[:10])
    )


@pytest.fixture(autouse=True)
def _stub_codex_cli_version(monkeypatch):
    """Never let a codex argv build reach the real ``codex --version``."""
    monkeypatch.setenv("FNO_CODEX_VERSION", "0.148.0")


@pytest.fixture(autouse=True)
def _block_live_provider_exec(request, monkeypatch, tmp_path_factory):
    """No test execs a real provider binary or mutates the real launchd domain.

    The discriminator is which binary runs: a fake on a tmp-isolated PATH may
    run; one that resolves outside the pytest tmp tree is blocked.
    """
    guard_providers = not request.node.get_closest_marker("smoke")
    tmp_root = str(tmp_path_factory.getbasetemp().resolve())
    provider_bins = {"claude", "codex", "pi", "grok", "kimi", "dsh", "agy", "cursor-agent"}
    mutating = {"bootstrap", "bootout", "load", "unload", "kickstart", "enable",
                "disable", "remove", "submit", "start", "stop"}

    def _outside_tmp(argv0: str) -> bool:
        resolved = argv0 if Path(argv0).is_absolute() else (shutil.which(argv0) or "")
        return not resolved or not str(Path(resolved).resolve()).startswith(tmp_root)

    class _GuardedPopen(subprocess.Popen):
        def __init__(self, args, *popenargs, **kwargs):
            argv = list(args) if isinstance(args, (list, tuple)) else [args]
            argv0 = str(argv[0]) if argv else ""
            name = Path(argv0).name
            if (name == "launchctl" and len(argv) > 1 and str(argv[1]) in mutating
                    and _outside_tmp(argv0)):
                raise AssertionError(
                    f"live launchctl mutation blocked under pytest in {request.node.nodeid}"
                )
            if guard_providers and name in provider_bins and _outside_tmp(argv0):
                raise AssertionError(
                    f"live provider exec blocked under pytest in {request.node.nodeid}: "
                    f"{argv0!r}. Install a fake on a tmp-isolated PATH."
                )
            super().__init__(args, *popenargs, **kwargs)

    monkeypatch.setattr(subprocess, "Popen", _GuardedPopen)


@pytest.fixture(autouse=True)
def _hermetic_fleet_incident_home(monkeypatch, tmp_path):
    """Default every test to an empty fleet-incident home."""
    monkeypatch.setenv("FNO_AGENTS_HOME", str(tmp_path / ".fno" / "agents"))


@pytest.fixture(autouse=True)
def _hermetic_worker_add_dirs(monkeypatch):
    """Isolate the seam-published writable-dir grant per test."""
    monkeypatch.delenv("FNO_WORKER_ADD_DIRS", raising=False)


_PLAN_JOURNAL_PINNED_MODULES = frozenset({"test_epic_status"})


@pytest.fixture(autouse=True)
def _plan_hermetic_events_journal(request, tmp_path, monkeypatch):
    """Pin FNO_EVENTS_PATH to this test's tmp journal for the pinned modules."""
    if request.module.__name__.rsplit(".", 1)[-1] in _PLAN_JOURNAL_PINNED_MODULES:
        monkeypatch.setenv("FNO_EVENTS_PATH", str(tmp_path / "events.jsonl"))


@pytest.fixture(autouse=True)
def _door_binary_from_this_checkout(monkeypatch):
    """Pin door-routed commands to this checkout's fno-agents build, when
    one exists; an operator's FNO_AGENTS_BIN always wins."""
    pinned = (os.environ.get("FNO_AGENTS_BIN") or "").strip()
    if not (pinned and Path(pinned).is_file()):
        found = find_dev_binary()
        if found is not None:
            monkeypatch.setenv("FNO_AGENTS_BIN", str(found))
    pinned_worker = (os.environ.get("FNO_AGENTS_WORKER") or "").strip()
    if not (pinned_worker and Path(pinned_worker).is_file()):
        worker = find_dev_binary("fno-agents-worker")
        if worker is not None:
            monkeypatch.setenv("FNO_AGENTS_WORKER", str(worker))


@pytest.fixture
def cwd_tmp(tmp_path: Path, monkeypatch):
    """Run in a disposable tmp dir that is also HOME, with no claims-root pin."""
    monkeypatch.chdir(tmp_path)
    monkeypatch.delenv("FNO_CLAIMS_ROOT", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))
    yield tmp_path


@pytest.fixture
def native_backlog_door(monkeypatch):
    """Pin the native door to THIS checkout's build; skip when there is none."""
    binary = find_dev_binary()
    if binary is None:
        pytest.skip("no fno-agents dev build (cargo build -p fno-agents)")
    monkeypatch.setenv("FNO_AGENTS_BIN", str(binary))


# ---------------------------------------------------------------------------
# Hermetic state isolation
# ---------------------------------------------------------------------------
# Applied at module load, before any test module imports: the native
# `fno-agents test-run env` assembles the neutralised env, the same env
# `fno doctor test` applies in-process. A run with no binary refuses, unless
# the env is already fenced (the outer runner set FNO_TEST_HERMETIC=1).

_SANDBOX = tempfile.mkdtemp(prefix="fno-test-sandbox-")

_TRIPWIRE = None
if not os.environ.get("PYTEST_XDIST_WORKER"):
    from prod_tripwire import live_roots, resolve_real_plans_dir, snapshot

    _real_env = dict(os.environ)
    _real_home = _real_env.get("HOME")
    _excludes = {str(Path(_SANDBOX).resolve())}
    if _real_env.get("FNO_TEST_SANDBOX"):
        _excludes.add(str(Path(_real_env["FNO_TEST_SANDBOX"]).resolve()))
    _roots = (
        live_roots(
            Path(_real_home),
            REPO_ROOT,
            resolve_real_plans_dir(_real_env, REPO_ROOT),
            exclude=frozenset(_excludes),
        )
        if _real_home
        else []
    )
    _TRIPWIRE = (_roots, snapshot(_roots))


def _hermetic_env(sandbox: Path) -> dict:
    env = dict(os.environ)
    binary = resolve_binary()
    if binary is None:
        if env.get("FNO_TEST_HERMETIC") == "1":
            return env
        sys.stderr.write(
            "conftest: no fno-agents binary on FNO_AGENTS_BIN, PATH or this checkout's "
            "target/; the hermetic env is native. Build it with cargo build -p fno-agents.\n"
        )
        raise SystemExit(3)
    payload = {"mode": "neutralise", "env": env, "sandbox": str(sandbox)}
    proc = subprocess.run([str(binary), "test-run", "env", json.dumps(payload)],
                          capture_output=True, text=True)
    if proc.returncode != 0:
        if env.get("FNO_TEST_HERMETIC") == "1":
            return env
        sys.stderr.write(proc.stderr)
        raise SystemExit(3)
    return {str(k): str(v) for k, v in json.loads(proc.stdout)["env"].items()}


_ENV = _hermetic_env(Path(_SANDBOX))
os.environ.clear()
os.environ.update(_ENV)


@pytest.fixture(autouse=True, scope="session")
def _config_search_ceiling(tmp_path_factory: pytest.TempPathFactory):
    """Widen the config ceiling to include the pytest basetemp."""
    basetemp = str(tmp_path_factory.getbasetemp())
    previous = os.environ.get("FNO_CONFIG_SEARCH_ROOT", "")
    os.environ["FNO_CONFIG_SEARCH_ROOT"] = os.pathsep.join(
        [basetemp, previous] if previous else [basetemp]
    )
    yield
    os.environ["FNO_CONFIG_SEARCH_ROOT"] = previous


def pytest_sessionfinish(session: pytest.Session, exitstatus: int) -> None:
    """Fail the run on a write outside the sandbox, then remove the sandbox."""
    if _TRIPWIRE is not None:
        from prod_tripwire import find_leaks

        roots, before = _TRIPWIRE
        markers: set[str] = {str(_SANDBOX), os.path.realpath(_SANDBOX)}
        try:
            basetemp = Path(session.config._tmp_path_factory.getbasetemp())
            markers |= {str(basetemp), os.path.realpath(basetemp)}
        except Exception:
            print("prod tripwire: could not read the session basetemp", file=sys.stderr)
        leaks = find_leaks(before, roots, markers)
        for leak in leaks:
            print(f"prod tripwire: {leak} was created by this test session", file=sys.stderr)
        if leaks:
            session.exitstatus = pytest.ExitCode.TESTS_FAILED
    shutil.rmtree(_SANDBOX, ignore_errors=True)
