# fno

**fno takes a feature description to a green, reviewed pull request, and it does not stop until GitHub says the work is done.**

```
curl -fsSL fno.sh | sh
```

![A replay of one real target run, from the start command to merged PR 4195](docs/images/target-run.gif)

The GIF replays [PR #4195](https://github.com/bllshttng/fno/pull/4195) at 270x. The run took 1 hour 53 minutes, and CI failed three times before the merge. Each failure got a fix and a new push, and the loop kept going.

Then open a session in a git repo you can push to, and run `/fno:target "your feature"`. Next: the [quickstart](docs/quickstart.md). Site: [fno.sh](https://fno.sh).

Set the targets. Your agents do the rest. Say F[no] to mostly done.

- fno runs as a plugin on any harness that accepts plugins and hooks. Claude Code, Codex, OpenCode, and agy are wired today, with a standalone CLI underneath.
- Point it at a feature description or a backlog node. It plans, builds with TDD, reviews its own diff, and ships the PR.
- Completion is decided by the world, not by a model's mood. The PR exists, CI is green, review has had its rounds.
- A dependency-graph backlog keeps the next piece of work ready, so an unattended loop never stalls on "what now?"

![The fno mux with four agent panes working fno tasks, a sideline roster, and a live status row](docs/images/ux-shot-live-fleet.png)

## Install

The line at the top installs the `fno` CLI, and `uv` first when `uv` is missing. It wires the plugin into every agent CLI it finds and runs the one-time setup with defaults. `FNO_NO_WIRE=1` skips the wiring and the setup. `fno config setup run --check` names anything the machine still lacks. Keep it current with `fno update`.

[docs/install.md](docs/install.md) covers the other routes: plugin marketplaces, `uv tool install fno`, `cargo install fno`, a clone, and each harness.

## Uninstall

```
fno uninstall --dry-run
fno uninstall
```

The first command lists what it found and changes nothing. The second removes the plugins, the hooks fno added to harness config, the launchd agents and the binaries. It also stops the daemon and the mux. It keeps `~/.fno`. Add `--purge` to delete that too, after you type the confirmation word. Run it from a plain terminal, not from inside an fno mux pane.

## How it works

Named stages, one sentence each:

- **think** explores the design space and writes cited findings before anyone commits to a plan.
- **blueprint** turns the approved direction into an executable plan: waves, tasks, acceptance criteria.
- **target** is the walk-away loop. It executes the plan and will not stop until the PR is up, CI is green, and review is done.
- **review** reads the diff before it ships. An inline lane emits a head-pinned attestation, with `config.review.max_rounds` (default 2) capping the rounds.
- **pr** drives the lifecycle: create, check for external review, and the post-merge ritual.

## What it enforces

The finish line is a PR with CI green and review done under your configured policy. The review round cap releases still-open findings into the PR conversation rather than blocking forever. The merge itself is yours until you opt in to auto-merge. See what can run without you, gate by gate: `fno agents admit autonomy status`. Not a sandbox: it runs your plans with your credentials on your machine, and [docs/security-posture.md](docs/security-posture.md) draws the trust boundary.

## Docs

- [Docs home](docs/README.md): every page, grouped by what you are trying to do
- [Install](docs/install.md): install, verify, and set up a project
- [Quickstart](docs/quickstart.md): your first PR
- [Workflows](docs/workflows.md): which command for which task
- [Target pipeline](docs/guides/target.md): the loop's flags, gates, and resume behavior
- [Think and plan](docs/guides/think-and-plan.md): design exploration and planning
- [PR lifecycle](docs/guides/pr-lifecycle.md): review, create, check, merged
- [Agents quickstart](docs/guides/agents-quickstart.md): spawn and message peer agents
- [Vocabulary](docs/architecture/vocabulary-user-and-operator.md): citizen, role, and the rest of the mesh's words
- [Troubleshooting](docs/troubleshooting.md) and [best practices](docs/best-practices.md)
- [Security posture](docs/security-posture.md)

## Requirements

macOS (Apple Silicon or Intel), Linux (x86_64 / arm64), or Windows via WSL2. Python 3.11+ or uv, `jq`, and `gh` (authenticated).

## License

Apache-2.0, [Jason Noah Choi](https://github.com/bllshttng)
