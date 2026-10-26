"""The hermetic child env: ambient classification, and the state-root fence.

The env builder (neutralise, poison, and the vocabulary they read) lives
native in crates/fno-agents/src/test_env.rs with the native test command. This
module keeps two duties that stayed Python:

- the state-root fence (declared_root / UndeclaredStateRootError), still
  Python beside the events gate that shares it;
- the classify service, reading the shared vocabulary TOML that build.rs
  byte-copies freshness-tripwired from the canonical copy beside the Rust
  module.

Characterization lives beside the Rust module (tests/golden/test_command/),
asserted by the Rust parity tests.
"""

from __future__ import annotations

import os
import sys
from pathlib import Path
from typing import Mapping, Optional

__all__ = [
    "AMBIENT_LEAK_CANARY",
    "UndeclaredStateRootError",
    "ambient_names",
    "classify",
    "declared_root",
]

# Every FNO_* and TARGET_* name is ambient until proven otherwise. This is
# the whole point: a 125th FNO_ var added to source neutralises without
# editing anything. TARGET_* earns its place the same way - a live /target
# session exports TARGET_INPUT / TARGET_PLAN_PATH / TARGET_SIZE and more,
# so a suite run from inside one inherits that session's parameters.
_AMBIENT_PREFIXES = ("FNO_", "TARGET_")

# Named in the shared vocabulary; the TOML is canonical beside the Rust
# module and byte-copied here by build.rs.
_AMBIENT_VOCAB_TOML = Path(__file__).with_name("ambient_vocab.toml")


def _vocab() -> dict:
    import tomllib

    with open(_AMBIENT_VOCAB_TOML, "rb") as fh:
        return tomllib.load(fh)


AMBIENT_LEAK_CANARY = "AMBIENT_LEAK_CANARY"



def _is_ambient(name: str) -> bool:
    v = _vocab()
    passthrough = set(v.get("runner_passthrough", [])) | set(v.get("environment", []))
    ambient = set(v.get("ambient_names", []))
    if name in passthrough:
        return False
    if name in ambient:
        return True
    return any(name.startswith(p) for p in _AMBIENT_PREFIXES)


def ambient_names(env: Optional[Mapping[str, str]] = None) -> tuple[str, ...]:
    """Names in env this module considers ambient. Sorted, for reporting."""
    src = os.environ if env is None else env
    return tuple(sorted(n for n in src if _is_ambient(n)))


def classify(name: str) -> str:
    """ambient, environment, or unclassified.

    test_ambient_surface.py walks every env read in source and fails on an
    unclassified result. That is the guard: a new env var cannot be added
    to source without someone deciding, in the shared vocabulary, whether
    a test may see the developer's value for it. Nobody has to remember;
    CI asks.
    """
    v = _vocab()
    environment = set(v.get("environment", []))
    passthrough = set(v.get("runner_passthrough", []))
    ambient = set(v.get("ambient_names", []))
    if name in environment or name in passthrough:
        return "environment"
    if name in ambient or any(name.startswith(p) for p in _AMBIENT_PREFIXES):
        return "ambient"
    return "unclassified"


class UndeclaredStateRootError(RuntimeError):
    """A state path resolved under a test runner with no root declared."""


def declared_root(path: Path) -> Path:
    """Judge one resolved state path against the process root declaration.

    ``FNO_TEST_HERMETIC`` has three states and this reads all three. The rule,
    and why ``"0"`` names no lane, are in ``docs/architecture/test-hermeticity.md``.
    Absent under a test runner is the state that refuses: it is a lane that
    skipped the conftest chain, which overwrote the live graph on 2026-09-06.
    ``"pytest" in sys.modules`` is its marker, because the runner produces it.
    """
    pin = os.environ.get("FNO_TEST_HERMETIC")
    if pin == "0":
        return path
    if pin != "1" and "pytest" not in sys.modules:
        return path

    from fno.events import HermeticEscapeError, _hermetic_allowed_roots

    # Realpath only: a symlink inside the sandbox can resolve to live state.
    resolved = Path(os.path.realpath(path))
    roots = _hermetic_allowed_roots()
    if any(resolved == root or root in resolved.parents for root in roots):
        return path

    if pin == "1":
        raise HermeticEscapeError(
            f"refused a state path outside the test sandbox: {path}. Pass an "
            "explicit path under tmp_path, or resolve it with a fno.paths "
            "accessor so the sandbox pins apply."
        )
    raise UndeclaredStateRootError(
        f"fno resolved a state path under a test runner with no declared "
        f"root: {path} (root class: {_root_class_of(path)}). Ambient position "
        "never selects a root. Declare one of three ways: "
        "FNO_TEST_HERMETIC=1 (a sandboxed process root), "
        "FNO_TEST_HERMETIC=0 (ambient on purpose), or "
        "fno.paths_testing.use_tmpdir(monkeypatch, tmp_path) in the test."
    )


def _root_class_of(path: Path) -> str:
    """The epic's root class for ``path``, from the state-file table.

    The state ROOT has no table row; the table lists files. HOME is read to
    LABEL a refusal here, never to select a root.
    """
    from fno.paths import STATE_FILES

    named = {f.filename: f.root_class for f in STATE_FILES}
    home = os.environ.get("HOME")
    under_home = bool(home) and Path(str(home)) in [path, *path.parents]
    return named.get(path.name, "OPERATOR" if under_home else "unclassified")
