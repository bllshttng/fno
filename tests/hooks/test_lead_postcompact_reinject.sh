#!/usr/bin/env bash
# test_lead_postcompact_reinject.sh
#
# Unit tests for hooks/lead-postcompact-reinject.sh: the post-compaction
# pointers a lead reads. Verifies: the teamed claude case names the brief,
# canon doc and FAQ files through hookSpecificOutput.additionalContext and
# pastes none of their text; the teamed codex case delivers through
# systemMessage; unpromoted rows, unknown sessions, non-compact sources, and a
# missing fno all degrade to empty output with exit 0; and the brief stays
# inside its byte budget (every lead reads it on every compaction).

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
LEAD="$REPO_ROOT/hooks/lead-postcompact-reinject.sh"
BRIEF="$REPO_ROOT/skills/lead/references/postcompact-brief.md"
# Keep the cap fixed: the brief must fit without changing the budget.
BRIEF_MAX_BYTES=1800

[[ -f "$LEAD" ]] || { echo "FAIL: lead hook not found at $LEAD" >&2; exit 1; }
export CLAUDE_PLUGIN_ROOT="$REPO_ROOT"

PASS=0
FAIL=0
pass() { echo "  PASS: $*"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $*"; FAIL=$((FAIL + 1)); }

TMP="$(mktemp -d -t lead-reinject-XXXXXX)"
trap 'rm -rf "$TMP"' EXIT

# Stub `fno` answering `agents internal registry-json` from a per-case fixture file, and
# `agents lead faq list --scope X` from a second fixture keyed by scope, so no
# real registry, daemon, or FAQ store is involved. $LEAD_REG_FIXTURE and
# $LEAD_FAQ_FIXTURE select the payloads; the FAQ fixture is empty (no output)
# unless a test overwrites it.
mkdir -p "$TMP/bin"
cat > "$TMP/bin/fno" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = "agents" ] && [ "$2" = "internal" ] && [ "$3" = "registry-json" ]; then
  # Mirror the real Rust client: the command refuses while the anti-recursion
  # pin is set (a leaked FNO_AGENTS_RUNTIME=python). The hook must
  # strip the pin before the read.
  if [ -n "${FNO_AGENTS_RUNTIME:-}" ]; then
    echo "no Python implementation -- this command runs only on the 'fno-agents' Rust runtime" >&2
    exit 127
  fi
  cat "$LEAD_REG_FIXTURE"
elif [ "$1" = "agents" ] && [ "$2" = "lead" ] && [ "$3" = "faq" ] && [ "$4" = "list" ]; then
  cat "$LEAD_FAQ_FIXTURE" 2>/dev/null || true
elif [ "$1" = "agents" ] && [ "$2" = "org" ] && [ "$3" = "faq" ] && [ "$4" = "list" ]; then
  # The canonical org spelling forwards the same action.
  cat "$LEAD_FAQ_FIXTURE" 2>/dev/null || true
elif [ "$1" = "config" ] && [ "$2" = "paths" ] && [ "$3" = "handoff" ]; then
  cat "$LEAD_HANDOFF_PATH_FIXTURE" 2>/dev/null || true
elif [ "$1" = "backlog" ] && [ "$2" = "get" ]; then
  # Batch node read: the fixture answers with the real command's shape (a JSON
  # array, misses as {"id":...,"error":"not found"}). Absent fixture = the
  # command itself failing (empty stdout, nonzero exit).
  if [ -n "$LEAD_BACKLOG_FIXTURE" ] && [ -f "$LEAD_BACKLOG_FIXTURE" ]; then
    cat "$LEAD_BACKLOG_FIXTURE"
  else
    exit 1
  fi
else
  exit 1
fi
STUB
chmod +x "$TMP/bin/fno"
export PATH="$TMP/bin:$PATH"
export LEAD_REG_FIXTURE="$TMP/registry.json"
export LEAD_FAQ_FIXTURE="$TMP/faq.txt"
export LEAD_HANDOFF_PATH_FIXTURE="$TMP/handoff-path.txt"
: > "$LEAD_FAQ_FIXTURE"
: > "$LEAD_HANDOFF_PATH_FIXTURE"

SID="sess-lead"
SID_OTHER="sess-someone-else"

# registry_fixture <row-json> - write a one-row registry around the given row.
registry_fixture() {
  printf '{"agents":[%s]}\n' "$1" > "$LEAD_REG_FIXTURE"
}

TEAMED_ROW='{"session_id":"'"$SID"'","harness_session_id":"full-'"$SID"'","name":"lead","status":"live","role_level":1,"role_scope":"fno"}'
TEAMED_HARNESS_ROW='{"session_id":"short-lead","harness_session_id":"'"$SID"'","name":"lead","status":"live","role_level":1,"role_scope":"fno"}'
UNPROMOTED_ROW='{"session_id":"'"$SID"'","harness_session_id":"full-'"$SID"'","name":"worker","status":"live","role_level":null,"role_scope":null}'

run_lead() { # $1 = event JSON ; FNO_PLATFORM env selects the lane
  printf '%s' "$1" | FNO_PLATFORM="$FNO_PLATFORM" bash "$LEAD" 2>/dev/null
}

# 1. Teamed row, source=compact, claude lane: the team line and a pointer
#    to the brief arrive on the model-context carrier. The brief's rules are
#    read from the file, never pasted.
registry_fixture "$TEAMED_ROW"
FNO_PLATFORM=claude
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"
RC=$?
[[ $RC -eq 0 ]] && echo "$OUT" | jq -e --arg brief "$BRIEF" '.hookSpecificOutput.additionalContext
    | contains("level 1 over fno") and contains("Read this file: " + $brief)
      and (contains("Encode, then step down") | not)' >/dev/null 2>&1 \
  && pass "teamed claude: additionalContext carries team + brief pointer, no pasted rules" \
  || fail "teamed claude rc=$RC payload=$OUT"

# 2. Teamed row, no source field, codex lane resolved through CODEX_THREAD_ID:
#    the real codex PostCompact event carries no session_id at all, so the SID
#    must come from the env marker the registry row's harness_session_id holds.
#    systemMessage carrier, never the claude-only hookSpecificOutput key.
registry_fixture "$TEAMED_HARNESS_ROW"
FNO_PLATFORM=codex
OUT="$(printf '%s' '{}' | env -u CODEX_SESSION_ID CODEX_THREAD_ID="$SID" FNO_PLATFORM=codex \
  CODEX_PLUGIN_ROOT="$REPO_ROOT" PLUGIN_ROOT="$REPO_ROOT" CLAUDE_PLUGIN_ROOT="$TMP/foreign-claude-plugin" \
  bash "$LEAD" 2>/dev/null)"
RC=$?
[[ $RC -eq 0 ]] && echo "$OUT" | jq -e 'has("systemMessage") and (has("hookSpecificOutput") | not)' >/dev/null 2>&1 \
  && pass "teamed codex via CODEX_THREAD_ID: systemMessage carrier" \
  || fail "teamed codex rc=$RC payload=$OUT"

# 3. Unpromoted row (both team fields null): nothing to re-teach, silence.
registry_fixture "$UNPROMOTED_ROW"
FNO_PLATFORM=claude
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"; RC=$?
[[ $RC -eq 0 && -z "$OUT" ]] && pass "unpromoted row: empty stdout, exit 0" \
  || fail "unpromoted row rc=$RC out=$OUT"

# 4. No row for this session id: the hook is not for this session, silence.
registry_fixture "$TEAMED_ROW"
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID_OTHER\"}")"; RC=$?
[[ $RC -eq 0 && -z "$OUT" ]] && pass "no registry row: empty stdout, exit 0" \
  || fail "no-row rc=$RC out=$OUT"

# 5. source=startup: the defensive gate must hold independent of registration.
OUT="$(run_lead "{\"source\":\"startup\",\"session_id\":\"$SID\"}")"; RC=$?
[[ $RC -eq 0 && -z "$OUT" ]] && pass "source=startup: empty stdout, exit 0" \
  || fail "startup rc=$RC out=$OUT"

# 6. No fno on PATH: no registry to read, silence (never a failed compaction).
OUT="$(printf '%s' "{\"source\":\"compact\",\"session_id\":\"$SID\"}" \
  | env PATH="/usr/bin:/bin" FNO_PLATFORM=claude bash "$LEAD" 2>/dev/null)"; RC=$?
[[ $RC -eq 0 && -z "$OUT" ]] && pass "no fno on PATH: empty stdout, exit 0" \
  || fail "no-fno rc=$RC out=$OUT"

# 6b. AC5-HP: a leaked FNO_AGENTS_RUNTIME=python pin must not blind
#     the team read. The stub refuses registry-json under the pin (mirroring
#     the real Rust client); the hook strips the pin before the read, so the
#     teamed reinjection still arrives and no failing-read line fires.
registry_fixture "$TEAMED_ROW"
FNO_PLATFORM=claude
PIN_ERR="$TMP/pin-err.txt"
OUT="$(printf '%s' "{\"source\":\"compact\",\"session_id\":\"$SID\"}" \
  | env FNO_PLATFORM=claude FNO_AGENTS_RUNTIME=python bash "$LEAD" 2>"$PIN_ERR")"; RC=$?
[[ $RC -eq 0 ]] && echo "$OUT" | jq -e '.hookSpecificOutput.additionalContext
    | contains("level 1 over fno")' >/dev/null 2>&1 \
  && ! grep -q "registry-json exited" "$PIN_ERR" \
  && pass "pinned env: team read survives the strip, reinjection arrives (AC5-HP)" \
  || fail "pinned env rc=$RC payload=$OUT err=$(cat "$PIN_ERR" 2>/dev/null)"

# 7b. Each FAQ entry file for the scope rides as one pointer line; a listed
#     path that is not a file adds nothing. The stub answers --paths with the
#     fixture's lines.
registry_fixture "$TEAMED_ROW"
FAQ_ENTRY="$TMP/lead-faq-1.md"
printf -- '---\nscope: fno\n---\nQ: what do I do?\nA: SENTINEL_FAQ_ANSWER\n' > "$FAQ_ENTRY"
printf '%s\n%s\n' "$FAQ_ENTRY" "$TMP/lead-faq-missing.md" > "$LEAD_FAQ_FIXTURE"
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"
RC=$?
[[ $RC -eq 0 ]] && echo "$OUT" | jq -e --arg faq "$FAQ_ENTRY" '.hookSpecificOutput.additionalContext
    | contains("Read this file: " + $faq) and (contains("lead-faq-missing") | not)
      and (contains("SENTINEL_FAQ_ANSWER") | not)' >/dev/null 2>&1 \
  && pass "FAQ entries: one pointer per file, text not pasted" \
  || fail "teamed+FAQ rc=$RC payload=$OUT"
: > "$LEAD_FAQ_FIXTURE"

# 7e. Canon pointer: the hook names the precompact doc as a file to read; it
#     pastes none of the doc's sections.
CANON_DOC="$TMP/canon-doc.md"
cat > "$CANON_DOC" <<'DOC'
# Canon doc: team fno

## Workarounds in force (session)
<!-- fno:session -->
SENTINEL_CANON_WORKAROUND restart sqld yourself.
<!-- /fno:session -->

Recall word: 00000009
DOC
# 0x9 % 8 = 1 picks the second opener: the recall word drives the rotation.
printf '%s\n' "$CANON_DOC" > "$LEAD_HANDOFF_PATH_FIXTURE"
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"
RC=$?
[[ $RC -eq 0 ]] \
  && echo "$OUT" | jq -e --arg doc "$CANON_DOC" '.hookSpecificOutput.additionalContext
      | contains("Fresh brain, who dis? Your memory is here. Read all of it: " + $doc)' >/dev/null 2>&1 \
  && ! printf '%s' "$OUT" | grep -q "SENTINEL_CANON_WORKAROUND" \
  && pass "canon pointer: doc path named, no section pasted" \
  || fail "canon pointer rc=$RC payload=${OUT:0:300}"
: > "$LEAD_HANDOFF_PATH_FIXTURE"

# 7f. No resolvable canon doc: no canon line, brief pointer still present
#     (positive marker, not absence alone).
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"
RC=$?
[[ $RC -eq 0 ]] && echo "$OUT" | grep -q "level 1 over fno" \
  && ! echo "$OUT" | grep -q "canon-doc.md" \
  && pass "no canon doc: no canon line, brief intact" \
  || fail "no-canon-doc rc=$RC payload=${OUT:0:300}"

# 8. Summary id resolution: the NEWEST isCompactSummary entry (by line
#    position) has its node-id candidates resolved through `fno backlog get`
#    and injected as unresolved:/resolved: rows the lead must act on.
registry_fixture "$TEAMED_ROW,{\"session_id\":\"worker-1\",\"harness_session_id\":\"full-worker-1\",\"name\":\"worker\",\"status\":\"live\"}"
export LEAD_BACKLOG_FIXTURE="$TMP/backlog-rows.json"
printf '[{"id":"cd-33334444","status":"in_progress","locked_by_harness":"claude","locked_by_harness_session":"worker-1"},{"id":"ee-77778888","error":"not found"}]\n' > "$LEAD_BACKLOG_FIXTURE"
SUMMARY_TX="$TMP/summary-transcript.jsonl"
cat > "$SUMMARY_TX" <<'EOF'
{"type":"user","message":"older context"}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-09-17T14:58:05.496Z"}
{"type":"user","isCompactSummary":true,"summary":"older beat cites aa-99998888 which must not win"}
{"type":"assistant","message":"work continues"}
{"type":"system","subtype":"compact_boundary","timestamp":"2026-09-17T15:58:05.496Z"}
{"type":"user","isCompactSummary":true,"summary":"newest beat: ab-11112222 running, cd-33334444 claimed, ee-77778888 gone, uuid 12345678-abcd-1234-abcd-123456789012 is not an id"}
EOF
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\",\"transcript_path\":\"$SUMMARY_TX\"}")"
RC=$?
[[ $RC -eq 0 ]] && printf '%s' "$OUT" | jq -e '.hookSpecificOutput.additionalContext
    | contains("The summary'"'"'s node ids, re-resolved")
      and contains("unresolved: ab-11112222")
      and contains("resolved: cd-33334444 status=in_progress locked_by=claude holder_live=yes")
      and contains("Act on these rows, not on the summary")' >/dev/null 2>&1 \
  && pass "summary ids: newest entry wins, unresolved + resolved rows injected" \
  || fail "summary-ids rc=$RC payload=${OUT:0:300}"

# 8a. The older summary entry's ids never win (line position, not timestamp).
printf '%s' "$OUT" | grep -q "aa-99998888" \
  && fail "older summary entry leaked into the payload" \
  || pass "older summary entry ignored (line position, not timestamp)"

# 8b. Whole uuids are masked before extraction: their inner hex groups
#     straddle the id grammar at word boundaries and are never node ids.
printf '%s' "$OUT" | grep -q "abcd-1234" \
  && fail "uuid fragment extracted as a candidate id" \
  || pass "uuid masked before id extraction"

# 8c. An error row from the batch read is unresolved, never resolved: the
#     command reports the miss as data and the hook relays it as a correction.
printf '%s' "$OUT" | grep -q "unresolved: ee-77778888" \
  && pass "batch error row lands under unresolved" \
  || fail "error row not surfaced as unresolved: ${OUT:0:200}"
: > "$LEAD_BACKLOG_FIXTURE"

# 8d. Degrade: `fno backlog get` failing (empty stdout) adds no section and
#     never a fabricated all-unresolved list; the other sections still ride.
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\",\"transcript_path\":\"$SUMMARY_TX\"}")"
RC=$?
[[ $RC -eq 0 ]] && printf '%s' "$OUT" | jq -e '.hookSpecificOutput.additionalContext
    | contains("level 1 over fno")
      and (contains("The summary'"'"'s node ids") | not)' >/dev/null 2>&1 \
  && pass "failed backlog read: no id section, brief intact, exit 0" \
  || fail "failed-read degrade rc=$RC payload=${OUT:0:300}"
unset LEAD_BACKLOG_FIXTURE

# 8e. A transcript with no isCompactSummary entry at all: no candidates, no
#     section, base brief intact.
printf '{"type":"user","message":"plain transcript, never compacted"}\n' > "$SUMMARY_TX"
OUT="$(run_lead "{\"source\":\"compact\",\"session_id\":\"$SID\"}")"
RC=$?
[[ $RC -eq 0 ]] && printf '%s' "$OUT" | grep -q "level 1 over fno" \
  && ! printf '%s' "$OUT" | grep -q "The summary's node ids" \
  && pass "no summary entry: no id section, brief intact" \
  || fail "no-summary rc=$RC payload=${OUT:0:300}"

# 7. Byte budget: the brief is paid on every compaction of every lead.
BRIEF_BYTES=$(wc -c < "$BRIEF" | tr -d '[:space:]')
[[ "$BRIEF_BYTES" -le "$BRIEF_MAX_BYTES" ]] \
  && pass "source brief within budget (${BRIEF_BYTES} <= ${BRIEF_MAX_BYTES} bytes)" \
  || fail "source brief over budget: ${BRIEF_BYTES} > ${BRIEF_MAX_BYTES} bytes"

echo ""
echo "lead-postcompact-reinject: $PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
