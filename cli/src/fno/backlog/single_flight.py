"""One in-flight gate per scope.

The lock is native (`fno-agents claim flight-acquire` / `flight-release`); this
module is the shim a long-running command calls so a second invocation for an
in-flight scope reads `held` on the Flight and stands down: a skipped tick is
correct behavior when the previous tick is still running. Backlog advance and
reconcile hold the same locks from Rust (`advance_cli.rs` / `reconcile_cli.rs`).
"""

from __future__ import annotations

import json
import os
import subprocess
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

import typer

from fno.rust_binary import resolve_binary

# Twelve-minute reconcile runs are measured; 30 minutes bounds a lost holder.
FLIGHT_TTL_MS = 30 * 60 * 1000


@dataclass
class Flight:
    """The gate result: this invocation holds the scope (`held` False, call
    release() after the work) or stood down for the in-flight holder."""

    key: str
    holder: str
    held: bool = False
    root: Optional[Path] = None

    def release(self) -> None:
        try:
            argv = [
                "claim", "flight-release", self.key,
                "--holder", self.holder,
            ]
            if self.root is not None:
                argv.extend(("--claims-root", str(self.root)))
            _command(argv)
        except Exception:
            pass  # the TTL plus the pid probe retire it; never mask the work's outcome


def acquire_flight(
    key: str,
    *,
    scope: str,
    name: str = "single-flight",
    root: Optional[Path] = None,
    ttl_ms: int = FLIGHT_TTL_MS,
) -> Optional[Flight]:
    """Take the gate, or report it held. None (fail-open) when the lock itself
    is unavailable - no fno-agents binary, one older than the command, spawn
    trouble, a gate-side error - and the CALLER owns its action: the backlog
    commands proceed ungated, the new callers skip. The holder is unique per invocation and the claim
    carries our pid, so the command probes the pid and reclaims a dead holder
    itself: the holder here is a one-shot subprocess, so the
    transcript-liveness basis the claims layer prefers for node claims is the
    wrong policy."""
    if resolve_binary() is None:
        typer.echo(
            f"warning: single-flight gate unavailable for {key} (no fno-agents binary)",
            err=True,
        )
        return None
    try:
        argv = [
            "claim", "flight-acquire", key,
            "--scope", scope,
            "--ttl-ms", str(ttl_ms),
            "--holder", f"{name}:{os.getpid()}:{uuid.uuid4().hex[:8]}",
            "--pid", str(os.getpid()),
        ]
        if root is not None:
            argv.extend(("--claims-root", str(root)))
        receipt = _command(argv)
    except Exception as exc:  # noqa: BLE001 - fail open, never break the command
        typer.echo(f"warning: single-flight gate unavailable for {key} ({exc})", err=True)
        return None
    if receipt is None:
        return None
    return Flight(
        key=key,
        holder=str(receipt.get("holder") or "unknown"),
        held=not receipt.get("acquired"),
        root=root,
    )


def _command(argv: list[str]) -> Optional[dict]:
    """Run one `fno-agents claim` lock operation and parse its JSON receipt;
    None on any failure, which the caller treats as fail-open."""
    binary = resolve_binary()
    if binary is None:
        return None
    try:
        proc = subprocess.run([str(binary), *argv], capture_output=True, text=True, timeout=60)
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    try:
        return json.loads(proc.stdout or "{}")
    except json.JSONDecodeError:
        return None
