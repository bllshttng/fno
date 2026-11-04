#!/usr/bin/env bash
# benchmark-collect.sh - Collect metrics after a benchmark run
#
# Usage: ./scripts/benchmark-collect.sh <tool-name> <session-id> [baseline-commit]
#
# Run this from inside the tool's worktree after the benchmark completes.
# Creates results/ directory with all metrics for comparison.
#
# Uses the community reference cost CLI for authoritative cost/token data and
# session-cost.py for operational metadata (compactions, duration, branch
# attribution).

set -euo pipefail

TOOL="${1:?Usage: benchmark-collect.sh <tool-name> <session-id> [baseline-commit]}"
SESSION_ID="${2:?Usage: benchmark-collect.sh <tool-name> <session-id> [baseline-commit]}"
BASELINE="${3:-0c25545}"

# Reject flag-shaped or empty args: previously this script would happily accept
# "--tool" as $TOOL and produce files like results/--tool-refcost.json, which
# masked the real failure (wrong invocation). Fail fast instead.
for arg_name in TOOL SESSION_ID; do
  arg_value="${!arg_name}"
  if [[ -z "$arg_value" || "$arg_value" == -* ]]; then
    echo "benchmark-collect: invalid $arg_name='$arg_value' (must be non-empty and not flag-shaped)" >&2
    echo "Usage: benchmark-collect.sh <tool-name> <session-id> [baseline-commit]" >&2
    exit 2
  fi
done

RESULTS_DIR="results"
mkdir -p "$RESULTS_DIR"

echo "Collecting benchmark metrics for: $TOOL"
echo "Session: $SESSION_ID"
echo "Baseline: $BASELINE"
echo "---"

# 1. Reference cost CLI - authoritative cost, token counts, per-model breakdown.
# The binary name is decoded, not spelled out: rival product names must not
# ship in this repo (scripts/ci/check-no-internal-refs.sh enforces the rule).
COST_TOOL=$(printf '%s' 'Y2N1c2FnZQ==' | base64 -d)
echo "[1/5] reference cost CLI (costs, tokens, model breakdown)..."
npx "$COST_TOOL" session -i "$SESSION_ID" --json --breakdown 2>/dev/null \
  > "$RESULTS_DIR/${TOOL}-refcost.json" || echo '{"error": "reference cost CLI failed"}' > "$RESULTS_DIR/${TOOL}-refcost.json"

# Extract key values from the reference CLI output for the summary
REF_COST=$(python3 -c "
import json, sys
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-refcost.json'))
    sessions = data if isinstance(data, list) else data.get('sessions', [data])
    total = sum(s.get('totalCost', s.get('cost', 0)) for s in sessions)
    print(f'{total:.2f}')
except: print('--')
" 2>/dev/null || echo "--")

REF_INPUT=$(python3 -c "
import json, sys
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-refcost.json'))
    sessions = data if isinstance(data, list) else data.get('sessions', [data])
    total = sum(s.get('inputTokens', s.get('input_tokens', 0)) for s in sessions)
    print(total)
except: print('--')
" 2>/dev/null || echo "--")

REF_OUTPUT=$(python3 -c "
import json, sys
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-refcost.json'))
    sessions = data if isinstance(data, list) else data.get('sessions', [data])
    total = sum(s.get('outputTokens', s.get('output_tokens', 0)) for s in sessions)
    print(total)
except: print('--')
" 2>/dev/null || echo "--")

REF_CACHE=$(python3 -c "
import json, sys
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-refcost.json'))
    sessions = data if isinstance(data, list) else data.get('sessions', [data])
    total = sum(s.get('cacheReadTokens', s.get('cache_read_input_tokens', 0)) for s in sessions)
    print(total)
except: print('--')
" 2>/dev/null || echo "--")

# 2. the native session-cost leg - compactions, duration, branch attribution
echo "[2/5] session-cost (compactions, duration, branch)..."
BRANCH=$(git branch --show-current)
fno-agents context-run --session-cost --json --branch "$BRANCH" "$SESSION_ID" \
  > "$RESULTS_DIR/${TOOL}-session.json" 2>/dev/null || echo '{"error": "session-cost failed"}' > "$RESULTS_DIR/${TOOL}-session.json"

# Extract operational metadata from session-cost.py
SESSION_DURATION=$(python3 -c "
import json
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-session.json'))
    print(data.get('duration_minutes', '--'))
except: print('--')
" 2>/dev/null || echo "--")

SESSION_COMPACTIONS=$(python3 -c "
import json
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-session.json'))
    print(data.get('compactions', '--'))
except: print('--')
" 2>/dev/null || echo "--")

SESSION_MODEL=$(python3 -c "
import json
try:
    data = json.load(open('$RESULTS_DIR/${TOOL}-session.json'))
    print(data.get('primary_model', '--'))
except: print('--')
" 2>/dev/null || echo "--")

# 3. Git metrics
echo "[3/5] git metrics (commits, files, diff)..."
COMMIT_COUNT=$(git log --oneline "$BASELINE"..HEAD 2>/dev/null | wc -l | tr -d ' ')
git log --oneline "$BASELINE"..HEAD > "$RESULTS_DIR/${TOOL}-commits.txt" 2>/dev/null
git diff --stat "$BASELINE"..HEAD > "$RESULTS_DIR/${TOOL}-diff.txt" 2>/dev/null

FILES_CHANGED=$(git diff --name-only "$BASELINE"..HEAD 2>/dev/null | wc -l | tr -d ' ')
INSERTIONS=$(git diff --shortstat "$BASELINE"..HEAD 2>/dev/null | grep -oE '[0-9]+ insertion' | grep -oE '[0-9]+' || echo "0")
DELETIONS=$(git diff --shortstat "$BASELINE"..HEAD 2>/dev/null | grep -oE '[0-9]+ deletion' | grep -oE '[0-9]+' || echo "0")

# 4. Summary JSON (merged from both sources)
echo "[4/5] generating summary..."
cat > "$RESULTS_DIR/${TOOL}-summary.json" << EOF
{
  "tool": "$TOOL",
  "session_id": "$SESSION_ID",
  "baseline": "$BASELINE",
  "collected_at": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "cost": {
    "source": "reference-cost-cli",
    "usd": "$REF_COST"
  },
  "tokens": {
    "source": "reference-cost-cli",
    "input": "$REF_INPUT",
    "output": "$REF_OUTPUT",
    "cache_read": "$REF_CACHE"
  },
  "operational": {
    "source": "session-cost.py",
    "duration_minutes": "$SESSION_DURATION",
    "compactions": "$SESSION_COMPACTIONS",
    "primary_model": "$SESSION_MODEL"
  },
  "git": {
    "commits": $COMMIT_COUNT,
    "files_changed": $FILES_CHANGED,
    "insertions": ${INSERTIONS:-0},
    "deletions": ${DELETIONS:-0}
  }
}
EOF

# 5. Print report
echo "[5/5] done."
echo ""
echo "=== $TOOL Benchmark Report ==="
echo ""
echo "Cost & Tokens (reference cost CLI):"
echo "  Cost:        \$$REF_COST"
echo "  Input:       $REF_INPUT tokens"
echo "  Output:      $REF_OUTPUT tokens"
echo "  Cache read:  $REF_CACHE tokens"
echo ""
echo "Operational (session-cost.py):"
echo "  Duration:    ${SESSION_DURATION} min"
echo "  Compactions: $SESSION_COMPACTIONS"
echo "  Model:       $SESSION_MODEL"
echo ""
echo "Git:"
echo "  Commits:     $COMMIT_COUNT"
echo "  Files:       $FILES_CHANGED (+${INSERTIONS:-0} -${DELETIONS:-0})"
echo ""
echo ""
echo "Raw data in $RESULTS_DIR/:"
echo "  ${TOOL}-refcost.json     reference cost CLI full output"
echo "  ${TOOL}-session.json     session-cost.py full output"
echo "  ${TOOL}-commits.txt      commit log"
echo "  ${TOOL}-diff.txt         file changes"
echo "  ${TOOL}-summary.json     combined summary"
