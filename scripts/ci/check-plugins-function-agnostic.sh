#!/usr/bin/env bash
# Conformance gate: crates/fno-agents/src/plugins/ carries no branch on a pack
# id or a function name. A pack is discovered by path and validated by schema;
# core code that keys on "growth-studio" or on marketing/comms/design/social/
# support/sales/operations is the function-coupling defect this gate catches.
#
# It scans Rust string LITERALS in production code: `//` comment lines and
# everything from the first `#[cfg(test)]` on are skipped, so a doc comment
# that names the first pack is fine while a code path that compares against
# the same literal fails loudly with file:line.
#
# The scan is scoped to the plugins module only: pack content under plugins/
# and the fixtures under crates/fno-agents/tests/ legitimately name functions.
#
# Positive control: an empty result is a claim, so before trusting "no matches"
# the script confirms the scan reached real content by checking a literal known
# to appear in the module ("plugin.yaml"). A broken traversal fails closed.
set -euo pipefail

root="$(git rev-parse --show-toplevel 2>/dev/null || pwd)"
target="$root/crates/fno-agents/src/plugins"

if [[ ! -d "$target" ]]; then
  echo "check-plugins-function-agnostic: $target not found; the module must exist to be scanned (fail closed)" >&2
  exit 1
fi

python3 - "$target" <<'PY'
import re
import sys
from pathlib import Path

target = Path(sys.argv[1])
forbidden = [
    "growth-studio",
    "marketing",
    "communications",
    "comms",
    "design",
    "social",
    "support",
    "sales",
    "operations",
]
literal = re.compile(r'"((?:[^"\\]|\\.)*)"')

violations = []
literals_seen = []
for path in sorted(target.rglob("*.rs")):
    for lineno, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        stripped = line.strip()
        if stripped.startswith("#[cfg(test)]"):
            break
        if stripped.startswith("//"):
            continue
        for match in literal.finditer(line):
            value = match.group(1)
            literals_seen.append(value)
            hit = next((token for token in forbidden if token in value.lower()), None)
            if hit is not None:
                violations.append(f"{path}:{lineno}: forbidden literal {hit!r} in {value!r}")

if "plugin.yaml" not in literals_seen:
    print("check-plugins-function-agnostic: positive control failed (\"plugin.yaml\" not seen); scan did not reach content", file=sys.stderr)
    sys.exit(1)

if violations:
    print("check-plugins-function-agnostic: crates/fno-agents/src/plugins/ is not function-agnostic", file=sys.stderr)
    for line in violations:
        print("  " + line, file=sys.stderr)
    sys.exit(1)

print("check-plugins-function-agnostic: crates/fno-agents/src/plugins/ carries no pack or function literal")
PY
