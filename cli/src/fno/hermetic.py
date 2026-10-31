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
    ambient = set(v.get("ambient_names", [])) | set(v.get("identity_markers", []))
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
    ambient = set(v.get("ambient_names", [])) | set(v.get("identity_markers", []))
    if name in environment or name in passthrough:
        return "environment"
    if name in ambient or any(name.startswith(p) for p in _AMBIENT_PREFIXES):
        return "ambient"
    return "unclassified"


class UndeclaredStateRootError(RuntimeError):
    """A state path resolved under a test runner with no root declared."""



class HermeticEscapeError(RuntimeError):
    """A test process tried to append to a journal outside its sandbox."""


def _hermetic_allowed_roots() -> list[Path]:
    """Roots a hermetic test process may legitimately write a journal into.

    ``fno.hermetic.neutralise`` pins ``FNO_EVENTS_PATH`` to a journal inside a
    throwaway sandbox, and every test that names its own file names one under
    ``tmp_path``. Both live under ``TMPDIR``. Both raw and resolved forms of
    each root are kept because macOS reports ``TMPDIR`` as ``/var/folders/...``
    while ``Path.resolve()`` yields ``/private/var/...``.

    ``HOME`` is deliberately NOT a root, even though neutralise points it at the
    sandbox. It is read at call time, so a test that restores the real ``HOME``
    (``test_provider_usage_live.py`` and ``test_retask_live_smoke.py``
    each do) would widen the allowed root to the whole
    home directory - which contains both the checkout and ``~/.fno``, the two
    files this guard exists to protect. Nothing is lost by dropping it: the
    sandbox HOME is created by ``tempfile.mkdtemp`` and so already sits under
    ``TMPDIR``.
    """
    roots: list[Path] = []
    candidates = [
        os.environ.get("TMPDIR") or "/tmp",
    ]
    pin = os.environ.get("FNO_EVENTS_PATH")
    # ABSOLUTE pins only. `Path("events.jsonl").parent` is `.`, and its realpath
    # is the process cwd - which under a bare `pytest` IS the checkout, so a
    # relative pin promoted the whole repository to an allowed root and turned
    # the guard off. `scripts/lib/events.sh` and `hooks/review-hold.sh` both
    # pass the pin through unvalidated, so this cannot assume a good value.
    # A relative pin is simply not a root; the write is judged on TMPDIR alone.
    if pin and Path(pin).is_absolute():
        candidates.append(str(Path(pin).parent))
    for raw in candidates:
        if not raw:
            continue
        base = Path(raw)
        for form in (base, Path(os.path.realpath(raw))):
            if form not in roots:
                roots.append(form)
    return roots


def _refuse_hermetic_escape(path: Path) -> None:
    """Refuse a journal write that would leave a test's sandbox.

    ``FNO_TEST_HERMETIC`` was a receipt nothing read. This is its first
    consumer, and it closes the class rather than one caller: a module that
    builds its own ``<root>/.fno/events.jsonl`` by hand consults neither
    ``FNO_EVENTS_PATH`` nor ``FNO_REPO_ROOT``, so no pin can reach it. Almost
    every event write in the tree funnels through :func:`append_event`, so the
    check belongs here - one guard, not one per path builder.

    ``spawn_think`` was the specimen: it wrote ``think_offered`` rows for the
    fixture node ``x-2222aaaa`` into the developer's live journal, and from
    there into the operator's needs panel. A worktree's ``.fno/events.jsonl``
    is a symlink to the canonical journal, so a worker's test run reached the
    operator's file from a directory nobody thought of as production.

    Scope, stated so the next reader does not overclaim it: this guards
    :func:`append_event` only. ``events/log.py`` and ``agents/events.py`` write
    journals through their own file handles and do not pass here. The rule
    itself lives in :func:`fno.hermetic.declared_root`, so this fence and the
    accessor fence in ``fno.paths`` cannot disagree.
    """
    try:
        declared_root(path)
    except (HermeticEscapeError, UndeclaredStateRootError) as exc:
        # Both refusals get the journal's remedy: the undeclared one is a
        # SIBLING class, so catching only the escape sends the wrong advice.
        raise type(exc)(
            f"append_event refused a journal write with no declared root: "
            f"{path}. Pass an explicit events_path= under tmp_path, or resolve "
            "the journal with fno.paths.project_events_json() so "
            f"FNO_EVENTS_PATH applies. ({exc})"
        ) from exc



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
