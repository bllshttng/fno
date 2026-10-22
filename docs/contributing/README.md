# Contributing to fno

## Is this page for you?

You change fno itself, as a person or as an agent. This page links the rules, the gates, and the design notes, and it says where a new doc goes.

Not for: using fno. Start at the [docs home](../README.md).

## Start here

- [CONTRIBUTING.md](../../CONTRIBUTING.md): dev setup, the local checks, and the CI gates.
- [AGENTS.md](../../AGENTS.md): the working principles every agent session loads.
- [Style rules](../style-rules.md): the house style. `fno lint style` enforces it.
- [Code standards](../code-standards.md) and [design guidelines](../design-guidelines.md).
- [Testing guide](../testing-guide.md): test layout, markers, and each tier.

## How it is built

- [System architecture](../system-architecture.md): the big picture.
- [architecture/](../architecture/): one design note per subsystem. The [control-plane loop](../architecture/control-plane-loop.md) is the completion model.
- [State-root inventory](../state-root-inventory.md): every file fno writes, with its owner and lifetime.
- [Dual-implementation inventory](../architecture/dual-implementation-inventory.md): Python and Rust legs per surface.
- [Harness adapters](../harnesses/harness-adapters.md) and the [promise sentinel](../harnesses/promise-sentinel.md) for third-party loop drivers.

## Release and measure

- [Distribution](../distribution.md): packaging and release.
- [Evals](../evals.md), [product qualification](../guides/product-qualification.md), [measuring overhead](../guides/measuring-fno-overhead.md), and the [task-to-PR demo](../guides/task-to-verified-pr-demo.md).
- [test-audit/](../test-audit/): the test-cut campaign ledgers.

## Where a new doc goes

[docs-map.tsv](../docs-map.tsv) is the one navigation source. A new page gets a row there in the same change. Its kind picks its folder, and its stage picks its nav group. A page that fits no stage gets stage `none`, kind `internal`, and an `unplaced:` reason, and it waits for review. It is never guessed into a slot. The design: [docs information architecture](../architecture/docs-information-architecture.md).
