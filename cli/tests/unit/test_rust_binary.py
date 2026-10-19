"""Binary-locator guards for ``fno.rust_binary``.

Two things this module can get wrong silently, both covered here:

* The ``__file__``-relative depths. The locator moved up one package level out
  of ``fno/agents/``, so ``_bundled_binary``'s ``parent`` and
  ``_cargo_dev_binary``'s ``parents[3]`` both shifted. A wrong depth returns
  ``None`` on every install instead of raising, so nothing else would notice.
* ``$FNO_AGENTS_BIN``. Both "binary not found" messages tell the operator to
  set it, and for a long time the Python resolver did not read it.
"""

from __future__ import annotations

import os
import stat
from pathlib import Path

from fno import rust_binary


def _make_exe(path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("#!/bin/sh\nexit 0\n")
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return path


def _fake_checkout(tmp_path: Path, monkeypatch) -> Path:
    """Mirror the real layout and point the module's ``__file__`` at it.

    Drives the locator's own depth arithmetic instead of restating it, so a
    wrong ``parent`` / ``parents[N]`` fails here rather than passing a test that
    recomputed the same constant.
    """
    root = tmp_path / "checkout"
    pkg = root / "cli" / "src" / "fno"
    pkg.mkdir(parents=True)
    (root / "crates").mkdir()
    monkeypatch.setattr(rust_binary, "__file__", str(pkg / "rust_binary.py"))
    return root


def test_bundled_lookup_reads_the_package_bin_dir(tmp_path, monkeypatch):
    root = _fake_checkout(tmp_path, monkeypatch)
    bundled = _make_exe(root / "cli" / "src" / "fno" / "_bin" / rust_binary.BINARY_NAME)
    assert rust_binary._bundled_binary() == bundled


def test_cargo_dev_lookup_prefers_the_newest_build(tmp_path, monkeypatch):
    """A fresh release must beat a stale debug; the mtimes inverted, a fresh
    debug must win. Never the profile name."""
    root = _fake_checkout(tmp_path, monkeypatch)
    debug = _make_exe(root / "crates" / "fno-agents" / "target" / "debug" / rust_binary.BINARY_NAME)
    release = _make_exe(root / "crates" / "fno-agents" / "target" / "release" / rust_binary.BINARY_NAME)
    os.utime(debug, ns=(1_000_000_000, 1_000_000_000))
    assert rust_binary._cargo_dev_binary() == release
    os.utime(release, ns=(0, 0))
    assert rust_binary._cargo_dev_binary() == debug


def test_worker_binary_lets_path_compete_on_freshness(tmp_path, monkeypatch):
    """The store's resolver used to answer the first checkout artifact and
    never reached PATH; a stale debug build beat a fresh installed one."""
    from fno.graph import store

    root = _fake_checkout(tmp_path, monkeypatch)
    (root / "crates" / "fno-agents").mkdir()
    stale = _make_exe(root / "crates" / "fno-agents" / "target" / "debug" / "fno-agents-worker")
    os.utime(stale, ns=(0, 0))
    fresh = _make_exe(tmp_path / "onpath" / "fno-agents-worker")
    monkeypatch.delenv("FNO_AGENTS_WORKER", raising=False)
    monkeypatch.delenv("FNO_AGENTS_FRONT", raising=False)
    monkeypatch.setattr(store.shutil, "which", lambda name: str(fresh))
    assert store._worker_binary() == fresh


def test_env_override_outranks_the_search(tmp_path, monkeypatch):
    override = _make_exe(tmp_path / "override" / rust_binary.BINARY_NAME)
    other = _make_exe(tmp_path / "onpath" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("PATH", str(other.parent))
    monkeypatch.setenv(rust_binary.BINARY_ENV, str(override))
    assert rust_binary.resolve_binary() == override


def test_front_env_is_honored_when_path_has_none(tmp_path, monkeypatch):
    """The smoke lanes export only FNO_AGENTS_FRONT for the built binary.

    The footprint door resolves through resolve_binary, so a lane that built
    the binary and exported the FRONT name must stay readable; a stale FRONT
    export must still lose to a fresh install on PATH."""
    front = _make_exe(tmp_path / "built" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("FNO_AGENTS_FRONT", str(front))
    monkeypatch.delenv(rust_binary.BINARY_ENV, raising=False)
    monkeypatch.setattr(rust_binary, "_bundled_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_sibling_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_cargo_dev_binary", lambda: None)
    monkeypatch.delenv("PATH", raising=False)
    assert rust_binary.resolve_binary() == front

    on_path = _make_exe(tmp_path / "onpath" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("PATH", str(on_path.parent))
    assert rust_binary.resolve_binary() == on_path

    monkeypatch.delenv("PATH", raising=False)
    for bad in ("", "   ", str(tmp_path / "gone")):
        monkeypatch.setenv("FNO_AGENTS_FRONT", bad)
        # Every other finder is stubbed out, so a junk FRONT is a fall-through
        # to nothing: None, never the stale value.
        assert rust_binary.resolve_binary() is None, f"junk FRONT won for {bad!r}"


def test_unusable_env_override_falls_through(tmp_path, monkeypatch):
    """A stale export must not make an installed binary unreachable."""
    on_path = _make_exe(tmp_path / "onpath" / rust_binary.BINARY_NAME)
    monkeypatch.setenv("PATH", str(on_path.parent))
    monkeypatch.setattr(rust_binary, "_bundled_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_sibling_binary", lambda: None)

    for bad in ("", "   ", str(tmp_path / "does-not-exist")):
        monkeypatch.setenv(rust_binary.BINARY_ENV, bad)
        assert rust_binary.resolve_binary() == on_path, f"failed to fall through for {bad!r}"

    # Present but not executable: also a fall-through, not a hard stop.
    not_exec = tmp_path / "plain" / rust_binary.BINARY_NAME
    not_exec.parent.mkdir(parents=True, exist_ok=True)
    not_exec.write_text("not runnable")
    not_exec.chmod(not_exec.stat().st_mode & ~stat.S_IXUSR & ~stat.S_IXGRP & ~stat.S_IXOTH)
    monkeypatch.setenv(rust_binary.BINARY_ENV, str(not_exec))
    assert rust_binary.resolve_binary() == on_path


def test_installed_lookup_ignores_the_env_override(tmp_path, monkeypatch):
    """resolve_installed_binary decides the DEFAULT runtime, so it stays deaf
    to the override; FNO_AGENTS_RUNTIME=rust is the opt-in."""
    binary = _make_exe(tmp_path / "custom" / rust_binary.BINARY_NAME)
    monkeypatch.setenv(rust_binary.BINARY_ENV, str(binary))
    monkeypatch.setenv("PATH", str(tmp_path / "empty"))
    monkeypatch.setattr(rust_binary, "_bundled_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_sibling_binary", lambda: None)
    assert rust_binary.resolve_installed_binary() is None
    assert os.environ[rust_binary.BINARY_ENV] == str(binary)


def test_installed_resolve_excludes_cargo_dev(monkeypatch, tmp_path) -> None:
    """A cargo dev artifact must NOT satisfy the installed-only resolver: a dev
    checkout stays on Python by default (the test process is never replaced)."""
    dev = _make_exe(tmp_path / "target" / "release" / rust_binary.BINARY_NAME)
    monkeypatch.setattr(rust_binary, "_bundled_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_sibling_binary", lambda: None)
    monkeypatch.setattr(rust_binary, "_path_binary", lambda: None)
    # Even if the cargo dev finder would resolve, the installed-only path ignores it.
    monkeypatch.setattr(rust_binary, "_cargo_dev_binary", lambda: dev)
    assert rust_binary.resolve_installed_binary() is None


def test_verb_call_attaches_returncode(monkeypatch) -> None:
    """A clean non-zero exit carries its code as a structured field.

    The gh budget's admit gate refuses on a signalled reader (negative code or
    137) and admits on a clean failure, so the code must be readable as data,
    not only as prose in the message.
    """
    import subprocess

    import pytest

    from fno.rust_binary import VerbUnavailable

    proc = subprocess.CompletedProcess(
        ["fno-agents", "fleet-incident"], returncode=1, stdout="", stderr="no ledger"
    )
    monkeypatch.setattr(rust_binary, "find_dev_binary", lambda: "/dev/null/fno-agents")
    monkeypatch.setattr(subprocess, "run", lambda *a, **k: proc)
    with pytest.raises(VerbUnavailable) as excinfo:
        rust_binary.verb_call("fleet-incident", {})
    assert excinfo.value.returncode == 1
    assert "exited 1" in str(excinfo.value)


def test_delivered_terminals_reads_once_and_refuses_closed(monkeypatch) -> None:
    """The vocabulary door resolves once per process and names the remedy on
    a failed read, like the roster door it mirrors.

    Folded under the test-delta declaration cap: one function carries the
    three contracts (content, cache identity, fail-closed refusal)."""
    import pytest

    from fno.rust_binary import VerbUnavailable

    monkeypatch.setattr(
        rust_binary,
        "call_binary_json",
        lambda verb, **k: (None, {"delivered": ["DonePRGreen", "DoneBatched"]}),
    )
    rust_binary.delivered_terminals.cache_clear()
    assert rust_binary.delivered_terminals() == frozenset({"DonePRGreen", "DoneBatched"})
    assert rust_binary.delivered_terminals() is rust_binary.delivered_terminals()

    monkeypatch.setattr(
        rust_binary,
        "call_binary_json",
        lambda verb, **k: ("fno-agents binary not found", None),
    )
    rust_binary.delivered_terminals.cache_clear()
    with pytest.raises(VerbUnavailable, match="fno doctor update"):
        rust_binary.delivered_terminals()
    rust_binary.delivered_terminals.cache_clear()


def test_a_silent_law_door_raises_a_named_timeout_not_a_raw_one(monkeypatch):
    import subprocess

    import pytest

    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: "/bin/fno")

    def hang(*args, **kwargs):
        raise subprocess.TimeoutExpired(args[0], kwargs["timeout"])

    monkeypatch.setattr(subprocess, "run", hang)
    with pytest.raises(rust_binary.FrontTimeout) as caught:
        rust_binary.call_front_json({"mode": "decisions"}, timeout=60)
    assert isinstance(caught.value, rust_binary.VerbUnavailable)
    assert "gave no answer in 60s (machine load" in str(caught.value)


def test_law_door_timeout_knob_feeds_the_default_bound_only(monkeypatch):
    """FNO_LAW_DOOR_TIMEOUT_SECONDS raises the DEFAULT bound; an explicit
    timeout= argument outranks it, and junk falls back to 60.

    The validator's law read measured 61s wall at load 80 against the fixed
    60s bound, wedging every blueprint on a loaded machine.
    """
    import subprocess

    seen = {}
    proc = subprocess.CompletedProcess(
        ["fno", "inbox", "law", "match"], returncode=0, stdout='{"ok":true}', stderr=""
    )

    def run(cmd, **kwargs):
        seen["timeout"] = kwargs.get("timeout")
        return proc

    monkeypatch.setattr(rust_binary, "resolve_front_binary", lambda: "/bin/fno")
    monkeypatch.setattr(subprocess, "run", run)

    monkeypatch.delenv(rust_binary.LAW_DOOR_TIMEOUT_ENV, raising=False)
    rust_binary.call_front_json({})
    assert seen["timeout"] == 60.0

    monkeypatch.setenv(rust_binary.LAW_DOOR_TIMEOUT_ENV, "240")
    rust_binary.call_front_json({})
    assert seen["timeout"] == 240.0

    rust_binary.call_front_json({}, timeout=5)
    assert seen["timeout"] == 5

    for bad in ("", "   ", "abc"):
        monkeypatch.setenv(rust_binary.LAW_DOOR_TIMEOUT_ENV, bad)
        rust_binary.call_front_json({})
        assert seen["timeout"] == 60.0, f"junk knob {bad!r} did not fall back"
