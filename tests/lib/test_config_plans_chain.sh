#!/usr/bin/env bash
# test_config_plans_chain.sh - the plans-dir keys answer from the Rust
# plans_path chain, never from the bash config tiers.
#
# scripts/lib/config.sh routes plans.focused_path / plans.full_path through
# `fno do plan path` (the chain's front door) and answers the default when fno
# is absent or the probe fails. These cases pin both legs: the delegation, and
# the no-second-opinion degrade (a planted plans.focused_path in config.toml
# must NOT answer when the chain is unresolvable).
#
# CRITICAL test discipline (same as test_config_global_precedence.sh): every
# case sources config.sh FRESH in a subshell with a controlled environment, so
# ambient GLOBAL_SETTINGS/CONFIG_FILE cannot leak in.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
CONFIG_SH="$REPO_ROOT/scripts/lib/config.sh"

PASS=0
FAIL=0
pass() { echo "  PASS: $1"; PASS=$((PASS + 1)); }
fail() { echo "  FAIL: $1"; FAIL=$((FAIL + 1)); }
assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [[ "$actual" == "$expected" ]]; then
        pass "$desc"
    else
        fail "$desc (expected='$expected', got='$actual')"
    fi
}

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# A PATH with the coreutils the library needs and no fno on it. bash must be
# on it too: `env PATH=... bash -c` resolves bash from the NEW PATH, and the
# stub's `#!/usr/bin/env bash` shebang searches it as well.
BIN="$TMP/bin"
mkdir -p "$BIN"
for tool in bash dirname tail; do
    ln -s "$(command -v "$tool")" "$BIN/$tool"
done

# A stub fno that answers only the plans-dir probe, with the chain's front
# door spelling. Anything else (the header's shell-stub ping) stays silent.
STUB_BIN="$TMP/fno-bin"
mkdir -p "$STUB_BIN"
printf '#!/usr/bin/env bash\nif [[ "${1:-} ${2:-} ${3:-}" == "do plan path" ]]; then\n  echo "/plans-fake/plans/20260101-_config_plans_probe.md"\nfi\n' > "$STUB_BIN/fno"
chmod +x "$STUB_BIN/fno"

# Source config.sh fresh with the pinned environment; echo the answer.
ask() {
    local path="$1" key="$2" default="${3-}"
    env -u CONFIG_FILE -u FNO_GLOBAL_SETTINGS_PATH -u GLOBAL_SETTINGS -u LOCAL_SETTINGS \
        PATH="$path" \
        LOCAL_SETTINGS="$TMP/local.toml" \
        GLOBAL_SETTINGS="$TMP/global.toml" \
        LEGACY_CONFIG="$TMP/legacy.yaml" \
        CFG_TEST_CONFIG_SH="$CONFIG_SH" \
        CFG_TEST_KEY="$key" \
        CFG_TEST_DEFAULT="$default" \
        bash -c 'source "$CFG_TEST_CONFIG_SH"; get_config "$CFG_TEST_KEY" "$CFG_TEST_DEFAULT"'
}

echo "test_control_subshell_runs"
# A non-plans key through the tiers proves the subshell bash actually ran, so
# an empty answer below can never pass vacuously.
out=$(ask "$BIN" max_iterations CTRL)
assert_eq "subshell bash runs and tiers answer the default" "CTRL" "$out"

echo "test_chain_resolves_both_plans_keys"
printf 'plans.focused_path = "/planted/local"\n' > "$TMP/local.toml"
out=$(ask "$STUB_BIN:$BIN" plans.focused_path FALLBACK)
assert_eq "focused_path answers the chain dir" "/plans-fake/plans" "$out"
out=$(ask "$STUB_BIN:$BIN" plans.full_path FALLBACK)
assert_eq "full_path answers the same chain dir" "/plans-fake/plans" "$out"

echo "test_chain_unresolvable_never_reads_tiers"
out=$(ask "$BIN" plans.focused_path FALLBACK)
assert_eq "planted tier value does not answer; default does" "FALLBACK" "$out"

echo "test_no_default_arg_degrades_empty"
out=$(ask "$BIN" plans.focused_path)
assert_eq "missing default degrades to empty" "" "$out"

echo ""
echo "Passed: $PASS  Failed: $FAIL"
[[ "$FAIL" -eq 0 ]]
