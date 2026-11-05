#!/usr/bin/env bash
# Print the sibling repos of one repo, read from the workspace config.
#
# Usage: fno config get work.workspaces | sibling-roots.sh <repo-root>
#
# stdin is the JSON object `fno config get work.workspaces` prints. A sibling
# is any other project listed in a workspace that also lists <repo-root>.
# Output is one `name<TAB>path` row per sibling whose path exists on disk,
# sorted by name. A linked worktree resolves to its canonical checkout, so a
# review inside a worktree still finds the workspace that names the repo.
#
# Exit 0 with rows. Exit 3 when no workspace lists the repo: the caller must
# say so in the report, because an empty sibling list is not a measured zero.
# Exit 2 on unreadable input.
set -uo pipefail

if [[ $# -ne 1 ]]; then
    echo "usage: fno config get work.workspaces | sibling-roots.sh <repo-root>" >&2
    exit 2
fi
input="$(cat)"
if ! printf '%s' "${input:-null}" | jq empty 2>/dev/null; then
    echo "sibling-roots: stdin is not JSON" >&2
    exit 2
fi
if ! printf '%s' "${input:-null}" | jq -e 'type == "object"' >/dev/null 2>&1; then
    echo "sibling-roots: stdin is not a workspaces object" >&2
    exit 2
fi

# The physical path of an existing directory, ~ expanded; empty otherwise.
physical() {
    local p="$1"
    [[ "$p" == "~"* ]] && p="$HOME${p#\~}"
    (cd "$p" 2>/dev/null && pwd -P)
}

root="$(physical "$1")"
common="$(git -C "$1" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"
if [[ -n "$common" && "${common##*/}" == ".git" ]]; then
    root="$(physical "${common%/.git}")"
fi
[[ -n "$root" ]] || root="$1"

# One `workspace<TAB>name<TAB>path` row per listed project with a path.
rows="$(printf '%s' "$input" | jq -r 'to_entries[] | .key as $ws
    | ((.value // {}).projects // [])[] | select(.path != null and .path != "")
    | [$ws, (.name // ""), .path] | @tsv')"

listing=""
while IFS=$'\t' read -r ws _ path; do
    [[ -n "$ws" && "$(physical "$path")" == "$root" ]] && listing="$listing$ws"$'\n'
done <<< "$rows"
if [[ -z "$listing" ]]; then
    echo "sibling-roots: no workspace lists $root" >&2
    exit 3
fi

while IFS=$'\t' read -r ws name path; do
    grep -qxF -- "$ws" <<< "$listing" || continue
    real="$(physical "$path")"
    [[ -n "$real" && "$real" != "$root" ]] || continue
    printf '%s\t%s\n' "${name:-${real##*/}}" "$real"
done <<< "$rows" | awk -F'\t' '!seen[$1]++' | sort -t$'\t' -k1,1
exit 0
