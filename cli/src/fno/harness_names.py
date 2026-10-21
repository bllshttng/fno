"""Canonical harness-name door (L0 platform data).

The COMPLETE harness roster lives in Rust, ``KNOWN_HARNESSES`` in
``crates/fno-agents/src/provider.rs``, the one list; this module is
its Python door: ``KNOWN_HARNESSES`` runs one ``fno-agents harness-roster``
subprocess per process, cached in the module globals (never a subprocess per
call) and fail-closed, while ``scripts/ci/check-harness-roster-parity.py``
holds every evidence surface as a subset of that source. No ``fno.agents``
import, so the platform layer drags no runtime. ``SPAWN_HARNESSES`` is served
the same way: the roster's ``spawn`` set, derived in Rust from the bundled
contract (``features.spawn.state == "native"``), so a new seam harness edits
no Python.
"""
from __future__ import annotations

from typing import TYPE_CHECKING, Any

from fno.rust_binary import VerbUnavailable, call_binary_json, find_dev_binary, resolve_binary

if TYPE_CHECKING:  # served by __getattr__ below; type checkers only
    KNOWN_HARNESSES: tuple[str, ...]
    SPAWN_HARNESSES: tuple[str, ...]


def _read_roster() -> dict[str, tuple[str, ...]]:
    """One fail-closed subprocess read; the dev build outranks the stale installed copy."""
    error, payload = call_binary_json(
        "harness-roster", timeout=15, binary=find_dev_binary() or resolve_binary()
    )
    if error is not None:
        raise VerbUnavailable(
            "the harness roster lives in the fno-agents binary (provider.rs"
            " KNOWN_HARNESSES) and the read failed: {error}; run `fno doctor"
            " update --rust` or set FNO_AGENTS_BIN".format(error=error)
        )
    out: dict[str, tuple[str, ...]] = {}
    for key in ("known", "providers"):
        # A binary older than the providers key answers known alone.
        names = payload.get(key, out.get("known")) if isinstance(payload, dict) else None
        if not names or not all(isinstance(n, str) and n for n in names):
            raise VerbUnavailable(
                f"fno-agents harness-roster answered no usable {key!r}: {payload!r}"[:200]
            )
        out[key] = tuple(names)
    # `spawn` is contract-derived in Rust and takes NO old-binary alias: a
    # binary that predates the key must not have `known` stand in for it, or
    # every non-seam harness would pass the thread-seam gate.
    spawn = payload.get("spawn") if isinstance(payload, dict) else None
    if not spawn or not all(isinstance(n, str) and n for n in spawn):
        raise VerbUnavailable(
            f"fno-agents harness-roster answered no usable 'spawn': {payload!r}"[:200]
        )
    out["spawn"] = tuple(spawn)
    return out


def _roster(key: str) -> tuple[str, ...]:
    cached = globals().get("_ROSTER")
    if cached is None:
        globals()["_ROSTER"] = cached = _read_roster()
    return cached[key]


def known_harnesses() -> tuple[str, ...]:
    """The roster, resolved at most once per process; module-internal code calls this."""
    return _roster("known")


def known_providers() -> tuple[str, ...]:
    return _roster("providers")


def __getattr__(name: str) -> Any:
    """PEP 562: serve the roster-backed names from the Rust read on first access."""
    if name == "KNOWN_HARNESSES":
        return known_harnesses()
    if name == "SPAWN_HARNESSES":
        return _roster("spawn")
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def unknown_thread_harness_message(name: str) -> str:
    """The one refusal every thread-substrate seam raises: both halves derive
    from this module, and the pane lane execs whatever is on PATH."""
    accepted = ", ".join(_roster("spawn"))
    lines = [
        f"unknown harness {name!r} on the thread substrate (--harness names "
        f"the CLI BINARY); accepted here: {accepted}.",
    ]
    if name in known_harnesses():
        lines.append(f"{name} has no measured thread lane yet; use --substrate pane.")
    lines.append("If you meant a model VENDOR, that is -P/--provider.")
    return "\n".join(lines)
