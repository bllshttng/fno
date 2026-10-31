# Quickstart: your first PR with fno

## Is this page for you?

You installed fno and want to see it ship something. This page takes you from an installed plugin to your first PR. You run a few commands, and one agent does the work while you are away. Run time depends on your repo and your CI: one small fix in fno's own repo took 1 hour 53 minutes, most of it CI.

Not for: installing fno. If `fno --version` fails, start at [install](install.md).

## Before you start

Check three things:

```bash
fno --version                  # prints a version
fno config setup run --check   # names each missing piece and its fix
```

Inside Claude Code, type `/fno:` and `target` appears in the autocomplete. If a check fails, [install](install.md) has more detail.

Run the one-time setup and accept every default:

```bash
fno config setup run --yes
```

## Ship your first feature

These steps use Claude Code. On another harness, see the [command matrix](harness-command-matrix.md) for its form of the command.

Your repo needs a GitHub remote, and `gh` must be signed in. Check with `gh auth status`.

Your repo also needs CI. With no checks on the PR, the loop blocks and says so, because it never reads "no checks" as green. A one-job GitHub Actions workflow is enough.

Open a Claude Code session in a git repo you can push to. Point the loop at a small task:

```
/fno:target "add a health check endpoint that returns server status"
```

Target plans the change, implements it with TDD, reviews its own diff, and opens the PR. Watch it or walk away. When it ships, the PR URL prints.

The internal review is the configured inline lane: one head-pinned reviewer by default (`config.review.posture` floors at `self_review`), with at most `config.review.max_rounds` rounds (default 2). With no `config.review.github_apps` set there is no external-bot gate. Name a bot there to make target also wait for that review.

## How you know it is done

The run ends at the PR, not at a merge. A stop hook reads three facts: the PR exists, CI is green, and review had its rounds. [How done is decided](guides/how-done-is-decided.md) explains each read.

Target merges on its own only with `config.auto_merge.enabled` (default `false`). Otherwise it stops and the merge is yours.

## Drive it step by step instead

When you want to approve the design and the plan before any code:

```
/fno:think "health check endpoint"     # explore the design space, approve a direction
/fno:blueprint "health check endpoint"  # turn it into an executable plan
/fno:target path/to/plan.md             # execute the plan end to end
```

## Fix a broken run

- [Troubleshooting](troubleshooting.md): the failures first runs hit, with the fix for each.
- [Best practices](best-practices.md): habits for reliable, cost-bounded runs.

## Next

[Workflows](workflows.md) maps each daily task to its command.
