#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)/cli"
uv sync --quiet

# The root --json contract rides the `backlog get` leaf: a read of an id that
# cannot exist is a miss (rc 1), and a miss still prints one parseable JSON
# document on stdout.
err=$(mktemp)
trap 'rm -f "$err"' EXIT

out=$(uv run fno-py --json backlog get x-0000 2>"$err") || true
printf '%s' "$out" | python3 -m json.tool > /dev/null || {
  echo "FAIL: --json backlog get did not produce parseable JSON"; echo "$out"; cat "$err"; exit 1;
}
echo "PASS: --json flag produces parseable JSON"
