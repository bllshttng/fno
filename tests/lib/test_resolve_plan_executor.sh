#!/usr/bin/env bash
# test_resolve_plan_executor.sh - inline-path executor resolution (Bug 2).
#
# "Done when": a fixture changeset touching components/**/*.tsx resolves the
# impeccable executor on the INLINE path (resolve-plan-executor.sh), and a
# backend-only plan resolves `tdd`. Mirrors /operator's resolution at the
# flat-plan granularity /execute works at.
#
# The Python parity legs died with the worker-family port that deleted the
# leg. The canonical stdout bytes stay: the bash resolver answers them alone,
# and the deleted leg's answers are frozen in the executor_route goldens.

set -uo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
R="$REPO_ROOT/scripts/lib/resolve-plan-executor.sh"

PASS=0; FAIL=0
ck() { local l="$1" exp="$2" act="$3"
    if [[ "$exp" == "$act" ]]; then echo "  PASS: $l"; PASS=$((PASS+1))
    else echo "  FAIL: $l (exp=$exp act=$act)"; FAIL=$((FAIL+1)); fi; }

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# The plan-frontmatter enum is `tdd`; `do` remains accepted for one release.
# Compare files rather than command substitutions so the trailing newline
# stays part of the contract.
printf 'tdd\n' > "$TMP/expected-tdd.out"
for spelling in tdd do; do
    cat > "$TMP/alias-$spelling.md" <<EOF
---
executor: $spelling
---
# Backend task
**Files:** cli/src/fno/loop.py
EOF
    bash "$R" "$TMP/alias-$spelling.md" > "$TMP/bash-$spelling.out"
    if cmp -s "$TMP/bash-$spelling.out" "$TMP/expected-tdd.out"; then
        echo "  PASS: positive marker: executor=$spelling resolves canonical tdd bytes"
        PASS=$((PASS+1))
    else
        echo "  FAIL: executor=$spelling does not resolve canonical tdd bytes"
        od -An -tx1 "$TMP/bash-$spelling.out" "$TMP/expected-tdd.out"
        FAIL=$((FAIL+1))
    fi
done

cat > "$TMP/frontend.md" <<'EOF'
# Add settings panel

### 1.1 Build the panel
**Files:** src/components/SettingsPanel.tsx, src/styles/settings.css
EOF
ck "frontend plan -> impeccable (inline)" impeccable "$(bash "$R" "$TMP/frontend.md")"
printf 'impeccable\n' > "$TMP/expected-impeccable.out"
bash "$R" "$TMP/frontend.md" > "$TMP/bash-frontend.out"
if cmp -s "$TMP/bash-frontend.out" "$TMP/expected-impeccable.out"; then
    echo "  PASS: positive marker: frontend resolves canonical impeccable bytes"
    PASS=$((PASS+1))
else
    echo "  FAIL: frontend does not resolve canonical impeccable bytes"
    od -An -tx1 "$TMP/bash-frontend.out" "$TMP/expected-impeccable.out"
    FAIL=$((FAIL+1))
fi

cat > "$TMP/mixed-frontend.md" <<'EOF'
---
executor: mixed
---
# Mixed plan
**Files:** src/components/Mixed.tsx
EOF
bash "$R" "$TMP/mixed-frontend.md" > "$TMP/bash-mixed-frontend.out"
if cmp -s "$TMP/bash-mixed-frontend.out" "$TMP/expected-tdd.out"; then
    echo "  PASS: positive marker: mixed frontend resolves canonical tdd bytes"
    PASS=$((PASS+1))
else
    echo "  FAIL: mixed frontend does not resolve canonical tdd bytes"
    od -An -tx1 "$TMP/bash-mixed-frontend.out" "$TMP/expected-tdd.out"
    FAIL=$((FAIL+1))
fi

cat > "$TMP/uppercase-files.md" <<'EOF'
# Uppercase heading
**FILES:** components/Uppercase.ts
EOF
bash "$R" "$TMP/uppercase-files.md" > "$TMP/bash-uppercase-files.out"
if cmp -s "$TMP/bash-uppercase-files.out" "$TMP/expected-impeccable.out"; then
    echo "  PASS: positive marker: uppercase FILES resolves canonical impeccable bytes"
    PASS=$((PASS+1))
else
    echo "  FAIL: uppercase FILES does not resolve canonical impeccable bytes"
    od -An -tx1 "$TMP/bash-uppercase-files.out" "$TMP/expected-impeccable.out"
    FAIL=$((FAIL+1))
fi

printf '%s\r\n' '---' 'executor: impeccable' '---' '# CRLF plan' \
    '**Files:** src/components/CrLf.tsx' > "$TMP/crlf.md"
bash "$R" "$TMP/crlf.md" > "$TMP/bash-crlf.out"
if cmp -s "$TMP/bash-crlf.out" "$TMP/expected-impeccable.out"; then
    echo "  PASS: positive marker: CRLF plan resolves canonical impeccable bytes"
    PASS=$((PASS+1))
else
    echo "  FAIL: CRLF plan does not resolve canonical impeccable bytes"
    od -An -tx1 "$TMP/bash-crlf.out" "$TMP/expected-impeccable.out"
    FAIL=$((FAIL+1))
fi

cat > "$TMP/backend.md" <<'EOF'
# Add migration

### 1.1 Migrate schema
**Files:** cli/src/fno/loop.py, migrations/0003.sql
EOF
ck "backend plan -> tdd (inline)" tdd "$(bash "$R" "$TMP/backend.md")"

cat > "$TMP/override.md" <<'EOF'
---
executor: do
---
# Backend job that happens to touch components/
**Files:** src/components/Legacy.tsx
EOF
ck "plan executor: do alias wins over tsx inference" tdd "$(bash "$R" "$TMP/override.md")"

# stdin form
ck "frontend plan via stdin -> impeccable" impeccable "$(bash "$R" < "$TMP/frontend.md")"

# Missing plan path -> exit 2, no stdin hang (Gemini PR #385 MEDIUM).
bash "$R" "$TMP/does-not-exist.md" </dev/null >/dev/null 2>&1; rc=$?
ck "missing plan file -> exit 2" 2 "$rc"

# Case-sensitive executor match: a capital-E 'Executor:' prose line must NOT
# be read as a directive (Gemini PR #385 MEDIUM). Backend files -> inference
# resolves tdd; the old case-insensitive grep would have routed to impeccable.
cat > "$TMP/prose.md" <<'EOF'
# Design note
Executor: impeccable
**Files:** cli/src/fno/loop.py
EOF
ck "capital-E prose 'Executor:' not a directive -> tdd" tdd "$(bash "$R" "$TMP/prose.md")"

# Line-range suffix on a .tsx path must not defeat the *.tsx arm (Codex P2
# on PR #385). app/page.tsx relies on the extension arm, not a dir arm.
cat > "$TMP/ranges.md" <<'EOF'
# App router tweak
**Files:** app/page.tsx (lines 1-5), app/layout.tsx (lines 10-20)
EOF
ck "Files with (lines N-M) ranges -> impeccable" impeccable "$(bash "$R" "$TMP/ranges.md")"

echo ""
echo "test_resolve_plan_executor: ${PASS} passed, ${FAIL} failed"
[[ $FAIL -eq 0 ]]
