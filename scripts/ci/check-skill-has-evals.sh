#!/usr/bin/env bash
# check-skill-has-evals.sh - CI gate: a PR that changes skills/<name>/SKILL.md
# must leave an eval case beside it, in skills/<name>/evals/. Only changed
# skills are checked, so the rest of the tree adopts the convention as each
# skill is next touched. `fno doctor evals run` loads skills/<name>/evals/*.yaml
# (golden-task format); `claude plugin eval --eval-dir skills/<name>/evals` reads
# the case directories there. One runner reads both, so a case here runs.
#
# Base: PR_BASE_REF (default main) merge base on a PR; FILE_BUDGET-style push
# runs pass SKILL_EVALS_BASE_SHA (the previous tip). With no base the gate
# says so and passes, never a silent clean read.
#
# Run: bash scripts/ci/check-skill-has-evals.sh
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

base="${SKILL_EVALS_BASE_SHA:-}"
if [[ -z "$base" || "$base" =~ ^0+$ ]]; then
    base=$(git merge-base HEAD "origin/${PR_BASE_REF:-main}" 2>/dev/null || true)
fi
if [[ -z "$base" ]]; then
    echo "check-skill-has-evals: no base to diff against; nothing checked"
    exit 0
fi

if ! changed=$(git diff --name-only --diff-filter=AMR "$base" HEAD -- 'skills/*/SKILL.md'); then
    echo "check-skill-has-evals: 'git diff' failed" >&2
    exit 1
fi

checked=0
missing=()
while IFS= read -r skill_md; do
    [[ -z "$skill_md" ]] && continue
    checked=$((checked + 1))
    dir=$(dirname "$skill_md")
    if [[ -z "$(git ls-files -- "$dir/evals")" ]]; then
        missing+=("$dir")
    fi
done <<< "$changed"

if ((${#missing[@]} == 0)); then
    echo "check-skill-has-evals: ${checked} changed SKILL.md file(s), each with an evals/ case"
    exit 0
fi

{
    echo "check-skill-has-evals: this change edits a SKILL.md with no eval beside it:"
    echo
    printf '  %s/evals/\n' "${missing[@]}"
    echo
    echo "Add a golden task (skills/<name>/evals/<id>.yaml, the evals/bank format that"
    echo "'fno doctor evals run' loads) or a 'claude plugin eval' case directory. For a"
    echo "pack skill, put it in the plugin source and run scripts/generate-skill-bundles.sh."
} >&2
exit 1
