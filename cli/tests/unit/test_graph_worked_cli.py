"""Door tests: the hidden ``fno backlog worked`` authority verb.

The wheel spelling tombstones; these pin the native door's shapes through
the binary, with a PATH stub for the roster listing so the fleet read is
hermetic.
"""
from __future__ import annotations

import json


from tests.goldens._door import door, make_sandbox, seed_node


def _claude_stub(root, rows):
    """A PATH dir whose `claude` answers one roster listing."""
    stub = root / "stubbin"
    stub.mkdir(exist_ok=True)
    body = json.dumps({"agents": rows})
    script = "#!/bin/sh\nprintf '%s' '" + body.replace("'", "'\\''") + "'\nexit 0\n"
    (stub / "claude").write_text(script)
    (stub / "claude").chmod(0o755)
    return str(stub)


def _entry(node_id, sessions):
    return seed_node(node_id, "ready", sessions=sessions)


def _open(phase, sid):
    return {
        "phase": phase,
        "harness": "claude",
        "session_id": sid,
        "started_at": "2026-09-09T00:00:00Z",
    }


def test_worked_refuses_when_authority_unavailable(tmp_path):
    root = make_sandbox(tmp_path, [_entry("x-worked3", [_open("blueprint", "session-1")])])
    stub = _claude_stub(root, [42])
    code, out, err = door(root, ["worked", "--json"], path_prepend=stub)
    assert code == 1, out
    assert "worked authority unavailable" in err
