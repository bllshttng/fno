"""Fold-order checks. Exit 0 only when every precedence case holds."""
import sys

from fold_config import fold

fails = []

# The user enabled auto-merge in config; no run-level value was chosen.
merged = fold({"auto_merge": True})
if merged["auto_merge"] is not True:
    fails.append("user-enabled auto_merge was clobbered by the stamp")

# An explicit per-run refusal still wins over user config.
merged = fold({"auto_merge": True}, {"auto_merge": False})
if merged["auto_merge"] is not False:
    fails.append("explicit per-run False lost to user config")

# An explicit per-run grant wins over user config.
merged = fold({"auto_merge": False}, {"auto_merge": True})
if merged["auto_merge"] is not True:
    fails.append("explicit per-run True lost to user config")

# Silence falls back to the defaults.
merged = fold({}, None)
if merged["auto_merge"] is not False or merged["effort"] != "medium":
    fails.append("defaults no longer hold under total silence")

# Run-level extras still land.
merged = fold({}, {"effort": "high"})
if merged["effort"] != "high":
    fails.append("per-run extras stopped landing")

for line in fails:
    print(f"FAIL: {line}")
sys.exit(1 if fails else 0)
