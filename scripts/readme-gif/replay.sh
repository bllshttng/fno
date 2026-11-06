#!/usr/bin/env bash
# Print an events.jsonl from extract.sh at a compressed, timed pace.
#
# Usage: replay.sh <events.jsonl> [budget_seconds]
#
# The real gaps between events keep their order but are squashed by a square
# root, then scaled so the whole replay fits the budget. The header says it
# is a replay and states the real run length, so nobody reads it as live.
set -euo pipefail

[[ $# -ge 1 ]] || { sed -n '2,8p' "$0" >&2; exit 2; }
budget="${2:-24}"

# One jq pass gives each event its clock stamp and the pause after it.
jq -r -s --argjson budget "$budget" '
  def clock: floor as $s | "\($s / 60 | floor)m\($s % 60 | tostring | if length < 2 then "0" + . else . end)s";
  . as $e
  | [range(1; length) | ($e[.].t - $e[. - 1].t) | if . < 0 then 0 else . end | sqrt] as $g
  | (($g | add) // 0) as $sum
  | (if $sum > 0 then $budget / $sum else 0 end) as $scale
  | "header\t\(($e[-1].t // 0) | clock)\t0\t",
    (range(0; length) as $i | "\($e[$i].kind)\t\($e[$i].t | clock)\t\(($g[$i] // 0) * $scale)\t\($e[$i].text)")
' "$1" | while IFS=$'\t' read -r kind stamp pause text; do
  case "$kind" in
    header)
      printf '\033[2mreplay of a real fno run · %s shown in %ss\033[0m\n\n' "$stamp" "$budget"
      continue ;;
    prompt) line=$'\033[1m> '"$text"$'\033[0m' ;;
    say) line=$'\033[36m● \033[0m'"$text" ;;
    *) line=$'\033[2m  ⎿ '"$text"$'\033[0m' ;;
  esac
  printf '\033[2m%6s\033[0m %s\n' "$stamp" "$line"
  sleep "$pause"
done
