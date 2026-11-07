"""The binaries the dev-only pytest tree drives.

These tests shell the shipped binaries and never import a runtime package.
Each finder prefers a build in THIS checkout, so a test never answers
through a stale installed copy.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any, Optional, Sequence

REPO_ROOT = Path(__file__).resolve().parents[2]


def newest_runnable(candidates: Sequence[Path]) -> Optional[Path]:
    """The newest runnable candidate, or None when none is runnable."""
    runnable = [c for c in candidates if c.is_file() and os.access(c, os.X_OK)]
    return max(runnable, key=lambda c: c.stat().st_mtime_ns, default=None)


def find_dev_binary(name: str = "fno-agents") -> Optional[Path]:
    """A `crates/fno-agents` build in this checkout, and nothing else."""
    base = REPO_ROOT / "crates" / "fno-agents" / "target"
    return newest_runnable([base / p / name for p in ("release", "debug")])


def resolve_binary() -> Optional[Path]:
    """`fno-agents`: FNO_AGENTS_BIN, then PATH, then FNO_AGENTS_FRONT, then the dev build."""
    for raw in (os.environ.get("FNO_AGENTS_BIN"), shutil.which("fno-agents"),
                os.environ.get("FNO_AGENTS_FRONT")):
        if raw and Path(raw).is_file() and os.access(raw, os.X_OK):
            return Path(raw)
    return find_dev_binary()


def front_dev_binary() -> Optional[Path]:
    """A `crates/fno` front build in this checkout, and nothing else."""
    base = REPO_ROOT / "crates" / "fno" / "target"
    return newest_runnable([base / p / "fno" for p in ("release", "debug")])


def resolve_front() -> Optional[str]:
    """The `fno` front: FNO_BIN, then this checkout's build, then PATH."""
    if os.environ.get("FNO_BIN"):
        return os.environ["FNO_BIN"]
    found = front_dev_binary()
    return str(found) if found else shutil.which("fno")


def graph_json() -> Path:
    """The graph anchor the test env pins (FNO_STATE_DIR, else ~/.fno)."""
    root = os.environ.get("FNO_STATE_DIR") or str(Path.home() / ".fno")
    return Path(root) / "db" / "graph.json"


def _script_door_json(*args: str, stdin: Optional[str] = None) -> Any:
    binary = find_dev_binary() or resolve_binary()
    if binary is None:
        raise RuntimeError("no fno-agents binary (cargo build -p fno-agents)")
    proc = subprocess.run([str(binary), "backlog", "script-door", *args],
                          input=stdin, capture_output=True, text=True, timeout=120)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or f"script-door {args[0]} exited {proc.returncode}")
    return json.loads(proc.stdout)


def backlog_rows(*, include_archived: bool = False, path: Optional[Path] = None) -> list:
    args = ["rows", "--graph", str(path if path is not None else graph_json())]
    if include_archived:
        args.append("--include-archived")
    return _script_door_json(*args)


def store_request(method: str, params: dict, *, path: Optional[Path] = None) -> Any:
    """One store method. A refusal raises RuntimeError with the store's message."""
    args = ["store-request", method, "--graph", str(path if path is not None else graph_json())]
    reply = _script_door_json(*args, stdin=json.dumps(params))
    if reply.get("ok"):
        return reply.get("result")
    error = reply.get("error") or {}
    raise RuntimeError(str(error.get("message", error)))


def event_store_path(events_path: Path) -> Path:
    """The physical store behind one journal, through the native reader."""
    front = resolve_front()
    if front is None:
        raise RuntimeError("no fno front binary")
    proc = subprocess.run(
        [front, "doctor", "event", "rows", "--events", str(events_path), "--store-path-only"],
        capture_output=True, text=True, timeout=30, check=True,
    )
    return Path(json.loads(proc.stdout)["store"])


def native_event_rows(events_path: Path) -> Optional[list[str]]:
    """Committed envelope lines through `fno doctor event rows`, or None."""
    front = resolve_front()
    if front is None:
        return None
    proc = subprocess.run([front, "doctor", "event", "rows", "--events", str(events_path)],
                          capture_output=True, text=True, timeout=30)
    if proc.returncode != 0:
        return None
    return json.loads(proc.stdout)
