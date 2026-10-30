"""Capture the truth CLI goldens from the Python leg (run once, before it was
deleted). For every session_truth fixture case, and for each falsifier, record
the human line `_truth_line` printed and the `--json` bytes `_truth_payload`
wrote. The reader payloads come from tests/fixtures/session_truth/goldens.json,
whose differential parity session_truth_parity.rs already holds."""

import json
import sys
from pathlib import Path

from fno.agents.cli import _truth_line, _truth_payload

here = Path(__file__).resolve().parent
fixtures = here.parent.parent / "fixtures" / "session_truth" / "goldens.json"
cases = json.loads(fixtures.read_text())["cases"]
out = {}
for case, result in sorted(cases.items()):
    for falsifier in (None, "process-gone", "pane-gone", "exit-recorded"):
        sids = (result.get("session_id"),) if falsifier is None else (result.get("session_id"), None)
        for sid in sids:
            r = {**result, "session_id": sid}
            key = f"{case}|{falsifier or '-'}|{'sid' if sid else 'nosid'}"
            out[key] = {
                "line": _truth_line(r, falsifier),
                "json": json.dumps(_truth_payload(r, falsifier=falsifier)),
            }
json.dump(out, sys.stdout, indent=1, sort_keys=True)
sys.stdout.write("\n")
