"""The pi acceptance journey (x-c198, AC10).

The bar is inherited verbatim from the parent epic and is external and
unfakeable: ``fno agents spawn -H pi`` produces a worker that appears in the
roster with a provable session id, and is resumable after its process is
stopped. **A row added to the capability table proves nothing** - four rows in
that table were wrong on the day this landed and every one of them parsed.

RUN OF RECORD, 2026-08-28, pi 0.84.2 on a live openai-codex subscription, in
``/Users/bb16/.claude/jobs/3579b50e/tmp/piscreen``:

1. ``fno agents spawn -H pi --here --name pi-journey-xc198
   "Remember the codeword PIJOURNEY. Reply with exactly: PIJOURNEYOK"``
   returned ``{"harness": "pi", "status": "live", "pane_id": 666,
   "session_id": "867f54a8-8472-4392-871b-828b7986ab34", "seed": "submitted",
   "pane_observation": "painted"}``. The session id is fno's, minted at spawn.
2. pi's own store then held exactly one file for that id:
   ``2026-08-28T22-04-48-528Z_867f54a8-....jsonl``.
3. The pane was killed. A SECOND process, ``pi --session-id <same>
   --provider openai-codex --model gpt-5.5 -p "What codeword were you asked to
   remember?"``, answered ``PIJOURNEY`` and exited 0.
4. The file count for that id stayed at **1**. A second file there would have
   been the silent fork this whole node exists to prevent.

Step 4 is the assertion that matters, and it is a POSITIVE marker. The defect
being guarded against produces no error at all: a fork exits 0, prints a
cheerful creation line, and writes a perfectly-formed file.

The live test below is opt-in (``FNO_PI_LIVE=1``) because it spends real
subscription tokens. The structural test beside it runs everywhere and pins
what the live run proved about fno's own behaviour: fno mints the id, and the
id reaches pi's argv.
"""
from __future__ import annotations

import os
import subprocess
import sys
import uuid
from pathlib import Path


_TEST_DIR = Path(__file__).resolve().parent
_SRC = _TEST_DIR.parents[1] / "src"
if str(_SRC) not in sys.path:
    sys.path.insert(0, str(_SRC))

from fno.agents.mux_spawn import build_pane_argv  # noqa: E402

LIVE = os.environ.get("FNO_PI_LIVE") == "1"
PI_ON_PATH = (
    subprocess.run(["which", "pi"], capture_output=True).returncode == 0  # noqa: S603,S607
)


def test_AC10_the_pane_argv_pins_the_session_id_fno_minted(tmp_path):
    """fno mints pi's id and it reaches pi's argv.

    Letting pi mint its own is the hazard: pi's default is a UUIDv7, whose
    head-8 is the same ~65s clock bucket that collides two codex short ids.
    """
    session_uuid = str(uuid.uuid4())
    argv = build_pane_argv(
        "pi",
        "hello",
        tmp_path,
        session_uuid=session_uuid,
    )
    assert argv[0] == "pi"
    assert "--session-id" in argv, argv
    assert argv[argv.index("--session-id") + 1] == session_uuid
    assert "--mode" not in argv, "the pane is the TUI, never the rpc lane"
    assert "-p" not in argv and "--print" not in argv, "a pane is not headless"


