# The cloud substrate

`--substrate cloud` seats a claude worker on Anthropic-managed infrastructure: the session runs remotely, footnote tracks it, mails it, and teleports its work back on demand. Claude-only; every other harness refuses naming that. The lane's contract rows live in `crates/fno-agents/src/harness_capabilities.toml` under `[harness.claude.cloud]`, and the one (harness, substrate) router resolves it in `crates/fno-agents/src/spawn_backends.rs`.

## Is this page for you?

You spawned or inherited a cloud row and want to know what it can and cannot do. This page owns the substrate's shape and limits. Not for: the thread keeper lanes (that is [thread-lanes.md](thread-lanes.md)), or the mux pane lane.

## What the create does

`claude --cloud` refuses a non-interactive invocation outright (measured 2026-10-18), so the client runs the create under a pty it holds (`crates/fno-agents/src/claude_cloud.rs`). The CLI prints the minted id (`Resume with: claude --teleport session_<id>`, with the same id in the `View:` URL) and exits at once. The session survives client death, so no local process outlives the create and the pty is released. The registry row carries the cloud id as `harness_session_id`, `substrate=cloud`, status `live`, and no pid.

## How the lane reaches the session

Mail is queued delivery: the contract's follow-up argv renders `claude -p "<envelope>" --cloud <session-id>`, the CLI prints `Sent to cloud session.` and exits 0, and the exit code is the delivery answer. The reply never returns on stdout, and the session keeps no local transcript, so a cloud row reads the honest `?` in `fno agents list` and the watchdog reports LEAVE where a transcript-less local row would read ghost.

Attach is teleport: `fno agents attach <name>` on a cloud row measures the pre-checks the CLI enforces (same repository by git common dir, clean working tree) and refuses by name before any exec, then runs `claude --teleport <session-id>`. Teleport pulls the session's pushed branch into the local checkout; a dirty tree must be committed or stashed first.

## The limits

- **GitHub-only push-back.** The cloud session pushes work as git branches on the repo's GitHub remote; other remotes are the session's own problem, not a lane feature.
- **No state-root access.** The worker has no access to fno's state root; `state_root_grant.cloud = "remote-unreachable"` is the contract's stamp, and spawn-time state-root questions refuse.
- **No loop participation.** A cloud row never enters the target loop: its stop hook does not exist locally, so `loop-check` never reads it.
- **Plan-limit drain.** Each parallel cloud session drains the account's plan usage independently of local seats; the create form carries the description only, so model, effort and permission axes refuse by name.
- **Teleport needs a pushed branch and a clean tree.** Both are pre-checked or left to the CLI's own refusal, which the attach verb surfaces.

## The gate evidence

The lane shipped in the same commit as its journeys: the unit journeys (`crates/fno-agents/tests/spawn_routing.rs`, cloud cases) cover the router arm, the refusal sentences and the mail argv without a live session, and the opt-in live journey (`cli/tests/agents/test_cloud_spawn_journey.py`, `FNO_CLOUD_LIVE=1`) runs spawn, id read-back, one queued follow-up and the dirty-tree teleport refusal against a real cloud session. The gate rule in [thread-lanes.md](thread-lanes.md) applies by analogy: a substrate ships with the journey that proves it.
