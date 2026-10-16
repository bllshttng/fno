#!/usr/bin/env bash
# Build and publish only in CI. The generated commit retains the channel tip.
set -euo pipefail
[[ "${GITHUB_ACTIONS:-}" == true ]] || { echo 'plugin publication requires CI' >&2; exit 1; }
sha=$(git rev-parse --verify "${1:?source commit}^{commit}")
version=${2:?plugin version}
branch=${3:?stable or plugin-nightly}
remote=${4:-origin}
case "$branch" in stable|plugin-nightly) ;; *) echo 'invalid plugin branch' >&2; exit 1 ;; esac
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
git archive "$sha" | tar -x -C "$scratch"
rustc --edition=2021 "$scratch/crates/fno-agents/ci/plugin_tree.rs" -o "$scratch/plugin-tree"
"$scratch/plugin-tree" build "$scratch" "$scratch/tree" "$sha" "$version"

ref="refs/heads/$branch"
previous=''
rc=0
advertised=$(git ls-remote --exit-code "$remote" "$ref") || rc=$?
if [[ "$rc" == 0 ]]; then
    git fetch --no-tags "$remote" "$ref"
    previous=$(git rev-parse FETCH_HEAD)
    [[ "$previous" == "${advertised%%[[:space:]]*}" ]] || { echo 'plugin tip moved during fetch; retry CI' >&2; exit 1; }
elif [[ "$rc" != 2 ]]; then
    echo 'cannot read plugin branch; refusing publication' >&2
    exit "$rc"
fi

# A private index constructs the exact tree without touching the checkout.
git_dir=$(git rev-parse --absolute-git-dir)
export GIT_INDEX_FILE="$scratch/index"
stage_git=(git -C "$scratch/tree" --git-dir="$git_dir" --work-tree="$scratch/tree")
"${stage_git[@]}" add -- .
tree=$("${stage_git[@]}" write-tree)
parents=()
[[ -n "$previous" ]] && parents=(-p "$previous")
export GIT_AUTHOR_NAME='github-actions[bot]' GIT_COMMITTER_NAME='github-actions[bot]'
export GIT_AUTHOR_EMAIL='41898282+github-actions[bot]@users.noreply.github.com'
export GIT_COMMITTER_EMAIL="$GIT_AUTHOR_EMAIL"
commit=$(printf 'chore(plugin): %s from %s\n' "$version" "$sha" | git commit-tree "$tree" "${parents[@]}")
if [[ -n "$previous" ]]; then
    git merge-base --is-ancestor "$previous" "$commit"
fi
# Ordinary push is the concurrency gate: a competing child cannot overwrite it.
git push "$remote" "$commit:$ref"
echo "plugin-publish: $branch ${previous:-new} -> $commit source=$sha tree=$tree (fast-forward)"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then
    printf 'plugin_commit=%s\n' "$commit" >> "$GITHUB_OUTPUT"
fi
