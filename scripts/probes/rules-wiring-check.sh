#!/usr/bin/env bash
# rules-wiring-check.sh - how many of the 8 rule decision points call
# fno::rules::decide, read from the working tree (docs/architecture/event-rules.md).
#
# The 4 hook points (prompt, skill, pre-tool Edit|Write, pre-tool Bash) count
# when both manifests register hooks/rules-dispatch.sh and the hook module
# reaches rules_source::rule. Each door counts when its file names
# rules_source::rule. The STAGES table, row_paths, edit_answer and the old
# law-stage-inject hook must be gone. Prints `decision points wired: N/8`.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

wired=0
missing=()
has() { grep -q -- "$2" "$1" 2>/dev/null; }

hooks_ok=1
has crates/fno-agents/src/hook/rules.rs 'rules_source::rule(' || hooks_ok=0
for manifest in hooks/hooks.json hooks/codex-hooks.json; do
    has "$manifest" 'rules-dispatch.sh\\" prompt' || hooks_ok=0
    has "$manifest" 'rules-dispatch.sh\\" pre-tool' || hooks_ok=0
done
if [[ $hooks_ok -eq 1 ]]; then
    wired=$((wired + 4))
else
    missing+=("hooks: prompt, skill, pre-tool Edit|Write, pre-tool Bash")
fi

for door in \
    "door:merge crates/fno-agents/src/authorized_merge.rs" \
    "door:send crates/fno-agents/src/mail_cli/send/mod.rs" \
    "door:spawn crates/fno-agents/src/spawn_gate.rs" \
    "plan-start crates/fno-agents/src/target_init.rs"; do
    name="${door%% *}" file="${door#* }"
    if has "$file" 'rules_source::rule('; then
        wired=$((wired + 1))
    else
        missing+=("$name ($file)")
    fi
done

residue="$(git grep -n -E 'const STAGES|fn row_paths|fn edit_answer|law-stage-inject' -- crates hooks 2>/dev/null)"
if [[ -n "$residue" ]]; then
    echo "residue of the old law read:"
    printf '%s\n' "$residue"
    wired=0
fi

for m in ${missing[@]+"${missing[@]}"}; do
    echo "not wired: $m"
done
echo "decision points wired: $wired/8"
[[ $wired -eq 8 ]]
