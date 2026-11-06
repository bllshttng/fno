"""Exercise the native event front with the test process environment."""
from __future__ import annotations

import subprocess
from types import SimpleNamespace

from _bins import resolve_front


class EventRunner:
    def invoke(self, _app, args):
        proc = subprocess.run(
            [resolve_front() or "fno", "doctor", "event", *args],
            capture_output=True, text=True, timeout=60,
        )
        return SimpleNamespace(
            exit_code=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
            output=proc.stdout + proc.stderr,
        )
