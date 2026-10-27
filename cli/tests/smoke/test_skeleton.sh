#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
test -f cli/pyproject.toml
test -f cli/README.md
test -f cli/src/fno/__init__.py
test -f cli/src/fno/cli.py
# `gates` removed: the package was deleted by the control-plane collapse
# wedge (ab-d0337fbc); `claims` stands in as the structural probe.
# `worker` removed: the worker-family port retired the package; the parser
# answers from crates (`fno-agents executor-route`).
# `state` removed: the target-family port retired the package; the manifest
# contract is replayed by `crates/fno-agents/tests/target_family_parity.rs`.
for sub in graph runtime events claims; do
  test -f "cli/src/fno/${sub}/__init__.py"
done
echo "PASS: skeleton complete"
