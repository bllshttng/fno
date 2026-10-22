"""Install the dedicated `gh` proxy used by Footnote worker environments."""
from __future__ import annotations

import os
import shutil
import stat
import sys
import tempfile
from contextlib import suppress
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Mapping, Optional

from fno.paths import github_cli_proxy_dir

PROXY_EXEC_LINE = 'exec fno-gh-proxy "$@"'
# A shim can also take the pip/uv console-script shape: an import line for the
# Python entry point retired by pr port 7, with no exec line, so it read as a
# real binary. On 2026-09-07 `~/.local/bin/gh` was one, and every gh read died
# on the re-entry refusal. The native broker still recognizes that retired
# shape on disk; this installer only ever writes the exec-line form.
_WRAPPER = f"#!/bin/sh\n{PROXY_EXEC_LINE}\n"
_PROXY_DIR_ENV = "FNO_GH_PROXY_DIR"
# The proxy stamps this into the env it hands `os.execve`, so its own successor
# can recognize itself. It is good for that one hop only. Anything else building
# a gh environment drops it, or a descendant inherits a marker it never earned
# and the proxy refuses a first-and-only entry.
PROXY_DEPTH_ENV = "FNO_GH_PROXY_DEPTH"
_WHICH = shutil.which


@dataclass(frozen=True)
class InstallResult:
    proxy: Path
    delegate: Path
    changed: bool
    backup: Optional[Path] = None


def fallback_proxy_dir() -> Path:
    """Config-free proxy home for commands whose config cannot be loaded."""
    uid = os.getuid() if hasattr(os, "getuid") else os.getpid()
    return Path(tempfile.gettempdir()) / f"fno-{uid}" / "github-cli"


def _bundled_helper(name: str) -> Optional[Path]:
    """The wheel's co-located gh-transport binary (``fno/_bin/<name>``).

    The Rust spawners reach the same file as sibling-of-their-own-exe; the
    shim installer needs it spelled out, because a pip-only box has no
    ``fno-gh-proxy`` on PATH and no venv script sibling."""
    try:
        import fno
    except ImportError:
        return None
    candidate = Path(fno.__file__).with_name("_bin") / name
    return candidate if candidate.is_file() else None


def _is_retired_script(path: Path) -> bool:
    # The console script for the entry point pr port 7 retired. Its import
    # dies on any fno newer than its own interpreter's, which is the stale
    # dev-venv link that broke every gh read on 2026-10-22.
    try:
        with path.open("rb") as handle:
            return b"from fno.pr.gh_proxy import main" in handle.read(4096)
    except OSError:
        return False


def _resolve_helper(root: Path) -> Optional[Path]:
    """The ``fno-gh-proxy`` the shim's link points at: on PATH, co-located in
    the wheel's ``_bin``, or beside the interpreter. The PATH search skips
    ``root``, where the link itself would answer for its own stale target."""
    own = os.path.realpath(root)
    on_path = [
        Path(found)
        for part in os.environ.get("PATH", "").split(os.pathsep)
        if part and os.path.realpath(part) != own
        if (found := shutil.which("fno-gh-proxy", path=part))
    ]
    for candidate in (
        *on_path,
        _bundled_helper("fno-gh-proxy"),
        Path(sys.executable).with_name("fno-gh-proxy"),
    ):
        if candidate is not None and candidate.is_file() and not _is_retired_script(candidate):
            return candidate
    return None


def _helper_resolvable() -> bool:
    """True when a helper exists for the bare ``fno-gh-proxy`` name the
    wrapper execs. Without it the wrapper would strand every gh call behind
    a not-found exec, so the installers degrade and leave the real gh
    unswrapped."""
    return _resolve_helper(github_cli_proxy_dir()) is not None


def _link_helper(root: Path) -> bool:
    """Point ``root/fno-gh-proxy`` at the current helper. True when the link
    changed. A link left on an old target outlives the install it named, so
    a mismatch is replaced, never kept."""
    helper = _resolve_helper(root)
    if helper is None:
        return False
    link = root / "fno-gh-proxy"
    if link.is_symlink() and os.path.realpath(link) == os.path.realpath(helper):
        return False
    staged = root / f".fno-gh-proxy.{os.getpid()}"
    staged.unlink(missing_ok=True)
    staged.symlink_to(helper)
    os.replace(staged, link)
    return True


def _is_wrapper(path: Path) -> bool:
    # A stock CI runner's gh is a compiled binary, not utf-8 text.
    try:
        return path.read_text() == _WRAPPER
    except (OSError, UnicodeDecodeError):
        return False


def ensure_proxy(
    *,
    directory: Optional[Path] = None,
    real_gh: Optional[Path] = None,
    which: Callable[[str], Optional[str]] = shutil.which,
) -> InstallResult:
    resolved = real_gh or (Path(found) if (found := which("gh")) else None)
    if resolved is None:
        raise FileNotFoundError("real gh executable not found on PATH")
    root = directory or github_cli_proxy_dir()
    root.mkdir(parents=True, exist_ok=True)
    if _resolve_helper(root) is None:
        # Nothing provides the name the wrapper execs, so the install would
        # strand every gh call behind a not-found exec. Degrade: leave the
        # real gh unswrapped and let the quota policy stand down for this
        # process tree.
        return InstallResult(proxy=resolved, delegate=resolved, changed=False)
    relinked = _link_helper(root)
    proxy = root / "gh"
    resolved = resolved.resolve()
    if resolved == proxy.resolve():
        raise RuntimeError("resolved gh delegate points back to the Footnote proxy")

    backup = None
    rewrap = not proxy.exists() or not _is_wrapper(proxy)
    changed = rewrap or relinked
    if rewrap:
        if proxy.exists():
            backup = proxy.with_name("gh.pre-fno")
            if not backup.exists():
                shutil.copy2(proxy, backup)
        fd, raw_tmp = tempfile.mkstemp(prefix=".gh.", dir=root)
        try:
            with os.fdopen(fd, "w") as handle:
                handle.write(_WRAPPER)
                handle.flush()
                os.fsync(handle.fileno())
            tmp = Path(raw_tmp)
            tmp.chmod(tmp.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
            os.replace(tmp, proxy)
        finally:
            Path(raw_tmp).unlink(missing_ok=True)
    return InstallResult(proxy=proxy, delegate=resolved, changed=changed, backup=backup)


def worker_environment(base: Mapping[str, str]) -> dict[str, str]:
    inherited_delegate = base.get("FNO_REAL_GH")
    env = dict(base)
    # The identity scrub lives HERE, at the floor every adapter's child env
    # already crosses, not in each adapter: a per-adapter scrub is one the
    # next adapter declines to call (- claude's adapter scrubbed,
    # codex's never did, so a codex worker inherited its claude parent's
    # CLAUDE_CODE_SESSION_ID and stamped harness="claude-code" on every mail).
    # Before the no-gh early return: identity is not conditional on gh
    # presence. A spawned child inherits its parent's ROUTE but never its
    # parent's IDENTITY; each harness re-mints its own, so the scrub is
    # lossless. The pane (argv) substrate cannot cross this floor and strips
    # via ambient_identity_env_unset_args() - the same
    # AMBIENT_IDENTITY_ENV, so the shapes cannot drift.
    from fno.harness_identity import scrub_ambient_identity

    scrub_ambient_identity(env)
    # A child never inherits its parent's spawn cause.
    env.pop("FNO_SPAWN_TRIGGER", None)
    # Seed provenance is inherited the same way and for the same reason it must
    # not be (node): these fields name WHO SENT THIS CHILD ITS SEED, so a
    # child that inherits them attributes its own first message to whoever
    # seeded its parent - an envelope naming the wrong peer, which is worse than
    # none. Scrubbed at the floor rather than per adapter, so a launcher that
    # knows the seed SETS them after crossing it, and one that does not (a
    # headless one-shot) emits no sidecar instead of a wrong one.
    from fno.mail.seed_provenance import SEED_PROVENANCE_KEYS

    for _seed_key in SEED_PROVENANCE_KEYS:
        env.pop(_seed_key, None)
    # Dropped once, before any return: a worker inherits the marker from a
    # delegated gh, never earns it, and would have its first gh call refused.
    env.pop(PROXY_DEPTH_ENV, None)
    requested_dir = env.get(_PROXY_DIR_ENV)
    found = inherited_delegate or _WHICH("gh", path=env.get("PATH"))
    if not found:
        return env
    delegate = Path(found)
    try:
        inherited_shim = delegate.is_file() and _is_wrapper(delegate)
    except OSError:
        inherited_shim = False
    if inherited_shim:
        parent = delegate.parent.resolve()
        keep = bool(requested_dir) or parent != fallback_proxy_dir().resolve()
        if not keep:
            # The temp shim is reapable: move the lineage to the durable home,
            # and keep the temp one only when that home cannot be installed.
            try:
                result = ensure_proxy(real_gh=delegate)
            except Exception:
                keep = True
        if keep:
            with suppress(OSError):
                _link_helper(parent)
            env[_PROXY_DIR_ENV] = str(parent)
            env.pop("FNO_REAL_GH", None)
            return env
    else:
        if not _helper_resolvable():
            # No helper: no shim install and no PATH prepend. The children
            # resolve gh by plain PATH order and the quota policy stands
            # down for this process tree.
            return env
        try:
            result = ensure_proxy(
                directory=Path(requested_dir) if requested_dir else None,
                real_gh=delegate,
            )
        except Exception:
            if requested_dir:
                raise
            result = ensure_proxy(directory=fallback_proxy_dir(), real_gh=delegate)
    old_path = env.get("PATH", "")
    env["PATH"] = str(result.proxy.parent) + (os.pathsep + old_path if old_path else "")
    env[_PROXY_DIR_ENV] = str(result.proxy.parent.resolve())
    env.pop("FNO_REAL_GH", None)
    return env
