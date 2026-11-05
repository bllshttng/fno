#!/usr/bin/env bash
# Acceptance runs in CI against archives and a disposable remote, never live refs.
set -euo pipefail
[[ "${GITHUB_ACTIONS:-}" == true ]] || { echo 'run plugin-tree acceptance in CI' >&2; exit 1; }
repo=$(git rev-parse --show-toplevel)
scratch=$(mktemp -d)
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT
mkdir "$scratch/source"
git archive HEAD | tar -x -C "$scratch/source"
sha=$(git rev-parse HEAD)
rustc --edition=2021 "$scratch/source/crates/fno-agents/ci/plugin_tree.rs" -o "$scratch/plugin-tree"
"$scratch/plugin-tree" build "$scratch/source" "$scratch/tree" "$sha" 0.0.0-ci
test -f "$scratch/tree/docs/HARNESSES.md"
test -f "$scratch/tree/docs/architecture/help-routing.md"
claude plugin validate "$scratch/tree" --strict
mkdir "$scratch/codex-home"
CODEX_HOME="$scratch/codex-home" codex plugin marketplace add "$scratch/tree" --json
CODEX_HOME="$scratch/codex-home" codex plugin add fno@fno --json
find "$scratch/codex-home" -name SOURCE_COMMIT -print > "$scratch/installed-stamps"
[[ -s "$scratch/installed-stamps" ]]
while IFS= read -r stamp; do cmp "$stamp" "$scratch/tree/SOURCE_COMMIT"; done < "$scratch/installed-stamps"

expect_refusal() {
    local needle=$1
    if "$scratch/plugin-tree" check "$scratch/tree" >"$scratch/refusal" 2>&1; then
        echo "expected refusal: $needle" >&2; exit 1
    fi
    cat "$scratch/refusal"
    grep -F "$needle" "$scratch/refusal"
}

printf '%s\n' 'uv run --project "${CLAUDE_PLUGIN_ROOT}/cli" python helper.py' > "$scratch/tree/hooks/unsafe-env.sh"
expect_refusal 'hook may create a plugin environment'
rm "$scratch/tree/hooks/unsafe-env.sh"

mkdir "$scratch/tree/cli"
touch "$scratch/tree/cli/forbidden"
expect_refusal 'forbidden plugin path'
rm -r "$scratch/tree/cli"
mkdir "$scratch/tree/crates"
touch "$scratch/tree/crates/forbidden"
expect_refusal 'forbidden plugin path'
rm -r "$scratch/tree/crates"
ln "$scratch/tree/SOURCE_COMMIT" "$scratch/tree/hardlink"
expect_refusal 'hard link refused'
rm "$scratch/tree/hardlink"
ln -s SOURCE_COMMIT "$scratch/tree/symlink"
expect_refusal 'symlink refused'
rm "$scratch/tree/symlink"
count=$(find "$scratch/tree" -type f | wc -l)
mkdir "$scratch/tree/overflow"
for ((i=count; i<5000; i++)); do touch "$scratch/tree/overflow/$i"; done
"$scratch/plugin-tree" check "$scratch/tree"
touch "$scratch/tree/overflow/one-too-many"
expect_refusal '5001 files; maximum is 5000'
rm -r "$scratch/tree/overflow"

# The packaged joined-worker hook must relay the native judge's denial and
# start no uv. This job builds no fno-agents, so a stub answers the
# `hook join-partition-guard` arm with the judge's block shape; the judge's
# own decisions are unit-tested beside it in crates/fno-agents.
mkdir -p "$scratch/project/.fno/join-partition" "$scratch/bin" "$scratch/home"
printf '%s\n' '{"band":"one","deny_edit":["peer.rs"]}' > "$scratch/project/.fno/join-partition/ci-one.json"
printf '#!/bin/sh\ntouch "%s/uv-called"\nexit 1\n' "$scratch" > "$scratch/bin/uv"
printf '#!/bin/sh\n[ "$1 $2" = "hook join-partition-guard" ] || exit 2\ncat > "%s/judge-input"\nprintf '\''%%s\\n'\'' '\''{"decision":"block","reason":"peer.rs","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"peer.rs"}}'\''\n' "$scratch" > "$scratch/bin/fno-agents"
chmod +x "$scratch/bin/uv" "$scratch/bin/fno-agents"
chmod -R a-w "$scratch/tree"
decision=$(printf '{"cwd":"%s/project","tool_input":{"file_path":"peer.rs"}}' "$scratch" |
    PATH="$scratch/bin:$PATH" HOME="$scratch/home" FNO_WORKER_NAME=ci-one \
    FNO_AGENTS_BIN="$scratch/bin/fno-agents" \
    bash "$scratch/tree/hooks/join-partition-write-guard.sh")
jq -e '.decision == "block"' <<<"$decision"
grep -qF 'peer.rs' "$scratch/judge-input"
[[ ! -e "$scratch/uv-called" ]]
"$scratch/plugin-tree" check "$scratch/tree"

# Both matching and mismatching source versions exercise the shared warning.
printf '#!/bin/sh\nprintf '\''{"git_rev":"%s"}\\n'\''\n' "${sha:0:12}" > "$scratch/bin/fno"
chmod +x "$scratch/bin/fno"
[[ -z "$(PATH="$scratch/bin:$PATH" bash "$scratch/tree/hooks/plugin-source-session-start.sh")" ]]
other=000000000000
[[ "$sha" != "$other"* ]] || other=111111111111
printf '#!/bin/sh\nprintf '\''{"git_rev":"%s"}\\n'\''\n' "$other" > "$scratch/bin/fno"
PATH="$scratch/bin:$PATH" bash "$scratch/tree/hooks/plugin-source-session-start.sh" > "$scratch/warning"
grep -F 'Footnote source mismatch:' "$scratch/warning"

# Seed a legacy full-repository stable tip to prove migration keeps ancestry.
git init --bare "$scratch/remote.git"
git -C "$scratch/remote.git" config receive.denyNonFastForwards true
git clone --no-hardlinks "$repo" "$scratch/publisher"
git -C "$scratch/publisher" remote set-url origin "$scratch/remote.git"
git -C "$scratch/publisher" push origin HEAD:refs/heads/stable
git clone --branch stable "$scratch/remote.git" "$scratch/existing-install"
cd "$scratch/publisher"
legacy=$(git ls-remote origin refs/heads/stable | cut -f1)
bash scripts/release/publish-plugin.sh "$sha" 0.0.0-ci.1 stable
first=$(git ls-remote origin refs/heads/stable | cut -f1)
git merge-base --is-ancestor "$legacy" "$first"
bash scripts/release/publish-plugin.sh "$sha" 0.0.0-ci.2 stable
second=$(git ls-remote origin refs/heads/stable | cut -f1)
git merge-base --is-ancestor "$first" "$second"
[[ "$first" != "$second" ]]
stale=$(printf 'stale competing publisher\n' | git -c user.name=CI -c user.email=ci@example.com commit-tree "${second}^{tree}" -p "$first")
if git push origin "$stale:refs/heads/stable"; then
    echo 'stale competing publication was accepted' >&2; exit 1
fi
[[ "$(git ls-remote origin refs/heads/stable | cut -f1)" == "$second" ]]
git -C "$scratch/existing-install" pull --ff-only
[[ "$(git -C "$scratch/existing-install" rev-parse HEAD)" == "$second" ]]
mkdir "$scratch/published"
git archive "$second" | tar -x -C "$scratch/published"
"$scratch/plugin-tree" check "$scratch/published"
cmp "$scratch/published/SOURCE_COMMIT" "$scratch/tree/SOURCE_COMMIT"
jq -e '.version == "0.0.0-ci.2"' "$scratch/published/.claude-plugin/plugin.json"
jq -e '.version == "0.0.0-ci.2"' "$scratch/published/.codex-plugin/plugin.json"
bash scripts/release/publish-plugin.sh "$sha" 0.0.0-ci.1 plugin-nightly
bash scripts/release/publish-plugin.sh "$sha" 0.0.0-ci.2 plugin-nightly
echo "plugin-tree CI proof: legacy=$legacy first=$first second=$second; existing install pulled --ff-only"
if [[ -n "${GITHUB_STEP_SUMMARY:-}" ]]; then
    printf 'Plugin tree: %s files. Count boundary: 5000 accepted, 5001 refused. Claude validation and Codex install passed. Stable ancestry: `%s` -> `%s` -> `%s`; existing clone pulled with `--ff-only`. Stale publication refused.\n' \
        "$count" "$legacy" "$first" "$second" >> "$GITHUB_STEP_SUMMARY"
fi
