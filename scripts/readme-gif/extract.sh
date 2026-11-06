#!/usr/bin/env bash
# Turn a Claude Code transcript into a short, redacted event list for replay.
#
# Usage: extract.sh <transcript.jsonl> [OLD=NEW ...] > events.jsonl
#
# Each output line is {"t": seconds_since_start, "kind": "...", "text": "..."}.
# Kinds: prompt, say, tool. Only the first line of each message is kept.
# Each OLD=NEW pair replaces text, so a name or host never reaches the GIF.
# Home paths (/Users/<name>) always become ~.
set -euo pipefail

[[ $# -ge 1 ]] || { sed -n '2,9p' "$0" >&2; exit 2; }
src="$1"
shift

# About what a 24-second replay can show and still be read.
max_events=40

jq -R -n -c --argjson max "$max_events" '
  def ts:
    capture("^(?<b>[^.Z]+)(?<f>\\.[0-9]+)?Z$") as $m
    | ($m.b + "Z" | fromdateiso8601) + ("0" + ($m.f // "") | tonumber);
  def trim: gsub("^\\s+|\\s+$"; "");
  def line1($n):
    (trim | split("\n")[0] // "") as $l
    | if ($l | length) > $n then $l[0:$n - 3] + "..." else $l end;
  # What the user typed, or "" for injected text (hook context, skill bodies).
  def prompt_text:
    if test("<command-name>") then
      ((capture("<command-name>(?<n>[\\s\\S]*?)</command-name>").n | trim)
        + " " + ((capture("<command-args>(?<a>[\\s\\S]*?)</command-args>").a // "") | trim))
      | trim
    elif startswith("<") or startswith("Base directory for this skill")
      or startswith("Stop hook feedback") then ""
    else . end;
  # A reply that is only a control tag (<watching>, <help>, <promise>) is loop
  # plumbing, not something the agent said.
  def say_text: if (trim | startswith("<")) then "" else . end;
  def tool_label:
    .name as $n | (.input // {}) as $i
    | if $n == "Bash" then ($i.description // ($i.command // "" | line1(80)))
      elif $n == "Edit" or $n == "Write" or $n == "Read" then "\($n) \(($i.file_path // "") | split("/") | last)"
      elif $n == "Skill" then "skill \($i.skill // "")"
      else $n end;
  def redact:
    reduce ($ARGS.positional[] | select(contains("=")) | split("=")) as $p
      (.; split($p[0]) | join($p[1:] | join("=")))
    | gsub("/Users/[^/\\s]+"; "~");
  # Keep the first and last events; sample the middle evenly to fit the cap.
  def thin($cap):
    if length <= $cap then .
    else . as $e | (length - 2) as $n
      | [$e[0]] + [range(0; $cap - 2) | $e[1 + (. * $n / ($cap - 2) | floor)]] + [$e[-1]]
    end;

  [inputs | fromjson? | select((.type == "user" or .type == "assistant") and .timestamp)] as $rows
  | if ($rows | length) == 0 then empty else
      ($rows[0].timestamp | ts) as $start
      | [ $rows[]
          | ((.timestamp | ts) - $start) as $t
          | .type as $ty
          | (.message.content | if type == "string" then [{type: "text", text: .}] else (. // []) end)[]
          | if $ty == "user" and .type == "text" then {t: $t, kind: "prompt", text: (.text | prompt_text | line1(80))}
            elif $ty == "assistant" and .type == "text" then {t: $t, kind: "say", text: (.text | say_text | line1(80))}
            elif $ty == "assistant" and .type == "tool_use" then {t: $t, kind: "tool", text: (tool_label | line1(80))}
            else empty end
          | .text |= redact
          | select(.text != "") ]
      | thin($max)[]
    end
' --args -- "$@" < "$src"
