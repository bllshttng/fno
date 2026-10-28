#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)/cli"
uv sync --quiet

# The `do state show` probe died with the target-family port. The root
# --json contract now rides the `event gc` leaf: a dry run over a temp
# journal honors the root flag and prints parseable JSON on stdout (the
# bare-spelling move notice goes to stderr).
TMPDIR_STATE=$(mktemp -d)
trap 'rm -rf "$TMPDIR_STATE"' EXIT

: > "$TMPDIR_STATE/events.jsonl"

err=$(mktemp)
out=$(uv run fno-py --json event gc --events "$TMPDIR_STATE/events.jsonl" --dry-run 2>"$err") || {
  echo "FAIL: --json event gc exited non-zero"; echo "$out"; cat "$err"; rm -f "$err"; exit 1;
}
printf '%s' "$out" | python3 -m json.tool > /dev/null || {
  echo "FAIL: --json event gc did not produce parseable JSON"; echo "$out"; cat "$err"; rm -f "$err"; exit 1;
}
rm -f "$err"
echo "PASS: --json flag produces parseable JSON"
