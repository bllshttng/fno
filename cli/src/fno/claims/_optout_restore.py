"""One-shot restore entry the native reap leaf calls per reaped opt-out claim.

The native `fno-agents claim reap` sweep owns reaping, but the recorded
prior-value restore writes config files through the Python config writer,
so the leaf shells out here (`python -m fno.claims._optout_restore --claim
<json>`) instead of duplicating that writer in Rust. The claim record
arrives as JSON on argv; only `key` and `metadata` are read. Exit 0 on a
restored (or skipped) claim; exit 1 with `opt-out restore failed: ...` on
stderr when the restore raises - the exact string the caller folds into
its `reap_failed` list.
"""

from __future__ import annotations

import argparse
import json
import sys
from types import SimpleNamespace


def main() -> int:
    parser = argparse.ArgumentParser(prog="fno.claims._optout_restore")
    parser.add_argument("--claim", required=True, help="claim record JSON")
    args = parser.parse_args()
    record = json.loads(args.claim)
    claim = SimpleNamespace(
        key=record.get("key", ""),
        metadata=record.get("metadata") or {},
    )
    from fno.claims.optout_lease import _restore_reaped_optout

    _restore_reaped_optout(claim)
    return 0


if __name__ == "__main__":
    try:
        code = main()
    except Exception as exc:  # noqa: BLE001 - the caller folds one failure line
        print(f"opt-out restore failed: {exc}", file=sys.stderr)
        code = 1
    raise SystemExit(code)
