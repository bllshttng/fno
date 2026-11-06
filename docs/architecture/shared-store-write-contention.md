# Shared-store write contention

The shared primary serializes writes. A slow replication transaction can delay a claim statement even while read-only requests still answer. A responsive HTTP endpoint does not establish write health.

## Replicated updates

Local triggers capture before and after row images. `backlog_share::narrow_updates` retains changed columns and primary-key columns. The key values identify the row for compare-and-swap. An assignment is needed only for a changed value.

The conditional writer assigns only values that changed. Its WHERE clause retains the complete narrowed pre-image, including unchanged key values, with `IS` comparisons so NULL pre-images remain guarded. Genuine key changes remain assignments. The existing net-change fold removes updates whose before and after images are identical.

When an update assigns an unchanged primary key, SQLite can check referencing tables. On the measured primary, an `agent_sessions` update that assigned `id` planned repeated scans of related tables. Omitting the unchanged identity assignment left one indexed lookup. An isolated SQLite fixture with 60,000 referencing rows performed about 480,000 VM steps with the identity assignment. This establishes unnecessary work. It does not establish the cause of an intermittent server stall.

## The stall: sqld gives the write lock to a closed connection

The recurring stall is a server defect in sqld 0.24.33, in `libsql-server/src/connection/connection_manager.rs`. The upstream report is [libsql issue 2286](https://github.com/tursodatabase/libsql/issues/2286).

A waiter joins `write_queue`, then can return `SQLITE_BUSY` while another connection holds the slot for a checkpoint. Its queue entry stays behind. Later `schedule_next` pops that entry and marks the slot `Notified` for a connection that closed. Nothing moves a `Notified` slot on, because lock stealing applies only to `Acquired`. Each later writer then loops on a `park_deadline` that is already in the past.

The signature on r2d2:

- Writes stop, and the `data` and `wal` file times stop with them. Reads keep working.
- `BEGIN IMMEDIATE` and every claim write time out at the 15 s reply bound.
- Load climbs without limit, because each waiting writer adds one spinning thread.
- After hours, sqld can abort with `Thread count overflowed the configured max count`. On 2026-10-31 that came 3 hours 9 minutes after writes stopped.

Backlog notes appear to pass during a stall because they commit to the local database. The daemon flushes them later. Claim writes go to the primary and fail closed, so only they hang.

The log line `failed to rollback: cannot rollback - no transaction is active` often comes before a stall. It also appears many times with no stall, so it is not a stall detector.

### Reproduction

Run the deployed image in a scratch container, with its data on a tmpfs. Send 40 clients that each upsert one row on a new `/v2/pipeline` stream, plus 2 bulk writers that grow the WAL. Without the mitigation, writes stopped at about 18 s in one run and about 230 s in another. Reads stayed healthy, and 44 to 81 threads were in state R at one full CPU.

### Mitigation

Set `SQLD_CHECKPOINT_INTERVAL_S`. This turns off the frame-count auto-checkpoint, which removes the common trigger. With a value of 60 under the same load, a 15-minute run landed 427,129 writes with no stall, and all 175 lock probes passed. About 0.2% of writes returned an error under that load, mostly `database is locked`. This is not a proof that every path to the defect is gone. Only an upstream fix does that.

The setting needs a container recreate. It is not live until the container's environment shows it.

## The abort: compaction cannot swap the wallog

A second sqld defect aborts the primary at log compaction. When the wallog passes its size cap, sqld writes an empty replacement log in `to_compact/` and swaps the two names with `renameat2(RENAME_EXCHANGE)`. A filesystem that does not implement that flag answers EINVAL, and sqld unwraps the error, so the process aborts and the container manager restarts it within seconds. Each restart fails every claim write in flight and resets the unchanged-container-start window. The upstream site is `libsql-server/src/replication/primary/logger.rs` line 398 at tag `libsql-server-v0.24.33`, with the flag call at lines 472 to 490. Upstream main still carries both. The upstream report is [libsql issue 1902](https://github.com/tursodatabase/libsql/issues/1902), open.

The signature on r2d2:

- The log shows `performing log compaction`, then a panic: `failed to perform snapshot file swap ... -> iku.db/dbs/default/wallog`.
- Aborts were observed 2026-10-28 13:49Z, 2026-10-29 03:20Z and 2026-10-30 20:07Z, each a restart of about 1.5 seconds.
- A soak that needs one unchanged container start cannot outlast a compaction.

Btrfs gained `RENAME_EXCHANGE` in kernel 4.7, and r2d2 runs 4.4.302+ with the data directory on btrfs. Measured on the primary's data directory on 2026-11-06 with a two-file `renameat2` probe inside the container runtime. On btrfs the probe answered `-1 EINVAL`. On tmpfs the same probe answered `0`.

### Check before you start sqld

Run the probe against the directory you plan to bind to `/var/lib/sqld`. It prints `renameat2 -> 0` on a filesystem that can swap. Anything else rules that directory out for sqld:

```bash
docker run --rm -v /path/to/sqld-data:/d python:3.12-alpine python -c "import ctypes,os;open('/d/.x1','w');open('/d/.x2','w');l=ctypes.CDLL(None,use_errno=True);r=l.renameat2(-100,b'/d/.x1',-100,b'/d/.x2',2);print('renameat2 ->',r,os.strerror(ctypes.get_errno()));os.remove('/d/.x1');os.remove('/d/.x2')"
```

ext4, tmpfs and APFS pass. btrfs on kernels older than 4.7 fails, and so does any other filesystem without the flag.

### Remediation on the shared primary (2026-11-06)

The sqld data directory moved onto a 16 GB ext4 image, `/volume2/docker/fno-sqld.ext4.img`, loop-mounted at `/volume2/docker/fno-sqld-ext4`. The container was recreated on that mount with its previous environment, port and restart policy. Its entrypoint now waits for a `.on-ext4` marker in the data directory, so an auto-start that lands before the mount cannot reinitialize an empty store. The boot hook `/usr/local/etc/rc.d/fno-sqld-ext4-mount.sh` mounts the image and restarts the container. If the marker is missing, it stops the container instead, because a store that is clearly down beats one that silently starts empty. The old btrfs directory stays in place until the clean window below closes. Downtime was about 3 minutes.

## Diagnose before recovery

Run `fno agents claim backlog probe` to measure one read and one empty write transaction. Capture its read and write times, change-log bounds, and recent average batch bytes. This is a point-in-time write-lock probe, not a measurement of every claim write.

During a stall, preserve the primary's query statistics, current connections and transaction evidence before changing its state. Compare slowest-query entries with a baseline. Without timestamps, persisted query statistics cannot date an old slow query.

The primary's `backlog_writers` table records each committed batch's origin, PID, command and timestamp. Read recent rows with:

```sql
SELECT batch, pid, cmd, at
FROM backlog_writers
ORDER BY at DESC
LIMIT 50;
```

The stamp commits with the batch. A visible stamp identifies a committed writer. It cannot directly identify an uncommitted transaction holding the write lock. A missing established TCP connection also does not establish that no server-side HTTP stream holds transaction state.

## Operational verification

A code regression and a short healthy probe establish different facts from sustained operation. Record the deployed fixed build and the observation start and end. Include actual claim-write timing coverage, fleet activity, container start time, and observation gaps. Record timeouts and failed measurements explicitly. Empty write-lock probes alone do not prove that every claim write met its latency bound.

If the stall recurs, preserve the new evidence and continue diagnosis. Do not treat removal of the measured scan amplification as proof that another lock holder or server fault is absent.

The same discipline applies to the compaction fix. The observation window for the ext4 remediation started 2026-11-06 08:13Z with the remediated container. When a 72-hour log scan shows at least one `performing log compaction` line and zero `snapshot file swap` failures, the window closes. A window with no compaction line does not close, because it proves the logs were quiet, not that the swap works.
