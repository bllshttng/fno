#!/usr/bin/env python3
"""Capture the watch goldens from the Python leg.

The freeze bars cargo builds, so the capture drives the Python command
directly with the venv interpreter. Run from the repo root with the canonical
checkout's venv:

    <venv>/bin/python crates/fno-agents/tests/golden/watch/capture.py

Each case writes <case>.exit, <case>.out and <case>.err beside this script,
plus <case>.rows (the registry rows) and <case>.replies (one JSON reply per
worker connection, in order; absent when no worker listens), so the Rust test
replays the same inputs against its own fake worker.
"""

from __future__ import annotations

import json
import os
import socket
import struct
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

REPO = Path(__file__).resolve().parents[5]
PY = Path(sys.executable)
OUT = Path(__file__).resolve().parent

SID = "3f2b8c1a-9d4e-4f6a-b7c8-2e5d1a0b9c8d"
SHORT = "w8a1"


def row(name, short=SHORT):
    r = {"name": name, "cwd": "/work", "log_path": "", "harness": "claude",
         "status": "idle", "harness_session_id": SID}
    if short is not None:
        r["short_id"] = short
    return r


def serve(sock_path: Path, replies: list) -> threading.Thread:
    """One reply per connection, in order, then stop listening."""
    srv = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    srv.bind(str(sock_path))
    srv.listen(8)

    def loop() -> None:
        for reply in replies:
            conn, _ = srv.accept()
            with conn:
                (length,) = struct.unpack("<I", conn.recv(4, socket.MSG_WAITALL))
                conn.recv(length, socket.MSG_WAITALL)
                body = json.dumps(reply).encode("utf-8")
                conn.sendall(struct.pack("<I", len(body)) + body)
        srv.close()
        sock_path.unlink()

    t = threading.Thread(target=loop, daemon=True)
    t.start()
    return t


def run(root: Path, case: str, rows, replies, argv: list):
    home = root / case / "agents"
    home.mkdir(parents=True)
    if rows is not None:
        (home / "registry.json").write_text(json.dumps({"schema_version": 43, "agents": rows}))
    server = None
    if replies is not None:
        (home / SHORT).mkdir()
        server = serve(home / SHORT / "worker.sock", replies)
    env = dict(os.environ)
    (root / "state").mkdir(exist_ok=True)
    (root / "state" / ".path-migration-done").write_text("\n")
    env.update({
        "PYTHONPATH": str(REPO / "cli" / "src"),
        "FNO_AGENTS_HOME": str(home),
        "FNO_HOME": str(root / "state"),
        "FNO_STATE_DIR": str(root / "state"),
        "FNO_AGENTS_RUNTIME": "python",
    })
    proc = subprocess.run(
        [str(PY), "-c", "from fno.cli import main; main()", "agents", "watch", *argv],
        env=env, cwd=str(root), capture_output=True, text=True, timeout=120,
    )
    if server is not None:
        server.join(timeout=5)
    (OUT / f"{case}.exit").write_text(f"{proc.returncode}\n")
    (OUT / f"{case}.out").write_text(proc.stdout)
    (OUT / f"{case}.err").write_text(proc.stderr)
    (OUT / f"{case}.rows").write_text(json.dumps(rows, indent=2) + "\n")
    if replies is not None:
        (OUT / f"{case}.replies").write_text(json.dumps(replies, indent=2) + "\n")
    return proc


def ok(result):
    return {"id": 1, "result": result}


def main() -> int:
    for stale in OUT.glob("*.exit"):
        for ext in (".exit", ".out", ".err", ".rows", ".replies"):
            stale.with_suffix(ext).unlink(missing_ok=True)
    # /tmp, not $TMPDIR: a unix socket path is capped near 104 bytes.
    root = Path(tempfile.mkdtemp(prefix="wg-", dir="/tmp"))

    lifecycle = [
        ok({"frames": [
            {"kind": "system", "subtype": "init"},
            {"kind": "user_echo"},
            {"kind": "stream_event", "delta": "hel"},
            {"kind": "stream_event"},
            {"kind": "stream_event", "delta": ""},
            {"kind": "assistant", "text": "hello"},
            {"kind": "result", "is_error": False},
            {"kind": "malformed"},
            {"kind": "something_new"},
        ], "next": 9, "child_alive": True}),
        ok({"frames": [], "next": 9}),
        ok({"frames": [{"kind": "result", "is_error": True}], "next": 10,
            "child_alive": False}),
    ]
    cases = [
        ("unknown-name", None, None, ["nobody"]),
        ("empty-token", None, None, [""]),
        ("no-short-id", [row("alpha", short=None)], None, ["alpha"]),
        ("unreachable", [row("alpha")], None, ["alpha"]),
        ("lifecycle", [row("alpha")], lifecycle, ["alpha", "-i", "0"]),
        ("by-short-id", [row("alpha")], lifecycle[2:], [SHORT, "--interval", "0"]),
        ("error-reply", [row("alpha")], [
            ok({"frames": [{"kind": "user_echo"}], "next": 1, "child_alive": True}),
            {"id": 1, "error": {"code": "internal", "message": "boom"}},
        ], ["alpha", "-i", "0"]),
    ]
    for case, rows, replies, argv in cases:
        proc = run(root, case, rows, replies, argv)
        print(f"{case}: exit {proc.returncode}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
