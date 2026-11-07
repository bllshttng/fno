#!/usr/bin/env bash
# fno hook: agy PreToolUse - lead delegation guard (tombstone)
# Superseded by agy-pretool.sh, which runs the full claude PreToolUse block
# guard chain through the pretool host. This stub execs it, so a session that
# cached the old registration still meets every guard; delete it one release
# after the install switch (the shim tombstone rule, docs/architecture/
# multi-cli-hooks.md).
exec "$(dirname "${BASH_SOURCE[0]}")/agy-pretool.sh" "$@"
