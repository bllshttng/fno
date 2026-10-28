#!/usr/bin/env python3
"""The fake fno-agents both config-set legs shell in the parity fixtures.

The Python writer and the native one reach the same doors: `claim status`,
`claim acquire`, `claim release`, `attest identity` and `pr-watch
install|uninstall`. A real binary answers from the host (live claims, the
process ancestry, launchd), so the fixture installs this script instead and
both legs read one deterministic answer. Claims live as JSON files under
`<fixture>/fake_claims/`; a case seeds one by writing the file first.
"""
from __future__ import annotations

import json
import os
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CLAIMS = ROOT / "fake_claims"
ACQUIRED_AT = 1700000000000


def claim_file(key: str) -> Path:
    return CLAIMS / (key.replace(":", "__").replace("/", "_") + ".json")


def flag(args: list[str], name: str) -> str | None:
    if name in args:
        index = args.index(name)
        if index + 1 < len(args):
            return args[index + 1]
    return None


def claim(args: list[str]) -> int:
    op, key = args[0], args[1] if len(args) > 1 else ""
    path = claim_file(key)
    if op == "root":
        print(json.dumps({"key": key, "root": str(CLAIMS)}))
        return 0
    if op == "status":
        if path.is_file():
            print(path.read_text(encoding="utf-8").strip())
        else:
            print(json.dumps({"key": key, "state": "free"}))
        return 0
    if op == "acquire":
        holder = flag(args, "--holder") or ""
        if path.is_file():
            held = json.loads(path.read_text(encoding="utf-8"))
            if held.get("holder") != holder:
                print(json.dumps({"outcome": "held_by_other", "holder": held.get("holder"), "pid": 4242, "host": "fixture-host"}))
                return 1
        ttl = int(flag(args, "--ttl-ms") or "0")
        metadata = json.loads(flag(args, "--metadata") or "{}")
        record = {
            "schema_version": 1,
            "key": key,
            "holder": holder,
            "acquired_at": ACQUIRED_AT,
            "pid": 4242,
            "host": "fixture-host",
            "expires_at": ACQUIRED_AT + ttl,
            "reason": flag(args, "--reason"),
            "metadata": metadata,
            "state": "live",
        }
        CLAIMS.mkdir(parents=True, exist_ok=True)
        path.write_text(json.dumps(record), encoding="utf-8")
        print(json.dumps({"claim": record}))
        return 0
    if op == "release":
        if not path.is_file():
            print(json.dumps({"released": False}))
            return 0
        record = json.loads(path.read_text(encoding="utf-8"))
        path.unlink()
        print(json.dumps({"released": True, "claim": record}))
        return 0
    print(f"fake fno-agents: claim {op} not faked", file=sys.stderr)
    return 2


def main() -> int:
    args = sys.argv[1:]
    with (ROOT / "fake_agents.log").open("a", encoding="utf-8") as log:
        # The caller's pid and the absolute binary paths are the host's; the
        # claim reads are the loader's, not the writer's.
        shown = ["<PID>" if i and args[i - 1] == "--pid" else a for i, a in enumerate(args)]
        if args[:2] not in (["claim", "status"], ["claim", "root"]):
            log.write(" ".join(a for a in shown if not a.startswith("/")) + "\n")
    if args[:1] == ["claim"]:
        return claim(args[1:])
    if args[:2] == ["attest", "identity"]:
        session = os.environ.get("FNO_FIXTURE_ATTESTER", "")
        print(json.dumps({"session_id": session, "witness": "env_only"}))
        return 0
    if args[:2] == ["pr-watch", "install"]:
        print("activated")
        return 0
    if args[:2] == ["pr-watch", "uninstall"]:
        print("unloaded")
        return 0
    print(f"fake fno-agents: {' '.join(args)} not faked", file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
