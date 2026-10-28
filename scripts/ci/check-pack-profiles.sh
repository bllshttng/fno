#!/usr/bin/env bash
# check-pack-profiles.sh - CI gate: every hook entry picks an install profile.
#
# pack-profiles.json splits the plugin into the pack (runs in any harness with
# no mux) and the mux (mail, panes, the daemon registry). A pack install
# (fno config plugin install <harness> --pack) drops every mux path and every
# hook entry that runs a mux script. This gate fails when a hook script that
# hooks/hooks.json, hooks/codex-hooks.json or hooks/context-hooks.json runs
# sits in neither mux nor pack_hooks, when a script sits in both, and when a
# manifest path does not exist. Skills, agents and commands default to pack,
# so they count as classified.
#
# Run: bash scripts/ci/check-pack-profiles.sh [--self-test]
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

check() {
  python3 - "$@" <<'PY'
import json, os, re, sys

root, manifest_path = sys.argv[1:3]
hook_files = ["hooks/hooks.json", "hooks/codex-hooks.json", "hooks/context-hooks.json"]
SCRIPT = re.compile(r'((?:hooks|scripts)/[A-Za-z0-9._/-]+\.(?:sh|py))')

manifest = json.load(open(os.path.join(root, manifest_path)))
mux = manifest.get("mux", [])
pack_hooks = manifest.get("pack_hooks", [])
errors = []
for path in mux + pack_hooks:
    if not os.path.exists(os.path.join(root, path)):
        errors.append(f"{manifest_path}: {path} does not exist; remove it or restore the file")
for path in sorted(set(mux) & set(pack_hooks)):
    errors.append(f"{manifest_path}: {path} sits in both mux and pack_hooks; pick one")
classified = set(mux) | set(pack_hooks)

def commands(node):
    if isinstance(node, dict):
        if isinstance(node.get("command"), str):
            yield node["command"]
        if isinstance(node.get("argv"), list) and node["argv"]:
            yield str(node["argv"][0])
        for value in node.values():
            yield from commands(value)
    elif isinstance(node, list):
        for value in node:
            yield from commands(value)

entries = 0
unclassified = set()
for rel in hook_files:
    full = os.path.join(root, rel)
    if not os.path.exists(full):
        continue
    for command in commands(json.load(open(full))):
        found = SCRIPT.findall(command)
        if not found:
            errors.append(f"{rel}: a hook entry names no hook script: {command[:60]}")
            continue
        entries += 1
        if found[0] not in classified:
            unclassified.add((rel, found[0]))
for rel, script in sorted(unclassified):
    errors.append(f"{rel}: {script} is unclassified; add it to mux or pack_hooks in {manifest_path}")

files = 0
for top in ("skills", "agents", "commands"):
    base = os.path.join(root, top)
    if os.path.isdir(base):
        files += sum(1 for name in os.listdir(base) if not name.startswith("."))

for e in errors:
    print(f"FAIL {e}", file=sys.stderr)
print(f"pack-profiles: {entries + files} classified, {len(unclassified)} unclassified")
sys.exit(1 if errors else 0)
PY
}

if [[ "${1:-}" == "--self-test" ]]; then
  tmp="$(mktemp -d -t pack-profiles-XXXXXX)"
  trap 'rm -rf "$tmp"' EXIT
  mkdir -p "$tmp/hooks" "$tmp/skills/mail"
  : > "$tmp/hooks/a.sh"
  : > "$tmp/hooks/m.sh"
  printf '{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"\\"${R}/hooks/a.sh\\""},{"type":"command","command":"\\"${R}/hooks/m.sh\\""}]}]}}' > "$tmp/hooks/hooks.json"
  printf '{"mux":["hooks/m.sh","skills/mail"],"pack_hooks":["hooks/a.sh"]}' > "$tmp/good.json"
  check "$tmp" good.json >/dev/null \
    || { echo "check-pack-profiles: self-test: a full manifest must pass" >&2; exit 1; }
  printf '{"mux":["hooks/m.sh"],"pack_hooks":[]}' > "$tmp/missing.json"
  if check "$tmp" missing.json >/dev/null 2>&1; then
    echo "check-pack-profiles: self-test: an unclassified hook must refuse" >&2; exit 1
  fi
  printf '{"mux":["hooks/m.sh","skills/gone"],"pack_hooks":["hooks/a.sh"]}' > "$tmp/gone.json"
  if check "$tmp" gone.json >/dev/null 2>&1; then
    echo "check-pack-profiles: self-test: a manifest path that does not exist must refuse" >&2; exit 1
  fi
  printf '{"mux":["hooks/m.sh"],"pack_hooks":["hooks/a.sh","hooks/m.sh"]}' > "$tmp/both.json"
  if check "$tmp" both.json >/dev/null 2>&1; then
    echo "check-pack-profiles: self-test: a script in both lists must refuse" >&2; exit 1
  fi
  echo "check-pack-profiles: self-test ok"
  exit 0
fi

check . pack-profiles.json
