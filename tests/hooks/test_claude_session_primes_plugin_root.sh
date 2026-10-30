#!/usr/bin/env bash
# tests/hooks/test_claude_session_primes_plugin_root.sh
#
# x-8e27. The machine plugin-root pointer (~/.fno/install/plugin-root) is the
# only env-less source for `fno do target init`'s helper resolution, and only
# the codex/gemini wrapper (session-start.sh) primed it. A claude worker's
# init then resolved whatever copy codex last installed - measured live
# 2026-10-30 as a pre-4168 init copy refusing a worker's own spawn handover.
# The fix primes the pointer from session-start-using-fno.sh too, through the
# shared hooks/helpers/prime-plugin-root.sh, BEFORE the hook's early noop
# exits (the hook noops whenever the join preamble is not due, which is most
# session starts).
#
# Scenarios:
#   (a) a stale pointer is re-pointed by a claude session start (integration)
#   (b) a worktree root (.git FILE) never primes (the x-dc87 guard)
#   (c) an equal pointer is byte-identical after the run (idempotence)
#   (d) a canonical-checkout root (.git DIRECTORY) primes
#   (e) the prime runs even when the hook noops early (no session id)
#
# Run: bash tests/hooks/test_claude_session_primes_plugin_root.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT_REAL="$(cd "${SCRIPT_DIR}/../.." && pwd)"
HOOK="${REPO_ROOT_REAL}/hooks/session-start-using-fno.sh"
HELPER="${REPO_ROOT_REAL}/hooks/helpers/prime-plugin-root.sh"

log()  { printf '[prime-ptr] %s\n' "$*"; }
fail() { printf '[prime-ptr] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[prime-ptr] PASS: %s\n' "$*"; }

[[ -f "$HOOK" ]] || fail "hook not found at $HOOK"
[[ -f "$HELPER" ]] || fail "helper not found at $HELPER"

WORK="$(mktemp -d -t prime-ptr-XXXXXX)"
trap 'rm -rf "$WORK"' EXIT

# A fake fno-agents: the join report answers not-due, so the hook noops after
# the prime - which is exactly the production path on most session starts.
FAKEBIN="$WORK/bin"
mkdir -p "$FAKEBIN"
cat > "$FAKEBIN/fno-agents" <<'FAKE'
#!/usr/bin/env bash
echo "not-due"
FAKE
chmod +x "$FAKEBIN/fno-agents"

# A minimal plugin root: the marker the priming guard requires, plus the
# SKILL.md the hook reads on its way to the join report.
make_plugin_root() {
    local root="$1" git_kind="$2"
    mkdir -p "$root/.claude-plugin" "$root/skills/using-fno"
    printf '{"name":"fake-fno"}\n' > "$root/.claude-plugin/plugin.json"
    printf 'fake skill body\n' > "$root/skills/using-fno/SKILL.md"
    case "$git_kind" in
        worktree)  printf 'gitdir: /else/where\n' > "$root/.git" ;;
        canonical) mkdir -p "$root/.git" ;;
        stage)     : ;;
    esac
}

run_claude_start() {
    # $1 = plugin root, $2 = payload ("" for none)
    local root="$1" payload="$2"
    (
        export PATH="$FAKEBIN:$PATH"
        export CLAUDE_PLUGIN_ROOT="$root"
        export FNO_HOME="$WORK/home"
        unset CODEX_PLUGIN_ROOT FNO_HOOK_HARNESS FNO_PLATFORM 2>/dev/null
        if [[ -n "$payload" ]]; then
            printf '%s' "$payload" | bash "$HOOK" SessionStart
        else
            bash "$HOOK" SessionStart < /dev/null
        fi
    )
}

ptr() { printf '%s/install/plugin-root' "$WORK/home"; }

# ── (a) stale pointer is re-pointed by a claude session start ────────────
ROOT_A="$WORK/stage-a"
make_plugin_root "$ROOT_A" stage
mkdir -p "$WORK/home/install"
printf '%s\n' "/Users/bb16/.codex/plugins/cache/footnote/fno/0.4.3" > "$(ptr)"
OUT_A="$(run_claude_start "$ROOT_A" '{"session_id":"test-sess-a"}')" || fail "(a) hook exited nonzero: $OUT_A"
[[ "$(cat "$(ptr)")" == "$ROOT_A" ]] || fail "(a) pointer reads '$(cat "$(ptr)")', wanted $ROOT_A"
[[ "$OUT_A" == "{"* ]] || fail "(a) hook stdout is not a JSON object: $OUT_A"
pass "(a) stale pointer re-pointed by the claude session start"

# ── (b) a worktree root never primes ─────────────────────────────────────
ROOT_B="$WORK/wt-b"
make_plugin_root "$ROOT_B" worktree
printf '%s\n' "$ROOT_A" > "$(ptr)"
run_claude_start "$ROOT_B" '{"session_id":"test-sess-b"}' > /dev/null || fail "(b) hook exited nonzero"
[[ "$(cat "$(ptr)")" == "$ROOT_A" ]] || fail "(b) worktree root repointed the pointer to '$(cat "$(ptr)")'"
pass "(b) worktree root leaves the pointer alone"

# ── (c) an equal pointer is untouched ────────────────────────────────────
ROOT_C="$WORK/stage-c"
make_plugin_root "$ROOT_C" stage
printf '%s\n' "$ROOT_C" > "$(ptr)"
BEFORE_C="$(cat "$(ptr)")
"
run_claude_start "$ROOT_C" '{"session_id":"test-sess-c"}' > /dev/null || fail "(c) hook exited nonzero"
[[ "$(cat "$(ptr)")
" == "$BEFORE_C" ]] || fail "(c) equal pointer was rewritten"
pass "(c) equal pointer is a no-op"

# ── (d) a canonical-checkout root primes ─────────────────────────────────
ROOT_D="$WORK/canon-d"
make_plugin_root "$ROOT_D" canonical
printf '%s\n' "$ROOT_A" > "$(ptr)"
run_claude_start "$ROOT_D" '{"session_id":"test-sess-d"}' > /dev/null || fail "(d) hook exited nonzero"
[[ "$(cat "$(ptr)")" == "$ROOT_D" ]] || fail "(d) canonical root did not prime; pointer reads '$(cat "$(ptr)")'"
pass "(d) canonical checkout root primes"

# ── (e) the prime runs before the hook's early noop exits ────────────────
# No session id: the hook noops at the session-id gate, after the prime.
ROOT_E="$WORK/stage-e"
make_plugin_root "$ROOT_E" stage
printf '%s\n' "/some/stale/copy" > "$(ptr)"
OUT_E="$(run_claude_start "$ROOT_E" '')" || fail "(e) hook exited nonzero: $OUT_E"
[[ "$OUT_E" == "{}" ]] || fail "(e) wanted the {} noop, got: $OUT_E"
[[ "$(cat "$(ptr)")" == "$ROOT_E" ]] || fail "(e) prime did not run before the noop; pointer reads '$(cat "$(ptr)")'"
pass "(e) prime runs ahead of the early noop"

# ── direct unit check: the helper sources cleanly under set -u ───────────
( set -u; source "$HELPER"; PLUGIN_ROOT="$ROOT_A"; FNO_HOME="$WORK/home2"; prime_plugin_root_pointer; [[ "$(cat "$WORK/home2/install/plugin-root")" == "$ROOT_A" ]] ) \
    || fail "direct helper source+call failed"
pass "helper sources and primes standalone"

log "all scenarios passed"
exit 0
