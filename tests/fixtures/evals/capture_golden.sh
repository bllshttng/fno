#!/usr/bin/env bash
# Capture (and re-freeze) the evals goldens by driving the Python leaf.
# Run from the repo root:
#
#   bash tests/fixtures/evals/capture_golden.sh
#
# Writes crates/fno-agents/tests/golden/evals/<case>.{exit,out,err} and
# asserts stability by running the whole set twice: the second pass must be
# byte-identical (the FNO_CAPTURE_GOLDEN convention: new equals old before
# anything freezes).
#
# Provenance: this capture runs the Python leg (cli/src/fno/evals/) through
# the source tree (`uv run --project cli fno-py doctor evals ...`) BEFORE the
# port deletes it, so the goldens carry the Python leaf's exact bytes. The
# characterization test re-derives every fixture through the same leaf after
# the port (the leaf forwards the native dispatch), so the frozen bytes pin
# the leaf contract across the language move.
#
# Determinism: the fixture base is FIXED, every fixture commit pins
# author/committer dates, the fold cases read history rows stamped far in
# the past (outside every stale window), and the fixture settings.yaml
# points state, worktrees and history inside FIXBASE. Each golden masks the
# fixture base as FIXTURE and the Python warnings site prefix as WARNING:.
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
GOLDENS="${FNO_CAPTURE_GOLDEN_OUT:-$ROOT/crates/fno-agents/tests/golden/evals}"
# A FIXED base, not mktemp: bank refusals echo the bank dir and the cohort
# pins digest the fixture repo's path, so a randomized dir would make every
# byte run-specific and the stability pass would never hold.
FIXBASE="/tmp/evals-capture-fixture"
rm -rf "$FIXBASE"
trap 'rm -rf "$FIXBASE"' EXIT
mkdir -p "$GOLDENS" "$FIXBASE/state" "$FIXBASE/wtbase"

# The deployed fno-agents binary answers the native doors the leaf calls
# (evals-trend, evals-attempt, evals-macro). find_dev_binary needs a build in
# THIS checkout, which the capture never performs, so the deployed binary is
# the door that resolves.
AGENTS_BIN="${FNO_CAPTURE_AGENTS_BIN:-$(command -v fno-agents || true)}"

# The leaf, driven through the source tree. Hermetic: the fixture settings
# file is the only config, state lands in FIXBASE, and the canonical config
# never merges.
fno_leaf() {
  local cwd="$1"; shift
  ( cd "$cwd" && env -u FNO_TASK_CONTEXT_FILE -u FNO_CLAIMS_ROOT -u FNO_EVENTS_PATH \
      -u FNO_HOME -u FNO_REPO_ROOT \
      FNO_CONFIG="$FIXBASE/settings.yaml" \
      FNO_STATE_DIR="$FIXBASE/state" \
      FNO_NO_CANONICAL_CONFIG=1 \
      FNO_AGENTS_BIN="${AGENTS_BIN:-/nonexistent-fno-agents}" \
      PYTHONWARNINGS=always \
      PYTHONHASHSEED=0 \
      uv run --project "$ROOT/cli" fno-py doctor evals "$@" )
}

# The shared byte mask: the fixture base, and every rfc3339 wall-clock
# stamp (the fold echoes its window bounds, which move with the clock).
mask_bytes() {
  sed -E -e "s,$FIXBASE,FIXTURE,g" \
      -e "s,/privateFIXTURE,FIXTURE,g" \
      -e "s,[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}([.][0-9]+)?(Z|[+-][0-9]{2}:[0-9]{2})?,<TS>,g"
}

# One case: run the leaf, mask, freeze.
#   case_run <name> <cwd> [stdin-file] -- <args...>
case_run() {
  local name="$1" cwd="$2" stdin="$3"; shift 3
  [ "$1" = "--" ] && shift
  local out err rc
  out="$FIXBASE/.stdout"; err="$FIXBASE/.stderr"
  set +e
  if [ "$stdin" = "-" ]; then
    fno_leaf "$cwd" "$@" > "$out" 2> "$err"
  else
    fno_leaf "$cwd" "$@" < "$stdin" > "$out" 2> "$err"
  fi
  rc=$?
  set -e
  {
    printf '%s\n' "$rc"
    cat "$out"
  } | mask_bytes > "$GOLDENS/$name.out"
  mask_bytes < "$err" | sed -E -e "s|^[^ ]*bank\.py:[0-9]+: UserWarning: |WARNING: |" \
      -e "/^  [a-z_]+\(/d" \
      > "$GOLDENS/$name.err"
  rm -f "$out" "$err"
  printf '%s\n' "$rc" > "$GOLDENS/$name.exit"
}

# A fixture repo: one git repo, one deterministic commit, an evals/bank with
# the tasks the cases name. Idempotent so several cases share one fixture.
new_repo_fixture() {
  local dir="$1"
  mkdir -p "$dir/repo/evals/bank" "$dir/repo/.fno"
  git -C "$dir/repo" init -q -b main
  git -C "$dir/repo" config user.email cap@example.test
  git -C "$dir/repo" config user.name cap
  git -C "$dir/repo" config commit.gpgsign false
  printf 'seed body\n' > "$dir/repo/seed.txt"
  git -C "$dir/repo" add seed.txt
  if ! git -C "$dir/repo" rev-parse -q --verify HEAD > /dev/null 2>&1; then
    GIT_AUTHOR_DATE="2026-01-01T00:00:00Z" GIT_COMMITTER_DATE="2026-01-01T00:00:00Z" \
      git -C "$dir/repo" commit -q -m seed
  fi
}

# Bank tasks. grade-only (no prompt): the runner never spawns a worker.
bank_grade_only() {
  cat > "$1/repo/evals/bank/grade-only.yaml" <<'EOF'
id: grade-only
tier: regression
repo_fixture: HEAD
grade:
  - kind: file-exists
    path: "seed.txt"
  - kind: grep
    path: "seed.txt"
    pattern: "seed body"
timeout_minutes: 1
EOF
}

bank_decorative() {
  cat > "$1/repo/evals/bank/decorative.yaml" <<'EOF'
id: decorative
tier: capability
repo_fixture: HEAD
grade:
  - kind: exit
    command: "true"
timeout_minutes: 1
EOF
}

bank_invalid() {
  cat > "$1/repo/evals/bank/invalid.yaml" <<'EOF'
id: invalid
tier: regression
repo_fixture: HEAD
grade: []
timeout_minutes: 1
EOF
}

bank_graduate() {
  cat > "$1/repo/evals/bank/grad-me.yaml" <<'EOF'
id: grad-me
tier: capability   # the reviewed retag target
repo_fixture: HEAD
grade:
  - kind: file-exists
    path: "seed.txt"
timeout_minutes: 1
EOF
}

# The fixture settings file: state, worktrees and history all inside FIXBASE.
write_settings() {
  {
    printf 'state_dir: %s/state\n' "$FIXBASE"
    printf 'paths:\n'
    printf '  worktrees_base: %s/wtbase\n' "$FIXBASE"
    printf '  evals_history: %s/state/evals-history.jsonl\n' "$FIXBASE"
    printf 'evals:\n'
    printf '  enabled: true\n'
  } > "$FIXBASE/settings.yaml"
}
write_settings

# Warm the project venv before the first case: uv's first run creates the
# environment and its bootstrap lines would otherwise land in a golden.
uv run --project "$ROOT/cli" fno-py --help > /dev/null 2>&1 || true

# Warm the fixture env too: the CLI's first-run path-migration line under a
# fresh state dir would otherwise land in the first case's stderr.
( cd "$ROOT" && env -u FNO_TASK_CONTEXT_FILE -u FNO_CLAIMS_ROOT -u FNO_EVENTS_PATH \
    -u FNO_HOME -u FNO_REPO_ROOT \
    FNO_CONFIG="$FIXBASE/settings.yaml" \
    FNO_STATE_DIR="$FIXBASE/state" \
    FNO_NO_CANONICAL_CONFIG=1 \
    uv run --project "$ROOT/cli" fno-py doctor evals --help > /dev/null 2>&1 ) || true

## ── run: refusals ─────────────────────────────────────────────────────────

RB="$FIXBASE/run-refusals"
new_repo_fixture "$RB"

case_run run-bad-repeat "$RB/repo" - run --bank "$RB/repo/evals/bank" --repeat 0

case_run run-bad-variant "$RB/repo" - run --bank "$RB/repo/evals/bank" --variant v2

case_run run-bad-variant-name "$RB/repo" - run --bank "$RB/repo/evals/bank" --variant nope

# An empty bank dir is "no bank", a missing one is the default path refusal.
mkdir -p "$RB/emptybank"
case_run run-no-bank "$RB/repo" - run --bank "$RB/emptybank"

bank_grade_only "$RB"
case_run run-no-selection "$RB/repo" - run --bank "$RB/repo/evals/bank" --task nope

bank_invalid "$RB"
case_run run-invalid-task "$RB/repo" - run --bank "$RB/repo/evals/bank"

## ── run: the confirm gate ─────────────────────────────────────────────────

# 1 task x 21 repeats crosses the 20-run line; stdin answers no. Its own
# fixture: a shared bank would carry the invalid task and abort at load.
printf 'n\n' > "$FIXBASE/no.txt"
CF="$FIXBASE/run-confirm"
new_repo_fixture "$CF"
bank_grade_only "$CF"
case_run run-confirm-abort "$CF/repo" "$FIXBASE/no.txt" run --bank "$CF/repo/evals/bank" --repeat 21

## ── run: happy path (grade-only, no worker spawn) ─────────────────────────

RH="$FIXBASE/run-hp"
new_repo_fixture "$RH"
bank_grade_only "$RH"
case_run run-hp "$RH/repo" - run --bank "$RH/repo/evals/bank"

# The decorative grade warns at load; the warning site prefix is masked and
# the message body is the contract.
RD="$FIXBASE/run-decorative"
new_repo_fixture "$RD"
bank_decorative "$RD"
case_run run-decorative "$RD/repo" - run --bank "$RD/repo/evals/bank"

## ── graduate ──────────────────────────────────────────────────────────────

GG="$FIXBASE/graduate"
new_repo_fixture "$GG"
bank_graduate "$GG"
case_run graduate-hp "$GG/repo" - --bank "$GG/repo/evals/bank" grad-me

case_run graduate-missing "$GG/repo" - --bank "$GG/repo/evals/bank" no-such-task

cat > "$GG/repo/evals/bank/already.yaml" <<'EOF'
id: already
tier: regression
repo_fixture: HEAD
grade:
  - kind: file-exists
    path: "seed.txt"
timeout_minutes: 1
EOF
case_run graduate-not-capability "$GG/repo" - --bank "$GG/repo/evals/bank" already

## ── grade: the research-brief scorer ──────────────────────────────────────

GR="$FIXBASE/grade"
mkdir -p "$GR"

# A GREEN brief: one content section, one cited claim, one live source, and
# a golden whose headings the section covers.
mkdir -p "$GR/green"
cat > "$GR/green/brief.md" <<'EOF'
---
sources: brief.sources.jsonl
---
## Findings

- the fold is native [S1]

## Sources

[S1]: https://example.test/fold
EOF
cat > "$GR/green/brief.sources.jsonl" <<'EOF'
{"url": "https://example.test/fold", "verified": true, "reason": ""}
EOF
cat > "$GR/green/golden.md" <<'EOF'
---
title: golden
---
## Findings

- fold the history natively
EOF
case_run grade-green "$GR/green" - grade --brief brief.md --golden golden.md

# Uncited claim: RED on (a).
mkdir -p "$GR/uncited"
sed 's/ \[S1\]//' "$GR/green/brief.md" > "$GR/uncited/brief.md"
cp "$GR/green/brief.sources.jsonl" "$GR/uncited/"
cp "$GR/green/golden.md" "$GR/uncited/"
case_run grade-uncited "$GR/uncited" - grade --brief brief.md --golden golden.md

# Dead source: RED on (b).
mkdir -p "$GR/dead"
cat > "$GR/dead/brief.sources.jsonl" <<'EOF'
{"url": "https://example.test/fold", "verified": false, "reason": "fetch failed"}
EOF
cp "$GR/green/brief.md" "$GR/dead/"
cp "$GR/green/golden.md" "$GR/dead/"
case_run grade-dead-url "$GR/dead" - grade --brief brief.md --golden golden.md

# Uncovered section: RED on (c).
mkdir -p "$GR/uncovered"
cat > "$GR/uncovered/golden.md" <<'EOF'
## Methods

- measure twice
EOF
cp "$GR/green/brief.md" "$GR/uncovered/"
cp "$GR/green/brief.sources.jsonl" "$GR/uncovered/"
case_run grade-uncovered "$GR/uncovered" - grade --brief brief.md --golden golden.md

# Setup errors: exit 2, never a verdict.
case_run grade-missing-brief "$GR/green" - grade --brief absent.md --golden golden.md
case_run grade-missing-golden "$GR/green" - grade --brief brief.md --golden absent.md
case_run grade-sidecar-missing "$GR/green" - grade --brief brief.md --golden golden.md --sidecar absent.jsonl

# A golden with no headings is a setup error, not a red brief.
mkdir -p "$GR/noheadings"
printf 'no headings here\n' > "$GR/noheadings/golden.md"
cp "$GR/green/brief.md" "$GR/noheadings/"
cp "$GR/green/brief.sources.jsonl" "$GR/noheadings/"
case_run grade-no-checklist "$GR/noheadings" - grade --brief brief.md --golden golden.md

## ── export / qualify: the cohort glue over the native door ────────────────

CQ="$FIXBASE/cohorts"
new_repo_fixture "$CQ"
bank_grade_only "$CQ"
cat > "$CQ/repo/evals/bank/qual-task.yaml" <<'EOF'
id: qual-task
tier: regression
repo_fixture: HEAD
grade:
  - kind: file-exists
    path: "seed.txt"
timeout_minutes: 1
EOF
cat > "$CQ/repo/evals/bank/holdout.yaml" <<'EOF'
id: holdout
tier: regression
repo_fixture: HEAD
grade:
  - kind: file-exists
    path: "seed.txt"
timeout_minutes: 1
EOF
BANK_REV="$(git -C "$CQ/repo" rev-parse HEAD)"
{
  printf 'bank_rev: %s\n' "$BANK_REV"
  printf 'train:\n  - grade-only\n'
  printf 'validation:\n  - holdout\n'
  printf 'qualification:\n  - qual-task\n'
} > "$CQ/repo/evals/bank/cohorts.yaml"

# History rows the doors classify: two train rows, one qualification row
# that grades, one holdout row that never enters either view. bank_rev
# matches the pin so --repo passes bank_unchanged.
ROWS="$FIXBASE/state/evals-history.jsonl"
ROWTRAIN='{"ts": "2026-01-01T00:00:00Z", "task_id": "grade-only", "tier": "regression", "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0, "attempt_index": 0, "attempt_id": "a1", "run_id": "r1", "obs": {"fixture_prepared": true, "worker_required": false, "grader_ran": true, "grader_passed": true}, "bank_rev": "BANKREV", "variant": "baseline"}'
ROWTRAIN2='{"ts": "2026-01-01T00:01:00Z", "task_id": "grade-only", "tier": "regression", "pass": false, "reason": "grader said no", "duration_s": 1.0, "repeat_index": 1, "attempt_index": 0, "attempt_id": "a2", "run_id": "r1", "obs": {"fixture_prepared": true, "worker_required": false, "grader_ran": true, "grader_passed": false}, "bank_rev": "BANKREV", "variant": "baseline"}'
ROWQUAL='{"ts": "2026-01-01T00:02:00Z", "task_id": "qual-task", "tier": "regression", "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0, "attempt_index": 0, "attempt_id": "a3", "run_id": "r1", "obs": {"fixture_prepared": true, "worker_required": false, "grader_ran": true, "grader_passed": true}, "bank_rev": "BANKREV", "variant": "baseline"}'
ROWHOLD='{"ts": "2026-01-01T00:03:00Z", "task_id": "holdout", "tier": "regression", "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0, "attempt_index": 0, "attempt_id": "a4", "run_id": "r1", "obs": {"fixture_prepared": true, "worker_required": false, "grader_ran": true, "grader_passed": true}, "bank_rev": "BANKREV", "variant": "baseline"}'
{
  printf '%s\n' "$ROWTRAIN" | sed "s|BANKREV|$BANK_REV|g"
  printf '%s\n' "$ROWTRAIN2" | sed "s|BANKREV|$BANK_REV|g"
  printf '%s\n' "$ROWQUAL" | sed "s|BANKREV|$BANK_REV|g"
  printf '%s\n' "$ROWHOLD" | sed "s|BANKREV|$BANK_REV|g"
} > "$ROWS"

case_run export-no-out "$CQ/repo" - export --bank "$CQ/repo/evals/bank"
# --history is explicit on the two row-reading cases: the rows contract is
# pinned by flag because the native config reader's documented surface is
# TOML-only, and this fixture pins a YAML config the Python leg read but the
# native dispatch must not have to parse.
case_run export-hp "$CQ/repo" - export --bank "$CQ/repo/evals/bank" --history "$ROWS" --out "$FIXBASE/state/train.jsonl"

# A bank with no cohorts.yaml refuses the export outright.
NS="$FIXBASE/no-split"
new_repo_fixture "$NS"
bank_grade_only "$NS"
case_run export-no-split "$NS/repo" - export --bank "$NS/repo/evals/bank" --out "$FIXBASE/state/x.jsonl"

case_run qualify-no-split "$NS/repo" - qualify --bank "$NS/repo/evals/bank"
case_run qualify-hp "$CQ/repo" - qualify --bank "$CQ/repo/evals/bank" --history "$ROWS"

## ── the native folds through the leaf ─────────────────────────────────────

# Ancient rows sit outside every stale window, so the fold's verdict is
# wall-clock independent.
AH="$FIXBASE/ancient"
mkdir -p "$AH"
{
  printf '{"ts": "2020-01-01T00:00:00Z", "task_id": "old", "tier": "regression", "pass": true, "reason": "", "duration_s": 1.0, "repeat_index": 0, "attempt_index": 0, "attempt_id": "a1", "run_id": "r1", "obs": {"fixture_prepared": true, "grader_ran": true, "grader_passed": true}, "bank_rev": "%s", "variant": "baseline"}\n' "$BANK_REV"
} > "$AH/history.jsonl"
case_run trend-ancient "$AH" - trend --history "$AH/history.jsonl"
case_run report-ancient "$AH" - report --history "$AH/history.jsonl"

# Macro over one quiet journal: no patterns to find.
MJ="$FIXBASE/macro"
mkdir -p "$MJ"
: > "$MJ/events.jsonl"
case_run macro-quiet "$MJ" - macro --events "$MJ/events.jsonl"

echo "captured $(ls "$GOLDENS" | grep -c '\.exit$') cases into $GOLDENS"
