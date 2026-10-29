# Shared-store write contention

The shared primary serializes writes. A slow replication transaction can delay a claim statement even while read-only requests still answer. A responsive HTTP endpoint does not establish write health.

## Replicated updates

Local triggers capture before and after row images. `backlog_share::narrow_updates` retains changed columns and primary-key columns. The key values identify the row for compare-and-swap. An assignment is needed only for a changed value.

The conditional writer assigns only values that changed. Its WHERE clause retains the complete narrowed pre-image, including unchanged key values, with `IS` comparisons so NULL pre-images remain guarded. Genuine key changes remain assignments. The existing net-change fold removes updates whose before and after images are identical.

When an update assigns an unchanged primary key, SQLite can check referencing tables. On the measured primary, an `agent_sessions` update that assigned `id` planned repeated scans of related tables. Omitting the unchanged identity assignment left one indexed lookup. An isolated SQLite fixture with 60,000 referencing rows performed about 480,000 VM steps with the identity assignment. This establishes unnecessary work. It does not establish the cause of an intermittent server stall.

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
