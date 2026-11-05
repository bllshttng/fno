#!/usr/bin/env bash
# fno hook: multi-event - surface captured operator turns
# Stop + SessionStart hook: report the operator-capture queue depth, so the
# lead's capture loop is a measured number at the moment, not prose to
# remember.
#
# The failure this closes: a lead records from the direction it is pushed.
# Worker mail arrives as a discrete event (id, queue, ack), so it gets
# recorded; operator conversation is a stream with no boundary, so it does
# not - one lead recorded twenty-two rulings from worker mail and zero from
# operator conversation. `fno inbox operator` derives the queue from the
# session transcript; this hook is the push mechanism that surfaces its
# depth at the two turn boundaries a lead actually pauses on.
#
# Depth-gated on purpose: it prints nothing when the queue is empty, so a
# quiet session costs one bounded read and no context. A non-zero depth
# prints the count, the oldest turn's age, and its excerpt - the same
# measured line inject-mail-notify.sh delivers for mail, which is the channel
# that gets recorded.
#
# NEVER gate this on a team. `hooks/lead-postcompact-reinject.sh` gated on
# `team_level`, a field a different command writes, and exited 0 silently for
# every lead in the fleet. The queue depth here is state the command derives
# for itself; a session with an empty queue is the steady state.
#
# Hook contract: stdout is appended to the session prompt; exit 0 always.
# Codex runs this at SessionStart only (codex-hooks.json): a Codex Stop
# handler must print one JSON object or nothing, and this script prints
# prompt Markdown.
set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

command -v fno >/dev/null 2>&1 || exit 0

HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WT_LIB="$HOOK_DIR/../scripts/lib/hook-budget.sh"
[[ -f "$WT_LIB" ]] || exit 0
# shellcheck source=../scripts/lib/hook-budget.sh
source "$WT_LIB" 2>/dev/null || exit 0

# Overload skip: past the threshold the preamble alone can pass the harness
# cap; the queue depth reports next turn.
hook_overloaded && exit 0

# The queue read derives from the session transcript, so it is keyed per
# session and served from a stale-while-revalidate cache: a copy younger
# than a minute costs milliseconds, a served copy past two thirds of its
# life arms a DETACHED refresher for the next boundary, and a live read that
# skipped or expired under load serves the stale copy rather than nothing.
# The window is one minute because an ack must retire the nag promptly:
# depth a minute old is inside this hook's resolution - it reports a count,
# never a verdict. No fingerprint: the command resolves its own inputs.
session="$(cat 2>/dev/null | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1)"
rc=0
# Role audience: a worker session reads as no queue. Inside
# context-run the audience filter already dropped this producer for the
# session's role; the probe here covers the direct runs (claude hooks.json
# Stop, codex-hooks.json SessionStart) where no filter ran.
if [[ "${FNO_SESSION_ROLE:-}" == "worker" ]]; then
    exit 0
fi
if [[ -n "$session" ]]; then
    source "$HOOK_DIR/lib/agents-bin.sh" 2>/dev/null || true
    probe_bin="$(fno_agents_bin "$HOOK_DIR/.." 2>/dev/null || true)"
    probe_word="$("$probe_bin" context-run --role "$session" 2>/dev/null || true)"
    if [[ "$probe_word" == "worker" ]]; then
        exit 0
    fi
fi
# The inbox family lives at the Rust front door. A dev or smoke checkout
# whose PATH `fno` is the Python wheel no longer carries the word (the
# Python group is deleted), so resolve the checkout's front build first and
# fall back to PATH. A deployed repo has no checkout build and reads the
# installed front either way.
front_bin=""
# Debug outranks release: the checkout's fresh smoke build is debug, and a
# release binary can only be a cache leftover from an older commit.
for candidate in "$HOOK_DIR/../crates/fno/target/debug/fno" "$HOOK_DIR/../crates/fno/target/release/fno"; do
    [[ -x "$candidate" ]] && { front_bin="$candidate"; break; }
done
[[ -n "$front_bin" ]] || front_bin="fno"
if [[ -n "$session" ]]; then
    payload=$(hook_cache_serve "opcap-$session" 60 "" -- "$front_bin" inbox operator status --json 2>/dev/null) || rc=$?
else
    payload=$(hook_run_optional "$front_bin" inbox operator status --json 2>/dev/null) || rc=$?
fi

# A non-zero exit is not silence: collapsing a failed read into an empty
# string is the absence-as-success trap. But exit 2 is Typer's "no such
# command", which a DEPLOYED fno older than this feature returns on every
# turn until someone runs `fno doctor update`; nagging forever is noise.
if [[ $rc -ne 0 ]]; then
    [[ $rc -eq 2 ]] && exit 0
    printf '## Operator capture\n\ncould not be read (fno inbox operator status exit %s). Run it directly.\n' "$rc"
    exit 0
fi

printf '%s' "$payload" | jq -Rsr '
def isint: type == "number" and . == floor;
# Take the first object that LOOKS LIKE THIS PAYLOAD, not the first that
# merely parses: the command shares stdout with whatever preamble the deployed
# fno prints, and a preamble can carry a brace. "depth" present is the
# marker; an unreadable payload is a REPORT, never silence. Offsets walk
# codepoints, so a non-ASCII preamble cannot skew a slice.
explode as $c
| ($c | indices(123)) as $opens
| ($c | indices(125)) as $closes
| [ $opens[] as $i
    | first($closes[] | select(. > $i) as $j
        | ($c[$i:$j + 1] | implode | try fromjson catch null)
        | select(. != null)) // null
    | select(type == "object") ] as $objs
| (first($objs[] | select(has("depth"))) // null) as $p
| if $p == null then
    if ($objs | length) > 0 then
      "## Operator capture\n\nread returned a JSON object with no depth key; run `fno inbox operator status` directly."
    else empty end
  elif ($p.depth | isint | not) or $p.depth <= 0 then empty
  else
    (if ($p.oldest_age_s | isint) then ", oldest \($p.oldest_age_s)s old" else "" end) as $age
    | ($p.oldest_excerpt | if . == null or . == false or . == "" then "" else tostring end) as $excerpt
    | ($p.skipped | if type == "object" then
        [to_entries | sort_by(.key)[] | select(.value | isint)] else [] end) as $sk
    | ([$sk[].value] | add // 0) as $total
    | "## Operator capture: \($p.depth) undispositioned operator turn(s)\($age)\n"
      + (if $total != 0 then
          "\nSkipped at capture: \($total) machine turn(s) (\([$sk[] | "\(.key)=\(.value)"] | join(", ")))\n"
        else "" end)
      + "\nDisposition each before the tick ends: record with `fno backlog idea --source-kind operator_request`, `fno backlog capture add`, or `fno inbox law set`, then `fno inbox operator ack <turn-id> --outcome law:<id>|capture:<fu-id>|node:<id>|nothing`."
      + (if $excerpt != "" then "\n\nOldest: \($excerpt)" else "" end)
  end
'
exit 0
