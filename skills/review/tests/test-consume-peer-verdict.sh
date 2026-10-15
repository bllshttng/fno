#!/usr/bin/env bash
set -euo pipefail

REVIEW_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONSUME="$REVIEW_DIR/scripts/consume-peer-verdict.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

REPO="$TMP/repo"
BIN="$TMP/bin"
mkdir -p "$REPO" "$BIN"
git -C "$REPO" init -q
git -C "$REPO" config user.email test@example.com
git -C "$REPO" config user.name Test
touch "$REPO/tracked"
git -C "$REPO" add tracked
git -C "$REPO" commit -qm init
# The emit under a clean verdict measures the diff under review and refuses
# a zero-file one (a review of nothing is not a pass), so the fixture carries
# a real change on top of the base the emit resolves: without the second
# commit the base falls back to HEAD and every clean case trips the refusal.
git -C "$REPO" update-ref refs/remotes/origin/main "$(git -C "$REPO" rev-parse HEAD)"
printf 'body\n' > "$REPO/tracked"
git -C "$REPO" add tracked
git -C "$REPO" commit -qm feature

# The stub stands in for the CLI boundary only: it runs the real classifier -
# the native record door - logs the record it built, and for an --attest call
# logs the verdict that record measures, so run_case asserts what the writer
# wrote instead of an argv spelling. The verdict rule mirrors classify
# --attest: pass only on zero blocking findings.
REPO_ROOT="$(cd "$REVIEW_DIR/../.." && pwd)"
CLASSIFY_BIN=""
for candidate in \
  "$REPO_ROOT/crates/fno-agents/target/release/fno-agents" \
  "$REPO_ROOT/crates/fno-agents/target/debug/fno-agents"; do
  if [[ -x "$candidate" ]]; then CLASSIFY_BIN="$candidate"; break; fi
done
if [[ -z "$CLASSIFY_BIN" ]]; then
  echo "SKIP: no built fno-agents binary for the native classifier read" >&2
  echo "  tried crates/fno-agents/target/{release,debug}/fno-agents." >&2
  exit 77
fi
export CLASSIFY_BIN
cat > "$BIN/fno" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FNO_TEST_LOG"
if [[ "${1:-}" == "do" && "${2:-}" == "review" && "${3:-}" == "classify" ]]; then
  f=""; attest=""
  shift 3
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --findings-file) f="$2"; shift 2 ;;
      --attest) attest="$2"; shift 2 ;;
      *) shift ;;
    esac
  done
  record="$("$CLASSIFY_BIN" review-level findings record --findings-file "$f")" || exit $?
  out="$(printf '%s' "$record" | jq -c .)"
  {
    printf '%s\n' "$out"
    if [[ -n "$attest" ]]; then
      verdict="$(printf '%s' "$record" | jq -r 'if (.findings_blocking // 1) == 0 then "pass" else "fail" end')"
      printf '{"verdict":"%s"}\n' "$verdict"
    fi
  } >> "$FNO_TEST_LOG"
  printf '%s\n' "$out"
  exit 0
fi
exit 0
SH
chmod +x "$BIN/fno"
export PATH="$BIN:$PATH"
export FNO_TEST_LOG="$TMP/events"

run_case() {
  name="$1"
  expected_rc="$2"
  expected_verdict="$3"
  body="$4"
  file="$TMP/$name.txt"
  printf '%s' "$body" > "$file"
  : > "$FNO_TEST_LOG"
  set +e
  (cd "$REPO" && bash "$CONSUME" "$file") >"$TMP/$name.out" 2>"$TMP/$name.err"
  rc=$?
  set -e
  if [[ "$rc" -ne "$expected_rc" ]]; then
    echo "$name: expected rc=$expected_rc, got $rc" >&2
    cat "$TMP/$name.err" >&2
    exit 1
  fi
  grep -q "verdict\\\":\\\"$expected_verdict" "$FNO_TEST_LOG" || {
    echo "$name: expected emitted verdict $expected_verdict" >&2
    cat "$FNO_TEST_LOG" >&2
    exit 1
  }
}

run_case clean 0 pass $'No findings.\nfno-peer-verdict: {"verdict":"clean","blocking_findings":0}\n'
run_case blocked 1 fail $'P1 src/lib.rs:9 - panic - handle the error\nfno-peer-verdict: {"verdict":"blocked","blocking_findings":1}\n'
run_case contradictory 1 fail $'P1 src/lib.rs:9 - panic - handle the error\nfno-peer-verdict: {"verdict":"clean","blocking_findings":0}\n'
run_case malformed 1 fail $'Looks fine.\nverdict: clean\n'
run_case empty 1 fail ''
run_case count_mismatch 1 fail $'P1 a:1 - one - fix\nfno-peer-verdict: {"verdict":"blocked","blocking_findings":2}\n'

# Bare severity markers are SECTION HEADERS, not findings. codex emits this
# shape routinely, and counting the headers rejected a genuinely clean review
# as "declares 0 blocking finding(s), but output contains 2".
run_case bare_markers_are_headers 0 pass $'P1\nP2\nP3\nverdict\nfno-peer-verdict: {"verdict":"clean","blocking_findings":0}\n'
run_case bare_marker_with_colon 0 pass $'P1:\nP2:\nfno-peer-verdict: {"verdict":"clean","blocking_findings":0}\n'
# ...but a header ABOVE real findings must not change the count.
run_case headers_plus_findings 1 fail $'P1\nP1 a:1 - one - fix\nP2\nP2 b:2 - two - fix\nfno-peer-verdict: {"verdict":"blocked","blocking_findings":2}\n'

# The current terminal-record shape: the findings array classifies through
# the shared rule, and the emitted verdict follows the classified blocking
# count. A P1 line in the body still has to agree with the classified count.
run_case new_shape_clean_nonblocking_only 0 pass $'Looks fine apart from wording.\nfno-peer-verdict: {"verdict":"clean","findings":[{"category":"typo","file":"a.py","line":1,"summary":"teh","failure_scenario":"reader stumble"}]}\n'
run_case new_shape_blocked 1 fail $'P1 src/lib.rs:9 - panic - handle the error\nfno-peer-verdict: {"verdict":"blocked","findings":[{"category":"correctness","file":"src/lib.rs","line":9,"summary":"panic","failure_scenario":"crash on empty input"}]}\n'
run_case new_shape_clean_contradiction 1 fail $'P1 src/lib.rs:9 - panic - handle the error\nfno-peer-verdict: {"verdict":"clean","findings":[{"category":"correctness","file":"src/lib.rs","line":9,"summary":"panic","failure_scenario":"crash on empty input"}]}\n'
run_case new_shape_count_mismatch 1 fail $'P1 a:1 - one - fix\nP1 b:2 - two - fix\nfno-peer-verdict: {"verdict":"blocked","findings":[{"category":"correctness","file":"a","line":1,"summary":"one","failure_scenario":"f"},{"category":"correctness","file":"b","line":2,"summary":"two","failure_scenario":"f"}]}\n'

# The classified record rides on the emitted event: the pass over a
# non-blocking finding carries findings_nonblocking:1, so the gate can
# re-derive instead of trusting the verdict word.
: > "$FNO_TEST_LOG"
set +e
(cd "$REPO" && bash "$CONSUME" "$TMP/new_shape_clean_nonblocking_only.txt") >/dev/null 2>&1
rerun_rc=$?
set -e
if [[ $rerun_rc -ne 0 ]]; then
  echo "clean rerun failed rc=$rerun_rc" >&2
  exit 1
fi
grep -q '"findings_nonblocking":1' "$FNO_TEST_LOG" \
  && echo "  PASS: pass carries the classified record" \
  || { echo "  FAIL: pass carries the classified record" >&2; exit 1; }

echo "PASS consume-peer-verdict"
