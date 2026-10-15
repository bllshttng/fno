#!/usr/bin/env bash
# Advisory only: a binary that names a commit should match the plugin source.
set -uo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
[[ -f "$root/SOURCE_COMMIT" ]] || exit 0
source_commit=$(cat "$root/SOURCE_COMMIT")
[[ "$source_commit" =~ ^[0-9a-f]{40}$ ]] || exit 0
command -v fno >/dev/null 2>&1 || exit 0
source "$root/scripts/lib/with-timeout.sh" 2>/dev/null || exit 0
version=$(with_timeout 2 fno version --json 2>/dev/null) || exit 0
commit=$(jq -er '.git_rev | select(type == "string")' <<<"$version" 2>/dev/null) || exit 0
[[ "$commit" =~ ^[0-9a-f]{7,40}$ && "$source_commit" != "$commit"* ]] || exit 0
printf 'Footnote source mismatch: plugin %s; fno %s. Run `fno update` to align the runtime and plugin.\n' "$source_commit" "$commit"
