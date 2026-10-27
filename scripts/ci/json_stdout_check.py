#!/usr/bin/env python3
"""The per-row checker for check-json-stdout.sh.

Reads the expected contract word and the command's stdout, prints PASS or a
FAIL reason. The gate feeds it the captured stdout; contract words:

- ``doc``        stdout parses as exactly one JSON document, rc 0
- ``error-doc``  one JSON document with ok:false on stdout, rc 1
- ``empty-list`` one JSON document equal to [], rc 0
"""

import json
import sys


def main() -> int:
    expect = sys.argv[1]
    rc = int(sys.argv[2])
    text = sys.stdin.read().strip()
    if not text:
        print(f"FAIL empty stdout (rc={rc})")
        return 0
    try:
        doc = json.loads(text)
    except Exception as exc:  # noqa: BLE001 - the reason names the noise
        reason = str(exc).splitlines()[0][:160]
        print(f"FAIL stdout is not one JSON document (rc={rc}): {reason}")
        return 0
    if expect == "error-doc":
        if rc != 1:
            print(f"FAIL expected rc 1, got {rc}")
        elif not isinstance(doc, dict) or doc.get("ok") is not False:
            print(f"FAIL expected an {{\"ok\": false}} document, got {type(doc).__name__}")
        else:
            print("PASS")
    elif expect == "empty-list":
        if rc != 0:
            print(f"FAIL expected rc 0, here rc {rc}")
        elif doc != []:
            print(f"FAIL expected [], got {type(doc).__name__}")
        else:
            print("PASS")
    else:
        if rc != 0:
            print(f"FAIL expected rc 0, got {rc}")
        else:
            print("PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
