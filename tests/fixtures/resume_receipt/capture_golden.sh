#!/usr/bin/env bash
# Capture (and re-freeze) the resume receipt goldens by driving the native
# door. Run from the repo root:
#
#   FNO_CAPTURE_BIN=<path-to-fno-agents> bash tests/fixtures/resume_receipt/capture_golden.sh
#
# Writes crates/fno-agents/tests/golden/resume_receipt/<case>.{exit,out,err}
# and asserts stability by running the whole set twice: the second pass must
# be byte-identical (the FNO_CAPTURE_GOLDEN convention: new equals old before
# anything freezes).
#
# Provenance: the FIRST capture ran the deleted Python leg
# (cli/src/fno/resume/receipt.py) through the source tree before this PR
# deleted it, so the goldens carry the Python leg's exact bytes. From here on
# the script drives `fno-agents resume-receipt` (FNO_CAPTURE_BIN), the same
# argv the forwarding leaf passes, so a contract change re-freezes the
# goldens against the door that answers them. The characterization test in
# crates/fno-agents/tests/resume_receipt_parity.rs re-derives every fixture
# and asserts the frozen bytes.
#
# Determinism: every fixture commit pins author/committer dates, the write
# verb pins --written-at, and the fixture base is fixed (the receipt's
# content_sha digests the raw worktree path). Each golden masks the fixture
# base as FIXTURE.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
# The test captures into a scratch dir via FNO_CAPTURE_GOLDEN_OUT; a bare run
# re-freezes the committed goldens in place.
GOLDENS="${FNO_CAPTURE_GOLDEN_OUT:-$ROOT/crates/fno-agents/tests/golden/resume_receipt}"
# A FIXED base, not mktemp: the receipt's content_sha digests the raw
# worktree path, so a randomized fixture dir would make every digest
# run-specific and the stability pass would never hold.
FIXBASE="/tmp/resume-receipt-capture-fixture"
rm -rf "$FIXBASE"
trap 'rm -rf "$FIXBASE"' EXIT
mkdir -p "$GOLDENS"

# The door, driven by the binary named in FNO_CAPTURE_BIN (the same argv the
# forwarding leaf passes). Ambient FNO_ carriers are stripped so the capture
# is hermetic (a live FNO_TASK_CONTEXT_FILE would embed this run's binding
# into the receipts).
fno_door() {
  local root="$1"; shift
  env -u FNO_TASK_CONTEXT_FILE -u FNO_CLAIMS_ROOT -u FNO_CONFIG \
      -u FNO_STATE_DIR -u FNO_EVENTS_PATH -u FNO_HOME \
      FNO_REPO_ROOT="$root" \
      "${FNO_CAPTURE_BIN:?FNO_CAPTURE_BIN must name the fno-agents binary}" \
      resume-receipt "$@"
}

# A fixture: one git repo, one deterministic commit, an empty .fno. Idempotent
# so several cases can share one fixture.
new_fixture() {
  local dir="$1"
  mkdir -p "$dir/repo/.fno/artifacts/handoff" "$dir/claims"
  git -C "$dir/repo" init -q -b feature/x
  git -C "$dir/repo" config user.email cap@example.test
  git -C "$dir/repo" config user.name cap
  git -C "$dir/repo" config commit.gpgsign false
  printf 'seed\n' > "$dir/repo/seed.txt"
  git -C "$dir/repo" add seed.txt
  if ! git -C "$dir/repo" rev-parse -q --verify HEAD > /dev/null 2>&1; then
    GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z" \
      git -C "$dir/repo" commit -q -m seed
  fi
}

head_of() { git -C "$1/repo" rev-parse HEAD; }

# One case: ensure the fixture, run the door, mask, freeze. The case args
# keep the old typer-path spelling (`do resume receipt <verb> ...`); the
# fixed prefix drops here, leaving the verb the door takes.
#   case_run <name> <fixture-dir> <args...>
case_run() {
  local name="$1" dir="$2"; shift 2
  new_fixture "$dir"
  local out err rc
  out="$dir/.stdout"; err="$dir/.stderr"
  set +e
  fno_door "$dir/repo" "${@:4}" > "$out" 2> "$err"
  rc=$?
  set -e
  {
    printf '%s\n' "$rc"
    cat "$out"
  } | sed "s|$FIXBASE|FIXTURE|g" > "$GOLDENS/$name.out"
  sed "s|$FIXBASE|FIXTURE|g" "$err" > "$GOLDENS/$name.err"
  rm -f "$out" "$err"
  printf '%s\n' "$rc" > "$GOLDENS/$name.exit"
}

## ── happy path ────────────────────────────────────────────────────────────

# write-hp: the full option surface, fixed stamp. The written receipt file
# itself is a golden (write-hp-file): the byte shape a successor parses.
HP="$FIXBASE/write-hp"
new_fixture "$HP"
head="$(head_of "$HP")"
case_run write-hp "$HP" \
  do resume receipt write --node x-1 --session-id sess-1 --phase do \
  --generation 1 --repo fno --worktree "$HP/repo" --branch feature/x \
  --head "$head" --next-command "/fno:ship pr check 12" --next-target x-1 \
  --completed-tasks "1.1
1.2" --remaining-tasks "2.1" --written-at 2026-10-24T12:00:00Z
receipt_file="$(ls "$HP/repo/.fno/artifacts/handoff"/receipt-x-1-*.json)"
sed "s|$FIXBASE|FIXTURE|g" "$receipt_file" > "$GOLDENS/write-hp-file.out"
printf '0\n' > "$GOLDENS/write-hp-file.exit"
: > "$GOLDENS/write-hp-file.err"

# show + validate consume the fixture write-hp just produced.
case_run show-hp "$HP" \
  do resume receipt show --node x-1

case_run validate-hp "$HP" \
  do resume receipt validate --node x-1 --session-id sess-1 \
  --worktree "$HP/repo" --claims-root "$HP/claims" \
  --events "$HP/repo/.fno/events.jsonl"

# The legacy alias answers identically.
case_run session-alias "$HP" \
  do resume receipt write --node x-alias --session legacy-sess --phase do \
  --generation 1 --repo fno --worktree "$HP/repo" --branch feature/x \
  --head "$head" --next-command "next" --written-at 2026-10-24T12:00:00Z

## ── errors ────────────────────────────────────────────────────────────────

case_run validate-no-receipt "$HP" \
  do resume receipt validate --node x-none --claims-root "$HP/claims" \
  --worktree "$HP/repo"

case_run show-no-receipt "$HP" \
  do resume receipt show --node x-none

# Duplicate write refuses: the immutable-identity guard.
case_run write-duplicate "$HP" \
  do resume receipt write --node x-1 --session-id sess-1 --phase do \
  --generation 1 --repo fno --worktree "$HP/repo" --branch feature/x \
  --head "$head" --next-command again \
  --written-at 2026-10-24T13:00:00Z

# generation 0 fails validation before any write attempt.
case_run write-bad-generation "$FIXBASE/write-bad-generation" \
  do resume receipt write --node x-1 --session-id s --phase do \
  --generation 0 --repo fno --worktree "$FIXBASE/write-bad-generation/repo" --branch b \
  --head "0123456789abcdef" --next-command next

# A corrupt latest receipt is malformed, never silently skipped.
cp -R "$HP" "$FIXBASE/malformed"
f="$(ls "$FIXBASE/malformed/repo/.fno/artifacts/handoff"/receipt-x-1-*.json)"
printf '{"version": 1, "broken": ' > "$f"
case_run validate-malformed "$FIXBASE/malformed" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/malformed/claims" \
  --worktree "$FIXBASE/malformed/repo" --session-id sess-1

# Tampered payload: content_sha mismatch refuses.
cp -R "$HP" "$FIXBASE/tampered"
f="$(ls "$FIXBASE/tampered/repo/.fno/artifacts/handoff"/receipt-x-1-*.json)"
python3 - "$f" <<'PYEOF'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["repo"] = "other"
json.dump(d, open(p, "w"), indent=2, sort_keys=True)
PYEOF
case_run validate-tampered "$FIXBASE/tampered" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/tampered/claims" \
  --worktree "$FIXBASE/tampered/repo" --session-id sess-1

# stale_head: the candidate moved.
cp -R "$HP" "$FIXBASE/stale"
GIT_AUTHOR_DATE="2026-01-02T00:00:00Z" GIT_COMMITTER_DATE="2026-01-02T00:00:00Z" \
  git -C "$FIXBASE/stale/repo" commit -q --allow-empty -m move
case_run validate-stale-head "$FIXBASE/stale" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/stale/claims" \
  --worktree "$FIXBASE/stale/repo" --session-id sess-1

# foreign_claim: a live claim held by another session refuses. The lockfile
# is the native record format at the native path (root/.fno/claims/), pid
# pinned to this script's own pid so the holder reads LIVE, not suspect.
cp -R "$HP" "$FIXBASE/foreign"
mkdir -p "$FIXBASE/foreign/claims/.fno/claims"
{
  printf 'schema_version: 1\n'
  printf 'key: node:x-1\n'
  printf 'holder: someone-else\n'
  printf 'acquired_at: 1\n'
  printf 'pid: %s\n' "$$"
  printf 'host: cap\n'
  printf 'expires_at: 99999999999999\n'
} > "$FIXBASE/foreign/claims/.fno/claims/node%3Ax-1.lock"
case_run validate-foreign-claim "$FIXBASE/foreign" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/foreign/claims" \
  --worktree "$FIXBASE/foreign/repo" --session-id sess-1

# corrupted_claim: an unreadable lockfile fails CLOSED (placed at the real
# claim path, not a guessed one).
cp -R "$HP" "$FIXBASE/corruptclaim"
mkdir -p "$FIXBASE/corruptclaim/claims/.fno/claims"
printf '{not json' > "$FIXBASE/corruptclaim/claims/.fno/claims/node%3Ax-1.lock"
case_run validate-corrupted-claim "$FIXBASE/corruptclaim" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/corruptclaim/claims" \
  --worktree "$FIXBASE/corruptclaim/repo" --session-id sess-1

# superseded: a foreign later termination postdates the receipt.
cp -R "$HP" "$FIXBASE/superseded"
cat >> "$FIXBASE/superseded/repo/.fno/events.jsonl" <<'EOF2'
{"ts": "2026-10-25T00:00:00Z", "type": "termination", "node_id": "x-1", "data": {"session_id": "foreign"}}
EOF2
case_run validate-superseded "$FIXBASE/superseded" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/superseded/claims" \
  --worktree "$FIXBASE/superseded/repo" --session-id sess-1

# duplicate_generation: a delegated event minted this generation off-session.
cp -R "$HP" "$FIXBASE/dupgen"
cat >> "$FIXBASE/dupgen/repo/.fno/events.jsonl" <<'EOF2'
{"ts": "2026-10-24T09:00:00Z", "type": "delegated", "node_id": "x-1", "data": {"node_id": "x-1", "harness": "claude", "generation": 1, "from_session": "the-other-one"}}
EOF2
case_run validate-duplicate-generation "$FIXBASE/dupgen" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/dupgen/claims" \
  --worktree "$FIXBASE/dupgen/repo" --session-id sess-1 --harness claude

## ── edges ─────────────────────────────────────────────────────────────────

# The receipt's OWN later continuation never supersedes.
cp -R "$HP" "$FIXBASE/own-cont"
cat >> "$FIXBASE/own-cont/repo/.fno/events.jsonl" <<'EOF2'
{"ts": "2026-10-26T00:00:00Z", "type": "delegated", "node_id": "x-1", "data": {"node_id": "x-1", "generation": 2, "from_session": "sess-1"}}
EOF2
case_run validate-own-continuation "$FIXBASE/own-cont" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/own-cont/claims" \
  --worktree "$FIXBASE/own-cont/repo" --session-id sess-1

# An undated late event never supersedes (conservative on missing ts).
cp -R "$HP" "$FIXBASE/undated"
cat >> "$FIXBASE/undated/repo/.fno/events.jsonl" <<'EOF2'
{"type": "termination", "node_id": "x-1", "data": {"session_id": "foreign"}}
EOF2
case_run validate-undated-never-supersedes "$FIXBASE/undated" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/undated/claims" \
  --worktree "$FIXBASE/undated/repo" --session-id sess-1

# The ephemeral sibling journal is read too (claim lifecycle kinds live there).
cp -R "$HP" "$FIXBASE/ephemeral"
cat >> "$FIXBASE/ephemeral/repo/.fno/events.jsonl.ephemeral" <<'EOF2'
{"ts": "2026-10-25T00:00:00Z", "type": "claim_acquired", "node_id": "x-1", "data": {"session_id": "sess-1"}}
EOF2
case_run validate-ephemeral-sibling "$FIXBASE/ephemeral" \
  do resume receipt validate --node x-1 --claims-root "$FIXBASE/ephemeral/claims" \
  --worktree "$FIXBASE/ephemeral/repo" --session-id sess-1

echo "captured $(ls "$GOLDENS" | grep -c '\.exit$') cases into $GOLDENS"
