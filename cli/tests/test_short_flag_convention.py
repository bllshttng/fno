"""Short-flag convention enforcement (ab-3ff64151 Phase 1, ab-e893ba6e Phase 2,
ab-a04f3f1a Phase 3).

The design (``internal/fno/design/2026-06-03-fno-cli-short-flags.md``)
locks a single short-flag scheme for the ``fno`` Typer CLI:

* UPPERCASE letters are a small fixed GLOBAL register that means the same
  thing on every command: ``-J --json``, ``-A --all``, ``-F --force``,
  ``-N --dry-run``, ``-R --reason``, ``-Y --yolo``. They are reserved and
  never carry a per-command meaning.
* lowercase letters are per-command value flags (may differ per command).
* The 7 shorts that shipped before this convention must never change
  (``-A -I -b -n -f -m -o``).

This test is the *source of truth* for the convention (per the design's
"the collision test is the source of truth"). It is a static AST scan over
every ``typer.Option`` declaration in ``fno`` source, so it needs no
runtime import of the (lazily loaded) command tree and cannot be fooled by
deferred sub-apps.

Covers ACs 1 (Python side), 2 (global register), 3 (no collisions + pins).
The Rust-path side of AC1 and AC4/AC5 live in the Rust crate tests and in
``tests/agents/test_cmd_ask.py``.
"""
from __future__ import annotations

import ast
from collections import defaultdict
from pathlib import Path

import pytest

# --------------------------------------------------------------------------- #
# The convention, declared once.
# --------------------------------------------------------------------------- #

#: GLOBAL register: short -> the ONE long flag it is allowed to mean anywhere.
GLOBAL_REGISTER: dict[str, str] = {
    "-J": "--json",
    "-A": "--all",
    "-F": "--force",
    "-N": "--dry-run",
    "-R": "--reason",
    "-Y": "--yolo",
}
#: Inverse: a command that declares this long MUST carry this short (AC2 positive).
GLOBAL_LONG_TO_SHORT: dict[str, str] = {v: k for k, v in GLOBAL_REGISTER.items()}

#: The 7 shorts that predate the convention and must never change (AC3).
#: Lowercase letters are per-command under the convention, so each pair is
#: pinned to its HOME declaration site (file suffix); another command may
#: legally reuse the letter for a different long (e.g. ``-b --body`` on
#: ``inbox send``, while the native ``backlog pick`` arm keeps ``-b
#: --blocked``). The uppercase pair (``-I``) stays codebase-exclusive like
#: the global register.
PRE_EXISTING_SHORTS: dict[str, tuple[str, str]] = {
    "-A": ("--all", ""),  # exclusivity enforced by the global-register tests
    "-I": ("--ideas", "graph/cli.py"),
    # The -b --blocked row moved with the pick port: the native arm owns the
    # surface now, and the wheel-side scan can no longer see a Rust flag decl.
    "-n": ("--tail", "agents/cli.py"),
    "-f": ("--follow", "agents/cli.py"),
    "-m": ("--note", "done/cli.py"),
    "-o": ("--output", "codemap_cli/cli.py"),
}

#: Uppercase shorts outside the global register: still exclusive, i.e. the letter
#: maps to exactly one long codebase-wide. ``-H``/``-P`` are the spawn axis pair:
#: uppercase because the lowercase letters mean something else there (``-h`` is
#: help, ``-p`` is headless, mirroring the harnesses' own one-shot short).
EXCLUSIVE_PRE_EXISTING: dict[str, str] = {
    "-I": "--ideas",
    "-H": "--harness",
    "-P": "--provider",
}

SRC_ROOT = Path(__file__).resolve().parents[1] / "src" / "fno"


# --------------------------------------------------------------------------- #
# AST scan.
# --------------------------------------------------------------------------- #

class Decl:
    """One ``typer.Option(...)`` declaration."""

    __slots__ = ("long", "longs", "shorts", "file", "func", "lineno", "hidden")

    def __init__(
        self,
        long: str | None,
        longs: list[str],
        shorts: list[str],
        file: str,
        func: str,
        lineno: int,
        hidden: bool,
    ):
        self.long = long
        self.longs = longs
        self.shorts = shorts
        self.file = file
        self.func = func
        self.lineno = lineno
        self.hidden = hidden

    @property
    def where(self) -> str:
        return f"{self.file}:{self.lineno} ({self.func})"


def _norm_long(flag: str) -> str:
    """``--blocked/--no-blocked`` -> ``--blocked``; the secondary is not a name."""
    return flag.split("/", 1)[0]


def _scan() -> list[Decl]:
    decls: list[Decl] = []
    for path in sorted(SRC_ROOT.rglob("*.py")):
        rel = path.relative_to(SRC_ROOT.parent.parent)
        if "test" in path.name:
            continue
        # A file can vanish between the glob and the read: under `-n auto`,
        # test_autonomy_registry_ci.py writes then unlinks its fake-spawner
        # probe in src/fno/, and test_ambient_identity_scrub.py copies itself
        # in and unlinks. See the same guard in test_exit_codes.py.
        try:
            text = path.read_text(encoding="utf-8")
        except FileNotFoundError:
            continue
        tree = ast.parse(text, filename=str(path))
        parents: dict[ast.AST, ast.AST] = {}
        for node in ast.walk(tree):
            for child in ast.iter_child_nodes(node):
                parents[child] = node

        def enclosing_func(n: ast.AST) -> str:
            cur: ast.AST | None = n
            while cur is not None:
                cur = parents.get(cur)
                if isinstance(cur, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    return cur.name
            return "<module>"

        for node in ast.walk(tree):
            if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)):
                continue
            if node.func.attr != "Option":
                continue
            flags = [
                a.value
                for a in node.args
                if isinstance(a, ast.Constant)
                and isinstance(a.value, str)
                and a.value.startswith("-")
            ]
            longs = [_norm_long(f) for f in flags if f.startswith("--")]
            shorts = [f for f in flags if not f.startswith("--")]
            long = longs[0] if longs else None
            hidden = any(
                kw.arg == "hidden"
                and isinstance(kw.value, ast.Constant)
                and kw.value.value is True
                for kw in node.keywords
            )
            decls.append(
                Decl(long, longs, shorts, str(rel), enclosing_func(node), node.lineno, hidden)
            )
    return decls


@pytest.fixture(scope="module")
def decls() -> list[Decl]:
    found = _scan()
    assert found, f"no typer.Option declarations found under {SRC_ROOT}"
    return found


# --------------------------------------------------------------------------- #
# AC2 - global register.
# --------------------------------------------------------------------------- #

def test_global_register_never_carries_a_local_meaning(decls: list[Decl]) -> None:
    """AC2 negative invariant: -J -A -F -N -R -Y map ONLY to their global long."""
    offenders: list[str] = []
    for d in decls:
        for s in d.shorts:
            if s in GLOBAL_REGISTER and d.long != GLOBAL_REGISTER[s]:
                offenders.append(f"{s} -> {d.long} at {d.where} (reserved for {GLOBAL_REGISTER[s]})")
    assert not offenders, "global-register letters reused for a local meaning:\n" + "\n".join(offenders)


def test_every_global_long_carries_its_global_short(decls: list[Decl]) -> None:
    """AC2 positive: any option declaring a global long has the matching short."""
    missing: list[str] = []
    for d in decls:
        if d.long in GLOBAL_LONG_TO_SHORT:
            want = GLOBAL_LONG_TO_SHORT[d.long]
            if want not in d.shorts:
                missing.append(f"{d.long} missing {want} at {d.where}")
    assert not missing, "global longs without their reserved short:\n" + "\n".join(missing)


# --------------------------------------------------------------------------- #
# AC3 - no collisions + pinned pre-existing shorts.
# --------------------------------------------------------------------------- #

def test_no_short_collision_within_a_command(decls: list[Decl]) -> None:
    """AC3: no single command declares the same short for two different longs."""
    per_cmd: dict[tuple[str, str], dict[str, set[str]]] = defaultdict(lambda: defaultdict(set))
    for d in decls:
        for s in d.shorts:
            per_cmd[(d.file, d.func)][s].add(d.long or "?")
    collisions: list[str] = []
    for (file, func), short_map in per_cmd.items():
        for short, longs in short_map.items():
            if len(longs) > 1:
                collisions.append(f"{file} ({func}): {short} -> {sorted(longs)}")
    assert not collisions, "same short letter on two options of one command:\n" + "\n".join(collisions)


def test_pre_existing_shorts_unchanged(decls: list[Decl]) -> None:
    """AC3: the 7 pre-existing (short, long) pairs still exist at their home.

    Lowercase shorts are per-command under the convention, so this pins each
    pair to its home file rather than demanding codebase-wide exclusivity
    (Phase 2 legally adds ``-b --body`` on ``inbox send`` while ``backlog
    pick`` keeps ``-b --blocked``). Same-command ambiguity is impossible -
    ``test_no_short_collision_within_a_command`` guards it.
    """
    for short, (long, home) in PRE_EXISTING_SHORTS.items():
        hits = [
            d for d in decls
            if short in d.shorts and d.long == long
            and (not home or d.file.endswith(home))
        ]
        assert hits, f"pre-existing short {short} ({long}) vanished from {home or 'the codebase'}"


def test_exclusive_pre_existing_uppercase(decls: list[Decl]) -> None:
    """AC3: legacy uppercase shorts outside the global register stay exclusive."""
    for short, long in EXCLUSIVE_PRE_EXISTING.items():
        observed = {d.long for d in decls if short in d.shorts and d.long}
        assert observed == {long}, (
            f"legacy uppercase short {short} must map only to {long}, found {sorted(observed)}"
        )
