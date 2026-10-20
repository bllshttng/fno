#!/usr/bin/env bash
# autocorrect-review.sh - send a packet to a fresh Claude API call, capture
# the patch list to ~/.claude/proposed-patches/{review_id}.md.
#
# Reads a packet (from stdin or --packet-file) produced by autocorrect-pack.sh,
# substitutes it into the prompt template at
# skills/autocorrect/references/autocorrect-prompts.md, calls the Claude API,
# and writes the response.
#
# Refuses to call the API if any implicated_rules entry is missing full_text -
# the reviewer would be operating blind. Refuses if ANTHROPIC_API_KEY is unset.
#
# Every run leaves a receipt: one start line and one end line in
# autocorrect-review.log (launchd merges stdout and stderr into that same
# file), one autocorrect_review_run event in the project journal, and one
# push channel line - an inbox notification when patches were proposed, an
# attention item naming the reason on a refusal. All three legs are
# best-effort; the receipt lines are the trace that never fails.
#
# Exit codes:
#   0  - review captured, patch list written
#   1  - API failure (rate limit, auth, network); watermark NOT advanced
#   2  - validation failure (missing key, missing rule text, malformed packet)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
PROMPT_FILE="${AUTOCORRECT_PROMPT_FILE:-$REPO_ROOT/skills/autocorrect/references/autocorrect-prompts.md}"

CLAUDE_DIR="${CLAUDE_DIR_OVERRIDE:-$HOME/.claude}"
PATCHES_DIR="$CLAUDE_DIR/proposed-patches"

LOG_DIR="${AUTOCORRECT_LOG_DIR:-$HOME/.claude/logs}"
LOG_FILE="$LOG_DIR/autocorrect-review.log"

MODEL="${AUTOCORRECT_MODEL:-claude-sonnet-4-6}"
PACKET_FILE=""
SEVERITY_LABEL=""
PACKET_SOURCE="manual"
MAX_TOKENS=4096
API_URL="${ANTHROPIC_API_URL:-https://api.anthropic.com/v1/messages}"
ANTHROPIC_VERSION="2023-06-01"

# Receipt state. INPUT_COUNT / PATCH_COUNT / OUTPUT_PATH / REASON fill in as
# the run progresses; the EXIT trap reads whatever is set by then.
REASON=""
INPUT_COUNT=""
PATCH_COUNT=""
OUTPUT_PATH=""
START_WRITTEN=0
TMPDIR_R=""

write_receipt() {
  # The receipt must never flip the run's exit code or abort the trap.
  printf '%s\n' "$1" >> "$LOG_FILE" 2>/dev/null || true
}

write_start_receipt() {
  START_WRITTEN=1
  # The inline call at packet-read time runs before the trap's mkdir, so the
  # first receipt of a fresh log dir must create it.
  mkdir -p "$LOG_DIR" 2>/dev/null || true
  write_receipt "$(date -u +%Y-%m-%dT%H:%M:%SZ) autocorrect-review run start source=${PACKET_SOURCE} inputs=${INPUT_COUNT:--}"
}

refuse() {
  # refuse <exit-code> <reason>: print the refusal line the old exits
  # printed, name it for the end receipt, exit.
  REASON="$2"
  echo "autocorrect-review: $2" >&2
  exit "$1"
}

# run_fno <args...>: run the fno CLI from the best available substrate.
# FNO_BIN (the same override scripts/lib/events.sh honors) wins, so a
# hermetic run or a test can point the visibility legs at a stub; then the
# checkout venv, then fno-py / fno on PATH. Returns 127 when none exists,
# which every caller treats as skip.
run_fno() {
  if [[ -n "${FNO_BIN:-}" ]]; then
    "$FNO_BIN" "$@"
    return
  fi
  if [[ -x "$REPO_ROOT/cli/.venv/bin/python" ]]; then
    PYTHONPATH="$REPO_ROOT/cli/src" "$REPO_ROOT/cli/.venv/bin/python" -m fno.cli "$@"
    return
  fi
  if command -v fno-py >/dev/null 2>&1; then
    fno-py "$@"
    return
  fi
  command -v fno >/dev/null 2>&1 || return 127
  fno "$@"
}

emit_run_event() {
  # One autocorrect_review_run row per run, through the existing emitter
  # verb. The row is judged against the schema's event_types registry, so
  # this dies loudly in dev if the type drifts; under launchd with no fno
  # substrate it is a skip.
  local rc="$1" outcome="failed" data
  if [[ "$rc" == "0" ]]; then
    outcome="ok"
  elif [[ "$rc" == "2" ]]; then
    outcome="refused"
  fi
  command -v jq >/dev/null 2>&1 || return 0
  data=$(jq -cn \
    --arg outcome "$outcome" \
    --argjson exit_code "$rc" \
    --arg packet_source "$PACKET_SOURCE" \
    --argjson input_count "${INPUT_COUNT:-null}" \
    --argjson patch_count "${PATCH_COUNT:-null}" \
    --arg output_path "${OUTPUT_PATH:-}" \
    --arg reason "${REASON:-}" \
    '{
      outcome: $outcome,
      exit_code: $exit_code,
      packet_source: $packet_source,
      input_count: ($input_count | select(type == "number")),
      patch_count: ($patch_count | select(type == "number")),
      output_path: ($output_path | select(length > 0)),
      reason: ($reason | select(length > 0))
    } | with_entries(select(.value != null))') || return 0
  (cd "$REPO_ROOT" && run_fno doctor event emit -t autocorrect_review_run -s bash -d "$data") >/dev/null 2>&1 || true
}

notify_operator() {
  # Step 5: one inbox item naming the output path when patches landed, one
  # attention item naming the reason on a REFUSAL (exit 2) only. An API
  # failure (exit 1) stays in the receipts and the event row: the S0 watcher
  # retries it within 15 minutes, and a pin per transient failure would
  # spam the operator queue through every outage.
  local rc="$1"
  if [[ "$rc" == "0" ]]; then
    [[ -n "${PATCH_COUNT:-}" && "$PATCH_COUNT" -gt 0 ]] || return 0
    run_fno inbox notify "autocorrect review" \
      "$PATCH_COUNT patches proposed: $OUTPUT_PATH" \
      --pointer "$OUTPUT_PATH" >/dev/null 2>&1 || true
    return 0
  fi
  if [[ "$rc" != "2" ]]; then
    return 0
  fi
  local reason="${REASON:-validation refusal (see stderr)}"
  run_fno inbox outstanding ask "$reason" \
    --subject "autocorrect review" \
    --ask "clear once the launchd key or the packet problem is fixed" >/dev/null 2>&1 || true
}

finish_run() {
  # The one EXIT trap every exit path flows through: the end receipt, the
  # event row, the push channel, and the tmp cleanup the old trap did.
  local rc=$?
  mkdir -p "$LOG_DIR" 2>/dev/null || true
  if [[ "$START_WRITTEN" != "1" ]]; then
    write_start_receipt
  fi
  if [[ "$rc" == "0" ]]; then
    write_receipt "$(date -u +%Y-%m-%dT%H:%M:%SZ) autocorrect-review run end exit=0 patches=${PATCH_COUNT:-0} out=${OUTPUT_PATH:-}"
  else
    local outcome="failed"
    if [[ "$rc" == "2" ]]; then
      outcome="refused"
    fi
    write_receipt "$(date -u +%Y-%m-%dT%H:%M:%SZ) autocorrect-review run end exit=${rc} outcome=${outcome} reason=${REASON:-unspecified (see stderr)}"
  fi
  emit_run_event "$rc"
  notify_operator "$rc"
  if [[ -n "$TMPDIR_R" && -d "$TMPDIR_R" ]]; then
    rm -rf "$TMPDIR_R"
  fi
  return 0
}

usage() {
  cat >&2 <<'EOF'
Usage: autocorrect-review.sh [--packet-file <path>] [--model <id>] [--severity S0|S1|S2]

Options:
  --packet-file <path>    read packet from file (default: stdin)
  --model <id>            override model id (default claude-sonnet-4-6)
  --severity <tier>       label the review id; informational only
  --packet-source <id>    identifier for where the packet came from
                          (e.g. verifier-inline, watcher, manual)
  --max-tokens <n>        max response tokens (default 4096)

Env:
  ANTHROPIC_API_KEY       required
  AUTOCORRECT_MODEL       overrides default model
  AUTOCORRECT_PROMPT_FILE overrides the prompt template path
  ANTHROPIC_API_URL       overrides the API endpoint (for testing)
  AUTOCORRECT_LOG_DIR     overrides the receipt log directory
EOF
  exit 2
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --packet-file)   PACKET_FILE="${2:-}"; shift 2 ;;
    --model)         MODEL="${2:-}"; shift 2 ;;
    --severity)      SEVERITY_LABEL="${2:-}"; shift 2 ;;
    --packet-source) PACKET_SOURCE="${2:-}"; shift 2 ;;
    --max-tokens)    MAX_TOKENS="${2:-}"; shift 2 ;;
    -h|--help)       usage ;;
    *) echo "autocorrect-review: unknown argument: $1" >&2; usage ;;
  esac
done

# From here on the invocation is a run: every exit leaves its receipt pair.
trap finish_run EXIT

if [[ -z "${ANTHROPIC_API_KEY:-}" ]]; then
  refuse 2 "ANTHROPIC_API_KEY env var is required"
fi

if [[ ! -f "$PROMPT_FILE" ]]; then
  refuse 2 "prompt template not found at $PROMPT_FILE"
fi

if ! command -v curl >/dev/null 2>&1; then
  refuse 2 "curl is required"
fi
if ! command -v jq >/dev/null 2>&1; then
  refuse 2 "jq is required"
fi

# Read packet from file or stdin.
TMPDIR_R="$(mktemp -d)"
PACKET_TMP="$TMPDIR_R/packet.yaml"
if [[ -n "$PACKET_FILE" ]]; then
  if [[ ! -f "$PACKET_FILE" ]]; then
    refuse 2 "--packet-file not found: $PACKET_FILE"
  fi
  cp "$PACKET_FILE" "$PACKET_TMP"
else
  cat > "$PACKET_TMP"
fi

if [[ ! -s "$PACKET_TMP" ]]; then
  refuse 2 "packet is empty"
fi

# The packet is in hand: the start receipt can name its size. The packer's
# event_count field is the honest count; a hand-rolled packet falls back to
# counting the events list entries.
INPUT_COUNT="$(sed -n 's/^event_count: //p' "$PACKET_TMP" | head -1)"
if [[ -z "$INPUT_COUNT" ]]; then
  INPUT_COUNT="$(grep -c '^  - timestamp:' "$PACKET_TMP" || true)"
fi
write_start_receipt

# Validate: every implicated_rules entry must have a non-empty full_text.
# Missing full_text means the reviewer would be operating blind on that
# file. The previous version counted occurrences globally, which was
# fragile to rule files whose contents happened to include a YAML-shaped
# `full_text:` line (e.g. a rule documenting the packet format itself).
# Walk the YAML structurally with python's parser instead.
PACKET_PATH="$PACKET_TMP" python3 - <<'PY' >&2 || { REASON="rule text not provided for all implicated files"; exit 2; }
import os, sys, re

with open(os.environ["PACKET_PATH"]) as f:
    text = f.read()

# Find the implicated_rules: block start and read until the next
# top-level key (no leading indent). Inside the block, each entry begins
# with "  - file:" at two-space indent and SHOULD be followed by a
# "    full_text:" at four-space indent within the same entry.
m = re.search(r"^implicated_rules:\s*$(.*?)(^\S|\Z)", text, re.MULTILINE | re.DOTALL)
if not m:
    # No implicated_rules block at all - nothing to validate.
    sys.exit(0)

block = m.group(1)
# Empty list literal "[]" on the same line as the heading is rendered by
# pack.sh when there are no entries; tolerate that.
if block.strip() == "[]":
    sys.exit(0)

# Split into entries by the "  - file:" marker.
entries = re.split(r"^  - file:\s*", block, flags=re.MULTILINE)[1:]
missing = []
for entry in entries:
    # Path is the first line of the entry (until newline).
    path = entry.split("\n", 1)[0].strip()
    # Look for a "    full_text:" line within the entry's indent block,
    # before the next "  - file:" marker (already partitioned by split).
    has_text = re.search(r"^    full_text:", entry, re.MULTILINE)
    if not has_text:
        missing.append(path)

if missing:
    print(
        "autocorrect-review: rule text not provided for all implicated files; "
        "reviewer would be operating blind", file=sys.stderr,
    )
    for p in missing:
        print(f"  - missing full_text: {p}", file=sys.stderr)
    sys.exit(2)
PY

# Extract prompt body between markers.
PROMPT_BODY="$TMPDIR_R/prompt.txt"
awk '/<!-- PROMPT_START -->/{found=1; next} /<!-- PROMPT_END -->/{found=0} found' "$PROMPT_FILE" > "$PROMPT_BODY"
if [[ ! -s "$PROMPT_BODY" ]]; then
  refuse 2 "failed to extract prompt body from $PROMPT_FILE"
fi

# Substitute {PACKET} with the packet contents. Use a sentinel + python so
# pipe characters / special chars in the packet don't break sed.
FILLED_PROMPT="$TMPDIR_R/filled.txt"
PACKET_PATH="$PACKET_TMP" PROMPT_PATH="$PROMPT_BODY" python3 - <<'PY' > "$FILLED_PROMPT"
import os
with open(os.environ["PROMPT_PATH"]) as f:
    prompt = f.read()
with open(os.environ["PACKET_PATH"]) as f:
    packet = f.read()
print(prompt.replace("{PACKET}", packet))
PY

# Compute review_id (matches packet builder's scheme if possible).
REVIEW_ID=$(grep -E '^review_id: ' "$PACKET_TMP" | head -1 | sed 's/^review_id: //' || true)
if [[ -z "$REVIEW_ID" ]]; then
  REVIEW_ID="$(date -u +%Y-%m-%dT%H%M%SZ)-${SEVERITY_LABEL:-manual}"
fi

# Build JSON body via jq for safety.
REQUEST_BODY="$TMPDIR_R/request.json"
jq -n \
  --arg model "$MODEL" \
  --argjson max_tokens "$MAX_TOKENS" \
  --rawfile prompt "$FILLED_PROMPT" \
  '{model: $model, max_tokens: $max_tokens, messages: [{role: "user", content: $prompt}]}' \
  > "$REQUEST_BODY"

# Call the API.
RESPONSE_BODY="$TMPDIR_R/response.json"
HTTP_CODE=$(curl -sS -o "$RESPONSE_BODY" -w "%{http_code}" \
  -X POST "$API_URL" \
  -H "x-api-key: $ANTHROPIC_API_KEY" \
  -H "anthropic-version: $ANTHROPIC_VERSION" \
  -H "content-type: application/json" \
  -d @"$REQUEST_BODY" \
  || true)

if [[ "$HTTP_CODE" != "200" ]]; then
  if [[ -s "$RESPONSE_BODY" ]]; then
    jq -r '.error.message // .' "$RESPONSE_BODY" 2>/dev/null >&2 || cat "$RESPONSE_BODY" >&2
  fi
  echo "autocorrect-review: watermark NOT advanced; rerun with same window to retry" >&2
  refuse 1 "API call failed with HTTP ${HTTP_CODE}"
fi

# Extract text content + usage stats.
PATCH_TEXT="$TMPDIR_R/patches.md"
jq -r '.content[]? | select(.type == "text") | .text' "$RESPONSE_BODY" > "$PATCH_TEXT" || {
  echo "autocorrect-review: failed to parse API response" >&2
  jq . "$RESPONSE_BODY" 2>&1 | head -20 >&2
  refuse 1 "failed to parse API response"
}

INPUT_TOKENS=$(jq -r '.usage.input_tokens // 0' "$RESPONSE_BODY")
OUTPUT_TOKENS=$(jq -r '.usage.output_tokens // 0' "$RESPONSE_BODY")

# Persist patch list to proposed-patches/.
mkdir -p "$PATCHES_DIR"
OUTPUT_PATH="$PATCHES_DIR/${REVIEW_ID}.md"
{
  echo "# Autocorrect review: $REVIEW_ID"
  echo
  echo "- generated_at: $(date -u +"%Y-%m-%dT%H:%M:%SZ")"
  echo "- model: $MODEL"
  echo "- packet_source: $PACKET_SOURCE"
  echo "- input_tokens: $INPUT_TOKENS"
  echo "- output_tokens: $OUTPUT_TOKENS"
  echo
  echo "---"
  echo
  cat "$PATCH_TEXT"
} > "$OUTPUT_PATH"
chmod 600 "$OUTPUT_PATH" 2>/dev/null || true

# Count patches: simple heuristic - look for "N. Source:" headings.
PATCH_COUNT=$(grep -cE '^[0-9]+\. Source:' "$PATCH_TEXT" || true)

echo "autocorrect-review: $PATCH_COUNT patches proposed; saved to $OUTPUT_PATH" >&2
echo "autocorrect-review: usage: ${INPUT_TOKENS} in / ${OUTPUT_TOKENS} out tokens" >&2

# Print summary to stdout (callers can pipe).
printf '%s\n' "$OUTPUT_PATH"
