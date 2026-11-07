#!/usr/bin/env bash
# Enforce the company pack boundary: every root agent or skill that carries a
# pack marker must name a pack that ships a plugins/<pack>/plugin.yaml.
set -euo pipefail

ROOT=""

usage() {
  cat <<'EOF'
Usage: check-company-boundaries.sh [ROOT]

Attributes each pack-marked root projection (agents/*.md, skills/*/SKILL.md)
to its source pack and fails on a marker that names no plugins/<pack>/plugin.yaml.
ROOT defaults to the current repository root.
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      echo "check-company-boundaries: unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
    *)
      if [[ -n "$ROOT" ]]; then
        echo "check-company-boundaries: unexpected argument: $1" >&2
        exit 2
      fi
      ROOT="$1"
      shift
      ;;
  esac
done

ROOT="${ROOT:-$(git rev-parse --show-toplevel 2>/dev/null || pwd)}"

# Keep this reader byte-for-byte equivalent to the freshness gate's reader so
# both checks agree on which root files are build-time pack projections.
_pack_marker_value() {
  awk 'NR==1 && /^---[[:space:]]*$/ {f=1; next} f && /^---[[:space:]]*$/ {exit} f && /^pack:/ {sub(/^pack:[[:space:]]*/,""); gsub(/["'\'']/,""); print; exit}' "$1" 2>/dev/null
}

_pack_marker_line() {
  awk 'NR==1 && /^---[[:space:]]*$/ {f=1; next} f && /^---[[:space:]]*$/ {exit} f && /^pack:/ {print NR; exit}' "$1" 2>/dev/null
}

ATTRIBUTED=()
ORPHANS=0
_classify_projection() {
  local path="$1" rel="$2" marker line
  marker="$(_pack_marker_value "$path")"
  [[ -n "$marker" ]] || return 0
  line="$(_pack_marker_line "$path")"
  if [[ -f "$ROOT/plugins/$marker/plugin.yaml" ]]; then
    ATTRIBUTED+=("$rel -> $marker")
    return 0
  fi
  echo "  $rel:$line: pack marker '$marker' names no plugins/$marker/plugin.yaml" >&2
  ORPHANS=1
}

while IFS= read -r path; do
  [[ -f "$path" ]] || continue
  _classify_projection "$path" "agents/$(basename "$path")"
done < <(find "$ROOT/agents" -maxdepth 1 -name '*.md' -type f 2>/dev/null | sort)
while IFS= read -r path; do
  [[ -f "$path/SKILL.md" ]] || continue
  _classify_projection "$path/SKILL.md" "skills/$(basename "$path")"
done < <(find "$ROOT/skills" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)

if [[ ${#ATTRIBUTED[@]} -gt 0 ]]; then
  printf 'check-company-boundaries: projections attributed: '
  (IFS=', '; echo "${ATTRIBUTED[*]}")
fi
if [[ $ORPHANS -ne 0 ]]; then
  echo "check-company-boundaries: orphaned pack projection" >&2
  exit 1
fi
echo "check-company-boundaries: ${#ATTRIBUTED[@]} pack projections attributed, 0 orphans"
