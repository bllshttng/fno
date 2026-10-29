#!/usr/bin/env bash
# context-probe.sh - shim over the CLI's single context-window implementation.
#
# Usage: context-probe.sh <transcript-jsonl-path>
#
# Output (exit 0): one JSON line, identical field-for-field to `fno whoami context --json`:
#   {"used_tokens": N, "window_tokens": N, "used_pct": N, "model": "..."}
#
# Exit 3 ("unreadable") on ANY failure, including no native context reader.
# Every caller treats nonzero as "no pressure" (fail-safe), so a missing binary
# degrades toward silence rather than a false handoff.
#
# This stays a shim rather than being deleted because
# skills/target/scripts/handoff.sh resolves it as "$_SCRIPT_DIR/context-probe.sh"
# and that resolution is what skill self-containment requires of a bundled skill.
# The token math and the model->window policy live ONCE in
# crates/fno-agents/src/context_window.rs. The Python CLI is a transport bridge.
# tests/test-context-probe.sh exercises the shell shim's real CLI path.
_EXIT_UNREADABLE=3

if [ $# -lt 1 ] || [ -z "$1" ]; then
  exit "$_EXIT_UNREADABLE"
fi

# `context` claims natively now: the front door owns both spellings
# (classify_context) and execs fno-agents context-run, so reach it through
# `fno` and exit unreadable when no binary exists. The old `fno-py` first
# preference died with the Python leg: a wheel without the command answers
# nothing, and a python registration would sit behind the native intercept.
if command -v fno >/dev/null 2>&1; then
  _ctx_door=fno
else
  exit "$_EXIT_UNREADABLE"
fi

# `<door> context` exits 3 on unreadable; normalize ANY nonzero (incl. a
# transient error) to exit 3 so the shim's failure contract is identical to the
# old shell probe's. stdout passes through verbatim on success.
_out="$("$_ctx_door" whoami context --transcript "$1" --json 2>/dev/null)"
_rc=$?
if [ "$_rc" -ne 0 ]; then
  exit "$_EXIT_UNREADABLE"
fi
printf '%s\n' "$_out"
