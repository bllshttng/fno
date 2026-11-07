#!/usr/bin/env bash
# Repo-wide Python static step. CI (guards.yml job main-python-static) and the
# merge-result probe run this one file; `fno doctor test` does not, which is
# why that command states this gap in its own output.
# $1 is the directory to check (default: tests/py next to this repo root).
# The product ships no Python; what is left is dev-only test code, so the
# check is ruff's syntax and undefined-name rules, with no type pass.
set -euo pipefail
export NO_COLOR=1
dir="${1:-$(cd "$(dirname "$0")/../.." && pwd)/tests/py}"
cd "$dir"
python_files="$(find . -type f -name '*.py' -print | wc -l | tr -d '[:space:]')"
# Zero files means the target moved, not that the code is clean.
if [ "$python_files" -eq 0 ]; then
    echo "python-static: no Python files under $dir; the target drifted and the check is vacuous" >&2
    exit 1
fi
${RUFF:-uvx ruff} check --no-respect-gitignore --color=never --output-format=concise \
    --isolated --select E9,F63,F7,F82 .
echo "python-static: checked ${python_files} Python files with ruff"
