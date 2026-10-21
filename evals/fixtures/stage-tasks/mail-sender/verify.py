"""Envelope checks. Exit 0 only when every envelope carries its sender."""
import sys

from mailsend import REGISTRY, build_direct, build_resume

fails = []

envelope = build_resume({"lead": "lead-handle-1", "kind": "quiet-recovery"})
if envelope["from_name"] != REGISTRY["quiet-recovery"]:
    fails.append("resume envelope carries no sender name (or the wrong one)")
if envelope["to"] != "lead-handle-1":
    fails.append("resume envelope lost its recipient")

envelope = build_resume({"lead": "lead-handle-2", "kind": "watchdog"})
if envelope["from_name"] != REGISTRY["watchdog"]:
    fails.append("watchdog resume lost its sender name")

direct = build_direct("some-lead", "hello", "quiet-recovery")
if direct["from_name"] != REGISTRY["quiet-recovery"]:
    fails.append("direct path regressed")

for line in fails:
    print(f"FAIL: {line}")
sys.exit(1 if fails else 0)
