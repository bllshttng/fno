#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
USING_FNO="$ROOT/skills/using-fno/SKILL.md"
MAIL_SKILL="$ROOT/skills/mail/SKILL.md"
HOOK="$ROOT/hooks/session-start-using-fno.sh"

for file in "$USING_FNO" "$MAIL_SKILL"; do
  grep -Fq "## Relay compression contract" "$file"
  grep -Fq 'fno mux pane send' "$file"
  grep -Fq "80 words or fewer" "$file"
  grep -Fq "Keep technical terms" "$file"
done

# The hook injects only when fno-agents answers `due` for a joined session.
STUB_DIR="$(mktemp -d)"
trap 'rm -rf "$STUB_DIR"' EXIT
printf '#!/bin/sh\nexit 0\n' >"$STUB_DIR/silent"
printf '#!/bin/sh\necho due\n' >"$STUB_DIR/due"
chmod +x "$STUB_DIR/silent" "$STUB_DIR/due"
unjoined="$(FNO_AGENTS_BIN="$STUB_DIR/silent" CLAUDE_CODE_SESSION_ID=s1 CLAUDE_PLUGIN_ROOT="$ROOT" bash "$HOOK" </dev/null)"
[[ "$unjoined" == "{}" ]] || { echo "an unjoined session got a preamble: $unjoined"; exit 1; }

FNO_AGENTS_BIN="$STUB_DIR/due" CLAUDE_CODE_SESSION_ID=s1 CLAUDE_PLUGIN_ROOT="$ROOT" bash "$HOOK" </dev/null | python3 -c '
import json
import sys

payload = json.load(sys.stdin)
context = payload["hookSpecificOutput"]["additionalContext"]
required = (
    "## Relay compression contract",
    "fno agents mail send",
    "fno agents mail reply",
    "fno mux pane send",
    "80 words or fewer",
    "Keep technical terms",
)
missing = [item for item in required if item not in context]
if missing:
    raise SystemExit(f"missing from SessionStart context: {missing}")
'

echo "mail concision contract reaches SessionStart, mail, and mux surfaces"
