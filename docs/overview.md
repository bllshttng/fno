# What fno is

## Is this page for you?

You use Claude Code or another agent harness and heard of fno. This page says what fno does, how it decides a job is done, and what it will and will not do on your machine. When you want to try it, go to [install](install.md).

## The problem

A coding agent writes code. It does not ship a feature. Between "here is some code" and "here is a tested, reviewed PR" a developer still splits the work into tasks, runs the tests, reviews the diff, opens the PR, and answers the review. fno does those steps. It stops after the PR is done.

## What it does

Point fno at a feature description, a plan, or a backlog node. It plans the change, builds it with TDD, reviews its own diff, and opens the PR. It runs as a plugin on any harness that accepts plugins and hooks. Claude Code, Codex, OpenCode and agy are wired today, with a standalone `fno` CLI underneath.

The stages, one sentence each:

- **think** explores the design space and writes cited findings before anyone commits to a plan.
- **blueprint** turns the approved direction into an executable plan: waves, tasks, acceptance criteria.
- **target** is the walk-away loop. It executes the plan and will not stop until the PR is up, CI is green, and review is done.
- **review** reads the diff before it ships. An inline lane emits a head-pinned attestation, with `config.review.max_rounds` (default 2) capping the rounds.
- **ship** drives the PR lifecycle: create, check for external review, and the post-merge ritual.

A dependency-graph backlog keeps the next piece of work ready, so an unattended loop never stalls on "what now?"

## How done is decided

The world decides, not the model. A stop hook reads three facts before a session ends. The PR exists. CI is green on it. Review had its rounds under your configured policy. A model that says "done" early gets sent back to work. [How done is decided](guides/how-done-is-decided.md) walks through each read.

The merge itself is yours until you opt in to auto-merge with `config.auto_merge.enabled`.

## Who it is for

fno fits a solo founder or a small team. Agents carry the mechanical work: tests, review, PRs. People make the product and architecture calls. It assumes git and PR-based delivery. It fits badly where a team wants to approve every line before it is written.

## What it will and will not do

fno is not a sandbox. It runs your plans with your credentials on your machine, commits to real branches, and opens real PRs. [Security posture](security-posture.md) draws the trust boundary, and `fno agents admit autonomy status` shows what can run without you, gate by gate.

## Next

- [Install](install.md) fno on your harness.
- [Quickstart](quickstart.md): your first PR.
