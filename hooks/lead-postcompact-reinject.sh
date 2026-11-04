#!/usr/bin/env bash
# fno hook: multi-event - restore lead context after compaction
# Re-inject the lead's operating rules after a context compaction.
#
# The team survives a compaction (team_level / team_scope live on the agent
# registry row) but the operating discipline that came with it does not, so a
# post-compact lead still holds real authority with no rules for using it and
# the operator re-teaches it by hand. This hook re-teaches it mechanically. It
# also reads back the team's precompact canon handoff doc: the judgment halves
# it holds survive the compact in no other channel.
#
# Carrier: shared with target-postcompact-reinject.sh in
# scripts/lib/postcompact-carrier.sh - on Claude SessionStart(source=="compact")
# via hookSpecificOutput.additionalContext, on Codex PostCompact via
# systemMessage. Never re-derive the carrier here; shipped a hook that
# emitted a payload no harness delivered and it went unnoticed for months.
#
# NEVER blocks. A compaction is often triggered to recover from a context-limit
# error, so this hook always exits 0 and degrades to silence when anything it
# reads is missing: no lib, no fno, no registry row, no team, no brief.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

# BASH_SOURCE-relative, never `git rev-parse`: cwd is the session's repo, not
# the plugin (the fix banked from 502af79f2).
SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${FNO_PLATFORM:-}" == "codex" ]]; then
    PLUGIN_ROOT="${CODEX_PLUGIN_ROOT:-${PLUGIN_ROOT:-$SOURCE_ROOT}}"
else
    PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-${CODEX_PLUGIN_ROOT:-$SOURCE_ROOT}}"
fi
CARRIER_LIB="$PLUGIN_ROOT/scripts/lib/postcompact-carrier.sh"
BRIEF="$PLUGIN_ROOT/skills/lead/references/postcompact-brief.md"

[[ -r "$CARRIER_LIB" ]] || exit 0
# shellcheck source=../scripts/lib/postcompact-carrier.sh
source "$CARRIER_LIB"

# Read the hook event through the shared lib (TTY-guarded; one jq pass).
EVENT="$(postcompact_read_event)"
SOURCE="$(printf '%s' "$EVENT" | sed -n 1p)"
SID="$(printf '%s' "$EVENT" | sed -n 2p)"
TRANSCRIPT="$(printf '%s' "$EVENT" | sed -n 3p)"

# Defensive gate: on SessionStart, reinject only for compaction. The matcher
# ("compact") already enforces this at registration; the check keeps the script
# correct independent of registration and directly testable. An empty SOURCE
# (PostCompact on Codex) passes through unchanged.
if [[ -n "$SOURCE" && "$SOURCE" != "compact" ]]; then
    exit 0
fi

# Session id through the shared resolver: event field, then transcript
# basename, then the env markers in HARNESS_SESSION_MARKERS precedence.
SID="$(postcompact_resolve_sid "$SID" "$TRANSCRIPT")"
[[ -n "$SID" ]] || exit 0

# The brief, checked before the registry read: it is a pure file stat, and the
# common case (an untitled session compacting) must not pay the fno/jq spawns.
# Missing or empty means no injection, never a partial one.
[[ -r "$BRIEF" && -s "$BRIEF" ]] || exit 0

# Read the team from the registry, not from a guess. `fno agents internal registry-json`
# is a daemon-free file read; `fno agents session list` is Rust-routed and would
# lazy-start the daemon (see hooks/context-nudge.sh for the same choice). This
# session's row matches session_id OR harness_session_id - the registry stores
# a short id in one field and the full harness id in the other.
command -v fno >/dev/null 2>&1 || exit 0
command -v jq  >/dev/null 2>&1 || exit 0
# Keep the exit code, so a broken read never reads silently as "no row".
AGENTS_JSON="$(fno agents internal registry-json 2>/dev/null)"
REG_RC=$?
[[ "$REG_RC" -ne 0 ]] \
  && echo "lead-postcompact-reinject.sh: fno agents internal registry-json exited $REG_RC; team treated as unknown" >&2
MY_ROW="$(printf '%s' "$AGENTS_JSON" | jq -c --arg sid "$SID" \
    '.agents[] | select(.session_id == $sid or .harness_session_id == $sid)' 2>/dev/null | head -1)"
[[ -n "$MY_ROW" ]] || exit 0
TEAM_LEVEL="$(printf '%s' "$MY_ROW" | jq -r '.role_level // empty' 2>/dev/null)"
TEAM_SCOPE="$(printf '%s' "$MY_ROW" | jq -r '.role_scope // empty' 2>/dev/null)"
[[ -n "$TEAM_LEVEL" || -n "$TEAM_SCOPE" ]] || exit 0

# Pointers, not prose: every file the lead needs after a compact is named on
# one line, and the lead reads it. A byte-capped paste in doc order cut the
# canon doc's Workarounds section on every compaction, and the lead re-asked
# the user for a command the doc held. Only the node re-resolve table below
# stays inline: it is computed here and exists in no file.
CONTEXT="## You are still the lead

Team: level ${TEAM_LEVEL:-?} over ${TEAM_SCOPE:-?}. Confirm with \`fno whoami\`. Read every file below in full before your first lever.

Read this file: ${BRIEF}"

# The canon doc the PreCompact hook wrote, then this team's FAQ entries. Both
# degrade to no line: no resolvable doc, no FAQ entry, never a failed hook.
if [[ -n "$TEAM_SCOPE" ]]; then
    CANON_PATH="$(fno config paths handoff --scope "$TEAM_SCOPE" 2>/dev/null || true)"
    if [[ -n "$CANON_PATH" && -f "$CANON_PATH" ]]; then
        # The opener rotates like a spinner word, picked by the doc's recall
        # word, so the line never reads as boilerplate the lead skims past.
        # The tail never changes: org checkin's refusal names the same path.
        OPENERS=(
            "Feeling amnesia?"
            "Fresh brain, who dis?"
            "Woke up in a new context?"
            "Your past self left you a note."
            "Lost the plot?"
            "Context wiped clean?"
            "Deja vu, minus the memories?"
            "New session, same job."
        )
        RECALL="$(sed -n 's/^Recall word: *\([0-9a-f]\{1,8\}\).*/\1/p' "$CANON_PATH" 2>/dev/null | tail -1)"
        OPENER="${OPENERS[$(( 0x${RECALL:-0} % ${#OPENERS[@]} ))]}"
        CONTEXT="$CONTEXT
${OPENER} Your memory is here. Read all of it: ${CANON_PATH}"
    fi
    while IFS= read -r FAQ_FILE; do
        [[ -n "$FAQ_FILE" && -f "$FAQ_FILE" ]] || continue
        CONTEXT="$CONTEXT
Read this file: ${FAQ_FILE}"
    done < <(fno agents org faq list --scope "$TEAM_SCOPE" --paths 2>/dev/null || true)
fi

# A tenured lead (its scope manifest reports a shape AND names this session)
# also re-reads the lead skill: the beat, the stop arm and the levers live
# there. A missing manifest or a foreign session id adds no line.
LEAD_MANIFEST="$(fno agents org chart manifest-path --harness-session-id "$SID" 2>/dev/null || true)"
if [[ -n "$LEAD_MANIFEST" && -f "$LEAD_MANIFEST" ]]; then
    LEAD_SHAPE="$(sed -n 's/^shape:[[:space:]]*//p' "$LEAD_MANIFEST" | head -1 | tr -d '[:space:]')"
    LEAD_SID="$(sed -n 's/^harness_session_id:[[:space:]]*//p' "$LEAD_MANIFEST" | head -1 | tr -d '[:space:]')"
    if [[ -n "$LEAD_SHAPE" && "$LEAD_SID" == "$SID" && -f "$PLUGIN_ROOT/skills/lead/SKILL.md" ]]; then
        CONTEXT="$CONTEXT
Read this file: $PLUGIN_ROOT/skills/lead/SKILL.md"
    fi
fi

# Summary id resolution: the compaction summary is the team's snapshot of
# the board, and after a compact nothing else re-resolves it. Candidates come
# from the NEWEST isCompactSummary entry, chosen by LINE POSITION (tail -1),
# never by timestamp: on a real transcript the summary line carried an
# earlier timestamp than the boundary line before it. Resolution goes through
# the CLI, never a graph.json read - with the sqlite backend the file is the
# relational store's stale twin. Same degrade contract as every section
# above: no transcript, no summary, no candidates, or a read that failed
# (empty stdout) means no section, never a failed hook.
ID_MAX_BYTES=2000
if [[ -n "$TRANSCRIPT" && -f "$TRANSCRIPT" ]] && command -v jq >/dev/null 2>&1; then
    # Two literals, not one: a tool result echoing a transcript line (this
    # hook greps its own needle) must not shadow the real summary entry.
    SUMMARY_LINE="$(grep '"isCompactSummary":true' "$TRANSCRIPT" 2>/dev/null | grep '"type":"user"' | tail -1 || true)"
    CANDIDATES="$(printf '%s' "$SUMMARY_LINE" | jq -Rsj '
# Whole uuids mask first: their inner hex groups straddle the id grammar at
# word boundaries (...-abcd-1234-...) and are never node ids.
gsub("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}"; " ")
| [scan("\\b(?:[a-z][a-z0-9]{0,7}-[0-9a-f]{4,8}|x[0-9a-f]{4,8})\\b")]
| unique | .[:40] | join("\n")
' 2>/dev/null || true)"
    if [[ -n "$CANDIDATES" ]]; then
        BOARD_ROWS="$(printf '%s\n' "$CANDIDATES" | xargs fno backlog get 2>/dev/null || true)"
        BOARD_BLOCK="$(CANDIDATES="$CANDIDATES" ROWS_JSON="$BOARD_ROWS" \
            REGISTRY_JSON="$AGENTS_JSON" CAP="$ID_MAX_BYTES" jq -nj '
def cpbytes: if . < 128 then 1 elif . < 2048 then 2 elif . < 65536 then 3 else 4 end;
# UTF-8-safe cut: keep whole characters up to $cap bytes, never split one.
def bytecut($cap):
  reduce explode[] as $c ({n: 0, out: [], done: false};
    if .done then .
    elif .n + ($c | cpbytes) > $cap then .done = true
    else .n += ($c | cpbytes) | .out += [$c] end)
  | .out | implode;
[($ENV.CANDIDATES // "") | split("\n")[] | select(. != "")] as $cands
# A batch answer is always a JSON array (even an all-miss one). Empty or
# unparseable stdout means the command itself failed; emit nothing rather than
# fabricate an unverified all-unresolved list.
| (($ENV.ROWS_JSON // "") as $raw
   | if ($raw | test("\\S")) then ($raw | try fromjson catch null) else null end) as $data
| if ($data | type) != "array" then empty else
  (reduce ($data[] | select(type == "object" and (.id // false) and ((.error // false) | not))) as $r
     ({}; . + {($r.id | tostring): $r})) as $by_id
  | ((($ENV.REGISTRY_JSON // "") | if . == "" then null else (try fromjson catch null) end) // {}) as $reg
  | ([($reg.agents // [])[] | (.session_id, .harness_session_id) | select(. != null and . != false and . != "")]) as $live
  | ([$cands[] | select($by_id[.] == null) | "unresolved: " + .]
     + [$cands[] | . as $c | $by_id[$c] | select(. != null)
        | ((.locked_by_harness_session // "") | if . == "" or . == false then "" else . end) as $holder
        | (if $holder == "" then "-" elif ($live | index([$holder])) then "yes" else "no" end) as $held
        | "resolved: \($c) status=\(.status // "?" | if . == "" or . == false then "?" else . end) locked_by=\(.locked_by_harness // "-" | if . == "" or . == false then "-" else . end) holder_live=\($held)"])
    | join("\n") as $out
    | (($ENV.CAP // "") | if . == "" then 2000 else tonumber end) as $cap
    | if ($out | utf8bytelength) > $cap
      then ($out | bytecut($cap)) + "\n\n_(truncated at \($cap)B)_"
      else $out end
  end
' 2>/dev/null || true)"
        if [[ -n "$BOARD_BLOCK" ]]; then
            CONTEXT="$CONTEXT

## The summary's node ids, re-resolved

Act on these rows, not on the summary; any id whose state differs from what the summary said is a correction.

$BOARD_BLOCK"
        fi
    fi
fi

# The read-command lesson rides the post-compact brief: a compaction boundary
# newer than the taught one re-teaches it once, and the door prints nothing
# when the session is current. Best-effort - a stale or absent binary
# teaches nothing and never blocks the brief.
if [[ -r "$SOURCE_ROOT/hooks/lib/agents-bin.sh" ]]; then
    # shellcheck source=../hooks/lib/agents-bin.sh
    source "$SOURCE_ROOT/hooks/lib/agents-bin.sh"
    TEACH_BIN="$(fno_agents_bin "$SOURCE_ROOT")"
    if [[ -n "$TEACH_BIN" && -x "$TEACH_BIN" && -n "$SID" ]]; then
        TEACH="$("$TEACH_BIN" mail-teach --session "$SID" --mark 2>/dev/null || true)"
        [[ -n "$TEACH" ]] && CONTEXT="${CONTEXT}

$TEACH"
    fi
fi

postcompact_emit "$(postcompact_carrier "$SOURCE")" "$CONTEXT"

exit 0
