# fno documentation

Pick the stage you are at. Each entry page answers one question and links onward. For the deepest technical detail, each skill's `SKILL.md` under `skills/` is the source of truth.

| Stage | Your question | Start at |
|---|---|---|
| Discover | What does fno do, and is it safe to point at my repo? | [overview.md](overview.md) |
| Install | How do I install it, and how do I know it worked? | [install.md](install.md) |
| First target | How do I ship my first PR with it? | [quickstart.md](quickstart.md) |
| Daily driver | Which command do I run for this task? | [workflows.md](workflows.md) |
| Fleet | How do I spawn, watch, route and recover workers? | [fleet.md](fleet.md) |
| Contribute | How is fno built, and where does my change go? | [contributing/README.md](contributing/README.md) |

## Guides

- [guides/target.md](guides/target.md) - the autonomous loop: flags, gates, cross-project, resume
- [guides/how-done-is-decided.md](guides/how-done-is-decided.md) - how the stop gate decides a run is done
- [guides/think-and-plan.md](guides/think-and-plan.md) - design exploration, plan creation, wave execution
- [guides/pr-lifecycle.md](guides/pr-lifecycle.md) - review, create, check, merged: the PR arc by hand
- [guides/review.md](guides/review.md) - the owned review lane versus a cross-model second opinion
- [guides/fix.md](guides/fix.md) - fast repair versus methodical diagnosis
- [backlog-usage.md](backlog-usage.md) and [backlog-triage.md](backlog-triage.md) - the backlog, day to day
- [guides/worktrees.md](guides/worktrees.md) - isolate each feature in its own worktree
- [guides/per-task-executors.md](guides/per-task-executors.md) - how an executor is picked per task
- [guides/buddy.md](guides/buddy.md) - the companion mod above the Claude Code prompt
- [guides/reading-shipped-plans.md](guides/reading-shipped-plans.md) - the completion-stamp format
- [troubleshooting.md](troubleshooting.md) and [best-practices.md](best-practices.md) - first-run failures and habits
- [auth.md](auth.md) - credential setup and multi-account notes

## Reference

- [configuration-guide.md](configuration-guide.md) - the full `config.toml` reference
- [path-config.md](path-config.md) - the `config.paths.*` schema, env vars, template variables
- [env-vars.md](env-vars.md) - every environment variable fno reads
- [provider-rotation.md](provider-rotation.md) - provider records, failover, per-agent routing, combos
- [api-reference.md](api-reference.md) - command surface reference
- [HARNESSES.md](HARNESSES.md) and [SKILL-COMPAT-MATRIX.md](SKILL-COMPAT-MATRIX.md) - what works on each CLI
- [harnesses/](harnesses/) - per-harness install notes
- [SETUP-HERMES.md](SETUP-HERMES.md) and [SETUP-OPENCLAW.md](SETUP-OPENCLAW.md) - alternate runtimes
- [security-posture.md](security-posture.md) - the trust boundary for plan execution (see also [../SECURITY.md](../SECURITY.md))

## Where a page belongs

[docs-map.tsv](docs-map.tsv) gives every page a stage, a kind and a slot. A new page adds its row in the same change.
