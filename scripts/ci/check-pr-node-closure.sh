#!/usr/bin/env bash
# check-pr-node-closure.sh - CI gate: a node-bearing branch must exact-claim
# its own node in the PR's closure line.
#
# A PR naming several backlog nodes only ever closed the ONE node
# individually stamped at creation; every other named node stayed open
# forever. The fix is an exact `Fixes <id> [<id>...]` line (the retired
# `Backlog-Closure:` spelling still reads), bound atomically at merge - this
# gate is its CI backstop for the direct `gh pr create` path, which never
# runs the `fno do pr closure-trailer` generator. It never infers extra
# nodes from prose or diffs: it only checks that a node id already present
# in the HEAD ref is also named in the exact closure line. An approved body
# line `Retarget <from> <to> <approval>` counts <from> as claimed when <to>
# is on the closure line, so an approved retarget needs no branch rename.
#
# Run: PR_BODY="<body>" PR_HEAD_REF="<branch>" bash scripts/ci/check-pr-node-closure.sh
# Env: PR_BODY (the PR body), PR_HEAD_REF (the PR's head branch name),
# FNO_AGENTS_BIN (fno-agents binary; default fno-agents on PATH).
# Exit: 0 pass or skip (non-node branch, no PR_HEAD_REF set), 1 missing claim,
# 2 cannot judge (the branch scanner binary is missing or failed).

set -euo pipefail

PR_BODY="${PR_BODY:-}"
PR_HEAD_REF="${PR_HEAD_REF:-}"

# Fail-open: no head ref to check (local run, or an event that carries none).
if [[ -z "$PR_HEAD_REF" ]]; then
  echo "check-pr-node-closure: no PR_HEAD_REF set, skipping."
  exit 0
fi

# Graphless candidate shape, sourced from the shared shell library: the id
# families this repo mints (x-, legacy ab-, the historical compact x form).
# Other shapes - any lowercase-word-hex token like "pr-2809" on a revert
# branch - are ordinary branch words, and CI has no graph to confirm them.
_script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=../lib/node-id.sh
source "${_script_dir}/../lib/node-id.sh"
# _NODE_ID_CLOSURE_RE is anchored (^...$); strip both anchors so this script's
# `^${node_id_re}$` wrapping at the match site stays the single anchor.
node_id_re="${_NODE_ID_CLOSURE_RE#^}"
node_id_re="${node_id_re%\$}"
# FNO_CLOSURE_ID_FAMILIES widens the dashed families for an install whose
# config.backlog.id_prefix differs (CI reads no user config, so the default
# covers only the families THIS repo mints): a fork minting xy-1234 sets
# FNO_CLOSURE_ID_FAMILIES='x|ab|xy' in its workflow. Malformed values are
# ignored, never guessed at.
if [[ -n "${FNO_CLOSURE_ID_FAMILIES:-}" ]]; then
  if [[ "${FNO_CLOSURE_ID_FAMILIES}" =~ ^[a-z0-9]+(\|[a-z0-9]+)*$ ]]; then
    node_id_re="(${FNO_CLOSURE_ID_FAMILIES})-[0-9a-f]{4,8}|x[0-9a-f]{4,8}"
  else
    echo "check-pr-node-closure: FNO_CLOSURE_ID_FAMILIES must be |-separated lowercase families; ignoring '${FNO_CLOSURE_ID_FAMILIES}'." >&2
  fi
fi

# Candidate ids come from the fno-agents binary's pr-closure-branch-ids command:
# the same delimiter-bounded scanner the merge-time bind runs, so this gate can
# no longer disagree with the producer about what the ref names. The bash
# re-glue loop this replaced kept its own split table (a single `IFS='/-'` once
# re-glued "target/deadbeef" as "target-deadbeef") and red a PR over a line
# nothing could generate.
#
# The scanner returns grammar candidates, not confirmed nodes: CI reads no
# graph, so the family filter below still decides which ids this repo mints.
# Any other shape - "pr-2809" on a revert branch - stays an ordinary branch
# word and skips the gate.
bin="${FNO_AGENTS_BIN:-fno-agents}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT
set +e
printf '%s' "$(jq -cn --arg h "$PR_HEAD_REF" '{head_ref:$h}')" \
  | "$bin" pr-closure-branch-ids >"$tmp_dir/ids.json" 2>"$tmp_dir/err"
rc=$?
set -e
if [[ $rc -ne 0 ]]; then
  cause="$(tr '\n' ' ' <"$tmp_dir/err" | sed 's/[[:space:]]*$//')"
  echo "check-pr-node-closure: fno-agents pr-closure-branch-ids unavailable (${cause:-exit $rc}); build it first" >&2
  exit 2
fi
if ! jq -e '(.ids | type) == "array"' "$tmp_dir/ids.json" >/dev/null 2>&1; then
  echo "check-pr-node-closure: fno-agents pr-closure-branch-ids unavailable (stdout was not an ids array); build it first" >&2
  exit 2
fi
candidates=()
while IFS= read -r cand; do
  [[ -z "$cand" ]] && continue
  if [[ "$cand" =~ ^${node_id_re}$ ]]; then
    candidates+=("$cand")
  fi
done < <(jq -r '.ids[]' "$tmp_dir/ids.json")

if [[ ${#candidates[@]} -eq 0 ]]; then
  echo "check-pr-node-closure: no node id in HEAD ref '$PR_HEAD_REF', skipping (non-node branch)."
  exit 0
fi

# The LAST closure line only, either spelling, colon optional (mirrors the
# binary's pr-closure-parse grammar: a stale earlier line, e.g. carried
# forward by a rebase, must not satisfy this).
trailer_line=$(printf '%s\n' "$PR_BODY" | grep -iE '^(fixes|backlog-closure):?[[:space:]]*' | tail -1 || true)

# Strip the keyword itself (plus its optional colon and any immediate
# spaces/tabs), mirroring the Rust grammar. The line is lowercased before the
# strip so a plain sed works on both GNU and BSD; node ids are lowercase by
# grammar, so the comparison below loses nothing. Matching against the raw
# line (keyword still attached) let ANY colon in the line - including a
# stray one BETWEEN two ids, e.g. "Fixes: x-aaaa:x-1111" - read as a valid
# separator via the leading-boundary group below, so the gate passed a line
# the real parser tokenizes as one malformed run and binds zero ids from
# (round-10 review fix, reproduced live: gate passed / parser returned []).
trailer_body=""
if [[ -n "$trailer_line" ]]; then
  trailer_body="$(printf '%s' "$trailer_line" | tr '[:upper:]' '[:lower:]' | sed -E 's/^(fixes|backlog-closure):?[[:space:]]*//')"
fi

# The approved Retarget line, mirroring the Rust grammar in
# org_board/pr_closure.rs (retarget/retargeted_from): keyword case-insensitive
# with optional colon, then exactly three tokens - two differing node ids and
# an approval (msg-... or d-...). Ids stay case-sensitive, so no lowercasing
# here. The LAST well-formed line wins; prose never erases it.
retarget_from=""
retarget_to=""
retarget_approval=""
while IFS= read -r rline; do
  rline="${rline%$'\r'}"
  rrest="${rline:8}"
  rrest="${rrest#:}"
  read -r -a rtok <<< "$rrest"
  if [[ ${#rtok[@]} -ne 3 ]]; then continue; fi
  if [[ "${rtok[0]}" =~ ^${node_id_re}$ && "${rtok[1]}" =~ ^${node_id_re}$ \
       && "${rtok[0]}" != "${rtok[1]}" \
       && "${rtok[2]}" =~ ^(msg-[0-9a-f]{6,}|d-[0-9a-f]{8})$ ]]; then
    retarget_from="${rtok[0]}"
    retarget_to="${rtok[1]}"
    retarget_approval="${rtok[2]}"
  fi
done < <(printf '%s\n' "$PR_BODY" | grep -E '^[Rr][Ee][Tt][Aa][Rr][Gg][Ee][Tt](:|[[:space:]])' || true)

missing=()
retarget_covered=0
for cand in "${candidates[@]}"; do
  # The preceding boundary also accepts "," - the runtime parser's
  # `.replace(",", " ")` before splitting treats a comma as an equivalent
  # separator with no space required either side. "x-aaaa,x-1111" binds both
  # ids at merge time; without "," in the LEADING alternation this gate
  # reports the second id as missing even though it closes correctly
  # (round-8 fix). No ":" in this alternation - trailer_body already has the
  # label's own colon stripped, so any colon reaching here is a real
  # malformed token, not a separator.
  if printf '%s' "$trailer_body" | grep -qE "(^|[[:space:]]|,)${cand}([[:space:]]|,|\$)"; then
    continue
  fi
  # An approved retarget counts the branch node as claimed when the node it
  # hands the PR to sits on the closure line (same boundary regex). The
  # merge owner still refuses the PR until the graph binding has moved.
  if [[ "$cand" == "$retarget_from" ]] \
     && printf '%s' "$trailer_body" | grep -qE "(^|[[:space:]]|,)${retarget_to}([[:space:]]|,|\$)"; then
    retarget_covered=1
    continue
  fi
  missing+=("$cand")
done

# AT LEAST ONE claimed, never all of them. This gate has no graph (see the
# format-check note above), so it cannot tell a real node id from ordinary
# English that fits the same grammar. The producer CAN, and refuses to claim
# an id the graph does not carry, because one unknown id makes the
# merge-time bind-created leg refuse the WHOLE binding at merge.
#
# Demanding all of them therefore made some branches unsatisfiable rather than
# merely strict: on "feature/x-cccc-cache-dead" the producer writes x-cccc and
# this gate demanded "cache-dead", so no body passed both. Reproduced live
# before this change. An unsatisfiable gate is worse than a liberal one - it
# has no green state, so the only way past it is to ignore it.
#
# One claim still catches the defect this gate exists for: a `gh pr create`
# that wrote no trailer at all names zero ids and fails here.
claimed=$(( ${#candidates[@]} - ${#missing[@]} ))
if [[ $claimed -eq 0 ]]; then
  # How many exact trailer lines the body holds, and what the LAST one (the
  # only line this gate and pr-closure-parse read) names - the shape a
  # two-line body needs to understand before it can be fixed.
  trailer_count=$(printf '%s\n' "$PR_BODY" | grep -icE '^(fixes|backlog-closure):?[[:space:]]*' || true)
  {
    echo "check-pr-node-closure: HEAD ref '$PR_HEAD_REF' names $(IFS=,; echo "${candidates[*]}"), and the exact closure line claims none of them."
    if [[ "$trailer_count" -eq 1 ]]; then
      echo "  The body holds 1 closure line; this gate reads only the LAST one."
    else
      echo "  The body holds $trailer_count closure lines; this gate reads only the LAST one."
    fi
    if [[ -n "$trailer_body" ]]; then
      echo "  That last line names: $trailer_body"
    else
      echo "  That last line names: nothing"
    fi
    echo "  The gate wanted: ${candidates[*]}"
    echo "  Remedy: fno do pr closure-trailer <node-id> --extra <id> [--extra <id> ...]"
    echo "  prints ONE Fixes line naming every id. Replace EVERY closure line in"
    echo "  the PR body with that one line. The command checks the ids against the"
    echo "  graph and PRINTS the line; it does not edit the PR. Do NOT paste a"
    echo "  candidate from this message:"
    echo "  a branch segment can match the id grammar without being a real node,"
    echo "  and one unknown id voids the whole binding at merge."
    echo "  Editing the body starts a fresh run of this check by itself. Do NOT"
    echo "  rerun this failed run: a rerun replays the old body and fails again."
    echo "  Branch names the wrong node? Move the graph binding, then add 'Retarget"
    echo "  <branch-node> <right-node> <msg-or-d-id>' under the Fixes line; recipe in skills/ship/references/create.md."
  } >&2
  exit 1
fi

if [[ $retarget_covered -eq 1 ]]; then
  echo "check-pr-node-closure: HEAD ref '$PR_HEAD_REF' node $retarget_from is retargeted to $retarget_to (approval $retarget_approval)."
fi
if [[ ${#missing[@]} -gt 0 ]]; then
  echo "check-pr-node-closure: HEAD ref '$PR_HEAD_REF' claims $claimed of ${#candidates[@]} candidate(s); unclaimed: ${missing[*]} (not demanded - this gate reads no graph)."
elif [[ $retarget_covered -eq 0 ]]; then
  echo "check-pr-node-closure: HEAD ref '$PR_HEAD_REF' node id(s) [${candidates[*]}] all present in the exact trailer."
fi
