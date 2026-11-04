#!/usr/bin/env bash
# Skill-reach lint. The fno skills ship together as one plugin, so /target
# may load any of them by name mid-run. This lint guards that reach.
#
# Three checks:
#   1. Each of target, review, fix and think declares its `fno` binary
#      dependency under `metadata.requires.binaries` in frontmatter. Mirrors
#      how an npm skill declares `node` or a CLI tool declares `gh`.
#   2. skills/target/references/toolbag.md names every other skill dir as
#      `fno:<name>`, so a new skill cannot ship without target knowing it.
#   3. No file under skills/target says target never hands off to a sibling
#      skill. That sentence was the old portability rule.
#
# The ${REPO_ROOT}/scripts ban is a separate lint
# (no-repo-root-scripts-in-skills.sh) and still holds.
#
# Usage:
#   bash scripts/lint/check-skill-reach.sh
#   bash scripts/lint/check-skill-reach.sh --root /tmp/fixture
#
# The --root flag lets tests point the lint at a fixture tree instead
# of the real repo. Default: the git toplevel of the working directory.
set -euo pipefail

# ---------------------------------------------------------------------------
# Arg parsing
# ---------------------------------------------------------------------------
ROOT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --root)
      ROOT="$2"
      shift 2
      ;;
    --root=*)
      ROOT="${1#--root=}"
      shift
      ;;
    -h|--help)
      sed -n '2,/^# Usage/p' "$0" >&2
      exit 0
      ;;
    *)
      echo "ERROR: unknown arg: $1" >&2
      exit 2
      ;;
  esac
done

if [[ -z "$ROOT" ]]; then
  ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
  if [[ -z "$ROOT" ]]; then
    echo "ERROR: not in a git repo and --root not set" >&2
    exit 2
  fi
fi

if [[ ! -d "$ROOT" ]]; then
  echo "ERROR: --root not a directory: $ROOT" >&2
  exit 2
fi

# Helper paths resolve from the lint script's own location so test
# fixtures (which set --root to a tmp dir that doesn't bundle helpers)
# still find the helpers in the real repo.
SCRIPT_DIR_REAL="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT_REPO_ROOT="$(cd "$SCRIPT_DIR_REAL/../.." && pwd)"
FRONTMATTER_CHECKER="$SCRIPT_REPO_ROOT/scripts/lib/check-skill-frontmatter.py"

# PyYAML: prefer the host interpreter, else an ephemeral uv env. Homebrew's
# python3 is PEP 668 externally-managed, so pyyaml is routinely absent there and
# `pip install` refuses; uv is already this repo's standard everywhere else.
# When neither is available, leave PY as plain python3 - the checker's own
# rc=2 "PyYAML not available" path reports it better than a lint-level abort.
if python3 -c 'import yaml' 2>/dev/null || ! command -v uv >/dev/null 2>&1; then
  PY=(python3)
else
  PY=(uv run --no-project --with pyyaml python3)
  # A uv that cannot actually materialize pyyaml (offline, cold cache) would
  # otherwise surface once per skill as "does not declare fno in frontmatter",
  # blaming the author for an environment fault. Fall back so the checker's own
  # rc=2 reports the real cause.
  "${PY[@]}" -c 'import yaml' 2>/dev/null || PY=(python3)
fi

# Skills that must declare the fno binary. Missing dirs are tolerated (lint
# just skips them) so tests can fixture only the skills they care about.
FNO_DECLARING_SKILLS=(target review fix think)
EXIT=0

for skill in "${FNO_DECLARING_SKILLS[@]}"; do
  SKILL_DIR="$ROOT/skills/$skill"
  if [[ ! -d "$SKILL_DIR" ]]; then
    continue
  fi

  # -------------------------------------------------------------------
  # Check 1: metadata.requires.binaries.fno declared in SKILL.md frontmatter.
  # -------------------------------------------------------------------
  SKILL_FILE="$SKILL_DIR/SKILL.md"
  if [[ ! -f "$SKILL_FILE" ]]; then
    echo "ERROR: skills/$skill/SKILL.md missing." >&2
    EXIT=1
    continue
  fi
  # Frontmatter validation lives in scripts/lib/check-skill-frontmatter.py
  # so it can be tested in isolation and reused. The helper splits failure
  # modes: rc=2 for PyYAML-missing / file-missing / malformed-frontmatter
  # (substrate problems) and rc=1 for "binary not declared" (authoring
  # problem). The `if ...; then:; else rc=$?; fi` pattern is tested
  # context, so `set -e` does NOT abort when the helper exits non-zero.
  rc=0
  if "${PY[@]}" "$FRONTMATTER_CHECKER" "$SKILL_FILE" --require fno; then
    :  # frontmatter OK and declares fno
  else
    rc=$?
  fi
  if [[ $rc -eq 2 ]]; then
    # PyYAML missing: the python helper already printed the install hint
    # to stderr. Emit a context-bearing follow-up so the user knows which
    # SKILL.md the lint was inspecting when the dep was missing.
    echo "  (while checking skills/$skill/SKILL.md frontmatter)" >&2
    echo "" >&2
    EXIT=1
  elif [[ $rc -ne 0 ]]; then
    echo "ERROR: skills/$skill/SKILL.md does not declare 'fno' under metadata.requires.binaries in frontmatter." >&2
    echo "  Add a requires: block to the frontmatter." >&2
    echo "" >&2
    EXIT=1
  fi
done

TARGET_DIR="$ROOT/skills/target"
if [[ -d "$TARGET_DIR" ]]; then
  # -------------------------------------------------------------------
  # Check 2: the toolbag names every other skill.
  # -------------------------------------------------------------------
  TOOLBAG="$TARGET_DIR/references/toolbag.md"
  if [[ ! -f "$TOOLBAG" ]]; then
    echo "ERROR: skills/target/references/toolbag.md missing." >&2
    EXIT=1
  else
    TOTAL=0
    NAMED=0
    for skill_md in "$ROOT"/skills/*/SKILL.md; do
      [[ -f "$skill_md" ]] || continue
      name="$(basename "$(dirname "$skill_md")")"
      [[ "$name" == "target" ]] && continue
      TOTAL=$((TOTAL + 1))
      # The boundary keeps fno:ship from matching a row for fno:ship-docs.
      if grep -qE "fno:${name}([^a-z0-9-]|\$)" "$TOOLBAG"; then
        NAMED=$((NAMED + 1))
      else
        echo "ERROR: skills/target/references/toolbag.md does not name fno:$name." >&2
        EXIT=1
      fi
    done
    if [[ $NAMED -eq $TOTAL ]]; then
      echo "toolbag names $NAMED of $TOTAL skills"
    fi
  fi

  # -------------------------------------------------------------------
  # Check 3: target never claims it cannot hand off to a sibling skill.
  # -------------------------------------------------------------------
  HITS=$(grep -rn 'never hands off' "$TARGET_DIR" --include='*.md' 2>/dev/null || true)
  if [[ -n "$HITS" ]]; then
    echo "ERROR: skills/target says target never hands off to a sibling skill." >&2
    echo "  The fno skills ship together. Name the skill in toolbag.md and reach for it." >&2
    echo "$HITS" >&2
    EXIT=1
  fi
fi

exit $EXIT
