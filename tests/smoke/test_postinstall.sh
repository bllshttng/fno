#!/usr/bin/env bash
# Validates that the plugin installer is wired through the SessionStart
# front-door hook (Claude Code never runs a plugin.json postInstall key), and
# that the installer exists and is executable. Does NOT run the installer (that
# would modify the user's PATH / Python env).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

test -f .claude-plugin/plugin.json || { echo "FAIL: .claude-plugin/plugin.json missing"; exit 1; }
test -f .claude-plugin/postinstall.sh || { echo "FAIL: .claude-plugin/postinstall.sh missing"; exit 1; }
test -x .claude-plugin/postinstall.sh || { echo "FAIL: .claude-plugin/postinstall.sh not executable"; exit 1; }

# Claude Code ignores these keys at load AND install time, so they wire nothing.
if grep -q '"postInstall"' .claude-plugin/plugin.json; then
  echo "FAIL: plugin.json carries postInstall, which Claude Code never runs; the installer is wired from hooks/frontdoor-nudge-session-start.sh"
  exit 1
fi
if grep -q '"policy"' .claude-plugin/marketplace.json; then
  echo "FAIL: .claude-plugin/marketplace.json carries policy, an unknown field to claude plugin validate --strict"
  exit 1
fi
grep -q ".claude-plugin/postinstall.sh" hooks/frontdoor-nudge-session-start.sh \
  || { echo "FAIL: hooks/frontdoor-nudge-session-start.sh does not start .claude-plugin/postinstall.sh"; exit 1; }
grep -q "frontdoor-nudge-session-start.sh" hooks/context-hooks.json \
  || { echo "FAIL: hooks/context-hooks.json does not wire frontdoor-nudge-session-start.sh"; exit 1; }
# A plugin-only install has no fno-agents, so context-run.sh must reach the hook without it.
grep -q "frontdoor-nudge-session-start.sh" hooks/context-run.sh \
  || { echo "FAIL: hooks/context-run.sh does not run the front-door hook when fno-agents is missing"; exit 1; }

if claude plugin validate --help >/dev/null 2>&1; then
  claude plugin validate .claude-plugin/marketplace.json --strict >/dev/null 2>&1 \
    || { echo "FAIL: marketplace validation exits nonzero"; exit 1; }
else
  echo "SKIP: claude CLI absent, strict manifest validation not run"
fi


# The installer lands the release binaries: the tarball fetch, its sha256
# check, the staged front door's --adopt, and fno.sh as the fallback. No uv,
# pip or Python path remains.
for needle in \
  "try_binary_install" \
  "fno-agents-\${plat}.tar.gz" \
  "sha256 mismatch" \
  "update --adopt" \
  "scripts/install/fno.sh" \
  "plugin.json" \
  "plugin_channel"; do
  grep -qF "$needle" .claude-plugin/postinstall.sh \
    || { echo "FAIL: postinstall.sh missing expected content: $needle"; exit 1; }
done
for absent in "uv tool install" "pip install" "fno-py" "cli/pyproject.toml"; do
  if grep -qF "$absent" .claude-plugin/postinstall.sh; then
    echo "FAIL: postinstall.sh still names the Python install path: $absent"
    exit 1
  fi
done

# Syntax check: a broken postinstall silently no-ops the plugin install.
bash -n .claude-plugin/postinstall.sh \
  || { echo "FAIL: postinstall.sh has a syntax error"; exit 1; }

echo "PASS: installer wired from the SessionStart front-door hook, executable, and installs the release binaries"
