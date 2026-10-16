#!/usr/bin/env bash
# Blueprint shapes types and owners before it cuts tasks.
#
# Specimen (main red 2026-10-09): 4a44862870 declared
# `SUPERVISOR_WIDE_FNO_KEYS: [&str; 15]`, a type that restates a count its
# data already holds. 23 later commits edited the length, five only to fix
# it, and one concurrent key add broke the main build (E0308). f08f92ce44
# made it a slice. Nobody shaped the type before the tasks that added keys.
#
# The skill is prose, so the contract is the prompt bytes a /blueprint run
# reads: the design step sits before the write, and its Types rule rejects
# the specimen's shape.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SKILL="$SCRIPT_DIR/.."
MAIN="$SKILL/SKILL.md"
TEMPLATE="$SKILL/references/quick-template.md"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }
line_of() { grep -nF -- "$2" "$1" | head -1 | cut -d: -f1; }
heading_of() { grep -nxF -- "$2" "$1" | head -1 | cut -d: -f1; }

test_design_precedes_tasks() {
  echo "== step 2e runs after consolidation and before the plan and its tasks are written"
  local consolidate design write
  consolidate="$(line_of "$MAIN" '2d. **Consolidation Gate**')"
  design="$(line_of "$MAIN" '2e. **Shape the design before the tasks**')"
  write="$(line_of "$MAIN" '3. **Write** the plan.')"
  if [ -n "$design" ] && [ "$consolidate" -lt "$design" ] && [ "$design" -lt "$write" ] \
    && grep -qF -- 'A type never restates a count its data already holds.' "$MAIN"; then
    pass "2d ($consolidate) < 2e ($design) < 3 ($write), and its Types rule rules out the fixed length"
  else
    fail "design step missing, out of order, or lost the restated-count rule (2d=$consolidate 2e=$design 3=$write)"
  fi
}

test_template_places_design_before_changes() {
  echo "== both templates put ## Design between Context and Changes"
  local f ctx des chg ok=1
  for f in "$MAIN" "$TEMPLATE"; do
    ctx="$(heading_of "$f" '## Context')"
    des="$(heading_of "$f" '## Design')"
    chg="$(heading_of "$f" '## Changes')"
    if [ -z "$des" ] || [ "$ctx" -ge "$des" ] || [ "$des" -ge "$chg" ]; then ok=0; fi
  done
  if [ "$ok" -eq 1 ]; then
    pass "Design sits before Changes in SKILL.md and quick-template.md"
  else
    fail "a template lost ## Design or moved it after Changes"
  fi
}

test_design_precedes_tasks
test_template_places_design_before_changes

echo
echo "design-step: $PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
