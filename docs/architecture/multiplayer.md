# Multiplayer: one shared primary for dispatch claims

Two or more machines run one fleet. Each machine dials out to one libSQL server (sqld), the primary. No machine accepts a connection from another, so the machines need no shared file system. The ruling that chose libSQL, its host, and the config key prints with `fno backlog decisions multiplayer-store`.

## The key

`store.remote_url` in the per-machine global config (`~/.fno/config.toml`) turns it on. It holds the primary's URL, for example `http://r2d2:18080` on the tailnet. `store.remote_token` is optional. When it is set, each request carries it as a bearer token. The key is per machine and never enters git. A project config cannot set it, because the reader reads the global file only.

Unset is the stock install. Every store stays a local file and no fno process opens a socket for it. That holds forever: a machine with no account and no network keeps working.

A malformed value refuses every shared claim, with a message that names the key. A typo never falls back to local in silence. Only `http://` URLs are accepted, because the client carries no TLS. The tailnet owns remote reach, the same way `fno web` leaves it.

## What moves to the primary

The claim keys that decide dispatch move: `node:`, `dispatch:`, and `reconcile:` (`SHARED_PREFIXES` in `crates/fno-agents/src/claim_store.rs`). Every statement runs the same SQL on either store, so the local and remote legs cannot drift. Each claim is one conditional statement on the primary, and no decision reads a copy.

Every other claim stays local. `build:cargo`, `test:`, `session:`, `worker:` and `flight:` name resources of one machine. A cargo lock shared across machines serializes builds that never touch each other.

The agent registry, mail and the event store stay local. A second key moves the backlog, as the next section says. Without it, each machine keeps its own backlog in `graph.db`. The node claim is the dispatch decision. So two machines never build one node, even with backlog copies that disagree on status. Cross-host mail is the exception: the send writes one row on the primary, and the next section carries it.

## The shared backlog

`store.share_backlog = true` in the global config moves the backlog tables to the primary too. It needs `store.remote_url`. It is off by default. With it off, no backlog write opens a socket.

The tables that move are listed in `SHARED_TABLES` in `crates/fno-agents/src/backlog_share.rs`. The content version, the sync cursor and the search index stay local in `graph.db`. A new table in neither list fails a test.

Reads stay local. This machine's `graph.db` is a replica. Every 5 seconds, one daemon arm per machine sends the outbox and then applies the primary's change log. `fno agents claim backlog sync` does both at once.

Every backlog write opens through one seam, `backlog::open_connection`. With the key on, triggers there copy each row change into the local `backlog_outbox`, in the writer's own transaction. The commit stays local, so no writer holds the graph lock across the network. A flush then sends the outbox to the primary in one request, as one transaction. Each update and delete matches every old column of its row. Each insert must not collide. The same request adds one row to the primary's change log. The flush takes its own lock file, never the graph lock. A write through `mutate_rows` flushes right after the graph lock drops. The daemon flushes every other write within 5 seconds.

A build from before the shared backlog writes with no outbox, and the primary never sees that write. So a store that shares carries a guard trigger on each shared table. The trigger calls `fno_backlog_writer()`, and only a current build registers that function. An older `fno-agents`, daemon, worker, or a plain `sqlite3` shell fails its write with `no such function: fno_backlog_writer` and changes nothing. When the key is unset, the next open by a current build drops the guard. Before you install an older build, unset the key and run one `fno backlog` command.

A row that a peer changed first refuses the whole batch, and the primary writes nothing. The replica then takes the primary's rows back for every row the batch touched, and the refusal names the table. A write through `mutate_rows` syncs and retries by itself.

If the primary cannot be reached, backlog writes still land in the replica and wait in the outbox. The next flush that reaches the primary sends them, and a peer's newer change refuses them then. Reads keep working from the replica. To work alone for good, unset `store.share_backlog`.

## Start sharing

1. Run `fno doctor update` on every machine and restart its daemon. Once the store shares, an older `fno-agents`, `fno-agents-daemon` or `fno-agents-worker` cannot write the backlog.
2. Copy `graph.db`. The first sync on a machine replaces that machine's backlog with the primary's. It also writes a `graph.pre-share-<ms>.db` backup beside the store.
3. On one machine, set both keys and run `fno agents claim backlog seed`. It copies this backlog to the primary. It refuses a primary that already holds one.
4. On every other machine, set both keys and run `fno agents claim backlog sync`.

If the primary holds no seeded backlog, a flush refuses and names the seed verb. The writes wait in the outbox.

A later fno version that adds a backlog column does not change the primary. Writes then refuse with the primary's "no such column" error. Export, unset the keys, and seed again from a current machine.

## The store clock decides a peer's lease

A machine cannot probe a peer's pids. So for a row another machine wrote, only the lease decides, read on the primary's clock. A takeover of a peer's row carries `claims.expires_at <= <store clock>` in its compare-and-swap. Two machines with skewed clocks still agree on the moment a lease ran out.

## Cross-host mail

A send to `<name>@<host>` writes one row to `fno_mail_bus`, the mail table on the primary. Each host's daemon pulls the rows that name it, on `agents.remote.pull_interval_secs` (a global-config key, default 15 seconds), and injects each through the receiver's own local send, so lane selection, holds, and receipts stay the local path's. The row order is the primary's `seq`, never host clocks.

The sender reads as `<user>@<host>/<session name>` in the row's `from_id`. That field is written by the sender, so it proves nothing by itself: the tailnet is the boundary, and the raw allowlist is a second check, not a proof of identity (operator ruling, 2026-10-10). A row with `raw = 1` (the pane-typing lane) is refused unless its `from_id` head matches a sender in `agents.remote.raw_senders`, a global-config list; the default empty list refuses every cross-host raw row.

Rows wait up to 7 days for their host (`MAIL_TTL_SECS`); the expire leg flips past-due rows to `expired`, and a pull that finds the name unknown leaves the row pending until then. `INSERT OR IGNORE` on the unique `msg_id` makes a retry after an ambiguous timeout a no-op, and the receiver's seen ledger (`mail_seen.jsonl` in the agents state root) keeps a redelivered row from injecting twice: delivery is at-least-once at the bus and once at the receiver.

When the primary is unreachable, the send queues the row in `mail_outbox.jsonl` and reports `queued (local, store unreachable)`; the daemon's flush leg drains it when the store answers again. With no `store.remote_url` set at all, a cross-host send relays over ssh: the remote machine's own `fno` answers, its exit code and receipt pass through, and `--from-self` refuses on that lane (it would stamp the remote machine's identity; pass `--from-name`).

Host spellings normalize on write and on pull: lowercase, trailing `.local` dropped, so `iMac2D2.local` and `imac2d2` are one host. The two-machine walk-through is the same throwaway-primary drill as [the shared backlog](#start-sharing).

## The lease heartbeat

A worker renews its node claim at every stop. One turn can run for hours, and no stop comes in that time. So the daemon renews too (`crates/fno-agents/src/lease_heartbeat.rs`). Every 60 seconds it reads the shared claims the primary records for this machine. It keeps the ones whose holder pid is alive here, and moves their leases to 10 minutes past the store clock, in one statement. A claim whose pid is dead is not renewed, so it lapses and a peer can take it.

## Failure: the primary is unreachable

A claim verb refuses. The error names the URL and the key, and says nothing was written locally. A request that timed out can still have reached the primary, so a retry by the same holder is the safe next step. A 4xx reply, such as a bad token, is named as a refusal and never read as an outage. `fno backlog next` refuses selection. `fno do target init` writes a cancel signal with `store_unreachable` and owns no node. A running worker finishes its turn: at its stop, loop-check reads the unreachable renewal and allows the stop, so the worker holds. The machine claims no new work until the network returns. To work alone, unset the key.

## Failure: a peer took the claim

A machine that stays offline past its lease loses the claim, and a peer can take it. When the network returns, the heartbeat names the taker in a `claim_lost` event. At the worker's next stop, loop-check reads the same reason and ends the run with `Interrupted`. The message tells the worker to stop and not to push. A holder change on this same machine is a handover, never a loss.

## Leaving: export

`fno-agents claim export --out <file> [--url <primary>]` copies every table on the primary into a new local SQLite file. The file opens as a normal store. The primary stamps its claims table as the authority at creation, so the local open keeps the exported rows. Use `--url` after the key is unset.

## Test it

The two-machine walk-through is [docs/guides/multiplayer-test.md](../guides/multiplayer-test.md). It uses a throwaway primary and never the live store.
