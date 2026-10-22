"""Install the dedicated `gh` proxy used by Footnote worker environments."""
from __future__ import annotations

import os
import shlex
import shutil
import stat
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Mapping, Optional

from fno.paths import github_cli_proxy_dir
from fno.rust_binary import resolve_binary

# The proxy is the `gh` subcommand of fno-agents, exec'd by absolute path, so
# no PATH entry or helper binary stands between the shim and it. A reinstall
# that dropped the old fno-gh-proxy helper broke gh in every session.
# The native broker still recognizes the older shim shapes on disk.
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


def _wrapper(binary: Path) -> str:
    return f'#!/bin/sh\nexec {shlex.quote(str(binary))} gh "$@"\n'


def _in_temp(path: Path) -> bool:
    temp = os.path.realpath(tempfile.gettempdir())
    return os.path.realpath(path).startswith(temp + os.sep)


def _read_text(path: Path) -> Optional[str]:
    # A stock CI runner's gh is a compiled binary, not utf-8 text.
    try:
        return path.read_text()
    except (OSError, UnicodeDecodeError):
        return None


def _is_wrapper(path: Path) -> bool:
    """Any shim this installer wrote, current or from an older install."""
    text = _read_text(path)
    if text == '#!/bin/sh\nexec fno-gh-proxy "$@"\n':
        return True
    if text is None or not text.startswith("#!/bin/sh\nexec "):
        return False
    try:
        words = shlex.split(text.splitlines()[1])
    except (IndexError, ValueError):
        return False
    return len(words) == 4 and Path(words[1]).name == "fno-agents" and words[2:] == ["gh", "$@"]


def ensure_proxy(
    *,
    directory: Optional[Path] = None,
    real_gh: Optional[Path] = None,
    which: Callable[[str], Optional[str]] = shutil.which,
) -> InstallResult:
    resolved = real_gh or (Path(found) if (found := which("gh")) else None)
    if resolved is None:
        raise FileNotFoundError("real gh executable not found on PATH")
    binary = resolve_binary()
    if binary is None:
        # No fno-agents to exec, so the shim would strand every gh call.
        # Degrade: leave the real gh unswrapped and let the quota policy
        # stand down for this process tree.
        return InstallResult(proxy=resolved, delegate=resolved, changed=False)
    root = directory or github_cli_proxy_dir()
    if _in_temp(binary) and not _in_temp(root):
        # A durable shim must not pin a binary a temp sweep deletes: links
        # into a reaped mktemp dir once broke every gh read on the machine.
        return InstallResult(proxy=resolved, delegate=resolved, changed=False)
    wrapper = _wrapper(Path(os.path.abspath(binary)))
    root.mkdir(parents=True, exist_ok=True)
    # Retired: the old shim exec'd this link by PATH name.
    (root / "fno-gh-proxy").unlink(missing_ok=True)
    proxy = root / "gh"
    resolved = resolved.resolve()
    if resolved == proxy.resolve():
        raise RuntimeError("resolved gh delegate points back to the Footnote proxy")

    backup = None
    changed = _read_text(proxy) != wrapper
    if changed:
        if proxy.exists() and not _is_wrapper(proxy):
            backup = proxy.with_name("gh.pre-fno")
            if not backup.exists():
                shutil.copy2(proxy, backup)
        fd, raw_tmp = tempfile.mkstemp(prefix=".gh.", dir=root)
        try:
            with os.fdopen(fd, "w") as handle:
                handle.write(wrapper)
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
            env[_PROXY_DIR_ENV] = str(parent)
            env.pop("FNO_REAL_GH", None)
            return env
    else:
        if resolve_binary() is None:
            # No fno-agents: no shim install and no PATH prepend. The children
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
