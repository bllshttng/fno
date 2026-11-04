#!/usr/bin/env bash
# fno hook: PreCompact and SessionEnd - save a readable session transcript.
# Policy lives in crates/fno-agents/src/hook/save_session.rs. A machine with no
# fno-agents saves nothing; a hook here never blocks.
command -v fno-agents >/dev/null 2>&1 || exit 0
exec fno-agents hook save-session
