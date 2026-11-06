#!/usr/bin/env bash
# test-setup-worktree-canonical-guard.sh - the same-root refusal in
# scripts/setup/setup-worktree.sh - guards against the canonical-to-canonical
# self-destruction where link_artifact `rm -f`s a real file and replaces it
# with a symlink pointing at itself.
#
# The guard is load-bearing against real data loss, not untidiness:
# link_artifact `rm -f`s the real file before `ln -sf "$source" "$target"`,
# so with CANONICAL == WORKTREE it deletes the file and replaces it with a
# symlink pointing at itself. That is how this repo's .fno/codemap.md was
# lost on 2026-07-26. The script had no coverage until this file.
#
# Tests:
#  1. Same root: exits 0, names the root, and .fno/ is untouched (still a real
#     file with its original bytes, not a symlink).
#  2. Same root reached through a symlinked path: still refuses. `-ef` compares
#     device+inode, which a string equality test on the two roots would miss.
#  3. Distinct roots: links as it does today, so the guard changes nothing for
#     the real invocation.
#  4. A WORKTREE that does not exist: refuses non-zero and creates nothing.
#     The script's `mkdir -p` used to build the whole path, so a caller naming
#     a location nobody made got a fully linked directory conjured at it. On
#     2026-09-03 nine of those sat under <repo>/worktrees/, a location
#     .claude/rules/worktrees.md forbids. The assertion that matters is the
#     absence of the directory afterwards, not the exit code alone.
#
# Bash 3.2 compatible; hermetic mktemp sandboxes.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SETUP="$REPO_ROOT/scripts/setup/setup-worktree.sh"

pass=0
fail=0

check_eq() {
  local desc="$1" expected="$2" actual="$3"
  if [ "$expected" = "$actual" ]; then
    echo "PASS: $desc"
    pass=$((pass+1))
  else
    echo "FAIL: $desc (expected='$expected' actual='$actual')"
    fail=$((fail+1))
  fi
}

check_contains() {
  local desc="$1" needle="$2" haystack="$3"
  if printf '%s' "$haystack" | grep -qF "$needle"; then
    echo "PASS: $desc"
    pass=$((pass+1))
  else
    echo "FAIL: $desc (needle='$needle' not found in: $haystack)"
    fail=$((fail+1))
  fi
}

# A canonical checkout carrying two real files: codemap.md, the file the
# 2026-07-26 self-link destroyed, and config.toml, which link_file skips.
make_canonical() {
  local root="$1"
  mkdir -p "$root/.fno" "$root/.claude"
  printf 'CODEMAP-ORIGINAL\n' > "$root/.fno/codemap.md"
  printf 'CONFIG-ORIGINAL\n' > "$root/.fno/config.toml"
}

# ---------------------------------------------------------------------------
# T1: same root - refuse, exit 0, name the root, touch nothing
# ---------------------------------------------------------------------------
SBX1="$(mktemp -d)"
make_canonical "$SBX1"
OUT1="$(CANONICAL="$SBX1" WORKTREE="$SBX1" bash "$SETUP" 2>&1)"
RC1=$?

check_eq "T1: exits 0 (a no-op is a success, and target start treats non-zero as fatal)" "0" "$RC1"
check_contains "T1: refusal names the root" "$SBX1" "$OUT1"
check_contains "T1: refusal says what it declined" "canonical -> canonical" "$OUT1"
check_eq "T1: codemap.md survives with its original bytes" \
  "CODEMAP-ORIGINAL" "$(cat "$SBX1/.fno/codemap.md" 2>/dev/null)"
if [ -L "$SBX1/.fno/codemap.md" ]; then
  echo "FAIL: T1: codemap.md became a symlink (the 2026-07-26 self-link bug)"
  fail=$((fail+1))
else
  echo "PASS: T1: codemap.md is still a real file, not a self-referential symlink"
  pass=$((pass+1))
fi
check_eq "T1: config.toml survives" \
  "CONFIG-ORIGINAL" "$(cat "$SBX1/.fno/config.toml" 2>/dev/null)"

# ---------------------------------------------------------------------------
# T2: same root via a symlinked path - inode comparison, not string equality
# ---------------------------------------------------------------------------
SBX2="$(mktemp -d)"
make_canonical "$SBX2/real"
ln -s "$SBX2/real" "$SBX2/alias"
OUT2="$(CANONICAL="$SBX2/real" WORKTREE="$SBX2/alias" bash "$SETUP" 2>&1)"
RC2=$?

check_eq "T2: exits 0 through the alias" "0" "$RC2"
check_contains "T2: refuses despite the two roots spelling differently" \
  "canonical -> canonical" "$OUT2"
check_eq "T2: codemap.md survives the aliased invocation" \
  "CODEMAP-ORIGINAL" "$(cat "$SBX2/real/.fno/codemap.md" 2>/dev/null)"

# ---------------------------------------------------------------------------
# T3: distinct roots - the guard changes nothing for the real invocation
# ---------------------------------------------------------------------------
SBX3="$(mktemp -d)"
make_canonical "$SBX3/canonical"
mkdir -p "$SBX3/worktree"
OUT3="$(CANONICAL="$SBX3/canonical" WORKTREE="$SBX3/worktree" bash "$SETUP" 2>&1)"
RC3=$?

check_eq "T3: exits 0 on a genuine worktree" "0" "$RC3"
# State links are retired: project state lives in the repo's space under
# ~/.fno/spaces/<slug>/, keyed on the canonical root, so every worktree
# resolves it without a link. .fno/ gets NOTHING linked now (config.toml is
# found by the config loader's climb to canonical; the only link into a
# checkout left is internal/ and .claude/).
if [ -L "$SBX3/worktree/.fno" ] || [ -L "$SBX3/worktree/.fno/codemap.md" ] || [ -L "$SBX3/worktree/.fno/config.toml" ]; then
  echo "FAIL: T3: an .fno state link was created (retired in x-b1ee): $OUT3"
  fail=$((fail+1))
else
  echo "PASS: T3: no .fno state link (project state resolves through the space)"
  pass=$((pass+1))
fi
check_eq "T3: canonical codemap is untouched" \
  "CODEMAP-ORIGINAL" "$(cat "$SBX3/canonical/.fno/codemap.md" 2>/dev/null)"
check_eq "T3: canonical config.toml is untouched" \
  "CONFIG-ORIGINAL" "$(cat "$SBX3/canonical/.fno/config.toml" 2>/dev/null)"

# ---------------------------------------------------------------------------
# T4: a WORKTREE nobody created - refuse, and leave the path absent
# ---------------------------------------------------------------------------
SBX4="$(mktemp -d)"
make_canonical "$SBX4/canonical"
ABSENT="$SBX4/canonical/worktrees/target-ab-aaaa1111"
OUT4="$(CANONICAL="$SBX4/canonical" WORKTREE="$ABSENT" bash "$SETUP" 2>&1)"
RC4=$?

check_eq "T4: exits non-zero on a worktree that does not exist" "1" "$RC4"
check_contains "T4: refusal names the missing path" "$ABSENT" "$OUT4"
check_contains "T4: refusal says how to fix it" "git worktree add" "$OUT4"
if [ -d "$ABSENT" ]; then
  echo "FAIL: T4: the refused path was created anyway (the litter bug)"
  fail=$((fail+1))
else
  echo "PASS: T4: nothing was created at the refused path"
  pass=$((pass+1))
fi
if [ -d "$SBX4/canonical/worktrees" ]; then
  echo "FAIL: T4: the parent worktrees/ dir was created anyway"
  fail=$((fail+1))
else
  echo "PASS: T4: no parent worktrees/ dir either"
  pass=$((pass+1))
fi
check_eq "T4: canonical is untouched" \
  "CODEMAP-ORIGINAL" "$(cat "$SBX4/canonical/.fno/codemap.md" 2>/dev/null)"

# ---------------------------------------------------------------------------
# T5: a WORKTREE inside the canonical checkout - refuse, create nothing.
# The 2026-10-30 litter: a run from crates/fno-agents left .agents, .codex,
# .codex-plugin and .claude/ symlinks inside the crate, because CANONICAL
# still resolves to the repo root from any cwd inside the checkout while the
# destination defaulted to $(pwd). The same-root -ef guard cannot see a
# SUBDIRECTORY of the canonical checkout; the show-toplevel guard does.
# ---------------------------------------------------------------------------
SBX5="$(mktemp -d)"
make_canonical "$SBX5/canonical"
/usr/bin/git -C "$SBX5/canonical" init -q
printf 'LOCAL-PERMS\n' > "$SBX5/canonical/.claude/settings.local.json"
CRATE="$SBX5/canonical/crates/fno-agents"
mkdir -p "$CRATE"
OUT5="$(CANONICAL="$SBX5/canonical" WORKTREE="$CRATE" bash "$SETUP" 2>&1)"
RC5=$?

check_eq "T5: exits non-zero on a destination inside the canonical checkout" "1" "$RC5"
check_contains "T5: refusal names the destination" "$CRATE" "$OUT5"
check_contains "T5: refusal says what it declined" "canonical checkout itself" "$OUT5"
if [ -e "$CRATE/.claude" ]; then
  echo "FAIL: T5: a .claude dir was created inside the crate (the litter bug)"
  fail=$((fail+1))
else
  echo "PASS: T5: nothing was created inside the crate"
  pass=$((pass+1))
fi
if [ -L "$CRATE/.agents" ] || [ -L "$CRATE/.codex" ] || [ -L "$CRATE/.codex-plugin" ]; then
  echo "FAIL: T5: harness-dir symlinks landed inside the crate"
  fail=$((fail+1))
else
  echo "PASS: T5: no .agents/.codex/.codex-plugin links inside the crate"
  pass=$((pass+1))
fi
if [ -L "$SBX5/canonical/.claude/settings.local.json" ]; then
  echo "FAIL: T5: canonical settings.local.json became a symlink"
  fail=$((fail+1))
else
  echo "PASS: T5: canonical settings.local.json is still a real file"
  pass=$((pass+1))
fi

# ---------------------------------------------------------------------------
# T6: a REAL git worktree still links. The T5 guard reads show-toplevel, so
# pin the genuine invocation: a linked worktree's toplevel is itself, never
# the canonical root. PATH is stripped to system dirs so an optional
# codegraph binary cannot start a background index against the sandbox.
# ---------------------------------------------------------------------------
SBX6="$(mktemp -d)"
make_canonical "$SBX6/canonical"
/usr/bin/git -C "$SBX6/canonical" init -q
/usr/bin/git -C "$SBX6/canonical" -c user.email=test@test -c user.name=test commit -q --allow-empty -m seed
/usr/bin/git -C "$SBX6/canonical" worktree add -q -b side "$SBX6/wt"
printf 'LOCAL-PERMS\n' > "$SBX6/canonical/.claude/settings.local.json"
OUT6="$(PATH="/usr/bin:/bin:/usr/sbin:/sbin" CANONICAL="$SBX6/canonical" WORKTREE="$SBX6/wt" bash "$SETUP" 2>&1)"
RC6=$?

check_eq "T6: exits 0 on a genuine git worktree" "0" "$RC6"
if [ -L "$SBX6/wt/.claude/settings.local.json" ]; then
  echo "PASS: T6: the shared settings link landed in the worktree"
  pass=$((pass+1))
else
  echo "FAIL: T6: the shared settings link did not land: $OUT6"
  fail=$((fail+1))
fi
if [ -L "$SBX6/canonical/.claude/settings.local.json" ]; then
  echo "FAIL: T6: the guard over-fired and symlinked the canonical root"
  fail=$((fail+1))
else
  echo "PASS: T6: the canonical root itself keeps its real file"
  pass=$((pass+1))
fi

# ---------------------------------------------------------------------------
# T7: a REAL worktree placed INSIDE the canonical tree still links. The
# claude lane's own placement is .claude/worktrees/<name>, a real checkout
# sitting inside the canonical directory tree. Its show-toplevel is itself,
# so the T5 guard must not fire on it. probed live 2026-11-06: macOS answers
# /private/tmp for a /tmp path, and -ef absorbs the spelling difference.
# ---------------------------------------------------------------------------
SBX7="$(mktemp -d)"
make_canonical "$SBX7/canonical"
/usr/bin/git -C "$SBX7/canonical" init -q
/usr/bin/git -C "$SBX7/canonical" -c user.email=test@test -c user.name=test commit -q --allow-empty -m seed
mkdir -p "$SBX7/canonical/.claude/worktrees"
/usr/bin/git -C "$SBX7/canonical" worktree add -q -b w1 "$SBX7/canonical/.claude/worktrees/w1"
printf 'LOCAL-PERMS\n' > "$SBX7/canonical/.claude/settings.local.json"
OUT7="$(PATH="/usr/bin:/bin:/usr/sbin:/sbin" CANONICAL="$SBX7/canonical" WORKTREE="$SBX7/canonical/.claude/worktrees/w1" bash "$SETUP" 2>&1)"
RC7=$?

check_eq "T7: exits 0 on a worktree inside the canonical tree" "0" "$RC7"
if [ -L "$SBX7/canonical/.claude/worktrees/w1/.claude/settings.local.json" ]; then
  echo "PASS: T7: the shared settings link landed in the nested worktree"
  pass=$((pass+1))
else
  echo "FAIL: T7: the nested-worktree link did not land: $OUT7"
  fail=$((fail+1))
fi
if [ -L "$SBX7/canonical/.claude/settings.local.json" ]; then
  echo "FAIL: T7: the guard over-fired and symlinked the canonical root"
  fail=$((fail+1))
else
  echo "PASS: T7: the canonical root itself keeps its real file"
  pass=$((pass+1))
fi

rm -rf "$SBX1" "$SBX2" "$SBX3" "$SBX4" "$SBX5" "$SBX6" "$SBX7"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
echo ""
echo "Results: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
