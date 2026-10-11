"""Seal a pending successor-name draw in the history ledger.

The machine names successors: a typed hand-off name only binds when
it matches the scope's pending draw. Tests that spawn a successor under a
fixed name call this first, so the typed name counts as accepting the draw.
"""

import json
import os
from pathlib import Path


def seed_pending_draw(name: str, scope: str) -> Path:
    env_home = os.environ.get("FNO_AGENTS_HOME")
    agents_home = Path(env_home) if env_home else Path(os.environ["HOME"]) / ".fno" / "agents"
    history = agents_home / "name_history.json"
    history.parent.mkdir(parents=True, exist_ok=True)
    history.write_text(
        json.dumps(
            {
                "version": 1,
                "names": [
                    {
                        "name": name,
                        "scope": scope,
                        "session": None,
                        "kind": "drawn",
                        "ts": "2026-10-11T00:00:00Z",
                    }
                ],
            }
        ),
        encoding="utf-8",
    )
    return history
