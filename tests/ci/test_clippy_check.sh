#!/usr/bin/env bash
# Positive control for scripts/ci/clippy-check.sh (the rust-ci clippy gate).
#
# The gate parses clippy's JSON stream into baseline keys and holds them
# against tests/ci/clippy-baseline.txt. A gate nobody has watched fail is
# not known to work, so this asserts, without a cargo build:
#   - the parser emits the exact key per clippy lint, and filters rustc's
#     own diagnostics and macro-expanded foreign paths,
#   - a baselined hit passes,
#   - the same lint on an EDITED line (different text) fails as new,
#   - --write-baseline round-trips through a re-check.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
GATE="$REPO_ROOT/scripts/ci/clippy-check.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

fail() { echo "test_clippy_check: $1" >&2; exit 1; }

jsonl="$TMP/clippy.jsonl"
cat >"$jsonl" <<'EOF'
{"reason":"compiler-message","message":{"code":{"code":"clippy::indexing_slicing"},"spans":[{"file_name":"src/spawn_seam.rs","line_start":92,"is_primary":true,"text":[{"text":"    let typed = flag_value(&args[1..], &[\"--name\"]).is_some();"}]}]}}
{"reason":"compiler-message","message":{"code":{"code":"clippy::indexing_slicing"},"spans":[{"file_name":"src/spawn_seam.rs","line_start":92,"is_primary":true,"text":[{"text":"    let typed = flag_value(&args[1..len], &[\"--name\"]).is_some();"}]}]}}
{"reason":"compiler-message","message":{"code":{"code":"clippy::disallowed_methods"},"spans":[{"file_name":"src/plugin_install.rs","line_start":303,"is_primary":true,"text":[{"text":"            .output()"}]}]}}
{"reason":"compiler-message","message":{"code":{"code":"unused_variables"},"spans":[{"file_name":"src/other.rs","line_start":1,"is_primary":true,"text":[{"text":"    let unused = 1;"}]}]}}
{"reason":"compiler-message","message":{"code":{"code":"clippy::correctness"},"spans":[{"file_name":"/rustc/deadbeef/library/core/src/num.rs","line_start":1,"is_primary":true,"text":[{"text":"panic!()"}]}]}}
{"reason":"compiler-artifact","target":{"name":"fno-agents"}}
EOF

CRATE=crates/fno-agents
key_slice="$CRATE/src/spawn_seam.rs	clippy::indexing_slicing	let typed = flag_value(&args[1..], &[\"--name\"]).is_some();"
key_slice_edited="$CRATE/src/spawn_seam.rs	clippy::indexing_slicing	let typed = flag_value(&args[1..len], &[\"--name\"]).is_some();"
key_output="$CRATE/src/plugin_install.rs	clippy::disallowed_methods	.output()"

# 1. Parser: exact keys for the clippy lints, two filters applied.
# No `grep -q` on a pipe: under pipefail, grep's early exit on a match can
# SIGPIPE the producer and flip a found match into a false miss.
keys="$("$GATE" --extract "$jsonl" "$CRATE")"
[[ "$(printf '%s\n' "$keys" | grep -c .)" -eq 3 ]] || fail "expected 3 keys, got: $keys"
printf '%s\n' "$keys" | grep -Fx "$key_slice" >/dev/null || fail "missing baselined slice key"
printf '%s\n' "$keys" | grep -Fx "$key_slice_edited" >/dev/null || fail "missing edited-line key"
printf '%s\n' "$keys" | grep -Fx "$key_output" >/dev/null || fail "missing disallowed_methods key"
if printf '%s\n' "$keys" | grep -F "unused_variables" >/dev/null; then fail "rustc diagnostic leaked into clippy keys"; fi
if printf '%s\n' "$keys" | grep -F "/rustc/" >/dev/null; then fail "foreign macro path leaked into clippy keys"; fi

# 2. A baselined hit passes.
printf '# header\n%s\n' "$key_slice" >"$TMP/baseline.tsv"
printf '%s\n' "$key_slice" >"$TMP/hits.tsv"
"$GATE" --hits-file "$TMP/hits.tsv" --baseline "$TMP/baseline.tsv" "$CRATE" >/dev/null 2>&1 \
    || fail "baselined hit should pass"

# 3. The same lint on an edited line is a NEW hit and fails, naming the key.
printf '%s\n%s\n' "$key_slice" "$key_slice_edited" >"$TMP/hits_new.tsv"
out="$("$GATE" --hits-file "$TMP/hits_new.tsv" --baseline "$TMP/baseline.tsv" "$CRATE" 2>&1)" \
    && fail "edited hit should fail"
printf '%s\n' "$out" | grep -F "FAILED" >/dev/null || fail "refusal does not say FAILED"
printf '%s\n' "$out" | grep -Fx "  $key_slice_edited" >/dev/null || fail "refusal does not name the edited key"

# 4. --write-baseline round-trips through a re-check.
"$GATE" --hits-file "$TMP/hits_new.tsv" --baseline "$TMP/baseline.tsv" --write-baseline "$CRATE" >/dev/null
"$GATE" --hits-file "$TMP/hits_new.tsv" --baseline "$TMP/baseline.tsv" "$CRATE" >/dev/null \
    || fail "write-baseline round-trip should pass"

# 4b. A stale baseline entry (a hit that no longer fires) never fails the
# gate; it is reported as a ratchet-down, not a violation.
printf '# header\n%s\n%s\n' "$key_slice" "$key_output" >"$TMP/baseline_stale.tsv"
out="$("$GATE" --hits-file "$TMP/hits.tsv" --baseline "$TMP/baseline_stale.tsv" "$CRATE" 2>&1)" \
    || fail "stale baseline entry must not fail the gate"
printf '%s\n' "$out" | grep -F "stale baseline entries" >/dev/null || fail "stale entry not reported as ratchet-down"

# 5. A missing baseline refuses with the remedy instead of passing.
rm -f "$TMP/baseline.tsv"
"$GATE" --hits-file "$TMP/hits_new.tsv" --baseline "$TMP/baseline.tsv" "$CRATE" >/dev/null 2>&1 \
    && fail "missing baseline should refuse"
[[ -f "$TMP/baseline.tsv" ]] && fail "missing baseline was not reported"

echo "test_clippy_check: all controls held"
