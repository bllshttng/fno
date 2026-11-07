#!/usr/bin/env bash
# ambient-canary.sh - the dirty lane's positive control.
#
# The env builder deliberately leaves AMBIENT_LEAK_CANARY unscrubbed in the
# dirty lane. In that lane this canary's failure is EXPECTED: it proves the
# lane can detect a channel the ambient surface does not cover. In the clean
# lane a failure means something poisons the environment outside
# `fno doctor test smoke --ambient dirty`.
set -uo pipefail

if [ -n "${AMBIENT_LEAK_CANARY+set}" ]; then
  echo "AMBIENT_LEAK_CANARY=${AMBIENT_LEAK_CANARY} reached a test process." >&2
  exit 1
fi
echo "ambient canary: no leak"
