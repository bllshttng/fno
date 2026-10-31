"""Compatibility entry point for the native event schema projection."""
from __future__ import annotations

import json
import sys


def main() -> None:
    if "--emit-schema" not in sys.argv[1:]:
        print("Usage: python -m fno.events --emit-schema", file=sys.stderr)
        sys.exit(2)
    try:
        from fno.events.store_client import _native_json
        output = _native_json(["schema-projection"], "schema projection")
    except Exception as exc:  # noqa: BLE001
        print(f"emit-schema error: {exc}", file=sys.stderr)
        sys.exit(1)
    print(json.dumps(output, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
