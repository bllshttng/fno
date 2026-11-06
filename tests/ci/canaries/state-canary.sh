#!/usr/bin/env bash
# state-canary.sh - the state lane's positive control.
#
# The populated lane seeds a profile whose graph holds a node named
# STATE_LEAK_CANARY. This canary copies that profile into the step's home the
# way a leak would, then fails when the name is readable under ~/.fno. In the
# populated lane the failure is EXPECTED: it proves the lane can see a leak.
# In the clean lane a failure means the sandbox was populated unexpectedly.
set -uo pipefail

if [ "${STATE_LANE:-}" = populated ] && [ -n "${STATE_PROFILE_DIR:-}" ]; then
  mkdir -p "$HOME/.fno"
  cp -R "$STATE_PROFILE_DIR"/. "$HOME/.fno/"
fi
if [ -d "$HOME/.fno" ] && grep -rqa STATE_LEAK_CANARY "$HOME/.fno"; then
  echo "STATE_LEAK_CANARY reached a test process (expected only in the populated lane)." >&2
  exit 1
fi
echo "state canary: no leak"
