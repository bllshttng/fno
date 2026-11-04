#!/usr/bin/env bash
# fno hook: PreCompact - precompact canon doc
# precompact-canon-doc.sh - PreCompact mechanical backstop.
#
# Writes and refreshes the MECHANICAL sections of this session's canon handoff
# doc (identity, live workers, node pointers, open PRs) and prints a pointer so
# compaction spends its fixed output budget on the conversation since the doc
# was written, not on re-deriving the doc's contents.
#
# What it is NOT: the writer. It cannot run a model (PreCompact supports only
# command/http/mcp_tool, not prompt/agent), so the two judgment sections -
# merge order, open decisions - are left as empty headings filled by the
# session at full context (the context-nudge asks for that). This is a
# backstop that reaches every compacting session, including the lead passes and
# non-target sessions that the manifest-gated arm-handoff hook never reaches.
#
# NEVER blocks. Compaction triggered to recover from a context-limit error must
# not have its request fail at the moment it fires, so this hook always exits 0,
# never emits decision: block, and degrades to an omitted section (never a
# failure) when fno / gh / the registry is unreadable or absent.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

SOURCE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "${FNO_PLATFORM:-}" == "codex" ]]; then
  PLUGIN_ROOT="${CODEX_PLUGIN_ROOT:-${PLUGIN_ROOT:-$SOURCE_ROOT}}"
else
  PLUGIN_ROOT="${CLAUDE_PLUGIN_ROOT:-${PLUGIN_ROOT:-$SOURCE_ROOT}}"
fi
FNO_DIR=".fno"
# The session-id fallback chain lives in the shared postcompact lib so a marker
# change lands once. Unreadable lib: keep the local chain quiet, never fail.
CARRIER_LIB="$PLUGIN_ROOT/scripts/lib/postcompact-carrier.sh"
MARKER_LIB="$PLUGIN_ROOT/scripts/lib/canon-doc-marker.sh"
[[ -r "$MARKER_LIB" ]] && source "$MARKER_LIB"

# ---------------------------------------------------------------------------
# Read the hook event from stdin (non-fatal if absent).
# ---------------------------------------------------------------------------
INPUT=""
if [[ ! -t 0 ]]; then
  INPUT="$(cat 2>/dev/null || true)"
fi

_json_field() {
  # Echo a top-level string field from the hook event JSON, or "".
  printf '%s' "$INPUT" | jq -r --arg k "$1" '
    (.[$k] // "") | if . == 0 then "" elif type == "string" then . else tojson end' 2>/dev/null
}

# ---------------------------------------------------------------------------
# Resolve the session id through the shared postcompact lib: transcript
# basename first (PreCompact carries transcript_path), then the env markers in
# HARNESS_SESSION_MARKERS precedence. No session id -> nothing useful to point
# at; emit nothing, exit clean.
# ---------------------------------------------------------------------------
_tp="$(_json_field transcript_path)"
if [[ -r "$CARRIER_LIB" ]]; then
  # shellcheck source=../scripts/lib/postcompact-carrier.sh
  source "$CARRIER_LIB"
  SID="$(postcompact_resolve_sid "" "$_tp")"
else
  # Unreadable lib: degrade to the transcript basename only (the claude path;
  # PreCompact always carries it there) rather than duplicating the chain.
  SID=""
  if [[ -n "$_tp" ]]; then
    SID="$(basename "$_tp")"
    SID="${SID%.jsonl}"
  fi
fi
# No session id -> nothing useful to point at. Emit nothing, exit clean.
[[ -n "$SID" ]] || exit 0

# ---------------------------------------------------------------------------
# Compaction marker (wave 1): stamp "this session is compacting" so the
# cap actor and every idle reader can tell a compacting session from an idle
# one. Best-effort: a failed mark must not fail the hook, so every failure
# degrades to no marker (the reader reports Unknown, never a false all-clear).
# ---------------------------------------------------------------------------
if command -v fno-agents >/dev/null 2>&1; then
  fno-agents compaction mark --session "$SID" >/dev/null 2>&1 || true
fi

# Short id for display. Labeled tail-8 because that is the mail handle today;
# the full SID above is the authoritative identity key regardless.
SHORT="${SID: -8}"

# ---------------------------------------------------------------------------
# Teamed-ness + team scope, read before the doc-path resolution because a
# teamed session keys its doc on the SCOPE (a team outlives its sessions; a
# successor resolves the same rolling doc). Computed once from the registry
# read and handed into the heredoc below via env: the heredoc's stdout carries
# ONLY the auto block text, matching every other fact in this script. An
# earlier version smuggled this boolean out as a synthetic first stdout line
# for bash to split off by position - fragile, since any reordering of the
# heredoc's own output silently corrupts the auto block with no error (the
# whole heredoc is wrapped in `|| true`).
# ---------------------------------------------------------------------------
REG_ROWS=""
REG_RC=0
if command -v fno >/dev/null 2>&1; then
  # Keep the exit code, and let a failed read read as `unknown`, never as
  # unpromoted (a silent `|| true` took a failure for "no registry row" and a
  # teamed lead got a plain session doc).
  REG_ROWS="$(fno agents internal registry-json 2>/dev/null)"
  REG_RC=$?
  if [[ "$REG_RC" -ne 0 ]]; then
    echo "precompact-canon-doc.sh: fno agents internal registry-json exited $REG_RC; the doc's team line reads unknown" >&2
  fi
fi

TEAM_INFO="$(SID="$SID" REG_ROWS="$REG_ROWS" jq -nr '
($ENV.SID // "") as $sid
| (($ENV.REG_ROWS // "") | if . == "" then "[]" else . end | try fromjson catch []) as $data
| (if ($data | type) == "object" then ($data.agents // $data.rows // []) else $data end) as $data
| (if ($data | type) == "array" then $data else [] end) as $rows
| [$rows[] | select(.session_id == $sid or .harness_session_id == $sid)] as $mine
| ($mine[0] // {}) as $r
| (if ($mine | length) > 0 and ($r.role_level != null or $r.role_scope != null) then "1" else "0" end),
  (if ($r.role_scope | type) == "string" then $r.role_scope else "" end),
  ([$r.project_root, $r.cwd, ""] | map(select(. != null and . != false and . != "")) | first // "" | tostring)
' 2>/dev/null || true)"
IS_TEAMED="$(printf '%s' "$TEAM_INFO" | sed -n 1p)"
TEAM_SCOPE="$(printf '%s' "$TEAM_INFO" | sed -n 2p)"
ROLE_ROOT="$(printf '%s' "$TEAM_INFO" | sed -n 3p)"
[[ "$IS_TEAMED" == "1" ]] || IS_TEAMED=0

# A teamed session's facts come from the role's project, never the shell's
# cwd: a lead whose shell sat in a job tmp dir refreshed a doc listing zero
# open PRs. Same rule as the native role commands. A worktree of the role's
# own repo shares its git common dir and stays put.
_git_common() {
  (cd "$1" 2>/dev/null && cd "$(git rev-parse --git-common-dir 2>/dev/null)" 2>/dev/null && pwd -P)
}
if [[ -n "$TEAM_SCOPE" && -n "$ROLE_ROOT" ]]; then
  if [[ -d "$ROLE_ROOT" ]]; then
    if [[ "$(_git_common "$PWD")" != "$(_git_common "$ROLE_ROOT")" ]]; then
      cd "$ROLE_ROOT" 2>/dev/null || true
    fi
  else
    echo "precompact-canon-doc.sh: cwd $PWD is outside role $TEAM_SCOPE's project $ROLE_ROOT, which does not exist; facts read from the cwd" >&2
  fi
fi

# ---------------------------------------------------------------------------
# Resolve the canon doc path. A manual /compact <path> carries a path the
# session deliberately chose - enrich THAT file rather than minting a sibling.
# Otherwise fall back to fno config paths handoff: --scope for a teamed
# session (the newest doc for that team, else today's scope-keyed name), the
# session-keyed form otherwise. Only treat custom_instructions as a path when
# it plainly is one (ends in .md); prose instructions fall through.
# ---------------------------------------------------------------------------
DOC_PATH=""
_ci="$(_json_field custom_instructions)"
_ci="$(printf '%s' "$_ci" | tr -d '\r\n')"
_ci="${_ci#"${_ci%%[![:space:]]*}"}"  # trim leading whitespace
# Only treat custom_instructions as a doc path when it plainly IS one: a
# path-anchored .md route the session deliberately chose, or an existing file.
# Bare prose that happens to end in .md ("remember to update README.md") must
# fall through to fno config paths handoff, not become a junk filename in the cwd.
case "$_ci" in
  /*.md|./*.md|../*.md) DOC_PATH="$_ci" ;;
  *.md) [[ -f "$_ci" ]] && DOC_PATH="$_ci" ;;
esac
if [[ -z "$DOC_PATH" && -n "$TEAM_SCOPE" ]]; then
  DOC_PATH="$(fno config paths handoff --scope "$TEAM_SCOPE" 2>/dev/null || true)"
fi
if [[ -z "$DOC_PATH" ]]; then
  DOC_PATH="$(fno config paths handoff --session-id "$SID" 2>/dev/null || true)"
fi
# No resolvable doc path -> a bare discard list with no pointer is worth little.
[[ -n "$DOC_PATH" ]] || exit 0

# A doc path that is itself a symlink: resolve it, so the atomic rename below
# replaces the real file and the link keeps pointing at the fresh content.
# Renaming over the link would silently replace the link with a regular file.
# Plain readlink resolves one hop per call, which stays portable to hosts
# whose readlink has no -f.
for _hop in 1 2 3 4 5; do
  [[ -L "$DOC_PATH" ]] || break
  _link="$(readlink "$DOC_PATH" 2>/dev/null)" || break
  [[ -n "$_link" ]] || break
  case "$_link" in
    /*) DOC_PATH="$_link" ;;
    *) DOC_PATH="$(dirname "$DOC_PATH")/$_link" ;;
  esac
done
unset _hop _link

# ---------------------------------------------------------------------------
# Gather mechanical facts. Each degrades to an empty/omitted value, never an
# error. Bounded subprocess calls only.
# ---------------------------------------------------------------------------
ISO="$(date -u +%Y-%m-%dT%H:%M:%SZ 2>/dev/null || echo unknown)"

# Node pointer + plan from the target manifest, if this session has one.
NODE=""
PLAN=""
if [[ -f "$FNO_DIR/target-state.md" ]]; then
  NODE="$(grep -m1 -E "^input:" "$FNO_DIR/target-state.md" 2>/dev/null | sed -E 's/^input:[[:space:]]*//; s/^"(.*)"$/\1/' || true)"
  PLAN="$(grep -m1 -E "^plan_path:" "$FNO_DIR/target-state.md" 2>/dev/null | sed -E 's/^plan_path:[[:space:]]*//; s/^"(.*)"$/\1/' || true)"
fi

PR_RAW=""
if command -v fno >/dev/null 2>&1; then
  # Omit the section entirely if fno is missing, there is no remote, or REST fails
  # - never a failed hook (the vertical-generalization epic takes fno past code).
  PR_RAW="$(fno do pr list --state open 2>/dev/null || true)"
fi

# The team scope's children, id plus status, via the existing epic status
# read. A portfolio team stores its scope as a comma-joined set of epics -
# query each member and merge, so a level-2 lead over more than one epic sees
# every member's children, not just the first. Bounded (5s per member) and
# degrade-only: a member that is not a queryable epic (a level-1
# whole-project team, an unreadable graph) contributes nothing rather than
# failing the whole read; the section reads unavailable only when EVERY
# member yields nothing.
PURVIEW=""
if [[ "$IS_TEAMED" == "1" && -n "$TEAM_SCOPE" ]] && command -v fno >/dev/null 2>&1; then
  # shellcheck source=../scripts/lib/with-timeout.sh
  source "$SOURCE_ROOT/scripts/lib/with-timeout.sh" 2>/dev/null || with_timeout() { shift; "$@"; }
  _children=""
  IFS=',' read -r -a _members <<< "$TEAM_SCOPE"
  for _member in "${_members[@]}"; do
    _member="${_member#"${_member%%[![:space:]]*}"}"
    _member="${_member%"${_member##*[![:space:]]}"}"
    [[ -n "$_member" ]] || continue
    _children+="$(with_timeout 5 fno backlog epic status "$_member" --json 2>/dev/null \
      | jq -c '.children | if type == "array" then .[] else empty end' 2>/dev/null || true)"$'\n'
  done
  PURVIEW="$(printf '%s' "$_children" | jq -rs '
    def pys: if . == null then "None" elif . == true then "True" elif . == false then "False"
      elif type == "string" then . else tojson end;
    def get($k; $d): if has($k) then .[$k] else $d end;
    reduce .[] as $c ({seen: [], rows: []};
      if (.seen | index([$c.id])) then . else .seen += [$c.id] | .rows += [$c] end)
    | .rows | map("- \(get("id"; "-") | pys) [\(get("status"; "-") | pys)] \(get("slug"; "") | pys)")
    | join("\n")' 2>/dev/null || true)"
  unset _children _members _member
fi

# ---------------------------------------------------------------------------
# Build the auto block. One jq program reads the facts from env and emits the
# mechanical sections. JSON parsing stays in jq; bash never touches it.
# ---------------------------------------------------------------------------
AUTO_BLOCK="$(SID="$SID" SHORT="$SHORT" NODE="$NODE" PLAN="$PLAN" \
             REG_ROWS="$REG_ROWS" REG_RC="$REG_RC" PR_RAW="$PR_RAW" IS_TEAMED="$IS_TEAMED" \
             PURVIEW="$PURVIEW" jq -nr '
# The values print the way the old python printf did ("%s" of a value).
def pys: if . == null then "None" elif . == true then "True" elif . == false then "False"
  elif type == "string" then . else tojson end;
def get($k; $d): if has($k) then .[$k] else $d end;
def rows_from: if . == "" then [] else (try fromjson catch []) end
  | if type == "object" then (.agents // .rows // []) else . end
  | if type == "array" then . else [] end;
def tail8: (.session_id // "") | if type == "string" then .[-8:] else "" end;
def worker_row: "\(tail8) | \(get("name"; "-") | pys) | \(get("status"; "-") | pys)";
($ENV.SID // "") as $sid
| ($ENV.SHORT // "") as $short
| ($ENV.NODE // "") as $node
| ($ENV.PLAN // "") as $plan
| (($ENV.REG_ROWS // "") | rows_from) as $rows
| [$rows[] | select(.session_id == $sid or .harness_session_id == $sid)] as $mine
| ($mine[0] // {}) as $r
| $r.role_level as $lvl
| $r.role_scope as $scp
| (($ENV.IS_TEAMED // "") == "1") as $teamed
| ($ENV.REG_RC // "") as $reg_rc
| (if ($reg_rc != "" and $reg_rc != "0") then "unknown (registry-json exit \($reg_rc))"
   elif ($mine | length) == 0 then "none (no registry row for this session)"
   elif ($teamed | not) then "none (unpromoted)"
   else "level \(if $lvl != null then $lvl | pys else "-" end) | scope \(if $scp != null then $scp | pys else "-" end)"
   end) as $team
# The stored `status` word lies (a dead row can read `live` indefinitely), so
# this reads the SERVED `liveness` field instead. A row whose liveness is null
# (never measured, or stale past the window) is unresolved, not alive: listed
# under its own label so a broken reader never reads as an all-clear.
| [$rows[] | select(.spawned_by_session == $sid and .liveness == "alive")] as $alive
| [$rows[] | select(.spawned_by_session == $sid and .liveness != "alive" and .liveness != "dead")] as $unresolved
| (if ($alive | length) == 0 and ($unresolved | length) == 0 then "none"
   else ([$alive[] | "- " + worker_row]
         + (if ($unresolved | length) > 0
            then ["- unresolved liveness:"] + [$unresolved[] | "  - " + worker_row]
            else [] end)) | join("\n")
   end) as $workers
| (if $node != "" or $plan != "" then
     ([if $node != "" then "- node: \($node)" else empty end,
       if $plan != "" then "- plan: \($plan)" else empty end] | join("\n"))
   else "none (no target manifest - non-target session)" end) as $nodes
| (($ENV.PR_RAW // "") | rows_from) as $pr_rows
| ["## Identity (auto)",
   "- mail handle (tail-8): `\($short)`",
   "- team: \($team)",
   "",
   "## Live workers (auto)",
   $workers,
   "",
   "## Node pointers (auto)",
   $nodes]
  + (if ($pr_rows | length) > 0 then
       ["", "## Open PRs (auto)"]
       + [$pr_rows[] | "- #\(get("number"; "-") | pys) [\(get("state"; "-") | pys)] \(get("title"; "-") | pys) (\(get("headRefName"; "-") | pys))"]
     else [] end)
# A lead additionally holds its nodes under purview and its own live workers.
# Skipped whole when unpromoted so a non-lead canon doc stays byte-identical.
  + (if $teamed then
       ["",
        "## Lead: nodes under purview (auto)",
        "level \(if $lvl != null then $lvl | pys else "-" end) over \(if $scp != null then $scp | pys else "-" end)",
        "",
        (($ENV.PURVIEW // "") | if . == "" then "_(nodes under purview: unavailable)_" else . end),
        "",
        "## Lead: live workers in scope (auto)",
        $workers]
     else [] end)
| join("\n")
' 2>/dev/null || true)"

# ---------------------------------------------------------------------------
# Preserve any judgment the session already wrote under the two session markers.
# The hook regenerates the auto block on every fire; it must NOT clobber what
# the session filled at full context (the compact that triggers this hook would
# otherwise erase the judgment right when post-compact context needs it).
# ---------------------------------------------------------------------------
_session_block_raw() {
  # $1 = heading label substring. The verbatim body between the markers under
  # the matching "## <label>" heading, or "" when the doc is absent or the
  # label has no block.
  awk -v label="$1" '
    /^## / { heading = $0; next }
    /<!-- fno:session -->/ { grab = (index(heading, label) > 0); next }
    grab && /<!-- \/fno:session -->/ { grab=0; next }
    grab { print }
  ' "$DOC_PATH" 2>/dev/null
}

_session_block() {
  # $1 = heading label substring, $2 = default instruction text. Matched by
  # the "## <label>" heading immediately above each marker, not by ordinal
  # position: a lead who hand-writes only the two team headings on a first
  # compaction (the doc did not exist yet to hold headings 1/2) still binds
  # correctly, instead of silently landing in the wrong slot.
  local label="$1" default="$2" preserved=""
  if [[ -f "$DOC_PATH" ]]; then
    preserved="$(_session_block_raw "$label")"
  fi
  if [[ -n "$(printf '%s' "$preserved" | tr -d '[:space:]')" ]]; then
    printf '%s\n' "$preserved"
  else
    printf '%s\n' "$default"
  fi
}

_unowned_session_blocks() {
  # Every fno:session-marked section whose heading is NOT one of the four the
  # writer regenerates below, captured verbatim (heading + markers + body).
  # The assembly re-emits only the four known sections, so without this pass a
  # refresh deletes a lead's hand-written "## HANDOFF (session)" whole. Only
  # content below the hook's own title line is ours to keep: above it, PRIOR
  # already preserves the body verbatim, and both passes would duplicate it.
  awk -v k1="Merge order and why" -v k2="Open decisions awaiting the operator" \
      -v k3="Gaps and open thinking" -v k4="Workarounds in force" '
    /^# Canon doc: / { started = 1; heading = ""; next }
    !started { next }
    /^## / { heading = $0; next }
    {
      if (index($0, "<!-- fno:session -->") > 0) {
        known = index(heading, k1) > 0 || index(heading, k2) > 0 \
             || index(heading, k3) > 0 || index(heading, k4) > 0
        keep = !known
        bhead = heading
        if (keep && heading != "") buf = buf heading "\n"
        heading = ""
      }
      if (keep) {
        buf = buf $0 "\n"
        if (index($0, "<!-- /fno:session -->") > 0) {
          # One copy per heading, ever: if the writer ever regenerates a
          # section this pass also captures (a label list drift), the copy
          # already on disk must not compound into a second one per fire.
          if (bhead == "" || !(bhead in seen)) {
            if (bhead != "") seen[bhead] = 1
            printf "%s", buf
          }
          buf = ""; keep = 0
        }
      }
    }
    END { printf "%s", buf }
  ' "$DOC_PATH" 2>/dev/null
}

DEFAULT_MERGE="_Merge order and the reason for it. Nothing external knows this. The session fills it at full context._"
DEFAULT_DECISIONS="_Open decisions awaiting the operator. Nothing external knows this. The session fills it at full context._"
DEFAULT_GAPS="_Gaps and open thinking only this team holds. Nothing external knows this. The session fills it at full context._"
DEFAULT_WORKAROUNDS="_Workarounds in force only this team is running. Nothing external knows this. The session fills it at full context._"

# Capture the preserved-or-defaulted session blocks BEFORE opening the doc for
# write. The assembly below redirects to $DOC_PATH, which truncates it on open;
# reading inside that block would see an empty file and always default.
SB1="$(_session_block "Merge order and why" "$DEFAULT_MERGE")"
SB2="$(_session_block "Open decisions awaiting the operator" "$DEFAULT_DECISIONS")"
SB3="" SB4=""
if [[ "$IS_TEAMED" == "1" ]]; then
  SB3="$(_session_block "Gaps and open thinking" "$DEFAULT_GAPS")"
  SB4="$(_session_block "Workarounds in force" "$DEFAULT_WORKAROUNDS")"
fi

# A doc the session wrote by hand carries none of the markers above, so the
# preserve helper reads nothing from it and the write below would truncate it
# whole - erasing a brief written at full context. Keep everything that sits
# above the hook's own title line. Re-fires re-read the same span, so the body
# is preserved once, not appended again.
PRIOR=""
if [[ -f "$DOC_PATH" ]]; then
  PRIOR="$(awk '/^# Canon doc: /{exit} {print}' "$DOC_PATH" 2>/dev/null)"
fi

# Unowned session sections (any marked "## X (session)" heading beyond the
# four regenerated above, e.g. a lead's "## HANDOFF (session)") captured
# before the truncate like the blocks above and re-emitted verbatim, so a
# refresh keeps session-owned content instead of deleting it silently.
EXTRA_SESSIONS=""
if [[ -f "$DOC_PATH" ]]; then
  EXTRA_SESSIONS="$(_unowned_session_blocks)"
fi

# The fno:user block is the one section the machine NEVER writes and ALWAYS
# reads: whatever the user typed between its markers round-trips byte-for-byte,
# and the closing marker is re-added below when a partial edit dropped it
# (capture runs to the next heading or EOF, so nothing is lost either way).
# Captured before the truncate like the session blocks. Seeded only when no
# open marker exists yet; from then on the placeholder is ordinary content.
USER_BLOCK=""
USER_SEED=1
if [[ -f "$DOC_PATH" ]] && command -v canon_doc_extract_marker >/dev/null 2>&1; then
  USER_BLOCK="$(canon_doc_extract_marker "$DOC_PATH" user)" && USER_SEED=0
fi
if [[ "$USER_SEED" == "1" ]]; then
  USER_BLOCK="$(canon_doc_user_placeholder)"
fi

# ---------------------------------------------------------------------------
# Content gate: refuse the whole write when a block the doc still holds as
# real judgment captured back as its default placeholder. That combination
# only happens when the capture misread - a read racing another refresh, a
# doc briefly vanishing - and writing would stamp placeholders over judgment
# (the 2026-09-13 team-doc loss). Leaving the doc untouched is always safe:
# the next fire reads again. Never blocks: the pointer below still prints.
# ---------------------------------------------------------------------------
WRITE_REFUSED=0
_gate_block() { # $1 label, $2 captured value, $3 default text
  [[ "$WRITE_REFUSED" == "1" ]] && return 0
  local raw=""
  [[ -f "$DOC_PATH" ]] && raw="$(_session_block_raw "$1")"
  if [[ -n "$(printf '%s' "$raw" | tr -d '[:space:]')" && "$raw" != "$3" && "$2" == "$3" ]]; then
    WRITE_REFUSED=1
    echo "canon doc: captured '$1' reads empty while the doc holds it; refusing to overwrite $DOC_PATH" >&2
  fi
  return 0
}
_gate_block "Merge order and why" "$SB1" "$DEFAULT_MERGE"
_gate_block "Open decisions awaiting the operator" "$SB2" "$DEFAULT_DECISIONS"
if [[ "$IS_TEAMED" == "1" ]]; then
  _gate_block "Gaps and open thinking" "$SB3" "$DEFAULT_GAPS"
  _gate_block "Workarounds in force" "$SB4" "$DEFAULT_WORKAROUNDS"
fi
# The user block gets the same guard: it is the one section the machine never
# writes, so re-seeding its placeholder over real notes is the same loss.
if [[ "$WRITE_REFUSED" == "0" && -f "$DOC_PATH" ]] \
  && command -v canon_doc_is_placeholder >/dev/null 2>&1 \
  && command -v canon_doc_extract_marker >/dev/null 2>&1; then
  _uprior="$(canon_doc_extract_marker "$DOC_PATH" user 2>/dev/null || true)"
  if [[ -n "$(printf '%s' "$_uprior" | tr -d '[:space:]')" ]] \
    && ! canon_doc_is_placeholder "$_uprior" \
    && canon_doc_is_placeholder "$USER_BLOCK"; then
    WRITE_REFUSED=1
    echo "canon doc: captured user block reads empty while the doc holds notes; refusing to overwrite $DOC_PATH" >&2
  fi
fi

# ---------------------------------------------------------------------------
# Assemble the doc into a fresh temp file in the doc's own directory, then
# rename it over the doc. Auto block fully regenerated; the session blocks are
# preserved-or-defaulted. The old `> "$DOC_PATH"` truncated the doc on open,
# so a hook killed mid-assembly left a partial doc and a refresh overlapping
# another one captured that partial and stamped placeholders over judgment.
# rename(2) within one directory is atomic, so a reader or a killed writer
# never sees a partial file. Ensure the parent dir exists (handoffs_dir may
# resolve to a path that does not yet exist, e.g. the state-dir fallback on a
# fresh setup); without this the write fails silently and the pointer below
# would lie.
# ---------------------------------------------------------------------------
mkdir -p "$(dirname "$DOC_PATH")" 2>/dev/null || true
_TMP_OUT=""
_doc_tmp_cleanup() { [[ -n "$_TMP_OUT" ]] && rm -f "$_TMP_OUT" 2>/dev/null; return 0; }
trap _doc_tmp_cleanup EXIT
if [[ "$WRITE_REFUSED" != "1" ]]; then
  _TMP_OUT="$(mktemp "$(dirname "$DOC_PATH")/.canon-doc.tmp.XXXXXX" 2>/dev/null)" || _TMP_OUT=""
fi
if [[ -n "$_TMP_OUT" ]]; then
  # Keep the mode the doc already had: mktemp is 0600 and a shared-vault
  # reader must not lose access because the hook refreshed the doc.
  if [[ -f "$DOC_PATH" ]]; then
    _mode="$(stat -f '%Lp' "$DOC_PATH" 2>/dev/null || stat -c '%a' "$DOC_PATH" 2>/dev/null || echo 644)"
    chmod "$_mode" "$_TMP_OUT" 2>/dev/null || true
  fi
  {
  if [[ -n "$(printf '%s' "$PRIOR" | tr -d '[:space:]')" ]]; then
    printf '%s\n\n' "$PRIOR"
  fi
  # A teamed session's doc is the team's rolling doc (scope-keyed name), so
  # the title names the team; a successor reading it is not session ${SHORT}.
  if [[ "$IS_TEAMED" == "1" && -n "$TEAM_SCOPE" ]]; then
    echo "# Canon doc: team ${TEAM_SCOPE}"
  else
    echo "# Canon doc: session ${SHORT}"
  fi
  echo ""
  echo "Session id (authoritative): \`${SID}\`  |  refreshed ${ISO} by precompact-canon-doc.sh."
  # Blank line, deliberately: a paragraph is one physical line, so two echoes
  # in a row read as a wrapped paragraph and `fno doctor lint style` refuses
  # the doc this hook just wrote.
  echo ""
  echo "Mechanical sections below are auto-generated. The two judgment sections are filled by the session."
  echo ""
  echo "<!-- fno:auto -->"
  printf '%s\n' "$AUTO_BLOCK"
  echo "<!-- /fno:auto -->"
  echo ""
  echo "## Merge order and why (session)"
  echo "<!-- fno:session -->"
  printf '%s\n' "$SB1"
  echo "<!-- /fno:session -->"
  echo ""
  echo "## Open decisions awaiting the operator (session)"
  echo "<!-- fno:session -->"
  printf '%s\n' "$SB2"
  echo "<!-- /fno:session -->"
  echo ""
  if [[ "$IS_TEAMED" == "1" ]]; then
    echo "## Gaps and open thinking (session)"
    echo "<!-- fno:session -->"
    printf '%s\n' "$SB3"
    echo "<!-- /fno:session -->"
    echo ""
    echo "## Workarounds in force (session)"
    echo "<!-- fno:session -->"
    printf '%s\n' "$SB4"
    echo "<!-- /fno:session -->"
    echo ""
  fi
  if [[ -n "$(printf '%s' "$EXTRA_SESSIONS" | tr -d '[:space:]')" ]]; then
    printf '%s\n' "$EXTRA_SESSIONS"
    echo ""
  fi
  echo "## User notes (you write here; the machine only ever reads this)"
  echo "<!-- fno:user -->"
  printf '%s\n' "$USER_BLOCK"
  echo "<!-- /fno:user -->"
  # The read check: a fresh word once per compaction, last in the doc. A
  # teamed lead's org checkin refuses until --recall names it, so only a read
  # to the end gets the first beat past a compaction. A real compaction hands
  # this hook its event on stdin; org refresh-canon hands it none and keeps
  # the word, so a routine beat pays no full read.
  if [[ "$IS_TEAMED" == "1" ]]; then
    _recall="$(sed -n 's/^Recall word: *\([0-9a-f]\{1,8\}\).*/\1/p' "$DOC_PATH" 2>/dev/null | tail -1)"
    if [[ -n "$(printf '%s' "$INPUT" | tr -d '[:space:]')" || -z "$_recall" ]]; then
      _recall="$(od -An -N4 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
    fi
    echo ""
    echo "Recall word: ${_recall}"
  fi
  } > "$_TMP_OUT" 2>/dev/null || true
  mv -f "$_TMP_OUT" "$DOC_PATH" 2>/dev/null || true
  _TMP_OUT=""
else
  echo "canon doc: could not create a temp file beside $DOC_PATH; leaving the doc untouched" >&2
fi

# ---------------------------------------------------------------------------
# Stdout: spend the budget on DISCARD, not preserve. The base compaction prompt
# already demands the nine preservation categories; restating them competes for
# the same fixed 20k-token budget. Point at the doc and name what to drop.
# ---------------------------------------------------------------------------
PLAN_LINE=""
if [[ -n "$PLAN" ]]; then
  PLAN_LINE="The plan at ${PLAN} describes intent, not what happened."
fi
cat <<EOF
A canon doc for this session exists at ${DOC_PATH} and is authoritative for
decisions, open questions, and worker state. Do not re-derive its contents
in the summary; spend the budget on the conversation since it was written.
${PLAN_LINE}
Discard raw grep/glob output, files read only for exploration, and
intermediate debugging output - keep conclusions only.
EOF

exit 0
