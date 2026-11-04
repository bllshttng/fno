#!/usr/bin/env bash
set -euo pipefail
cd "$(git rev-parse --show-toplevel)/cli"

# Every assertion below inspects the INSTALLED wheel via `import fno` in the
# throwaway venv, so an inherited PYTHONPATH pointing at src/ silently redirects
# all of them to the source tree. preflight.sh exports exactly that, which is
# why this file has twice reported a wheel defect that only existed in its own
# environment.
unset PYTHONPATH
[ -z "${PYTHONPATH:-}" ] || { echo "FAIL: PYTHONPATH survived unset (readonly?); refusing to smoke against a contaminated sys.path"; exit 1; }
uv sync --quiet
uv build --quiet
ls dist/*.whl >/dev/null
TMPDIR_INSTALL=$(mktemp -d)
trap "rm -rf $TMPDIR_INSTALL" EXIT
uv venv --quiet "$TMPDIR_INSTALL/venv"
uv pip install --quiet --python "$TMPDIR_INSTALL/venv/bin/python" dist/*.whl
out=$("$TMPDIR_INSTALL/venv/bin/fno-py" --version 2>&1 || true)
# version-agnostic: the binary must report SOME semver, not a pinned one, so a
# release bump never breaks this smoke test (the build wires the version).
echo "$out" | grep -qE '[0-9]+\.[0-9]+\.[0-9]+'
echo "PASS: uv build produces installable wheel"

# ab-18563bcc US5: the wheel must carry LICENSE + NOTICE (Apache-2.0 is declared
# in pyproject.toml, but the texts must ship in the distribution). The build
# hook force-includes them under fno/_licenses/ from the repo root (direct
# build) or the sdist vendor copy (wheel-from-sdist). Regression: a build that
# ships no license text is non-compliant.
PKG_LIC_DIR=$("$TMPDIR_INSTALL/venv/bin/python" -c \
  "import fno, os; print(os.path.join(os.path.dirname(fno.__file__), '_licenses'))")
for f in LICENSE NOTICE; do
  test -f "$PKG_LIC_DIR/$f" || { echo "FAIL: wheel did not ship fno/_licenses/$f"; exit 1; }
done
echo "PASS: wheel ships LICENSE + NOTICE under fno/_licenses/"

# Import + schema-load from an EMPTY cwd with every schema/project env var
# unset - exercises ONLY the in-package fallback, not a dev-tree walk-up.
# The events schema left the wheel with the events package residue port: it
# lives beside its Rust readers in crates/fno-agents/src/event_store/, so
# there is no in-package schema to load here.
