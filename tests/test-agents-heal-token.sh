#!/usr/bin/env bash
# End-to-end check of the registry-miss heal (x-da8c): a stored-but-unregistered
# session must be addressable by the Rust lifecycle commands, which heal in
# process (crates/fno-agents/src/store_heal.rs). The goldens pin the verb's
# bytes; this tree drives the lifecycle commands through a real adoption.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/crates/fno-agents/target/debug/fno-agents"
VENV_PY="$ROOT/cli/.venv/bin/python"

# Exit 77, not 0: a skip must not read as a green run. `smoke.sh --only '*heal*'`
# selects this step without the build/sync steps that produce its prerequisites,
# and exit 0 there would report "passed" having asserted nothing.
if [[ ! -x "$BIN" ]]; then
  echo "SKIP: no debug fno-agents binary (cargo build --manifest-path crates/fno-agents/Cargo.toml)"
  exit 77
fi
if [[ ! -x "$VENV_PY" ]]; then
  echo "SKIP: no cli venv (uv sync --project cli)"
  exit 77
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
FIXCWD="$tmp/repo-one"
# claude slug-encodes a project dir path (/ and . -> -), so the transcript
# dir is derived from the fixture cwd the same way the resume resolver does.
FIXSLUG="$(printf '%s' "$FIXCWD" | tr '/.' '--')"
mkdir -p "$tmp/bin" "$FIXCWD" "$tmp/projects/$FIXSLUG" "$tmp/codex/2026/07/20" "$tmp/home"
# Run from a cwd that is NOT a known project: project confinement (defect 1) is
# N/A outside a project, so this test exercises machine-wide adoption. The confined path (same-project adopt,
# foreign refuse) is covered by cli/tests/agents/test_store_fallback_confinement.py.
cd "$tmp" || { echo "FAIL: could not enter tmp"; exit 1; }

# Shim `fno` to this worktree's source. `mux` never
# forwards: the Python front execs the `fno` on PATH for that command, which is
# this shim again, and the recursion runs until the runner kills it. attach
# reaches the mux through `fno mux thread`, so the shim answers as a box with
# no live mux server.
restore_fno() {
  cat > "$tmp/bin/fno" <<EOF
#!/bin/sh
if [ "\$1" = mux ]; then
  echo "fno mux: no live mux server (heal-token shim)" >&2
  exit 24
fi
exec "$VENV_PY" -c 'from fno.cli import app; app()' "\$@"
EOF
  chmod +x "$tmp/bin/fno"
}
restore_fno

# `resume` checks its provider CLI is on PATH and exits 14 BEFORE rendering, even
# under --print-command. A CI runner has no claude, so without this stub the
# harness dies at the first assertion having tested nothing. Only ever reached by
# a --print-command call, which returns before any exec, so the stub never runs.
printf '%s\n%s\n' '#!/bin/sh' 'exit 0' > "$tmp/bin/claude"
chmod +x "$tmp/bin/claude"

CLAUDE_UUID=c655c326-1111-2222-3333-444455556666
TWIN_UUID=c655c326-9999-8888-7777-666655554444
printf '%s\n%s\n%s\n' \
  '{"type":"summary"}' \
  '{"type":"user","cwd":"'"$FIXCWD"'"}' \
  '{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"<promise>done</promise>"}]}}' \
  > "$tmp/projects/$FIXSLUG/$CLAUDE_UUID.jsonl"

export HOME="$tmp/home"
export FIXCWD="$FIXCWD"
export FNO_CLAUDE_PROJECTS_DIR="$tmp/projects"
export FNO_CODEX_SESSIONS_DIR="$tmp/codex"
export PYTHONPATH="$ROOT/cli/src"
export PATH="$tmp/bin:$PATH"
export FNO_AGENTS_BIN="$BIN"
REGISTRY="$tmp/home/.fno/agents/registry.json"

fail() { echo "FAIL: $1"; exit 1; }

# The registry is a table in graph.db and registry.json is only its migration
# fence, so every read and reset goes through the registry-commit door.
reg_rows() {
  "$VENV_PY" -c 'import json, sys; from pathlib import Path
from fno.registry_door import read_registry_rows
print(json.dumps(read_registry_rows(Path(sys.argv[1]))))' "${1:-$REGISTRY}"
}
reg_clear() {
  "$VENV_PY" -c 'import sys; from pathlib import Path
from fno.registry_door import commit_registry_document, read_registry_document
path = Path(sys.argv[1])
document, revision = read_registry_document(path)
commit_registry_document(path, {"schema_version": document["schema_version"], "agents": [], "replace": True}, revision)' "${1:-$REGISTRY}" \
    || fail "could not reset the registry table"
}

# 1. A session-shaped token the store knows resolves and is ADOPTED. `resume
#    --print-command` is the assertion surface: it proves resolution AND that the
#    healed row carries the uuid the dead arm needs (`logs` would hand off to
#    claude's own job store, which knows nothing about a reaped session).
out=$("$BIN" resume c655c326 --print-command 2>&1) || fail "resume of a stored session: $out"
grep -q -- "--resume $CLAUDE_UUID" <<<"$out" || fail "dead arm did not build the uuid argv: $out"
grep -qF "cd $FIXCWD" <<<"$out" || fail "healed row lost its recorded cwd: $out"
reg_rows | "$VENV_PY" -c '
import json, os, sys
rows = json.load(sys.stdin)
assert len(rows) == 1, rows
r = rows[0]
# Store membership proves the session EXISTS, never that it runs.
assert r["status"] == "orphaned", r
assert r["short_id"] == "c655c326", r
assert r["cwd"] == os.environ["FIXCWD"], r
' || fail "adopted row is wrong (see above)"

# 2. A name-shaped token never probes: byte-identical refusal, nothing adopted.
before=$(reg_rows)
err=$("$BIN" logs reviewer 2>&1); rc=$?
[[ $rc -eq 13 ]] || fail "name miss exited $rc, want 13"
[[ "$err" == "no agent matching 'reviewer'; accepted forms: name, canonical handle, transport short id, or full session id" ]] \
  || fail "name miss message drifted: $err"
[[ "$(reg_rows)" == "$before" ]] || fail "a name-shaped miss mutated the registry"

# 5. Ambiguity refuses loudly with EVERY candidate and adopts nothing.
printf '{"type":"session_meta","payload":{"id":"%s","cwd":"/repo/two"}}\n' "$TWIN_UUID" \
  > "$tmp/codex/2026/07/20/rollout-2026-07-20T10-00-00-$TWIN_UUID.jsonl"
reg_clear
err=$("$BIN" logs c655c326 2>&1); rc=$?
# The contract code, not merely non-zero: `-ne 0` would also pass on a panic.
[[ $rc -eq 13 ]] || fail "ambiguous token exited $rc, want 13 (logs' refusal code)"
grep -q "$CLAUDE_UUID" <<<"$err" || fail "ambiguity message omits the claude candidate: $err"
grep -q "$TWIN_UUID" <<<"$err" || fail "ambiguity message omits the codex candidate: $err"
[[ "$(reg_rows)" == "[]" ]] || fail "an ambiguous token adopted a row"

# 6. attach resolves through the same wrapper. A healed row's liveness comes from
#    probing reality (locate_session + socket), never the row's `status`, so this
#    fixture - a stored session with no live supervisor - must reach the
#    dead-revivable pointer rather than the pre-heal "no agent matching".
reg_clear
rm -f "$tmp/codex/2026/07/20/rollout-2026-07-20T10-00-00-$TWIN_UUID.jsonl"
err=$("$BIN" attach c655c326 2>&1); rc=$?
[[ $rc -ne 0 ]] || fail "attach of a dead stored session unexpectedly succeeded: $err"
grep -q "no agent matching" <<<"$err" && fail "attach did not heal: $err"
grep -q "has exited" <<<"$err" || fail "attach did not reach the revival pointer: $err"

# 8. The heal adopts into the registry the CALLER read. Without that, the row
#    lands in another file: the command still works, so nothing warns, but the
#    roster never gains the row and every later call re-heals.
reg_clear
ALT_HOME="$tmp/alt-agents"
mkdir -p "$ALT_HOME"
out=$(FNO_AGENTS_HOME="$ALT_HOME" "$BIN" resume c655c326 --print-command 2>&1) || \
  fail "heal under FNO_AGENTS_HOME failed: $out"
[[ "$(reg_rows "$ALT_HOME/registry.json")" != "[]" ]] || fail "adopted row did not land in FNO_AGENTS_HOME"
[[ "$(reg_rows)" == "[]" ]] || fail "adopted row leaked into the default registry"

echo "PASS"
