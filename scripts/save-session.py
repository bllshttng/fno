"""Tombstone stub: the save-session hook runs `fno-agents hook save-session`.

A session started before that change still names this path in its hook row.
The row launches it through hooks/lib/run-python.sh, now a stub that exits 0,
so nothing runs this file. It stays one release so the path resolves; delete
it in a later release (`fno doctor lint hook-tombstones`).
"""
