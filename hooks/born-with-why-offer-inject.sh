#!/usr/bin/env bash
# fno hook: UserPromptSubmit - born with why offer inject
# hooks/born-with-why-offer-inject.sh -- surface a pending born-with-why offer.
#
# UserPromptSubmit hook. The attended born-with-why path (backlog/birth.rs) emits a
# `think_offered` event to .fno/events.jsonl, but its only surfacing to the agent
# is a stderr line that can be misread or dropped. This hook re-surfaces an
# unconsumed offer ONCE as a <system-reminder> the harness owns, so the operator
# actually gets the yes/no choice the offer path exists to present.
#
# Read-only over .fno/events.jsonl + a byte-offset cursor (.fno/.think-offer-cursor).
# Never blocks, never mutates graph/state, always exits 0. Fires exactly once per
# offer: the cursor advances to EOF after each scan, so a consumed event never
# re-surfaces (AC2-ERR). A malformed/truncated events line is skipped (AC2-EDGE).
#
# ponytail: single project-local cursor (not session-keyed) -- surfacing an
# attended offer once TOTAL across sessions is the intent; two concurrent
# sessions sharing .fno should not both nag. Bursts DO happen (four births 3s
# apart, 2026-07-30T02:39:58..02:40:08), so only the newest gets the full offer,
# but the rest ride along as bare ids: the cursor eats them either way, and
# naming them is the difference between deferred and destroyed.

set -uo pipefail

# Survive a caller env with no usable PATH (see worktree-write-protect.sh).
PATH="${PATH:+$PATH:}/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

# fno shells can wedge on a stalled daemon / graph lock; bound every call with
# the shared load-aware budget rather than the harness's 30s hook timeout.
# Fails closed like the other injection hooks: a missing helper
# exits 0.
HOOK_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/hook-budget.sh
source "$HOOK_DIR/../scripts/lib/hook-budget.sh" 2>/dev/null || exit 0
# Overload skip BEFORE the cursor: past the threshold the preamble alone can
# pass the harness cap, and a death mid-scan would burn the one-way slice.
# The cursor stays put, so the offer re-surfaces next turn.
hook_overloaded && exit 0
OFFER_BUDGET="$(hook_budget_secs)"
# shellcheck source=../scripts/lib/events-lock.sh
source "$HOOK_DIR/../scripts/lib/events-lock.sh" 2>/dev/null || exit 0

REPO_ROOT=$(git -C "$PWD" rev-parse --show-toplevel 2>/dev/null || echo "$PWD")
# HOME-as-repo guard (law d-8ddaba56): a cwd of $HOME outside git resolves
# REPO_ROOT to $HOME, so the cursor write below would land a dot file at the
# top level of the state root. No checkout, no offer to surface.
if [[ "$REPO_ROOT" == "$HOME" ]] \
    || [[ "$(cd "${FNO_HOME:-$HOME/.fno}" 2>/dev/null && pwd -P || true)" \
        == "$(cd "$REPO_ROOT/.fno" 2>/dev/null && pwd -P || true)" ]]; then
    exit 0
fi
# The project journal and the cursor both live in the repo's space, so every
# worktree shares one daily offer budget. Resolution is deliberately
# subprocess-free: the migration leaves a MOVED-TO pointer in the checkout, so
# a stat finds the space; a fresh checkout has no journal at all and exits
# before any expensive work, which is the posture the old hook had and the
# smoke test's prompt-return bound still holds.
EVENTS_DIR=""
if [[ -f "$REPO_ROOT/.fno/MOVED-TO" ]]; then
    EVENTS_DIR=$(head -n 1 "$REPO_ROOT/.fno/MOVED-TO" 2>/dev/null || true)
fi
if [[ -z "$EVENTS_DIR" && -f "$REPO_ROOT/.fno/events.jsonl" ]]; then
    EVENTS_DIR="$REPO_ROOT/.fno"
fi
if [[ -z "$EVENTS_DIR" ]]; then
    # Never migrated, never created: no journal to read, no offer to make.
    exit 0
fi
EVENTS="$EVENTS_DIR/events.jsonl"
[[ -f "$EVENTS" ]] || exit 0

CURSOR="$EVENTS_DIR/.think-offer-cursor"
if [[ -L "$CURSOR" ]]; then
    CURSOR=$(_resolve_event_symlink "$CURSOR") || exit 0
fi
CURSOR_LOCK="${CURSOR}.lock.d"
CURSOR_LOCK_TOKEN="$(hostname):$$:$(date -u +%s):$RANDOM"

cursor_lock_attempts=0
while ! mkdir "$CURSOR_LOCK" 2>/dev/null; do
    if _steal_stale_event_dir "$CURSOR_LOCK"; then
        continue
    fi
    (( cursor_lock_attempts >= 20 )) && exit 0
    sleep 0.05
    cursor_lock_attempts=$((cursor_lock_attempts + 1))
done
printf '%s' "$CURSOR_LOCK_TOKEN" > "$CURSOR_LOCK/owner" 2>/dev/null || {
    rmdir "$CURSOR_LOCK" 2>/dev/null || true
    exit 0
}
cleanup_cursor_lock() {
    [[ -n "${SLICE_FILE:-}" ]] && command -p rm -f "$SLICE_FILE" 2>/dev/null
    [[ -r "$CURSOR_LOCK/owner" ]] || return
    [[ "$(< "$CURSOR_LOCK/owner")" == "$CURSOR_LOCK_TOKEN" ]] || return
    command -p rm -f "$CURSOR_LOCK/owner" 2>/dev/null || true
    rmdir "$CURSOR_LOCK" 2>/dev/null || true
}
trap cleanup_cursor_lock EXIT

# jq is required before the cursor can move. A GC rewrite once published a
# recoverable inode-pinned cursor mapping before replacing the journal; finish
# that mapping here if an old GC died in the narrow replace-to-cursor window.
command -v jq >/dev/null 2>&1 || exit 0
CURSOR_PENDING="${CURSOR}.gc-pending"
if [[ -f "$CURSOR_PENDING" ]]; then
    pending=$(jq -er '
        if (.cursor | type) == "number" and .cursor == (.cursor | floor) and .cursor >= 0
        then "\(.device) \(.inode) \(.cursor)" else error("invalid cursor") end' \
        "$CURSOR_PENDING" 2>/dev/null) || exit 0
    read -r p_dev p_ino p_cursor <<<"$pending"
    events_id=$(stat -c '%d %i' "$EVENTS" 2>/dev/null || stat -f '%d %i' "$EVENTS" 2>/dev/null) || exit 0
    if [[ "$events_id" == "$p_dev $p_ino" ]]; then
        recover="${CURSOR}.recover.$$"
        if ! (set -o noclobber; printf '%s' "$p_cursor" > "$recover") 2>/dev/null \
            || ! mv -f "$recover" "$CURSOR" 2>/dev/null; then
            command -p rm -f "$recover" 2>/dev/null
            exit 0
        fi
        command -p rm -f "$CURSOR_PENDING" 2>/dev/null || exit 0
    fi
fi

size=$(wc -c < "$EVENTS" 2>/dev/null | tr -d ' ')
[[ "$size" =~ ^[0-9]+$ ]] || exit 0

offset=0
[[ -f "$CURSOR" ]] && offset=$(tr -d ' \n' < "$CURSOR" 2>/dev/null)
[[ "$offset" =~ ^[0-9]+$ ]] || offset=0
# File shrank/rotated -> reset to start (don't trust a stale offset).
(( offset > size )) && offset=0
# Nothing new appended since last scan.
(( offset >= size )) && exit 0

# Never burn the slice we cannot deliver: the cursor advance below is one-way,
# and both tools are used unconditionally after it. jq is NOT a given here -
# session-start.sh exits silently without it - and losing the slice is worse
# than re-scanning it next turn.
# Scan only the slice [offset, size): bound the read with `head -c` so events
# appended AFTER we captured `size` are NOT consumed here -- the cursor only
# advances to `size`, so a racing append belongs to the next run, never both
# (once-per-offer). Per-line JSON parse so a malformed/truncated line is skipped,
# not fatal. Latest think_offered wins; carry its offer_line, the authoritative
# command the offer path recorded (a reconstructed bare `/think <id>` is a single
# non-mode token the router rejects -- skills/think/SKILL.md). Older offers in
# the slice ride along as bare ids. \x1f-separated, not tab: tab is
# IFS-whitespace, so `read` would collapse an empty offer_line and shift the
# ride-along list into it (same trap the enrichment parse below documents).
# The slice lands in a file first and must hold exactly the byte count
# measured above. A completion sentinel alone proves the parser ran, not that
# it was fed: a failing `tail`/`head` hands it an empty slice, it finds no
# offers, and the cursor advances over a live offer. A short read leaves the
# sentinel off instead, so the cursor stays put and the next turn re-scans.
# jq -R decodes with replacement, never raising: .fno/events.jsonl has
# non-JSON-aware appenders and `head -c` cuts on a byte boundary, so a
# truncated multi-byte char is reachable; a mangled line fails fromjson and is
# skipped. offer_line is free text, so it gets the same reminder-tag defang
# the enrichment parse applies to title/details.
SLICE_FILE=$(mktemp "${TMPDIR:-/tmp}/born-with-why-slice-XXXXXX") || exit 0
tail -c +"$((offset + 1))" "$EVENTS" 2>/dev/null | head -c "$((size - offset))" > "$SLICE_FILE" 2>/dev/null
slice_bytes=$(wc -c < "$SLICE_FILE" 2>/dev/null | tr -d ' ')
parsed=""
if [[ "$slice_bytes" == "$((size - offset))" ]]; then
    # Trailing \x04 is the completion sentinel: the caller refuses to advance
    # without it, so a failed parse re-scans instead of eating the slice. A
    # clean scan that found no offer still carries it, and still advances.
    parsed=$(jq -Rsj '
        def defang: gsub("<\\s*(?<s>/?)\\s*system-reminder\\s*>"; "[\(.s)system-reminder]"; "i");
        def falsy: . == null or . == false or . == 0 or . == "" or . == [] or . == {};
        reduce (split("\n")[]
            | try fromjson catch null
            | select(type == "object" and .type == "think_offered")
            | (.data | if falsy then {} else . end)
            | select(type == "object")
            | {x: .node_id, o: .offer_line}
            | select((.x | type) == "string" and .x != "")) as $e
          ({nid: "", offer: "", older: []};
           (if .nid != "" then .older += [.nid] else . end)
           | .nid = ($e.x | defang)
           | .offer = (if ($e.o | type) == "string" then ($e.o | defang) else "" end))
        # Cap the ride-along list. Not defensive padding: on a COLD START there is no
        # cursor, so offset is 0 and the entire history is one slice - measured at 418
        # ids and 3472 chars against the events.jsonl in this repo. Bursts are small (4
        # is the observed max), which is why an earlier cut of this deleted the cap as
        # speculative; that reasoning missed the offset==0 case entirely.
        | .older as $o
        | .older = (if ($o | length) > 5 then $o[-5:] + ["+\(($o | length) - 5) more"] else $o end)
        | [.nid, .offer, (.older | join(", "))] | join("\u001f")
    ' < "$SLICE_FILE" 2>/dev/null) && parsed+=$'\x04'
fi
command -p rm -f "$SLICE_FILE" 2>/dev/null
SLICE_FILE=""

# No completion sentinel -> the parse died rather than finding nothing. Leave the
# cursor where it is so the next turn re-scans; burning the slice on a broken
# interpreter is the loss this hook exists to prevent.
[[ "$parsed" == *$'\x04' ]] || exit 0
parsed="${parsed%$'\x04'}"

# Advance the cursor to the captured EOF regardless of what we found -- consuming
# exactly the [offset, size) slice we scanned is what makes the reminder fire
# once per offer.
printf '%s' "$size" > "$CURSOR" 2>/dev/null || true

IFS=$'\x1f' read -r node_id offer_cmd older_ids <<<"$parsed"
[[ -n "$node_id" ]] || exit 0

# The cursor already consumed these, so this clause is their only surfacing.
also_born_line=""
[[ -n "${older_ids:-}" ]] && also_born_line="
Also born this gap (not offered separately): ${older_ids}."

# Suppressing the NEWEST offer must not destroy the older ids with it: the guard
# below exits before any reminder is built, and the cursor is already past the
# whole slice (codex P2). Naming an id is not an offer, so it is safe to list
# them unresolved - a stale id is noise, a discarded one is data loss.
emit_older_only() {
    [[ -n "${older_ids:-}" ]] || exit 0
    jq -n --arg ctx "<system-reminder>
Nodes born this gap, not offered: ${older_ids}.
</system-reminder>" '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":$ctx}}'
    exit 0
}

# Resolve + in-progress guard: suppress an offer that should not reach the
# operator. Two cases, both from one `fno backlog get`:
#   (a) PHANTOM  -- the node no longer resolves (removed / superseded / a
#       never-persisted legacy-prefix id). Keyed off the command's EXIT CODE.
#   (b) UNDERWAY -- the node is already being worked: it has a PR, or a
#       lifecycle state past just-born (claimed / next / done / superseded). A
#       born-with-why /think only makes sense on a just-born, not-yet-started
#       node; once it is claimed or has a PR the "why" conversation already
#       happened, so re-offering just spawns a DUPLICATE /think on a live
#       session (observed: offered in an unrelated session AND in its
#       own, while claimed + PR open). Keyed off the resolved JSON.
# Degrade to surfacing whenever `fno` is unavailable or its output cannot be
# parsed, so a missing/garbled resolver never eats a real fresh offer. Run from
# $REPO_ROOT so resolution is deterministic even if graph_json is project-local.
if command -v fno >/dev/null 2>&1; then
    # `cd` failure exits 1 -- the SAME code as an authoritative not-found -- so a
    # deleted worktree (archive-worktree.sh / `fno agents workspace worktree cleanup` can remove one
    # under a live session) would read as "node absent" and destroy a live offer.
    # Map it to 99 so it lands in the degrade-to-surfacing branch below.
    node_json=$( cd "$REPO_ROOT" 2>/dev/null || exit 99; with_timeout "$OFFER_BUDGET" fno backlog get "$node_id" 2>/dev/null )
    _get_rc=$?
    if [[ "$_get_rc" -eq 0 ]]; then
        # Resolved. Suppress only if the node is already underway; a parse
        # failure or unknown shape exits 1 -> surface (fail safe).
        if printf '%s' "$node_json" | jq -e '
            type == "object" and (
                (.pr_number | . != null and . != false and . != 0 and . != "" and . != [] and . != {})
                or (.status | IN("in_progress", "claimed", "next", "done", "superseded")))
        ' >/dev/null 2>&1; then
            emit_older_only
        fi
    elif [[ "$_get_rc" -eq 1 ]]; then
        # Authoritative not-found -> phantom, suppress.
        emit_older_only
    fi
    # Any OTHER nonzero degrades to surfacing, because the cursor already
    # advanced and suppressing would discard a possibly-real offer for good.
    # Only rc 1 means "graph read cleanly, node absent": `fno backlog get` exits
    # 3 on an unreadable graph (GRAPH_UNREADABLE_EXIT, crates/fno-agents/src/backlog/get_cli.rs),
    # 2 on a click usage error, 124 when with_timeout kills a wedged call, and 99
    # when the `cd` above failed. Every one of those is transient or our own
    # fault, never evidence the node does not exist.
fi

# Fall back to the router-valid dispatch form if the event carried no offer_line.
[[ -n "$offer_cmd" ]] || offer_cmd="/think dispatch ${node_id}"

# The full v1 bare-id reminder. This is the FALLBACK, surfaced verbatim whenever
# enrichment cannot run or parse (fno unavailable, empty/non-dict node body,
# titleless node) -- never blank, never truncated JSON (AC2-ERR). It is the ONLY
# place the v1 "offer is pending" phrasing survives.
v1_reminder="<system-reminder>
A born-with-why offer is pending for ${node_id}. Surface it to the operator as a
yes/no before wrapping up: \"Run \`${offer_cmd}\` now, or skip?\" This is an
offer, not something that already ran - nothing was spawned.${also_born_line}
</system-reminder>"

reminder="$v1_reminder"

# Enrichment (offer path only). Reuse $node_json captured by the underway guard
# above -- the offered node is fetched exactly once, ever. One parse emits
# tab-separated title / <=200-char why-excerpt / domain; whitespace is collapsed
# first so neither field can carry a tab or newline. Any failure (fno absent, so
# node_json unset; empty or non-dict body; titleless node) leaves $reminder as
# the v1 fallback.
if [[ -n "${node_json:-}" ]]; then
    # Node title/details are free text embedded inside the hook-owned
    # <system-reminder> wrapper. jq --arg keeps the JSON valid but does NOT
    # neutralize a literal </system-reminder> in that text, so the reminder
    # delimiter is defanged first; the real wrapper is added in bash. Unit
    # separator between fields: tab is IFS-whitespace, so bash read would
    # collapse an empty why field and shift domain into it.
    enrich=$(printf '%s' "$node_json" | jq -j '
        def defang: gsub("<\\s*(?<s>/?)\\s*system-reminder\\s*>"; "[\(.s)system-reminder]"; "i");
        def falsy: . == null or . == false or . == 0 or . == "" or . == [] or . == {};
        def words: if falsy then "" elif type == "string" then [splits("\\s+") | select(. != "")] | join(" ") else error("not text") end;
        if type != "object" then error("not a node") else . end
        | (.title | words | defang) as $title
        | if $title == "" then error("no title") else . end
        | (.details | words | defang) as $why0
        | (if ($why0 | length) > 200 then
             ($why0[0:200]) as $head
             | ($head | if test(" ") then sub(" [^ ]*$"; "") else . end | sub("\\s+$"; "")) as $cut
             | (if $cut == "" then $head else $cut end) + "…"
           else $why0 end) as $why
        | (.domain | if falsy then "" elif type == "string" then sub("^\\s+"; "") | sub("\\s+$"; "") else error("not text") end) as $domain
        | [$title, $why, $domain] | join("\u001f")
    ' 2>/dev/null) || enrich=""

    if [[ -n "$enrich" ]]; then
        IFS=$'\x1f' read -r e_title e_why e_domain <<<"$enrich"

        # Empty details -> omit the "Why:" clause entirely, no dangling label (AC1-EDGE).
        why_line=""
        [[ -n "$e_why" ]] && why_line=" Why: ${e_why}."

        # Second candidate (US3): top-ranked ready node sharing the offered node's
        # domain (board/rank order, excluding the offered node), else `fno backlog
        # next`, else none. Every step degrades to empty on failure -> solo offer.
        cand_id=""
        cand_title=""
        if command -v fno >/dev/null 2>&1; then
            cand_id=$( cd "$REPO_ROOT" && with_timeout "$OFFER_BUDGET" fno backlog ready 2>/dev/null \
                | jq -r --arg offered "$node_id" --arg domain "$e_domain" '
                    if type == "array" then
                        first(.[] | select(type == "object" and .id != $offered)
                            | select($domain == "" or .domain == $domain)
                            | (.id | if . == null or . == false or . == "" then "" else tostring end))
                    else empty end' 2>/dev/null ) || cand_id=""

            if [[ -z "$cand_id" ]]; then
                cand_id=$( cd "$REPO_ROOT" && with_timeout "$OFFER_BUDGET" fno backlog next 2>/dev/null \
                    | jq -r --arg offered "$node_id" '
                        if type == "object" and (.id | . != null and . != false and . != "") and .id != $offered
                        then .id | tostring else empty end' 2>/dev/null ) || cand_id=""
            fi

            if [[ -n "$cand_id" ]]; then
                cand_title=$( cd "$REPO_ROOT" && with_timeout "$OFFER_BUDGET" fno backlog get "$cand_id" 2>/dev/null \
                    | jq -r '
                        def defang: gsub("<\\s*(?<s>/?)\\s*system-reminder\\s*>"; "[\(.s)system-reminder]"; "i");
                        if type == "object" then
                            (.title | if . == null or . == false or . == "" then "" else [splits("\\s+") | select(. != "")] | join(" ") end)
                        else "" end | defang' 2>/dev/null ) || cand_title=""
                # No title -> drop the candidate (the "Also on deck" shape needs one).
                [[ -n "$cand_title" ]] || cand_id=""
            fi
        fi

        ondeck_line=""
        [[ -n "$cand_id" ]] && ondeck_line="

Also on deck: ${cand_id} - \"${cand_title}\" (\`/think ${cand_id}\`)."

        reminder="<system-reminder>
It's about time you think about ${node_id} - \"${e_title}\".${why_line}
Run \`${offer_cmd}\` now, or skip?${also_born_line}${ondeck_line}

This is an offer, not something that already ran - nothing was spawned.
</system-reminder>"
    fi
fi

# jq presence was checked before the cursor advanced, so reaching here means it
# exists. All node text reaches JSON only through --arg, so backticks / quotes /
# $() in a title render literally and never trigger shell expansion (AC2-EDGE).
jq -n --arg ctx "$reminder" \
    '{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":$ctx}}'

exit 0
