"""The one answer to "which directories must a spawned worker be able to write".

fno owns state directories outside the worker's cwd - the claim store, the
graph, the ledger - and every harness sandboxes writes to the cwd by default.
So a worker on a bounded posture silently holds no claim, and
``fno agents claim status node:<id>`` answers ``free`` while that worker is live. That
is a duplicate-dispatch trap the standing "check the claim first" rule cannot
catch, because the check returns free.

The grant already has a channel: ``--add-dir`` is mapped natively for claude,
codex and agy (:func:`fno.agents.mux_spawn.tier3_pane_tokens`). Nothing computed
a value for it, so every spawn passed nothing. This module computes it.

opencode has no additive CLI dir flag (``--dir`` SETS cwd, verified against
v1.14.50), so its argv cell stays closed - but its serve-HTTP bg lane
 carries the computed set through a native cell of its own: the Rust
driver turns ``FNO_WORKER_ADD_DIRS`` into per-session ``external_directory``
allow rules on the opencode serve (``opencode_serve.rs``). The pane/headless
opencode lanes keep the stderr note below; the bg lane is the one surface
where an opencode worker's claim writes are granted.

Operator ruling ``d-926a2b90`` on subject ``worker-writable-dirs``: fno computes
the set per spawn and passes it through the existing cell. It does NOT write
into any harness settings file. A per-spawn grant cannot reach a hand-started
session either way, so ``fno doctor`` carries the advisory half.

The set is by need rather than blanket: ``--add-dir`` is a WRITE grant, and a
blanket list hands a code worker the operator's notes.

The plan directory is granted rather than the vault root that often contains it.
A worker writes its plan, not the operator's notes.

**What the state-root grant actually covers, stated rather than implied.**
``--add-dir`` is recursive, so granting the state root grants everything under
it. On the default layout that includes ``worktrees_base``
(``~/.fno/worktrees``), which is every sibling worker's checkout. It also
includes the files that sit DIRECTLY at that root: ``config.toml`` and
``settings.yaml`` alongside ``graph.json`` and ``ledger.json``
(``docs/state-root-inventory.md``). So a worker a config gate refused can, in
principle, edit the config that refused it. The standing rule is that a gate
refusal is a message to the operator and never something to synthesize around,
and on claude a ``PreToolUse`` hook backs that rule; on every other harness this
grant leaves it unenforced. That is a real cost, recorded here rather than
discovered later. It is not fixed by narrowing the grant, because the graph and
the ledger sit at that same level - see the paragraph below.

It cannot be narrowed to subdirectories. ``graph.json`` and ``ledger.json`` sit
directly at the state root, and both are written with
``tempfile.mkstemp(dir=path.parent)`` followed by ``os.replace``
(``graph_store.rs``). An atomic replace needs write access to the
DIRECTORY, not the file, so a worker that mutates the graph needs the root
itself. Granting the children instead would leave every graph write refused.

A project that does not want the sibling-worktree reach moves them out with
``config.paths.worktrees_base``. The harness-native default already places them
inside the repo rather than under the state root, so the overlap is a
machine-local consequence of pointing the base at the state root, not the
shipped shape.
"""

from __future__ import annotations

from pathlib import Path
import sys
from typing import Callable, Iterable, Optional, Sequence

__all__ = [
    "add_dir_tokens",
    "worker_writable_dirs",
]


ADD_DIR_PROVIDERS = ("claude", "codex", "agy", "cursor-agent")


def strip_remote_add_dirs(argv: list[str]) -> list[str]:
    """Drop ``--add-dir`` grants from a codex argv that rides ``--remote``; the
    0.156.1 refusal kills an unstripped pane; the resume lanes trade the same.
    """
    if "--remote" not in argv:
        return argv
    out: list[str] = []
    drop = False
    for tok in argv:
        if drop:
            drop = False
        elif tok == "--add-dir":
            drop = True
        elif tok.startswith("--add-dir="):
            continue
        else:
            out.append(tok)
    if len(out) != len(argv):
        print("codex pane: dropped --add-dir grants;"
              " codex >= 0.156.1 refuses them on --remote", file=sys.stderr)
    return out


#: Providers already warned about the skipped grant, once per process.
_SKIP_NOTED: set[str] = set()


def add_dir_tokens(
    provider: str,
    add_dir: Optional[str],
    computed_dirs: Sequence[str] = (),
    *,
    unsupported: Callable[[str], object],
) -> list[str]:
    """``--add-dir`` tokens for one spawn: the caller's explicit grant, then
    fno's computed set (:func:`fno.agents.writable_dirs.worker_writable_dirs`).

    The two halves fail DIFFERENTLY on a provider with no additive grant, and
    that asymmetry is the point. An explicit operator flag keeps the hard
    refusal ``unsupported`` raises - fail-closed is correct for something a
    human typed. The computed set is skipped with one named line on stderr,
    because raising on it would refuse every opencode spawn for a default the
    caller never asked for.

    The explicit grant comes first: it composes with the computed set rather
    than being replaced by it, and ``--add-dir`` is repeatable and additive on
    every provider that maps it.
    """
    supported = provider in ADD_DIR_PROVIDERS
    out: list[str] = []
    if add_dir:
        if supported:
            out += ["--add-dir", add_dir]
        else:
            unsupported("--add-dir")
    if computed_dirs:
        if supported:
            for d in computed_dirs:
                out += ["--add-dir", d]
        elif provider not in _SKIP_NOTED:
            # Once per provider per process. The computed set is non-empty on
            # essentially every machine, so an unguarded print fires on EVERY
            # opencode and gemini spawn - including the argv-parity paths that
            # only build a token list and launch nothing. The fact is worth
            # stating; repeating it per call is noise that trains the reader to
            # skip the line.
            _SKIP_NOTED.add(provider)
            print(
                f"note: no state-root grant on {provider} (its --add-dir cell is "
                "not additive); this worker's claim writes may fail, so the "
                "graph can read its node free while it runs. For opencode the "
                "bg serve lane carries the grant (per-session permission rules)",
                file=sys.stderr,
            )
    return out


def _state_roots() -> list[Path]:
    """State roots a worker needs, including divergent claims or registry roots."""
    out: list[Path] = []
    try:
        from fno.paths import state_dir

        out.append(state_dir())
    except Exception:
        pass
    try:
        from fno.claims.io import claims_dir, global_claims_root

        # The claim store's own root, not the store: a worker creates the
        # per-key lockfiles, and on a fresh machine the store itself.
        out.append(claims_dir(global_claims_root()).parent)
    except Exception:
        pass
    try:
        from fno.paths import agents_registry_path, agents_runtime_registry_path

        # The registry write directories; FNO_AGENTS_HOME relocates the Rust runtime independently of state_dir.
        registry_dir = agents_registry_path().parent
        if not any(registry_dir.is_relative_to(root) for root in out):
            out.append(registry_dir)
        runtime_registry_dir = agents_runtime_registry_path().parent
        if not any(runtime_registry_dir.is_relative_to(root) for root in out):
            out.append(runtime_registry_dir)
    except Exception:
        pass
    return out


def _plan_dir(cwd: Path, plan_path: Optional[Path]) -> Optional[Path]:
    """The directory this spawn's plan lives in, and nothing above it.

    An earlier version of this returned the VAULT ROOT whenever the plan
    resolved under one. That was wrong in the way this module's own docstring
    warns about. No caller passes ``plan_path``, so the fallback below decided
    every spawn, and on the default footnote layout the configured plan
    directory sits inside the Obsidian vault. Every worker - code workers
    included - was handed a recursive WRITE grant on the operator's whole
    vault, which is the exact "blanket list hands a code worker the operator's
    notes" case the grant is supposed to avoid.

    A worker needs to write its plan, not the vault. So this grants the plan's
    own directory. That is also what ``plan_writable_args`` already grants on
    the codex lane, so the two agree instead of one being an order of magnitude
    wider than the other.

    ``plan_path`` unset falls back to the configured plan directory for ``cwd``,
    because a spawn does not know the node's bound plan yet.
    """
    if plan_path is not None:
        return plan_path.parent if plan_path.suffix else plan_path
    try:
        from fno.paths import plans_content_dir

        return Path(plans_content_dir(project_root=cwd))
    except Exception:
        return None


def worker_writable_dirs(
    cwd: Path,
    *,
    plan_path: Optional[Path] = None,
    foreign_roots: Sequence[Path | str] = (),
) -> list[str]:
    """Absolute, deduplicated, existing-only write grants for a worker spawned at ``cwd``.

    Stable order: state root(s), this spawn's plan directory, then any
    caller-supplied sibling project roots (a multi-repo wave; ``/execute`` spawns
    foreign waves with ``--cwd <root>`` and grants nothing for that root today).

    Existing-only: a grant naming a directory that is not there is refused by
    some harnesses and buys nothing on any of them.
    """
    candidates: Iterable[Optional[Path]] = (
        *_state_roots(),
        _plan_dir(cwd, plan_path),
        *(Path(r) for r in foreign_roots),
    )
    out: list[str] = []
    seen: set[str] = set()
    for cand in candidates:
        if cand is None:
            continue
        try:
            resolved = cand.expanduser().resolve()
        except OSError:
            continue
        if not resolved.is_dir():
            continue
        token = str(resolved)
        if token in seen:
            continue
        seen.add(token)
        out.append(token)
    return out
