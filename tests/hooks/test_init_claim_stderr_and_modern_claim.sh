#!/usr/bin/env bash
# test_init_claim_stderr_and_modern_claim.sh
#
# init-target-state.sh: let the modern `fno agents claim` be the authority for
# graph_node_id.
#
# Covers:
#   (a) defect 2: the modern `fno agents claim acquire` wins => graph_node_id is the
#       node id (NOT null), written exactly once. (The old legacy-claim stderr
#       capture into .fno/.init-claim.log was removed when the graph lock stamp
#       moved off the ambient python3 path; .init-claim.log is now a transient
#       stamp-failure log that is removed on success, so it is no longer
#       asserted here.)
#   (b) Boundary + AC3-FR: graph.json missing entirely => fail-closed. No
#       readable graph means no resolvable node means no claim, so graph_node_id
#       is null (written once) AND no phantom node:ab-<id> lockfile is acquired.
#       (x-8e98: the guard no longer has an ungrep'd ab- arm; an ab-id resolves
#       only when it is a real graph entry, matching _resolve_plan_for_blast.)
#   (c) AC1-HP: an ab-id absent from an existing graph => graph_node_id null AND
#       no phantom claim (the codex P2 the ab- arm removal closes).
#
# Exit codes: 0 pass / 1 assertion failed / 77 skipped (missing deps)

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
INIT="${REPO_ROOT}/hooks/helpers/init-target-state.sh"

log()  { printf '[init-claim] %s\n' "$*"; }
fail() { printf '[init-claim] FAIL: %s\n' "$*" >&2; exit 1; }
pass() { printf '[init-claim] PASS: %s\n' "$*"; }
skip() { printf '[init-claim] SKIP: %s\n' "$*" >&2; exit 77; }

command -v git     &>/dev/null || skip "git not on PATH"
command -v python3 &>/dev/null || skip "python3 not on PATH"
command -v fno     &>/dev/null || skip "fno not on PATH (modern claim required)"
[[ -f "$INIT" ]]   || fail "init script not found at $INIT"

bash -n "$INIT" || fail "bash -n rejected $INIT (syntax error)"
pass "init script passes bash -n"

_ALL_TMPS=()
trap 'rm -rf "${_ALL_TMPS[@]}"' EXIT

# An isolated temp repo. Crucially it has NO scripts/roadmap-tasks.py, so the
# legacy `python3 .../scripts/roadmap-tasks.py update` fails exactly like the
# transient flock contention it stands in for -- letting us assert the modern
# claim is the authority.
make_repo() {
  local _varname="$1" _dir
  _dir="$(mktemp -d -t init-claim.XXXXXX)" || fail "mktemp failed"
  eval "${_varname}=\"\${_dir}\""
  (cd "$_dir" && git init -q && mkdir -p .fno home/.fno) || fail "repo setup failed in $_dir"
  printf '# isolated\n' > "${_dir}/.fno/config.toml"
  printf '# isolated global\n' > "${_dir}/home/.fno/config.toml"
  # State-path stub: pins init's manifest location to the scenario space dir.
  mkdir -p "${_dir}/bin" "${_dir}/space"
  cp "${REPO_ROOT}/tests/helpers/fno-agents-state-path-stub.sh" "${_dir}/bin/fno-agents"
  chmod 755 "${_dir}/bin/fno-agents"
}

graph_node_id_of() {  # $1 = state file
  grep '^graph_node_id:' "$1" | sed 's/^graph_node_id:[[:space:]]*//' | tr -d '\r'
}

# Node claims land in the GLOBAL root's claim table ($HOME/.fno graph.db), not
# the project. Any state but "free" means a phantom claim was acquired. An
# unreadable table fails the test: a read error must never pass as "free".
node_claim_exists() {  # $1 = HOME dir, $2 = node id
  local state
  state="$(FNO_CLAIMS_ROOT="$1" uv run --project "$REPO_ROOT/cli" python -c '
import sys
from pathlib import Path
from fno.claims.core import claim_status
print(claim_status("node:" + sys.argv[2], root=Path(sys.argv[1])).get("state") or "free")
' "$1" "$2" 2>/dev/null)" || fail "claim status unreadable under $1"
  [[ -n "$state" ]] || fail "claim status empty under $1"
  [[ "$state" != "free" ]]
}

seed_graph() {
  uv run --project "$REPO_ROOT/cli" python "$REPO_ROOT/cli/tests/fixtures/graph_seed.py" "$1"
}

# ── (a) legacy fails, modern wins => node id + captured stderr ────────
log "(a): legacy-claim failure + modern-claim win => graph_node_id=node, stderr captured"

make_repo TMP_A
_ALL_TMPS+=("$TMP_A")
# A claimable node (session_id null => _CURRENT_CLAIM empty => legacy path taken).
seed_graph "${TMP_A}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-11000a","title":"legacy-fail modern-win test node","session_id":null}]}
JSON

(cd "$TMP_A" && \
  HOME="${TMP_A}/home" \
  PATH="${TMP_A}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_A}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_A}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  TARGET_START=1 \
  TARGET_INPUT="tst-11000a" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(a): init exited non-zero"

STATE_A="${TMP_A}/space/target-state.md"
[[ -f "$STATE_A" ]] || fail "(a): target-state.md was not created"

GNID_A="$(graph_node_id_of "$STATE_A")"
[[ "$GNID_A" == "tst-11000a" ]] \
  || fail "(a): expected graph_node_id 'tst-11000a' (modern claim is authority), got '${GNID_A}'"
pass "(a): graph_node_id falls back to node id when modern claim wins"

# exactly one graph_node_id line (AC1-FR)
_count_a="$(grep -c '^graph_node_id:' "$STATE_A")"
[[ "$_count_a" == "1" ]] || fail "(a): graph_node_id written ${_count_a}x, expected 1 (AC1-FR)"
pass "(a): graph_node_id written exactly once"

# ── (b) graph.json missing => fail-closed: null AND no phantom claim ──
log "(b): graph.json missing => graph_node_id null AND no claim (fail-closed, x-8e98)"

make_repo TMP_B
_ALL_TMPS+=("$TMP_B")
# No graph.json written. An ab-<8hex> id no longer resolves without a graph:
# the guard's single arm needs a graph-presence grep, so a missing graph means
# no resolvable node -> no claim (parity with _resolve_plan_for_blast).
rm -f "${TMP_B}/home/.fno/graph.json"

(cd "$TMP_B" && \
  HOME="${TMP_B}/home" \
  PATH="${TMP_B}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_B}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_B}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  TARGET_START=1 \
  TARGET_INPUT="ab-1100b0b0" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(b): init exited non-zero"

STATE_B="${TMP_B}/space/target-state.md"
[[ -f "$STATE_B" ]] || fail "(b): target-state.md was not created"

GNID_B="$(graph_node_id_of "$STATE_B")"
[[ "$GNID_B" == "null" ]] \
  || fail "(b): expected graph_node_id 'null' when graph.json missing, got '${GNID_B}'"
pass "(b): graph_node_id null when graph.json missing (Boundary preserved)"

_count_b="$(grep -c '^graph_node_id:' "$STATE_B")"
[[ "$_count_b" == "1" ]] || fail "(b): graph_node_id written ${_count_b}x, expected 1 (AC1-FR)"
pass "(b): graph_node_id written exactly once"

# No phantom claim: graph_node_id null was already true in the buggy state, so
# it alone does not prove the fix. Assert the lockfile is absent (AC3-FR).
if node_claim_exists "${TMP_B}/home" "ab-1100b0b0"; then
  fail "(b): phantom claim for node:ab-1100b0b0 acquired despite missing graph (fail-open regression)"
fi
pass "(b): no phantom claim acquired when graph.json missing (fail-closed)"

# ── (c) ab-id absent from an existing graph => null AND no claim (codex P2) ─
log "(c): ab-id not present in an existing graph.json => graph_node_id null AND no claim"

make_repo TMP_C
_ALL_TMPS+=("$TMP_C")
# Graph exists but does NOT contain the requested ab-id. The guard's single arm
# greps for the id and misses, so the node never resolves: graph_node_id stays
# null (no successor for a bogus node) AND no phantom claim is acquired (x-8e98).
seed_graph "${TMP_C}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-other0","title":"some other node","session_id":null}]}
JSON

(cd "$TMP_C" && \
  HOME="${TMP_C}/home" \
  PATH="${TMP_C}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_C}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_C}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  TARGET_START=1 \
  TARGET_INPUT="ab-deadbeef" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(c): init exited non-zero"

STATE_C="${TMP_C}/space/target-state.md"
[[ -f "$STATE_C" ]] || fail "(c): target-state.md was not created"
GNID_C="$(graph_node_id_of "$STATE_C")"
[[ "$GNID_C" == "null" ]] \
  || fail "(c): expected graph_node_id 'null' for ab-id absent from graph, got '${GNID_C}'"
pass "(c): graph_node_id null when node id is absent from an existing graph"

# Pin the phantom-claim regression: an absent ab-id must take no claim.
if node_claim_exists "${TMP_C}/home" "ab-deadbeef"; then
  fail "(c): phantom claim for node:ab-deadbeef acquired for an id absent from the graph"
fi
pass "(c): no phantom claim acquired for an ab-id absent from an existing graph"

# ── (e) AC4-HP: spawn-handover pre-claim, env UNSET, name proven => fallback ──
# The spawner's launch-window claim is the only claim on the node, but
# FNO_NODE_CLAIM_HOLDER does not always reach the worker. init used to pass no
# --handover-from and the ordinary acquire refused its own beneficiary
# (measured live 2026-09-09: rc=1 against spawn-handover:<this session's own
# worker>). The disk fallback supplies the holder VALUE, and the worker-name
# env supplies the PROOF it is this session's own.
log "(e): spawn-handover claim + env unset + own name => init acquires via the disk fallback"

make_repo TMP_E
_ALL_TMPS+=("$TMP_E")
seed_graph "${TMP_E}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-ca7471","title":"handover fallback test node","session_id":null}]}
JSON

HOME="${TMP_E}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim acquire "node:tst-ca7471" \
  --holder "spawn-handover:t-ho-worker" --ttl 15m --pid-unavailable \
  --reason "test stage" >/dev/null 2>&1 \
  || fail "(e): staging the spawn-handover claim failed"

(cd "$TMP_E" && \
  HOME="${TMP_E}/home" \
  PATH="${TMP_E}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_E}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_E}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  FNO_WORKER_NAME="t-ho-worker" \
  TARGET_START=1 \
  TARGET_INPUT="tst-ca7471" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(e): init exited non-zero"

_HOLDER_E="$(HOME="${TMP_E}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-ca7471" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_E" == target-session:* ]] \
  || fail "(e): claim holder is '${_HOLDER_E}', expected the init acquire to land via the handover fallback"
pass "(e): init acquire took over the launch-window claim (holder=${_HOLDER_E})"

_LOCKED_BY_E="$(HOME="${TMP_E}/home" uv run --project "${REPO_ROOT}/cli" python -c 'import sys; from pathlib import Path; from fno.graph.store import read_graph_strict; print(read_graph_strict(Path(sys.argv[1]))[0].get("locked_by") or "")' "${TMP_E}/home/.fno/graph.json" 2>/dev/null || true)"
[[ -n "$_LOCKED_BY_E" ]] \
  || fail "(e): locked_by is empty; the stamp never ran (AC4-HP)"
pass "(e): locked_by stamped (${_LOCKED_BY_E})"

# ── (g) a BYSTANDER's init never takes a launch-window claim minted for
# another worker: the disk holder is published to every reader, so the value
# alone is not proof. The name proven here names a DIFFERENT worker.
log "(g): spawn-handover claim naming another worker + own name proven => no takeover"

make_repo TMP_G
_ALL_TMPS+=("$TMP_G")
seed_graph "${TMP_G}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-feed42","title":"bystander control node","session_id":null}]}
JSON

HOME="${TMP_G}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  CLAUDE_CODE_SESSION_ID="g0000000-foreign-stage" \
  fno agents claim acquire "node:tst-feed42" \
  --holder "spawn-handover:someone-elses-worker" --ttl 15m --pid-unavailable \
  --reason "bystander stage" >/dev/null 2>&1 \
  || fail "(g): staging the foreign launch-window claim failed"

(cd "$TMP_G" && \
  HOME="${TMP_G}/home" \
  PATH="${TMP_G}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_G}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_G}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  FNO_WORKER_NAME="this-worker" \
  CLAUDE_CODE_SESSION_ID="g1111111-foreign-reader" \
  TARGET_START=1 \
  TARGET_INPUT="tst-feed42" \
  TARGET_LOCATION_OK="main-acknowledged" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(g): init exited non-zero"

_HOLDER_G="$(HOME="${TMP_G}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-feed42" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_G" == "spawn-handover:someone-elses-worker" ]] \
  || fail "(g): claim holder changed to '${_HOLDER_G}'; a bystander must never take a live claim minted for another worker"
pass "(g): foreign launch-window claim untouched (holder=${_HOLDER_G})"

# ── (f) control: a NON-handover holder on disk is never taken over ──
log "(f): non-handover claim + env unset => ordinary acquire still refuses (no takeover)"

make_repo TMP_F
_ALL_TMPS+=("$TMP_F")
seed_graph "${TMP_F}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-c0ffee","title":"no-takeover control node","session_id":null}]}
JSON

HOME="${TMP_F}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim acquire "node:tst-c0ffee" \
  --holder "target-session:resident" --ttl 2h --pid-unavailable \
  --reason "control stage" >/dev/null 2>&1 \
  || fail "(f): staging the resident claim failed"

(cd "$TMP_F" && \
  HOME="${TMP_F}/home" \
  PATH="${TMP_F}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_F}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_F}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  TARGET_START=1 \
  TARGET_INPUT="tst-c0ffee" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(f): init exited non-zero"

_HOLDER_F="$(HOME="${TMP_F}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-c0ffee" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_F" == "target-session:resident" ]] \
  || fail "(f): claim holder changed to '${_HOLDER_F}'; a published non-handover holder must never be taken over"
pass "(f): resident non-handover claim untouched (holder=${_HOLDER_F})"

# ── (h) a launch window that outlived its launcher: the recorded pid is the
# spawner's and the probe proves it gone (suspect + pid-absent), so the named
# worker never reached init. The builder dispatched for the node adopts the
# window through the takeover door. The bystander control (g) stays the
# boundary: its pid-unavailable shape proves nothing and is never taken.
log "(h): foreign-named window + launcher pid proven dead => builder adopts via takeover"

make_repo TMP_H
_ALL_TMPS+=("$TMP_H")
seed_graph "${TMP_H}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-deadbe1","title":"dead-launcher adoption node","session_id":null}]}
JSON

HOME="${TMP_H}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim acquire "node:tst-deadbe1" \
  --holder "spawn-handover:bp-stopped-planner" --ttl 15m --pid 999999999 \
  --reason "dead launcher stage" >/dev/null 2>&1 \
  || fail "(h): staging the dead-launcher window claim failed"

(cd "$TMP_H" && \
  HOME="${TMP_H}/home" \
  PATH="${TMP_H}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_H}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_H}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  FNO_WORKER_NAME="this-builder" \
  TARGET_START=1 \
  TARGET_INPUT="tst-deadbe1" \
  TARGET_LOCATION_OK="main-acknowledged" \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(h): init exited non-zero"

_HOLDER_H="$(HOME="${TMP_H}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-deadbe1" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_H" == target-session:* ]] \
  || fail "(h): claim holder is '${_HOLDER_H}'; a proven-dead launch window must land on the node's own builder"
pass "(h): dead launch window adopted (holder=${_HOLDER_H})"

# ── (i) the session-id proof: the engine stamps a spawn-side claim with the
# ambient harness session of its creator, so a window whose session_id names
# THIS session was minted for it. The window can still be live (the spawner
# mid-launch) and the registry row unresolvable: the name rung has no proof
# and the suspect rung needs a dead pid, and init used to refuse its own
# beneficiary (measured live 2026-10-30: a respawned bg job's init lost to
# its own spawn-handover window and wrote a claimless manifest whose blocked
# line ignored every watching tag).
log "(i): window stamped with this session's own session_id + no name proof => adopt"

make_repo TMP_I
_ALL_TMPS+=("$TMP_I")
seed_graph "${TMP_I}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-5e5510","title":"session-id adoption node","session_id":null}]}
JSON

HOME="${TMP_I}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  CLAUDE_CODE_SESSION_ID="i2222222-window-beneficiary" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  fno agents claim acquire "node:tst-5e5510" \
  --holder "spawn-handover:t-respawn-worker" --ttl 15m --pid-unavailable \
  --reason "respawn window stage" >/dev/null 2>&1 \
  || fail "(i): staging the session-stamped window claim failed"

(cd "$TMP_I" && \
  HOME="${TMP_I}/home" \
  PATH="${TMP_I}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_I}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_I}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  CLAUDE_CODE_SESSION_ID="i2222222-window-beneficiary" \
  TARGET_START=1 \
  TARGET_INPUT="tst-5e5510" \
  TARGET_LOCATION_OK="main-acknowledged" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(i): init exited non-zero"

_DISK_I="$(HOME="${TMP_I}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-5e5510" --json 2>/dev/null)"
_HOLDER_I="$(printf '%s' "$_DISK_I" \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_I" == target-session:* ]] \
  || fail "(i): claim holder is '${_HOLDER_I}'; a window stamped with this session's own session_id must land on this session (record: ${_DISK_I:-unreadable}; manifest: $(tail -5 "${TMP_I}/space/target-state.md" 2>/dev/null | tr '\n' '|'))"
pass "(i): session-stamped window adopted (holder=${_HOLDER_I})"

# ── (j) the session-id rung's own bystander boundary: a window stamped with
# ANOTHER session's id never lands on this one, whatever the holder shape.
log "(j): window stamped with another session's session_id + no name proof => no takeover"

make_repo TMP_J
_ALL_TMPS+=("$TMP_J")
seed_graph "${TMP_J}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-5e5520","title":"session-id bystander node","session_id":null}]}
JSON

HOME="${TMP_J}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  CLAUDE_CODE_SESSION_ID="j3333333-foreign-beneficiary" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  fno agents claim acquire "node:tst-5e5520" \
  --holder "spawn-handover:t-foreign-window" --ttl 15m --pid-unavailable \
  --reason "foreign window stage" >/dev/null 2>&1 \
  || fail "(j): staging the foreign session-stamped window claim failed"

(cd "$TMP_J" && \
  HOME="${TMP_J}/home" \
  PATH="${TMP_J}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_J}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_J}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  CLAUDE_CODE_SESSION_ID="j4444444-bystander-reader" \
  TARGET_START=1 \
  TARGET_INPUT="tst-5e5520" \
  TARGET_LOCATION_OK="main-acknowledged" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(j): init exited non-zero"

_HOLDER_J="$(HOME="${TMP_J}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-5e5520" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_J" == "spawn-handover:t-foreign-window" ]] \
  || fail "(j): claim holder changed to '${_HOLDER_J}'; a window minted for another session must never land on this one"
pass "(j): foreign session-stamped window untouched (holder=${_HOLDER_J})"

# ── (i2)/(j2) the same rung against the PROVEN-identity shape: a window
# staged with a pinned harness stamps session_id directly instead of the
# dispatched_by_session metadata (i)/(j) exercise. Both shapes must adopt and
# refuse identically.
log "(i2): window with proven session_id + no name proof => adopt"

make_repo TMP_I2
_ALL_TMPS+=("$TMP_I2")
seed_graph "${TMP_I2}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-5e5511","title":"proven session-id adoption node","session_id":null}]}
JSON

HOME="${TMP_I2}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  CLAUDE_CODE_SESSION_ID="i5555555-proven-beneficiary" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  fno agents claim acquire "node:tst-5e5511" \
  --holder "spawn-handover:t-respawn-worker-2" --ttl 15m --pid-unavailable \
  --harness claude \
  --reason "proven respawn window stage" >/dev/null 2>&1 \
  || fail "(i2): staging the proven window claim failed"

(cd "$TMP_I2" && \
  HOME="${TMP_I2}/home" \
  PATH="${TMP_I2}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_I2}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_I2}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  CLAUDE_CODE_SESSION_ID="i5555555-proven-beneficiary" \
  TARGET_START=1 \
  TARGET_INPUT="tst-5e5511" \
  TARGET_LOCATION_OK="main-acknowledged" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(i2): init exited non-zero"

_HOLDER_I2="$(HOME="${TMP_I2}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-5e5511" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_I2" == target-session:* ]] \
  || fail "(i2): claim holder is '${_HOLDER_I2}'; a window with a proven matching session_id must land on this session"
pass "(i2): proven session-stamped window adopted (holder=${_HOLDER_I2})"

log "(j2): window with proven FOREIGN session_id + no name proof => no takeover"

make_repo TMP_J2
_ALL_TMPS+=("$TMP_J2")
seed_graph "${TMP_J2}/home/.fno/graph.json" <<'JSON'
{"entries":[{"id":"tst-5e5521","title":"proven session-id bystander node","session_id":null}]}
JSON

HOME="${TMP_J2}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  CLAUDE_CODE_SESSION_ID="j6666666-foreign-proven" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  fno agents claim acquire "node:tst-5e5521" \
  --holder "spawn-handover:t-foreign-window-2" --ttl 15m --pid-unavailable \
  --harness claude \
  --reason "foreign proven window stage" >/dev/null 2>&1 \
  || fail "(j2): staging the foreign proven window claim failed"

(cd "$TMP_J2" && \
  HOME="${TMP_J2}/home" \
  PATH="${TMP_J2}/bin:$PATH" \
  FNO_AGENTS_BIN="${TMP_J2}/bin/fno-agents" \
  FNO_TEST_SPACE="${TMP_J2}/space" \
  FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  FNO_TARGET_INIT_GATED=1 \
  CLAUDE_CODE_SESSION_ID="j7777777-bystander-proven" \
  TARGET_START=1 \
  TARGET_INPUT="tst-5e5521" \
  TARGET_LOCATION_OK="main-acknowledged" \
  env -u CODEX_COMPANION_SESSION_ID -u CODEX_SESSION_ID -u CODEX_THREAD_ID \
  bash "$INIT" >/dev/null 2>&1) \
  || fail "(j2): init exited non-zero"

_HOLDER_J2="$(HOME="${TMP_J2}/home" FNO_BOOTSTRAP_WHEEL="${REPO_ROOT}/cli" \
  fno agents claim status "node:tst-5e5521" --json 2>/dev/null \
  | sed -n 's/.*"holder"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
[[ "$_HOLDER_J2" == "spawn-handover:t-foreign-window-2" ]] \
  || fail "(j2): claim holder changed to '${_HOLDER_J2}'; a proven foreign window must never land on this session"
pass "(j2): foreign proven window untouched (holder=${_HOLDER_J2})"

log "All init-claim scenarios passed"
