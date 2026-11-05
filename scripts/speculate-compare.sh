#!/usr/bin/env bash
# Start dev servers for all completed speculate variations
# Usage: speculate-compare.sh [state-file]
set -uo pipefail

STATE_FILE="${1:-.fno/speculate-state.json}"

if [[ ! -f "$STATE_FILE" ]]; then
    echo "No active speculation found (missing $STATE_FILE)"
    exit 1
fi

# Read state (pass file path as argv to avoid injection)
PORT_START=$(jq -r 'if has("port_start") then .port_start else 3001 end' "$STATE_FILE" 2>/dev/null || echo "3001")

VARIATIONS=$(jq -r '
    (.variations // {}) | to_entries | sort_by(.key)[]
    | select(.value.status == "complete")
    | "\(.key)|\(.value | if has("worktree") then .worktree else error("no worktree") end)|\(.value | if has("constraint") then .constraint else "default" end)"
' "$STATE_FILE" 2>/dev/null)

if [[ -z "$VARIATIONS" ]]; then
    echo "No completed variations found"
    exit 1
fi

# Find dev command
DEV_CMD="npm run dev"
if [[ -f "pnpm-lock.yaml" ]]; then
    DEV_CMD="pnpm dev"
elif [[ -f "bun.lockb" ]]; then
    DEV_CMD="bun dev"
fi

PORT=$PORT_START
PIDS=()
URLS=()

echo "Starting dev servers for comparison..."
echo ""

while IFS='|' read -r variant worktree constraint; do
    [[ -z "$variant" ]] && continue
    echo "  $variant ($constraint): http://localhost:$PORT  ($worktree)"

    # Start dev server in background (exec to prevent orphan processes)
    # Log to file so startup errors are visible via: cat .fno/speculate-*.log
    local logfile=".fno/speculate-${variant}.log"
    (cd "$worktree" && exec env PORT=$PORT $DEV_CMD >> "$logfile" 2>&1) &
    PIDS+=($!)
    URLS+=("http://localhost:$PORT")

    PORT=$((PORT + 1))
done <<< "$VARIATIONS"

echo ""
echo "All servers started. Compare in your browser:"
echo ""

# Open browser tabs (macOS)
for url in "${URLS[@]}"; do
    echo "  $url"
    if command -v open >/dev/null 2>&1; then
        open "$url" 2>/dev/null || true
    elif command -v xdg-open >/dev/null 2>&1; then
        xdg-open "$url" 2>/dev/null || true
    fi
done

echo ""
echo "Press Enter to stop all servers..."
read -r

# Cleanup
for pid in "${PIDS[@]}"; do
    kill "$pid" 2>/dev/null
done
echo "Servers stopped."
