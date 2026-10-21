# Fleet map

The fleet map shows every machine on one shared primary and the sessions on each. Any machine can read it. The code is `crates/fno-agents/src/fleet_beacon.rs`.

## Read it

```bash
fno agents fleet top --machines            # text: one block per machine
fno agents fleet top --machines --json     # the same rows as JSON
fno agents fleet top --machines --since 10m
```

A machine whose newest beat is older than `--since` (default 5m) shows `offline` with its age. With `store.remote_url` unset, this machine shows alone and the output names the missing key.

## How rows get there

Each daemon beats every 60 seconds. One beat writes this machine's `fleet_machine` row and replaces its own `fleet_session` rows, in one transaction on the primary. A row has one writer: the machine it names. The beat reads numbers the daemon already keeps. CPU, verdict and memory pressure come from the newest `machine_sample` journal row. Memory used and the cap come from the capacity state file. The beat samples nothing itself. An unreachable primary logs once and never blocks the tick.

Watts and GPU are null today. The mux meter's `macmon` child is their only reader, and it does not run on the daemon side. Booted simulators are not sampled: `xcrun simctl list devices booted` took over 120 seconds on one machine.

## Who owns a machine

The beat reads `fleet.owner` from the global config, else `$USER`. It reads `fleet.role`, else `worker`.

```toml
[fleet]
owner = "jason"
role = "lead"
```

## Takes work

```bash
fno agents fleet top --machines --takes-work off
fno agents fleet top --machines --takes-work on
```

`off` writes `<agents home>/fleet/takes-work-off`. The spawn gate then refuses new agent work on this machine with exit 82 and reason `machine-takes-no-work`. A spawn you type yourself still runs, as with the machine brake. The next beat publishes the flag to every machine's map.

## Parent edges

A session row carries its spawner's session prefix. When that prefix matches a session on any machine, the JSON adds `parent_at` as `<name>@<host>`. An ssh spawn records no edge yet, so a cross-machine child shows no parent.
