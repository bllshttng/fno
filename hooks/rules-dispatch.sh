#!/usr/bin/env bash
# fno hook: multi-event - the rule engine's hook decision points
# UserPromptSubmit and PreToolUse: `fno-agents hook rules --event <event>`
# decides the event against the rule set (fno::rules::decide) and prints the
# deny, the context, or `{}`. $1 is prompt or pre-tool; a PreToolUse Skill
# call answers as the skill event inside the binary.
#
# Fail open: a missing binary or a failing run allows the call.
stdin=
IFS= read -r -d '' stdin || true
event="${1:-pre-tool}"
root="$(cd -- "${0%/*}/.." && pwd)"
bins=()
[[ -n "${FNO_AGENTS_BIN:-}" ]] && bins+=("$FNO_AGENTS_BIN")
[[ -n "${FNO_AGENTS_FRONT:-}" ]] && bins+=("$FNO_AGENTS_FRONT")
IFS=: read -ra path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
    dir="${dir:-.}"
    [[ -x "$dir/fno-agents" ]] && { bins+=("$dir/fno-agents"); break; }
done
for bin in "${bins[@]}"; do
    [[ -x "$bin" ]] || continue
    if out="$(FNO_REPO_ROOT="$root" "$bin" hook rules --event "$event" <<<"$stdin")"; then
        printf '%s\n' "$out"
        exit 0
    fi
done
printf '%s\n' '{}'
