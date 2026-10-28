#!/usr/bin/env bash
# Test double for `fno-agents state path <name>`, the command
# hooks/helpers/init-target-state.sh uses to resolve its manifest location.
# Every other command delegates to the next real fno-agents on PATH (skipping this
# stub's own dir), so harnesses that exercise the real claim path get real
# claim behavior; with no real binary on PATH the call exits 1, the same as a
# CI runner with no binary.
if [[ "${1:-} ${2:-}" == "state path" && -n "${FNO_TEST_SPACE:-}" ]]; then
  case "${3:-}" in
    target-state) printf '%s\n' "$FNO_TEST_SPACE/target-state.md" ;;
    events) printf '%s\n' "$FNO_TEST_SPACE/events.jsonl" ;;
    *) printf '%s\n' "$FNO_TEST_SPACE/$3" ;;
  esac
  exit 0
fi

# `state mint-id`: the run-id mint the init hook calls. Answers a random v4
# UUID built from /dev/urandom (the hook's own pre-mint idiom, no uuidgen
# dependency); FNO_TEST_MINT_FAIL exercises the refusal path.
if [[ "${1:-} ${2:-}" == "state mint-id" ]]; then
  if [[ -n "${FNO_TEST_MINT_FAIL:-}" ]]; then
    exit 1
  fi
  _h="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  if [[ -z "$_h" ]]; then
    exit 1
  fi
  # Pin the v4 version nibble (byte 6) and the RFC-4122 variant nibble (byte 8).
  printf '%s-%s-%s-%s-%s\n' \
    "${_h:0:8}" "${_h:8:4}" "4${_h:13:3}" "8${_h:17:3}" "${_h:20:12}"
  exit 0
fi

# FNO_TEST_GATES_GREEN=1 answers the target-family gates green without the
# real binary: a harness whose subject is NOT the gates (claim-wait, say)
# still needs them quiet, and the real hold gate would read the ambient graph
# store and refuse on a sandbox that has none. resolve-owned-identity answers
# the same KEY=value block the old fno stub canned.
#
# The gates are the stub's OWN answers by default (same green), because every
# harness that copies this double runs under the smoke runner's job-wide
# FNO_AGENTS_BIN pin: delegating target-gates would hand the real gate a
# sandbox with no graph store. A harness whose subject IS a gate contract
# opts out with FNO_TEST_GATE_RC_FILE (the rc for check-review-gate, logged
# to FNO_TEST_GATE_CALL_LOG when set).
if [[ "${1:-}" == "target-gates" ]]; then
  case "${2:-}" in
    check-review-gate)
      if [[ -n "${FNO_TEST_GATE_CALL_LOG:-}" ]]; then
        printf 'gate-called\n' >> "$FNO_TEST_GATE_CALL_LOG"
      fi
      if [[ -n "${FNO_TEST_GATE_RC_FILE:-}" && -f "$FNO_TEST_GATE_RC_FILE" ]]; then
        echo "stub refusal: config.review.github_apps names a bot" >&2
        exit "$(cat "$FNO_TEST_GATE_RC_FILE")"
      fi
      exit 0 ;;
    check-dispatch-hold|check-contained)
      exit 0 ;;
    resolve-owned-identity)
      # Canned only on opt-in: the old fno stubs answered this command with
      # nothing and the helper degraded to its own provider detection, which
      # is the fiction harnesses like the session-id orphan case pin. A
      # harness that wants the proven block sets FNO_TEST_OWNED_IDENTITY_CANNED.
      if [[ "${FNO_TEST_OWNED_IDENTITY_CANNED:-}" == "1" ]]; then
        printf 'HARNESS=claude\nSESSION_ID=%s\nDISPOSITION=proven\nCOLLISION=\n' \
          "${CLAUDE_CODE_SESSION_ID:-}"
      fi
      exit 0 ;;
  esac
fi

_self_dir="$(cd "$(dirname "${BASH_SOURCE[0]:-$0}")" && pwd)"
_self="${BASH_SOURCE[0]:-$0}"
_oldifs="$IFS"
IFS=':'
for _d in $PATH; do
  IFS="$_oldifs"
  [[ "$_d" == "$_self_dir" ]] && continue
  [[ -x "$_d/fno-agents" ]] || continue
  # -ef catches a relative PATH entry that resolves to this same file, where
  # exec would re-enter the stub forever.
  [[ "$_d/fno-agents" -ef "$_self" ]] && continue
  exec "$_d/fno-agents" "$@"
done
IFS="$_oldifs"
exit 1
