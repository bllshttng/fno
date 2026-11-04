# Fleet map

The fleet map shows every machine on one shared primary and the sessions on each. Any machine can read it. The code is `crates/fno-agents/src/fleet_beacon.rs`.

## Read it

```bash
fno agents fleet top --machines            # text: one block per machine
fno agents fleet top --machines --json     # the same rows as JSON
fno agents fleet top --machines --since 10m
```

A machine whose newest beat is older than `--since` (default 5m) shows `offline` with its age. With `store.remote_url` unset, this machine shows alone and the output names the missing key.

The mux sideline reads this map to group rows by machine and open a session on another one: see [remote sessions](mux-remote-sessions.md).

## How rows get there

Each daemon beats every 60 seconds. One beat writes this machine's `fleet_machine` row and replaces its own `fleet_session` rows, in one transaction on the primary. A session's `age_s` is the whole seconds since its last activity. A row has one writer: the machine it names. The beat reads numbers the daemon already keeps. CPU, verdict and memory pressure come from the newest `machine_sample` journal row. Memory used and the cap come from the capacity state file. The beat samples nothing itself. An unreachable primary logs once and never blocks the tick.

The same beat replaces this machine's `fleet_seat` rows. A seat row holds the full session id, the model and its basis (`registry` or `observed`), and the start time. It also holds the last turn's stamp, the directory, the project, the role and the context reading. The JSON puts them on each session. A machine whose daemon predates the table writes no seat rows, so its sessions read these fields as null. A seat row whose session row is gone is dropped on read. So is a seat row from an older beat than its machine's newest. A seat that a newer daemon left behind then never attaches to an old daemon's session. When the seat read fails, the sessions still come back and the JSON names the failure in `seat_error`.

The beat also replaces this machine's `fleet_turn` rows and its `fleet_machine_facts` row, on the same rules. A turn row holds the session's last message, its last prompt, its user turn count and its progress. The beat reads the prompt, the count and the message off the session's transcript, for claude and codex. The read resumes where the last beat stopped. A message keeps its paragraphs and is cut at 1,200 characters, and a prompt is cut at 600. Other harnesses fall back to the `top` row's message, which is short and on one line, and their prompt and count read null. The progress comes from the `top` row. The facts row holds the display name and the provider lanes: each lane's cap, live count and whether it is full. A lane never carries the names of the sessions that hold it. This text goes to the primary at `store.remote_url`, and the web command page shows it in the session drawer. A read failure on either table joins `seat_error`.

The beat also replaces this machine's `fleet_mark` rows, on the same rules. A mark row holds the facts a sideline sorts, groups and prices a card on. They are the session's workspace name, whether it needs input, and whether you pinned it. They also include its cost in cents, its token count and its compaction count. The needs-input flag is the blocked word of the badge lattice the daemon already folds. The pin is the keep mark that `fno agents reap --keep` writes. Only the mux server resolves a session to a workspace, and only it serves the counts. It writes both to `fleet/workspaces.json` under the agents home, and the beat forwards them. The server refreshes the file only while a client is attached to it. A file older than five minutes reads as nothing filed, and the sideline then groups that session by its project and hides the counts. A count the server has no reading for is null, never zero. A machine whose daemon predates the table reads all six as null. A read failure joins `seat_error`.

Watts and GPU are null today. The mux meter's `macmon` child is their only reader, and it does not run on the daemon side. Booted simulators are not sampled: `xcrun simctl list devices booted` took over 120 seconds on one machine.

## Who owns a machine

The beat reads the owner from the one person resolver: the user a [user certificate](multiplayer.md#the-user-certificate) proves, else `fleet.owner` from the global config, else `$USER`. It reads `fleet.role`, else `worker`. It reads `fleet.name` as the display name, else null, and a reader then shows the host.

```toml
[fleet]
owner = "jason"
role = "lead"
name = "MacBook"
```

## Takes work

```bash
fno agents fleet top --machines --takes-work off
fno agents fleet top --machines --takes-work on
```

`off` writes `<agents home>/fleet/takes-work-off`. The spawn gate then refuses new agent work on this machine with exit 82 and reason `machine-takes-no-work`. A spawn you type yourself still runs, as with the machine brake. The next beat publishes the flag to every machine's map.

## Parent edges

A session row carries its spawner's session prefix. When that prefix matches a session on any machine, the JSON adds `parent_at` as `<name>@<host>`. An ssh spawn carries the spawner's session id across the hop as `--spawned-by`, so a cross-machine child shows its parent too.

## Keys

The mux's Fleet tab (the org board) acts on a seat or a machine that this mux does not host. Each key runs the public `fno` command, so the ssh relay stays the only path to another machine.

| Key | Row | Runs |
|---|---|---|
| space | a seat on another machine | `fno agents session peek <short>@<host> -n 40`, shown on a page that esc closes |
| `P` | a seat on another machine | `fno mux pane run fno agents session peek <short>@<host> --follow`, a local pane that follows the seat read-only |
| `s` | a machine, lead or seat | a prompt line, then `fno agents spawn --name @<host> --cwd <dir> "<prompt>"` |

`P` opens a viewer pane, not a portal. A portal needs a registry row on this mux, and a seat on another machine has none. `x` stops a seat on this machine only, and on another machine's seat it names the stop command to run there.

`s` starts the seat in the selected seat's directory. With no seat selected, it uses the directory of the first seat on that machine that reports one. A machine with no such seat refuses and names the row. On this machine the relay strips the host and spawns here. When the machine takes no work, the prompt line says so, and a spawn you type still runs.

When the machine's beat is stale, every action refuses. One action runs at a time, and a second key names the first. A failed ssh hop shows ssh's own message, and the board stays open.

`s` refuses on a machine whose owner differs from this machine's owner, and names that owner. Work on another person's machine needs team mode, which fno does not have yet. The owner is the stated `fleet.owner`, not proof, so the refusal is a courtesy and protects nothing. Plain ssh still reaches the machine.
