# Fleet cutover

Use `fno agents incident cutover` to move the fleet from one computer to another. It keeps the backlog, the event provenance and the history.

It rides the `incident` verb because law d-fe66560a allows no new top-level verb. The Rust entry is `crates/fno-agents/src/cutover.rs`, reached as an argument of `fleet-incident`.

## Save, on the source

```
fno agents incident cutover save [--out <dir>] [--idle-timeout <secs>] [--ignore <agent>]...
```

Run it from a plain terminal. If you run it from a fleet session, pass that session's name to `--ignore`. A session that is mid-reply blocks the wait.

The verb does these steps in order:

1. It arms the fleet incident with every scope held (spawns, tests, merges, loops).
2. It waits until no registry row is `Spawning`, `Busy` or `Restarting`. The default bound is 600 seconds. At the bound it refuses and leaves the incident armed.
3. It stops the daemon with SIGTERM, then SIGKILL after the grace. If the daemon survives, the verb refuses.
4. It copies every SQLite store under the state root with the SQLite backup API. The walk skips `cargo-build/` and `cutover/` and follows no symlink. It takes only files whose header reads as SQLite. An empty file such as the `graph.db` path anchor is not a store.
5. For each copy it runs `PRAGMA integrity_check` and compares per-table row counts with the live store. A mismatch refuses the whole save.
6. It checks that no daemon took the lock back during the copy.

The bundle defaults to `<state root>/cutover/<stamp>/`. It holds `manifest.json` and `stores/<path under the state root>`. The receipt names each store, its backup path and its counts, then prints the target steps.

The source is read-only after a save. The incident stays armed, and a daemon that starts again reads the armed record and holds its sweeps. Until a shared store exists, do not clear it on the source.

## Restore, on the target

1. Copy the rest of the state root: `rsync -a --exclude '/cargo-build/' <root>/ <target>:<root>/`.
2. Run `fno agents incident cutover restore <bundle>`.
3. Run `fno agents restart` to start the daemon.
4. Run `fno agents incident clear --reason 'cutover complete'`.
5. Run `fno mux workspace restore`.

State keys on absolute paths. Restore refuses a bundle whose state root or user differs from the target's.

Restore checks every backup against the manifest before it changes anything. It moves each existing store aside as `<store>.pre-cutover-<stamp>` and never deletes it. It then installs the copies and checks the counts again.

## Not covered

- Mux servers, store keepers, the Codex app-server and pr-watch are not stopped by `save`. Their stores are SQLite files, so the online backup stays consistent. Their writers can still run until the daemon and sessions are down.
- Session processes on the source are not killed. After `save`, stop them yourself so nothing wakes and writes again.
