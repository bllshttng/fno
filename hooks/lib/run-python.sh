#!/usr/bin/env bash
# Tombstone stub: the save-session hook moved to hooks/save-session.sh, which
# runs `fno-agents hook save-session`. A session started before that change
# still holds a PreCompact/SessionEnd row that launches this path, so it stays
# one release as a no-op that exits 0 and starts nothing. Delete it in a later
# release; `fno doctor lint hook-tombstones` enforces the two steps.
exit 0
