#!/usr/bin/env bash
# The mail module keeps the protocol free of the fleet: no file under
# crates/fno-agents/src/mail/ names a crate:: path outside crate::mail::, and
# none climbs with super::super. A crate wall gave this property; law d-6f5b834b
# bars the crate, so this gate holds the same line.
#
# Usage: check-mail-module-deps.sh [file...]   (default: every .rs under mail/)
# Exit 0 clean, 1 on a refused line, 2 when the positive control fails.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

MODULE_DIR="crates/fno-agents/src/mail"
CANARY="scripts/ci/fixtures/mail-module-deps-canary.rs"

# Print "file:line: text" for each refused line. Comment-only lines describe
# the code and never import anything.
scan() {
    awk -v f="$1" '
        { stripped = $0; sub(/^[ \t]+/, "", stripped) }
        stripped ~ /^\/\// { next }
        { line = $0; gsub(/crate::mail::/, "", line) }
        line ~ /crate::/ || line ~ /super::super/ { printf "%s:%d: %s\n", f, NR, $0 }
    ' "$1"
}

if [ -z "$(scan "$CANARY")" ]; then
    echo "check-mail-module-deps: the canary $CANARY was not refused; the matcher drifted" >&2
    exit 2
fi

if [ "$#" -gt 0 ]; then
    FILES=("$@")
else
    FILES=()
    while IFS= read -r f; do FILES+=("$f"); done < <(find "$MODULE_DIR" -name '*.rs' | sort)
fi

[ "${#FILES[@]}" -gt 0 ] || { echo "check-mail-module-deps: no files under $MODULE_DIR" >&2; exit 2; }

BAD=""
for f in "${FILES[@]}"; do
    BAD="${BAD}$(scan "$f")"
done

if [ -n "$BAD" ]; then
    echo "check-mail-module-deps: the mail module names a path outside crate::mail::" >&2
    printf '%s\n' "$BAD" >&2
    exit 1
fi
echo "check-mail-module-deps: ok (${#FILES[@]} file(s))"
