#!/usr/bin/env bash
# test_frontend_surface.sh - locked surface matcher via the executor-route door.
#
# The locked surface-inference list lives in the native executor-route door
# (crates/fno-agents/src/executor_route.rs), the SINGLE source of truth since
# the worker-family port deleted the Python leg. This test exercises the
# door's CLI through the same locked-pattern fixtures the retired
# infer-task-executor.sh bash covered, so has_ui inference and executor
# routing keep ONE copy of the patterns. Byte-for-byte parity with the
# deleted Python leg is frozen in the executor_route goldens and asserted by
# crates/fno-agents/tests/executor_surface_parity.rs.

set -uo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
if [[ -f "$REPO_ROOT/hooks/lib/agents-bin.sh" ]]; then
    # shellcheck source=/dev/null
    source "$REPO_ROOT/hooks/lib/agents-bin.sh"
else
    fno_agents_bin() {
        local root="${1:-.}"
        if [[ -n "${FNO_AGENTS_BIN:-}" ]] && [[ -x "${FNO_AGENTS_BIN}" ]]; then
            printf '%s' "${FNO_AGENTS_BIN}"
        elif [[ -x "$root/crates/fno-agents/target/release/fno-agents" ]]; then
            printf '%s' "$root/crates/fno-agents/target/release/fno-agents"
        elif [[ -x "$root/crates/fno-agents/target/debug/fno-agents" ]]; then
            printf '%s' "$root/crates/fno-agents/target/debug/fno-agents"
        else
            command -v fno-agents || printf ''
        fi
    }
fi
BIN="$(fno_agents_bin "$REPO_ROOT")"
if [[ -z "$BIN" ]]; then
    echo "test_frontend_surface: SKIP - fno-agents binary not built (FNO_AGENTS_BIN or cargo build crates/fno-agents)"
    exit 0
fi

PASS=0; FAIL=0
ck() { local l="$1" exp="$2" act="$3"
    if [[ "$exp" == "$act" ]]; then echo "  PASS: $l"; PASS=$((PASS+1))
    else echo "  FAIL: $l (exp=$exp act=$act)"; FAIL=$((FAIL+1)); fi; }
# m: single path -> impeccable|do.  a: stdin list -> impeccable|do.
m() { printf '%s\n' "$1" | "$BIN" executor-route surface; }
a() { "$BIN" executor-route surface; }

echo "is_frontend_surface_path (locked list)"
ck "tsx -> frontend"                      impeccable "$(m 'src/components/Foo.tsx')"
ck "jsx -> frontend"                      impeccable "$(m 'src/Widget.jsx')"
ck "app/page.tsx -> frontend (tsx arm)"   impeccable "$(m 'app/page.tsx')"
ck "components/*.ts -> frontend"          impeccable "$(m 'src/components/Bar.ts')"
ck "routes/ -> frontend"                  impeccable "$(m 'routes/api.ts')"
ck "nested routes/ -> frontend"           impeccable "$(m 'pkg/routes/x.go')"
ck "src/styles -> frontend"               impeccable "$(m 'src/styles/main.css')"
ck "app/main.py -> NOT frontend"          do  "$(m 'app/main.py')"
ck "py -> NOT frontend"                   do  "$(m 'cli/src/loop.py')"
ck "md -> NOT frontend"                   do  "$(m 'docs/readme.md')"

echo ""
echo "any_frontend_surface (stdin)"
ck "mixed list w/ tsx -> match"  impeccable "$(printf '%s\n' a.py b.py c.tsx | a)"
ck "backend-only list -> none"   do  "$(printf '%s\n' a.py b.go | a)"
ck "empty stdin -> none"         do  "$(printf '' | a)"
ck "no trailing newline -> match" impeccable "$(printf '%s' x.tsx | a)"

echo ""
echo "test_frontend_surface: ${PASS} passed, ${FAIL} failed"
[[ $FAIL -eq 0 ]]
