#!/usr/bin/env bash
# CI twin of the merge gate's visual-approval hold. Reads
# merge.visual_paint_paths from the checkout's .fno/config.toml, the one
# config tier a runner can see, and answers whether the PR's changed files
# touch a listed path. The matcher mirrors the gate's path_matches_paint
# (crates/fno-agents/src/merge_gates.rs): `dir/**` matches at a path boundary,
# a pattern with no slash matches a basename, anything else matches exactly.
# An empty, missing, or unparseable list disarms, like the gate. An unreadable
# changed-file list refuses: a silent skip would hide exactly the artifact a
# paint PR needs.
#
# Emits `paint=true|false` into $GITHUB_OUTPUT. A gate armed outside the repo
# (the canonical or global tier) never reaches this read, so the workflow's
# paint_binary dispatch input is the explicit lever for that case.
set -euo pipefail

: "${PR_NUMBER:?PR_NUMBER is required (github.event.pull_request.number)}"
: "${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}"

# Anchor to this checkout, never the caller's cwd: uv resolves --project
# relative to it, and the job's cwd is the checkout root anyway.
root="$(cd "$(dirname "$0")/../.." && pwd)"
# Injectable so a fixture tree can drive the read; defaults to the checkout
# tier, the only one a runner can see.
config="${PAINT_CONFIG_PATH:-$root/.fno/config.toml}"
patterns_file="$(mktemp)"
files_file="$(mktemp)"
gh_err="$(mktemp)"
trap 'rm -f "$patterns_file" "$files_file" "$gh_err"' EXIT

if [[ -f "$config" ]]; then
  # Through the cli project env, not bare python3: tomllib needs 3.11+, the
  # cli package pins that floor, and runners and workstations both already
  # carry uv for it. A config the gate cannot parse disarms it silently, so a
  # parse error here prints nothing and exits clean rather than failing the
  # selection.
  uv run --project "$root/cli" python - "$config" > "$patterns_file" <<'PY'
import sys, tomllib

with open(sys.argv[1], "rb") as f:
    try:
        table = tomllib.load(f)
    except tomllib.TOMLDecodeError:
        sys.exit(0)
merge = table.get("merge")
value = merge.get("visual_paint_paths") if isinstance(merge, dict) else None
if isinstance(value, str):
    value = [value]
if not isinstance(value, list):
    value = []
for item in value:
    if isinstance(item, str) and item.strip():
        print(item.strip())
PY
fi

if [[ -s "$patterns_file" ]]; then
  if ! gh api --paginate \
      "repos/$GITHUB_REPOSITORY/pulls/$PR_NUMBER/files?per_page=100" \
      --jq '.[].filename' > "$files_file" 2> "$gh_err"; then
    cat "$gh_err" >&2
    echo "the changed-file list was unreadable; refusing to answer" >&2
    exit 1
  fi
  matched="$(python3 - "$patterns_file" "$files_file" <<'PY'
import sys

with open(sys.argv[1]) as f:
    patterns = [line.strip() for line in f if line.strip()]
with open(sys.argv[2]) as f:
    files = [line.rstrip("\n") for line in f if line.strip()]


def matches(path: str, pattern: str) -> bool:
    if pattern.endswith("/**"):
        prefix = pattern[: -len("/**")].rstrip("/")
        return path == prefix or path.startswith(prefix + "/")
    if "/" not in pattern:
        return path.rsplit("/", 1)[-1] == pattern
    return path == pattern


print("true" if any(matches(p, pat) for p in files for pat in patterns) else "false")
PY
)"
else
  matched="false"
fi

echo "paint=$matched" | tee -a "${GITHUB_OUTPUT:-/dev/null}"
if [[ "$matched" != "true" ]]; then
  echo "no configured paint path touched; the paint-binary job stays off"
fi
