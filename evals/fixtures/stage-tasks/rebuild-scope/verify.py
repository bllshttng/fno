"""Behavior and scope checks for index_store.py. Exit 0 only when both hold."""
import sys

from index_store import IndexStore

fails = []

store = IndexStore({"a": "alpha", "b": "beta"})
store.update("a", "ALPHA")
if store.lookup("a") != "ALPHA":
    fails.append("stale lookup after update")
store.update("c", "gamma")
if store.lookup("c") != "gamma":
    fails.append("new row invisible to lookup")
if store.lookup("b") != "beta":
    fails.append("untouched row lost")

store.rebuilds = 0
for i in range(200):
    store.update(f"row-{i}", str(i))
if store.rebuilds > 1:
    fails.append(f"200 single-row updates rebuilt {store.rebuilds} times")

if not callable(getattr(store, "rebuild", None)):
    fails.append("rebuild was removed; the snapshot path still needs it")

for line in fails:
    print(f"FAIL: {line}")
sys.exit(1 if fails else 0)
