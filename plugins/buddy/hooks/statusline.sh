#!/bin/bash
# The status line entry point. Most ticks change nothing on screen: the same session input, the
# same width, and the same buddy frame as the last run. Then this prints the last output and
# no process starts. It uses only bash built-ins: under load, each extra process costs more
# than the whole check. Every HITS-th tick still runs the renderer, which drops a frame from a
# session that stopped drawing and stamps the heartbeat the mod reads.
# At one tick a second this must stay well under WRAPPER_SEEN_MS (45 s) in register.ts.
HITS=25
IFS= read -r -d '' input
dir=.
[[ $0 == */* ]] && dir=${0%/*}
[[ $dir == /* ]] || dir="$PWD/$dir"
session='"session_id"[[:space:]]*:[[:space:]]*"([^"]*)"'
# A regex match runs in C. An extglob substitution here took seconds of CPU on a 1.5 KB input in bash 3.2.
duration='^(.*)"[a-z_]*duration_ms"[[:space:]]*:[[:space:]]*[0-9.]+(.*)$'
if [[ $input =~ $session ]]; then
  f="$dir/frames/${BASH_REMATCH[1]}"
  # Durations tick every second without changing what the line shows, so they stay out of the key.
  rest=$input
  while [[ $rest =~ $duration ]]; do rest="${BASH_REMATCH[1]}${BASH_REMATCH[2]}"; done
  # The frame's own text is in the key: bash 3.2 compares file times in whole seconds, which is too
  # coarse for frames that land twice a second. read -d '' stops at end of file and reports a
  # failure there, so its status is not the test.
  frame= last= out= hits=0
  [[ -f $f.json ]] && IFS= read -r -d '' frame < "$f.json"
  key="$COLUMNS $rest $frame"
  # One file holds the key, a NUL, then the output. The renderer replaces the whole file, so a key
  # never pairs with the output of another run or with a half-written one.
  [[ -f $f.cache ]] && { IFS= read -r -d '' last; IFS= read -r -d '' out; } < "$f.cache"
  [[ -f $f.hits ]] && read -r hits < "$f.hits"
  if [[ -n $out && $last == "$key" && $hits -lt $HITS ]]; then
    printf '%s' $((hits + 1)) > "$f.hits" 2>/dev/null
    printf '%s' "$out"
    exit 0
  fi
  printf '0' > "$f.hits" 2>/dev/null
  export BUDDY_KEY="$key"
fi
export BUDDY_HOME="$dir"
# The renderer ships in fno-agents. A plugin update can land before the binary that knows this
# arm, so a missing or failed renderer falls back to the user's own line: the buddy hides, the
# user's status line never goes blank.
if command -v fno-agents >/dev/null 2>&1 \
    && rendered=$(printf '%s' "$input" | fno-agents hook buddy-statusline 2>/dev/null) && [[ -n $rendered ]]; then
  printf '%s\n' "$rendered"
  exit 0
fi
own=$(jq -r 'select(.statusLine.type == "command") | .statusLine.command // empty' "$dir/inner.json" 2>/dev/null)
[[ -n $own ]] && printf '%s' "$input" | exec /bin/sh -c "$own"
exit 0
