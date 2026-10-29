# Quickstart: your first PR with fno

## Is this page for you?

You installed fno and want to see it ship something. This page takes you from an installed plugin to a green, reviewed PR in about ten minutes of your time. The loop does the rest while you are away.

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

Open a Claude Code session in a git repo you can push to. Point the loop at a small task:

```
/fno:target "add a health check endpoint that returns server status"
```

Target plans the change, implements it with TDD, reviews its own diff, and opens the PR. Watch it or walk away. It will not quit until the PR is open and CI is green. When it ships, the PR URL prints.

The internal review is the configured inline lane: one head-pinned reviewer by default (`config.review.posture` floors at `self_review`), with at most `config.review.max_rounds` rounds (default 2). With no `config.review.github_apps` set there is no external-bot gate. Name a bot there to make target also wait for that review.

## How you know it is done

A green, reviewed PR is the finish line, not a merge. The stop hook reads the world: the PR exists, CI is green, and review had its rounds. A model cannot declare itself done. [How done is decided](guides/how-done-is-decided.md) explains each read.

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
