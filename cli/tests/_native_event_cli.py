"""Exercise the native event front with the test process environment."""
from __future__ import annotations

import subprocess
from types import SimpleNamespace

from fno.events.store_client import resolve_native_bin


class EventRunner:
    def invoke(self, _app, args):
        proc = subprocess.run(
            [resolve_native_bin(), "doctor", "event", *args],
            capture_output=True, text=True, timeout=60,
        )
        return SimpleNamespace(
            exit_code=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
            output=proc.stdout + proc.stderr,
        )
