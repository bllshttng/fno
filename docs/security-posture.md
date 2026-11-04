# Security posture

## The loop is for plans you trust

footnote's autonomous loop (target) is a delivery harness. When you point it at a plan, it executes the plan with your credentials, on your machine, against your repos. By default there is no sandbox beyond standard git worktrees. An optional [sandbox](architecture/sandbox-host.md) runs headless workers in a devcontainer, and it is not a security boundary.

This is intentional. The default stays sandbox-free. A solo founder running plans they wrote fears a mistake, not malice. Mistake recovery is what git, atomic commits, and human review are for.

This is also a constraint when footnote is shared with others.

## The trust boundary

You must run the loop only against plans you have read or trust the author of. Specifically:

- **Plans you wrote:** safe by definition (modulo your own mistakes).
- **Plans someone you trust wrote:** safe to the degree you trust them.
- **Plans from the footnote repository's `examples/` folder:** reviewed and safe.
- **Plans from random contributors, bug reports, gists, or untrusted sources:** read them carefully before running. The loop will execute them with your credentials.

The devcontainer sandbox is shared-state protection, not a security boundary. It keeps a headless worker from writing shared state, and it does not contain a malicious plan. Multi-tenant isolation remains out of scope.

## What the loop WILL do, given a plan

- Read and write files in the worktree.
- Run shell commands as the user invoking footnote.
- Make API calls to the configured model providers with the user's credentials.
- Open pull requests on GitHub with the user's credentials.
- Push branches to remotes the user has access to.
- Edit `~/.fno/` state files.

## What the loop WILL NOT do, by design

- Run with privileges higher than the user invoking it.
- Bypass git hooks, branch protection, or pre-commit checks.
- Skip review gates configured in the plan.
- Send your code to anyone other than the configured agent runtime.

## What the loop CANNOT prevent, given an adversarial plan

- A plan that asks the agent to leak credentials.
- A plan that asks the agent to delete files outside the repo.
- A plan that asks the agent to push to a third-party remote.
- A plan that constructs adversarial prompts for downstream skills.

If you are evaluating an footnote plan from an untrusted source, treat it as code review, not as configuration. Read the plan content, not just the title.

## The effect guard for hooked tool calls

For Claude-hosted workers whose PreToolUse hooks run footnote's plugin, there is a second layer between the tool list and the destination. The effect guard (`fno-agents hook effect-guard` and the Bash chain's last guard) classifies each hooked tool call into an effect class. Reads, searches, and local edits map to no effect and are never blocked by it. GitHub PR and issue comments also map to no effect: they are review traffic in the repo, like `gh pr create`. Sends and posts map to `external.communication`, publishes and deploys to `external.publication`, remote deletes to `infrastructure.destructive` (denied outright), and payments to `financial.payment` (denied outright). An approval-requiring class is refused unless an approved, unexpired decision by a principal named in `config.approvals.authorized_principals` matches it. The refusal names the request digest and the `fno inbox approvals decide <digest> --as <principal> --approve` line, and the worker can retry the call unchanged once approved.

What the guard does not cover: an effect hidden inside a script or a raw `curl` (the Bash rows match leading command tokens). It also does not cover a harness whose PreToolUse hooks footnote does not wire (codex, opencode, pi, agy). The merge effect stays with `authorized-merge` and git-protection. The tool list remains the outer boundary. The effect guard narrows what a hooked tool call can do inside it.

## Code scanning dispositions

CodeQL runs through advanced setup (`.github/workflows/codeql.yml` with `.github/codeql/codeql-config.yml`). The analyze job is skipped until the repository switches from default setup to advanced setup. The gate is the `CODEQL_ADVANCED_SETUP` repository variable. Between merge and the switch, default setup keeps its weekly scan and the skipped job keeps main green. Rule classes judged not to apply are excluded in the config or dismissed per alert, with the reasons below.

- **`rust/cleartext-logging` (404 alerts, excluded in the config).** False positive. Every source expression is an identifier printed to the user's own terminal: account arguments, session ids, uuids, account names. The twelve hits naming a secret, password, certificate or key are test assertion messages or counts. No hit writes key material to a log file. The same config ignores test trees: `paths-ignore` covers `crates/*/tests/**`, `cli/tests/**` and `**/fixtures/**`. That keeps `py/clear-text-storage-sensitive-data` (1 alert, a test fixture) out of every run.
- **`actions/cache-poisoning` (10 alerts, dismissed).** False positive. Both workflows trigger only on `workflow_dispatch` and `workflow_call`. The one caller passes a pinned main sha or rc tag as `ref`. No fork or pull-request code reaches a cacheable step.
- **`js/xss` (2 alerts) and `js/xss-through-dom` (1 alert), dismissed.** False positive. The mux web pages set `innerHTML` only from `renderRow` output: text passes the `esc()` escape helper and colors are numeric. The backlog link href is built from server-rendered data and `encodeURIComponent`.
- **`js/polynomial-redos` (1 alert) and `py/redos` (1 alert), fixed in code.** The lazy regexes went quadratic on nested-unclosed input. Both promise readers and the graphql inventory gate now use a linear scan or first-character-disjoint value tokens.
- **`actions/missing-workflow-permissions` (25 alerts), fixed in code.** The five workflows the alerts name carry top-level `permissions:` blocks now.
- **`rust/hard-coded-cryptographic-value` (8 alerts, 447-454).** False positive. The query reads the parameter name `nonce` in `verify_child_promise` as key material. The value is a session promise token that the code compares by equality with a stored journal string. No cipher, key or MAC consumes it. All eight hits are test literals (`crates/fno-agents/tests/verify_evidence_parity.rs`, and the `#[cfg(test)]` module in `crates/fno-agents/src/verify_evidence.rs`). Dismissal reason: `used in tests`.
- **`rust/command-line-injection` (1 alert, 446).** False positive. The attention API clears an item with an argv vector, never a shell. The `id` must match an open item from `fno-agents needs --items --json`. The `--answer` value is taken verbatim by the clear command. The route needs a bearer sink token and binds to 127.0.0.1. Dismissal reason: `false positive`.

Critical-severity alerts are held for the maintainer's decision and stay out of these dispositions.

## Future considerations

Two horizon items are tracked in the backlog with explicit "do not scope" tags:

- **Cost-anomaly alerts.** A real horizon need; will be planned when an incident creates concrete requirements.
- **APFS-snapshot phase rollback.** macOS-specific recovery capability; will be planned when an incident demands it.

Neither is a current capability. Do not assume either exists.

## Reporting issues

Security issues should be reported via a GitHub security advisory at https://github.com/<your-org>/footnote/security/advisories or by emailing the maintainer (see repository owner).
