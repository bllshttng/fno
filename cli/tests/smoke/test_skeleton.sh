#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
test -f cli/pyproject.toml
test -f cli/README.md
test -f cli/src/fno/__init__.py
test -f cli/src/fno/cli.py
# `gates` removed: the package was deleted by the control-plane collapse.
# `claims` removed: the claims port moved the family into crates/fno-agents.
# `worker` removed: the worker-family port retired the package; the parser
# answers from crates (`fno-agents executor-route`).
# `runtime` removed: the worktree-family port moved its registry into
# crates/fno-agents/src/worktree/.
# `state` removed: the target-family port retired the package; the manifest
# contract is replayed by `crates/fno-agents/tests/target_family_parity.rs`.
# `graph` removed: the graph core port moved it into crates/fno-agents.
for sub in events; do
  test -f "cli/src/fno/${sub}/__init__.py"
done
echo "PASS: skeleton complete"
