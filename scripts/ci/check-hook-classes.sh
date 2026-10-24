#!/usr/bin/env bash
# check-hook-classes.sh - CI gate: every registered hook declares its class.
#
# A hook is one of four classes (docs/hooks-fail-open.md, "Four hook classes"): block,
# wait, next-turn, rewake. hooks/hook-classes.tsv holds one row per
# harness + event + script. This gate fails when a registration has no row,
# a row has no registration, or the manifest flags disagree with the class:
# a Claude "async": true entry must be next-turn, "asyncRewake": true must be
# rewake, a sync entry must be block or wait, and a block row must name its
# fail mode. Codex has no async flag, so a Codex row is block or wait.
#
# Run: bash scripts/ci/check-hook-classes.sh [--self-test]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

check() {
  python3 - "$@" <<'PY'
import json, re, sys

table_path, claude_path, codex_path = sys.argv[1:4]
CLASSES = {"block", "wait", "next-turn", "rewake"}

rows = {}
errors = []
for n, line in enumerate(open(table_path), 1):
    line = line.rstrip("\n")
    if not line or line.startswith("#"):
        continue
    cols = line.split("\t")
    if len(cols) != 5:
        errors.append(f"{table_path}:{n}: want 5 tab-separated columns, got {len(cols)}")
        continue
    harness, event, script, klass, fail = cols
    key = (harness, event, script)
    if key in rows:
        errors.append(f"{table_path}:{n}: duplicate row {key}")
    if klass not in CLASSES:
        errors.append(f"{table_path}:{n}: class '{klass}' is not one of {sorted(CLASSES)}")
    if klass == "block" and fail not in ("open", "closed"):
        errors.append(f"{table_path}:{n}: a block row names its fail mode (open or closed), got '{fail}'")
    if klass != "block" and fail != "-":
        errors.append(f"{table_path}:{n}: only a block row names a fail mode; write '-'")
    rows[key] = klass

seen = set()
for harness, path in (("claude", claude_path), ("codex", codex_path)):
    manifest = json.load(open(path))
    for event, groups in manifest.get("hooks", {}).items():
        for group in groups:
            for hook in group.get("hooks", []):
                m = re.findall(r'/((?:hooks|scripts)/[A-Za-z0-9._/-]+\.(?:sh|py))', hook.get("command", ""))
                if not m:
                    errors.append(f"{path}: {event} command names no hook script: {hook.get('command', '')[:60]}")
                    continue
                script = m[0].rsplit("/", 1)[-1]
                key = (harness, event, script)
                seen.add(key)
                klass = rows.get(key)
                if klass is None:
                    errors.append(f"{path}: {harness} {event} {script} has no row in {table_path}; declare its class")
                    continue
                if hook.get("asyncRewake"):
                    want = {"rewake"}
                elif hook.get("async"):
                    want = {"next-turn"}
                else:
                    want = {"block", "wait"}
                if klass not in want:
                    errors.append(f"{path}: {harness} {event} {script} is declared {klass}, but its manifest entry runs as {'/'.join(sorted(want))}")

for key in sorted(set(rows) - seen):
    errors.append(f"{table_path}: row {key} has no registration; remove it")

for e in errors:
    print(f"FAIL {e}", file=sys.stderr)
if errors:
    sys.exit(1)
print(f"check-hook-classes: ok ({len(rows)} declared registrations)")
PY
}

if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d -t hook-classes-XXXXXX)"
  trap 'rm -rf "$tmp"' EXIT
  printf '{"hooks":{"PostToolUse":[{"matcher":"Skill","hooks":[{"type":"command","command":"bash \\"${R}/hooks/a.sh\\"","async":true}]}],"PreToolUse":[{"hooks":[{"type":"command","command":"\\"${R}/hooks/g.sh\\""}]}]}}' > "$tmp/claude.json"
  printf '{"hooks":{}}' > "$tmp/codex.json"
  printf 'claude\tPostToolUse\ta.sh\tnext-turn\t-\nclaude\tPreToolUse\tg.sh\tblock\tclosed\n' > "$tmp/good.tsv"
  check "$tmp/good.tsv" "$tmp/claude.json" "$tmp/codex.json" >/dev/null \
    || { echo "check-hook-classes: self-test: a matching table must pass" >&2; exit 1; }
  printf 'claude\tPostToolUse\ta.sh\twait\t-\nclaude\tPreToolUse\tg.sh\tblock\tclosed\n' > "$tmp/sync.tsv"
  if check "$tmp/sync.tsv" "$tmp/claude.json" "$tmp/codex.json" 2>/dev/null; then
    echo "check-hook-classes: self-test: an async entry declared wait must refuse" >&2; exit 1
  fi
  printf 'claude\tPostToolUse\ta.sh\tnext-turn\t-\n' > "$tmp/missing.tsv"
  if check "$tmp/missing.tsv" "$tmp/claude.json" "$tmp/codex.json" 2>/dev/null; then
    echo "check-hook-classes: self-test: an undeclared hook must refuse" >&2; exit 1
  fi
  printf 'claude\tPostToolUse\ta.sh\tnext-turn\t-\nclaude\tPreToolUse\tg.sh\tblock\t-\n' > "$tmp/nofail.tsv"
  if check "$tmp/nofail.tsv" "$tmp/claude.json" "$tmp/codex.json" 2>/dev/null; then
    echo "check-hook-classes: self-test: a block row with no fail mode must refuse" >&2; exit 1
  fi
  echo "check-hook-classes: self-test ok"
  exit 0
fi

check hooks/hook-classes.tsv hooks/hooks.json hooks/codex-hooks.json
