# The first-turn watch: every spawn reports back

A spawn either reaches its first turn, or its caller hears where it is stuck, by 5 minutes after the call. The daemon arm `crates/fno-agents/src/spawn_watch.rs` owns this. It ticks every 60 seconds beside the first check.

On 2026-10-13 one machine logged 54 spawn resolutions in 4 hours and 5 launches. A queued spawn waited up to 600 seconds before its caller got an answer. A launch whose harness never started left a pending launch record, and the record's prune deleted it after 10 minutes with no word to anyone.

## What it watches

When an agent types a spawn or a native launch line, the PreToolUse Bash hook writes a launch record. The record names the caller's session and the call time, and it lands before the spawn gate runs. The watch copies each record into `<agents home>/spawn-watch/<key>.json`, so the prune loses nothing. The pending copy and the claimed copy of one launch share one key. The native lane writes a second record of the same call after the gate admits it. That record folds into the earlier watch from the same caller and harness, inside the gate's wait, so one launch keeps one watch.

Only a detached launch is watched: a `claude --bg` line, a spawn verb, a pane run, or a dispatcher's record. A one-shot such as `codex exec` or `pi -p` is not watched. The caller's own Bash call waits for it and gets its answer there. A spawn with no launch record is not watched either. The mux composer has its own answer: its caller is the human, who sees Launched, Refused or Unknown. A daemon or advance dispatch that no agent typed writes no record either.

## The four stages

Each pass reads the world and names the furthest stage it can prove.

- **gate**: no worker row, and the gate holds no `worker:<name>` admission. A refused or abandoned call reads the same way.
- **launch**: the gate admitted the spawn, and no worker row exists yet.
- **harness boot**: a worker row exists, and it has no harness session id yet.
- **first prompt**: the session started, and its transcript holds no assistant line yet.

The watch finds the worker row by its harness session id once one is known. Before that, it takes the first row the caller spawned on the same harness after the call. If the record has a name, the row must carry it too. A row one launch took is never matched to a second launch.

## What the caller hears

Four minutes after the call, a launch with no first turn mails its caller once, from `fno-spawn-watch`. The line names the stage and the reading that proves it, for example `spawn quiet-otter: no first turn 4m after the call. Stuck at harness boot: worker row quiet-otter exists (Spawning) with no harness session id.` With the 60-second tick, the mail lands by 5 minutes. When that launch later reaches its first turn, one follow-up names the latency. A failed send retries on the next pass.

The gate answers inside the same bound. Its queue gives up after 240 seconds with the spawn's place and reason. When the gate refuses a spawn, the caller holds the refusal, so the gate closes that launch's watch and removes its pending record. The watch sends no second word about it.

## The measurement

The first assistant line stamped at or after the call closes the watch. A resumed transcript's older turns do not count. The watch records `first_turn_at_ms` and `latency_ms` from the call, and emits one `spawn_first_turn` event. The event carries the name, harness, substrate, the machine hash, `latency_ms`, and `row_ms`, the time from the call to the worker row. Read it per substrate:

```bash
fno doctor event find spawn_first_turn --field substrate=thread --since 7d
```

A watch with no first turn after 2 hours closes. A closed watch file prunes after 24 hours.

## After the first turn: a prompt with nobody to answer it

A first turn proves the launch, not the run. On 2026-10-22 a thread worker waited 11 hours on an EnterWorktree permission prompt, with nobody near the host. A thread worker has no mux pane, so the lead check-in's pane scrape never saw it. The daemon arm `crates/fno-agents/src/prompt_wait.rs` covers that gap. It ticks every 60 seconds on every host, beside this watch.

Each pass reads `claude agents --json --all` under every claude account root. A row with a live process and a `waitingFor` that names a permission prompt is waiting. The listing gives no start time for the wait. So the first sighting is written to `<agents home>/prompt-wait/<short id>.json`. A clean listing without the row deletes that file, so the next prompt counts from its own start. A partial or failed listing deletes nothing.

Five minutes after the first sighting, the arm mails once, from `fno/prompt-wait`. The spawner hears it, then the nearest ancestor that holds a lead role. The worker row names the spawner. With no row, this watch's file names it. The mail names the host and the remedy: `claude attach <short id>`, then pick an option. With no spawner on record, the arm files one fleet task instead. The lead check-in reads the same files and prints one `WAITING ON APPROVAL` line per overdue worker.
